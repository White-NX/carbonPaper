//! HMAC v2 migration for existing OCR results.

use rusqlite::params;
use std::sync::atomic::Ordering;
use super::super::StorageState;

impl StorageState {
    /// Marker key in app_metadata for bitmap HMAC v2 migration completion.
    pub(crate) const BITMAP_MIGRATION_DONE_KEY: &'static str = "hmac_v2_migration_done";
    /// Cursor key to track progress of HMAC v2 migration.
    pub(crate) const BITMAP_MIGRATION_CURSOR_KEY: &'static str = "hmac_v2_migration_cursor";

    /// Check if HMAC v2 migration is needed.
    pub fn check_hmac_migration_status(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("check_hmac_migration")?;
        let conn = guard.as_ref().unwrap();

        // 1. Check persistent marker
        let done: bool = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                params![Self::BITMAP_MIGRATION_DONE_KEY],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if done { return Ok(false); }

        // 2. Check if there is anything to migrate (old hashes)
        // Rows with text_hash = '' are newly captured and will be indexed by lazy indexer.
        // We only need full migration if there are rows with EXISTING non-HMAC hashes.
        let has_work: bool = conn
            .query_row(
                "SELECT 1 FROM ocr_results WHERE text_hash != '' LIMIT 1",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);

        Ok(has_work)
    }

    /// Run full HMAC v2 migration on existing data using a cursor.
    pub fn run_hmac_migration<F>(&self, mut progress_callback: F) -> Result<(), String>
    where
        F: FnMut(&str, usize, usize),
    {
        if self
            .hmac_migration_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ALREADY_RUNNING".to_string());
        }

        // Reset cancellation flag
        self.hmac_migration_cancel_requested.store(false, Ordering::SeqCst);

        let result = self.run_hmac_migration_internal(&mut progress_callback);

        self.hmac_migration_in_progress.store(false, Ordering::SeqCst);
        result
    }

    fn run_hmac_migration_internal<F>(&self, progress_callback: &mut F) -> Result<(), String>
    where
        F: FnMut(&str, usize, usize),
    {
        tracing::info!("[HMAC_MIGRATE] Starting high-concurrency cursor migration...");
        if !self.check_hmac_migration_status()? {
            return Ok(());
        }

        // Use cached total count - instant
        let total_rows = self.ocr_row_count.load(Ordering::Relaxed) as usize;
        let hmac_key = self.credential_state.get_hmac_key()?;

        // 1. Get current cursor (Read BEFORE potential clear)
        let mut cursor: i64 = {
            let guard = self.get_connection_named("hmac_migrate_get_cursor")?;
            let conn = guard.as_ref().unwrap();
            conn.query_row(
                "SELECT value FROM app_metadata WHERE key = ?1",
                params![Self::BITMAP_MIGRATION_CURSOR_KEY],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
        };

        // 2. Clear existing index only if starting from scratch (Instant)
        // Note: Wiping is disabled to avoid "blacking out" new snapshots indexed by the lazy indexer
        // during long migrations. Old (non-HMAC) entries will remain but won't be hit by HMAC queries.
        if cursor == 0 {
            tracing::info!("[HMAC_MIGRATE] Cursor is 0, starting fresh migration.");
        } else {
            tracing::info!("[HMAC_MIGRATE] Resuming migration from cursor: {}", cursor);
        }

        // Use cursor as an instant estimate for 'processed'
        let mut processed = cursor as usize;
        progress_callback("indexing", processed, total_rows);

        // 3. Migration loop with explicit lock yielding
        const MIGRATE_BATCH_SIZE: i64 = 500;
        loop {
            if self.is_hmac_migration_cancel_requested() {
                return Err("Cancelled".to_string());
            }

            // CRITICAL: Scope the MutexGuard to the smallest possible area
            let batch_result = {
                let guard = self.get_connection_named("hmac_migrate_batch")?;
                let conn = guard.as_ref().unwrap();

                // A. FETCH
                let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = {
                    let mut stmt = conn.prepare(
                        "SELECT id, text_enc, text_key_encrypted FROM ocr_results WHERE id > ?1 ORDER BY id ASC LIMIT ?2"
                    ).map_err(|e| e.to_string())?;

                    let mapped = stmt.query_map(params![cursor, MIGRATE_BATCH_SIZE], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, Vec<u8>>(2)?))
                    }).map_err(|e| e.to_string())?;

                    mapped.filter_map(|r| r.ok()).collect()
                };

                if rows.is_empty() {
                    Ok::<Option<(i64, usize)>, String>(None)
                } else {
                    let batch_len = rows.len();
                    let last_id_in_batch = rows.last().unwrap().0;

                    // B. INDEXING
                    self.index_batch_internal_on_conn(conn, rows, &hmac_key)?;

                    // C. UPDATE CURSOR
                    conn.execute(
                        "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, ?2)",
                        params![Self::BITMAP_MIGRATION_CURSOR_KEY, last_id_in_batch.to_string()],
                    ).ok();

                    Ok::<Option<(i64, usize)>, String>(Some((last_id_in_batch, batch_len)))
                }
                // MutexGuard 'guard' is DROPPED here automatically at the end of this block.
            }?;

            match batch_result {
                Some((new_cursor, count)) => {
                    cursor = new_cursor;
                    processed += count;
                    progress_callback("indexing", processed, total_rows);

                    // D. YIELD - The Mutex is now FREE. UI threads and Capture threads can jump in!
                    std::thread::sleep(std::time::Duration::from_millis(200));

                    if processed % 10000 < MIGRATE_BATCH_SIZE as usize {
                        tracing::info!("[HMAC_MIGRATE] Progress: {} / {} (ID: {})", processed, total_rows, cursor);
                    }
                }
                None => break,
            }
        }

        // 4. Mark as done (needs its own short-lived lock)
        {
            let guard = self.get_connection_named("hmac_migrate_done")?;
            let conn = guard.as_ref().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, '1')",
                params![Self::BITMAP_MIGRATION_DONE_KEY],
            )
            .ok();
            conn.execute(
                "DELETE FROM app_metadata WHERE key = ?1",
                params![Self::BITMAP_MIGRATION_CURSOR_KEY],
            )
            .ok();
        }

        tracing::info!("[HMAC_MIGRATE] Migration completed successfully.");
        Ok(())
    }
}
