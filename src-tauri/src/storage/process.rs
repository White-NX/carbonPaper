//! List distinct processes (two-phase: SQL aggregation + offline decryption of old records).

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};

use super::{
    wire_time, ProcessMonthlyThumbnailItem, ProcessMonthlyThumbnailPage, ProcessStorageStat,
    StorageState,
};

impl StorageState {
    /// List distinct process names with counts (two-phase: SQL aggregation + offline decryption).
    pub fn list_distinct_processes(&self) -> Result<Vec<(String, i64)>, String> {
        let fn_start = std::time::Instant::now();

        // Phase 1: aggregate and extract encrypted-only rows in one read
        // snapshot, so a concurrent plaintext backfill cannot omit/double-count a row.
        let (mut counts, encrypted_rows): (
            std::collections::HashMap<String, i64>,
            Vec<(Option<Vec<u8>>, Option<Vec<u8>>)>,
        ) = {
            let reader = self.open_read_connection_named("list_distinct_processes")?;
            let conn = reader
                .unchecked_transaction()
                .map_err(|e| format!("Failed to start process statistics read: {}", e))?;

            // Fast path: aggregate plaintext process_name via SQL.
            //
            // `INDEXED BY` is not a micro-optimization here. Left to itself the
            // planner drives this off `idx_screenshots_deleted_created_at`,
            // which carries neither `process_name` nor a usable grouping order,
            // so it fetches every matching row out of a multi-gigabyte table
            // and then sorts through a temp B-tree: 0.771 s against a
            // 54k-screenshot corpus, all of it under the DB mutex. Naming the
            // covering index turns the whole statement into one index scan and
            // measured 0.036 s on the same corpus. The planner has no
            // statistics to find this on its own — `ANALYZE` has never run on
            // this database, so `sqlite_stat1` does not exist.
            let mut counts = std::collections::HashMap::new();
            let mut stmt = conn
                .prepare(
                    "SELECT process_name, COUNT(*) FROM screenshots
                        INDEXED BY idx_screenshots_process_deleted_created_at
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

            drop(enc_stmt);
            drop(stmt);
            conn.commit()
                .map_err(|e| format!("Failed to finish process statistics read: {}", e))?;
            (counts, enc_rows)
            // The read connection and its maintenance permit are released here.
        };
        let query_dur = fn_start.elapsed();

        // Phase 2: decrypt old records after closing the read connection
        // (only if the user has authenticated).
        let session_valid = self.credential_state.is_session_valid();
        let skipped_encrypted = !session_valid && !encrypted_rows.is_empty();
        if session_valid {
            for (process_enc, key_enc) in &encrypted_rows {
                let mut row_key = key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(&self.credential_state, enc).ok());
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
        time_zone: Option<&str>,
    ) -> Result<ProcessMonthlyThumbnailPage, String> {
        let zone = crate::calendar::time_zone(time_zone)?;
        let safe_page = page.max(0);
        let safe_page_size = page_size.clamp(1, 200);
        let offset = safe_page
            .checked_mul(safe_page_size)
            .ok_or("Invalid page")?;

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
                "SELECT id, image_path, strftime('%s', created_at) AS ts
                 FROM screenshots
                 WHERE is_deleted = 0 AND process_name = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| format!("Failed to prepare monthly thumbnails query: {}", e))?;

        let items: Vec<ProcessMonthlyThumbnailItem> = stmt
            .query_map(
                [
                    &process_name as &dyn rusqlite::ToSql,
                    &safe_page_size,
                    &offset,
                ],
                |row| {
                    let ts_raw: Option<String> = row.get(2)?;
                    let timestamp = ts_raw.and_then(|v| v.parse::<i64>().ok());
                    Ok(ProcessMonthlyThumbnailItem {
                        screenshot_id: row.get(0)?,
                        image_path: row.get(1)?,
                        created_at: wire_time::from_optional_seconds(timestamp),
                        timestamp,
                        month: timestamp
                            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                            .map(|time| time.with_timezone(&zone).format("%Y-%m").to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    })
                },
            )
            .map_err(|e| format!("Failed to execute monthly thumbnails query: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to read monthly thumbnail: {e}"))?;

        let next_page = if (offset + items.len() as i64) < total {
            Some(safe_page + 1)
        } else {
            None
        };

        Ok(ProcessMonthlyThumbnailPage {
            process_name: process_name.to_string(),
            time_zone: zone.name().to_string(),
            page: safe_page,
            page_size: safe_page_size,
            total,
            items,
            next_page,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use rusqlite::{params, Connection};
    use std::sync::Arc;

    fn storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        (temp, storage)
    }

    #[test]
    fn local_month_grouping_and_deletion_share_exact_half_open_bounds() {
        for (zone, month, start, end) in [
            (
                "Asia/Shanghai",
                "2026-10",
                "2026-09-30T16:00:00Z",
                "2026-10-31T16:00:00Z",
            ),
            (
                "America/New_York",
                "2026-11",
                "2026-11-01T04:00:00Z",
                "2026-12-01T05:00:00Z",
            ),
        ] {
            let (_temp, storage) = storage();
            let start = chrono::DateTime::parse_from_rfc3339(start)
                .unwrap()
                .timestamp_millis();
            let end = chrono::DateTime::parse_from_rfc3339(end)
                .unwrap()
                .timestamp_millis();
            {
                let guard = storage.db.lock().unwrap();
                let conn = guard.as_ref().unwrap();
                for (id, time) in [
                    (1, start - 1000),
                    (2, start),
                    (3, end - 1000),
                    (4, end),
                    (5, start),
                ] {
                    conn.execute("INSERT INTO screenshots(id,image_path,image_hash,process_name,created_at,status) VALUES(?1,'fixture',?1,?2,?3,'committed')",
                        params![id, if id == 5 { "other.exe" } else { "editor.exe" }, crate::calendar::sqlite_utc(time).unwrap()]).unwrap();
                    conn.execute(
                        "INSERT INTO ocr_results(id,screenshot_id,text_hash) VALUES(?1,?1,?1)",
                        [id],
                    )
                    .unwrap();
                }
            }
            let page = storage
                .get_process_monthly_thumbnails("editor.exe", 0, 60, Some(zone))
                .unwrap();
            assert_eq!(page.time_zone, zone);
            let mut displayed_ids = page
                .items
                .iter()
                .filter(|item| item.month == month)
                .map(|item| item.screenshot_id)
                .collect::<Vec<_>>();
            displayed_ids.sort_unstable();
            assert_eq!(displayed_ids, vec![2, 3], "{zone}");
            assert!(page.items.iter().all(|item| item.created_at.ends_with('Z')));

            let range = crate::mcp_contract::TimeRange::from_args(
                &serde_json::json!({"start_ms":start,"end_ms":end}),
                true,
            )
            .unwrap();
            let (a, b) = range.storage_seconds();
            let snapshots = storage
                .screenshots()
                .in_time_range(a.unwrap(), b.unwrap(), Some(500))
                .unwrap();
            let mut snapshot_ids = snapshots.iter().map(|record| record.id).collect::<Vec<_>>();
            snapshot_ids.sort_unstable();
            assert_eq!(snapshot_ids, vec![2, 3, 5], "MCP time range: {zone}");

            let deleted = storage
                .soft_delete_process_month("editor.exe", Some(month), Some(&page.time_zone))
                .unwrap();
            assert_eq!(deleted.screenshots_marked, 2);
            assert_eq!(deleted.ocr_marked, 2);
            let guard = storage.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            for sql in [
                "SELECT id FROM screenshots WHERE is_deleted=1 ORDER BY id",
                "SELECT id FROM ocr_results WHERE is_deleted=1 ORDER BY id",
                "SELECT id FROM delete_queue_screenshots ORDER BY id",
                "SELECT id FROM delete_queue_ocr ORDER BY id",
            ] {
                let ids = conn
                    .prepare(sql)
                    .unwrap()
                    .query_map([], |row| row.get::<_, i64>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(ids, displayed_ids, "{zone}: {sql}");
            }
        }
    }

    #[test]
    fn an_invalid_month_or_missing_zone_cannot_turn_into_process_wide_deletion() {
        let (_temp, storage) = storage();
        for month in ["", "2026-00", "2026-13", "2026-1", "2026-10-01"] {
            assert!(storage
                .soft_delete_process_month("editor.exe", Some(month), Some("Asia/Shanghai"))
                .is_err());
        }
        assert_eq!(
            storage
                .soft_delete_process_month("editor.exe", Some("2026-10"), None)
                .unwrap_err(),
            "CALENDAR_TIME_ZONE_REQUIRED"
        );
        assert!(storage
            .soft_delete_process_month("editor.exe", Some("2026-10"), Some("invalid"))
            .is_err());
        assert!(storage
            .get_process_monthly_thumbnails("editor.exe", 0, 60, Some("invalid"))
            .is_err());
        assert!(storage
            .soft_delete_process_month("editor.exe", None, None)
            .is_ok());
    }
}
