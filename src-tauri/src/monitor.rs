//! Screenshot capture lifecycle and resource-control policy.
//!
//! This module owns starting, pausing and stopping the Rust capture loop,
//! capture filters, game-mode suppression, and frontend lifecycle events.

use crate::capture::CaptureState;
use crate::storage::StorageState;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};

use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Performance::*;

/// `MonitorState::lifecycle` while no capture session owns the monitor.
const LIFECYCLE_IDLE: u64 = 0;
/// `MonitorState::lifecycle` while a session that no longer captures is being
/// drained, whether a stop took it over or its loop ended on its own. Starts
/// are refused until the drain ends.
const LIFECYCLE_DRAINING: u64 = u64::MAX;

pub struct MonitorState {
    /// Which capture session owns the monitor: `LIFECYCLE_IDLE`,
    /// `LIFECYCLE_DRAINING`, or the id of the running session.
    ///
    /// One word instead of a flag, so a loop that ends on its own can give
    /// back exactly its own session. A stop that already took the session
    /// over, or a session started after it, is left alone.
    lifecycle: AtomicU64,
    /// Session ids are never reused, so a late exit cannot be mistaken for the
    /// session that replaced it.
    next_session: AtomicU64,
    /// Game mode: whether DirectML is currently suppressed due to game mode
    pub game_mode_dml_suppressed: AtomicBool,
    /// Game mode: whether DirectML is permanently suppressed due to game mode (until next restart)
    pub game_mode_permanently_suppressed: AtomicBool,
    /// Game mode: background task handle for monitoring game mode changes
    pub game_mode_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Set to true during a stop, after a capture loop ended on its own, and
    /// at application exit; cleared by the next start
    pub stopping: AtomicBool,
    /// Prevents the monitor from restarting during migration tasks
    pub migration_lock: AtomicBool,
}

impl MonitorState {
    pub fn new() -> Self {
        Self {
            lifecycle: AtomicU64::new(LIFECYCLE_IDLE),
            next_session: AtomicU64::new(1),
            game_mode_dml_suppressed: AtomicBool::new(false),
            game_mode_permanently_suppressed: AtomicBool::new(false),
            game_mode_task: Mutex::new(None),
            stopping: AtomicBool::new(false),
            migration_lock: AtomicBool::new(false),
        }
    }

    /// Whether a capture session owns the monitor, paused or not. A session
    /// still being drained counts until the drain ends.
    pub fn is_running(&self) -> bool {
        self.lifecycle.load(Ordering::SeqCst) != LIFECYCLE_IDLE
    }

    /// Claims the monitor for a new capture session and returns its id, or
    /// `None` while another session owns it.
    fn claim_session(&self) -> Option<u64> {
        let session = self.next_session.fetch_add(1, Ordering::SeqCst);
        self.lifecycle
            .compare_exchange(LIFECYCLE_IDLE, session, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| session)
    }

    /// Takes whichever session owns the monitor over for a stop. Returns
    /// `false` when there is none; the stop then only drains and leaves the
    /// lifecycle alone, so it cannot end a session claimed in the meantime.
    fn take_over_for_stop(&self) -> bool {
        self.lifecycle
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current != LIFECYCLE_IDLE).then_some(LIFECYCLE_DRAINING)
            })
            .is_ok()
    }

    /// Takes `session` over after its capture loop ended on its own. Returns
    /// `false` when a stop already took it over or a newer session owns the
    /// monitor; whichever it is owns the state from here.
    fn take_over_exited_session(&self, session: u64) -> bool {
        self.lifecycle
            .compare_exchange(
                session,
                LIFECYCLE_DRAINING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Ends a drain that took a session over. When two drains overlap, the
    /// first to finish returns the monitor to idle, and a session claimed
    /// after that is left running when the second one finishes.
    fn finish_drain(&self) {
        let _ = self.lifecycle.compare_exchange(
            LIFECYCLE_DRAINING,
            LIFECYCLE_IDLE,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    /// Whether game mode currently prevents GPU inference.
    ///
    /// The two flags have different lifetimes: the ordinary suppression is
    /// cleared when GPU pressure falls, while the permanent flag lasts until
    /// game mode (or the application) is restarted. Callers that choose a
    /// provider must observe both flags so a worker cannot bypass the
    /// game-mode policy.
    pub fn is_dml_suppressed(&self) -> bool {
        self.game_mode_dml_suppressed.load(Ordering::SeqCst)
            || self.game_mode_permanently_suppressed.load(Ordering::SeqCst)
    }

    /// Combines the persisted preference with the live game-mode policy.
    pub fn allows_directml(&self, configured: bool) -> bool {
        configured && !self.is_dml_suppressed()
    }
}

use serde_json::Value;

const MAX_MONITOR_COMMAND_PAYLOAD_BYTES: usize = 256 * 1024;

fn monitor_status_value(state: &MonitorState, capture_state: &CaptureState) -> Value {
    let running = state.is_running() && !state.stopping.load(Ordering::SeqCst);
    serde_json::json!({
        "paused": running && capture_state.paused.load(Ordering::SeqCst),
        "stopped": !running,
    })
}

fn validate_monitor_command_payload(payload: &Value) -> Result<(), String> {
    let payload_size = serde_json::to_vec(payload)
        .map_err(|e| format!("Invalid monitor payload: {}", e))?
        .len();
    if payload_size > MAX_MONITOR_COMMAND_PAYLOAD_BYTES {
        return Err("Monitor command payload too large".to_string());
    }
    Ok(())
}

fn apply_monitor_side_effects(
    command: &str,
    payload: &Value,
    capture_state: Option<&CaptureState>,
    storage: Option<&StorageState>,
) {
    match command {
        "update_filters" => {
            let Some(capture_state) = capture_state else {
                return;
            };
            let Some(storage) = storage else {
                return;
            };
            // Update Rust-side exclusion settings
            let filters = payload
                .get("filters")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let processes = filters
                .get("processes")
                .or_else(|| payload.get("processes"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                });
            let titles = filters
                .get("titles")
                .or_else(|| payload.get("titles"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                });
            let ignore_protected = filters
                .get("ignore_protected")
                .or_else(|| payload.get("ignore_protected"))
                .and_then(|v| v.as_bool());

            {
                let data_dir = storage
                    .data_dir
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                capture_state.update_exclusion_settings(processes, titles, ignore_protected);
                capture_state.save_exclusion_settings(&data_dir);
            }
        }
        "update_advanced_config" => {
            let Some(capture_state) = capture_state else {
                return;
            };
            let ocr_timeout_secs = payload
                .get("ocr_timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    crate::registry_config::get_u32("ocr_timeout_secs").unwrap_or(120) as u64
                })
                .clamp(30, 600) as u32;

            capture_state
                .ocr_timeout_secs
                .store(ocr_timeout_secs, Ordering::SeqCst);
        }
        _ => {}
    }
}

fn dispatch_typed_monitor_command(
    capture_state: Option<&CaptureState>,
    storage: Option<&StorageState>,
    payload: Value,
) -> Result<Value, String> {
    validate_monitor_command_payload(&payload)?;
    let command = payload
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    apply_monitor_side_effects(command, &payload, capture_state, storage);
    Ok(serde_json::json!({ "status": "success" }))
}

#[tauri::command]
pub async fn monitor_search_nl(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    query: String,
    limit: Option<u32>,
    offset: Option<u32>,
    process_names: Option<Vec<String>>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    _fuzzy: Option<bool>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let limit = limit.unwrap_or(20).min(crate::clip_query::MAX_CLIP_RESULTS);
    let offset = offset.unwrap_or(0).min(crate::clip_query::MAX_CLIP_OFFSET);
    let process_names = process_names.unwrap_or_default();

    // `fuzzy` is retained in the command contract but does not apply to
    // text-to-image similarity.
    match crate::clip_query::try_rust_clip_query(
        &app,
        crate::clip_query::ClipQueryRequest {
            query: &query,
            limit,
            offset,
            process_names: &process_names,
            start_time,
            end_time,
        },
    )
    .await
    {
        crate::clip_query::ClipQueryOutcome::Served(results) => Ok(serde_json::json!({
            "status": "success",
            "results": results,
            "backend": "rust",
        })),
        crate::clip_query::ClipQueryOutcome::Unavailable(reason) => {
            Err(format!("CLIP search unavailable: {reason}"))
        }
    }
}

#[tauri::command]
pub async fn monitor_update_filters(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    capture_state: State<'_, Arc<CaptureState>>,
    storage: State<'_, Arc<StorageState>>,
    filters: Value,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let payload = serde_json::json!({
        "command": "update_filters",
        "filters": filters,
    });
    dispatch_typed_monitor_command(Some(&capture_state), Some(&storage), payload)
}

#[tauri::command]
pub async fn monitor_update_advanced_config(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    capture_state: State<'_, Arc<CaptureState>>,
    ocr_timeout_secs: u32,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let payload = serde_json::json!({
        "command": "update_advanced_config",
        "ocr_timeout_secs": ocr_timeout_secs,
    });
    apply_monitor_side_effects(
        "update_advanced_config",
        &payload,
        Some(&capture_state),
        None,
    );
    Ok(serde_json::json!({ "status": "success" }))
}

#[tauri::command]
pub async fn monitor_nl_cluster_query(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    query: String,
    n_results: Option<u32>,
    enable_rerank: Option<bool>,
    _rerank_variant: Option<String>,
) -> Result<Value, String> {
    let enable_rerank = enable_rerank.unwrap_or(false);
    // A non-reranked query keeps its old generous bound: it costs one query
    // encode and a cosine scan, and 200 of those is sub-second. A reranked one
    // is paid per candidate by a CPU cross-encoder, so its bound is the one
    // thing that decides how long the user waits — see `MAX_RERANK_RESULTS`.
    // Clamped here so the command contract and the Rust query implementation
    // share one user-visible upper bound.
    let n_results = n_results.unwrap_or(30).min(if enable_rerank {
        crate::rerank::MAX_RERANK_RESULTS
    } else {
        200
    });
    // Checked before entering the Rust query path so protected screenshot data
    // is never read for an unauthenticated request.
    crate::commands::check_auth_required(&credential_state)?;

    // M2.5 step 6: both halves of the query are Rust-served now. Step 4 took
    // the bi-encoder path; the reranked path — every Smart Cluster calibration
    // query — followed once the cross-encoder had a Rust consumer, and it moved
    // together with the scoring worker that compares against its thresholds.
    let outcome = if enable_rerank {
        crate::semantic_query::try_rust_reranked_nl_query(&app, &query, n_results).await
    } else {
        crate::semantic_query::try_rust_nl_query(&app, &query, n_results).await
    };
    match outcome {
        crate::semantic_query::RustQueryOutcome::Served(response) => return Ok(response),
        crate::semantic_query::RustQueryOutcome::Unavailable(reason) => {
            return Err(format!("Semantic retrieval unavailable: {reason}"));
        }
        // The user stopped it.
        //
        // Returned as a success with `cancelled` set rather than as an error:
        // the view has to tell "you stopped this" apart from "this broke", and
        // an `Err` is rendered as a failure by every caller of this command.
        crate::semantic_query::RustQueryOutcome::Cancelled => {
            return Ok(serde_json::json!({
                "status": "cancelled",
                "cancelled": true,
                "results": [],
                "reranked": false,
                "rerank_variant": null,
                "backend": "rust",
            }));
        }
    }
}

#[tauri::command]
pub async fn monitor_nl_cluster_reranker_status(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    Ok(crate::rerank::reranker_status_value(&app))
}

#[tauri::command]
pub async fn monitor_smart_cluster_worker_status(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    storage: State<'_, Arc<crate::storage::StorageState>>,
    worker: State<'_, Arc<crate::smart_cluster_scoring::SmartClusterWorkerState>>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let storage = storage.inner().clone();
    let (pending, scheduler_task) = tokio::task::spawn_blocking(move || {
        let pending = storage.count_smart_cluster_pending().unwrap_or(0);
        let scheduler_task = storage
            .background_scheduler_task(crate::background_scheduler::TASK_SMART_CLUSTER)
            .ok()
            .flatten();
        (pending, scheduler_task)
    })
    .await
    .map_err(|error| format!("pending count task failed: {error}"))?;
    Ok(crate::smart_cluster_scoring::status_value(
        worker.inner(),
        pending,
        scheduler_task.as_ref(),
    ))
}

#[tauri::command]
pub async fn monitor_smart_cluster_drain_now(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    storage: State<'_, Arc<crate::storage::StorageState>>,
    worker: State<'_, Arc<crate::smart_cluster_scoring::SmartClusterWorkerState>>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let scheduler = app
        .try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        .ok_or_else(|| "Background scheduler is unavailable".to_string())?;
    let count_storage = storage.inner().clone();
    let pending = tokio::task::spawn_blocking(move || count_storage.count_smart_cluster_pending())
        .await
        .map_err(|error| format!("pending count task failed: {error}"))??
        .max(0) as u64;
    let rollback = worker.request_drain_now(pending);
    worker.emit_progress(&app);
    tracing::info!("[SMART_CLUSTER] manual drain queued with {pending} pending snapshot(s)");
    if let Err(error) = scheduler.enqueue(
        &app,
        crate::background_scheduler::BackgroundTaskKind::SmartCluster,
        true,
    ) {
        worker.cancel_pending_drain_request(rollback);
        worker.emit_progress(&app);
        return Err(error);
    }
    Ok(serde_json::json!({ "status": "success", "pending_count": pending }))
}

#[tauri::command]
pub async fn monitor_smart_cluster_stop_drain(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    storage: State<'_, Arc<crate::storage::StorageState>>,
    worker: State<'_, Arc<crate::smart_cluster_scoring::SmartClusterWorkerState>>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    worker.request_stop_drain();
    // Stopping a forced drain must also cancel a request that has not reached
    // the worker yet. Otherwise the durable scheduler row would run it on the
    // next idle tick despite the user's stop action.
    storage.cancel_manual_background_task(crate::background_scheduler::TASK_SMART_CLUSTER)?;
    worker.emit_progress(&app);
    if let Some(scheduler) =
        app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
    {
        scheduler.wake();
    }
    Ok(serde_json::json!({ "status": "success" }))
}

#[tauri::command]
pub async fn monitor_classify_debug(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    title: Option<String>,
    ocr_text: Option<String>,
    process_name: Option<String>,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let result = crate::classification::debug(
        &app,
        crate::classification::scoring::Input {
            title: title.unwrap_or_default(),
            ocr_text: ocr_text.unwrap_or_default(),
            process_name: process_name.unwrap_or_default(),
        },
    )
    .await?;
    crate::commands::check_auth_required(&credential_state)?;
    Ok(result)
}

#[tauri::command]
pub async fn monitor_remove_local_anchors_by_process(
    app: tauri::AppHandle,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    category: String,
    process_name: String,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let removed = crate::classification::remove_local(&app, &category, &process_name).await?;
    Ok(serde_json::json!({"status":"success","removed_count":removed}))
}

/// Starts the Rust capture loop.
pub async fn start_monitor_impl(
    state: State<'_, MonitorState>,
    app: AppHandle,
) -> Result<String, String> {
    if state.migration_lock.load(Ordering::SeqCst) {
        return Err("Cannot start monitor: Migration is currently in progress".to_string());
    }

    // Check if required model files are complete
    if let Ok(model_status) = crate::model_management::check_model_files().await {
        if let Some(obj) = model_status.as_object() {
            let has_incomplete = obj.values().any(|m| {
                m.get("complete").and_then(|c| c.as_bool()) == Some(false)
                    && m.get("required").and_then(|r| r.as_bool()) != Some(false)
            });
            if has_incomplete {
                return Err(
                    "Model files are incomplete. Please download required models first."
                        .to_string(),
                );
            }
        }
    }

    let Some(session) = state.claim_session() else {
        return Ok("Monitor is already running".into());
    };
    state.stopping.store(false, Ordering::SeqCst);
    spawn_capture_loop(&app, session);
    crate::refresh_tray_menu(&app);
    Ok("Monitor started".into())
}

#[tauri::command]
pub async fn start_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    crate::maintenance::guard()?;
    start_monitor_impl(state, app).await
}

#[tauri::command]
pub fn get_monitor_autostart() -> bool {
    crate::registry_config::get_bool("autoStartMonitor").unwrap_or(true)
}

#[tauri::command]
pub fn set_monitor_autostart(
    window: tauri::Window,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    enabled: bool,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    crate::registry_config::set_bool("autoStartMonitor", enabled)
}

/// Spawn the Rust-side capture loop for `session` using CaptureState
fn spawn_capture_loop(app: &AppHandle, session: u64) {
    let capture_state = app.state::<Arc<CaptureState>>();
    let storage = app.state::<Arc<StorageState>>();
    let _monitor_state = app.state::<MonitorState>();

    // Reset capture state for new session
    capture_state.stopped.store(false, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);
    capture_state.in_flight_ocr_count.store(0, Ordering::SeqCst);
    capture_state.clear_wgc_session("spawn_capture_loop_reset");
    capture_state
        .startup_pending_cleanup_cancelled
        .store(false, Ordering::SeqCst);
    // Office observation follows capture; `drain_capture_session` and the
    // storage migrations close this gate.
    app.state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .resume();

    // Load exclusion settings from disk
    {
        let data_dir = storage
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        capture_state.load_exclusion_settings(&data_dir);
    }

    // Load advanced config from registry
    {
        let ocr_timeout_secs = crate::registry_config::get_u32("ocr_timeout_secs")
            .unwrap_or(120)
            .clamp(30, 600);
        capture_state
            .ocr_timeout_secs
            .store(ocr_timeout_secs, Ordering::SeqCst);
        capture_state
            .ocr_cold_start_pending
            .store(true, Ordering::SeqCst);
    }

    let cs = capture_state.inner().clone();
    let st = storage.inner().clone();
    {
        let cleanup_storage = st.clone();
        let cleanup_capture_state = cs.clone();
        tauri::async_runtime::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                cleanup_storage.abort_startup_pending_screenshots(|| {
                    cleanup_capture_state
                        .startup_pending_cleanup_cancelled
                        .load(Ordering::SeqCst)
                })
            })
            .await;
            match result {
                Ok(Ok(aborted)) if aborted > 0 => {
                    tracing::info!(
                        "[DIAG:STARTUP] aborted {} stale pending screenshots",
                        aborted
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("[DIAG:STARTUP] pending cleanup failed: {}", e),
                Err(e) => tracing::warn!("[DIAG:STARTUP] pending cleanup task failed: {}", e),
            }
        });
    }
    // MonitorState is not Arc-wrapped in Tauri managed state, but we access it via AppHandle
    // We need to pass the AppHandle so the capture loop can access MonitorState
    let app_handle = app.clone();
    let exit_capture_state = cs.clone();

    let handle = tauri::async_runtime::spawn(async move {
        // Use AssertUnwindSafe + catch_unwind to detect panics in the capture loop
        let result = std::panic::AssertUnwindSafe(crate::capture::run_capture_loop(
            cs,
            st,
            app_handle.clone(),
        ));
        let panicked = futures::FutureExt::catch_unwind(result).await.is_err();
        if panicked {
            // The global panic hook (installed via error_window::install_panic_hook)
            // already shows the error overlay; the session is ended below.
            tracing::error!("Capture loop panicked (error overlay shown by global hook)");
        }
        // The loop only returns by itself once `stopped` is set, and the stop
        // and exit paths that set it also own the monitor state. Any other
        // end would leave a monitor reporting a session that nothing runs.
        if panicked || !exit_capture_state.stopped.load(Ordering::SeqCst) {
            end_exited_session(&app_handle, &exit_capture_state, session).await;
        }
    });

    let mut guard = capture_state
        .capture_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);

    tracing::info!("Rust capture loop spawned");
}

/// Winds the monitor down after the capture loop of `session` ended without
/// being stopped, so status, tray and maintenance stop describing a loop that
/// no longer runs, and tells the frontend that nobody asked for this stop.
///
/// Runs on the capture task. A stop that arrives meanwhile aborts the task at
/// its next await and completes the drain itself.
async fn end_exited_session(app: &AppHandle, capture_state: &CaptureState, session: u64) {
    let state = app.state::<MonitorState>();
    if !state.take_over_exited_session(session) {
        return;
    }
    tracing::error!(
        "Capture session {} ended without a stop; returning the monitor to stopped",
        session
    );
    state.stopping.store(true, Ordering::SeqCst);
    capture_state.stopped.store(true, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);

    drain_capture_session(app, capture_state, "capture_loop_exited").await;

    state.finish_drain();
    crate::refresh_tray_menu(app);
    let _ = app.emit("monitor-stopped", serde_json::json!({"intentional": false}));
}

/// Releases what a capture session leaves behind once its loop is gone: the
/// WGC session, the in-flight OCR task, and Office observation.
async fn drain_capture_session(app: &AppHandle, capture_state: &CaptureState, reason: &str) {
    // Explicitly release WGC/D3D capture resources: a loop that was
    // force-aborted or panicked never reaches its own teardown.
    capture_state.clear_wgc_session(reason);

    // Wait for in-flight OCR tasks to complete (with timeout)
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while capture_state.in_flight_ocr_count.load(Ordering::SeqCst) > 0 {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "Timed out waiting for {} in-flight OCR tasks",
                capture_state.in_flight_ocr_count.load(Ordering::SeqCst)
            );
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Office association writes carry screenshot ids that only mean something
    // in the current database, and stopping the monitor is what backup and
    // data-directory switches do before they replace it. Drain before those
    // writes can outlive the database they were collected from, and release
    // the worker so it stops talking to Office once capture has stopped.
    app.state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .quiesce(tokio::time::Duration::from_secs(5))
        .await;
}

/// Stops the Rust capture loop.
pub async fn stop_monitor_impl(
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    // Own the session before signalling its loop: a loop that panics from
    // here on finds its session taken and leaves the rest to this stop.
    let took_over = state.take_over_for_stop();

    // 1. Stop the Rust capture loop
    capture_state.stopped.store(true, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);

    // Report the monitor as stopped while the capture task drains
    state.stopping.store(true, Ordering::SeqCst);

    // Abort the capture task
    {
        let mut guard = capture_state
            .capture_task
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    drain_capture_session(&app, &capture_state, "stop_monitor").await;

    if took_over {
        state.finish_drain();
    }
    crate::refresh_tray_menu(&app);
    let _ = app.emit("monitor-stopped", serde_json::json!({"intentional": true}));

    Ok("Monitor stopped".into())
}

#[tauri::command]
pub async fn stop_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    crate::maintenance::guard()?;
    stop_monitor_impl(state, capture_state, app).await
}

/// Pauses screenshot capture without stopping the capture loop.
pub async fn pause_monitor_impl(
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    capture_state.paused.store(true, Ordering::SeqCst);
    crate::refresh_tray_menu(&app);
    Ok(serde_json::json!({ "status": "paused" }).to_string())
}

#[tauri::command]
pub async fn pause_monitor(
    window: tauri::Window,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    crate::maintenance::guard()?;
    pause_monitor_impl(capture_state, app).await
}

/// Resumes screenshot capture after a pause.
pub async fn resume_monitor_impl(
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    capture_state.paused.store(false, Ordering::SeqCst);
    crate::refresh_tray_menu(&app);
    Ok(serde_json::json!({ "status": "resumed" }).to_string())
}

#[tauri::command]
pub async fn resume_monitor(
    window: tauri::Window,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    crate::maintenance::guard()?;
    resume_monitor_impl(capture_state, app).await
}

#[tauri::command]
pub async fn get_monitor_status(
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
) -> Result<String, String> {
    Ok(monitor_status_value(&state, &capture_state).to_string())
}

// GPU enumeration and game-mode resource suppression.

/// Enumerates hardware GPU adapters, excluding software renderers.
pub fn enumerate_gpus_internal() -> Result<Vec<serde_json::Value>, String> {
    // SAFETY: DXGI returns reference-counted COM interfaces whose lifetimes are managed
    // by windows-rs; adapter indices are enumerated until Windows reports exhaustion.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

        let mut gpus = Vec::new();
        let mut i: u32 = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            let desc = adapter
                .GetDesc1()
                .map_err(|e| format!("Failed to get adapter desc: {:?}", e))?;
            // Exclude software renderers from user-selectable acceleration devices.
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                let name = String::from_utf16_lossy(
                    &desc.Description[..desc
                        .Description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.Description.len())],
                );
                gpus.push(serde_json::json!({
                    "id": i,
                    "name": name.trim().to_string(),
                }));
            }
            i += 1;
        }
        Ok(gpus)
    }
}

#[tauri::command]
pub fn enumerate_gpus() -> Result<Vec<serde_json::Value>, String> {
    enumerate_gpus_internal()
}

/// Queries system-wide dedicated-memory use for one GPU via Windows PDH.
fn query_gpu_memory_usage(device_id: u32) -> Result<f64, String> {
    // SAFETY: DXGI COM interfaces are managed by windows-rs; the PDH path is
    // NUL-terminated, output pointers reference correctly typed stack storage, and the
    // query handle is closed on every path after it is opened.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(device_id)
            .map_err(|e| format!("Failed to enum adapter {}: {:?}", device_id, e))?;

        let desc = adapter
            .GetDesc1()
            .map_err(|e| format!("Failed to get adapter desc: {:?}", e))?;

        let total_vram = desc.DedicatedVideoMemory;
        if total_vram == 0 {
            return Ok(0.0);
        }

        // Build the PDH adapter instance name from the DXGI LUID.
        let luid = desc.AdapterLuid;
        let counter_path = format!(
            "\\GPU Adapter Memory(luid_0x{:08X}_0x{:08X}_phys_0)\\Dedicated Usage",
            luid.HighPart as u32, luid.LowPart
        );
        let counter_path_w: Vec<u16> = counter_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // Open and populate the PDH query.
        let mut query = 0isize;
        let status = PdhOpenQueryW(None, 0, &mut query);
        if status != 0 {
            return Err(format!("PdhOpenQuery failed: 0x{:08X}", status));
        }

        let mut counter = 0isize;
        let status = PdhAddEnglishCounterW(
            query,
            windows::core::PCWSTR(counter_path_w.as_ptr()),
            0,
            &mut counter,
        );
        if status != 0 {
            PdhCloseQuery(query);
            return Err(format!(
                "PdhAddEnglishCounter failed for '{}': 0x{:08X}",
                counter_path, status
            ));
        }

        // Collect once to establish the counter baseline before formatting the value.
        let status = PdhCollectQueryData(query);
        if status != 0 {
            PdhCloseQuery(query);
            return Err(format!("PdhCollectQueryData failed: 0x{:08X}", status));
        }

        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status = PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, None, &mut value);
        PdhCloseQuery(query);

        if status != 0 {
            return Err(format!(
                "PdhGetFormattedCounterValue failed: 0x{:08X}",
                status
            ));
        }

        let dedicated_usage = value.Anonymous.largeValue as u64;
        let ratio = dedicated_usage as f64 / total_vram as f64;
        Ok(ratio.clamp(0.0, 1.0))
    }
}

fn stop_rust_directml_worker_for_game_mode(app: &AppHandle) {
    let semantic = app.state::<Arc<crate::semantic_runtime::SemanticRuntimeState>>();
    if semantic.inner().stop_if_directml() {
        tracing::info!("Game mode: stopped resident Rust DirectML semantic worker");
    }
}

/// 启动游戏模式监控循环（GPU 负载 + 全屏非浏览器检测）
pub fn start_game_mode_monitor(app: AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    // 停止已有的监控任务
    {
        let mut guard = monitor_state
            .game_mode_task
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    tracing::info!("Game mode: starting monitor (GPU polling 10s, fullscreen polling 3s)");

    let app_clone = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        // 始终监控 GPU 0（主显卡/游戏显卡）
        const MONITOR_DEVICE_ID: u32 = 0;
        // 频繁切换计数：记录最近的触发时间戳
        let mut trigger_timestamps: Vec<std::time::Instant> = Vec::new();

        // Fullscreen polling runs every 3s, GPU polling runs every 10s.
        // We use a 3s tick and run GPU check every ~3rd tick.
        let mut gpu_tick_counter: u32 = 0;
        const GPU_CHECK_INTERVAL_TICKS: u32 = 3; // 3 * 3s ≈ 9s (close to original 10s)

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            // ── Fullscreen non-browser detection ──
            {
                let capture_state = app_clone.state::<Arc<CaptureState>>();
                let was_paused = capture_state
                    .game_mode_capture_paused
                    .load(Ordering::SeqCst);

                let should_pause = match crate::capture::check_foreground_fullscreen() {
                    Some((process_name, _window_class, true)) if !process_name.is_empty() => {
                        // Fullscreen detected with known process — pause only if it's NOT a
                        // browser (hardcoded list or a live extension NMH session, which
                        // covers Chromium forks the list doesn't know about)
                        !(crate::capture::is_browser_process(&process_name)
                            || crate::reverse_ipc::has_nmh_session_for_exe(&process_name))
                    }
                    Some((process_name, window_class, true)) if process_name.is_empty() => {
                        // Fullscreen but process name unavailable (likely elevated/protected).
                        // Pause unless it's a known system window (desktop, taskbar, etc.)
                        if crate::capture::is_system_window_class(&window_class) {
                            false
                        } else {
                            tracing::info!(
                                "Game mode: fullscreen window with inaccessible process (class: '{}'), treating as game",
                                window_class
                            );
                            true
                        }
                    }
                    _ => false,
                };

                if should_pause != was_paused {
                    capture_state
                        .game_mode_capture_paused
                        .store(should_pause, Ordering::SeqCst);
                    if should_pause {
                        tracing::info!(
                            "Game mode: non-browser fullscreen app detected, pausing capture"
                        );
                    } else {
                        tracing::info!("Game mode: fullscreen app exited, resuming capture");
                    }
                    let _ = app_clone.emit("game-mode-status", serde_json::json!({
                        "active": app_clone.state::<MonitorState>().game_mode_dml_suppressed.load(Ordering::SeqCst),
                        "permanent": app_clone.state::<MonitorState>().game_mode_permanently_suppressed.load(Ordering::SeqCst),
                        "fullscreen_paused": should_pause,
                    }));
                }
            }

            // ── GPU memory polling (every ~9s) ──
            gpu_tick_counter += 1;
            if gpu_tick_counter < GPU_CHECK_INTERVAL_TICKS {
                continue;
            }
            gpu_tick_counter = 0;

            // 检查 DML 是否仍然启用
            if !crate::registry_config::get_bool("use_dml").unwrap_or(false) {
                continue;
            }

            let state = app_clone.state::<MonitorState>();

            // 如果已经被永久关闭，不再轮询
            if state
                .game_mode_permanently_suppressed
                .load(Ordering::SeqCst)
            {
                continue;
            }

            let usage = match query_gpu_memory_usage(MONITOR_DEVICE_ID) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("Game mode: failed to query GPU 0 memory: {}", e);
                    continue;
                }
            };

            let currently_suppressed = state.game_mode_dml_suppressed.load(Ordering::SeqCst);
            tracing::debug!(
                "Game mode: GPU 0 memory usage {:.1}%, DML suppressed: {}",
                usage * 100.0,
                currently_suppressed
            );

            if !currently_suppressed && usage >= 0.50 {
                // 记录触发时间，检查频率限制
                let now = std::time::Instant::now();
                trigger_timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);
                trigger_timestamps.push(now);

                if trigger_timestamps.len() >= 3 {
                    // 1 分钟内触发 3 次以上，永久关闭 DML 直到程序重启
                    tracing::warn!(
                        "Game mode: triggered {} times in 60s, permanently disabling DML until app restart",
                        trigger_timestamps.len()
                    );
                    state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
                    state
                        .game_mode_permanently_suppressed
                        .store(true, Ordering::SeqCst);
                    stop_rust_directml_worker_for_game_mode(&app_clone);
                    let _ = app_clone.emit(
                        "game-mode-status",
                        serde_json::json!({
                            "active": true,
                            "usage": usage,
                            "permanent": true,
                        }),
                    );
                    continue;
                }

                tracing::info!(
                    "Game mode: GPU 0 memory usage {:.1}% >= 50%, suppressing DML",
                    usage * 100.0
                );
                state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
                stop_rust_directml_worker_for_game_mode(&app_clone);
                let _ = app_clone.emit(
                    "game-mode-status",
                    serde_json::json!({"active": true, "usage": usage}),
                );
            } else if currently_suppressed && usage <= 0.40 {
                // 记录触发时间（恢复也计入）
                let now = std::time::Instant::now();
                trigger_timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);
                trigger_timestamps.push(now);

                if trigger_timestamps.len() >= 3 {
                    tracing::warn!(
                        "Game mode: triggered {} times in 60s, permanently disabling DML until app restart",
                        trigger_timestamps.len()
                    );
                    state
                        .game_mode_permanently_suppressed
                        .store(true, Ordering::SeqCst);
                    let _ = app_clone.emit(
                        "game-mode-status",
                        serde_json::json!({
                            "active": true,
                            "usage": usage,
                            "permanent": true,
                        }),
                    );
                    // DML 已经被抑制，不需要再重启
                    continue;
                }

                tracing::info!(
                    "Game mode: GPU 0 memory usage {:.1}% <= 40%, restoring DML",
                    usage * 100.0
                );
                state
                    .game_mode_dml_suppressed
                    .store(false, Ordering::SeqCst);
                let _ = app_clone.emit(
                    "game-mode-status",
                    serde_json::json!({"active": false, "usage": usage}),
                );
            }
        }
    });

    let mut guard = monitor_state
        .game_mode_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);
}

/// 停止游戏模式监控
pub fn stop_game_mode_monitor(app: &AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    let mut guard = monitor_state
        .game_mode_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.take() {
        handle.abort();
    }

    // 重置所有游戏模式状态
    monitor_state
        .game_mode_permanently_suppressed
        .store(false, Ordering::SeqCst);
    let was_suppressed = monitor_state
        .game_mode_dml_suppressed
        .swap(false, Ordering::SeqCst);

    // 重置全屏暂停状态
    let capture_state = app.state::<Arc<CaptureState>>();
    let was_fullscreen_paused = capture_state
        .game_mode_capture_paused
        .swap(false, Ordering::SeqCst);

    if was_suppressed || was_fullscreen_paused {
        let _ = app.emit(
            "game-mode-status",
            serde_json::json!({
                "active": false,
                "usage": 0.0,
                "fullscreen_paused": false,
            }),
        );
    }
    tracing::info!("Game mode: monitor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directml_policy_requires_both_preference_and_clear_game_mode() {
        let state = MonitorState::new();
        assert!(!state.allows_directml(false));
        assert!(state.allows_directml(true));

        state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
        assert!(state.is_dml_suppressed());
        assert!(!state.allows_directml(true));

        state
            .game_mode_dml_suppressed
            .store(false, Ordering::SeqCst);
        state
            .game_mode_permanently_suppressed
            .store(true, Ordering::SeqCst);
        assert!(state.is_dml_suppressed());
        assert!(!state.allows_directml(true));
    }

    #[test]
    fn a_stop_keeps_the_session_until_its_drain_ends() {
        let state = MonitorState::new();
        let session = state
            .claim_session()
            .expect("an idle monitor can be claimed");
        assert!(state.is_running());
        assert_eq!(state.claim_session(), None);

        assert!(state.take_over_for_stop());
        assert!(state.is_running());
        assert_eq!(state.claim_session(), None, "starts wait for the drain");
        assert!(
            !state.take_over_exited_session(session),
            "a loop that panics during the stop leaves the session to the stop"
        );

        state.finish_drain();
        assert!(!state.is_running());
        assert!(state.claim_session().is_some());
    }

    #[test]
    fn a_stop_without_a_session_leaves_the_lifecycle_alone() {
        let state = MonitorState::new();
        assert!(!state.take_over_for_stop());
        assert!(!state.is_running());
        assert!(state.claim_session().is_some());
    }

    #[test]
    fn an_exited_loop_ends_only_its_own_session() {
        let state = MonitorState::new();
        let first = state.claim_session().unwrap();
        assert!(state.take_over_exited_session(first));
        assert!(
            state.is_running(),
            "the exited session drains before it reads as stopped"
        );
        assert_eq!(state.claim_session(), None);
        state.finish_drain();
        assert!(!state.is_running());

        let second = state.claim_session().unwrap();
        assert_ne!(first, second);
        assert!(
            !state.take_over_exited_session(first),
            "a late exit of an older session must not end the newer one"
        );
        assert!(state.is_running());
        assert!(state.take_over_exited_session(second));
    }

    #[test]
    fn a_drain_that_finishes_late_leaves_a_newer_session_running() {
        let state = MonitorState::new();
        state.claim_session().unwrap();
        // Two stops overlap on the same session.
        assert!(state.take_over_for_stop());
        assert!(state.take_over_for_stop());
        state.finish_drain();
        let restarted = state.claim_session().unwrap();

        state.finish_drain();
        assert!(state.is_running());
        assert!(state.take_over_exited_session(restarted));
    }
}
