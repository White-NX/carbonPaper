//! Persistent text permissions require an authenticated settings/main window
//! and explicit native Windows verification. Secrets are delivered only by the
//! native clipboard command; neither key material nor plaintext secrets cross IPC.
use crate::{
    credential_manager::CredentialManagerState, mcp_server::McpRuntimeState, storage::StorageState,
};
use carbonpaper_app_bound::protocol::{Request, Response};
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Emitter;

pub(super) async fn service(request: Request) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        carbonpaper_app_bound::windows::transport::call(request)
            .map_err(|e| format!("AGENT_SERVICE_UNAVAILABLE: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
pub(super) async fn fresh_verification(
    app: &tauri::AppHandle,
    window: &tauri::Window,
) -> Result<(), String> {
    let owner = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    crate::commands::credential::verify_user_for_owner(app.clone(), owner).await?;
    Ok(())
}

/// Authentication: status contains no text, paths, bearer secrets or keys.
/// Credential labels are included only for an authenticated application UI.
#[tauri::command]
pub async fn agent_access_get_status(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Value, String> {
    crate::settings_window::check_settings_ui(&window)?;
    let available = crate::agent_access::available();
    let preparation = storage_state.agent_preparation();
    let status = service(Request::AgentStatus {}).await;
    match status {
        Ok(Response::AgentStatus(status)) => Ok(
            json!({"enabled":status.enabled,"available":available,"phase":if preparation.phase.is_empty() || preparation.phase=="ready" {status.phase.as_str()}else{preparation.phase.as_str()},
            "preparation":preparation,"source_revision":status.source_revision,"applied_revision":status.applied_revision,
            "grants":if credential_state.is_session_valid(){status.grants}else{vec![]},"read_only":true,"text_only":true}),
        ),
        _ => Ok(
            json!({"enabled":storage_state.agent_access_enabled(),"available":false,"phase":if available{"unavailable"}else{"disabled"},"preparation":preparation,"grants":[],"read_only":true,"text_only":true}),
        ),
    }
}

/// Authentication and main/settings window authorization required. Enabling
/// performs fresh Windows verification even if the session is already valid.
#[tauri::command]
pub async fn agent_access_set_enabled(
    app: tauri::AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, McpRuntimeState>,
    enabled: bool,
) -> Result<Value, String> {
    super::check_auth_required(&credential_state)?;
    crate::settings_window::check_settings_ui(&window)?;
    if enabled {
        if !crate::agent_access::available() {
            return Err("AGENT_PROTECTED_RUNTIME_REQUIRED".into());
        }
        fresh_verification(&app, &window).await?;
        super::check_auth_required(&credential_state)?;
    }
    let _guard = mcp_state.lock_lifecycle().await;
    super::check_auth_required(&credential_state)?;
    set_enabled_under_lifecycle(
        &app,
        &credential_state,
        storage_state.inner(),
        &mcp_state,
        enabled,
    )
    .await?;
    let _ = app.emit("agent-access-changed", json!({"enabled":enabled}));
    Ok(json!({"enabled":enabled,"preparation":storage_state.agent_preparation()}))
}

/// Caller holds the MCP lifecycle lock and has checked session/window consent.
pub(super) async fn set_enabled_under_lifecycle(
    app: &tauri::AppHandle,
    credentials: &CredentialManagerState,
    storage: &Arc<StorageState>,
    runtime: &McpRuntimeState,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        if storage.agent_access_enabled() {
            let Response::AgentStatus(status) = service(Request::AgentStatus {}).await? else {
                return Err("AGENT_INVALID_STATUS".into());
            };
            if !status.enabled {
                // A retired service view cannot be revived with cached writer
                // keys. The caller has fresh consent to prepare a new generation.
                crate::mcp_server::stop_server(runtime).await;
                let writer = storage.clone();
                tokio::task::spawn_blocking(move || writer.finish_agent_access_disable())
                    .await
                    .map_err(|e| e.to_string())??;
            }
        }
        let was_enabled = storage.agent_access_enabled();
        let writer = storage.clone();
        tokio::task::spawn_blocking(move || writer.begin_agent_access())
            .await
            .map_err(|e| e.to_string())??;
        let persist = (|| {
            let mut policy = storage.load_policy()?;
            policy
                .as_object_mut()
                .ok_or("Invalid policy")?
                .insert("mcp_enabled".into(), json!(true));
            storage.save_policy(&policy)
        })();
        if let Err(error) = persist {
            if !was_enabled {
                let writer = storage.clone();
                let _ = tokio::task::spawn_blocking(move || writer.disable_agent_access()).await;
            }
            return Err(error);
        }
        crate::mcp_server::stop_server(runtime).await;
    } else {
        // Retire service authority before waiting for reader or exporter work.
        service(Request::AgentDisable {}).await?;
        crate::mcp_server::stop_server(runtime).await;
        let writer = storage.clone();
        tokio::task::spawn_blocking(move || writer.finish_agent_access_disable())
            .await
            .map_err(|e| e.to_string())??;
        if storage.load_policy()?["mcp_enabled"].as_bool() == Some(true)
            && credentials.is_session_valid()
        {
            let _ = crate::mcp_server::auto_start_under_lifecycle(
                app.clone(),
                credentials,
                storage,
                runtime,
            )
            .await;
        }
    }
    Ok(())
}

/// Authentication and UI authorization required; resumes an interrupted text
/// backfill using the ordinary unlocked session, without another permission grant.
#[tauri::command]
pub async fn agent_access_resume(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Value, String> {
    super::check_auth_required(&credential_state)?;
    crate::settings_window::check_settings_ui(&window)?;
    let storage = storage_state.inner().clone();
    tokio::task::spawn_blocking(move || storage.sync_agent_access())
        .await
        .map_err(|e| e.to_string())??;
    Ok(json!(storage_state.agent_preparation()))
}

/// Authentication, UI authorization and fresh Windows verification required.
/// Optional replacement revokes the old grant after successful one-time delivery.
#[tauri::command]
pub async fn agent_access_issue_credential(
    app: tauri::AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, McpRuntimeState>,
    label: String,
    expires_at_ms: Option<i64>,
    replace_grant_id: Option<String>,
) -> Result<Value, String> {
    super::check_auth_required(&credential_state)?;
    crate::settings_window::check_settings_ui(&window)?;
    let binding = storage_state
        .agent_binding()
        .ok_or("AGENT_ACCESS_DISABLED")?;
    if let Some(id) = &replace_grant_id {
        if !carbonpaper_app_bound::protocol::valid_id(id) {
            return Err("AGENT_INVALID_GRANT".into());
        }
    }
    fresh_verification(&app, &window).await?;
    super::check_auth_required(&credential_state)?;
    let _guard = mcp_state.lock_lifecycle().await;
    super::check_auth_required(&credential_state)?;
    if let Some(id) = &replace_grant_id {
        let Response::AgentStatus(status) = service(Request::AgentStatus {}).await? else {
            return Err("AGENT_INVALID_GRANT".into());
        };
        if !status
            .grants
            .iter()
            .any(|grant| &grant.id == id && !grant.revoked)
        {
            return Err("AGENT_INVALID_GRANT".into());
        }
    }
    let secret = zeroize::Zeroizing::new(crate::mcp_token::generate_token());
    let hash = hex::encode(crate::mcp_token::hash_token(&secret));
    let Response::AgentGrant(grant) = service(Request::AgentIssue {
        generation: binding.generation,
        label,
        token_hash: hash,
        expires_at_ms,
    })
    .await?
    else {
        return Err("AGENT_INVALID_GRANT".into());
    };
    if let Err(error) = super::mcp::copy_mcp_token_to_clipboard(&window, &secret) {
        service(Request::AgentRevoke { grant_id: grant.id }).await?;
        return Err(format!("AGENT_CLIPBOARD_FAILED: {error}"));
    }
    if let Some(id) = replace_grant_id {
        if let Err(error) = service(Request::AgentRevoke { grant_id: id }).await {
            let _ = app.emit("agent-access-changed", json!({"credential_changed":true}));
            return Err(format!("AGENT_REPLACEMENT_REVOCATION_FAILED: {error}"));
        }
    }
    let _ = app.emit("agent-access-changed", json!({"credential_changed":true}));
    Ok(json!({"grant":grant,"copied_to_clipboard":true}))
}

/// Authentication and UI authorization required; independently revokes one grant.
#[tauri::command]
pub async fn agent_access_revoke_credential(
    app: tauri::AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    grant_id: String,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;
    crate::settings_window::check_settings_ui(&window)?;
    service(Request::AgentRevoke { grant_id }).await?;
    let _ = app.emit("agent-access-changed", json!({"credential_changed":true}));
    Ok(())
}
