//! Resumable, foreground Chinese-CLIP image-vector migration — M2.5 step 7.
//!
//! Copies the existing Chroma `screenshots` collection into the Rust derived
//! cache under `clip_image`, from a stable persisted ID snapshot, then removes
//! Rust rows outside that snapshot's scope. **No inference runs here.** The
//! roadmap's migration gate is explicit that existing CLIP vectors are
//! float-copied rather than re-encoded: re-encoding tens of thousands of
//! screenshots through a vision transformer is hours of work to reproduce
//! vectors the machine already has.
//!
//! Triggered exclusively by an `app_metadata` sentinel at startup, exactly like
//! the MiniLM copy it borrows its orchestration from. There is no manual start
//! and no cancellation; closing the app is the only interruption, and the run
//! resumes from its persisted cursor on the next launch.
//!
//! ## Why this migration maps ids instead of parsing them
//!
//! `task_vectors` is keyed by `str(screenshot_id)`, so MiniLM recovers its
//! subject key by parsing. The CLIP collection is keyed by
//! `md5("memory://" + image_hash)` (`vector_store.py::_compute_id`), which is
//! not invertible. Two ways back exist and only one is acceptable:
//!
//! - Read the stored `image_path` metadata. It is encrypted at rest
//!   (`vector_store.py::add_image` runs it through `_encrypt_text`), so this
//!   would mean decrypting user file paths into an IPC payload to recover
//!   something SQLite already holds.
//! - Hash forward. Rust lists its own eligible image hashes, computes the same
//!   MD5 over each, and matches. Nothing sensitive crosses the pipe, and an id
//!   the map does not contain is exactly the definition of an orphan.
//!
//! The second is what this module does. The map is built once per run and held
//! for its duration — roughly 100 bytes per screenshot, against a copy that
//! already runs under maintenance mode.
//!
//! ## Why the source fingerprint is the image hash
//!
//! A `semantic_text` row's fingerprint covers text that can change under it, so
//! a re-OCR invalidates the vector. A CLIP image vector is a function of the
//! pixels alone, and `image_hash` *is* the identity of those pixels — the same
//! hash cannot denote different pixels. So the fingerprint is derived from the
//! hash, which makes a migrated row and a Rust-encoded row of the same image
//! agree on their contract without either having to know which produced it.

use crate::credential_manager::CredentialManagerState;
use crate::migration_support::{self, ExportPage, MigrationPhaseSink, SnapshotCommands};
use crate::storage::{
    DerivedIndexJobSpec, DerivedIndexKind, DerivedMigrationPageRow, DerivedMigrationRunRecord,
    StorageState,
};
use md5::Md5;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};

/// The Python commands that drive the `screenshots` snapshot.
const SNAPSHOT_COMMANDS: SnapshotCommands = SnapshotCommands {
    start: "start_clip_vectors_export",
    status: "get_clip_vectors_export_status",
    page: "export_clip_vectors_page",
    finish: "finish_clip_vectors_export",
};

pub const CLIP_MODEL_ID: &str = "chinese-clip-vit-base-patch16";
/// Compatibility contract for the shared CLIP vector space.
///
/// Recorded instead of a concrete ONNX artifact revision for the same reason
/// MiniLM records one: legacy Chroma rows carry no provenance, so claiming they
/// came from `model_q4.onnx` would be an invention. Rust-encoded rows join the
/// same space, which is what lets the two coexist in one index.
///
/// A user whose collection was built by the PyTorch path and one whose was
/// built by `onnxruntime-directml` both land here. That is a deliberate
/// tolerance, not an oversight: both are Chinese-CLIP ViT-B/16 projections into
/// the same 512-dimensional space, and cross-modal retrieval at a 0.32 cosine
/// floor does not resolve the difference between them.
pub const CLIP_VECTOR_SPACE_REVISION: &str = "chinese-clip-vit-b16-vector-space-v1";
const CLIP_MIGRATION_MODE: &str = "sentinel_copy_chroma_screenshots_v1";
pub const CLIP_EMBEDDING_VERSION: u32 = 1;
pub const CLIP_DIMENSIONS: usize = 512;
/// Zero (or numerically negligible) vectors would poison cosine queries; they
/// are quarantined as diagnostics instead of imported.
pub const CLIP_MIN_L2_NORM: f32 = 1e-6;
const CHROMA_PAGE_SIZE: u64 = 128;
const MAX_DIAGNOSTICS: usize = 500;
/// How often startup re-checks for an unlocked vault before starting the copy.
const AUTH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// The `image_path` every capture-written CLIP row was keyed under.
///
/// `worker_process.py` passes `f"memory://{image_hash}"` to `add_image`, which
/// MD5s it into the document id. Reproducing that string exactly is the whole
/// mapping, so it lives in one function with a test rather than being inlined
/// at the two places that need it.
pub fn clip_memory_uri(image_hash: &str) -> String {
    format!("memory://{image_hash}")
}

/// The Chroma document id for one image hash.
pub fn clip_document_id(image_hash: &str) -> String {
    let mut digest = Md5::new();
    digest.update(clip_memory_uri(image_hash).as_bytes());
    format!("{:x}", digest.finalize())
}

/// Versioned fingerprint of a CLIP row's source.
///
/// Versioned so that a future change to what the encoder is fed — a different
/// crop, say — invalidates every stored row rather than leaving two contracts
/// silently sharing one index.
pub fn clip_source_fingerprint(image_hash: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"clip-image-source-v1\0");
    digest.update(image_hash.as_bytes());
    format!("{:x}", digest.finalize())
}

pub fn clip_job_spec(image_hash: &str) -> DerivedIndexJobSpec {
    DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::ClipImage,
        subject_key: image_hash.to_string(),
        model_id: CLIP_MODEL_ID.to_string(),
        model_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
        embedding_version: CLIP_EMBEDDING_VERSION,
        source_fingerprint: clip_source_fingerprint(image_hash),
    }
}

pub(crate) fn validate_clip_vector(vector: &[f32]) -> Result<(), String> {
    migration_support::validate_migrated_vector(vector, CLIP_DIMENSIONS, CLIP_MIN_L2_NORM)
}

/// Codes this migration records against `derived_migration_errors`.
///
/// Named once, because two sides read them and they mean different things to a
/// user. `clip_index.rs::get_clip_backfill_offer` splits the census into "rows
/// correctly skipped" and "rows that could not be imported", and a literal
/// retyped there would silently move a whole population from one column to the
/// other the first time somebody renamed one here.
pub mod diagnostic_code {
    /// The Chroma snapshot listed an id whose row was gone by the time the page
    /// was read. Expected under concurrent deletion.
    pub const SNAPSHOT_ROW_MISSING: &str = "snapshot_row_missing";
    /// No live SQLite image hash reproduces this document id — the ordinary
    /// result of the user having deleted the screenshot.
    pub const ORPHAN_DOCUMENT_ID: &str = "orphan_document_id";
    /// The screenshot went away between building the id map and committing.
    pub const SCREENSHOT_DISAPPEARED: &str = "screenshot_disappeared";
    /// Python could not read the stored vector back out of Chroma.
    pub const LEGACY_VECTOR_DECODE_FAILED: &str = "legacy_vector_decode_failed";
    /// The vector arrived, and is not usable for cosine scoring.
    pub const INVALID_VECTOR: &str = "invalid_vector";
    /// The run itself stopped, recorded against no subject.
    pub const RUN_FAILED: &str = "run_failed";
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipMigrationDiagnostic {
    pub subject_key: Option<String>,
    pub phase: String,
    pub code: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipRebuildStatus {
    pub running: bool,
    pub phase: String,
    pub run_id: Option<String>,
    pub mode: String,
    pub chroma_total: u64,
    pub chroma_processed: u64,
    pub migrated: u64,
    pub already_current: u64,
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

impl Default for ClipRebuildStatus {
    fn default() -> Self {
        Self {
            running: false,
            phase: "idle".to_string(),
            run_id: None,
            mode: String::new(),
            chroma_total: 0,
            chroma_processed: 0,
            migrated: 0,
            already_current: 0,
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

fn status_from_run(run: &DerivedMigrationRunRecord, running: bool) -> ClipRebuildStatus {
    ClipRebuildStatus {
        running,
        phase: run.phase.clone(),
        run_id: Some(run.run_id.clone()),
        mode: run.mode.clone(),
        chroma_total: run.chroma_total,
        chroma_processed: run.chroma_processed,
        migrated: run.migrated,
        already_current: run.already_current,
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

pub struct ClipMigrationState {
    running: AtomicBool,
    status: Mutex<ClipRebuildStatus>,
    diagnostics: Mutex<Vec<ClipMigrationDiagnostic>>,
    run_id: Mutex<Option<String>>,
}

impl Default for ClipMigrationState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipMigrationState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            status: Mutex::new(ClipRebuildStatus::default()),
            diagnostics: Mutex::new(Vec::new()),
            run_id: Mutex::new(None),
        }
    }

    fn update(&self, update: impl FnOnce(&mut ClipRebuildStatus)) {
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
        diagnostics.push(ClipMigrationDiagnostic {
            subject_key,
            phase: phase.to_string(),
            code: code.to_string(),
            error,
        });
    }
}

impl MigrationPhaseSink for ClipMigrationState {
    fn set_phase(&self, phase: &str) {
        self.update(|status| status.phase = phase.to_string());
    }
}

/// Persist the worker-owned run record and mirror it into the UI status.
fn persist_run(
    storage: &StorageState,
    state: &ClipMigrationState,
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
    state: &ClipMigrationState,
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
        tracing::warn!("[CLIP] failed to persist migration diagnostic: {persist_error}");
    }
}

/// `md5(memory://hash) -> hash` for every image a vector may belong to.
///
/// Built once per run, and one entry per image: `screenshots.image_hash` is
/// UNIQUE, so the map is injective in both directions and a Chroma id either
/// resolves to exactly one image or to none at all.
fn build_document_id_map(storage: &StorageState) -> Result<HashMap<String, String>, String> {
    let hashes = storage.list_clip_eligible_image_hashes()?;
    let mut map = HashMap::with_capacity(hashes.len());
    for hash in hashes {
        map.insert(clip_document_id(&hash), hash);
    }
    Ok(map)
}

/// Copy every valid, mappable vector of the snapshot into the Rust cache.
///
/// Pages commit transactionally together with the persisted cursor, so a crash
/// either retries an uncommitted page or continues after it.
async fn copy_chroma_screenshots(
    app: &AppHandle,
    state: &Arc<ClipMigrationState>,
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
                .ok_or("CLIP migration run disappeared during reset")?;
        }
    }
    let total = match total {
        Some(total) => total,
        None => {
            let export_id = migration_support::new_run_id("clip");
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

    // Built after the snapshot is ready and before the first page, so every
    // page is mapped against one consistent view of SQLite. A screenshot
    // deleted mid-run therefore resolves the same way on every page — as an
    // orphan — instead of mapping on page one and not on page nine.
    let map_storage = storage.clone();
    let document_ids = tokio::task::spawn_blocking(move || build_document_id_map(&map_storage))
        .await
        .map_err(|error| format!("CLIP id map task failed: {error}"))??;
    tracing::info!(
        "[CLIP] mapping {} Chroma row(s) against {} eligible image hash(es)",
        total,
        document_ids.len()
    );

    let mut unmappable_total = run.unmappable;
    let mut cursor = run.export_cursor;
    loop {
        let response = migration_support::monitor_command_with_auth_retry(
            app,
            &**state,
            serde_json::json!({
                "command": SNAPSHOT_COMMANDS.page,
                "export_id": run.export_id,
                "cursor": cursor,
                "limit": CHROMA_PAGE_SIZE,
            }),
        )
        .await?;
        let page: ExportPage = serde_json::from_value(response)
            .map_err(|error| format!("Invalid CLIP vector export page: {error}"))?;
        if page.total != run.chroma_total {
            return Err(format!(
                "CLIP export snapshot total changed: expected {}, got {}",
                run.chroma_total, page.total
            ));
        }
        if !page.done && page.next_cursor <= cursor {
            return Err("CLIP export cursor did not advance".to_string());
        }
        let vectors = migration_support::decode_export_page_vectors(&page, CLIP_DIMENSIONS)?;

        for missing_id in &page.missing_ids {
            unmappable_total += 1;
            record_error(
                &storage,
                state,
                &run.run_id,
                Some(missing_id),
                "copying_chroma",
                diagnostic_code::SNAPSHOT_ROW_MISSING,
                "A screenshots row disappeared after the export snapshot was created",
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
                diagnostic_code::LEGACY_VECTOR_DECODE_FAILED,
                &export_error.error,
            );
        }

        let mut rows = Vec::with_capacity(page.ids.len());
        for (document_id, vector) in page.ids.iter().zip(vectors) {
            let Some(image_hash) = document_ids.get(document_id) else {
                // No active, OCR-bearing screenshot hashes to this id. Either
                // the screenshot was deleted after it was indexed, or the row
                // predates the `memory://` key scheme. Neither is recoverable
                // and neither is an error in the sense of "retry later".
                unmappable_total += 1;
                record_error(
                    &storage,
                    state,
                    &run.run_id,
                    Some(document_id),
                    "copying_chroma",
                    diagnostic_code::ORPHAN_DOCUMENT_ID,
                    "No active SQLite image hash reproduces this Chroma document id",
                );
                continue;
            };
            if let Err(error) = validate_clip_vector(&vector) {
                unmappable_total += 1;
                record_error(
                    &storage,
                    state,
                    &run.run_id,
                    Some(document_id),
                    "copying_chroma",
                    diagnostic_code::INVALID_VECTOR,
                    &error,
                );
                continue;
            }
            rows.push(DerivedMigrationPageRow {
                job: clip_job_spec(image_hash),
                vector,
                // Unlike MiniLM, there is no `legacy_chroma_unverified` case
                // here. That outcome exists for a text row whose SQLite source
                // has since gone empty, so the copied vector cannot be checked
                // against anything. A CLIP row's source is the image hash, and
                // the hash is how the row was found at all — a mapped row is by
                // construction a verified one.
                outcome: "migrated".to_string(),
            });
        }

        // Re-check liveness against SQLite for this page, rather than trusting
        // the run-long map. `commit_derived_migration_page` refuses a row whose
        // subject is no longer active and fails the *whole page* when it finds
        // one, so a screenshot that disappeared since the map was built would
        // abort a run that should simply have skipped it. Deletion during a run
        // is close to impossible — maintenance mode holds a full-window overlay
        // and pauses the delete queue — but "close to impossible" is a reason to
        // make the failure cheap, not a reason to let it be expensive.
        let page_hashes: Vec<String> = rows.iter().map(|row| row.job.subject_key.clone()).collect();
        let live_storage = storage.clone();
        let live: std::collections::HashSet<String> = tokio::task::spawn_blocking(move || {
            live_storage.map_image_hashes_to_screenshot_ids(&page_hashes)
        })
        .await
        .map_err(|error| format!("CLIP liveness task failed: {error}"))??
        .into_keys()
        .collect();
        rows.retain(|row| {
            if live.contains(&row.job.subject_key) {
                return true;
            }
            unmappable_total += 1;
            record_error(
                &storage,
                state,
                &run.run_id,
                Some(&row.job.subject_key),
                "copying_chroma",
                diagnostic_code::SCREENSHOT_DISAPPEARED,
                "The screenshot behind this image was removed while the copy was running",
            );
            false
        });

        storage.commit_derived_migration_page(
            &run.run_id,
            DerivedIndexKind::ClipImage,
            cursor,
            page.next_cursor,
            &rows,
        )?;
        *run = storage
            .get_derived_migration_run(&run.run_id)?
            .ok_or("CLIP migration run disappeared during copy")?;
        run.unmappable = unmappable_total;
        persist_run(&storage, state, run)?;
        cursor = page.next_cursor;
        if page.done {
            break;
        }
    }
    Ok(())
}

async fn publish_generation(
    app: &AppHandle,
    state: &Arc<ClipMigrationState>,
    run: &mut DerivedMigrationRunRecord,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    run.phase = "publishing_sync".to_string();
    persist_run(&storage, state, run)?;

    let progress_state = state.clone();
    migration_support::publish_generation(
        app,
        DerivedIndexKind::ClipImage,
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

async fn run_clip_migration(
    app: AppHandle,
    state: Arc<ClipMigrationState>,
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
            copy_chroma_screenshots(&app, &state, &mut run).await?;

            // Every sentinel-triggered run covers the whole collection, so rows
            // outside the persisted snapshot can always be removed.
            run.phase = "reconciling".to_string();
            persist_run(&storage, &state, &mut run)?;
            let removed = storage
                .reconcile_derived_migration_scope(DerivedIndexKind::ClipImage, &run.run_id)?;
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
            // `last_error` lives on the run row, and a resume clears it
            // (`start_clip_migration_inner`). So the diagnostics table is the
            // only place an orchestration failure can survive the retry that
            // follows it, and a run that fails without leaving one is a run
            // nobody can explain afterwards. The per-subject diagnostics above
            // record why individual rows were quarantined; this records why the
            // copy itself stopped.
            tracing::warn!("[CLIP] migration run {} failed: {error}", run.run_id);
            record_error(
                &storage,
                &state,
                &run.run_id,
                None,
                "run",
                diagnostic_code::RUN_FAILED,
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
        // The roadmap records that this migration has never been run against a
        // real collection. One line per completed run is what turns the next
        // real run into evidence instead of an anecdote.
        tracing::info!(
            "[CLIP] migration run {} {}: {} copied, {} already current, {} unmappable, {} removed out of scope, of {} Chroma row(s)",
            run.run_id,
            run.status,
            run.migrated,
            run.already_current,
            run.unmappable,
            run.removed_extra,
            run.chroma_total,
        );
    }
    // Terminally quarantined legacy rows do not make the copy unfinished. An
    // orphan document id is the routine outcome of ordinary screenshot
    // deletion, so a collection with any deletion history reaches
    // `completed_with_errors` — which must still settle the sentinel, or the
    // migration would re-run under maintenance mode at every launch.
    if run_settles_sentinel(&run) {
        if let Err(error) = storage
            .mark_auto_migration_done(DerivedIndexKind::ClipImage, &run.vector_space_revision)
        {
            tracing::warn!("[CLIP] failed to write the auto-migration sentinel: {error}");
        }
    }
    if let Err(error) = persist_run(&storage, &state, &mut run) {
        tracing::error!("[CLIP] failed to persist final migration state: {error}");
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
    run.mode == CLIP_MIGRATION_MODE
        && run.vector_space_revision == CLIP_VECTOR_SPACE_REVISION
        && run_is_resumable(run)
}

fn run_settles_sentinel(run: &DerivedMigrationRunRecord) -> bool {
    matches!(run.status.as_str(), "completed" | "completed_with_errors")
}

/// Single-flight entry: CAS the running flag, start the orchestrator, roll the
/// flag back if the start is rejected.
fn try_start_clip_migration(
    app: &AppHandle,
    migration: &Arc<ClipMigrationState>,
) -> Result<ClipRebuildStatus, String> {
    if migration
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A CLIP migration is already running".to_string());
    }
    let started = start_clip_migration_inner(app, migration);
    if let Err(error) = &started {
        tracing::warn!("[CLIP] migration start rejected: {error}");
        migration.running.store(false, Ordering::SeqCst);
    }
    started
}

/// Startup auto-trigger for the mandatory copy.
///
/// Deliberately spawned *after* the MiniLM one and gated on the same
/// maintenance guard, which serializes them: two migrations pausing and
/// restoring capture at once would fight over the monitor's state, and each
/// records the state it found as the state to restore.
pub fn spawn_clip_auto_migration(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run_auto_migration(app).await {
            tracing::warn!("[CLIP] automatic migration not started: {error}");
        }
    });
}

async fn run_auto_migration(app: AppHandle) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let migration = app.state::<Arc<ClipMigrationState>>().inner().clone();
    loop {
        let sentinel_done = storage
            .is_auto_migration_done(DerivedIndexKind::ClipImage, CLIP_VECTOR_SPACE_REVISION)?;
        if sentinel_done {
            return Ok(());
        }
        if migration.running.load(Ordering::SeqCst) {
            // A run is already in flight; its outcome settles the sentinel.
            return Ok(());
        }
        // Unlike the MiniLM copy this one decrypts nothing — it reads image
        // hashes, which are not encrypted columns. It still waits for the
        // unlock, because the Python side gates its export on an unlocked
        // session and would answer `AUTH_REQUIRED` to every page.
        let authenticated = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|value| value.is_session_valid())
            .unwrap_or(false);
        if authenticated {
            break;
        }
        tokio::time::sleep(AUTH_POLL_INTERVAL).await;
    }
    // Wait out a MiniLM copy rather than failing against its maintenance
    // guard: the loop above has already returned by the time this runs, so a
    // rejection here would postpone the CLIP copy to the *next* launch.
    loop {
        if !crate::maintenance::is_active() {
            break;
        }
        tokio::time::sleep(AUTH_POLL_INTERVAL).await;
    }
    tracing::info!("[CLIP] starting the automatic Chroma copy migration");
    try_start_clip_migration(&app, &migration).map(|_| ())
}

fn start_clip_migration_inner(
    app: &AppHandle,
    migration: &Arc<ClipMigrationState>,
) -> Result<ClipRebuildStatus, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();

    let mut run = match storage.get_latest_derived_migration_run(DerivedIndexKind::ClipImage)? {
        Some(previous) if run_is_compatible_resume(&previous) => {
            let mut resumed = previous;
            resumed.status = "running".to_string();
            resumed.phase = "starting".to_string();
            resumed.last_error = None;
            resumed.finished_at = None;
            resumed
        }
        _ => DerivedMigrationRunRecord {
            index_kind: DerivedIndexKind::ClipImage,
            run_id: migration_support::new_run_id("clip"),
            mode: CLIP_MIGRATION_MODE.to_string(),
            vector_space_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
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
    if is_new_run {
        // A fresh run means either the first copy or a vector-space revision
        // that invalidated every stored row. Both pose the backfill question
        // again over a different corpus, so a previous answer is cleared rather
        // than silently inherited: "no thanks" to re-encoding 200 images is not
        // the same answer as "no thanks" to re-encoding 40,000.
        if let Err(error) = storage.clear_backfill_decision(DerivedIndexKind::ClipImage) {
            tracing::warn!("[CLIP] could not clear the previous backfill decision: {error}");
        }
    }

    // Disk preflight before any vector write. The distinct eligible image count
    // is the tightest upper bound available before the snapshot exists, and it
    // is the same population the copy can actually map.
    let expected_rows = storage.count_expected_clip_image_rows()?.max(0) as u64;
    let preflight = storage.derived_migration_disk_preflight(CLIP_DIMENSIONS, expected_rows)?;
    run.required_free_bytes = preflight.required_free_bytes;
    run.available_free_bytes = preflight.available_free_bytes;
    if !preflight.sufficient {
        return Err(format!(
            "INSUFFICIENT_DISK_SPACE: the CLIP migration needs about {} MB free, {} MB available",
            preflight.required_free_bytes / (1024 * 1024),
            preflight.available_free_bytes / (1024 * 1024),
        ));
    }

    let Some(maintenance) = crate::maintenance::enter("clip_migration") else {
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
        run_clip_migration(worker_app, worker_state, run, maintenance).await;
    });
    Ok(migration
        .status
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone())
}

#[tauri::command]
pub fn get_clip_rebuild_status(
    app: AppHandle,
    migration: State<'_, Arc<ClipMigrationState>>,
) -> Result<ClipRebuildStatus, String> {
    if migration.running.load(Ordering::SeqCst) {
        return Ok(migration
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone());
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    match storage.get_latest_derived_migration_run(DerivedIndexKind::ClipImage) {
        Ok(Some(run)) => {
            let mut status = status_from_run(&run, false);
            if run_is_resumable(&run) {
                status.phase = "interrupted".to_string();
            }
            Ok(status)
        }
        Ok(None) => Ok(ClipRebuildStatus::default()),
        // Locked/uninitialized database: fall back to the in-memory view
        // instead of failing a pure status query.
        Err(_) => Ok(migration
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()),
    }
}

#[tauri::command]
pub fn list_clip_rebuild_errors(
    app: AppHandle,
    credential: State<'_, Arc<CredentialManagerState>>,
    migration: State<'_, Arc<ClipMigrationState>>,
    storage: State<'_, Arc<StorageState>>,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    crate::commands::check_auth_required(&credential)?;
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let run_id = migration
        .run_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .or_else(|| {
            storage
                .get_latest_derived_migration_run(DerivedIndexKind::ClipImage)
                .ok()
                .flatten()
                .map(|run| run.run_id)
        });
    let _ = app;
    let Some(run_id) = run_id else {
        return Ok(serde_json::json!({ "errors": [] }));
    };
    Ok(serde_json::json!({
        "run_id": run_id,
        "errors": storage.list_derived_migration_errors(&run_id, offset, limit)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_id_reproduces_the_python_key_scheme() {
        // `vector_store.py::_compute_id` is `md5(image_path)` and
        // `worker_process.py` passes `memory://{image_hash}`. These three are
        // pinned against values produced by CPython's own hashlib, not against
        // this module's arithmetic restated — the whole migration maps through
        // this function, so a divergence here would orphan every row in the
        // collection while looking like an empty index.
        assert_eq!(clip_memory_uri("abc123"), "memory://abc123");
        assert_eq!(
            clip_document_id("abc123"),
            "200c8cdc45dea346718762f394f2ac40"
        );
        assert_eq!(
            clip_document_id(&"deadbeef".repeat(8)),
            "16adb578e258ebbe026c81a1c1fae6cb"
        );
        assert_eq!(clip_document_id(""), "7f6e0a2288f241643c9cbe37e8f07cd3");
        assert_ne!(clip_document_id("abc123"), clip_document_id("abc124"));
    }

    #[test]
    fn source_fingerprint_is_versioned_and_derived_from_the_hash_alone() {
        let fingerprint = clip_source_fingerprint("abc123");
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(fingerprint, clip_source_fingerprint("abc123"));
        assert_ne!(fingerprint, clip_source_fingerprint("abc124"));
        // Not a bare hash of the input: the version prefix is what lets a
        // future contract change invalidate every stored row.
        assert_ne!(fingerprint, format!("{:x}", Sha256::digest(b"abc123")));
    }

    #[test]
    fn job_spec_keys_on_the_image_hash_and_records_the_vector_space() {
        let spec = clip_job_spec("abc123");
        assert_eq!(spec.index_kind, DerivedIndexKind::ClipImage);
        // The subject key is the hash itself, which is what
        // `derived_subject_is_active` matches against `screenshots.image_hash`.
        assert_eq!(spec.subject_key, "abc123");
        assert_eq!(spec.model_revision, CLIP_VECTOR_SPACE_REVISION);
        assert_eq!(spec.embedding_version, CLIP_EMBEDDING_VERSION);
    }

    #[test]
    fn vector_validation_pins_the_clip_width() {
        assert!(validate_clip_vector(&vec![0.25; CLIP_DIMENSIONS]).is_ok());
        // A MiniLM row must not be importable into the image index.
        assert!(validate_clip_vector(&vec![0.25; 384]).is_err());
        assert!(validate_clip_vector(&vec![0.0; CLIP_DIMENSIONS])
            .unwrap_err()
            .contains("zero vector"));
    }

    #[test]
    fn only_a_finished_run_settles_the_sentinel() {
        let mut run = DerivedMigrationRunRecord {
            index_kind: DerivedIndexKind::ClipImage,
            run_id: "run".to_string(),
            mode: CLIP_MIGRATION_MODE.to_string(),
            vector_space_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
            status: "running".to_string(),
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
        };
        assert!(!run_settles_sentinel(&run));
        assert!(run_is_resumable(&run));
        assert!(run_is_compatible_resume(&run));

        // Orphaned document ids are the ordinary consequence of deleting a
        // screenshot, so a run that hits them must still settle — otherwise the
        // copy repeats under maintenance mode at every launch.
        run.status = "completed_with_errors".to_string();
        run.phase = "completed_with_errors".to_string();
        assert!(run_settles_sentinel(&run));
        assert!(!run_is_resumable(&run));

        // A different vector space is a different index; never resume into it.
        run.status = "running".to_string();
        run.phase = "copying_chroma".to_string();
        run.vector_space_revision = "something-else".to_string();
        assert!(!run_is_compatible_resume(&run));
    }
}
