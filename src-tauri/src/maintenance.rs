//! Global application maintenance mode.
//!
//! While a maintenance task (currently the explicit MiniLM migration) is
//! running, background mutation paths must stand still so the migration's
//! Chroma snapshot and the Rust-derived cache cannot drift apart. The flag is
//! process-global because it gates surfaces that do not carry a Tauri
//! `AppHandle` (reverse IPC, MCP dispatch, background loops).
//!
//! Allowed while active: status queries, Windows Hello authentication, error
//! listing, and app exit. The migration itself has no user cancellation path.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const MAINTENANCE_IN_PROGRESS: &str = "MAINTENANCE_IN_PROGRESS";

static ACTIVE: AtomicBool = AtomicBool::new(false);
static REASON: Mutex<Option<String>> = Mutex::new(None);

/// RAII guard: maintenance mode ends when the guard drops, so a panicking or
/// early-returning worker cannot leave the app locked.
pub struct MaintenanceGuard {
    _private: (),
}

impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::SeqCst);
        *REASON.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

/// Enter maintenance mode. Returns `None` if another maintenance task is
/// already active.
pub fn enter(reason: &str) -> Option<MaintenanceGuard> {
    if ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return None;
    }
    *REASON.lock().unwrap_or_else(|error| error.into_inner()) = Some(reason.to_string());
    Some(MaintenanceGuard { _private: () })
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Standard rejection for gated entry points.
pub fn guard() -> Result<(), String> {
    if is_active() {
        Err(MAINTENANCE_IN_PROGRESS.to_string())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MaintenanceStatus {
    pub active: bool,
    pub reason: Option<String>,
}

#[tauri::command]
pub fn get_maintenance_status() -> MaintenanceStatus {
    MaintenanceStatus {
        active: is_active(),
        reason: REASON
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
    }
}

/// Reverse-IPC commands that remain usable during maintenance: session/crypto
/// helpers the migration itself depends on, read-only status, and NMH session
/// bookkeeping. Everything that writes screenshots, OCR, or clustering state is
/// rejected. The MiniLM mirror commands used to be allowed here because Python
/// wrote vectors Rust had to accept even mid-migration; M2.5 step 5 removed
/// them along with the Python-side writer.
pub fn reverse_ipc_command_allowed(command: &str) -> bool {
    matches!(
        command,
        "get_public_key"
            | "get_auth_status"
            | "get_idle_state"
            | "encrypt_for_chromadb"
            | "decrypt_from_chromadb"
            | "decrypt_many_from_chromadb"
            | "decrypt_from_chromadb_silent"
            | "decrypt_many_from_chromadb_silent"
            | "register_nmh"
            | "unregister_nmh"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_exclusive_and_releases_on_drop() {
        let guard = enter("test").expect("first enter succeeds");
        assert!(is_active());
        assert!(enter("second").is_none());
        assert_eq!(super::guard().unwrap_err(), MAINTENANCE_IN_PROGRESS);
        drop(guard);
        assert!(!is_active());
        assert!(super::guard().is_ok());
    }

    #[test]
    fn reverse_ipc_allowlist_rejects_mutations() {
        assert!(reverse_ipc_command_allowed("get_auth_status"));
        assert!(!reverse_ipc_command_allowed("save_screenshot"));
        assert!(!reverse_ipc_command_allowed("save_extension_screenshot"));
        assert!(!reverse_ipc_command_allowed("commit_screenshot"));
        assert!(!reverse_ipc_command_allowed(
            "smart_cluster_record_assignment"
        ));
        // Retired with the Python-side mirror. Still named here so that
        // re-adding either one is a deliberate edit rather than a revival.
        assert!(!reverse_ipc_command_allowed(
            "upsert_minilm_derived_embeddings"
        ));
        assert!(!reverse_ipc_command_allowed(
            "delete_minilm_derived_embeddings"
        ));
    }
}
