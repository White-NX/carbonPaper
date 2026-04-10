//! List distinct processes (two-phase: SQL aggregation + offline decryption of old records).

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};

use super::{
    ProcessMonthlyThumbnailItem, ProcessMonthlyThumbnailPage, ProcessStorageStat, StorageState,
};

impl StorageState {
    /// List distinct process names with counts (two-phase: SQL aggregation + offline decryption).
    pub fn list_distinct_processes(&self) -> Result<Vec<(String, i64)>, String> {
        let fn_start = std::time::Instant::now();

        // Phase 1: SQL aggregation + extract encrypted-only rows (hold mutex)
        let (mut counts, encrypted_rows): (
            std::collections::HashMap<String, i64>,
            Vec<(Option<Vec<u8>>, Option<Vec<u8>>)>,
        ) = {
            let guard = self.get_connection_named("list_distinct_processes")?;
            let conn = guard.as_ref().unwrap();

            // Fast path: aggregate plaintext process_name via SQL
            let mut counts = std::collections::HashMap::new();
            let mut stmt = conn
                .prepare(
                    "SELECT process_name, COUNT(*) FROM screenshots
                                 WHERE is_deleted = 0
                                     AND process_name IS NOT NULL AND process_name != ''
                 GROUP BY process_name",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| format!("Failed to execute query: {}", e))?;
            for row in rows.filter_map(|r| r.ok()) {
                counts.insert(row.0, row.1);
            }

            // Slow path: collect encrypted-only rows for offline decryption
            let mut enc_stmt = conn
                .prepare(
                    "SELECT process_name_enc, content_key_encrypted FROM screenshots
                                 WHERE is_deleted = 0
                                     AND process_name IS NULL
                                     AND process_name_enc IS NOT NULL",
                )
                .map_err(|e| format!("Failed to prepare enc query: {}", e))?;
            let enc_rows: Vec<_> = enc_stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<Vec<u8>>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                    ))
                })
                .map_err(|e| format!("Failed to execute enc query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            (counts, enc_rows)
            // guard dropped — mutex released
        };
        let query_dur = fn_start.elapsed();

        // Phase 2: Decrypt old records outside mutex (only if user has authenticated)
        let session_valid = self.credential_state.is_session_valid();
        let skipped_encrypted = !session_valid && !encrypted_rows.is_empty();
        if session_valid {
            for (process_enc, key_enc) in &encrypted_rows {
                let mut row_key = key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(enc).ok());
                let process_name = match (process_enc.as_ref(), row_key.as_ref()) {
                    (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => None,
                };
                if let Some(name) = process_name {
                    if !name.trim().is_empty() {
                        *counts.entry(name).or_insert(0) += 1;
                    }
                }
                if let Some(ref mut key) = row_key {
                    Self::zeroize_bytes(key);
                }
            }
        } else if skipped_encrypted {
            tracing::info!(
                "[DIAG:DB] list_distinct_processes: skipped {} encrypted rows (session not valid, waiting for user auth)",
                encrypted_rows.len()
            );
        }

        let mut results: Vec<(String, i64)> = counts.into_iter().collect();
        results.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });

        let total_dur = fn_start.elapsed();
        if total_dur.as_millis() >= 500 || !encrypted_rows.is_empty() {
            tracing::info!(
                "[DIAG:DB] list_distinct_processes total={:?} (query={:?}, decrypt={:?}, plaintext_groups={}, encrypted_rows={})",
                total_dur,
                query_dur,
                total_dur - query_dur,
                results.len(),
                encrypted_rows.len()
            );
        }

        Ok(results)
    }

    /// Get screenshot count stats by process with percentage distribution.
    pub fn get_process_stats(&self) -> Result<Vec<ProcessStorageStat>, String> {
        let grouped = self.list_distinct_processes()?;
        let total: i64 = grouped.iter().map(|(_, count)| *count).sum();

        let stats = grouped
            .into_iter()
            .map(|(process_name, screenshot_count)| {
                let percentage = if total > 0 {
                    (screenshot_count as f64 / total as f64) * 100.0
                } else {
                    0.0
                };

                ProcessStorageStat {
                    process_name,
                    screenshot_count,
                    percentage,
                }
            })
            .collect();

        Ok(stats)
    }

    /// Get paged screenshot thumbnails for a process, annotated with month key (YYYY-MM).
    pub fn get_process_monthly_thumbnails(
        &self,
        process_name: &str,
        page: i64,
        page_size: i64,
    ) -> Result<ProcessMonthlyThumbnailPage, String> {
        let safe_page = page.max(0);
        let safe_page_size = page_size.clamp(1, 200);
        let offset = safe_page * safe_page_size;

        let guard = self.get_connection_named("get_process_monthly_thumbnails")?;
        let conn = guard.as_ref().unwrap();

        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM screenshots WHERE is_deleted = 0 AND process_name = ?1",
                [process_name],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count process screenshots: {}", e))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, image_path, created_at, strftime('%s', created_at) AS ts, strftime('%Y-%m', created_at) AS month_key
                 FROM screenshots
                 WHERE is_deleted = 0 AND process_name = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| format!("Failed to prepare monthly thumbnails query: {}", e))?;

        let items: Vec<ProcessMonthlyThumbnailItem> = stmt
            .query_map([&process_name as &dyn rusqlite::ToSql, &safe_page_size, &offset], |row| {
                let ts_raw: Option<String> = row.get(3)?;
                let timestamp = ts_raw.and_then(|v| v.parse::<i64>().ok());
                Ok(ProcessMonthlyThumbnailItem {
                    screenshot_id: row.get(0)?,
                    image_path: row.get(1)?,
                    created_at: row.get(2)?,
                    timestamp,
                    month: row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "unknown".to_string()),
                })
            })
            .map_err(|e| format!("Failed to execute monthly thumbnails query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        let next_page = if (offset + items.len() as i64) < total {
            Some(safe_page + 1)
        } else {
            None
        };

        Ok(ProcessMonthlyThumbnailPage {
            process_name: process_name.to_string(),
            page: safe_page,
            page_size: safe_page_size,
            total,
            items,
            next_page,
        })
    }
}
