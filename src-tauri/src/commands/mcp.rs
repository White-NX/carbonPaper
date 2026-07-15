//! Tauri commands for local MCP server configuration and credential delivery.
//!
//! The server binds only to loopback and authenticates clients with a bearer token.
//! Status and privacy acknowledgement are readable before authentication; operations
//! that expose or change credentials and policy require a valid user session.

use crate::credential_manager::CredentialManagerState;
use crate::mcp_server;
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

        let port = policy
            .get("mcp_port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .unwrap_or(mcp_server::get_port(&storage_state));

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
            serde_json::json!({ "state": "running" }),
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
            serde_json::json!({ "state": "disabled" }),
        );

        Ok(serde_json::json!({ "status": "ok" }))
    }
}

/// Returns runtime, privacy, model, and search-capability status for MCP settings.
///
/// Authentication: not required; no token or encrypted secret is returned. The JSON
/// object contains `enabled`, `port`, `running`, `state`, `error`,
/// `privacy_acknowledged`, `server_version`, `skill`, and `capabilities`.
/// Frontend: `components/settings/useAiEmbeddingController.js`.
#[tauri::command]
pub async fn mcp_get_status(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    monitor_state: tauri::State<'_, crate::monitor::MonitorState>,
    ml_state: tauri::State<'_, Arc<crate::ml_runtime::MlRuntimeState>>,
) -> Result<serde_json::Value, String> {
    let policy = storage_state.load_policy()?;
    let enabled = policy
        .get("mcp_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let port = mcp_server::get_port(&storage_state);
    let running = mcp_state.is_running();
    let last_error = mcp_state.get_last_error();
    let privacy_acknowledged = mcp_privacy_acknowledged_from_policy_or_db(&storage_state, &policy);
    let python_running = monitor_state
        .process
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    let rust_ocr_enabled = crate::registry_config::get_bool("rust_ocr_enabled").unwrap_or(true);
    let ml_status = ml_state.status(storage_state.count_failed_ocr().unwrap_or(0));
    let app_for_model_status = app.clone();
    let ocr_model_status = tokio::task::spawn_blocking(move || {
        crate::ml_runtime::ocr_model_status(&app_for_model_status)
    })
    .await
    .ok()
    .and_then(Result::ok);
    let state = if !enabled {
        "disabled"
    } else if running {
        "running"
    } else if !credential_state.is_session_valid()
        || last_error
            .as_deref()
            .map(|e| e.contains("AUTH_REQUIRED"))
            .unwrap_or(false)
    {
        "pending_auth"
    } else if last_error.is_some() {
        "error"
    } else {
        "stopped"
    };

    Ok(serde_json::json!({
        "enabled": enabled,
        "port": port,
        "running": running,
        "state": state,
        "error": last_error,
        "privacy_acknowledged": privacy_acknowledged
        ,"server_version": env!("CARGO_PKG_VERSION")
        ,"skill": {
            "id": "carbonpaper-memory",
            "source_repository": "https://github.com/White-NX/carbonPaperSkill",
            "tool_schema_version": 1
        }
        ,"capabilities": {
            "ocr_engine": if rust_ocr_enabled { "rust" } else { "python" },
            "rust_ml_state": ml_status.state,
            "ocr_model_id": ocr_model_status.as_ref().map(|status| status.model_id.as_str()),
            "ocr_model_revision": ocr_model_status.as_ref().map(|status| status.revision.as_str()),
            "ocr_model_source": ocr_model_status.as_ref().map(|status| status.source.as_str()),
            "ocr_model_verified": ocr_model_status.as_ref().map(|status| status.installed).unwrap_or(false),
            "search_ocr_text": true,
            "search_nl": python_running,
            "search_nl_disabled_reason": if python_running { serde_json::Value::Null } else { serde_json::json!("legacy_python_monitor_not_running") }
        }
    }))
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

    let token = mcp_token::generate_token();
    let encrypted_b64 = mcp_token::encrypt_token(&credential_state, &token)?;

    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert(
        "mcp_token_encrypted".into(),
        serde_json::json!(encrypted_b64),
    );
    storage_state.save_policy(&policy)?;

    let token_hash = mcp_token::hash_token(&token);
    mcp_state.set_token_hash(token_hash);

    let was_running = mcp_state.is_running();
    if was_running {
        mcp_server::stop_server(&mcp_state).await;
        let port = mcp_server::get_port(&storage_state);
        mcp_server::start_server(app, port, token_hash).await?;
    }

    let copied_to_clipboard = copy_mcp_token_to_clipboard(&window, &token).is_ok();
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
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

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

/// Persists the loopback MCP listening port.
///
/// Authentication: required. `port` is a `u16`; returns JSON `null`. A running server
/// uses the new value after it is restarted.
#[tauri::command]
pub async fn mcp_set_port(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    port: u16,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;

    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
    storage_state.save_policy(&policy)
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
