//! Tauri commands for AI search and the model endpoints it uses.
//!
//! Reading the settings works while locked because it never includes a key.
//! Every change, and any request that needs a decrypted key, requires an
//! authenticated session.

use crate::ai::agent::{self, AgentEvent, AgentLimits, AgentOutcome};
use crate::ai::config::{is_local_url, AiSettings, AiSettingsView, ProviderInput};
use crate::ai::provider::{self, ConnectionTest, ProviderError};
use crate::ai::AiRuntimeState;
use crate::credential_manager::CredentialManagerState;
use crate::storage::StorageState;
use std::sync::Arc;

/// Authentication: not required. Returns [`AiSettingsView`]. Frontend:
/// `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_get_settings(
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<AiSettingsView, String> {
    Ok(AiSettings::load(&storage_state)?.view())
}

/// Authentication: required. Creates or updates a provider and returns the
/// updated [`AiSettingsView`]. Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_save_provider(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    provider: ProviderInput,
) -> Result<AiSettingsView, String> {
    super::check_auth_required(&credential_state)?;
    let mut settings = AiSettings::load(&storage_state)?;
    settings.upsert(&credential_state, provider)?;
    settings.save(&storage_state)?;
    Ok(settings.view())
}

/// Authentication: required. Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_delete_provider(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    id: String,
) -> Result<AiSettingsView, String> {
    super::check_auth_required(&credential_state)?;
    let mut settings = AiSettings::load(&storage_state)?;
    settings.remove(&id)?;
    settings.save(&storage_state)?;
    Ok(settings.view())
}

/// Authentication: required. Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_set_default_provider(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    id: String,
) -> Result<AiSettingsView, String> {
    super::check_auth_required(&credential_state)?;
    let mut settings = AiSettings::load(&storage_state)?;
    settings.set_default(&id)?;
    settings.save(&storage_state)?;
    Ok(settings.view())
}

/// Authentication: required. Tests the provider as currently edited, which
/// may be unsaved. When `provider.id` names a saved provider whose endpoint
/// matches, the measured tool support is stored. Returns [`ConnectionTest`].
/// Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_test_provider(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    provider: ProviderInput,
) -> Result<ConnectionTest, String> {
    super::check_auth_required(&credential_state)?;
    let settings = AiSettings::load(&storage_state)?;
    let id = provider.id.clone();
    let resolved = settings.resolve_input(&credential_state, provider)?;
    let result = provider::test_connection(&resolved).await;

    if let (true, Some(id)) = (result.ok, id) {
        // Reload: the settings may have changed while the request ran.
        let mut latest = AiSettings::load(&storage_state)?;
        let unchanged = latest.find(&id).is_some_and(|p| {
            p.kind == resolved.kind && p.base_url == resolved.base_url && p.model == resolved.model
        });
        if unchanged {
            latest.set_tool_calling(&id, result.tool_calling);
            latest.save(&storage_state)?;
        }
    }
    Ok(result)
}

/// Authentication: required. Records that the user agreed to send screenshot
/// text to endpoints outside this computer. Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_grant_remote_consent(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<AiSettingsView, String> {
    super::check_auth_required(&credential_state)?;
    let mut settings = AiSettings::load(&storage_state)?;
    settings.remote_consent = true;
    settings.save(&storage_state)?;
    Ok(settings.view())
}

const MAX_QUESTION_CHARS: usize = 4_000;
/// Wall-clock cap for one AI search, including every model call and tool.
const SEARCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// Authentication: required. Answers `question` by letting the default (or
/// the given) provider search the history. Progress streams to `on_event` as
/// [`AgentEvent`] values; the final [`AgentOutcome`] is the return value.
/// `request_id` is chosen by the caller and names the run for
/// [`ai_search_cancel`]. Errors use the `AI_*` codes. Frontend:
/// `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_search(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    runtime: tauri::State<'_, AiRuntimeState>,
    request_id: String,
    question: String,
    history: Option<Vec<agent::ConversationTurn>>,
    provider_id: Option<String>,
    on_event: tauri::ipc::Channel<AgentEvent>,
) -> Result<AgentOutcome, String> {
    super::check_auth_required(&credential_state)?;
    let question = question.trim();
    if question.is_empty() {
        return Err("AI_EMPTY_QUESTION".into());
    }
    let question: String = question.chars().take(MAX_QUESTION_CHARS).collect();

    let settings = AiSettings::load(&storage_state)?;
    let id = provider_id
        .as_deref()
        .or_else(|| settings.effective_default_id())
        .ok_or_else(|| "AI_NO_PROVIDER".to_string())?
        .to_string();
    let stored = settings
        .find(&id)
        .ok_or_else(|| "AI_PROVIDER_NOT_FOUND".to_string())?;
    if !settings.remote_consent && !is_local_url(&stored.base_url) {
        return Err("AI_REMOTE_CONSENT_REQUIRED".into());
    }
    let resolved = settings.resolve(&credential_state, &id)?;

    let cancel = runtime
        .register(&request_id)
        .ok_or_else(|| "AI_BUSY".to_string())?;
    let mut sink = |event: AgentEvent| {
        // A closed channel only means the page went away; keep going so the
        // cancel command (or the deadline) decides when to stop.
        let _ = on_event.send(event);
    };
    let result = tokio::time::timeout(
        SEARCH_DEADLINE,
        agent::run(
            &app,
            &resolved,
            &question,
            history.as_deref().unwrap_or_default(),
            AgentLimits::default(),
            &mut sink,
            &cancel,
        ),
    )
    .await;
    runtime.finish(&request_id);

    match result {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(ProviderError::Timeout.to_string()),
    }
}

/// Authentication: not required; it can only stop work. Returns whether a
/// run with that id was active. Frontend: `lib/ai_api.js`.
#[tauri::command]
pub async fn ai_search_cancel(
    runtime: tauri::State<'_, AiRuntimeState>,
    request_id: String,
) -> Result<bool, String> {
    Ok(runtime.cancel(&request_id))
}
