//! Task CRUD operations for the long-term task clustering feature.
//!
//! Tasks are created by the Python clustering pipeline and stored in the
//! `tasks` and `task_assignments` tables.  The frontend can list, rename,
//! merge, and delete tasks via the Tauri commands defined in `lib.rs`.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::StorageState;

/// A task cluster record returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: i64,
    pub label: Option<String>,
    pub auto_label: Option<String>,
    pub dominant_process: Option<String>,
    pub dominant_category: Option<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub snapshot_count: i64,
    pub layer: String,
    pub created_at: String,
    pub updated_at: String,
    /// Composite relevance score used for sorting (higher = more relevant).
    /// Computed at query time, not persisted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance_score: Option<f64>,
}

/// A lightweight screenshot stub for task-detail views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScreenshotStub {
    pub screenshot_id: i64,
    pub confidence: Option<f64>,
    pub image_path: String,
    pub process_name: Option<String>,
    pub window_title: Option<String>,
    pub created_at: String,
    pub category: Option<String>,
}

/// Result for "related screenshots" query — includes task info + screenshot stubs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedScreenshotsResult {
    pub task_id: i64,
    pub task_label: Option<String>,
    pub screenshots: Vec<TaskScreenshotStub>,
}

/// Input for saving/upserting a task from the Python pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveTaskRequest {
    pub auto_label: Option<String>,
    pub dominant_process: Option<String>,
    pub dominant_category: Option<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub snapshot_count: Option<i64>,
    pub layer: Option<String>,
    /// List of screenshot IDs belonging to this task.
    pub screenshot_ids: Option<Vec<i64>>,
    /// Confidence values corresponding to each screenshot_id (same order).
    pub confidences: Option<Vec<f64>>,
}

impl StorageState {
    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    /// Get all tasks, optionally filtered by layer and/or time range.
    /// Categories considered "entertainment" — tasks dominated by these are
    /// hidden when `hide_entertainment` is `true`.
    const ENTERTAINMENT_CATEGORIES: &'static [&'static str] = &["影音娱乐", "游戏"];

    /// Categories considered "social" — hidden separately when `hide_social` is `true`.
    const SOCIAL_CATEGORIES: &'static [&'static str] = &["社交通讯"];

    /// Inactivity threshold in seconds (30 days).
    const INACTIVE_THRESHOLD_SECS: f64 = 30.0 * 86400.0;

    pub fn get_tasks(
        &self,
        layer: Option<&str>,
        start_time: Option<f64>,
        end_time: Option<f64>,
        hide_inactive: Option<bool>,
        hide_entertainment: Option<bool>,
        hide_social: Option<bool>,
    ) -> Result<Vec<TaskRecord>, String> {
        let guard = self.get_connection_named("get_tasks")?;
        let conn = guard.as_ref().unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let mut sql = String::from(
            "SELECT id, label, auto_label, dominant_process, dominant_category, \
             start_time, end_time, snapshot_count, layer, created_at, updated_at \
             FROM tasks WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(l) = layer {
            sql.push_str(" AND layer = ?");
            param_values.push(Box::new(l.to_string()));
        }
        if let Some(st) = start_time {
            sql.push_str(" AND end_time >= ?");
            param_values.push(Box::new(st));
        }
        if let Some(et) = end_time {
            sql.push_str(" AND start_time <= ?");
            param_values.push(Box::new(et));
        }

        // Hide tasks whose end_time is older than 30 days
        if hide_inactive.unwrap_or(false) {
            let cutoff = now - Self::INACTIVE_THRESHOLD_SECS;
            sql.push_str(" AND end_time >= ?");
            param_values.push(Box::new(cutoff));
        }

        // Hide entertainment-dominated tasks
        if hide_entertainment.unwrap_or(false) {
            // Build placeholders for IN clause
            let placeholders: Vec<&str> = Self::ENTERTAINMENT_CATEGORIES
                .iter()
                .map(|_| "?")
                .collect();
            sql.push_str(&format!(
                " AND (dominant_category IS NULL OR dominant_category NOT IN ({}))",
                placeholders.join(", ")
            ));
            for cat in Self::ENTERTAINMENT_CATEGORIES {
                param_values.push(Box::new(cat.to_string()));
            }
        }

        // Hide social-dominated tasks
        if hide_social.unwrap_or(false) {
            let placeholders: Vec<&str> = Self::SOCIAL_CATEGORIES
                .iter()
                .map(|_| "?")
                .collect();
            sql.push_str(&format!(
                " AND (dominant_category IS NULL OR dominant_category NOT IN ({}))",
                placeholders.join(", ")
            ));
            for cat in Self::SOCIAL_CATEGORIES {
                param_values.push(Box::new(cat.to_string()));
            }
        }

        // No ORDER BY — we sort in Rust by composite score
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare get_tasks: {}", e))?;

        let rows = stmt
            .query_map(params_ref.as_slice(), |row| {
                Ok(TaskRecord {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    auto_label: row.get(2)?,
                    dominant_process: row.get(3)?,
                    dominant_category: row.get(4)?,
                    start_time: row.get(5)?,
                    end_time: row.get(6)?,
                    snapshot_count: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    layer: row.get::<_, Option<String>>(8)?.unwrap_or_else(|| "hot".into()),
                    created_at: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    updated_at: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    relevance_score: None, // computed below
                })
            })
            .map_err(|e| format!("Failed to query tasks: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            let mut task = row.map_err(|e| format!("Failed to read task row: {}", e))?;

            // Compute composite relevance score:
            //   score = snapshot_count * min(duration_hours, 720) / 720 * 1 / (hours_since_last_active + 1)
            let duration_hours = match (task.start_time, task.end_time) {
                (Some(s), Some(e)) => ((e - s) / 3600.0).clamp(0.0, 720.0),
                _ => 0.0,
            };
            let hours_since_active = match task.end_time {
                Some(e) => ((now - e) / 3600.0).max(0.0),
                None => 720.0, // treat missing end_time as very stale
            };
            let score = (task.snapshot_count as f64)
                * (duration_hours / 720.0)
                * (1.0 / (hours_since_active + 1.0));
            task.relevance_score = Some(score);

            results.push(task);
        }

        // Sort by relevance score descending
        results.sort_by(|a, b| {
            let sa = a.relevance_score.unwrap_or(0.0);
            let sb = b.relevance_score.unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }

    /// Get screenshots assigned to a specific task.
    pub fn get_task_screenshots(
        &self,
        task_id: i64,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<TaskScreenshotStub>, String> {
        let guard = self.get_connection_named("get_task_screenshots")?;
        let conn = guard.as_ref().unwrap();

        let offset = page * page_size;

        // Collect raw rows (including encrypted blobs) while holding the mutex
        let raw_results: Vec<_> = {
            let mut stmt = conn
                .prepare(
                    "SELECT ta.screenshot_id, ta.confidence, s.image_path, \
                     s.process_name, s.window_title, s.created_at, s.category, \
                     s.window_title_enc, s.process_name_enc, s.content_key_encrypted \
                     FROM task_assignments ta \
                     JOIN screenshots s ON s.id = ta.screenshot_id \
                     WHERE ta.task_id = ? \
                     ORDER BY s.created_at DESC \
                     LIMIT ? OFFSET ?",
                )
                .map_err(|e| format!("Failed to prepare get_task_screenshots: {}", e))?;

            let rows = stmt
                .query_map(params![task_id, page_size, offset], |row| {
                    Ok((
                        TaskScreenshotStub {
                            screenshot_id: row.get(0)?,
                            confidence: row.get(1)?,
                            image_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                            process_name: row.get(3)?,
                            window_title: row.get(4)?,
                            created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                            category: row.get(6)?,
                        },
                        row.get::<_, Option<Vec<u8>>>(7)?,  // window_title_enc
                        row.get::<_, Option<Vec<u8>>>(8)?,  // process_name_enc
                        row.get::<_, Option<Vec<u8>>>(9)?,  // content_key_enc
                    ))
                })
                .map_err(|e| format!("Failed to query task screenshots: {}", e))?;

            let mut collected = Vec::new();
            for row in rows {
                collected.push(row.map_err(|e| format!("Failed to read task screenshot row: {}", e))?);
            }
            collected
        };
        drop(guard);

        // Decrypt encrypted fields outside the DB mutex
        let results = raw_results
            .into_iter()
            .map(|(mut stub, wt_enc, pn_enc, key_enc)| {
                let row_key = key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(enc).ok());

                if let (Some(data), Some(key)) = (wt_enc.as_ref(), row_key.as_ref()) {
                    if let Ok(decrypted) = decrypt_with_master_key(key, data) {
                        if let Ok(title) = String::from_utf8(decrypted) {
                            stub.window_title = Some(title);
                        }
                    }
                }

                if let (Some(data), Some(key)) = (pn_enc.as_ref(), row_key.as_ref()) {
                    if let Ok(decrypted) = decrypt_with_master_key(key, data) {
                        if let Ok(name) = String::from_utf8(decrypted) {
                            stub.process_name = Some(name);
                        }
                    }
                }

                stub
            })
            .collect();

        Ok(results)
    }

    /// Find screenshots in the same task cluster as the given screenshot.
    /// Returns empty result if the screenshot is not assigned to any task.
    pub fn get_related_screenshots(
        &self,
        screenshot_id: i64,
        limit: i64,
    ) -> Result<RelatedScreenshotsResult, String> {
        let guard = self.get_connection_named("get_related_screenshots")?;
        let conn = guard.as_ref().unwrap();

        // First, find the task this screenshot belongs to
        let task_row: Option<(i64, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT t.id, t.label, t.auto_label \
                 FROM task_assignments ta \
                 JOIN tasks t ON t.id = ta.task_id \
                 WHERE ta.screenshot_id = ? \
                 LIMIT 1",
                params![screenshot_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        let (task_id, label, auto_label) = match task_row {
            Some(r) => r,
            None => {
                return Ok(RelatedScreenshotsResult {
                    task_id: -1,
                    task_label: None,
                    screenshots: vec![],
                });
            }
        };

        let display_label = label.or(auto_label);

        // Then, fetch other screenshots from the same task
        let raw_results: Vec<_> = {
            let mut stmt = conn
                .prepare(
                    "SELECT ta.screenshot_id, ta.confidence, s.image_path, \
                     s.process_name, s.window_title, s.created_at, s.category, \
                     s.window_title_enc, s.process_name_enc, s.content_key_encrypted \
                     FROM task_assignments ta \
                     JOIN screenshots s ON s.id = ta.screenshot_id \
                     WHERE ta.task_id = ? AND ta.screenshot_id != ? \
                     ORDER BY s.created_at DESC \
                     LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare get_related_screenshots: {}", e))?;

            let rows = stmt
                .query_map(params![task_id, screenshot_id, limit], |row| {
                    Ok((
                        TaskScreenshotStub {
                            screenshot_id: row.get(0)?,
                            confidence: row.get(1)?,
                            image_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                            process_name: row.get(3)?,
                            window_title: row.get(4)?,
                            created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                            category: row.get(6)?,
                        },
                        row.get::<_, Option<Vec<u8>>>(7)?,  // window_title_enc
                        row.get::<_, Option<Vec<u8>>>(8)?,  // process_name_enc
                        row.get::<_, Option<Vec<u8>>>(9)?,  // content_key_enc
                    ))
                })
                .map_err(|e| format!("Failed to query related screenshots: {}", e))?;

            let mut collected = Vec::new();
            for row in rows {
                collected.push(
                    row.map_err(|e| format!("Failed to read related screenshot row: {}", e))?,
                );
            }
            collected
        };
        drop(guard);

        // Decrypt encrypted fields outside the DB mutex
        let screenshots = raw_results
            .into_iter()
            .map(|(mut stub, wt_enc, pn_enc, key_enc)| {
                let row_key = key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(enc).ok());

                if let (Some(data), Some(key)) = (wt_enc.as_ref(), row_key.as_ref()) {
                    if let Ok(decrypted) = decrypt_with_master_key(key, data) {
                        if let Ok(title) = String::from_utf8(decrypted) {
                            stub.window_title = Some(title);
                        }
                    }
                }

                if let (Some(data), Some(key)) = (pn_enc.as_ref(), row_key.as_ref()) {
                    if let Ok(decrypted) = decrypt_with_master_key(key, data) {
                        if let Ok(name) = String::from_utf8(decrypted) {
                            stub.process_name = Some(name);
                        }
                    }
                }

                stub
            })
            .collect();

        Ok(RelatedScreenshotsResult {
            task_id,
            task_label: display_label,
            screenshots,
        })
    }

    // ------------------------------------------------------------------
    // Mutations
    // ------------------------------------------------------------------

    /// Save a task and its screenshot assignments.
    pub fn save_task(&self, request: &SaveTaskRequest) -> Result<i64, String> {
        let mut guard = self.get_connection_named("save_task")?;
        let conn = guard.as_mut().unwrap();

        conn.execute(
            "INSERT INTO tasks (auto_label, dominant_process, dominant_category, \
             start_time, end_time, snapshot_count, layer) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                request.auto_label,
                request.dominant_process,
                request.dominant_category,
                request.start_time,
                request.end_time,
                request.snapshot_count.unwrap_or(0),
                request.layer.as_deref().unwrap_or("hot"),
            ],
        )
        .map_err(|e| format!("Failed to insert task: {}", e))?;

        let task_id = conn.last_insert_rowid();

        // Insert screenshot assignments
        if let (Some(ids), Some(confs)) = (&request.screenshot_ids, &request.confidences) {
            let mut stmt = conn
                .prepare(
                    "INSERT OR IGNORE INTO task_assignments (screenshot_id, task_id, confidence) \
                     VALUES (?, ?, ?)",
                )
                .map_err(|e| format!("Failed to prepare task_assignment insert: {}", e))?;

            for (sid, conf) in ids.iter().zip(confs.iter()) {
                let _ = stmt.execute(params![sid, task_id, conf]);
            }
        } else if let Some(ids) = &request.screenshot_ids {
            let mut stmt = conn
                .prepare(
                    "INSERT OR IGNORE INTO task_assignments (screenshot_id, task_id, confidence) \
                     VALUES (?, ?, NULL)",
                )
                .map_err(|e| format!("Failed to prepare task_assignment insert: {}", e))?;

            for sid in ids {
                let _ = stmt.execute(params![sid, task_id]);
            }
        }

        Ok(task_id)
    }

    /// Update the user-facing label of a task.
    pub fn update_task_label(&self, task_id: i64, label: &str) -> Result<(), String> {
        let mut guard = self.get_connection_named("update_task_label")?;
        let conn = guard.as_mut().unwrap();

        conn.execute(
            "UPDATE tasks SET label = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![label, task_id],
        )
        .map_err(|e| format!("Failed to update task label: {}", e))?;

        Ok(())
    }

    /// Delete a task and its assignments (screenshots are preserved).
    pub fn delete_task(&self, task_id: i64) -> Result<(), String> {
        let mut guard = self.get_connection_named("delete_task")?;
        let conn = guard.as_mut().unwrap();

        conn.execute(
            "DELETE FROM task_assignments WHERE task_id = ?",
            params![task_id],
        )
        .map_err(|e| format!("Failed to delete task assignments: {}", e))?;

        conn.execute("DELETE FROM tasks WHERE id = ?", params![task_id])
            .map_err(|e| format!("Failed to delete task: {}", e))?;

        Ok(())
    }

    /// Merge multiple tasks into the first task in the list.
    pub fn merge_tasks(&self, task_ids: &[i64]) -> Result<i64, String> {
        if task_ids.len() < 2 {
            return Err("At least 2 task IDs are required for merge".to_string());
        }

        let mut guard = self.get_connection_named("merge_tasks")?;
        let conn = guard.as_mut().unwrap();

        let target_id = task_ids[0];
        let source_ids = &task_ids[1..];

        for sid in source_ids {
            // Re-assign screenshots to target task
            conn.execute(
                "UPDATE OR IGNORE task_assignments SET task_id = ? WHERE task_id = ?",
                params![target_id, sid],
            )
            .map_err(|e| format!("Failed to reassign task assignments: {}", e))?;

            // Delete orphaned assignments (duplicates that couldn't be moved)
            conn.execute(
                "DELETE FROM task_assignments WHERE task_id = ?",
                params![sid],
            )
            .map_err(|e| format!("Failed to clean up assignments: {}", e))?;

            // Delete source task
            conn.execute("DELETE FROM tasks WHERE id = ?", params![sid])
                .map_err(|e| format!("Failed to delete merged task: {}", e))?;
        }

        // Update target task metadata
        conn.execute(
            "UPDATE tasks SET \
             snapshot_count = (SELECT COUNT(*) FROM task_assignments WHERE task_id = ?), \
             updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?",
            params![target_id, target_id],
        )
        .map_err(|e| format!("Failed to update merged task: {}", e))?;

        Ok(target_id)
    }

    /// Bulk-save tasks from a clustering run (replaces all existing hot-layer tasks).
    pub fn save_clustering_results(
        &self,
        tasks: &[SaveTaskRequest],
    ) -> Result<Vec<i64>, String> {
        let mut guard = self.get_connection_named("save_clustering_results")?;
        let conn = guard.as_mut().unwrap();

        // Delete existing hot-layer tasks and their assignments
        conn.execute_batch(
            "DELETE FROM task_assignments WHERE task_id IN (SELECT id FROM tasks WHERE layer = 'hot'); \
             DELETE FROM tasks WHERE layer = 'hot';",
        )
        .map_err(|e| format!("Failed to clear old hot tasks: {}", e))?;

        let mut new_ids = Vec::new();

        for req in tasks {
            conn.execute(
                "INSERT INTO tasks (auto_label, dominant_process, dominant_category, \
                 start_time, end_time, snapshot_count, layer) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    req.auto_label,
                    req.dominant_process,
                    req.dominant_category,
                    req.start_time,
                    req.end_time,
                    req.snapshot_count.unwrap_or(0),
                    req.layer.as_deref().unwrap_or("hot"),
                ],
            )
            .map_err(|e| format!("Failed to insert task: {}", e))?;

            let task_id = conn.last_insert_rowid();
            new_ids.push(task_id);

            if let Some(ids) = &req.screenshot_ids {
                let mut stmt = conn
                    .prepare(
                        "INSERT OR IGNORE INTO task_assignments (screenshot_id, task_id, confidence) \
                         VALUES (?, ?, NULL)",
                    )
                    .map_err(|e| format!("Failed to prepare task_assignment insert: {}", e))?;

                for sid in ids {
                    let _ = stmt.execute(params![sid, task_id]);
                }
            }
        }

        Ok(new_ids)
    }
}
