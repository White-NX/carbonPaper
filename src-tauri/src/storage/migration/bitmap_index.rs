//! Bitmap index lazy indexing and maintenance.

use super::super::blind_index::{BlindIndex, MutationStats};
use super::super::{BlindIndexRepairProgress, StorageState};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[cfg(test)]
mod bench;
mod postings_io;

#[derive(Default)]
struct LazyIndexWriteDiagnostics {
    cache_setup: Duration,
    tx_begin: Duration,
    active_check: Duration,
    hash_update: Duration,
    bitmap_prepare: Duration,
    bitmap_filter: Duration,
    bitmap_read: Duration,
    bitmap_merge: Duration,
    bitmap_serialize: Duration,
    bitmap_write: Duration,
    commit: Duration,
    active_rows: u64,
    existing_postings: usize,
    postings_written: usize,
    postings_bytes_read: usize,
    postings_bytes_written: usize,
}

// Accumulate before the caller propagates an error, so failed slow batches
// retain the time spent in the failing operation as well.
fn measure_lazy_index_step<T>(elapsed: &mut Duration, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    *elapsed += started.elapsed();
    result
}

impl StorageState {
    /// Number of OCR rows to process per batch for lazy indexing.
    const LAZY_INDEXING_BATCH: i64 = 100;
    const BLIND_INDEX_REPAIR_BATCH: i64 = 500;
    const BLIND_INDEX_REPAIR_DONE_KEY: &'static str = "blind_index_repair_stale_postings_v1_done";
    const BLIND_INDEX_REPAIR_CURSOR_KEY: &'static str =
        "blind_index_repair_stale_postings_v1_cursor";
    const BLIND_INDEX_REPAIR_PROCESSED_KEY: &'static str =
        "blind_index_repair_stale_postings_v1_processed";
    const BLIND_INDEX_REPAIR_CHANGED_KEY: &'static str =
        "blind_index_repair_stale_postings_v1_changed";
    const BLIND_INDEX_REPAIR_DELETED_KEY: &'static str =
        "blind_index_repair_stale_postings_v1_deleted";
    const BLIND_INDEX_REPAIR_REMOVED_IDS_KEY: &'static str =
        "blind_index_repair_stale_postings_v1_removed_ids";

    /// Re-arm a full stale-posting scan after cleanup had to delete an indexed
    /// OCR row without knowing which token postings referenced it.
    pub(crate) fn mark_blind_index_repair_needed_on_conn(conn: &Connection) -> Result<(), String> {
        for key in [
            Self::BLIND_INDEX_REPAIR_DONE_KEY,
            Self::BLIND_INDEX_REPAIR_CURSOR_KEY,
            Self::BLIND_INDEX_REPAIR_PROCESSED_KEY,
            Self::BLIND_INDEX_REPAIR_CHANGED_KEY,
            Self::BLIND_INDEX_REPAIR_DELETED_KEY,
            Self::BLIND_INDEX_REPAIR_REMOVED_IDS_KEY,
        ] {
            conn.execute("DELETE FROM app_metadata WHERE key = ?1", [key])
                .map_err(|error| format!("Failed to re-arm blind-index repair: {error}"))?;
        }
        Ok(())
    }

    /// Whether this database still needs the stale-posting repair introduced
    /// after screenshot deletion could outrun OCR bitmap cleanup.
    pub fn is_blind_index_repair_needed(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("blind_index_repair_check")?;
        let conn = guard.as_ref().unwrap();
        let done = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                [Self::BLIND_INDEX_REPAIR_DONE_KEY],
                |_| Ok(true),
            )
            .unwrap_or(false);
        Ok(!done)
    }

    /// Remove every OCR id that no longer belongs to a query-visible row from
    /// each Roaring posting list. The database remains exclusively owned for
    /// the run; progress and the lexical token cursor commit with each batch.
    pub fn run_blind_index_repair<F>(
        &self,
        mut progress_callback: F,
    ) -> Result<BlindIndexRepairProgress, String>
    where
        F: FnMut(BlindIndexRepairProgress),
    {
        let _maintenance = self.database_maintenance("blind_index_repair");
        let guard = self.get_connection_named("blind_index_repair")?;
        let conn = guard.as_ref().unwrap();

        let done = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                [Self::BLIND_INDEX_REPAIR_DONE_KEY],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if done {
            return Ok(BlindIndexRepairProgress::default());
        }

        let metadata_u64 = |key: &str| -> u64 {
            conn.query_row(
                "SELECT value FROM app_metadata WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
        };
        let mut cursor = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key = ?1",
                [Self::BLIND_INDEX_REPAIR_CURSOR_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("Failed to load blind-index repair cursor: {error}"))?
            .unwrap_or_default();

        let mut valid_ocr_ids = roaring::RoaringBitmap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT o.id
                     FROM ocr_results o
                     JOIN screenshots s ON s.id = o.screenshot_id
                     WHERE o.is_deleted = 0 AND s.is_deleted = 0
                     ORDER BY o.id ASC",
                )
                .map_err(|error| format!("Failed to prepare valid OCR id scan: {error}"))?;
            let ids = stmt
                .query_map([], |row| row.get::<_, i64>(0))
                .map_err(|error| format!("Failed to scan valid OCR ids: {error}"))?;
            for id in ids {
                let id = id.map_err(|error| format!("Failed to decode valid OCR id: {error}"))?;
                let bitmap_id = u32::try_from(id)
                    .map_err(|_| format!("OCR row {id} exceeds bitmap id capacity"))?;
                valid_ocr_ids.insert(bitmap_id);
            }
        }

        let mut status = BlindIndexRepairProgress {
            processed_postings: metadata_u64(Self::BLIND_INDEX_REPAIR_PROCESSED_KEY),
            changed_postings: metadata_u64(Self::BLIND_INDEX_REPAIR_CHANGED_KEY),
            deleted_postings: metadata_u64(Self::BLIND_INDEX_REPAIR_DELETED_KEY),
            removed_ocr_ids: metadata_u64(Self::BLIND_INDEX_REPAIR_REMOVED_IDS_KEY),
            ..BlindIndexRepairProgress::default()
        };
        let index = BlindIndex::open(conn)?;
        let remaining = index.count_after(&cursor)?;
        status.total_postings = status
            .processed_postings
            .saturating_add(remaining.max(0) as u64);
        progress_callback(status.clone());

        loop {
            let mut batch = Vec::new();
            for hash in index.tokens_after(&cursor, Self::BLIND_INDEX_REPAIR_BATCH)? {
                let posting = index
                    .read(&hash, false)?
                    .ok_or("Blind posting vanished during exclusive repair")?;
                batch.push((hash, posting.bitmap));
            }

            if batch.is_empty() {
                break;
            }

            let visited = batch.len() as u64;
            let last_hash = batch
                .last()
                .map(|(hash, _)| hash.clone())
                .unwrap_or_default();
            let mut replacements: Vec<(String, roaring::RoaringBitmap)> = Vec::new();
            let mut deletions: Vec<String> = Vec::new();
            let mut removed_in_batch = 0u64;

            for (token_hash, mut bitmap) in batch {
                let before = bitmap.len();
                bitmap &= &valid_ocr_ids;
                let removed = before.saturating_sub(bitmap.len());
                if removed == 0 {
                    continue;
                }
                removed_in_batch = removed_in_batch.saturating_add(removed);
                if bitmap.is_empty() {
                    deletions.push(token_hash);
                } else {
                    replacements.push((token_hash, bitmap));
                }
            }

            let batch_len = replacements.len() + deletions.len();

            let tx = conn.unchecked_transaction().map_err(|error| {
                format!("Failed to start blind-index repair transaction: {error}")
            })?;
            {
                let index = BlindIndex::open(&tx)?;
                let mut stats = MutationStats::default();
                for (hash, bitmap) in &replacements {
                    index.replace(hash, bitmap, &mut stats)?;
                }
                for hash in &deletions {
                    index.replace(hash, &roaring::RoaringBitmap::new(), &mut stats)?;
                }
            }

            status.processed_postings = status.processed_postings.saturating_add(visited);
            status.changed_postings = status.changed_postings.saturating_add(batch_len as u64);
            status.deleted_postings = status
                .deleted_postings
                .saturating_add(deletions.len() as u64);
            status.removed_ocr_ids = status.removed_ocr_ids.saturating_add(removed_in_batch);

            let progress_values = [
                (Self::BLIND_INDEX_REPAIR_CURSOR_KEY, last_hash.clone()),
                (
                    Self::BLIND_INDEX_REPAIR_PROCESSED_KEY,
                    status.processed_postings.to_string(),
                ),
                (
                    Self::BLIND_INDEX_REPAIR_CHANGED_KEY,
                    status.changed_postings.to_string(),
                ),
                (
                    Self::BLIND_INDEX_REPAIR_DELETED_KEY,
                    status.deleted_postings.to_string(),
                ),
                (
                    Self::BLIND_INDEX_REPAIR_REMOVED_IDS_KEY,
                    status.removed_ocr_ids.to_string(),
                ),
            ];
            for (key, value) in progress_values {
                tx.execute(
                    "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, ?2)",
                    params![key, value],
                )
                .map_err(|error| {
                    format!("Failed to persist blind-index repair progress: {error}")
                })?;
            }
            tx.commit()
                .map_err(|error| format!("Failed to commit blind-index repair batch: {error}"))?;

            cursor = last_hash;
            progress_callback(status.clone());
        }

        let tx = conn
            .unchecked_transaction()
            .map_err(|error| format!("Failed to finish blind-index repair: {error}"))?;
        tx.execute(
            "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, '1')",
            [Self::BLIND_INDEX_REPAIR_DONE_KEY],
        )
        .map_err(|error| format!("Failed to mark blind-index repair complete: {error}"))?;
        for key in [
            Self::BLIND_INDEX_REPAIR_CURSOR_KEY,
            Self::BLIND_INDEX_REPAIR_PROCESSED_KEY,
            Self::BLIND_INDEX_REPAIR_CHANGED_KEY,
            Self::BLIND_INDEX_REPAIR_DELETED_KEY,
            Self::BLIND_INDEX_REPAIR_REMOVED_IDS_KEY,
        ] {
            tx.execute("DELETE FROM app_metadata WHERE key = ?1", [key])
                .map_err(|error| format!("Failed to clear blind-index repair cursor: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Failed to commit blind-index repair completion: {error}"))?;

        progress_callback(status.clone());
        Ok(status)
    }

    /// Attempt to start the lazy indexer (and check migration status).
    /// Called after user authentication succeeds.
    pub fn try_bitmap_index_migration(self: &std::sync::Arc<Self>) {
        if self
            .bitmap_index_migrated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        // Spawn a background thread to continually process lazy indexing for backlogged items
        let self_clone = self.clone();
        std::thread::spawn(move || {
            self_clone.run_lazy_indexer_loop();
        });
    }

    /// Background loop that periodically processes unindexed ocr_results.
    fn run_lazy_indexer_loop(&self) {
        tracing::info!("[LAZY_INDEXER] Started background thread for lazy indexing.");
        loop {
            if self.lazy_indexer_shutdown.load(Ordering::SeqCst) {
                tracing::info!("[LAZY_INDEXER] Shutting down background thread.");
                break;
            }

            if !*self.initialized.lock().unwrap_or_else(|e| e.into_inner()) {
                std::thread::sleep(std::time::Duration::from_millis(2000));
                continue;
            }

            if crate::maintenance::is_active() {
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            std::thread::sleep(std::time::Duration::from_millis(1000));

            // Process unindexed rows (text_hash = '') even if a full migration (old hashes -> HMAC) is pending.
            // This ensures new snapshots are searchable immediately during the migration process.
            match self.process_lazy_indexing_batch() {
                Ok(processed) => {
                    if processed == 0 {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                }
                Err(e) => {
                    tracing::warn!("[LAZY_INDEXER] Batch error: {}", e);
                    std::thread::sleep(std::time::Duration::from_secs(10));
                }
            }
        }
    }

    /// Process a batch of unindexed OCR results (where text_hash is empty).
    pub fn process_lazy_indexing_batch(&self) -> Result<usize, String> {
        let hmac_key = self.credential_state.get_hmac_key()?;

        // 1. Fetch rows that have NO hash (newly captured during this version)
        let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = {
            let guard = self.get_connection_named("lazy_indexer_read")?;
            let conn = guard.as_ref().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT id, text_enc, text_key_encrypted
                     FROM ocr_results
                     WHERE text_hash = '' AND is_deleted = 0
                     ORDER BY id ASC
                     LIMIT ?1",
                )
                .map_err(|e| format!("lazy prepare: {}", e))?;
            let mapped = stmt
                .query_map(params![Self::LAZY_INDEXING_BATCH], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })
                .map_err(|e| format!("lazy query: {}", e))?;
            mapped.filter_map(|r| r.ok()).collect()
        };

        if rows.is_empty() {
            return Ok(0);
        }

        self.index_batch_internal(rows, &hmac_key)
    }

    fn visible_ocr_bitmap_sql(id_count: usize) -> String {
        let placeholders = (0..id_count).map(|_| "?").collect::<Vec<_>>().join(",");
        // Without statistics SQLite can scan every live entry in
        // idx_ocr_deleted_screenshot, then filter the batch's ids. NOT INDEXED
        // excludes that secondary index but preserves INTEGER PRIMARY KEY
        // lookups, keeping the work proportional to the batch size.
        format!(
            "SELECT o.id
             FROM ocr_results o NOT INDEXED
             JOIN screenshots s ON s.id = o.screenshot_id
             WHERE o.id IN ({placeholders})
               AND o.is_deleted = 0
               AND s.is_deleted = 0"
        )
    }

    fn query_visible_ocr_bitmap(
        conn: &Connection,
        ids: &[i64],
        context: &str,
    ) -> Result<roaring::RoaringBitmap, String> {
        let mut active = roaring::RoaringBitmap::new();
        if ids.is_empty() {
            return Ok(active);
        }

        let sql = Self::visible_ocr_bitmap_sql(ids.len());
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|error| format!("{context} active-row prepare: {error}"))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| format!("{context} active-row query: {error}"))?;
        for id in rows {
            let id = id.map_err(|error| format!("{context} active-row decode: {error}"))?;
            let bitmap_id = u32::try_from(id)
                .map_err(|_| format!("OCR row {id} exceeds bitmap id capacity"))?;
            active.insert(bitmap_id);
        }
        Ok(active)
    }

    /// Internal helper to re-index a batch of rows.
    pub(crate) fn index_batch_internal(
        &self,
        rows: Vec<(i64, Vec<u8>, Vec<u8>)>,
        hmac_key: &[u8],
    ) -> Result<usize, String> {
        let mut batch_tokens: std::collections::HashMap<String, roaring::RoaringBitmap> =
            std::collections::HashMap::new();
        let mut row_hashes: Vec<(i64, String)> = Vec::new();

        for (ocr_id, text_enc, text_key_enc) in &rows {
            let plaintext = match self.decrypt_payload_with_row_key(text_enc, text_key_enc) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            let text_hash = Self::compute_hmac_hash(&plaintext, hmac_key);
            row_hashes.push((*ocr_id, text_hash));

            let bigrams = Self::bigram_tokenize(&plaintext);
            for token in bigrams {
                let token_hash = Self::compute_hmac_hash(&token, hmac_key);
                batch_tokens
                    .entry(token_hash)
                    .or_insert_with(roaring::RoaringBitmap::new)
                    .insert(*ocr_id as u32);
            }
        }

        // 3. Update DB
        // Traverse the token index in key order instead of HashMap order. Keep
        // only one posting blob resident in Rust while SQLite reuses its pages.
        let ordered_hashes = postings_io::ordered_token_hashes(batch_tokens.keys());
        let mut diagnostics = LazyIndexWriteDiagnostics::default();
        let mut posting_stats = MutationStats::default();
        let wait_started = Instant::now();
        let mut guard = self.get_connection_named("lazy_indexer_write")?;
        let mutex_wait = wait_started.elapsed();
        // Use the same start as the generic mutex-hold warning.
        let hold_started = guard.acquired_at;
        let write_result = (|| -> Result<(), String> {
            let conn = guard.as_mut().unwrap();
            let cache_budget = measure_lazy_index_step(&mut diagnostics.cache_setup, || {
                postings_io::PostingsCacheBudget::new(conn)
            })
            .map_err(|e| format!("lazy cache: {}", e))?;
            // The cache guard borrows the connection through commit/rollback.
            // This mutex-owned connection has no concurrent transaction users;
            // unchecked_transaction still rejects a nested BEGIN at runtime.
            let tx =
                measure_lazy_index_step(&mut diagnostics.tx_begin, || conn.unchecked_transaction())
                    .map_err(|e| format!("lazy tx: {}", e))?;

            let active_ids = measure_lazy_index_step(&mut diagnostics.active_check, || {
                let ids: Vec<i64> = row_hashes.iter().map(|(id, _)| *id).collect();
                Self::query_visible_ocr_bitmap(&tx, &ids, "lazy index")
            })?;
            diagnostics.active_rows = active_ids.len();

            // Update text_hash
            measure_lazy_index_step(&mut diagnostics.hash_update, || -> Result<(), String> {
                let mut upd_stmt = tx
                    .prepare_cached(
                        "UPDATE ocr_results SET text_hash = ?1
                         WHERE id = ?2 AND is_deleted = 0",
                    )
                    .map_err(|e| format!("lazy upd prep: {}", e))?;
                for (id, hash) in &row_hashes {
                    upd_stmt
                        .execute(params![hash, id])
                        .map_err(|e| format!("lazy text hash update: {e}"))?;
                }
                Ok(())
            })?;

            let index =
                measure_lazy_index_step(&mut diagnostics.bitmap_prepare, || BlindIndex::open(&tx))?;
            for hash in &ordered_hashes {
                let candidate_bitmap = &batch_tokens[*hash];
                let new_bitmap = measure_lazy_index_step(&mut diagnostics.bitmap_filter, || {
                    candidate_bitmap & &active_ids
                });
                if !new_bitmap.is_empty() {
                    index.add(hash, &new_bitmap, &mut posting_stats)?;
                }
            }

            measure_lazy_index_step(&mut diagnostics.commit, || tx.commit())
                .map_err(|e| format!("lazy commit: {}", e))?;
            drop(cache_budget);
            Ok(())
        })();
        diagnostics.bitmap_read = posting_stats.read;
        diagnostics.bitmap_merge = posting_stats.merge;
        diagnostics.bitmap_serialize = posting_stats.serialize;
        diagnostics.bitmap_write = posting_stats.write;
        diagnostics.existing_postings = posting_stats.existing_tokens;
        diagnostics.postings_written = posting_stats.fragments_written;
        diagnostics.postings_bytes_read = posting_stats.bytes_read;
        diagnostics.postings_bytes_written = posting_stats.bytes_written;
        let hold = hold_started.elapsed();
        drop(guard);

        // Emit after releasing the connection, including on transaction failure.
        // Only aggregate timings and sizes are logged, never OCR text or hashes.
        if hold >= Duration::from_secs(1) || mutex_wait >= Duration::from_secs(1) {
            tracing::warn!(
                rows = rows.len(),
                prepared_rows = row_hashes.len(),
                active_rows = diagnostics.active_rows,
                tokens = batch_tokens.len(),
                existing_postings = diagnostics.existing_postings,
                postings_written = diagnostics.postings_written,
                postings_bytes_read = diagnostics.postings_bytes_read,
                postings_bytes_written = diagnostics.postings_bytes_written,
                committed = write_result.is_ok(),
                ?mutex_wait,
                ?hold,
                cache_setup = ?diagnostics.cache_setup,
                tx_begin = ?diagnostics.tx_begin,
                active_check = ?diagnostics.active_check,
                hash_update = ?diagnostics.hash_update,
                bitmap_prepare = ?diagnostics.bitmap_prepare,
                bitmap_filter = ?diagnostics.bitmap_filter,
                bitmap_read = ?diagnostics.bitmap_read,
                bitmap_merge = ?diagnostics.bitmap_merge,
                bitmap_serialize = ?diagnostics.bitmap_serialize,
                bitmap_write = ?diagnostics.bitmap_write,
                commit = ?diagnostics.commit,
                "[DIAG:DB] lazy_indexer_write timing"
            );
        }
        write_result?;

        Ok(rows.len())
    }

    /// Internal helper to re-index a batch using a provided connection.
    pub(crate) fn index_batch_internal_on_conn(
        &self,
        conn: &Connection,
        rows: Vec<(i64, Vec<u8>, Vec<u8>)>,
        hmac_key: &[u8],
    ) -> Result<(), String> {
        let mut batch_tokens: std::collections::HashMap<String, roaring::RoaringBitmap> =
            std::collections::HashMap::new();
        let mut row_hashes: Vec<(i64, String)> = Vec::new();

        for (ocr_id, text_enc, text_key_enc) in &rows {
            let plaintext = match self.decrypt_payload_with_row_key(text_enc, text_key_enc) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };

            let text_hash = Self::compute_hmac_hash(&plaintext, hmac_key);
            row_hashes.push((*ocr_id, text_hash));

            let bigrams = Self::bigram_tokenize(&plaintext);
            for token in bigrams {
                let token_hash = Self::compute_hmac_hash(&token, hmac_key);
                batch_tokens
                    .entry(token_hash)
                    .or_insert_with(roaring::RoaringBitmap::new)
                    .insert(*ocr_id as u32);
            }
        }

        // Atomic update for the batch
        let _cache_budget = postings_io::PostingsCacheBudget::new(conn)
            .map_err(|e| format!("HMAC index cache: {e}"))?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        {
            let ids: Vec<i64> = row_hashes.iter().map(|(id, _)| *id).collect();
            let active_ids = Self::query_visible_ocr_bitmap(&tx, &ids, "HMAC index")?;

            let mut upd_stmt = tx
                .prepare_cached(
                    "UPDATE ocr_results SET text_hash = ?1
                     WHERE id = ?2 AND is_deleted = 0",
                )
                .map_err(|e| e.to_string())?;
            for (id, hash) in &row_hashes {
                upd_stmt
                    .execute(params![hash, id])
                    .map_err(|e| format!("HMAC text hash update: {e}"))?;
            }

            let index = BlindIndex::open(&tx)?;
            let mut stats = MutationStats::default();
            for hash in postings_io::ordered_token_hashes(batch_tokens.keys()) {
                let new_bitmap = &batch_tokens[hash] & &active_ids;
                if !new_bitmap.is_empty() {
                    index.add(hash, &new_bitmap, &mut stats)?;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use roaring::RoaringBitmap;
    use std::sync::Arc;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    fn serialize(ids: &[u32]) -> Vec<u8> {
        let bitmap: RoaringBitmap = ids.iter().copied().collect();
        let mut blob = Vec::new();
        bitmap.serialize_into(&mut blob).expect("serialize bitmap");
        blob
    }

    #[test]
    fn blind_index_repair_removes_only_non_query_visible_ids() {
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let connection = guard.as_ref().expect("database");
            connection
                .execute_batch(
                    "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                        (1, '1.enc', 'h1', 0),
                        (2, '2.enc', 'h2', 1),
                        (3, '3.enc', 'h3', 0),
                        (4, '4.enc', 'h4', 1);
                     INSERT INTO ocr_results (id, screenshot_id, text_hash, is_deleted) VALUES
                        (1, 1, 'a', 0),
                        (2, 2, 'b', 1),
                        (3, 3, 'c', 0),
                        (4, 4, 'd', 0);",
                )
                .expect("insert OCR fixture");
            connection
                .execute(
                    "INSERT INTO blind_bitmap_inline (token_hash, postings_blob) VALUES (?1, ?2)",
                    params!["active-and-stale", serialize(&[1, 2, 4, 99])],
                )
                .expect("insert mixed posting");
            connection
                .execute(
                    "INSERT INTO blind_bitmap_inline (token_hash, postings_blob) VALUES (?1, ?2)",
                    params!["active-only", serialize(&[3])],
                )
                .expect("insert active posting");
            connection
                .execute(
                    "INSERT INTO blind_bitmap_inline (token_hash, postings_blob) VALUES (?1, ?2)",
                    params!["stale-only", serialize(&[2, 99])],
                )
                .expect("insert stale posting");
        }

        assert!(storage.is_blind_index_repair_needed().unwrap());
        let mut snapshots = Vec::new();
        let summary = storage
            .run_blind_index_repair(|progress| snapshots.push(progress))
            .expect("repair blind index");

        assert_eq!(summary.processed_postings, 3);
        assert_eq!(summary.total_postings, 3);
        assert_eq!(summary.changed_postings, 2);
        assert_eq!(summary.deleted_postings, 1);
        assert_eq!(summary.removed_ocr_ids, 5);
        assert!(snapshots.len() >= 2);
        assert!(!storage.is_blind_index_repair_needed().unwrap());

        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let connection = guard.as_ref().expect("database");
        let mixed: Vec<u8> = connection
            .query_row(
                "SELECT postings_blob FROM blind_bitmap_inline WHERE token_hash = 'active-and-stale'",
                [],
                |row| row.get(0),
            )
            .expect("mixed posting survives");
        let mixed = RoaringBitmap::deserialize_from(&mixed[..]).expect("decode mixed posting");
        assert_eq!(mixed.iter().collect::<Vec<_>>(), vec![1]);

        let active: Vec<u8> = connection
            .query_row(
                "SELECT postings_blob FROM blind_bitmap_inline WHERE token_hash = 'active-only'",
                [],
                |row| row.get(0),
            )
            .expect("active posting survives");
        let active = RoaringBitmap::deserialize_from(&active[..]).expect("decode active posting");
        assert_eq!(active.iter().collect::<Vec<_>>(), vec![3]);

        let stale_exists: bool = connection
            .query_row(
                "SELECT 1 FROM blind_bitmap_inline WHERE token_hash = 'stale-only'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(!stale_exists);
    }

    #[test]
    fn query_visible_ocr_bitmap_rechecks_both_row_and_parent_state() {
        let (_temp, storage) = test_storage();
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let connection = guard.as_ref().expect("database");
        connection
            .execute_batch(
                "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                    (10, '10.enc', 'h10', 0),
                    (11, '11.enc', 'h11', 1),
                    (12, '12.enc', 'h12', 0);
                 INSERT INTO ocr_results (id, screenshot_id, text_hash, is_deleted) VALUES
                    (100, 10, '', 0),
                    (101, 11, '', 0),
                    (102, 12, '', 1);",
            )
            .expect("insert visibility fixture");

        let active =
            StorageState::query_visible_ocr_bitmap(connection, &[100, 101, 102, 999], "test")
                .expect("query visible OCR ids");
        assert_eq!(active.iter().collect::<Vec<_>>(), vec![100]);
    }

    #[test]
    fn blind_index_repair_cleans_chunked_postings_and_preserves_logical_counts() {
        let (_temp, storage) = test_storage();
        let original: RoaringBitmap = (0..70_000).map(|n| n * 37).collect();
        {
            let guard = storage.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            conn.execute_batch("INSERT INTO screenshots(id,image_path,image_hash,is_deleted) VALUES(1,'1.enc','h1',0),(2,'2.enc','h2',1);
                INSERT INTO ocr_results(id,screenshot_id,text_hash,is_deleted) VALUES(37,1,'a',0),(74,2,'b',0),(111,1,'c',1);").unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            let index = BlindIndex::open(&tx).unwrap();
            let mut stats = MutationStats::default();
            index.replace("mixed", &original, &mut stats).unwrap();
            let mut stale = original.clone();
            stale.remove(37);
            index.replace("stale", &stale, &mut stats).unwrap();
            tx.commit().unwrap();
        }
        let summary = storage.run_blind_index_repair(|_| {}).unwrap();
        assert_eq!(summary.total_postings, 2);
        assert_eq!(summary.processed_postings, 2);
        assert_eq!(summary.changed_postings, 2);
        assert_eq!(summary.deleted_postings, 1);
        assert_eq!(summary.removed_ocr_ids, (original.len() - 1) * 2);
        assert!(!storage.is_blind_index_repair_needed().unwrap());
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        let index = BlindIndex::open(conn).unwrap();
        assert_eq!(index.count_after("").unwrap(), 1);
        assert!(index.read("stale", true).unwrap().is_none());
        assert_eq!(
            index.read("mixed", true).unwrap().unwrap().bitmap,
            [37].into_iter().collect()
        );
        let probe = index.probe(&["mixed".into()]).unwrap().postings.remove(0);
        assert!(probe.chunked);
        assert_eq!(probe.cardinality, Some(1));
        assert_eq!(probe.bytes, serialize(&[37]).len());
    }

    #[test]
    fn visible_ocr_lookup_uses_primary_keys_with_bounded_work() {
        let (_temp, storage) = test_storage();
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        conn.execute_batch(
            "WITH RECURSIVE ids(id) AS (
                SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < 10000
             )
             INSERT INTO screenshots (id, image_path, image_hash)
             SELECT id, 'history.enc', CAST(id AS TEXT) FROM ids;
             INSERT INTO ocr_results (id, screenshot_id, text_hash)
             SELECT id, id, '' FROM screenshots;",
        )
        .expect("insert active OCR history");

        // Use the production schema and bundled SQLite engine. Without
        // statistics, the old query scans idx_ocr_deleted_screenshot for
        // is_deleted=0 even though only this batch's primary keys are needed.
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").expect("analyze fixture");
            }
            for first_id in [9901, 10001] {
                let ids: Vec<i64> = (first_id..first_id + 100).collect();
                let sql = StorageState::visible_ocr_bitmap_sql(ids.len());
                let plan = conn
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .expect("prepare visibility query plan")
                    .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                        row.get::<_, String>(3)
                    })
                    .expect("explain visibility query")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("read visibility query plan")
                    .join("; ");

                let mut stmt = conn.prepare(&sql).expect("prepare visibility query");
                let mut actual = stmt
                    .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                        row.get::<_, i64>(0)
                    })
                    .expect("query visible OCR ids")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("read visible OCR ids");
                actual.sort_unstable();
                let expected = if first_id == 9901 {
                    ids.clone()
                } else {
                    vec![]
                };
                assert_eq!(actual, expected);

                // VM steps catch an index-range scan too, which SQLite's
                // FullscanStep counter alone does not report as a table scan.
                let steps = stmt.get_status(rusqlite::StatementStatus::VmStep);
                assert!(
                    plan.contains("SEARCH o USING INTEGER PRIMARY KEY")
                        && plan.contains("SEARCH s USING INTEGER PRIMARY KEY"),
                    "analyzed={analyzed}, first_id={first_id}, steps={steps}: {plan}"
                );
                assert!(
                    steps < 50 * ids.len() as i32 + 100,
                    "batch lookup must not scan OCR history: analyzed={analyzed}, first_id={first_id}, steps={steps}"
                );
            }
        }
    }
}
