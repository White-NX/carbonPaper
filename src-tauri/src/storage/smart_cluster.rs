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
use sha2::{Digest, Sha256};

use super::derived_index::{decode_vector, encode_vector};
use super::StorageState;

/// The identity of an anchor text, for deciding whether a stored encoding of it
/// is still the encoding of *this* text.
///
/// Hashed rather than compared directly so the check costs the same whatever
/// the anchor's length, and so the stored column cannot become a second copy of
/// the anchor that drifts from the first.
pub fn anchor_text_hash(anchor_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(anchor_text.as_bytes());
    hex::encode(hasher.finalize())
}

/// Read the cached anchor encoding from a scoring-target row, if it has one.
///
/// A row that is missing any part, or whose blob no longer decodes at the
/// recorded width, yields `None`: a cold cache costs one encode, and there is
/// nothing here worth failing a whole scoring pass over.
fn read_cached_anchor_vector(row: &Row<'_>) -> rusqlite::Result<Option<CachedAnchorVector>> {
    let Some(bytes) = row.get::<_, Option<Vec<u8>>>(8)? else {
        return Ok(None);
    };
    let Some(dimensions) = row.get::<_, Option<i64>>(9)? else {
        return Ok(None);
    };
    let Some(source_hash) = row.get::<_, Option<String>>(10)? else {
        return Ok(None);
    };
    let Some(model_id) = row.get::<_, Option<String>>(11)? else {
        return Ok(None);
    };
    let Some(model_revision) = row.get::<_, Option<String>>(12)? else {
        return Ok(None);
    };
    let Ok(dimensions) = usize::try_from(dimensions) else {
        return Ok(None);
    };
    match decode_vector(&bytes, dimensions) {
        Ok(vector) => Ok(Some(CachedAnchorVector {
            vector,
            source_hash,
            model_id,
            model_revision,
        })),
        Err(_) => Ok(None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterRecord {
    pub id: i64,
    /// What the scorer matches snapshots against. Changing it changes what the
    /// cluster collects, so it is not what a rename writes — see `display_name`.
    pub anchor_text: String,
    /// The label shown in the UI. `None` for a cluster created before the two
    /// were separated, where the anchor text doubles as the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub threshold: f64,
    pub enabled: bool,
    pub dominant_color: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Computed at query time; not stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_count: Option<i64>,
    /// How many of those arrived in the last seven days. Computed at query
    /// time; what the list view shows as a cluster's recent activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_assignment_count: Option<i64>,
    /// When the most recent snapshot was filed here, and where it came from.
    /// All three are `None` for a cluster that has never matched anything, and
    /// the two source fields are also `None` when that snapshot recorded no
    /// process or window title of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assigned_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_process_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SmartClusterSummaryRecord>,
}

/// The columns and joins every read of a cluster row shares.
///
/// Column layout, which [`read_cluster_from_row`] depends on: `0..=6` the
/// cluster itself, `7` its assignment count, `8..=20` the joined summary (read
/// by `read_summary_from_row`), `21` how many assignments are from the last
/// seven days, `22..=24` when the most recent snapshot was filed here and what
/// it was showing, and `25` the display name.
///
/// The `la` join picks that single most recent assignment through the table's
/// own primary key rather than a window function, so it stays one index lookup
/// per cluster instead of a scan over every assignment ever made.
const SMART_CLUSTER_PROJECTION: &str = "SELECT sc.id, sc.anchor_text, sc.threshold, sc.enabled, \
            sc.dominant_color, sc.created_at, sc.updated_at, \
            COALESCE(\
                (SELECT COUNT(*) FROM smart_cluster_assignments a \
                 JOIN screenshots s ON s.id = a.screenshot_id \
                 WHERE a.smart_cluster_id = sc.id AND s.is_deleted = 0), 0) AS cnt, \
            ss.smart_cluster_id, ss.title, ss.summary, ss.ocr_summary, \
            ss.key_points_json, ss.evidence_json, ss.source_snapshot_count, \
            ss.source_hash, ss.model_provider, ss.model_name, ss.prompt_version, \
            ss.created_at, ss.updated_at, \
            COALESCE(\
                (SELECT COUNT(*) FROM smart_cluster_assignments ra \
                 JOIN screenshots rs ON rs.id = ra.screenshot_id \
                 WHERE ra.smart_cluster_id = sc.id AND rs.is_deleted = 0 \
                   AND ra.assigned_at >= datetime('now', '-7 days')), 0) AS recent_cnt, \
            la.assigned_at, ls.process_name, ls.window_title, sc.display_name \
     FROM smart_clusters sc \
     LEFT JOIN smart_cluster_summaries ss ON ss.smart_cluster_id = sc.id \
     LEFT JOIN smart_cluster_assignments la \
            ON la.smart_cluster_id = sc.id \
           AND la.screenshot_id = (\
               SELECT na.screenshot_id FROM smart_cluster_assignments na \
               JOIN screenshots ns ON ns.id = na.screenshot_id \
               WHERE na.smart_cluster_id = sc.id AND ns.is_deleted = 0 \
               ORDER BY na.assigned_at DESC, na.screenshot_id DESC LIMIT 1) \
     LEFT JOIN screenshots ls ON ls.id = la.screenshot_id";

fn read_cluster_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SmartClusterRecord> {
    Ok(SmartClusterRecord {
        id: row.get(0)?,
        anchor_text: row.get(1)?,
        display_name: row.get(25)?,
        threshold: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        dominant_color: row.get(4)?,
        created_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        assignment_count: Some(row.get::<_, i64>(7)?),
        recent_assignment_count: Some(row.get::<_, i64>(21)?),
        last_assigned_at: row.get(22)?,
        last_process_name: row.get(23)?,
        last_window_title: row.get(24)?,
        summary: read_summary_from_row(row, 8)?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterExample {
    pub screenshot_id: i64,
    pub is_positive: bool,
    pub rerank_score: Option<f64>,
}

/// The scorer that produced a stored threshold.
///
/// Mirrors `rerank::ScorerIdentity` as a storage type rather than reusing it,
/// so the storage layer keeps no dependency on the inference layer. Empty
/// strings represent a row written before provenance existed; `scorer_recorded`
/// on the target below is what distinguishes that from a genuine value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartClusterScorer {
    pub model_id: String,
    pub model_revision: String,
    pub variant: String,
    pub provider: String,
}

/// A cluster's anchor as the bi-encoder left it, plus what it was made from.
///
/// The provenance is the whole point: this vector is only usable if the anchor
/// text has not changed since it was encoded and the same model would encode it
/// the same way today. Both are checked against the row rather than tracked by
/// whoever edits a cluster, so there is no invalidation call to forget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAnchorVector {
    pub vector: Vec<f32>,
    /// SHA-256 of the anchor text this was encoded from.
    pub source_hash: String,
    pub model_id: String,
    pub model_revision: String,
}

/// One enabled cluster, as the background scorer sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartClusterScoringTarget {
    pub id: i64,
    pub anchor_text: String,
    pub threshold: f64,
    pub scorer: SmartClusterScorer,
    /// False for a threshold written before step 6, which is every threshold
    /// produced by the Python DirectML reranker.
    pub scorer_recorded: bool,
    /// The scorer under which re-deriving this cluster's threshold from its
    /// saved examples was already attempted and found impossible. `None` means
    /// no attempt has been given up on; a value that equals the current scorer
    /// means the worker must not attempt it again, because the answer cannot
    /// change until the examples do.
    pub rederive_failed_scorer: Option<String>,
    /// The stored anchor encoding, when the row has one that decodes. `None`
    /// means the pass has to encode the anchor itself and write the result
    /// back; it is not an error, just a cold cache.
    pub anchor_vector: Option<CachedAnchorVector>,
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
        display_name: Option<&str>,
    ) -> Result<i64, String> {
        let guard = self.get_connection_named("create_smart_cluster")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "INSERT INTO smart_clusters (anchor_text, threshold, dominant_color, display_name, enabled) \
             VALUES (?, ?, ?, ?, 1)",
            params![anchor_text, threshold, dominant_color, display_name],
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
            .prepare(&format!(
                "{SMART_CLUSTER_PROJECTION} ORDER BY sc.updated_at DESC"
            ))
            .map_err(|e| format!("Failed to prepare list_smart_clusters: {}", e))?;
        let rows = stmt
            .query_map([], read_cluster_from_row)
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
            &format!("{SMART_CLUSTER_PROJECTION} WHERE sc.id = ?"),
            params![id],
            read_cluster_from_row,
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

    /// Rename a cluster.
    ///
    /// This writes the label only. The anchor text the scorer matches against
    /// is deliberately left alone: rewriting it here would change what the
    /// cluster collects from now on and strand the threshold, which was
    /// calibrated against the old wording on examples the user has already
    /// approved.
    pub fn update_smart_cluster_display_name(&self, id: i64, name: &str) -> Result<(), String> {
        let guard = self.get_connection_named("update_smart_cluster_display_name")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters SET display_name = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![name, id],
        )
        .map_err(|e| format!("Failed to update smart cluster name: {}", e))?;
        Ok(())
    }

    // `update_smart_cluster_threshold` — a threshold writer that left the
    // provenance columns untouched — was removed with M2.5 step 6. It is not
    // merely unused: a caller reaching for it would write a new number while
    // leaving the *previous* scorer recorded beside it, and the worker would
    // then trust that number instead of re-deriving it. Use
    // `update_smart_cluster_threshold_with_scorer` below.

    /// Write a threshold together with the scorer that produced it.
    ///
    /// The provenance is set in the same statement as the number, never
    /// separately: a threshold whose recorded scorer does not describe the run
    /// that produced it is worse than one with no provenance at all, because
    /// the first is trusted and the second is repaired.
    ///
    /// Clears any "cannot be re-derived" verdict in the same statement. A
    /// threshold arriving with its provenance is exactly the state that verdict
    /// was blocking work on, so leaving it behind would keep a repaired cluster
    /// marked as broken.
    pub fn update_smart_cluster_threshold_with_scorer(
        &self,
        id: i64,
        threshold: f64,
        scorer: &SmartClusterScorer,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("update_smart_cluster_threshold_with_scorer")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters \
             SET threshold = ?, \
                 threshold_model_id = ?, \
                 threshold_model_revision = ?, \
                 threshold_variant = ?, \
                 threshold_provider = ?, \
                 threshold_calibrated_at = CURRENT_TIMESTAMP, \
                 threshold_rederive_failed_scorer = NULL, \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?",
            params![
                threshold,
                scorer.model_id,
                scorer.model_revision,
                scorer.variant,
                scorer.provider,
                id
            ],
        )
        .map_err(|e| format!("Failed to update smart cluster threshold: {}", e))?;
        Ok(())
    }

    /// Record that `scorer` cannot re-derive a threshold for this cluster from
    /// the examples it currently has.
    ///
    /// The worker calls this once and then skips the cluster, rather than
    /// re-discovering the same answer on every pass at the cost of a 570 MB
    /// model load. `updated_at` is deliberately left alone: the cluster list is
    /// ordered by it, and a background verdict must not reshuffle the user's
    /// clusters.
    pub fn mark_smart_cluster_threshold_unverifiable(
        &self,
        id: i64,
        scorer_fingerprint: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("mark_smart_cluster_threshold_unverifiable")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters SET threshold_rederive_failed_scorer = ? WHERE id = ?",
            params![scorer_fingerprint, id],
        )
        .map_err(|e| format!("Failed to mark smart cluster threshold unverifiable: {}", e))?;
        Ok(())
    }

    /// Store the bi-encoder's encoding of this cluster's anchor.
    ///
    /// Written by the background scorer after a cold-cache encode, so the next
    /// batch does not have to bring MiniLM back into a single-model engine just
    /// to re-derive a vector for text that has not changed.
    ///
    /// `updated_at` is deliberately left alone. The cluster list the user sees
    /// is ordered by it, and a cache fill is not an edit to their cluster.
    pub fn update_smart_cluster_anchor_vector(
        &self,
        id: i64,
        vector: &[f32],
        source_hash: &str,
        model_id: &str,
        model_revision: &str,
    ) -> Result<(), String> {
        let blob = encode_vector(vector)?;
        let dimensions = i64::try_from(vector.len())
            .map_err(|_| "Anchor vector dimensions exceed SQLite range".to_string())?;
        let guard = self.get_connection_named("update_smart_cluster_anchor_vector")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.execute(
            "UPDATE smart_clusters \
             SET anchor_vector = ?, \
                 anchor_vector_dimensions = ?, \
                 anchor_vector_source_hash = ?, \
                 anchor_vector_model_id = ?, \
                 anchor_vector_model_revision = ? \
             WHERE id = ?",
            params![blob, dimensions, source_hash, model_id, model_revision, id],
        )
        .map_err(|e| format!("Failed to update smart cluster anchor vector: {}", e))?;
        Ok(())
    }

    /// Everything the background scorer needs about one enabled cluster, in one
    /// read: what to score against, what to compare to, who produced the number
    /// being compared to, and whether repairing it has already been ruled out.
    pub fn list_smart_cluster_scoring_targets(
        &self,
    ) -> Result<Vec<SmartClusterScoringTarget>, String> {
        let guard = self.get_connection_named("list_smart_cluster_scoring_targets")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, anchor_text, threshold, threshold_model_id, \
                        threshold_model_revision, threshold_variant, threshold_provider, \
                        threshold_rederive_failed_scorer, \
                        anchor_vector, anchor_vector_dimensions, anchor_vector_source_hash, \
                        anchor_vector_model_id, anchor_vector_model_revision \
                 FROM smart_clusters WHERE enabled = 1 ORDER BY id",
            )
            .map_err(|e| format!("Failed to prepare scoring target query: {}", e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SmartClusterScoringTarget {
                    id: row.get(0)?,
                    anchor_text: row.get(1)?,
                    threshold: row.get(2)?,
                    scorer: SmartClusterScorer {
                        model_id: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        model_revision: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        variant: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        provider: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    },
                    scorer_recorded: row.get::<_, Option<String>>(3)?.is_some(),
                    rederive_failed_scorer: row.get::<_, Option<String>>(7)?,
                    anchor_vector: read_cached_anchor_vector(row)?,
                })
            })
            .map_err(|e| format!("Failed to query scoring targets: {}", e))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("Failed to read scoring target row: {}", e))?);
        }
        Ok(out)
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

    /// Replace a cluster's calibration examples.
    ///
    /// Also clears any "cannot be re-derived" verdict, in the same transaction.
    /// That verdict is a statement about a specific set of examples — typically
    /// that every positive one has been deleted — so a new set has to be given
    /// its own chance rather than inheriting the old answer.
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
        tx.execute(
            "UPDATE smart_clusters SET threshold_rederive_failed_scorer = NULL WHERE id = ?",
            params![cluster_id],
        )
        .map_err(|e| format!("Failed to clear rederive verdict: {}", e))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    /// Put the table back into the shape it had before the anchor cache
    /// existed, so the next `init_tables` is a migration rather than a create.
    fn drop_anchor_cache_columns(storage: &StorageState) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        for column in [
            "anchor_vector",
            "anchor_vector_dimensions",
            "anchor_vector_source_hash",
            "anchor_vector_model_id",
            "anchor_vector_model_revision",
        ] {
            conn.execute_batch(&format!("ALTER TABLE smart_clusters DROP COLUMN {column}"))
                .expect("drop anchor cache column");
        }
    }

    fn reinitialize(storage: &StorageState) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        storage
            .init_tables(guard.as_ref().expect("database"))
            .expect("re-initialize schema");
    }

    fn insert_cluster(storage: &StorageState, anchor_text: &str) -> i64 {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        conn.execute(
            "INSERT INTO smart_clusters (anchor_text, threshold, enabled) VALUES (?, ?, 1)",
            params![anchor_text, -2.5f64],
        )
        .expect("insert cluster");
        conn.last_insert_rowid()
    }

    #[test]
    fn a_database_written_before_the_anchor_cache_gains_it_without_losing_a_cluster() {
        let (_temp, storage) = test_storage();
        drop_anchor_cache_columns(&storage);
        let id = insert_cluster(&storage, "receipts");
        reinitialize(&storage);

        let targets = storage
            .list_smart_cluster_scoring_targets()
            .expect("scoring targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, id);
        assert_eq!(targets[0].anchor_text, "receipts");
        // A cluster that predates the cache reads back as a cold cache, which
        // costs one encode and is not an error.
        assert!(targets[0].anchor_vector.is_none());
    }

    /// Renaming a cluster must not change what it collects.
    ///
    /// The anchor is what the scorer matches against and what the stored
    /// threshold was calibrated for, so a rename that rewrote it would quietly
    /// re-aim the cluster and strand the number that decides how strict it is.
    #[test]
    fn renaming_a_cluster_leaves_the_text_it_matches_against_alone() {
        let (_temp, storage) = test_storage();
        let id = storage
            .create_smart_cluster("receipts and invoices", -2.5, None, Some("Receipts"))
            .expect("create cluster");

        storage
            .update_smart_cluster_display_name(id, "Shopping")
            .expect("rename cluster");

        let record = storage
            .get_smart_cluster(id)
            .expect("read cluster")
            .expect("cluster exists");
        assert_eq!(record.display_name.as_deref(), Some("Shopping"));
        assert_eq!(record.anchor_text, "receipts and invoices");

        let targets = storage
            .list_smart_cluster_scoring_targets()
            .expect("scoring targets");
        assert_eq!(targets[0].anchor_text, "receipts and invoices");
    }

    /// A cluster created before names and anchors were separate columns still
    /// has something to show in the list: readers fall back to the anchor text,
    /// which is the string those clusters were already displaying.
    #[test]
    fn a_cluster_written_before_names_existed_reads_back_without_one() {
        let (_temp, storage) = test_storage();
        let id = insert_cluster(&storage, "receipts");

        let record = storage
            .get_smart_cluster(id)
            .expect("read cluster")
            .expect("cluster exists");
        assert!(record.display_name.is_none());
        assert_eq!(record.anchor_text, "receipts");
    }

    #[test]
    fn a_cached_anchor_round_trips_with_the_provenance_that_makes_it_usable() {
        let (_temp, storage) = test_storage();
        let id = insert_cluster(&storage, "receipts");
        let vector = vec![0.6f32, 0.8];
        storage
            .update_smart_cluster_anchor_vector(
                id,
                &vector,
                &anchor_text_hash("receipts"),
                "paraphrase-multilingual-MiniLM-L12-v2",
                "2c4055b12046f11709e9df2c122e59ffbdc2f900",
            )
            .expect("cache the anchor vector");

        let targets = storage
            .list_smart_cluster_scoring_targets()
            .expect("scoring targets");
        let cached = targets[0]
            .anchor_vector
            .as_ref()
            .expect("the vector just written");
        assert_eq!(cached.vector, vector);
        assert_eq!(cached.source_hash, anchor_text_hash("receipts"));
        assert_eq!(cached.model_id, "paraphrase-multilingual-MiniLM-L12-v2");
        assert_eq!(
            cached.model_revision,
            "2c4055b12046f11709e9df2c122e59ffbdc2f900"
        );
    }

    #[test]
    fn a_half_written_anchor_cache_reads_as_no_cache_rather_than_a_failed_pass() {
        // Nothing writes these columns apart from one statement that sets all
        // of them, so a row like this means something outside this code touched
        // the database. Refusing to read the whole cluster list over it would
        // stop scoring entirely; treating it as a cold cache costs one encode
        // and repairs itself on the write-back.
        let (_temp, storage) = test_storage();
        let id = insert_cluster(&storage, "receipts");
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().expect("database");
            conn.execute(
                "UPDATE smart_clusters SET anchor_vector = ?, anchor_vector_dimensions = ? \
                 WHERE id = ?",
                params![vec![0u8; 8], 2i64, id],
            )
            .expect("write a vector with no provenance");
        }
        let targets = storage
            .list_smart_cluster_scoring_targets()
            .expect("scoring targets");
        assert!(targets[0].anchor_vector.is_none());
    }

    #[test]
    fn a_blob_that_no_longer_matches_its_recorded_width_reads_as_no_cache() {
        let (_temp, storage) = test_storage();
        let id = insert_cluster(&storage, "receipts");
        storage
            .update_smart_cluster_anchor_vector(id, &[0.6, 0.8], "hash", "model", "revision")
            .expect("cache the anchor vector");
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().expect("database");
            conn.execute(
                "UPDATE smart_clusters SET anchor_vector_dimensions = 384 WHERE id = ?",
                params![id],
            )
            .expect("corrupt the recorded width");
        }
        let targets = storage
            .list_smart_cluster_scoring_targets()
            .expect("scoring targets");
        assert!(targets[0].anchor_vector.is_none());
    }
}
