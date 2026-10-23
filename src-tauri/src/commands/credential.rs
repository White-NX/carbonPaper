//! Credential and authenticated-session commands.
//!
//! These commands form the frontend boundary for Windows Hello-backed key setup and
//! session lifetime management. Initialization and verification intentionally remain
//! callable before a session exists; changing session policy requires authentication.

use crate::credential_manager::{self, CredentialManagerState};
use crate::mcp_server;
use crate::storage::StorageState;
use std::sync::Arc;
use tauri::Manager;

/// Initializes the CNG key pair, cached public key, master key, and encrypted storage.
///
/// Authentication: not required because this command bootstraps authentication.
/// Returns a success message string; rejects non-Windows platforms.
/// Frontend: `lib/auth_api.js` (explicit unlock) and `lib/monitor_api.js`.
#[tauri::command]
pub async fn credential_initialize(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        match credential_manager::load_public_key_from_file(&credential_state) {
            Ok(_) => {}
            Err(_) => {
                let pk = credential_manager::export_or_get_public_key(&credential_state)
                    .map_err(|e| format!("Failed to initialize credentials: {}", e))?;

                credential_manager::save_public_key_to_file(&credential_state, &pk)
                    .map_err(|e| format!("Failed to save public key: {}", e))?;
            }
        };

        credential_manager::ensure_master_key_created(&credential_state)
            .map_err(|e| format!("Failed to create master key: {}", e))?;

        storage_state.initialize()?;
        // First-run initialization can happen after the Tauri setup callback
        // failed to load/export the public key. Start Office observation at the
        // same post-storage boundary used by the normal startup path.
        if let Some(office_runtime) =
            app.try_state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        {
            office_runtime.start(app.clone(), storage_state.inner().clone());
        }
        if let Some(scheduler) =
            app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        {
            scheduler.start(app.clone());
        }

        Ok("Credentials initialized successfully".to_string())
    }

    #[cfg(not(windows))]
    {
        let _ = &app;
        let _ = &credential_state;
        let _ = &storage_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// Shows the OS credential UI, unlocks the master key, and restores protected services.
///
/// Authentication: this command performs authentication and therefore needs no session.
/// Returns `true` after verification succeeds; errors include OS verification failures.
/// Concurrent windows share the first caller's owner HWND and result. A later
/// request performs fresh verification even if the session is still valid.
/// Frontend: `lib/auth_api.js`.
#[tauri::command]
pub async fn credential_verify_user(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let owner_hwnd = window
            .hwnd()
            .map_err(|e| format!("Failed to get calling window handle: {}", e))?
            .0 as isize;
        let credentials = state.inner().clone();
        let storage = storage_state.inner().clone();
        state
            .verification
            .run(async move {
                let verification_state = credentials.clone();
                tokio::task::spawn_blocking(move || {
                    credential_manager::force_verify_and_unlock_master_key(
                        &verification_state,
                        Some(owner_hwnd),
                    )
                    .map_err(|e| format!("Verification failed: {}", e))
                })
                .await
                .map_err(|e| format!("Verification task failed: {}", e))??;

                // Run completion work once, even if the original window closes.
                crate::settings_window::broadcast_auth_state(&app);
                if let Some(scheduler) =
                    app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
                {
                    scheduler.wake();
                }
                storage.try_bitmap_index_migration();
                let recap_app = app.clone();
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    crate::ai::recap::protected::sync_after_unlock(&recap_app)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
                {
                    tracing::warn!("[RECAP] Protected configuration sync deferred: {}", error);
                }
                let mcp_state = app.state::<mcp_server::McpRuntimeState>();
                if let Err(e) =
                    mcp_server::restore_if_enabled(app.clone(), &credentials, &storage, &mcp_state)
                        .await
                {
                    tracing::warn!("Failed to restore MCP after authentication: {}", e);
                }
                Ok(())
            })
            .await?;

        Ok(true)
    }

    #[cfg(not(windows))]
    {
        let _ = &app;
        let _ = &window;
        let _ = &state;
        let _ = &storage_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// Reports whether the current in-memory authenticated session is still valid.
///
/// Authentication: not required. Returns a JSON boolean.
/// Frontend: `hooks/useAuthSession.js`.
#[tauri::command]
pub async fn credential_check_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    crate::settings_window::broadcast_auth_state(&app);
    Ok(state.is_session_valid())
}

/// Invalidates the current authenticated session immediately.
///
/// Authentication: not required. Returns JSON `null` on success.
#[tauri::command]
pub async fn credential_lock_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    state.invalidate_session();
    crate::settings_window::broadcast_auth_state(&app);
    Ok(())
}

/// Records whether the main UI is foregrounded for session-expiry policy.
///
/// Authentication: not required. `in_foreground` is the current UI visibility state;
/// returns JSON `null` on success.
#[tauri::command]
pub async fn credential_set_foreground(
    app: tauri::AppHandle,
    window: tauri::Window,
    in_foreground: bool,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    let _ = in_foreground;
    crate::settings_window::refresh_ui_foreground(&app);
    Ok(())
}

/// Changes and persists the authenticated-session timeout in seconds.
///
/// Authentication: required. `timeout` is an integer number of seconds; returns JSON
/// `null`. Frontend: `hooks/useAuthSession.js`.
#[tauri::command]
pub async fn credential_set_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    timeout: i64,
) -> Result<(), String> {
    crate::commands::check_auth_required(&state)?;

    crate::registry_config::set_string("session_timeout_secs", &timeout.to_string())?;
    state.set_session_timeout(timeout);
    Ok(())
}

/// Returns the configured authenticated-session timeout in seconds.
///
/// Authentication: not required so the login UI can display policy. Returns a JSON
/// integer. Frontend: `hooks/useAuthSession.js`.
#[tauri::command]
pub async fn credential_get_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<i64, String> {
    Ok(state.get_session_timeout())
}

/// Returns whether unattended organization is enabled. The process lease is
/// deliberately not exposed here; scheduler status reports whether work is
/// currently waiting for an unlock.
#[tauri::command]
pub async fn credential_get_background_processing_enabled(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    Ok(state.background_processing_enabled())
}

/// Enables or disables unattended organization after the UI is locked. The
/// setting is persistent; disabling it revokes the current process lease.
#[tauri::command]
pub async fn credential_set_background_processing_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    enabled: bool,
) -> Result<(), String> {
    crate::commands::check_auth_required(&state)?;
    state.set_background_processing_enabled(enabled)?;
    if !enabled {
        let storage = app
            .state::<Arc<crate::storage::StorageState>>()
            .inner()
            .clone();
        tokio::task::spawn_blocking(move || storage.processing_stage.disable_if_installed())
            .await
            .map_err(|error| error.to_string())??;
    }
    if let Some(scheduler) =
        app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
    {
        scheduler.wake();
    }
    Ok(())
}
