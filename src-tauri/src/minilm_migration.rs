//! Resumable, foreground MiniLM migration orchestration.
//!
//! M2.4a (mandatory): copy the current Chroma `task_vectors` hot-layer
//! collection into the Rust-derived cache from a stable, persisted ID
//! snapshot, then remove Rust rows that are outside the snapshot scope so the
//! two stores stay behaviorally equivalent. No inference runs in this phase.
//! The copy is triggered exclusively by the `app_metadata` sentinel at
//! startup (`spawn_minilm_auto_migration`); there is no manual start, no
//! user cancellation, and no gap-repair inference anymore. An interrupted
//! run simply resumes on the next launch/unlock from the persisted cursor.
//!
//! The whole run executes under global maintenance mode with durable
//! `minilm_migration_runs` state, so a crash resumes from the persisted
//! cursor instead of starting over. Chroma/Python remains the authoritative
//! query backend throughout M2.4.

use crate::credential_manager::CredentialManagerState;
use crate::monitor::{authenticated_monitor_command, MonitorState};
use crate::storage::{
    BackgroundReadError, BackgroundScreenshotSummary, DerivedEmbeddingWrite, DerivedIndexJobSpec,
    DerivedIndexJobStatus, DerivedIndexKind, EnsureDerivedIndexJobResult, MinilmMigrationPageRow,
    MinilmMigrationRunRecord, StorageState,
};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

pub const MINILM_MODEL_ID: &str = "paraphrase-multilingual-MiniLM-L12-v2";
/// Compatibility contract for the shared MiniLM vector space. Legacy Chroma
/// rows do not retain per-row runtime provenance, so the derived index records
/// the reviewed vector-space contract instead of pretending every copied row
/// came from one concrete ONNX artifact revision.
pub const MINILM_VECTOR_SPACE_REVISION: &str = "minilm-l12-vector-space-v1";
const MINILM_MIGRATION_MODE: &str = "sentinel_copy_chroma_hot_layer_v1";
pub const MINILM_EMBEDDING_VERSION: u32 = 1;
pub const MINILM_DIMENSIONS: usize = 384;
/// Zero (or numerically negligible) vectors would poison cosine/ANN queries;
/// they are quarantined as diagnostics instead of imported.
pub const MINILM_MIN_L2_NORM: f32 = 1e-6;
/// The MiniLM source contract consumes at most this many OCR characters, so
/// batch reads only need to decrypt boxes until the prefix is covered.
pub const MINILM_OCR_SNIPPET_CHARS: usize = 200;
const CHROMA_PAGE_SIZE: u64 = 128;
const MAX_DIAGNOSTICS: usize = 500;
const AUTH_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Python enforces a 10 minute logical snapshot deadline; allow a little slack
/// before the Rust side also gives up.
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(11 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct MinilmMigrationDiagnostic {
    pub subject_key: Option<String>,
    pub phase: String,
    pub code: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MinilmRebuildStatus {
    pub running: bool,
    pub phase: String,
    pub run_id: Option<String>,
    pub mode: String,
    pub chroma_total: u64,
    pub chroma_processed: u64,
    pub migrated: u64,
    pub legacy_unverified: u64,
    pub already_current: u64,
    pub failed: u64,
    pub discarded: u64,
    pub unmappable: u64,
    pub removed_extra: u64,
    pub publish_current: u64,
    pub publish_total: u64,
    pub required_free_bytes: u64,
    pub available_free_bytes: u64,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub finished_at: Option<String>,
}

impl Default for MinilmRebuildStatus {
    fn default() -> Self {
        Self {
            running: false,
            phase: "idle".to_string(),
            run_id: None,
            mode: String::new(),
            chroma_total: 0,
            chroma_processed: 0,
            migrated: 0,
            legacy_unverified: 0,
            already_current: 0,
            failed: 0,
            discarded: 0,
            unmappable: 0,
            removed_extra: 0,
            publish_current: 0,
            publish_total: 0,
            required_free_bytes: 0,
            available_free_bytes: 0,
            last_error: None,
            started_at: None,
            heartbeat_at: None,
            finished_at: None,
        }
    }
}

fn status_from_run(run: &MinilmMigrationRunRecord, running: bool) -> MinilmRebuildStatus {
    MinilmRebuildStatus {
        running,
        phase: run.phase.clone(),
        run_id: Some(run.run_id.clone()),
        mode: run.mode.clone(),
        chroma_total: run.chroma_total,
        chroma_processed: run.chroma_processed,
        migrated: run.migrated,
        legacy_unverified: run.legacy_unverified,
        already_current: run.already_current,
        failed: run.failed,
        discarded: run.discarded,
        unmappable: run.unmappable,
        removed_extra: run.removed_extra,
        publish_current: run.publish_current,
        publish_total: run.publish_total,
        required_free_bytes: run.required_free_bytes,
        available_free_bytes: run.available_free_bytes,
        last_error: run.last_error.clone(),
        started_at: Some(run.started_at.clone()),
        heartbeat_at: Some(run.heartbeat_at.clone()),
        finished_at: run.finished_at.clone(),
    }
}

pub struct MinilmMigrationState {
    running: AtomicBool,
    status: Mutex<MinilmRebuildStatus>,
    diagnostics: Mutex<Vec<MinilmMigrationDiagnostic>>,
    run_id: Mutex<Option<String>>,
}

impl Default for MinilmMigrationState {
    fn default() -> Self {
        Self::new()
    }
}

impl MinilmMigrationState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            status: Mutex::new(MinilmRebuildStatus::default()),
            diagnostics: Mutex::new(Vec::new()),
            run_id: Mutex::new(None),
        }
    }

    fn update(&self, update: impl FnOnce(&mut MinilmRebuildStatus)) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        update(&mut status);
    }

    fn diagnostic(
        &self,
        subject_key: Option<String>,
        phase: &str,
        code: &str,
        error: impl Into<String>,
    ) {
        let error = error.into();
        let mut diagnostics = self.diagnostics.lock().unwrap_or_else(|e| e.into_inner());
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            diagnostics.remove(0);
        }
        diagnostics.push(MinilmMigrationDiagnostic {
            subject_key,
            phase: phase.to_string(),
            code: code.to_string(),
            error,
        });
    }
}

struct SourceRow {
    text: String,
    spec: DerivedIndexJobSpec,
}

#[derive(Deserialize)]
struct TaskVectorExportStart {
    export_id: String,
}

#[derive(Deserialize)]
struct TaskVectorExportStatus {
    state: String,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct TaskVectorExportError {
    id: Option<String>,
    error: String,
}

#[derive(Deserialize)]
struct TaskVectorExportPage {
    ids: Vec<String>,
    dimensions: usize,
    #[serde(default)]
    embeddings_f32_le_b64: String,
    #[serde(default)]
    missing_ids: Vec<String>,
    #[serde(default)]
    errors: Vec<TaskVectorExportError>,
    next_cursor: u64,
    done: bool,
    total: u64,
}

pub fn build_minilm_task_text(process_name: &str, window_title: &str, ocr_text: &str) -> String {
    let mut parts = Vec::with_capacity(3);
    if !process_name.is_empty() {
        parts.push(process_name.to_string());
    }
    if !window_title.is_empty() {
        parts.push(window_title.to_string());
    }
    if !ocr_text.is_empty() {
        let snippet: String = ocr_text.chars().take(MINILM_OCR_SNIPPET_CHARS).collect();
        let snippet = snippet.trim();
        if !snippet.is_empty() {
            parts.push(snippet.to_string());
        }
    }
    parts.join(" | ")
}

pub fn minilm_source_fingerprint(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"minilm-task-text-v1\0");
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

fn minilm_spec(screenshot_id: i64, text: &str) -> DerivedIndexJobSpec {
    DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::SemanticText,
        subject_key: screenshot_id.to_string(),
        model_id: MINILM_MODEL_ID.to_string(),
        model_revision: MINILM_VECTOR_SPACE_REVISION.to_string(),
        embedding_version: MINILM_EMBEDDING_VERSION,
        source_fingerprint: minilm_source_fingerprint(text),
    }
}

fn source_rows(
    summaries: Vec<BackgroundScreenshotSummary>,
    ocr: &HashMap<i64, String>,
) -> Vec<SourceRow> {
    summaries
        .into_iter()
        .map(|summary| {
            let text = build_minilm_task_text(
                summary.process_name.as_deref().unwrap_or(""),
                summary.window_title.as_deref().unwrap_or(""),
                ocr.get(&summary.id).map(String::as_str).unwrap_or(""),
            );
            let spec = minilm_spec(summary.id, &text);
            SourceRow { text, spec }
        })
        .collect()
}

fn validate_vector(vector: &[f32]) -> Result<(), String> {
    if vector.len() != MINILM_DIMENSIONS {
        return Err(format!(
            "Expected {MINILM_DIMENSIONS} dimensions, got {}",
            vector.len()
        ));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err("Embedding contains a non-finite value".to_string());
    }
    let norm_squared: f32 = vector.iter().map(|value| value * value).sum();
    if norm_squared.sqrt() <= MINILM_MIN_L2_NORM {
        return Err("Embedding is a zero vector".to_string());
    }
    Ok(())
}

/// Decode the little-endian float32 page payload into per-id vectors.
fn decode_export_page_vectors(page: &TaskVectorExportPage) -> Result<Vec<Vec<f32>>, String> {
    if page.dimensions != MINILM_DIMENSIONS {
        return Err(format!(
            "Chroma export page has {} dimensions, expected {MINILM_DIMENSIONS}",
            page.dimensions
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(page.embeddings_f32_le_b64.as_bytes())
        .map_err(|error| format!("Invalid Base64 embedding payload: {error}"))?;
    let expected = page
        .ids
        .len()
        .checked_mul(page.dimensions)
        .and_then(|floats| floats.checked_mul(4))
        .ok_or_else(|| "Chroma export page size overflow".to_string())?;
    if bytes.len() != expected {
        return Err(format!(
            "Chroma export payload is {} bytes, expected {expected} for {} ids",
            bytes.len(),
            page.ids.len()
        ));
    }
    let mut vectors = Vec::with_capacity(page.ids.len());
    for row in bytes.chunks_exact(page.dimensions * 4) {
        vectors.push(
            row.chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect(),
        );
    }
    Ok(vectors)
}

fn parse_monitor_success(response: serde_json::Value) -> Result<serde_json::Value, String> {
    if let Some(error) = response.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_string());
    }
    if response.get("status").and_then(|value| value.as_str()) == Some("error") {
        return Err(response
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("Monitor command failed")
            .to_string());
    }
    Ok(response)
}

fn is_auth_required_error(error: &str) -> bool {
    error.contains("AUTH_REQUIRED")
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn new_run_id() -> String {
    let mut digest = Sha256::new();
    digest.update(
        Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_le_bytes(),
    );
    digest.update(std::process::id().to_le_bytes());
    let hex = format!("{:x}", digest.finalize());
    format!("minilm-{}", &hex[..32])
}

/// Cancellation no longer exists: the mandatory copy blocks until the user
/// unlocks. Closing the app is the only interruption, and the run resumes
/// from its persisted cursor on the next launch/unlock.
async fn wait_for_auth(app: &AppHandle, state: &MinilmMigrationState) {
    loop {
        let authenticated = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|value| value.is_session_valid())
            .unwrap_or(false);
        if authenticated {
            return;
        }
        state.update(|status| status.phase = "waiting_for_auth".to_string());
        tokio::time::sleep(AUTH_POLL_INTERVAL).await;
    }
}

async fn monitor_command_with_auth_retry(
    app: &AppHandle,
    state: &MinilmMigrationState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    loop {
        wait_for_auth(app, state).await;
        let credential = app.state::<Arc<CredentialManagerState>>();
        let monitor = app.state::<MonitorState>();
        let result = authenticated_monitor_command(&credential, &monitor, payload.clone())
            .await
            .and_then(parse_monitor_success);
        match result {
            Err(error) if is_auth_required_error(&error) => continue,
            other => return other,
        }
    }
}

async fn background_read_with_auth_retry<T>(
    app: &AppHandle,
    state: &MinilmMigrationState,
    mut read: impl FnMut(&StorageState) -> Result<T, BackgroundReadError>,
) -> Result<T, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    loop {
        wait_for_auth(app, state).await;
        match read(&storage) {
            Ok(value) => return Ok(value),
            Err(BackgroundReadError::AuthRequired) => continue,
            Err(BackgroundReadError::Other(error)) => return Err(error),
        }
    }
}

fn background_error(error: BackgroundReadError) -> String {
    match error {
        BackgroundReadError::AuthRequired => "AUTH_REQUIRED".to_string(),
        BackgroundReadError::Other(error) => error,
    }
}

/// Persist the worker-owned run record and mirror it into the UI status.
fn persist_run(
    storage: &StorageState,
    state: &MinilmMigrationState,
    run: &mut MinilmMigrationRunRecord,
) -> Result<(), String> {
    run.heartbeat_at = now_rfc3339();
    run.updated_at = run.heartbeat_at.clone();
    storage.update_minilm_migration_run(run)?;
    let running = state.running.load(Ordering::SeqCst);
    state.update(|status| *status = status_from_run(run, running));
    Ok(())
}

fn record_error(
    storage: &StorageState,
    state: &MinilmMigrationState,
    run_id: &str,
    subject_key: Option<&str>,
    phase: &str,
    code: &str,
    error: &str,
) {
    state.diagnostic(subject_key.map(str::to_string), phase, code, error);
    if let Err(persist_error) =
        storage.record_minilm_migration_error(run_id, subject_key, phase, code, error)
    {
        tracing::warn!("[MINILM] failed to persist migration diagnostic: {persist_error}");
    }
}

/// Saved monitor/capture state so the migration can restore exactly what the
/// user had, instead of unconditionally resuming.
struct MonitorRestore {
    was_running: bool,
    was_paused: bool,
    started_by_migration: bool,
}

async fn prepare_monitor_for_migration(app: &AppHandle) -> Result<MonitorRestore, String> {
    let monitor = app.state::<MonitorState>();
    let capture = app.state::<Arc<crate::capture::CaptureState>>();
    let was_running = monitor
        .process
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    let was_paused = capture.paused.load(Ordering::SeqCst);
    let mut started_by_migration = false;
    if !was_running {
        // The Chroma hot layer only speaks through the Python monitor.
        crate::monitor::start_monitor_impl(app.state::<MonitorState>(), app.clone())
            .await
            .map_err(|error| format!("Cannot start the monitor for the migration: {error}"))?;
        started_by_migration = true;
    }
    if !was_paused {
        let _ = crate::monitor::pause_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
    }
    Ok(MonitorRestore {
        was_running,
        was_paused,
        started_by_migration,
    })
}

async fn restore_monitor_after_migration(app: &AppHandle, restore: &MonitorRestore) {
    if restore.started_by_migration && !restore.was_running {
        let _ = crate::monitor::stop_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
        return;
    }
    if !restore.was_paused {
        let _ = crate::monitor::resume_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
    }
}

/// Query the persisted snapshot's readiness without creating a new one.
/// Returns `None` when Python no longer knows the export (memory and disk).
async fn attach_chroma_snapshot(
    app: &AppHandle,
    state: &MinilmMigrationState,
    export_id: &str,
) -> Result<Option<u64>, String> {
    let response = monitor_command_with_auth_retry(
        app,
        state,
        serde_json::json!({
            "command": "get_task_vectors_export_status",
            "export_id": export_id,
        }),
    )
    .await?;
    let status: TaskVectorExportStatus = serde_json::from_value(response)
        .map_err(|error| format!("Invalid Chroma export status: {error}"))?;
    match status.state.as_str() {
        "ready" => Ok(Some(status.total)),
        _ => Ok(None),
    }
}

/// Start (or re-attach to) the asynchronous Python ID snapshot and wait until
/// it is ready. `start_task_vectors_export` returns immediately; readiness is
/// polled so a slow Chroma `get` cannot exhaust one long IPC window.
async fn wait_for_chroma_snapshot(
    app: &AppHandle,
    state: &MinilmMigrationState,
    export_id: &str,
) -> Result<u64, String> {
    let response = monitor_command_with_auth_retry(
        app,
        state,
        serde_json::json!({
            "command": "start_task_vectors_export",
            "export_id": export_id,
        }),
    )
    .await?;
    let started: TaskVectorExportStart = serde_json::from_value(response)
        .map_err(|error| format!("Invalid Chroma export start response: {error}"))?;
    if started.export_id != export_id {
        return Err("Chroma export id mismatch".to_string());
    }

    let deadline = tokio::time::Instant::now() + SNAPSHOT_DEADLINE;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "Chroma ID snapshot did not become ready within its deadline; restart the monitor and retry"
                    .to_string(),
            );
        }
        let response = monitor_command_with_auth_retry(
            app,
            state,
            serde_json::json!({
                "command": "get_task_vectors_export_status",
                "export_id": export_id,
            }),
        )
        .await?;
        let status: TaskVectorExportStatus = serde_json::from_value(response)
            .map_err(|error| format!("Invalid Chroma export status: {error}"))?;
        match status.state.as_str() {
            "ready" => return Ok(status.total),
            "preparing" => tokio::time::sleep(SNAPSHOT_POLL_INTERVAL).await,
            other => {
                return Err(format!(
                    "Chroma ID snapshot is {other}: {}",
                    status.error.unwrap_or_else(|| "no detail".to_string())
                ))
            }
        }
    }
}

/// M2.4a: copy every valid, mappable vector of the snapshot into the Rust
/// cache. Pages commit transactionally together with the persisted cursor, so
/// a crash either retries an uncommitted page or continues after it.
async fn copy_chroma_hot_layer(
    app: &AppHandle,
    state: &Arc<MinilmMigrationState>,
    run: &mut MinilmMigrationRunRecord,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    run.phase = "snapshotting_chroma".to_string();
    persist_run(&storage, state, run)?;
    // Resume path: re-attach to the persisted snapshot instead of starting a
    // new export under the same id, which would silently reorder pages under
    // the durable cursor. A snapshot Python cannot restore forces a reset.
    let mut total = None;
    if let Some(export_id) = run.export_id.clone() {
        total = attach_chroma_snapshot(app, state, &export_id).await?;
        if total.is_none() {
            storage.reset_minilm_migration_export(&run.run_id)?;
            *run = storage
                .get_minilm_migration_run(&run.run_id)?
                .ok_or("MiniLM migration run disappeared during reset")?;
        }
    }
    let total = match total {
        Some(total) => total,
        None => {
            let export_id = new_run_id();
            run.export_id = Some(export_id.clone());
            run.phase = "snapshotting_chroma".to_string();
            persist_run(&storage, state, run)?;
            wait_for_chroma_snapshot(app, state, &export_id).await?
        }
    };
    run.chroma_total = total;
    run.phase = "copying_chroma".to_string();
    persist_run(&storage, state, run)?;

    let mut unmappable_total = run.unmappable;
    let mut cursor = run.export_cursor;
    loop {
        let response = monitor_command_with_auth_retry(
            app,
            state,
            serde_json::json!({
                "command": "export_task_vectors_page",
                "export_id": run.export_id,
                "cursor": cursor,
                "limit": CHROMA_PAGE_SIZE,
            }),
        )
        .await?;
        let page: TaskVectorExportPage = serde_json::from_value(response)
            .map_err(|error| format!("Invalid Chroma vector export page: {error}"))?;
        if page.total != run.chroma_total {
            return Err(format!(
                "Chroma export snapshot total changed: expected {}, got {}",
                run.chroma_total, page.total
            ));
        }
        if !page.done && page.next_cursor <= cursor {
            return Err("Chroma export cursor did not advance".to_string());
        }
        let vectors = decode_export_page_vectors(&page)?;

        for missing_id in &page.missing_ids {
            unmappable_total += 1;
            record_error(
                &storage,
                state,
                &run.run_id,
                Some(missing_id),
                "copying_chroma",
                "snapshot_row_missing",
                "A task_vectors row disappeared after the export snapshot was created",
            );
        }
        for export_error in &page.errors {
            unmappable_total += 1;
            record_error(
                &storage,
                state,
                &run.run_id,
                export_error.id.as_deref(),
                "copying_chroma",
                "legacy_vector_decode_failed",
                &export_error.error,
            );
        }

        // Validate outside the transaction: canonical id, shape, finiteness,
        // and a non-zero norm.
        let mut valid: Vec<(i64, &str, Vec<f32>)> = Vec::with_capacity(page.ids.len());
        for (chroma_id, vector) in page.ids.iter().zip(vectors) {
            let screenshot_id = match chroma_id.parse::<i64>() {
                Ok(id) if id > 0 && id.to_string() == *chroma_id => id,
                _ => {
                    unmappable_total += 1;
                    record_error(
                        &storage,
                        state,
                        &run.run_id,
                        Some(chroma_id),
                        "copying_chroma",
                        "invalid_subject_key",
                        "task_vectors id is not a canonical positive screenshot id",
                    );
                    continue;
                }
            };
            if let Err(error) = validate_vector(&vector) {
                unmappable_total += 1;
                record_error(
                    &storage,
                    state,
                    &run.run_id,
                    Some(chroma_id),
                    "copying_chroma",
                    "invalid_vector",
                    &error,
                );
                continue;
            }
            valid.push((screenshot_id, chroma_id, vector));
        }

        let ids: Vec<i64> = valid.iter().map(|(id, _, _)| *id).collect();
        let summaries = background_read_with_auth_retry(app, state, |storage| {
            storage.get_screenshot_summaries_by_ids_silent(&ids)
        })
        .await?;
        let ocr = background_read_with_auth_retry(app, state, |storage| {
            storage.get_ocr_text_prefixes_by_screenshot_ids_silent(&ids, MINILM_OCR_SNIPPET_CHARS)
        })
        .await?;
        let mut summaries: HashMap<i64, BackgroundScreenshotSummary> = summaries
            .into_iter()
            .map(|summary| (summary.id, summary))
            .collect();

        let mut rows = Vec::with_capacity(valid.len());
        for (screenshot_id, chroma_id, vector) in valid {
            let Some(summary) = summaries.remove(&screenshot_id) else {
                unmappable_total += 1;
                record_error(
                    &storage,
                    state,
                    &run.run_id,
                    Some(chroma_id),
                    "copying_chroma",
                    "inactive_screenshot",
                    "No active SQLite screenshot maps to the Chroma row",
                );
                continue;
            };
            let mut sources = source_rows(vec![summary], &ocr);
            let source = sources.pop().expect("one source row");
            // A valid legacy vector whose current SQLite text is empty is
            // still copied — it must not be recomputed from nothing — but it
            // is marked so the parity gate can treat it separately.
            let outcome = if source.text.is_empty() {
                "legacy_chroma_unverified"
            } else {
                "migrated"
            };
            rows.push(MinilmMigrationPageRow {
                job: source.spec,
                vector,
                outcome: outcome.to_string(),
            });
        }

        storage.commit_minilm_migration_page(&run.run_id, cursor, page.next_cursor, &rows)?;
        *run = storage
            .get_minilm_migration_run(&run.run_id)?
            .ok_or("MiniLM migration run disappeared during copy")?;
        run.unmappable = unmappable_total;
        persist_run(&storage, state, run)?;
        cursor = page.next_cursor;
        if page.done {
            break;
        }
    }
    Ok(())
}

/// Publish the derived-index generation on a blocking worker. `sync_all`
/// cannot be interrupted; the UI presents that window as "safely writing to
/// disk".
async fn publish_generation(
    app: &AppHandle,
    state: &Arc<MinilmMigrationState>,
    run: &mut MinilmMigrationRunRecord,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    run.phase = "publishing_sync".to_string();
    persist_run(&storage, state, run)?;

    let blocking_storage = storage.clone();
    let progress_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        blocking_storage.publish_derived_index_generation_with_progress(
            DerivedIndexKind::SemanticText,
            move |phase, current, total| {
                // In-memory only: DB writes here would contend with the
                // publication's own connection usage.
                progress_state.update(|status| {
                    status.phase = phase.to_string();
                    status.publish_current = current;
                    status.publish_total = total;
                });
            },
            // The migration is no longer cancellable.
            || false,
        )
    })
    .await
    .map_err(|error| format!("Generation publication worker crashed: {error:?}"))?;

    match result {
        Ok(_generation) => {
            let status_snapshot = state
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            run.publish_current = status_snapshot.publish_current;
            run.publish_total = status_snapshot.publish_total;
            run.phase = "publishing_commit".to_string();
            persist_run(&storage, state, run)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn run_minilm_migration(
    app: AppHandle,
    state: Arc<MinilmMigrationState>,
    mut run: MinilmMigrationRunRecord,
    maintenance: crate::maintenance::MaintenanceGuard,
) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    let result = async {
        wait_for_auth(&app, &state).await;

        let restore = prepare_monitor_for_migration(&app).await?;
        run.monitor_was_running = restore.was_running;
        run.monitor_was_paused = restore.was_paused;

        let phase_result = async {
            // Everything after prepare_monitor_for_migration must flow through
            // phase_result: an early `?` here would skip the monitor restore
            // below and leave capture silently paused.
            persist_run(&storage, &state, &mut run)?;
            copy_chroma_hot_layer(&app, &state, &mut run).await?;

            // Every sentinel-triggered run covers the full Chroma hot layer,
            // so rows outside the persisted snapshot can always be removed.
            run.phase = "reconciling".to_string();
            persist_run(&storage, &state, &mut run)?;
            let removed = storage.reconcile_minilm_migration_scope(&run.run_id)?;
            run.removed_extra = removed;
            persist_run(&storage, &state, &mut run)?;

            publish_generation(&app, &state, &mut run).await
        }
        .await;

        // Best-effort snapshot release; the Python-side TTLs cover failures.
        if phase_result.is_ok() {
            if let Some(export_id) = run.export_id.clone() {
                let credential = app.state::<Arc<CredentialManagerState>>();
                let monitor = app.state::<MonitorState>();
                let _ = authenticated_monitor_command(
                    &credential,
                    &monitor,
                    serde_json::json!({
                        "command": "finish_task_vectors_export",
                        "export_id": export_id,
                    }),
                )
                .await;
            }
        }

        restore_monitor_after_migration(&app, &restore).await;
        phase_result
    }
    .await;

    let has_errors = run.failed > 0 || run.unmappable > 0 || run.discarded > 0;
    run.finished_at = Some(now_rfc3339());
    match &result {
        Err(error) => {
            run.status = "failed".to_string();
            run.phase = "failed".to_string();
            run.last_error = Some(error.clone());
        }
        _ if has_errors => {
            run.status = "completed_with_errors".to_string();
            run.phase = "completed_with_errors".to_string();
        }
        _ => {
            run.status = "completed".to_string();
            run.phase = "completed".to_string();
        }
    }
    // Terminally quarantined legacy rows do not make the copy unfinished.
    // Transient orchestration failures keep status `failed`, do not write the
    // sentinel, and therefore resume after the next launch/unlock.
    if run_settles_sentinel(&run) {
        if let Err(error) = storage.mark_minilm_auto_migration_done(&run.vector_space_revision) {
            tracing::warn!("[MINILM] failed to write the auto-migration sentinel: {error}");
        }
    }
    if let Err(error) = persist_run(&storage, &state, &mut run) {
        tracing::error!("[MINILM] failed to persist final migration state: {error}");
    }
    state.update(|status| {
        status.running = false;
    });
    state.running.store(false, Ordering::SeqCst);
    drop(maintenance);
}

fn run_is_resumable(run: &MinilmMigrationRunRecord) -> bool {
    matches!(run.status.as_str(), "running" | "failed")
        && run.phase != "completed"
        && run.phase != "completed_with_errors"
}

fn run_is_compatible_resume(run: &MinilmMigrationRunRecord) -> bool {
    run.mode == MINILM_MIGRATION_MODE
        && run.vector_space_revision == MINILM_VECTOR_SPACE_REVISION
        && run_is_resumable(run)
}

/// A run settles the once-per-revision sentinel only after the full snapshot,
/// reconcile, and generation publication complete. `completed_with_errors`
/// contains only terminally quarantined legacy rows (invalid ids/vectors,
/// disappeared rows, or inactive screenshots), not transient worker errors.
fn run_settles_sentinel(run: &MinilmMigrationRunRecord) -> bool {
    matches!(run.status.as_str(), "completed" | "completed_with_errors")
}

/// Single-flight entry for the startup auto-trigger: CAS the running flag,
/// start the orchestrator, roll the flag back if the start is rejected.
fn try_start_minilm_migration(
    app: &AppHandle,
    migration: &Arc<MinilmMigrationState>,
) -> Result<MinilmRebuildStatus, String> {
    if migration
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A MiniLM migration is already running".to_string());
    }
    let started = start_minilm_migration_inner(app, migration);
    if let Err(error) = &started {
        tracing::warn!("[MINILM] migration start rejected: {error}");
        migration.running.store(false, Ordering::SeqCst);
    }
    started
}

/// What the startup auto-trigger should do, decided from durable state only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoMigrationDecision {
    /// Sentinel present: this revision is settled, never re-trigger.
    AlreadyDone,
    /// Without a sentinel, start the mandatory copy. Compatible interrupted
    /// state is selected later; legacy manual history never settles the marker.
    Start,
}

fn auto_migration_decision(sentinel_done: bool) -> AutoMigrationDecision {
    if sentinel_done {
        AutoMigrationDecision::AlreadyDone
    } else {
        AutoMigrationDecision::Start
    }
}

/// Startup auto-trigger for the mandatory M2.4a copy, following the sentinel
/// design of the plaintext backfill and auto-vacuum markers: consult the
/// durable marker, wait for the user to unlock, then run the copy exactly
/// once per vector-space revision. This is the only trigger — there is no
/// manual start and no cancellation; an interrupted run resumes on the next
/// launch/unlock.
pub fn spawn_minilm_auto_migration(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run_auto_migration(app).await {
            tracing::warn!("[MINILM] automatic migration not started: {error}");
        }
    });
}

async fn run_auto_migration(app: AppHandle) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let migration = app.state::<Arc<MinilmMigrationState>>().inner().clone();
    // Re-evaluate every poll so the unlock-instant decision wins.
    loop {
        let sentinel_done = storage.is_minilm_auto_migration_done(MINILM_VECTOR_SPACE_REVISION)?;
        match auto_migration_decision(sentinel_done) {
            AutoMigrationDecision::AlreadyDone => return Ok(()),
            AutoMigrationDecision::Start => {}
        }
        if migration.running.load(Ordering::SeqCst) {
            // A run is already in flight; its outcome settles the sentinel.
            return Ok(());
        }
        // The copy phase decrypts OCR text to fingerprint every vector, and
        // CNG needs an unlocked session — idle here instead of surfacing the
        // maintenance overlay stuck in waiting_for_auth right at startup.
        let authenticated = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|value| value.is_session_valid())
            .unwrap_or(false);
        if authenticated {
            break;
        }
        tokio::time::sleep(AUTH_POLL_INTERVAL).await;
    }
    tracing::info!("[MINILM] starting the automatic Chroma copy migration");
    try_start_minilm_migration(&app, &migration).map(|_| ())
}

fn start_minilm_migration_inner(
    app: &AppHandle,
    migration: &Arc<MinilmMigrationState>,
) -> Result<MinilmRebuildStatus, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    // Resume only state created by this sentinel-only orchestration mode. Old
    // manual/time-bounded runs may have partial snapshots and must never be
    // reconciled as if they covered the complete Chroma hot layer.
    let mut run = match storage.get_latest_minilm_migration_run()? {
        Some(previous) if run_is_compatible_resume(&previous) => {
            let mut resumed = previous;
            resumed.status = "running".to_string();
            resumed.phase = "starting".to_string();
            resumed.last_error = None;
            resumed.finished_at = None;
            resumed
        }
        _ => MinilmMigrationRunRecord {
            run_id: new_run_id(),
            mode: MINILM_MIGRATION_MODE.to_string(),
            vector_space_revision: MINILM_VECTOR_SPACE_REVISION.to_string(),
            status: "running".to_string(),
            phase: "preflight".to_string(),
            export_id: None,
            export_cursor: 0,
            chroma_total: 0,
            chroma_processed: 0,
            migrated: 0,
            legacy_unverified: 0,
            already_current: 0,
            failed: 0,
            discarded: 0,
            unmappable: 0,
            removed_extra: 0,
            publish_current: 0,
            publish_total: 0,
            required_free_bytes: 0,
            available_free_bytes: 0,
            monitor_was_running: false,
            monitor_was_paused: false,
            last_error: None,
            started_at: now_rfc3339(),
            heartbeat_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            finished_at: None,
        },
    };
    let is_new_run = run.export_id.is_none() && run.chroma_processed == 0;

    // Disk preflight before any vector write. The Chroma snapshot size is not
    // known yet, so active screenshots are the conservative upper bound.
    let expected_rows = storage.count_active_screenshots()?.max(0) as u64;
    let preflight = storage.minilm_migration_disk_preflight(expected_rows)?;
    run.required_free_bytes = preflight.required_free_bytes;
    run.available_free_bytes = preflight.available_free_bytes;
    if !preflight.sufficient {
        return Err(format!(
            "INSUFFICIENT_DISK_SPACE: the migration needs about {} MB free, {} MB available",
            preflight.required_free_bytes / (1024 * 1024),
            preflight.available_free_bytes / (1024 * 1024),
        ));
    }

    let Some(maintenance) = crate::maintenance::enter("minilm_migration") else {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };

    if is_new_run && storage.get_minilm_migration_run(&run.run_id)?.is_none() {
        storage.create_minilm_migration_run(&run)?;
    } else {
        storage.update_minilm_migration_run(&run)?;
    }

    *migration
        .diagnostics
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Vec::new();
    *migration.run_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(run.run_id.clone());
    migration.update(|status| *status = status_from_run(&run, true));

    let worker_state = migration.clone();
    let worker_app = app.clone();
    tauri::async_runtime::spawn(async move {
        run_minilm_migration(worker_app, worker_state, run, maintenance).await;
    });
    Ok(migration
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone())
}

#[tauri::command]
pub fn get_minilm_rebuild_status(
    app: AppHandle,
    migration: State<'_, Arc<MinilmMigrationState>>,
) -> Result<MinilmRebuildStatus, String> {
    if migration.running.load(Ordering::SeqCst) {
        return Ok(migration
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone());
    }
    // No live worker: report the latest persisted run so an interrupted
    // migration is visible (and resumable) after a restart.
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    match storage.get_latest_minilm_migration_run() {
        Ok(Some(run)) => {
            let mut status = status_from_run(&run, false);
            if run_is_resumable(&run) {
                status.phase = "interrupted".to_string();
            }
            Ok(status)
        }
        Ok(None) => Ok(MinilmRebuildStatus::default()),
        Err(_) => {
            // Locked/uninitialized database: fall back to the in-memory view
            // instead of failing a pure status query.
            Ok(migration
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone())
        }
    }
}

#[tauri::command]
pub fn list_minilm_rebuild_errors(
    app: AppHandle,
    credential: State<'_, Arc<CredentialManagerState>>,
    migration: State<'_, Arc<MinilmMigrationState>>,
    storage: State<'_, Arc<StorageState>>,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    crate::commands::check_auth_required(&credential)?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let diagnostics = migration
        .diagnostics
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let run_id = migration
        .run_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .or_else(|| {
            app.state::<Arc<StorageState>>()
                .get_latest_minilm_migration_run()
                .ok()
                .flatten()
                .map(|run| run.run_id)
        });
    let persisted = match &run_id {
        Some(run_id) => storage.list_minilm_migration_errors(run_id, offset, limit)?,
        None => Vec::new(),
    };
    let failed = storage.list_derived_index_jobs(
        DerivedIndexKind::SemanticText,
        Some(DerivedIndexJobStatus::Failed),
        offset,
        limit,
    )?;
    let discarded = storage.list_derived_index_jobs(
        DerivedIndexKind::SemanticText,
        Some(DerivedIndexJobStatus::Discarded),
        offset,
        limit,
    )?;
    Ok(serde_json::json!({
        "run_id": run_id,
        "diagnostics": diagnostics,
        "persisted": persisted,
        "failed": failed,
        "discarded": discarded,
    }))
}

/// Why one Python dual-write row could not be mirrored, and — the part the
/// caller actually acts on — whether retrying it could ever succeed.
///
/// Python keeps a durable retry queue for failed mirrors, and the Rust read
/// path stands down while that queue is non-empty. A row Rust will reject
/// forever (its screenshot was deleted, its vector is malformed) would
/// therefore stall the queue and disable Rust retrieval permanently, so the
/// two kinds of failure have to be distinguishable over the wire.
pub(crate) struct ImportRejection {
    /// True when no amount of retrying changes the outcome, so the caller
    /// should drop the row rather than queue it.
    pub permanent: bool,
    pub message: String,
}

impl ImportRejection {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            permanent: true,
            message: message.into(),
        }
    }

    /// The store, the session or the source read failed. The same row may well
    /// succeed once the session is valid again or the writer stops contending.
    fn transient(message: impl Into<String>) -> Self {
        Self {
            permanent: false,
            message: message.into(),
        }
    }
}

/// Best-effort Python→Rust dual-write for newly produced MiniLM vectors. The
/// caller-provided metadata is deliberately ignored; source text and its
/// fingerprint are rehydrated from SQLite.
pub(crate) fn import_minilm_vector_from_python(
    storage: &StorageState,
    screenshot_id: i64,
    vector: Vec<f32>,
) -> Result<EnsureDerivedIndexJobResult, ImportRejection> {
    if screenshot_id <= 0 {
        return Err(ImportRejection::permanent("screenshot_id must be positive"));
    }
    // A vector of the wrong width, or one carrying non-finite values, is a
    // property of the payload rather than of the moment it arrived.
    validate_vector(&vector).map_err(ImportRejection::permanent)?;
    let summaries = storage
        .get_screenshot_summaries_by_ids_silent(&[screenshot_id])
        .map_err(|error| ImportRejection::transient(background_error(error)))?;
    let ocr = storage
        .get_ocr_text_prefixes_by_screenshot_ids_silent(&[screenshot_id], MINILM_OCR_SNIPPET_CHARS)
        .map_err(|error| ImportRejection::transient(background_error(error)))?;
    let mut sources = source_rows(summaries, &ocr);
    // Chroma keeps documents for screenshots the user has already deleted —
    // Python only prunes the hot layer on age — so this is the expected fate of
    // any queued id that gets deleted before its mirror lands, not an anomaly.
    let source = sources
        .pop()
        .ok_or_else(|| {
            ImportRejection::permanent("No active SQLite screenshot maps to screenshot_id")
        })?;
    if source.text.is_empty() {
        return Err(ImportRejection::permanent(
            "SQLite source produces an empty MiniLM input",
        ));
    }
    let ensured = storage
        .ensure_derived_index_job(&source.spec)
        .map_err(ImportRejection::transient)?;
    if matches!(
        ensured,
        EnsureDerivedIndexJobResult::Queued | EnsureDerivedIndexJobResult::Requeued
    ) {
        let lease = storage
            .mark_derived_index_job_processing(&source.spec)
            .map_err(ImportRejection::transient)?;
        if let Err(error) = storage.commit_derived_embedding(&DerivedEmbeddingWrite {
            job: source.spec.clone(),
            lease_token: lease.clone(),
            vector,
        }) {
            let _ = storage.mark_derived_index_job_failed(
                &source.spec,
                &lease,
                "rust_store_write_failed",
                &error,
                None,
            );
            return Err(ImportRejection::transient(error));
        }
    }
    Ok(ensured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_text_matches_python_contract_and_unicode_slice() {
        let ocr = format!("  {}tail", "界".repeat(199));
        let text = build_minilm_task_text("proc", "title", &ocr);
        assert!(text.starts_with("proc | title | "));
        assert_eq!(text.chars().filter(|c| *c == '界').count(), 198);
        assert!(!text.ends_with("tail"));
        assert_eq!(build_minilm_task_text("", "", "  "), "");
    }

    #[test]
    fn fingerprint_is_versioned_and_deterministic() {
        let first = minilm_source_fingerprint("proc | title | OCR");
        assert_eq!(first, minilm_source_fingerprint("proc | title | OCR"));
        assert_ne!(first, minilm_source_fingerprint("proc | title | OCR2"));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn spec_records_vector_space_contract_not_one_runtime_artifact() {
        let spec = minilm_spec(7, "proc | title | OCR");
        assert_eq!(spec.model_revision, MINILM_VECTOR_SPACE_REVISION);
        assert_eq!(spec.model_revision, "minilm-l12-vector-space-v1");
        assert!(!spec
            .model_revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn auth_required_detection_accepts_monitor_error_context() {
        assert!(is_auth_required_error("AUTH_REQUIRED"));
        assert!(is_auth_required_error(
            "monitor rejected request: AUTH_REQUIRED"
        ));
        assert!(!is_auth_required_error("monitor unavailable"));
    }

    #[test]
    fn vector_validation_rejects_wrong_shape_non_finite_and_zero_vectors() {
        assert!(validate_vector(&vec![0.25; MINILM_DIMENSIONS]).is_ok());
        assert!(validate_vector(&vec![0.25; 10]).is_err());
        let mut invalid = vec![0.25; MINILM_DIMENSIONS];
        invalid[3] = f32::NAN;
        assert!(validate_vector(&invalid).is_err());
        assert!(validate_vector(&vec![0.0; MINILM_DIMENSIONS])
            .unwrap_err()
            .contains("zero vector"));
    }

    #[test]
    fn export_page_base64_roundtrip_and_size_check() {
        let vector: Vec<f32> = (0..MINILM_DIMENSIONS).map(|i| i as f32 * 0.5).collect();
        let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        let page = TaskVectorExportPage {
            ids: vec!["7".to_string()],
            dimensions: MINILM_DIMENSIONS,
            embeddings_f32_le_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            missing_ids: Vec::new(),
            errors: Vec::new(),
            next_cursor: 1,
            done: true,
            total: 1,
        };
        let decoded = decode_export_page_vectors(&page).unwrap();
        assert_eq!(decoded, vec![vector]);

        let truncated = TaskVectorExportPage {
            embeddings_f32_le_b64: base64::engine::general_purpose::STANDARD
                .encode(&bytes[..bytes.len() - 4]),
            ids: vec!["7".to_string()],
            dimensions: MINILM_DIMENSIONS,
            missing_ids: Vec::new(),
            errors: Vec::new(),
            next_cursor: 1,
            done: true,
            total: 1,
        };
        assert!(decode_export_page_vectors(&truncated).is_err());
    }

    #[test]
    fn resumable_run_detection() {
        let mut run = test_run("running");
        assert!(run_is_resumable(&run));
        run.status = "failed".to_string();
        assert!(run_is_resumable(&run));
        run.status = "cancelled".to_string();
        assert!(!run_is_resumable(&run));
        run.status = "completed".to_string();
        assert!(!run_is_resumable(&run));
    }

    #[test]
    fn resume_rejects_legacy_manual_modes_and_other_vector_spaces() {
        let run = test_run("failed");
        assert!(run_is_compatible_resume(&run));

        let mut legacy_manual = run.clone();
        legacy_manual.mode = "copy_chroma_hot_layer".to_string();
        assert!(!run_is_compatible_resume(&legacy_manual));

        let mut old_vector_space = run;
        old_vector_space.vector_space_revision = "minilm-l12-vector-space-v0".to_string();
        assert!(!run_is_compatible_resume(&old_vector_space));
    }

    fn test_run(status: &str) -> MinilmMigrationRunRecord {
        MinilmMigrationRunRecord {
            run_id: "run".to_string(),
            mode: MINILM_MIGRATION_MODE.to_string(),
            vector_space_revision: MINILM_VECTOR_SPACE_REVISION.to_string(),
            status: status.to_string(),
            phase: "copying_chroma".to_string(),
            export_id: None,
            export_cursor: 0,
            chroma_total: 0,
            chroma_processed: 0,
            migrated: 0,
            legacy_unverified: 0,
            already_current: 0,
            failed: 0,
            discarded: 0,
            unmappable: 0,
            removed_extra: 0,
            publish_current: 0,
            publish_total: 0,
            required_free_bytes: 0,
            available_free_bytes: 0,
            monitor_was_running: false,
            monitor_was_paused: false,
            last_error: None,
            started_at: String::new(),
            heartbeat_at: String::new(),
            updated_at: String::new(),
            finished_at: None,
        }
    }

    #[test]
    fn auto_migration_decision_uses_only_the_sentinel() {
        use AutoMigrationDecision::*;
        assert_eq!(auto_migration_decision(true), AlreadyDone);
        // The sentinel is the sole completion authority. Missing means run a
        // full copy even if legacy history claims it previously completed.
        assert_eq!(auto_migration_decision(false), Start);
    }

    #[test]
    fn only_terminal_full_copy_statuses_settle_the_sentinel() {
        assert!(run_settles_sentinel(&test_run("completed")));
        assert!(run_settles_sentinel(&test_run("completed_with_errors")));
        assert!(!run_settles_sentinel(&test_run("failed")));
        assert!(!run_settles_sentinel(&test_run("running")));
    }
}
