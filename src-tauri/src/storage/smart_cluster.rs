//! Smart Cluster storage operations.
//!
//! User-defined NL-anchored clusters. The user types a natural-language
//! description (e.g. "California mountain research"), a few positive and
//! optional negative examples are collected during calibration, and a
//! per-cluster threshold is derived from the reranker scores of those
//! examples. New snapshots are evaluated in a background worker; matches
//! above the threshold are recorded in `smart_cluster_assignments`.

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::StorageState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterRecord {
    pub id: i64,
    pub anchor_text: String,
    pub threshold: f64,
    pub enabled: bool,
    pub dominant_color: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Computed at query time; not stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SmartClusterSummaryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterExample {
    pub screenshot_id: i64,
    pub is_positive: bool,
    pub rerank_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterAssignmentStub {
    pub screenshot_id: i64,
    pub rerank_score: Option<f64>,
    pub image_path: String,
    pub process_name: Option<String>,
    pub window_title: Option<String>,
    pub created_at: String,
    pub category: Option<String>,
    pub assigned_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterOcrCorpusItem {
    pub screenshot_id: i64,
    pub rerank_score: Option<f64>,
    pub process_name: Option<String>,
    pub window_title: Option<String>,
    pub created_at: String,
    pub category: Option<String>,
    pub assigned_at: String,
    pub ocr_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterSummaryRecord {
    pub smart_cluster_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_points: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_snapshot_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterSummaryUpsert {
    pub smart_cluster_id: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub ocr_summary: Option<String>,
    pub key_points: Option<Value>,
    pub evidence: Option<Value>,
    pub source_snapshot_count: Option<i64>,
    pub source_hash: Option<String>,
    pub model_provider: Option<String>,
    pub model_name: Option<String>,
    pub prompt_version: Option<String>,
}

fn parse_json_value(raw: Option<String>) -> Option<Value> {
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

fn encode_json_value(value: &Option<Value>) -> Option<String> {
    value.as_ref().map(Value::to_string)
}

fn normalize_optional_text(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn read_summary_from_row(
    row: &Row<'_>,
    start: usize,
) -> rusqlite::Result<Option<SmartClusterSummaryRecord>> {
    let smart_cluster_id: Option<i64> = row.get(start)?;
    match smart_cluster_id {
        Some(id) => Ok(Some(SmartClusterSummaryRecord {
            smart_cluster_id: id,
            title: row.get(start + 1)?,
            summary: row.get(start + 2)?,
            ocr_summary: row.get(start + 3)?,
            key_points: parse_json_value(row.get(start + 4)?),
            evidence: parse_json_value(row.get(start + 5)?),
            source_snapshot_count: row.get(start + 6)?,
            source_hash: row.get(start + 7)?,
            model_provider: row.get(start + 8)?,
            model_name: row.get(start + 9)?,
            prompt_version: row.get(start + 10)?,
            created_at: row
                .get::<_, Option<String>>(start + 11)?
                .unwrap_or_default(),
            updated_at: row
                .get::<_, Option<String>>(start + 12)?
                .unwrap_or_default(),
        })),
        None => Ok(None),
    }
}

impl StorageState {
    // ------------------------------------------------------------------
    // CRUD on smart_clusters
    // ------------------------------------------------------------------

    pub fn create_smart_cluster(
        &self,
        anchor_text: &str,
        threshold: f64,
        dominant_color: Option<&str>,
    ) -> Result<i64, String> {
        let guard = self.get_connection_named("create_smart_cluster")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "INSERT INTO smart_clusters (anchor_text, threshold, dominant_color, enabled) \
             VALUES (?, ?, ?, 1)",
            params![anchor_text, threshold, dominant_color],
        )
        .map_err(|e| format!("Failed to create smart cluster: {}", e))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_smart_clusters(&self) -> Result<Vec<SmartClusterRecord>, String> {
        let guard = self.get_connection_named("list_smart_clusters")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT sc.id, sc.anchor_text, sc.threshold, sc.enabled, sc.dominant_color, \
                        sc.created_at, sc.updated_at, \
                        COALESCE(\
                            (SELECT COUNT(*) FROM smart_cluster_assignments a \
                             JOIN screenshots s ON s.id = a.screenshot_id \
                             WHERE a.smart_cluster_id = sc.id AND s.is_deleted = 0), 0) AS cnt, \
                        ss.smart_cluster_id, ss.title, ss.summary, ss.ocr_summary, \
                        ss.key_points_json, ss.evidence_json, ss.source_snapshot_count, \
                        ss.source_hash, ss.model_provider, ss.model_name, ss.prompt_version, \
                        ss.created_at, ss.updated_at \
                 FROM smart_clusters sc \
                 LEFT JOIN smart_cluster_summaries ss ON ss.smart_cluster_id = sc.id \
                 ORDER BY sc.updated_at DESC",
            )
            .map_err(|e| format!("Failed to prepare list_smart_clusters: {}", e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SmartClusterRecord {
                    id: row.get(0)?,
                    anchor_text: row.get(1)?,
                    threshold: row.get(2)?,
                    enabled: row.get::<_, i64>(3)? != 0,
                    dominant_color: row.get(4)?,
                    created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    updated_at: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    assignment_count: Some(row.get::<_, i64>(7)?),
                    summary: read_summary_from_row(row, 8)?,
                })
            })
            .map_err(|e| format!("Failed to query smart clusters: {}", e))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("Failed to read smart cluster row: {}", e))?);
        }
        Ok(out)
    }

    pub fn get_smart_cluster(&self, id: i64) -> Result<Option<SmartClusterRecord>, String> {
        let guard = self.get_connection_named("get_smart_cluster")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        match conn.query_row(
            "SELECT sc.id, sc.anchor_text, sc.threshold, sc.enabled, sc.dominant_color, \
                    sc.created_at, sc.updated_at, \
                    COALESCE(\
                        (SELECT COUNT(*) FROM smart_cluster_assignments a \
                         JOIN screenshots s ON s.id = a.screenshot_id \
                         WHERE a.smart_cluster_id = sc.id AND s.is_deleted = 0), 0) AS cnt, \
                    ss.smart_cluster_id, ss.title, ss.summary, ss.ocr_summary, \
                    ss.key_points_json, ss.evidence_json, ss.source_snapshot_count, \
                    ss.source_hash, ss.model_provider, ss.model_name, ss.prompt_version, \
                    ss.created_at, ss.updated_at \
             FROM smart_clusters sc \
             LEFT JOIN smart_cluster_summaries ss ON ss.smart_cluster_id = sc.id \
             WHERE sc.id = ?",
            params![id],
            |row| {
                Ok(SmartClusterRecord {
                    id: row.get(0)?,
                    anchor_text: row.get(1)?,
                    threshold: row.get(2)?,
                    enabled: row.get::<_, i64>(3)? != 0,
                    dominant_color: row.get(4)?,
                    created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    updated_at: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    assignment_count: Some(row.get::<_, i64>(7)?),
                    summary: read_summary_from_row(row, 8)?,
                })
            },
        ) {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Database error in get_smart_cluster: {}", e)),
        }
    }

    pub fn delete_smart_cluster(&self, id: i64) -> Result<(), String> {
        let guard = self.get_connection_named("delete_smart_cluster")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute("DELETE FROM smart_clusters WHERE id = ?", params![id])
            .map_err(|e| format!("Failed to delete smart cluster: {}", e))?;
        Ok(())
    }

    pub fn update_smart_cluster_anchor(&self, id: i64, anchor: &str) -> Result<(), String> {
        let guard = self.get_connection_named("update_smart_cluster_anchor")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters SET anchor_text = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![anchor, id],
        )
        .map_err(|e| format!("Failed to update smart cluster anchor: {}", e))?;
        Ok(())
    }

    pub fn update_smart_cluster_threshold(&self, id: i64, threshold: f64) -> Result<(), String> {
        let guard = self.get_connection_named("update_smart_cluster_threshold")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters SET threshold = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![threshold, id],
        )
        .map_err(|e| format!("Failed to update smart cluster threshold: {}", e))?;
        Ok(())
    }

    pub fn update_smart_cluster_enabled(&self, id: i64, enabled: bool) -> Result<(), String> {
        let guard = self.get_connection_named("update_smart_cluster_enabled")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters SET enabled = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![if enabled { 1 } else { 0 }, id],
        )
        .map_err(|e| format!("Failed to update smart cluster enabled: {}", e))?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Examples (positive/negative calibration)
    // ------------------------------------------------------------------

    pub fn save_smart_cluster_examples(
        &self,
        cluster_id: i64,
        examples: &[SmartClusterExample],
    ) -> Result<(), String> {
        let mut guard = self.get_connection_named("save_smart_cluster_examples")?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin tx: {}", e))?;
        tx.execute(
            "DELETE FROM smart_cluster_examples WHERE smart_cluster_id = ?",
            params![cluster_id],
        )
        .map_err(|e| format!("Failed to clear examples: {}", e))?;
        for ex in examples {
            tx.execute(
                "INSERT OR REPLACE INTO smart_cluster_examples \
                 (smart_cluster_id, screenshot_id, is_positive, rerank_score) \
                 VALUES (?, ?, ?, ?)",
                params![
                    cluster_id,
                    ex.screenshot_id,
                    if ex.is_positive { 1 } else { 0 },
                    ex.rerank_score
                ],
            )
            .map_err(|e| format!("Failed to insert example: {}", e))?;
        }
        tx.commit()
            .map_err(|e| format!("Failed to commit examples: {}", e))?;
        Ok(())
    }

    pub fn list_smart_cluster_examples(
        &self,
        cluster_id: i64,
    ) -> Result<Vec<SmartClusterExample>, String> {
        let guard = self.get_connection_named("list_smart_cluster_examples")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT screenshot_id, is_positive, rerank_score \
                 FROM smart_cluster_examples WHERE smart_cluster_id = ?",
            )
            .map_err(|e| format!("Failed to prepare list examples: {}", e))?;
        let rows = stmt
            .query_map(params![cluster_id], |row| {
                Ok(SmartClusterExample {
                    screenshot_id: row.get(0)?,
                    is_positive: row.get::<_, i64>(1)? != 0,
                    rerank_score: row.get(2)?,
                })
            })
            .map_err(|e| format!("Failed to query examples: {}", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("Failed to read example: {}", e))?);
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // Assignments
    // ------------------------------------------------------------------

    pub fn record_smart_cluster_assignment(
        &self,
        cluster_id: i64,
        screenshot_id: i64,
        rerank_score: f64,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("record_smart_cluster_assignment")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO smart_cluster_assignments \
             (smart_cluster_id, screenshot_id, rerank_score, assigned_at) \
             VALUES (?, ?, ?, CURRENT_TIMESTAMP)",
            params![cluster_id, screenshot_id, rerank_score],
        )
        .map_err(|e| format!("Failed to record assignment: {}", e))?;
        Ok(())
    }

    pub fn list_smart_cluster_assignments(
        &self,
        cluster_id: i64,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<SmartClusterAssignmentStub>, String> {
        let guard = self.get_connection_named("list_smart_cluster_assignments")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let offset = page * page_size;
        let mut stmt = conn
            .prepare(
                "SELECT a.screenshot_id, a.rerank_score, s.image_path, s.process_name, \
                        s.window_title, s.created_at, s.category, a.assigned_at \
                 FROM smart_cluster_assignments a \
                 JOIN screenshots s ON s.id = a.screenshot_id \
                 WHERE a.smart_cluster_id = ? AND s.is_deleted = 0 \
                 ORDER BY a.rerank_score DESC \
                 LIMIT ? OFFSET ?",
            )
            .map_err(|e| format!("Failed to prepare list assignments: {}", e))?;
        let rows = stmt
            .query_map(params![cluster_id, page_size, offset], |row| {
                Ok(SmartClusterAssignmentStub {
                    screenshot_id: row.get(0)?,
                    rerank_score: row.get(1)?,
                    image_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    process_name: row.get(3)?,
                    window_title: row.get(4)?,
                    created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    category: row.get(6)?,
                    assigned_at: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                })
            })
            .map_err(|e| format!("Failed to query assignments: {}", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| format!("Failed to read assignment: {}", e))?);
        }
        Ok(out)
    }

    pub fn clear_smart_cluster_assignments(&self, cluster_id: i64) -> Result<(), String> {
        let guard = self.get_connection_named("clear_smart_cluster_assignments")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "DELETE FROM smart_cluster_assignments WHERE smart_cluster_id = ?",
            params![cluster_id],
        )
        .map_err(|e| format!("Failed to clear assignments: {}", e))?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Summaries and OCR corpus
    // ------------------------------------------------------------------

    pub fn list_smart_cluster_ocr_corpus(
        &self,
        cluster_id: i64,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<SmartClusterOcrCorpusItem>, String> {
        let assignments = self.list_smart_cluster_assignments(cluster_id, page, page_size)?;
        let screenshot_ids: Vec<i64> = assignments.iter().map(|s| s.screenshot_id).collect();
        let ocr_map = self.get_ocr_results_by_screenshot_ids(&screenshot_ids)?;
        Ok(assignments
            .into_iter()
            .map(|s| SmartClusterOcrCorpusItem {
                screenshot_id: s.screenshot_id,
                rerank_score: s.rerank_score,
                process_name: s.process_name,
                window_title: s.window_title,
                created_at: s.created_at,
                category: s.category,
                assigned_at: s.assigned_at,
                ocr_text: ocr_map.get(&s.screenshot_id).cloned().unwrap_or_default(),
            })
            .collect())
    }

    pub fn get_smart_cluster_summary(
        &self,
        cluster_id: i64,
    ) -> Result<Option<SmartClusterSummaryRecord>, String> {
        let guard = self.get_connection_named("get_smart_cluster_summary")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        match conn.query_row(
            "SELECT smart_cluster_id, title, summary, ocr_summary, key_points_json, \
                    evidence_json, source_snapshot_count, source_hash, model_provider, \
                    model_name, prompt_version, created_at, updated_at \
             FROM smart_cluster_summaries WHERE smart_cluster_id = ?",
            params![cluster_id],
            |row| read_summary_from_row(row, 0).map(|v| v.expect("summary row has id")),
        ) {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get smart cluster summary: {}", e)),
        }
    }

    pub fn upsert_smart_cluster_summary(
        &self,
        input: &SmartClusterSummaryUpsert,
    ) -> Result<SmartClusterSummaryRecord, String> {
        let title = normalize_optional_text(&input.title);
        let summary = normalize_optional_text(&input.summary);
        let ocr_summary = normalize_optional_text(&input.ocr_summary);
        if title.is_none() && summary.is_none() && ocr_summary.is_none() {
            return Err("At least one of title, summary, or ocr_summary is required".to_string());
        }

        let key_points_json = encode_json_value(&input.key_points);
        let evidence_json = encode_json_value(&input.evidence);
        let source_hash = normalize_optional_text(&input.source_hash);
        let model_provider = normalize_optional_text(&input.model_provider);
        let model_name = normalize_optional_text(&input.model_name);
        let prompt_version = normalize_optional_text(&input.prompt_version);

        {
            let guard = self.get_connection_named("upsert_smart_cluster_summary")?;
            let conn = guard
                .as_ref()
                .ok_or_else(|| "Database connection is None".to_string())?;
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM smart_clusters WHERE id = ?",
                    params![input.smart_cluster_id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to check smart cluster existence: {}", e))?;
            if exists == 0 {
                return Err(format!(
                    "Smart cluster {} not found",
                    input.smart_cluster_id
                ));
            }

            conn.execute(
                "INSERT INTO smart_cluster_summaries \
                 (smart_cluster_id, title, summary, ocr_summary, key_points_json, \
                  evidence_json, source_snapshot_count, source_hash, model_provider, \
                  model_name, prompt_version, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP) \
                 ON CONFLICT(smart_cluster_id) DO UPDATE SET \
                   title = excluded.title, \
                   summary = excluded.summary, \
                   ocr_summary = excluded.ocr_summary, \
                   key_points_json = excluded.key_points_json, \
                   evidence_json = excluded.evidence_json, \
                   source_snapshot_count = excluded.source_snapshot_count, \
                   source_hash = excluded.source_hash, \
                   model_provider = excluded.model_provider, \
                   model_name = excluded.model_name, \
                   prompt_version = excluded.prompt_version, \
                   updated_at = CURRENT_TIMESTAMP",
                params![
                    input.smart_cluster_id,
                    title,
                    summary,
                    ocr_summary,
                    key_points_json,
                    evidence_json,
                    input.source_snapshot_count,
                    source_hash,
                    model_provider,
                    model_name,
                    prompt_version,
                ],
            )
            .map_err(|e| format!("Failed to upsert smart cluster summary: {}", e))?;
        }

        self.get_smart_cluster_summary(input.smart_cluster_id)?
            .ok_or_else(|| "Failed to read saved smart cluster summary".to_string())
    }

    pub fn delete_smart_cluster_summary(&self, cluster_id: i64) -> Result<bool, String> {
        let guard = self.get_connection_named("delete_smart_cluster_summary")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let deleted = conn
            .execute(
                "DELETE FROM smart_cluster_summaries WHERE smart_cluster_id = ?",
                params![cluster_id],
            )
            .map_err(|e| format!("Failed to delete smart cluster summary: {}", e))?;
        Ok(deleted > 0)
    }

    // ------------------------------------------------------------------
    // Pending queue
    // ------------------------------------------------------------------

    /// Days a pending row is allowed to live before being treated as
    /// out-of-window and pruned. Matches the smart-cluster hot window
    /// (`HOT_LAYER_DAYS`) — anything older has already aged out of the
    /// layer the worker is supposed to operate on, so re-scoring it would
    /// just waste compute on cold data.
    pub const SMART_CLUSTER_PENDING_TTL_DAYS: i64 = 30;

    pub fn enqueue_smart_cluster_pending(&self, screenshot_id: i64) -> Result<(), String> {
        let guard = self.get_connection_named("enqueue_smart_cluster_pending")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO smart_cluster_pending (screenshot_id) VALUES (?)",
            params![screenshot_id],
        )
        .map_err(|e| format!("Failed to enqueue pending: {}", e))?;
        Ok(())
    }

    /// Enqueue every non-deleted screenshot in the given time window.
    /// Used for backfill on cluster creation and manual rescan.
    pub fn enqueue_pending_from_recent(&self, days: i64) -> Result<i64, String> {
        let mut guard = self.get_connection_named("enqueue_pending_from_recent")?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin tx: {}", e))?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO smart_cluster_pending (screenshot_id) \
                 SELECT id FROM screenshots \
                 WHERE is_deleted = 0 \
                   AND created_at >= datetime('now', '-' || ? || ' days')",
                params![days],
            )
            .map_err(|e| format!("Failed to enqueue from recent: {}", e))?;
        tx.commit()
            .map_err(|e| format!("Failed to commit enqueue: {}", e))?;
        Ok(inserted as i64)
    }

    /// Read up to `limit` pending screenshot ids WITHOUT removing them from
    /// the queue. Rows older than `SMART_CLUSTER_PENDING_TTL_DAYS` are
    /// pruned in the same transaction so the worker never sees stale ids
    /// and the queue stays bounded if the worker has been offline (e.g.
    /// reranker model missing). The caller is expected to invoke
    /// `delete_smart_cluster_pending_ids` after a successful scoring pass —
    /// on any failure the rows remain in the queue and are retried on the
    /// next idle window, with `INSERT OR REPLACE` keeping assignment
    /// writes idempotent.
    pub fn peek_smart_cluster_pending_batch(&self, limit: i64) -> Result<Vec<i64>, String> {
        let mut guard = self.get_connection_named("peek_smart_cluster_pending_batch")?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin tx: {}", e))?;
        // Prune expired rows opportunistically. Cheap thanks to the
        // queued_at index; bounded by however many expired since last peek.
        tx.execute(
            "DELETE FROM smart_cluster_pending \
             WHERE queued_at < datetime('now', '-' || ? || ' days')",
            params![Self::SMART_CLUSTER_PENDING_TTL_DAYS],
        )
        .map_err(|e| format!("Failed to prune expired pending: {}", e))?;

        let ids: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    "SELECT screenshot_id FROM smart_cluster_pending \
                     ORDER BY queued_at ASC LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare peek: {}", e))?;
            let rows = stmt
                .query_map(params![limit], |row| row.get::<_, i64>(0))
                .map_err(|e| format!("Failed to query peek: {}", e))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| format!("Failed to read peek row: {}", e))?);
            }
            out
        };
        tx.commit()
            .map_err(|e| format!("Failed to commit peek tx: {}", e))?;
        Ok(ids)
    }

    /// Remove specific pending ids — call after the batch has been
    /// scored and any matching assignments have been written.
    pub fn delete_smart_cluster_pending_ids(&self, ids: &[i64]) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        // SQLite parameter limit is conservatively 999; chunk to be safe
        // in case a future caller hands us a larger slice.
        const CHUNK: usize = 500;
        let mut guard = self.get_connection_named("delete_smart_cluster_pending_ids")?;
        let conn = guard
            .as_mut()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to begin tx: {}", e))?;
        for chunk in ids.chunks(CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "DELETE FROM smart_cluster_pending WHERE screenshot_id IN ({})",
                placeholders
            );
            let bound: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            tx.execute(&sql, bound.as_slice())
                .map_err(|e| format!("Failed to delete pending ids: {}", e))?;
        }
        tx.commit()
            .map_err(|e| format!("Failed to commit delete pending: {}", e))?;
        Ok(())
    }

    pub fn count_smart_cluster_pending(&self) -> Result<i64, String> {
        let guard = self.get_connection_named("count_smart_cluster_pending")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM smart_cluster_pending \
                 WHERE queued_at >= datetime('now', '-' || ? || ' days')",
                params![Self::SMART_CLUSTER_PENDING_TTL_DAYS],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count pending: {}", e))?;
        Ok(n)
    }
}
