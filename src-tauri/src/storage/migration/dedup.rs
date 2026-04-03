//! Dedup migration: move inline data to dedup tables.

use rusqlite::params;
use std::sync::atomic::Ordering;
use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};

use super::super::StorageState;

impl StorageState {
    /// Marker key in app_metadata for dedup migration completion.
    const DEDUP_MIGRATION_DONE_KEY: &'static str = "dedup_migration_done";

    /// Migrate existing inline page_icon_enc / visible_links_enc data into dedup tables.
    ///
    /// Processes rows in batches: for each row with inline data but no dedup reference,
    /// decrypts the inline blob, creates/reuses a dedup entry, sets the FK, and NULLs
    /// the inline column. Safe to call multiple times (idempotent).
    pub fn migrate_inline_to_dedup(&self) -> Result<(usize, usize), String> {
        // Check persistent completion marker
        {
            let guard = self.get_connection_named("dedup_migrate_check")?;
            let conn = guard.as_ref().unwrap();
            let done: bool = conn
                .query_row(
                    "SELECT 1 FROM app_metadata WHERE key = ?1",
                    params![Self::DEDUP_MIGRATION_DONE_KEY],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if done {
                return Ok((0, 0));
            }
        }

        const BATCH_SIZE: i64 = 100;
        let mut migrated_icons: usize = 0;
        let mut migrated_links: usize = 0;
        let mut has_errors = false;

        loop {
            let mut guard = self.get_connection_named("migrate_inline_to_dedup")?;
            let conn = guard.as_mut().unwrap();

            // Fetch a batch of rows that still have inline page_icon_enc but no dedup ref
            let mut stmt = conn
                .prepare(
                    "SELECT id, page_icon_enc, content_key_encrypted
                     FROM screenshots
                     WHERE page_icon_enc IS NOT NULL AND page_icon_id IS NULL
                     LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare migration query (icons): {}", e))?;

            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
                .query_map(params![BATCH_SIZE], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| format!("Failed to query migration rows (icons): {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            if rows.is_empty() {
                break;
            }

            for (id, icon_enc, content_key_enc) in &rows {
                // Decrypt inline data using the screenshot's row key
                let row_key = match decrypt_row_key_with_cng(content_key_enc) {
                    Ok(k) => k,
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt row key for screenshot id={}: {}", id, e);
                        has_errors = true;
                        continue;
                    }
                };
                let plaintext = match decrypt_with_master_key(&row_key, icon_enc) {
                    Ok(d) => match String::from_utf8(d) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("migrate_inline_to_dedup: invalid utf8 in page_icon for id={}: {}", id, e);
                            has_errors = true;
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt page_icon for id={}: {}", id, e);
                        has_errors = true;
                        continue;
                    }
                };

                // Create or reuse page_icon entry
                match Self::get_or_create_page_icon_id(conn, &plaintext) {
                    Ok(icon_id) => {
                        // Update screenshot and NULL out the inline column
                        if let Err(e) = conn.execute(
                            "UPDATE screenshots SET page_icon_id = ?, page_icon_enc = NULL WHERE id = ?",
                            params![icon_id, id],
                        ) {
                            tracing::warn!("migrate_inline_to_dedup: failed to update screenshot id={}: {}", id, e);
                            has_errors = true;
                        } else {
                            migrated_icons += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to create page_icon for screenshot id={}: {}", id, e);
                        has_errors = true;
                    }
                }
            }

            // If we got fewer than BATCH_SIZE, we're done
            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }

        // Now migrate visible_links_enc
        loop {
            let mut guard = self.get_connection_named("migrate_inline_to_dedup")?;
            let conn = guard.as_mut().unwrap();

            let mut stmt = conn
                .prepare(
                    "SELECT id, visible_links_enc, content_key_encrypted
                     FROM screenshots
                     WHERE visible_links_enc IS NOT NULL AND link_set_id IS NULL
                     LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare migration query (links): {}", e))?;

            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
                .query_map(params![BATCH_SIZE], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| format!("Failed to query migration rows (links): {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            if rows.is_empty() {
                break;
            }

            for (id, links_enc, content_key_enc) in &rows {
                let row_key = match decrypt_row_key_with_cng(content_key_enc) {
                    Ok(k) => k,
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt row key for screenshot id={}: {}", id, e);
                        has_errors = true;
                        continue;
                    }
                };
                let plaintext = match decrypt_with_master_key(&row_key, links_enc) {
                    Ok(d) => match String::from_utf8(d) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("migrate_inline_to_dedup: invalid utf8 in link_set for id={}: {}", id, e);
                            has_errors = true;
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt link_set for id={}: {}", id, e);
                        has_errors = true;
                        continue;
                    }
                };

                let links: Vec<super::super::VisibleLink> = match serde_json::from_str(&plaintext) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to parse links JSON for id={}: {}", id, e);
                        has_errors = true;
                        continue;
                    }
                };

                // If empty, just NULL out the inline column and continue
                if links.is_empty() {
                    if let Err(e) = conn.execute(
                        "UPDATE screenshots SET visible_links_enc = NULL WHERE id = ?",
                        params![id],
                    ) {
                        tracing::warn!("migrate_inline_to_dedup: failed to clear empty links for id={}: {}", id, e);
                        has_errors = true;
                    }
                    continue;
                }

                // Create or reuse link_set entry
                match Self::get_or_create_link_set_id(conn, &links) {
                    Ok(link_set_id) => {
                        if let Err(e) = conn.execute(
                            "UPDATE screenshots SET link_set_id = ?, visible_links_enc = NULL WHERE id = ?",
                            params![link_set_id, id],
                        ) {
                            tracing::warn!("migrate_inline_to_dedup: failed to update screenshot id={}: {}", id, e);
                            has_errors = true;
                        } else {
                            migrated_links += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to create link_set for screenshot id={}: {}", id, e);
                        has_errors = true;
                    }
                }
            }

            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }

        if migrated_icons > 0 || migrated_links > 0 {
            tracing::info!(
                "[DEDUP:MIGRATE] Migrated {} page_icons, {} link_sets from inline to dedup tables",
                migrated_icons,
                migrated_links
            );
        }

        // Migration complete, write success marker (even if some rows were skipped due to errors)
        {
            if has_errors {
                tracing::warn!("[DEDUP:MIGRATE] Completed with some errors; skipped rows will not be retried.");
            }
            let guard = self.get_connection_named("dedup_migrate_mark")?;
            let conn = guard.as_ref().unwrap();
            let _ = conn.execute(
                "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, '1')",
                params![Self::DEDUP_MIGRATION_DONE_KEY],
            );
        }

        Ok((migrated_icons, migrated_links))
    }

    /// Attempt dedup migration if not already done this session.
    /// Should be called after user authentication succeeds.
    /// Uses compare_exchange to ensure it only runs once.
    pub fn try_dedup_migration(&self) {
        // Only run once per session
        if self
            .dedup_migrated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let t0 = std::time::Instant::now();
        match self.migrate_inline_to_dedup() {
            Ok((icons, links)) => {
                if icons > 0 || links > 0 {
                    tracing::info!(
                        "[DEDUP:MIGRATE] Completed in {:?} ({} icons, {} link_sets)",
                        t0.elapsed(),
                        icons,
                        links
                    );
                }
            }
            Err(e) => {
                tracing::warn!("[DEDUP:MIGRATE] Migration failed (non-fatal): {}", e);
                // Reset flag so it can be retried on next auth
                self.dedup_migrated.store(false, Ordering::SeqCst);
            }
        }
    }
}
