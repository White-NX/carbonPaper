//! Bitmap index lazy indexing and maintenance.

use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;
use super::super::StorageState;

impl StorageState {
    /// Number of OCR rows to process per batch for lazy indexing.
    const LAZY_INDEXING_BATCH: i64 = 100;

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
                    "SELECT id, text_enc, text_key_encrypted FROM ocr_results WHERE text_hash = '' ORDER BY id ASC LIMIT ?1",
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

    /// Internal helper to re-index a batch of rows.
    pub(crate) fn index_batch_internal(&self, rows: Vec<(i64, Vec<u8>, Vec<u8>)>, hmac_key: &[u8]) -> Result<usize, String> {
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
        {
            let mut guard = self.get_connection_named("lazy_indexer_write")?;
            let conn = guard.as_mut().unwrap();
            let tx = conn
                .transaction()
                .map_err(|e| format!("lazy tx: {}", e))?;

            // Update text_hash
            {
                let mut upd_stmt = tx.prepare_cached(
                    "UPDATE ocr_results SET text_hash = ?1 WHERE id = ?2"
                ).map_err(|e| format!("lazy upd prep: {}", e))?;
                for (id, hash) in &row_hashes {
                    upd_stmt.execute(params![hash, id]).ok();
                }
            }

            // Update blind_bitmap_index
            {
                let mut get_stmt = tx
                    .prepare_cached(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                    )
                    .map_err(|e| format!("lazy get prep: {}", e))?;
                let mut put_stmt = tx
                    .prepare_cached(
                        "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)",
                    )
                    .map_err(|e| format!("lazy put prep: {}", e))?;

                for (hash, new_bitmap) in &batch_tokens {
                    let existing_blob: Option<Vec<u8>> = get_stmt
                        .query_row(params![hash], |row| row.get(0))
                        .optional()
                        .map_err(|e| format!("lazy get: {}", e))?;

                    let merged = if let Some(blob) = existing_blob {
                        let mut existing =
                            roaring::RoaringBitmap::deserialize_from(&blob[..])
                                .map_err(|e| format!("lazy deser: {}", e))?;
                        existing |= new_bitmap;
                        existing
                    } else {
                        new_bitmap.clone()
                    };

                    let mut buf = Vec::new();
                    merged
                        .serialize_into(&mut buf)
                        .map_err(|e| format!("lazy ser: {}", e))?;
                    put_stmt
                        .execute(params![hash, buf])
                        .map_err(|e| format!("lazy put: {}", e))?;
                }
            }

            tx.commit()
                .map_err(|e| format!("lazy commit: {}", e))?;
        }

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
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        {
            let mut upd_stmt = tx
                .prepare_cached("UPDATE ocr_results SET text_hash = ?1 WHERE id = ?2")
                .map_err(|e| e.to_string())?;
            for (id, hash) in &row_hashes {
                let _ = upd_stmt.execute(params![hash, id]);
            }

            let mut get_stmt = tx
                .prepare_cached("SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1")
                .map_err(|e| e.to_string())?;
            let mut put_stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)",
                )
                .map_err(|e| e.to_string())?;

            for (hash, new_bitmap) in &batch_tokens {
                let existing_blob: Option<Vec<u8>> = get_stmt
                    .query_row(params![hash], |row| row.get(0))
                    .optional()
                    .unwrap_or(None);
                let merged = if let Some(blob) = existing_blob {
                    if let Ok(mut existing) = roaring::RoaringBitmap::deserialize_from(&blob[..]) {
                        existing |= new_bitmap;
                        existing
                    } else {
                        new_bitmap.clone()
                    }
                } else {
                    new_bitmap.clone()
                };

                let mut buf = Vec::new();
                if merged.serialize_into(&mut buf).is_ok() {
                    let _ = put_stmt.execute(params![hash, buf]);
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}
