//! Coordinated security presets. Persistent Agent consent remains native.
use crate::{
    credential_manager::CredentialManagerState,
    mcp_server::McpRuntimeState,
    registry_config,
    storage::{AgentPreparationProgress, StorageState},
};
use carbonpaper_app_bound::protocol::{Request, Response};
use serde::Serialize;
use std::sync::Arc;
use tauri::Emitter;

#[derive(Serialize)]
pub struct SecurityPolicy {
    level: String,
    session_timeout: i64,
    unattended_enabled: bool,
    available: bool,
    phase: String,
    preparation: AgentPreparationProgress,
    intro_seen: bool,
}

fn inferred_level(timeout: i64, unattended: bool, preference: &str) -> &'static str {
    if preference == "custom" {
        return "custom";
    }
    match (timeout, unattended) {
        (900, false) => "strict",
        (-1, false) => "relaxed",
        (-1, true) => "unattended",
        _ => "custom",
    }
}
fn validate(level: &str, timeout: i64, unattended: bool) -> Result<(), String> {
    if timeout != -1 && !(1..=31_536_000).contains(&timeout) {
        return Err("INVALID_SECURITY_POLICY".into());
    }
    match level {
        "custom" => Ok(()),
        "strict" if timeout == 900 && !unattended => Ok(()),
        "relaxed" if timeout == -1 && !unattended => Ok(()),
        "unattended" if timeout == -1 && unattended => Ok(()),
        _ => Err("INVALID_SECURITY_POLICY".into()),
    }
}
fn permission_change_needed(requested: bool, desktop: bool, authority: Option<bool>) -> bool {
    if requested {
        !desktop || authority == Some(false)
    } else {
        desktop || authority == Some(true)
    }
}
async fn policy(credentials: &CredentialManagerState, storage: &StorageState) -> SecurityPolicy {
    let timeout = credentials.get_session_timeout();
    let preparation = storage.agent_preparation();
    let (unattended, available, phase) =
        match super::agent_access::service(Request::AgentStatus {}).await {
            Ok(Response::AgentStatus(status)) => (
                status.enabled,
                crate::agent_access::available(),
                if preparation.phase.is_empty() || preparation.phase == "ready" {
                    status.phase
                } else {
                    preparation.phase.clone()
                },
            ),
            _ => (storage.agent_access_enabled(), false, "unavailable".into()),
        };
    SecurityPolicy {
        level: inferred_level(
            timeout,
            unattended,
            &registry_config::get_string("security_level").unwrap_or_default(),
        )
        .into(),
        session_timeout: timeout,
        unattended_enabled: unattended,
        available,
        phase,
        preparation,
        intro_seen: registry_config::get_bool("security_intro_seen").unwrap_or(false),
    }
}

/// Public policy metadata only; authorized main/settings windows only.
#[tauri::command]
pub async fn security_get_policy(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, McpRuntimeState>,
) -> Result<SecurityPolicy, String> {
    crate::settings_window::check_settings_ui(&window)?;
    let _lifecycle = mcp_state.lock_lifecycle().await;
    Ok(policy(&credential_state, &storage_state).await)
}

/// Public UI progress marker only; it never changes permissions or sessions.
#[tauri::command]
pub fn security_acknowledge_intro(
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    registry_config::set_bool("security_intro_seen", true)?;
    app.emit("security-intro-seen", true)
        .map_err(|e| e.to_string())
}

/// Session and main/settings authorization required. Enabling persistent
/// access requires fresh Windows verification before changing either setting.
#[tauri::command]
pub async fn security_set_policy(
    app: tauri::AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, McpRuntimeState>,
    level: String,
    session_timeout: i64,
    unattended_enabled: bool,
) -> Result<SecurityPolicy, String> {
    super::check_auth_required(&credential_state)?;
    crate::settings_window::check_settings_ui(&window)?;
    validate(&level, session_timeout, unattended_enabled)?;
    let mut verified = false;
    let (_lifecycle, change_needed) = loop {
        let lifecycle = mcp_state.lock_lifecycle().await;
        super::check_auth_required(&credential_state)?;
        let authority = match super::agent_access::service(Request::AgentStatus {}).await {
            Ok(Response::AgentStatus(status)) => Some(status.enabled),
            _ => None,
        };
        let change_needed = permission_change_needed(
            unattended_enabled,
            storage_state.agent_access_enabled(),
            authority,
        );
        if unattended_enabled && change_needed && !verified {
            if !crate::agent_access::available() {
                return Err("AGENT_PROTECTED_RUNTIME_REQUIRED".into());
            }
            // Verification completion also restores MCP. Release its lifecycle
            // lock while awaiting consent, then recheck the actual permission.
            drop(lifecycle);
            super::agent_access::fresh_verification(&app, &window).await?;
            verified = true;
            continue;
        }
        break (lifecycle, change_needed);
    };
    let previous_timeout = credential_state.get_session_timeout();
    let previous_level = registry_config::get_string("security_level").unwrap_or_default();
    registry_config::set_string("session_timeout_secs", &session_timeout.to_string())?;
    let result = async {
        registry_config::set_string("security_level", &level)?;
        if change_needed {
            super::agent_access::set_enabled_under_lifecycle(
                &app,
                &credential_state,
                storage_state.inner(),
                &mcp_state,
                unattended_enabled,
            )
            .await?;
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        // Revoked credentials are never recreated by rollback. The next policy
        // read derives the actual level if a disable completed partially.
        let _ = registry_config::set_string("session_timeout_secs", &previous_timeout.to_string());
        let _ = registry_config::set_string("security_level", &previous_level);
        let _ = app.emit(
            "agent-access-changed",
            serde_json::json!({"policy_changed":true}),
        );
        return Err(error);
    }
    credential_state.set_session_timeout(session_timeout);
    let _ = app.emit("settings-preferences-changed", vec!["session", "security"]);
    let _ = app.emit(
        "agent-access-changed",
        serde_json::json!({"policy_changed":true}),
    );
    Ok(policy(&credential_state, &storage_state).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_reject_inconsistent_permissions_and_preserve_explicit_custom() {
        assert!(validate("strict", 900, false).is_ok());
        assert!(validate("strict", 900, true).is_err());
        assert!(validate("relaxed", -1, false).is_ok());
        assert!(validate("unattended", 900, true).is_err());
        assert!(validate("unattended", -1, true).is_ok());
        assert!(validate("custom", 300, true).is_ok());
        assert!(validate("custom", 0, false).is_err());
        assert_eq!(inferred_level(900, false, "custom"), "custom");
        assert_eq!(inferred_level(300, false, "strict"), "custom");
    }

    #[test]
    fn policy_changes_reconcile_service_authority_and_desktop_state() {
        // A missing desktop view must not leave service grants active on disable.
        assert!(permission_change_needed(false, false, Some(true)));
        // Retired authority requires a fresh enable even with cached writer keys.
        assert!(permission_change_needed(true, true, Some(false)));
        assert!(permission_change_needed(true, false, Some(true)));
        // Ordinary presets remain usable without an installed service, and an
        // active permission needs no new consent for a timeout refinement.
        assert!(!permission_change_needed(false, false, None));
        assert!(!permission_change_needed(true, true, Some(true)));
    }
}
