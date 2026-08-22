//! Tauri commands for the Smart Cluster feature.
//!
//! CRUD over the SQLite tables defined in `storage/smart_cluster.rs`,
//! plus thin orchestration helpers (drain-now flag, status fetch).
//! The actual scoring logic lives in the Python worker; these commands
//! just write to the persistence layer and signal the worker via a
//! reverse-IPC ping or the next idle poll.

use std::sync::Arc;

use crate::credential_manager::CredentialManagerState;
use crate::storage::smart_cluster::{
    SmartClusterAssignmentStub, SmartClusterExample, SmartClusterOcrCorpusItem, SmartClusterRecord,
    SmartClusterSummaryRecord, SmartClusterSummaryUpsert,
};
use crate::storage::StorageState;
use serde::{Deserialize, Serialize};

use super::check_auth_required;

/// Days of hot-layer screenshots to consider when backfilling on cluster
/// creation. Matches `monitor/task_clustering.py::HOT_LAYER_DAYS` and the
/// pending-queue TTL in `storage::smart_cluster`.
const HOT_LAYER_DAYS: i64 = 30;

fn normalize_anchor_text(anchor: &str) -> Result<String, String> {
    let trimmed = anchor.trim();
    if trimmed.is_empty() {
        return Err("anchor_text cannot be empty".to_string());
    }
    Ok(trimmed.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSmartClusterRequest {
    pub anchor_text: String,
    /// The label to show for this cluster. Absent means the anchor text is
    /// also the name, which is how every cluster read before the two were
    /// separated.
    #[serde(default)]
    pub display_name: Option<String>,
    pub threshold: f64,
    pub dominant_color: Option<String>,
    pub examples: Vec<SmartClusterExample>,
    /// Which backend served the calibration query these scores came from, as
    /// reported in that query's `backend` field. Absent when the caller cannot
    /// say, which is recorded as "no provenance" rather than guessed at.
    #[serde(default)]
    pub scorer_backend: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateSmartClusterResponse {
    pub id: i64,
    pub enqueued: i64,
}

/// Lists every user-defined smart cluster.
///
/// Authentication: required. Returns `SmartClusterRecord[]`.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_list(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<SmartClusterRecord>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_clusters()
}

/// Returns one smart cluster by `id`, or JSON `null` when it does not exist.
///
/// Authentication: required. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_get(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<Option<SmartClusterRecord>, String> {
    check_auth_required(&credential_state)?;
    state.get_smart_cluster(id)
}

/// Returns calibration examples stored for cluster `id`.
///
/// Authentication: required. Returns `SmartClusterExample[]`.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_get_examples(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<Vec<SmartClusterExample>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_cluster_examples(id)
}

/// Creates a smart cluster and queues recent screenshots for scoring.
///
/// Authentication: required. `req` contains anchor text, threshold, optional color, and
/// examples. Returns `{ "id": number, "enqueued": number }`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn smart_cluster_create(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    req: CreateSmartClusterRequest,
) -> Result<CreateSmartClusterResponse, String> {
    check_auth_required(&credential_state)?;
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let anchor = normalize_anchor_text(&req.anchor_text)?;
        let display_name = req
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let id = state.create_smart_cluster(
            &anchor,
            req.threshold,
            req.dominant_color.as_deref(),
            display_name,
        )?;
        // Stamp the scorer that produced this threshold. The Python identity is
        // retained for requests from older clients so those thresholds are
        // re-derived rather than trusted against Rust logits.
        if let Some(scorer) = scorer_stamp_for_backend(req.scorer_backend.as_deref()) {
            state.update_smart_cluster_threshold_with_scorer(id, req.threshold, &scorer)?;
        }
        // No stamp at all when the backend is unknown. The provenance columns
        // stay NULL, which reads exactly like a threshold written before they
        // existed and routes the cluster into re-derivation on the worker's
        // first pass — the repair path, rather than a fabricated identity.
        state.save_smart_cluster_examples(id, &req.examples)?;

        // Backfill — enqueue every non-deleted screenshot in the hot window for
        // the worker to score against this new cluster's anchor.
        let enqueued = state.enqueue_pending_from_recent(HOT_LAYER_DAYS)?;

        Ok(CreateSmartClusterResponse { id, enqueued })
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

/// The scorer identity for a threshold derived from scores a given backend
/// produced. `None` when the backend is unknown or unrecognized.
fn scorer_stamp_for_backend(
    backend: Option<&str>,
) -> Option<crate::storage::smart_cluster::SmartClusterScorer> {
    match backend {
        Some("rust") => {
            let scorer = crate::rerank::ScorerIdentity::current();
            Some(crate::storage::smart_cluster::SmartClusterScorer {
                model_id: scorer.model_id,
                model_revision: scorer.model_revision,
                variant: scorer.variant,
                provider: scorer.provider,
            })
        }
        Some("python") => Some(python_scorer()),
        _ => None,
    }
}

/// Python picks its provider at load time and does not report it back, so the
/// honest record is "the Python reranker, provider unknown" — which can never
/// match the Rust identity and therefore never gets silently reused.
fn python_scorer() -> crate::storage::smart_cluster::SmartClusterScorer {
    crate::storage::smart_cluster::SmartClusterScorer {
        model_id: "bge-reranker-v2-m3".to_string(),
        model_revision: "python".to_string(),
        variant: crate::rerank::RERANK_VARIANT.to_string(),
        provider: "python".to_string(),
    }
}

/// The scorer a number read off the screen right now was produced by.
///
/// Used for a hand-adjusted threshold, where current assignments are always
/// produced by the Rust worker.
fn configured_scorer() -> crate::storage::smart_cluster::SmartClusterScorer {
    let scorer = crate::rerank::ScorerIdentity::current();
    crate::storage::smart_cluster::SmartClusterScorer {
        model_id: scorer.model_id,
        model_revision: scorer.model_revision,
        variant: scorer.variant,
        provider: scorer.provider,
    }
}

/// Deletes cluster `id` and its dependent data.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_delete(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.delete_smart_cluster(id)
}

/// Renames cluster `id`.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
///
/// The name is a label. The anchor text a cluster matches against is fixed at
/// creation, when its threshold is calibrated against examples the user picked
/// for that exact wording — so renaming does not touch it.
#[tauri::command]
pub fn smart_cluster_rename(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
    name: String,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    let name = name.trim();
    if name.is_empty() {
        return Err("name cannot be empty".to_string());
    }
    state.update_smart_cluster_display_name(id, name)
}

/// Changes the match threshold for cluster `id`.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
///
/// A hand-adjusted threshold is stamped with the current scorer for the same
/// reason a calibrated one is: the number is only meaningful next to the logits
/// it will be compared against. Unlike calibration, the logits the user is
/// reading here are the ones stored on the cluster's assignments, which were
/// produced by the Rust worker.
#[tauri::command]
pub fn smart_cluster_update_threshold(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
    threshold: f64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.update_smart_cluster_threshold_with_scorer(id, threshold, &configured_scorer())
}

/// Enables or disables scoring for cluster `id`.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_toggle_enabled(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
    enabled: bool,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.update_smart_cluster_enabled(id, enabled)
}

/// Returns a page of screenshot assignments for `cluster_id`.
///
/// Authentication: required. Pagination defaults to page 0 and size 50; returns
/// `SmartClusterAssignmentStub[]`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_assignments(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<Vec<SmartClusterAssignmentStub>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_cluster_assignments(cluster_id, page.unwrap_or(0), page_size.unwrap_or(50))
}

/// Returns a page of OCR corpus items for summarizing `cluster_id`.
///
/// Authentication: required. Pagination defaults to page 0 and size 50; returns
/// `SmartClusterOcrCorpusItem[]`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_ocr_corpus(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<Vec<SmartClusterOcrCorpusItem>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_cluster_ocr_corpus(cluster_id, page.unwrap_or(0), page_size.unwrap_or(50))
}

/// Returns the saved summary for `cluster_id`, or JSON `null`.
///
/// Authentication: required. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_get_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
) -> Result<Option<SmartClusterSummaryRecord>, String> {
    check_auth_required(&credential_state)?;
    state.get_smart_cluster_summary(cluster_id)
}

/// Creates or replaces a smart-cluster summary.
///
/// Authentication: required. Returns the persisted `SmartClusterSummaryRecord`.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_upsert_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    summary: SmartClusterSummaryUpsert,
) -> Result<SmartClusterSummaryRecord, String> {
    check_auth_required(&credential_state)?;
    state.upsert_smart_cluster_summary(&summary)
}

/// Deletes the saved summary for `cluster_id`.
///
/// Authentication: required. Returns whether a row was deleted.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_delete_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
) -> Result<bool, String> {
    check_auth_required(&credential_state)?;
    state.delete_smart_cluster_summary(cluster_id)
}

/// Re-enqueues all recent hot-layer screenshots; the worker re-evaluates
/// every (snapshot, enabled cluster) pair, which has the effect of
/// rescanning the given cluster among others. Existing assignments are
/// NOT cleared automatically — callers may invoke
/// `smart_cluster_clear_assignments` first if desired.
///
/// Authentication: required. Returns the number of queued screenshots.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn smart_cluster_rescan(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    _cluster_id: i64,
) -> Result<i64, String> {
    check_auth_required(&credential_state)?;
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.enqueue_pending_from_recent(HOT_LAYER_DAYS))
        .await
        .map_err(|e| format!("Task execution failed: {}", e))?
}

/// Re-enqueue all recent hot-layer screenshots against every enabled
/// cluster. Equivalent to `smart_cluster_rescan` but without a misleading
/// per-cluster parameter — use from "rescan all" UI affordances.
///
/// Authentication: required. Returns the number of queued screenshots.
#[tauri::command]
pub async fn smart_cluster_rescan_all(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<i64, String> {
    check_auth_required(&credential_state)?;
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.enqueue_pending_from_recent(HOT_LAYER_DAYS))
        .await
        .map_err(|e| format!("Task execution failed: {}", e))?
}

/// Clears all screenshot assignments for `cluster_id` without deleting the cluster.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_clear_assignments(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.clear_smart_cluster_assignments(cluster_id)
}

#[derive(Debug, Clone, Serialize)]
pub struct SmartClusterStatus {
    pub pending_count: i64,
    pub enabled_cluster_count: i64,
    pub total_cluster_count: i64,
}

/// Returns pending-work and enabled/total cluster counts.
///
/// Authentication: required. Returns `{ "pending_count", "enabled_cluster_count",
/// "total_cluster_count" }`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub fn smart_cluster_status(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<SmartClusterStatus, String> {
    check_auth_required(&credential_state)?;
    let pending_count = state.count_smart_cluster_pending()?;
    let clusters = state.list_smart_clusters()?;
    let enabled_cluster_count = clusters.iter().filter(|c| c.enabled).count() as i64;
    let total_cluster_count = clusters.len() as i64;
    Ok(SmartClusterStatus {
        pending_count,
        enabled_cluster_count,
        total_cluster_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rerank::ScorerIdentity;

    #[test]
    fn a_calibration_threshold_is_stamped_with_the_backend_that_served_the_query() {
        let current = ScorerIdentity::current();
        let rust = scorer_stamp_for_backend(Some("rust")).expect("rust is a known backend");
        assert!(current.matches_stored(
            Some(&rust.model_id),
            Some(&rust.model_revision),
            Some(&rust.variant),
            Some(&rust.provider),
        ));
    }

    #[test]
    fn a_python_served_calibration_never_passes_for_the_rust_scorer() {
        // The whole point of taking the backend from the response: a reranked
        // Older clients can still submit a threshold produced by Python. It
        // must never be trusted against Rust logits.
        let current = ScorerIdentity::current();
        let python = scorer_stamp_for_backend(Some("python")).expect("python is a known backend");
        assert!(!current.matches_stored(
            Some(&python.model_id),
            Some(&python.model_revision),
            Some(&python.variant),
            Some(&python.provider),
        ));
    }

    #[test]
    fn an_unknown_backend_records_no_provenance_at_all() {
        // Not an invented identity: leaving the columns NULL is indistinguishable
        // from a threshold written before provenance existed, which is exactly
        // the state the worker repairs by re-deriving.
        assert!(scorer_stamp_for_backend(None).is_none());
        assert!(scorer_stamp_for_backend(Some("")).is_none());
        assert!(scorer_stamp_for_backend(Some("directml")).is_none());
    }
}
