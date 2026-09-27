//! Tauri commands for configuring the model endpoints used by AI search.
//!
//! Reading the settings works while locked because it never includes a key.
//! Every change, and any request that needs a decrypted key, requires an
//! authenticated session.

use crate::ai::config::{AiSettings, AiSettingsView, ProviderInput};
use crate::ai::provider::{self, ConnectionTest};
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
