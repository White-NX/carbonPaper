//! Authenticated database journal-mode diagnostics and maintenance commands.

use crate::capture::CaptureState;
use crate::credential_manager::CredentialManagerState;
use crate::monitor::{start_monitor_impl, stop_monitor_impl, MonitorState};
use crate::storage::{DatabaseModeEligibility, DatabaseModeMetadata, StorageState};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{Manager, State};

/// Returns the persisted journal-mode state for diagnostics and maintenance
/// tooling. Authentication: required.
#[tauri::command]
pub async fn storage_get_database_mode_metadata(
    credential_state: State<'_, Arc<CredentialManagerState>>,
    state: State<'_, Arc<StorageState>>,
) -> Result<DatabaseModeMetadata, String> {
    super::check_auth_required(&credential_state)?;
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.database_mode_metadata())
        .await
        .map_err(|error| format!("Database mode metadata task failed: {error}"))?
}

/// Checks the local filesystem, write probe, and free-space requirements for a
/// controlled WAL-to-DELETE transition. Authentication: required.
#[tauri::command]
pub async fn storage_check_database_mode_eligibility(
    credential_state: State<'_, Arc<CredentialManagerState>>,
    state: State<'_, Arc<StorageState>>,
) -> Result<DatabaseModeEligibility, String> {
    super::check_auth_required(&credential_state)?;
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.check_database_mode_eligibility())
        .await
        .map_err(|error| format!("Database mode eligibility task failed: {error}"))?
}

/// Drains database activity and performs the controlled WAL-to-DELETE
/// transition. Authentication: required.
#[tauri::command]
pub async fn storage_transition_wal_to_delete(
    app_handle: tauri::AppHandle,
    credential_state: State<'_, Arc<CredentialManagerState>>,
    state: State<'_, Arc<StorageState>>,
    monitor_state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
) -> Result<DatabaseModeMetadata, String> {
    super::check_auth_required(&credential_state)?;

    let Some(_maintenance) = crate::maintenance::enter("database_mode_wal_to_delete") else {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };

    let was_running = monitor_state
        .process
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_some();
    monitor_state.migration_lock.store(true, Ordering::SeqCst);
    let office_runtime = app_handle
        .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .inner()
        .clone();
    let was_active = office_runtime.is_active();

    if let Err(error) = stop_monitor_impl(
        monitor_state.clone(),
        capture_state.clone(),
        app_handle.clone(),
    )
    .await
    {
        monitor_state.migration_lock.store(false, Ordering::SeqCst);
        if was_active {
            office_runtime.resume();
        }
        return Err(format!(
            "Failed to stop monitor before database mode transition: {error}"
        ));
    }
    office_runtime
        .quiesce(std::time::Duration::from_secs(5))
        .await;

    let state = state.inner().clone();
    let state_after_transition = Arc::clone(&state);
    let transition = tokio::task::spawn_blocking(move || state.transition_wal_to_delete())
        .await
        .map_err(|error| format!("Database mode transition task failed: {error}"));
    let storage_ready = state_after_transition.is_initialized();

    monitor_state.migration_lock.store(false, Ordering::SeqCst);
    if was_running && storage_ready {
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(error) = start_monitor_impl(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!(
                "Database mode transition: failed to restart monitor after transition: {error}"
            );
        }
    } else if was_active && storage_ready {
        office_runtime.resume();
    }

    transition?
}
