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
//! `derived_migration_runs` state, so a crash resumes from the persisted
//! cursor instead of starting over. Chroma/Python remains the authoritative
//! query backend throughout M2.4.

use crate::credential_manager::CredentialManagerState;
use crate::migration_support::{self, ExportPage, MigrationPhaseSink, SnapshotCommands};
use crate::storage::{
    BackgroundScreenshotSummary, DerivedIndexJobSpec, DerivedIndexJobStatus, DerivedIndexKind,
    DerivedMigrationPageRow, DerivedMigrationRunRecord, StorageState,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// The Python commands that drive the `task_vectors` snapshot.
const SNAPSHOT_COMMANDS: SnapshotCommands = SnapshotCommands {
    start: "start_task_vectors_export",
    status: "get_task_vectors_export_status",
    page: "export_task_vectors_page",
    finish: "finish_task_vectors_export",
};

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
/// How often startup re-checks for an unlocked vault before starting the copy.
const AUTH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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

fn status_from_run(run: &DerivedMigrationRunRecord, running: bool) -> MinilmRebuildStatus {
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

impl MigrationPhaseSink for MinilmMigrationState {
    fn set_phase(&self, phase: &str) {
        self.update(|status| status.phase = phase.to_string());
    }
}

struct SourceRow {
    text: String,
    spec: DerivedIndexJobSpec,
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

pub(crate) fn minilm_job_spec(screenshot_id: i64, text: &str) -> DerivedIndexJobSpec {
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
            let spec = minilm_job_spec(summary.id, &text);
            SourceRow { text, spec }
        })
        .collect()
}

pub(crate) fn validate_minilm_vector(vector: &[f32]) -> Result<(), String> {
    migration_support::validate_migrated_vector(vector, MINILM_DIMENSIONS, MINILM_MIN_L2_NORM)
}

/// Persist the worker-owned run record and mirror it into the UI status.
fn persist_run(
    storage: &StorageState,
    state: &MinilmMigrationState,
    run: &mut DerivedMigrationRunRecord,
) -> Result<(), String> {
    run.heartbeat_at = migration_support::now_rfc3339();
    run.updated_at = run.heartbeat_at.clone();
    storage.update_derived_migration_run(run)?;
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
        storage.record_derived_migration_error(run_id, subject_key, phase, code, error)
    {
        tracing::warn!("[MINILM] failed to persist migration diagnostic: {persist_error}");
    }
}

/// M2.4a: copy every valid, mappable vector of the snapshot into the Rust
/// cache. Pages commit transactionally together with the persisted cursor, so
/// a crash either retries an uncommitted page or continues after it.
async fn copy_chroma_hot_layer(
    app: &AppHandle,
    state: &Arc<MinilmMigrationState>,
    run: &mut DerivedMigrationRunRecord,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    run.phase = "snapshotting_chroma".to_string();
    persist_run(&storage, state, run)?;
    // Resume path: re-attach to the persisted snapshot instead of starting a
    // new export under the same id, which would silently reorder pages under
    // the durable cursor. A snapshot Python cannot restore forces a reset.
    let mut total = None;
    if let Some(export_id) = run.export_id.clone() {
        total = migration_support::attach_snapshot(app, &**state, SNAPSHOT_COMMANDS, &export_id)
            .await?;
        if total.is_none() {
            storage.reset_derived_migration_export(&run.run_id)?;
            *run = storage
                .get_derived_migration_run(&run.run_id)?
                .ok_or("MiniLM migration run disappeared during reset")?;
        }
    }
    let total = match total {
        Some(total) => total,
        None => {
            let export_id = migration_support::new_run_id("minilm");
            run.export_id = Some(export_id.clone());
            run.phase = "snapshotting_chroma".to_string();
            persist_run(&storage, state, run)?;
            migration_support::wait_for_snapshot(app, &**state, SNAPSHOT_COMMANDS, &export_id)
                .await?
        }
    };
    run.chroma_total = total;
    run.phase = "copying_chroma".to_string();
    persist_run(&storage, state, run)?;

    let mut unmappable_total = run.unmappable;
    let mut cursor = run.export_cursor;
    loop {
        let response = migration_support::monitor_command_with_auth_retry(
            app,
            &**state,
            serde_json::json!({
                "command": "export_task_vectors_page",
                "export_id": run.export_id,
                "cursor": cursor,
                "limit": CHROMA_PAGE_SIZE,
            }),
        )
        .await?;
        let page: ExportPage = serde_json::from_value(response)
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
        let vectors = migration_support::decode_export_page_vectors(&page, MINILM_DIMENSIONS)?;

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
            if let Err(error) = validate_minilm_vector(&vector) {
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
        let summaries =
            migration_support::background_read_with_auth_retry(app, &**state, |storage| {
                storage.get_screenshot_summaries_by_ids_silent(&ids)
            })
            .await?;
        let ocr = migration_support::background_read_with_auth_retry(app, &**state, |storage| {
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
            rows.push(DerivedMigrationPageRow {
                job: source.spec,
                vector,
                outcome: outcome.to_string(),
            });
        }

        storage.commit_derived_migration_page(
            &run.run_id,
            DerivedIndexKind::SemanticText,
            cursor,
            page.next_cursor,
            &rows,
        )?;
        *run = storage
            .get_derived_migration_run(&run.run_id)?
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
    run: &mut DerivedMigrationRunRecord,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    run.phase = "publishing_sync".to_string();
    persist_run(&storage, state, run)?;

    let progress_state = state.clone();
    migration_support::publish_generation(
        app,
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
    )
    .await?;

    let status_snapshot = state
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    run.publish_current = status_snapshot.publish_current;
    run.publish_total = status_snapshot.publish_total;
    run.phase = "publishing_commit".to_string();
    persist_run(&storage, state, run)
}

async fn run_minilm_migration(
    app: AppHandle,
    state: Arc<MinilmMigrationState>,
    mut run: DerivedMigrationRunRecord,
    maintenance: crate::maintenance::MaintenanceGuard,
) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    let result = async {
        migration_support::wait_for_auth(&app, &*state).await;

        let restore = migration_support::prepare_monitor_for_migration(&app).await?;
        run.monitor_was_running = restore.was_running();
        run.monitor_was_paused = restore.was_paused();

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
            let removed = storage
                .reconcile_derived_migration_scope(DerivedIndexKind::SemanticText, &run.run_id)?;
            run.removed_extra = removed;
            persist_run(&storage, &state, &mut run)?;

            publish_generation(&app, &state, &mut run).await
        }
        .await;

        // Best-effort snapshot release; the Python-side TTLs cover failures.
        if phase_result.is_ok() {
            if let Some(export_id) = run.export_id.clone() {
                migration_support::release_snapshot(&app, SNAPSHOT_COMMANDS, &export_id).await;
            }
        }

        migration_support::restore_monitor_after_migration(&app, &restore).await;
        phase_result
    }
    .await;

    let has_errors = run.failed > 0 || run.unmappable > 0 || run.discarded > 0;
    run.finished_at = Some(migration_support::now_rfc3339());
    match &result {
        Err(error) => {
            run.status = "failed".to_string();
            run.phase = "failed".to_string();
            run.last_error = Some(error.clone());
            // Same reasoning as the CLIP copy: a resume clears `last_error`, so
            // without this row the only account of why the run needed resuming
            // is erased by the resume itself. The overlay shows `last_error`
            // while the run is live; this is what remains afterwards.
            tracing::warn!("[MINILM] migration run {} failed: {error}", run.run_id);
            record_error(
                &storage,
                &state,
                &run.run_id,
                None,
                "run",
                "run_failed",
                error,
            );
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
    if result.is_ok() {
        tracing::info!(
            "[MINILM] migration run {} {}: {} copied, {} already current, {} unmappable, {} removed out of scope, of {} Chroma row(s)",
            run.run_id,
            run.status,
            run.migrated,
            run.already_current,
            run.unmappable,
            run.removed_extra,
            run.chroma_total,
        );
    }
    // Terminally quarantined legacy rows do not make the copy unfinished.
    // Transient orchestration failures keep status `failed`, do not write the
    // sentinel, and therefore resume after the next launch/unlock.
    if run_settles_sentinel(&run) {
        if let Err(error) = storage
            .mark_auto_migration_done(DerivedIndexKind::SemanticText, &run.vector_space_revision)
        {
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

fn run_is_resumable(run: &DerivedMigrationRunRecord) -> bool {
    matches!(run.status.as_str(), "running" | "failed")
        && run.phase != "completed"
        && run.phase != "completed_with_errors"
}

fn run_is_compatible_resume(run: &DerivedMigrationRunRecord) -> bool {
    run.mode == MINILM_MIGRATION_MODE
        && run.vector_space_revision == MINILM_VECTOR_SPACE_REVISION
        && run_is_resumable(run)
}

/// A run settles the once-per-revision sentinel only after the full snapshot,
/// reconcile, and generation publication complete. `completed_with_errors`
/// contains only terminally quarantined legacy rows (invalid ids/vectors,
/// disappeared rows, or inactive screenshots), not transient worker errors.
fn run_settles_sentinel(run: &DerivedMigrationRunRecord) -> bool {
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
        let sentinel_done = storage
            .is_auto_migration_done(DerivedIndexKind::SemanticText, MINILM_VECTOR_SPACE_REVISION)?;
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
    let mut run = match storage.get_latest_derived_migration_run(DerivedIndexKind::SemanticText)? {
        Some(previous) if run_is_compatible_resume(&previous) => {
            let mut resumed = previous;
            resumed.status = "running".to_string();
            resumed.phase = "starting".to_string();
            resumed.last_error = None;
            resumed.finished_at = None;
            resumed
        }
        _ => DerivedMigrationRunRecord {
            index_kind: DerivedIndexKind::SemanticText,
            run_id: migration_support::new_run_id("minilm"),
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
            started_at: migration_support::now_rfc3339(),
            heartbeat_at: migration_support::now_rfc3339(),
            updated_at: migration_support::now_rfc3339(),
            finished_at: None,
        },
    };
    let is_new_run = run.export_id.is_none() && run.chroma_processed == 0;

    // Disk preflight before any vector write. The Chroma snapshot size is not
    // known yet, so active screenshots are the conservative upper bound.
    let expected_rows = storage.count_active_screenshots()?.max(0) as u64;
    let preflight = storage.derived_migration_disk_preflight(MINILM_DIMENSIONS, expected_rows)?;
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

    if is_new_run && storage.get_derived_migration_run(&run.run_id)?.is_none() {
        storage.create_derived_migration_run(&run)?;
    } else {
        storage.update_derived_migration_run(&run)?;
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
    match storage.get_latest_derived_migration_run(DerivedIndexKind::SemanticText) {
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
                .get_latest_derived_migration_run(DerivedIndexKind::SemanticText)
                .ok()
                .flatten()
                .map(|run| run.run_id)
        });
    let persisted = match &run_id {
        Some(run_id) => storage.list_derived_migration_errors(run_id, offset, limit)?,
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
        let spec = minilm_job_spec(7, "proc | title | OCR");
        assert_eq!(spec.model_revision, MINILM_VECTOR_SPACE_REVISION);
        assert_eq!(spec.model_revision, "minilm-l12-vector-space-v1");
        assert!(!spec
            .model_revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn vector_validation_pins_the_minilm_width() {
        // The shared validator covers non-finite and zero vectors; what is
        // MiniLM's own is that 384 is the only accepted width, so a CLIP row
        // cannot be imported into this index by mistake.
        assert!(validate_minilm_vector(&vec![0.25; MINILM_DIMENSIONS]).is_ok());
        assert!(validate_minilm_vector(&vec![0.25; 512]).is_err());
        assert!(validate_minilm_vector(&vec![0.0; MINILM_DIMENSIONS])
            .unwrap_err()
            .contains("zero vector"));
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

    fn test_run(status: &str) -> DerivedMigrationRunRecord {
        DerivedMigrationRunRecord {
            index_kind: DerivedIndexKind::SemanticText,
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
