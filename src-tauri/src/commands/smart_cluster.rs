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
    pub threshold: f64,
    pub dominant_color: Option<String>,
    pub examples: Vec<SmartClusterExample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateSmartClusterResponse {
    pub id: i64,
    pub enqueued: i64,
}

#[tauri::command]
pub fn smart_cluster_list(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<SmartClusterRecord>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_clusters()
}

#[tauri::command]
pub fn smart_cluster_get(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<Option<SmartClusterRecord>, String> {
    check_auth_required(&credential_state)?;
    state.get_smart_cluster(id)
}

#[tauri::command]
pub fn smart_cluster_get_examples(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<Vec<SmartClusterExample>, String> {
    check_auth_required(&credential_state)?;
    state.list_smart_cluster_examples(id)
}

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
        let id =
            state.create_smart_cluster(&anchor, req.threshold, req.dominant_color.as_deref())?;
        state.save_smart_cluster_examples(id, &req.examples)?;

        // Backfill — enqueue every non-deleted screenshot in the hot window for
        // the worker to score against this new cluster's anchor.
        let enqueued = state.enqueue_pending_from_recent(HOT_LAYER_DAYS)?;

        Ok(CreateSmartClusterResponse { id, enqueued })
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

#[tauri::command]
pub fn smart_cluster_delete(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.delete_smart_cluster(id)
}

#[tauri::command]
pub fn smart_cluster_update_anchor(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
    anchor: String,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    let anchor = normalize_anchor_text(&anchor)?;
    state.update_smart_cluster_anchor(id, &anchor)
}

#[tauri::command]
pub fn smart_cluster_update_threshold(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
    threshold: f64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;
    state.update_smart_cluster_threshold(id, threshold)
}

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

#[tauri::command]
pub fn smart_cluster_get_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
) -> Result<Option<SmartClusterSummaryRecord>, String> {
    check_auth_required(&credential_state)?;
    state.get_smart_cluster_summary(cluster_id)
}

#[tauri::command]
pub fn smart_cluster_upsert_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    summary: SmartClusterSummaryUpsert,
) -> Result<SmartClusterSummaryRecord, String> {
    check_auth_required(&credential_state)?;
    state.upsert_smart_cluster_summary(&summary)
}

#[tauri::command]
pub fn smart_cluster_delete_summary(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    cluster_id: i64,
) -> Result<bool, String> {
    check_auth_required(&credential_state)?;
    state.delete_smart_cluster_summary(cluster_id)
}

/// Re-enqueue all recent hot-layer screenshots; the worker re-evaluates
/// every (snapshot, enabled cluster) pair, which has the effect of
/// rescanning the given cluster among others. Existing assignments are
/// NOT cleared automatically — callers may invoke
/// `smart_cluster_clear_assignments` first if desired.
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
