//! Credential and authenticated-session commands.
//!
//! These commands form the frontend boundary for Windows Hello-backed key setup and
//! session lifetime management. Initialization and verification intentionally remain
//! callable before a session exists; changing session policy requires authentication.

use crate::credential_manager::{self, CredentialManagerState};
use crate::mcp_server;
use crate::storage::StorageState;
use std::sync::Arc;

/// Initializes the CNG key pair, cached public key, master key, and encrypted storage.
///
/// Authentication: not required because this command bootstraps authentication.
/// Returns a success message string; rejects non-Windows platforms.
/// Frontend: `components/AuthMask.jsx` and `lib/monitor_api.js`.
#[tauri::command]
pub async fn credential_initialize(
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

        Ok("Credentials initialized successfully".to_string())
    }

    #[cfg(not(windows))]
    {
        let _ = &credential_state;
        let _ = &storage_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// Shows the OS credential UI, unlocks the master key, and restores protected services.
///
/// Authentication: this command performs authentication and therefore needs no session.
/// Returns `true` after verification succeeds; errors include OS verification failures.
/// Frontend: `components/AuthMask.jsx`.
#[tauri::command]
pub async fn credential_verify_user(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let owner_hwnd = window
            .hwnd()
            .map_err(|e| format!("Failed to get main window handle: {}", e))?;
        credential_manager::force_verify_and_unlock_master_key(&state, Some(owner_hwnd.0 as isize))
            .map_err(|e| format!("Verification failed: {}", e))?;

        state.update_auth_time();
        storage_state.try_dedup_migration();
        storage_state.try_bitmap_index_migration();
        if let Err(e) =
            mcp_server::restore_if_enabled(app, &state, &storage_state, &mcp_state).await
        {
            tracing::warn!("Failed to restore MCP after authentication: {}", e);
        }

        Ok(true)
    }

    #[cfg(not(windows))]
    {
        let _ = &app;
        let _ = &window;
        let _ = &state;
        let _ = &storage_state;
        let _ = &mcp_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// Reports whether the current in-memory authenticated session is still valid.
///
/// Authentication: not required. Returns a JSON boolean.
/// Frontend: `hooks/useAuthSession.js`.
#[tauri::command]
pub async fn credential_check_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    Ok(state.is_session_valid())
}

/// Invalidates the current authenticated session immediately.
///
/// Authentication: not required. Returns JSON `null` on success.
#[tauri::command]
pub async fn credential_lock_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    state.invalidate_session();
    Ok(())
}

/// Records whether the main UI is foregrounded for session-expiry policy.
///
/// Authentication: not required. `in_foreground` is the current UI visibility state;
/// returns JSON `null` on success.
#[tauri::command]
pub async fn credential_set_foreground(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    in_foreground: bool,
) -> Result<(), String> {
    state.set_foreground_state(in_foreground);
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

    state.set_session_timeout(timeout);
    if let Err(e) = crate::registry_config::set_string("session_timeout_secs", &timeout.to_string())
    {
        tracing::error!("Failed to persist session_timeout_secs: {}", e);
    }
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
