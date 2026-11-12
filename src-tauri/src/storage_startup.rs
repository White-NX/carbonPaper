//! One application-owned storage startup job, shared by setup, unlock and retry.
//! Progress reads never acquire the database or wait for compaction.
use crate::credential_manager::{self, CredentialManagerState};
use crate::storage::{InitializationPhase, InitializationProgress, StorageState};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::watch;

const PROGRESS_EVENT: &str = "storage-initialization-progress";
pub(crate) const STORAGE_INITIALIZING: &str = "STORAGE_INITIALIZING";

#[derive(Clone, Serialize)]
pub struct InitializationStatus {
    pub running: bool,
    pub ready: bool,
    #[serde(flatten)]
    progress: InitializationProgress,
    error: Option<String>,
    revision: u64,
}

pub(crate) struct StorageStartupState {
    progress: watch::Sender<InitializationStatus>,
    services_started: AtomicBool,
}

impl StorageStartupState {
    pub fn new() -> Self {
        let (progress, _) = watch::channel(InitializationStatus {
            running: false,
            ready: false,
            progress: InitializationProgress::phase(InitializationPhase::Credentials),
            error: None,
            revision: 0,
        });
        Self {
            progress,
            services_started: AtomicBool::new(false),
        }
    }

    fn snapshot(&self) -> InitializationStatus {
        self.progress.borrow().clone()
    }

    fn claim(&self, storage_ready: bool) -> bool {
        self.progress.send_if_modified(|status| {
            if status.running || (status.ready && storage_ready) {
                return false;
            }
            status.running = true;
            status.ready = false;
            status.error = None;
            status.progress = InitializationProgress::phase(InitializationPhase::Credentials);
            status.revision += 1;
            true
        })
    }

    fn update(&self, app: &tauri::AppHandle, progress: InitializationProgress) {
        self.progress.send_modify(|status| {
            status.progress = progress;
            status.revision += 1;
        });
        let _ = app.emit(PROGRESS_EVENT, self.snapshot());
    }

    fn finish(&self, result: Result<(), String>) {
        self.progress.send_modify(|status| {
            status.running = false;
            status.ready = result.is_ok();
            status.error = result.err();
            status.revision += 1;
        });
    }
}

pub(crate) fn spawn(app: tauri::AppHandle) {
    let state = app.state::<Arc<StorageStartupState>>().inner().clone();
    if !state.claim(app.state::<Arc<StorageState>>().is_initialized()) {
        return;
    }
    let _ = app.emit(PROGRESS_EVENT, state.snapshot());
    tauri::async_runtime::spawn(async move {
        let worker_app = app.clone();
        let worker_state = state.clone();
        let result = tauri::async_runtime::spawn_blocking(move || run(&worker_app, &worker_state))
            .await
            .unwrap_or_else(|error| Err(format!("Storage initialization task failed: {error}")));
        if let Err(error) = &result {
            tracing::error!("Background storage startup failed; capture remains stopped: {error}");
        }
        state.finish(result);
        let _ = app.emit(PROGRESS_EVENT, state.snapshot());
        crate::monitor::wake_automation(&app);
        if state.snapshot().ready {
            crate::commands::migration::spawn_auto_hmac_migration(app);
        }
    });
}

pub(crate) fn is_ready(app: &tauri::AppHandle) -> bool {
    app.state::<Arc<StorageStartupState>>().snapshot().ready
        && app.state::<Arc<StorageState>>().is_initialized()
}

pub(crate) async fn ensure_initialized(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<Arc<StorageStartupState>>().inner().clone();
    let mut progress = state.progress.subscribe();
    spawn(app);
    loop {
        let status = progress.borrow().clone();
        if status.ready {
            return Ok(());
        }
        if let Some(error) = status.error {
            return Err(error);
        }
        progress
            .changed()
            .await
            .map_err(|error| error.to_string())?;
    }
}

fn run(app: &tauri::AppHandle, state: &StorageStartupState) -> Result<(), String> {
    let started = std::time::Instant::now();
    let credentials = app.state::<Arc<CredentialManagerState>>();
    credential_manager::initialize_public_key(&credentials)
        .map_err(|error| format!("Credential validation failed: {error}"))?;
    #[cfg(windows)]
    credential_manager::ensure_master_key_created(&credentials)
        .map_err(|error| format!("Credential recovery is required: {error}"))?;
    #[cfg(not(windows))]
    return Err("Windows Hello is only available on Windows".into());

    tracing::info!(
        "[DB:STARTUP] Credentials validated in {:?}",
        started.elapsed()
    );
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    storage.initialize_with_progress(|progress| state.update(app, progress))?;
    // Complete unattended database preparation before publishing readiness or
    // admitting any producer, even when there is no foreground window.
    if storage.check_startup_vacuum_needed()? {
        state.update(
            app,
            InitializationProgress::phase(InitializationPhase::Compacting),
        );
        storage.run_startup_vacuum_if_needed()?;
    }
    state.update(
        app,
        InitializationProgress::phase(InitializationPhase::Finalizing),
    );
    if crate::IS_QUITTING.load(std::sync::atomic::Ordering::Relaxed)
        || crate::IS_UPDATING.load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err("Application is stopping".into());
    }
    // A later reopen (for example after failed backup recovery) still reports
    // startup progress, but must not duplicate loops or override a manual stop.
    if !state.services_started.swap(true, Ordering::AcqRel) {
        start_storage_services(app, &storage);
    }
    tracing::info!(
        "[DB:STARTUP] Background storage startup completed in {:?}",
        started.elapsed()
    );
    Ok(())
}

// All database-dependent startup work is admitted after migration and key
// publication. This runs once even when a window closes or unlock joins startup.
fn start_storage_services(app: &tauri::AppHandle, storage: &Arc<StorageState>) {
    storage.start_storage_compaction(app.clone());
    crate::ai::recap::start_scheduler(app.clone());
    let office = app.state::<Arc<crate::office_runtime::OfficeRuntimeState>>();
    office.start(app.clone(), storage.clone());
    match storage.recover_incomplete_ocr_postprocess() {
        Ok(recovered) if recovered > 0 => tracing::info!(
            "[ML:POSTPROCESS] recovered {} incomplete rows from the previous application process",
            recovered
        ),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!("[ML:POSTPROCESS] failed to recover incomplete startup rows: {error}")
        }
    }
    crate::blind_index_repair::spawn_blind_index_auto_repair(app.clone());
    crate::legacy_vector_discard::spawn_legacy_vector_discard(app.clone());
    crate::clip_ann::spawn_startup_arm(app.clone());
    let cleanup_app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::run_delete_queue_maintenance_loop(cleanup_app).await;
    });
    let postprocess_app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::ml_runtime::run_postprocess_retry_loop(postprocess_app).await;
    });
    let cache_storage = storage.clone();
    let cache_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            ticker.tick().await;
            cache_storage
                .evict_semantic_vector_cache_if_idle(crate::storage::SEMANTIC_CACHE_IDLE_TTL);
            if let Some(ann) = cache_app.try_state::<Arc<crate::clip_ann::ClipAnnState>>() {
                ann.evict_reader_if_idle(crate::storage::SEMANTIC_CACHE_IDLE_TTL);
            }
        }
    });
    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let nmh_storage = storage.clone();
    let nmh_capture = app
        .state::<Arc<crate::capture::CaptureState>>()
        .inner()
        .clone();
    let nmh_app = app.clone();
    std::thread::spawn(
        move || match crate::reverse_ipc::generate_nmh_auth_token(&data_dir) {
            Ok(token) => {
                let mut server = crate::reverse_ipc::NmhPipeServer::new();
                if let Err(error) = server.start(nmh_storage, nmh_capture, nmh_app, token) {
                    tracing::error!("Failed to start NMH pipe server: {error}");
                }
                std::mem::forget(server);
            }
            Err(error) => tracing::error!("Failed to generate NMH auth token: {error}"),
        },
    );
    let filters = app.state::<Arc<crate::sensitive_filter::SensitiveFilterState>>();
    filters.load_dicts(app);
    if let Ok(policy) = storage.load_policy() {
        if let Some(config) = policy.get("sensitive_filter") {
            if let Ok(config) = serde_json::from_value::<
                crate::sensitive_filter::SensitiveFilterConfig,
            >(config.clone())
            {
                filters.update_config(config.upgraded());
            }
        }
    }
    app.state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        .start(app.clone());
    if crate::registry_config::get_bool("game_mode_enabled").unwrap_or(false) {
        crate::monitor::start_game_mode_monitor(app.clone());
    }
    let mcp_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let storage = mcp_app.state::<Arc<StorageState>>();
        let credentials = mcp_app.state::<Arc<CredentialManagerState>>();
        let runtime = mcp_app.state::<crate::mcp_server::McpRuntimeState>();
        if let Ok(policy) = storage.load_policy() {
            if policy
                .get("mcp_enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                if let Err(error) =
                    crate::mcp_server::auto_start(mcp_app.clone(), &credentials, &storage, &runtime)
                        .await
                {
                    tracing::error!("MCP auto-start failed: {error}");
                }
            }
        }
    });
}

/// Read-only bootstrap progress; available before authentication and after a
/// hidden-start window is recreated. It contains no archive content.
#[tauri::command]
pub fn storage_get_initialization_status(app: tauri::AppHandle) -> InitializationStatus {
    app.state::<Arc<StorageStartupState>>().snapshot()
}

/// Retry the same restartable startup migration, without admitting capture or
/// opening an authentication prompt. Only the main UI may request a retry.
#[tauri::command]
pub fn storage_retry_initialization(
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<InitializationStatus, String> {
    crate::commands::check_main_window(&window)?;
    spawn(app.clone());
    Ok(app.state::<Arc<StorageStartupState>>().snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_startup_claims_are_deduplicated_and_errors_allow_retry() {
        let state = Arc::new(StorageStartupState::new());
        let workers = (0..8)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || state.claim(false))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|claimed| *claimed)
                .count(),
            1
        );
        assert!(state.snapshot().running);
        assert!(!state.claim(false));
        let before = state.snapshot().revision;
        state.finish(Err("compaction failed".into()));
        assert!(!state.snapshot().ready);
        assert!(state.claim(false));
        assert!(state.snapshot().revision > before);
        assert!(state.snapshot().error.is_none());
        state.finish(Ok(()));
        assert!(state.snapshot().ready);
        assert!(!state.claim(true));
        assert!(
            state.claim(false),
            "a closed database still permits recovery"
        );
        assert!(!state.snapshot().ready);
    }
}
