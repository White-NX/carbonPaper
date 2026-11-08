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

async fn service(request: Request) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        carbonpaper_app_bound::windows::transport::call(request)
            .map_err(|e| format!("AGENT_SERVICE_UNAVAILABLE: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
async fn fresh_verification(app: &tauri::AppHandle, window: &tauri::Window) -> Result<(), String> {
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
    let storage = storage_state.inner().clone();
    if enabled {
        tokio::task::spawn_blocking(move || storage.begin_agent_access())
            .await
            .map_err(|e| e.to_string())??;
        let mut policy = storage_state.load_policy()?;
        policy
            .as_object_mut()
            .ok_or("Invalid policy")?
            .insert("mcp_enabled".into(), json!(true));
        storage_state.save_policy(&policy)?;
        crate::mcp_server::stop_server(&mcp_state).await;
    } else {
        // Revoke in the service before shutdown or waiting for backfill, so
        // already admitted requests cannot return while disable is completing.
        service(Request::AgentDisable {}).await?;
        crate::mcp_server::stop_server(&mcp_state).await;
        tokio::task::spawn_blocking(move || storage.disable_agent_access())
            .await
            .map_err(|e| e.to_string())??;
        if storage_state.load_policy()?["mcp_enabled"].as_bool() == Some(true)
            && credential_state.is_session_valid()
        {
            let _ = crate::mcp_server::auto_start_under_lifecycle(
                app.clone(),
                &credential_state,
                &storage_state,
                &mcp_state,
            )
            .await;
        }
    }
    let _ = app.emit("agent-access-changed", json!({"enabled":enabled}));
    Ok(json!({"enabled":enabled,"preparation":storage_state.agent_preparation()}))
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
        service(Request::AgentRevoke { grant_id: id }).await?;
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
