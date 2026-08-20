//! Tauri commands for local MCP server configuration and credential delivery.
//!
//! The server binds only to loopback and authenticates clients with a bearer token.
//! Status and privacy acknowledgement are readable before authentication; operations
//! that expose or change credentials and policy require a valid user session.

use crate::credential_manager::CredentialManagerState;
use crate::mcp_contract;
use crate::mcp_server;
use crate::mcp_smoke::{self, McpSmokeReport};
use crate::mcp_token;
use crate::sensitive_filter::{self, SensitiveFilterState};
use crate::storage::StorageState;
use std::sync::Arc;
use tauri::Emitter;

#[cfg(windows)]
struct GlobalMemGuard {
    handle: windows::Win32::Foundation::HGLOBAL,
    transferred: bool,
}

#[cfg(windows)]
impl Drop for GlobalMemGuard {
    fn drop(&mut self) {
        if !self.transferred {
            // SAFETY: `handle` came from `GlobalAlloc`, remains owned by this guard, and
            // is freed only when ownership was not transferred to the clipboard.
            unsafe {
                let _ = windows::Win32::Foundation::GlobalFree(self.handle);
            }
        }
    }
}

#[cfg(windows)]
fn copy_mcp_token_to_clipboard(window: &tauri::Window, token: &str) -> Result<(), String> {
    use std::mem::size_of;
    use std::ptr;
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    let mut wide: Vec<u16> = token.encode_utf16().collect();
    wide.push(0);
    let byte_len = wide.len() * size_of::<u16>();

    // SAFETY: all UTF-16 and byte buffers remain alive for the duration of the Win32
    // calls; the allocated HGLOBAL is locked before copying and its ownership is either
    // retained by `global_mem` or transferred exactly once to the clipboard.
    unsafe {
        let owner_hwnd = window
            .hwnd()
            .map_err(|e| format!("Failed to get window handle: {}", e))?;
        OpenClipboard(HWND(owner_hwnd.0 as _))
            .map_err(|e| format!("Failed to open clipboard: {:?}", e))?;
        let clipboard_open = ClipboardGuard;

        EmptyClipboard().map_err(|e| format!("Failed to empty clipboard: {:?}", e))?;
        let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len)
            .map_err(|e| format!("GlobalAlloc failed: {:?}", e))?;
        let mut global_mem = GlobalMemGuard {
            handle,
            transferred: false,
        };
        let locked = GlobalLock(global_mem.handle);
        if locked.is_null() {
            return Err("GlobalLock failed".to_string());
        }

        ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, locked as *mut u8, byte_len);
        match GlobalUnlock(global_mem.handle) {
            Ok(()) => {}
            // GlobalUnlock returns zero when the lock count reaches zero; in that
            // success case GetLastError remains ERROR_SUCCESS, which windows-rs
            // surfaces as an HRESULT(0) Error.
            Err(e) if e.code().0 == 0 => {}
            Err(e) => return Err(format!("GlobalUnlock failed: {:?}", e)),
        }
        SetClipboardData(13, HANDLE(global_mem.handle.0))
            .map_err(|e| format!("Failed to set clipboard data: {:?}", e))?;
        global_mem.transferred = true;

        std::mem::forget(clipboard_open);
        CloseClipboard().map_err(|e| format!("Failed to close clipboard: {:?}", e))?;
        Ok(())
    }
}

#[cfg(windows)]
struct ClipboardGuard;

#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after `OpenClipboard` succeeds and closes
        // that thread-owned clipboard exactly once on early-return paths.
        unsafe {
            let _ = windows::Win32::System::DataExchange::CloseClipboard();
        }
    }
}

#[cfg(not(windows))]
fn copy_mcp_token_to_clipboard(_window: &tauri::Window, _token: &str) -> Result<(), String> {
    Err("MCP token clipboard delivery is only available on Windows".to_string())
}

fn policy_as_object_mut(
    policy: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    policy
        .as_object_mut()
        .ok_or_else(|| "Policy is not a valid JSON object".to_string())
}

fn mcp_privacy_acknowledged_from_policy_or_db(
    storage_state: &StorageState,
    policy: &serde_json::Value,
) -> bool {
    let legacy_enabled = policy
        .get("mcp_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_existing_token = policy
        .get("mcp_token_encrypted")
        .and_then(|v| v.as_str())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    legacy_enabled
        || has_existing_token
        || storage_state.is_mcp_privacy_acknowledged().unwrap_or(false)
}

/// Enables or disables the loopback MCP server and persists the choice.
///
/// Authentication: required. `enabled` selects the desired state. Returns
/// `{ "status": "ok", "port"?: number }`. Frontend:
/// `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_set_enabled(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;
    let _lifecycle_guard = mcp_state.lock_lifecycle().await;

    if enabled {
        let mut policy = storage_state.load_policy()?;
        let was_enabled = policy
            .get("mcp_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let existing_token = policy.get("mcp_token_encrypted").and_then(|v| v.as_str());
        let (token_plaintext, is_new_token) = if let Some(encrypted_b64) = existing_token {
            let token = mcp_token::decrypt_token(&credential_state, encrypted_b64)?;
            if !mcp_token::is_current_format(encrypted_b64) {
                let encrypted_v2 = mcp_token::encrypt_token(&credential_state, &token)?;
                policy_as_object_mut(&mut policy)?.insert(
                    "mcp_token_encrypted".into(),
                    serde_json::json!(encrypted_v2),
                );
            }
            (token, false)
        } else {
            let token = mcp_token::generate_token();
            let encrypted_b64 = mcp_token::encrypt_token(&credential_state, &token)?;
            policy_as_object_mut(&mut policy)?.insert(
                "mcp_token_encrypted".into(),
                serde_json::json!(encrypted_b64),
            );
            (token, true)
        };

        let port = mcp_server::port_from_policy(&policy);

        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(true));
        if policy.get("mcp_port").is_none() {
            policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
        }
        storage_state.save_policy(&policy)?;

        let token_hash = mcp_token::hash_token(&token_plaintext);
        mcp_state.set_token_hash(token_hash);
        if let Err(e) = mcp_server::start_server(app.clone(), port, token_hash).await {
            mcp_state.set_last_error(e.clone());
            if !was_enabled {
                if let Ok(mut rollback_policy) = storage_state.load_policy() {
                    if let Ok(obj) = policy_as_object_mut(&mut rollback_policy) {
                        obj.insert("mcp_enabled".into(), serde_json::json!(false));
                        let _ = storage_state.save_policy(&rollback_policy);
                    }
                }
            }
            let _ = app.emit(
                "mcp-status-changed",
                serde_json::json!({ "state": "error", "error": e.clone() }),
            );
            return Err(e);
        }

        let _ = app.emit(
            "mcp-status-changed",
            serde_json::json!({
                "state": "running",
                "active_port": port,
                "runtime_generation": mcp_state.generation(),
            }),
        );

        let _ = is_new_token;
        Ok(serde_json::json!({ "status": "ok", "port": port }))
    } else {
        mcp_server::stop_server(&mcp_state).await;
        let mut policy = storage_state.load_policy()?;
        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(false));
        storage_state.save_policy(&policy)?;
        mcp_state.clear_last_error();

        let _ = app.emit(
            "mcp-status-changed",
            serde_json::json!({
                "state": "disabled",
                "runtime_generation": mcp_state.generation(),
            }),
        );

        Ok(serde_json::json!({ "status": "ok" }))
    }
}

/// Returns runtime, privacy, model, and search-capability status for MCP settings.
///
/// Authentication: not required; no token or encrypted secret is returned. The JSON
/// object contains configured and active ports, runtime generation, `enabled`,
/// `running`, `state`, `error`, `privacy_acknowledged`, `server_version`,
/// `skill`, and `capabilities`.
/// Frontend: `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_get_status(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    ml_state: tauri::State<'_, Arc<crate::ml_runtime::MlRuntimeState>>,
) -> Result<serde_json::Value, String> {
    let storage_for_status = storage_state.inner().clone();
    let app_for_status = app.clone();
    // Policy/model inspection and every database read stay off the async IPC
    // dispatcher. The tool list itself is static; this is diagnostic status.
    let status_reads = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let policy = storage_for_status.load_policy()?;
        let port = mcp_server::port_from_policy(&policy);
        let privacy_acknowledged =
            mcp_privacy_acknowledged_from_policy_or_db(&storage_for_status, &policy);
        let search_nl = clip_search_readiness(&storage_for_status);
        let failed_ocr = storage_for_status.count_failed_ocr().unwrap_or(0);
        let ocr_model_status = crate::ml_runtime::ocr_model_status(&app_for_status).ok();
        Ok((
            policy,
            port,
            privacy_acknowledged,
            search_nl,
            failed_ocr,
            ocr_model_status,
        ))
    })
    .await
    .map_err(|error| format!("Failed to read MCP status: {error}"))?;
    let (policy, port, privacy_acknowledged, search_nl, failed_ocr, ocr_model_status) =
        status_reads?;
    let enabled = policy
        .get("mcp_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let active_port = mcp_state.active_port();
    let running = active_port.is_some();
    let port_consistent = active_port.is_none() || active_port == Some(port);
    let runtime_generation = mcp_state.generation();
    let last_error = mcp_state.get_last_error();
    let status_error = if running && !port_consistent {
        Some(format!(
            "Configured MCP port {} does not match active listener on {}",
            port,
            active_port.expect("running MCP server must have an active port"),
        ))
    } else {
        last_error
    };
    let ml_status = ml_state.status(failed_ocr);
    let state = if !enabled {
        "disabled"
    } else if running && !port_consistent {
        "error"
    } else if running {
        "running"
    } else if !credential_state.is_session_valid()
        || status_error
            .as_deref()
            .map(|e| e.contains("AUTH_REQUIRED"))
            .unwrap_or(false)
    {
        "pending_auth"
    } else if status_error.is_some() {
        "error"
    } else {
        "stopped"
    };

    Ok(serde_json::json!({
        "enabled": enabled,
        "port": port,
        "active_port": active_port,
        "port_consistent": port_consistent,
        "runtime_generation": runtime_generation,
        "running": running,
        "state": state,
        "error": status_error,
        "privacy_acknowledged": privacy_acknowledged
        ,"server_version": env!("CARGO_PKG_VERSION")
        ,"skill": {
            "id": mcp_contract::AGENT_SKILL_ID,
            "source_repository": mcp_contract::AGENT_SKILL_SOURCE_REPOSITORY,
            "tool_schema_version": mcp_contract::TOOL_SCHEMA_VERSION
        }
        ,"capabilities": {
            "ocr_engine": "rust_raw_rgb",
            "rust_ml_state": ml_status.state,
            "ocr_model_id": ocr_model_status.as_ref().map(|status| status.model_id.as_str()),
            "ocr_model_revision": ocr_model_status.as_ref().map(|status| status.revision.as_str()),
            "ocr_model_source": ocr_model_status.as_ref().map(|status| status.source.as_str()),
            "ocr_model_verified": ocr_model_status.as_ref().map(|status| status.installed).unwrap_or(false),
            "search_ocr_text": true,
            // M2.5 step 9: two backends can answer this now, so the capability
            // flag stops being "is Python up". The reason string is what the
            // skill shows a user, so it names the backend that is actually
            // missing rather than the one that used to be the only option.
            "search_nl": search_nl.backend.is_some(),
            "search_nl_backend": search_nl.backend,
            "search_nl_disabled_reason": search_nl.disabled_reason
        }
    }))
}

/// Runs an authenticated, read-only loopback MCP protocol smoke test.
///
/// Authentication: required. The bearer token is decrypted and used only in Rust;
/// neither it nor metadata returned by the probe is serialized to JavaScript. The
/// report contains fixed error codes, stage timings, and tool counts only.
#[tauri::command]
pub async fn mcp_run_smoke_test(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<McpSmokeReport, String> {
    super::check_auth_required(&credential_state)?;
    let _lifecycle_guard = mcp_state.lock_lifecycle().await;

    let runtime_generation = mcp_state.generation();
    let active_port = mcp_state.active_port();
    if active_port.is_none() {
        let failure_kind = mcp_state
            .get_last_error()
            .filter(|error| {
                let error = error.to_ascii_lowercase();
                error.contains("bind port") || error.contains("address already in use")
            })
            .map(|_| "port_unavailable")
            .unwrap_or("server_not_running");
        return Ok(McpSmokeReport::preflight_failure(failure_kind)
            .with_runtime_generation(runtime_generation));
    }

    let policy = storage_state.load_policy()?;
    let configured_port = mcp_server::port_from_policy(&policy);
    let active_port = active_port.expect("active port checked above");
    if configured_port != active_port {
        // Never send the bearer token to a policy-selected port unless this
        // runtime currently owns that exact listener.
        return Ok(McpSmokeReport::preflight_failure("port_mismatch")
            .with_runtime_generation(runtime_generation));
    }
    let encrypted_token = policy
        .get("mcp_token_encrypted")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "MCP token is not configured".to_string())?;
    let token = mcp_token::decrypt_token(&credential_state, encrypted_token)?;

    Ok(mcp_smoke::run(active_port, &token)
        .await
        .with_runtime_generation(runtime_generation))
}

/// Persists acknowledgement of the MCP data-exposure warning.
///
/// Authentication: not required. Returns JSON `null`.
/// Frontend: `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_ack_privacy_warning(
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    storage_state.mark_mcp_privacy_acknowledged()
}

/// Rotates the MCP bearer token and copies the new plaintext token to the clipboard.
///
/// Authentication: required. Returns `{ "status": "ok", "token_delivery":
/// "clipboard", "copied_to_clipboard": boolean }`; the token is never serialized to
/// JavaScript. Frontend: `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_reset_token(
    app: tauri::AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;
    let _lifecycle_guard = mcp_state.lock_lifecycle().await;

    let policy = storage_state.load_policy()?;
    let configured_port = mcp_server::port_from_policy(&policy);
    let active_port = mcp_state.active_port();
    let old_token_hash = if let Some(active_port) = active_port {
        if active_port != configured_port {
            return Err(
                "MCP configured port does not match the active listener; restart the service before rotating its token"
                    .to_string(),
            );
        }
        let encrypted_token = policy
            .get("mcp_token_encrypted")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "MCP token is not configured".to_string())?;
        let old_token = mcp_token::decrypt_token(&credential_state, encrypted_token)?;
        let old_token_hash = mcp_token::hash_token(&old_token);
        if mcp_state.get_token_hash() != Some(old_token_hash) {
            return Err(
                "MCP runtime credential does not match the configured token; restart the service before rotating its token"
                    .to_string(),
            );
        }
        Some(old_token_hash)
    } else {
        None
    };

    let token = mcp_token::generate_token();
    let encrypted_b64 = mcp_token::encrypt_token(&credential_state, &token)?;
    let token_hash = mcp_token::hash_token(&token);
    let mut new_policy = policy.clone();
    policy_as_object_mut(&mut new_policy)?.insert(
        "mcp_token_encrypted".into(),
        serde_json::json!(encrypted_b64),
    );

    if let (Some(active_port), Some(old_token_hash)) = (active_port, old_token_hash) {
        mcp_server::stop_server(&mcp_state).await;
        if let Err(error) = mcp_server::start_server(app.clone(), active_port, token_hash).await {
            let restored = match mcp_server::start_server(app.clone(), active_port, old_token_hash)
                .await
            {
                Ok(()) => true,
                Err(restore_error) => {
                    mcp_state.set_last_error(format!(
                        "Failed to rotate MCP token: {error}; failed to restore the previous listener: {restore_error}"
                    ));
                    false
                }
            };
            let _ = app.emit(
                "mcp-status-changed",
                serde_json::json!({
                    "state": if restored { "running" } else { "error" },
                    "error": error.clone(),
                    "active_port": mcp_state.active_port(),
                    "runtime_generation": mcp_state.generation(),
                }),
            );
            return Err(error);
        }
        if let Err(error) = storage_state.save_policy(&new_policy) {
            mcp_server::stop_server(&mcp_state).await;
            let restored = match mcp_server::start_server(app.clone(), active_port, old_token_hash)
                .await
            {
                Ok(()) => true,
                Err(restore_error) => {
                    mcp_state.set_last_error(format!(
                        "Failed to persist rotated MCP token: {error}; failed to restore the previous listener: {restore_error}"
                    ));
                    false
                }
            };
            let _ = app.emit(
                "mcp-status-changed",
                serde_json::json!({
                    "state": if restored { "running" } else { "error" },
                    "error": error.clone(),
                    "active_port": mcp_state.active_port(),
                    "runtime_generation": mcp_state.generation(),
                }),
            );
            return Err(error);
        }
    } else {
        storage_state.save_policy(&new_policy)?;
    }
    mcp_state.set_token_hash(token_hash);

    let copied_to_clipboard = copy_mcp_token_to_clipboard(&window, &token).is_ok();
    let _ = app.emit(
        "mcp-status-changed",
        serde_json::json!({
            "state": if mcp_state.active_port().is_some() { "running" } else { "stopped" },
            "active_port": mcp_state.active_port(),
            "runtime_generation": mcp_state.generation(),
        }),
    );
    Ok(serde_json::json!({
        "status": "ok",
        "token_delivery": "clipboard",
        "copied_to_clipboard": copied_to_clipboard
    }))
}

/// Decrypts the existing MCP token directly into the Windows clipboard.
///
/// Authentication: required. Returns `{ "status": "ok", "token_delivery":
/// "clipboard", "copied_to_clipboard": true }`; plaintext never crosses IPC.
/// Frontend: `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_copy_token_to_clipboard(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;
    let _lifecycle_guard = mcp_state.lock_lifecycle().await;

    let policy = storage_state.load_policy()?;
    let encrypted_token = policy
        .get("mcp_token_encrypted")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No MCP token found in policy".to_string())?;
    let token = mcp_token::decrypt_token(&credential_state, encrypted_token)?;
    copy_mcp_token_to_clipboard(&window, &token)?;
    Ok(serde_json::json!({
        "status": "ok",
        "token_delivery": "clipboard",
        "copied_to_clipboard": true
    }))
}

/// Returns the configured loopback MCP port as a JSON integer.
///
/// Authentication: not required. The settings UI currently obtains this through
/// [`mcp_get_status`].
#[tauri::command]
pub async fn mcp_get_port(
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<u16, String> {
    Ok(mcp_server::get_port(&storage_state))
}

/// Updates the loopback MCP port and atomically restarts an active listener.
///
/// Authentication: required. When a rebind fails, the command restores the
/// previous listener and leaves the persisted port unchanged.
#[tauri::command]
pub async fn mcp_set_port(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    port: u16,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;
    if port == 0 {
        return Err("MCP port must be between 1 and 65535".to_string());
    }
    let _lifecycle_guard = mcp_state.lock_lifecycle().await;

    let policy = storage_state.load_policy()?;
    let old_port = mcp_server::port_from_policy(&policy);
    let mut new_policy = policy.clone();
    policy_as_object_mut(&mut new_policy)?.insert("mcp_port".into(), serde_json::json!(port));
    let active_port = mcp_state.active_port();
    let old_active_port = active_port.unwrap_or(old_port);
    let was_running = active_port.is_some();
    if was_running && active_port != Some(port) {
        let encrypted_token = policy
            .get("mcp_token_encrypted")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "MCP token is not configured".to_string())?;
        let token = mcp_token::decrypt_token(&credential_state, encrypted_token)?;
        let token_hash = mcp_token::hash_token(&token);
        if mcp_state.get_token_hash() != Some(token_hash) {
            return Err(
                "MCP runtime credential does not match the configured token; restart the service before changing its port"
                    .to_string(),
            );
        }
        mcp_server::stop_server(&mcp_state).await;
        if let Err(error) = mcp_server::start_server(app.clone(), port, token_hash).await {
            let restored = match mcp_server::start_server(app.clone(), old_active_port, token_hash)
                .await
            {
                Ok(()) => true,
                Err(restore_error) => {
                    mcp_state.set_last_error(format!(
                        "Failed to move MCP listener to port {port}: {error}; failed to restore port {old_active_port}: {restore_error}"
                    ));
                    false
                }
            };
            if restored {
                mcp_state.clear_last_error();
            }
            let _ = app.emit(
                "mcp-status-changed",
                serde_json::json!({
                    "state": if restored { "running" } else { "error" },
                    "error": error.clone(),
                    "active_port": mcp_state.active_port(),
                    "runtime_generation": mcp_state.generation(),
                }),
            );
            return Err(error);
        }
    }
    if let Err(error) = storage_state.save_policy(&new_policy) {
        if was_running && active_port != Some(port) {
            mcp_server::stop_server(&mcp_state).await;
            if let Some(token_hash) = mcp_state.get_token_hash() {
                if let Err(restore_error) =
                    mcp_server::start_server(app.clone(), old_active_port, token_hash).await
                {
                    mcp_state.set_last_error(format!(
                        "Failed to save MCP port: {error}; failed to restore listener: {restore_error}"
                    ));
                }
            }
        }
        let _ = app.emit(
            "mcp-status-changed",
            serde_json::json!({
                "state": if mcp_state.active_port().is_some() { "running" } else { "error" },
                "active_port": mcp_state.active_port(),
                "runtime_generation": mcp_state.generation(),
            }),
        );
        return Err(error);
    }
    mcp_state.bump_generation();
    let _ = app.emit(
        "mcp-status-changed",
        serde_json::json!({
            "state": if was_running { "running" } else { "stopped" },
            "active_port": mcp_state.active_port(),
            "runtime_generation": mcp_state.generation(),
        }),
    );
    Ok(())
}

/// Returns the active sensitive-content filter configuration.
///
/// Authentication: required. The serialized object is
/// [`sensitive_filter::SensitiveFilterConfig`]. Frontend:
/// `components/settings/agent-access/useSensitiveFilterSettings.js`.
#[tauri::command]
pub async fn mcp_get_sensitive_filter_config(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
) -> Result<sensitive_filter::SensitiveFilterConfig, String> {
    super::check_auth_required(&credential_state)?;

    Ok(filter_state.get_config())
}

/// Replaces and persists the sensitive-content filter configuration.
///
/// Authentication: required. `config` uses the
/// [`sensitive_filter::SensitiveFilterConfig`] JSON shape; returns JSON `null`.
/// Frontend: `components/settings/agent-access/useSensitiveFilterSettings.js`.
#[tauri::command]
pub async fn mcp_set_sensitive_filter_config(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    config: sensitive_filter::SensitiveFilterConfig,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;

    filter_state.update_config(config.clone());

    let mut policy = storage_state.load_policy()?;
    let config_value =
        serde_json::to_value(&config).map_err(|e| format!("Failed to serialize config: {}", e))?;
    policy_as_object_mut(&mut policy)?.insert("sensitive_filter".into(), config_value);
    storage_state.save_policy(&policy)
}

#[derive(Clone, Copy)]
struct ClipSearchReadiness {
    backend: Option<&'static str>,
    disabled_reason: Option<&'static str>,
}

/// Whether the Rust natural-language image search can answer right now.
///
/// Deliberately a readiness diagnostic rather than a configuration one. The
/// Rust path can still stand down for an unfinished step-7 migration or an
/// empty index. The MCP tool table remains static; this check mirrors the refusals
/// in `clip_query::try_rust_clip_query` without running a query. Callers must
/// keep this synchronous read on a blocking thread.
fn clip_search_readiness(storage: &StorageState) -> ClipSearchReadiness {
    if crate::maintenance::is_active() {
        return ClipSearchReadiness {
            backend: None,
            disabled_reason: Some("maintenance_in_progress"),
        };
    }

    let rust_ready = crate::clip_query::migration_settled(storage)
        && storage
            .has_query_visible_embeddings(crate::storage::DerivedIndexKind::ClipImage)
            .unwrap_or(false);
    if rust_ready {
        return ClipSearchReadiness {
            backend: Some("rust"),
            disabled_reason: None,
        };
    }
    ClipSearchReadiness {
        backend: None,
        disabled_reason: Some("clip_index_unavailable"),
    }
}
