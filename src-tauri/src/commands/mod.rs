//! Tauri command boundary shared by the React frontend and native backend.
//!
//! Command functions document their authentication requirements, serialized return
//! contract, and the frontend module that invokes them. Keep privileged operations
//! behind [`check_auth_required`] and restrict window-management commands with
//! [`check_main_window`].

use crate::credential_manager::CredentialManagerState;

/// Checks whether the current session requires re-authentication.
pub fn check_auth_required(credential_state: &CredentialManagerState) -> Result<(), String> {
    if !credential_state.is_session_valid() {
        return Err("AUTH_REQUIRED".to_string());
    }
    Ok(())
}

/// Rejects calls that did not originate from CarbonPaper's main Tauri window.
pub fn check_main_window(window: &tauri::Window) -> Result<(), String> {
    if window.label() != "main" {
        return Err("WINDOW_NOT_AUTHORIZED".to_string());
    }
    Ok(())
}

pub mod credential;
pub mod mcp;
pub mod migration;
pub mod smart_cluster;
pub mod storage;
pub mod utility;
