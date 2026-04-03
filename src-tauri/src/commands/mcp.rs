use std::sync::Arc;
use crate::credential_manager::CredentialManagerState;
use crate::storage::StorageState;
use crate::mcp_server;
use crate::sensitive_filter::{self, SensitiveFilterState};

fn policy_as_object_mut(policy: &mut serde_json::Value) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    policy.as_object_mut().ok_or_else(|| "Policy is not a valid JSON object".to_string())
}

#[tauri::command]
pub async fn mcp_set_enabled(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    if enabled {
        let mut policy = storage_state.load_policy()?;
        let existing_token = policy.get("mcp_token_encrypted").and_then(|v| v.as_str());
        let (token_plaintext, is_new_token) = if let Some(encrypted_b64) = existing_token {
            let token = mcp_server::decrypt_token(&credential_state, encrypted_b64)?;
            (token, false)
        } else {
            let token = mcp_server::generate_token();
            let encrypted_b64 = mcp_server::encrypt_token(&credential_state, &token)?;
            policy_as_object_mut(&mut policy)?.insert("mcp_token_encrypted".into(), serde_json::json!(encrypted_b64));
            (token, true)
        };

        let port = policy.get("mcp_port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .unwrap_or(mcp_server::get_port(&storage_state));

        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(true));
        if policy.get("mcp_port").is_none() {
            policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
        }
        storage_state.save_policy(&policy)?;

        let token_hash = mcp_server::hash_token(&token_plaintext);
        mcp_state.set_token_hash(token_hash);
        mcp_server::start_server(app, port, token_hash).await?;

        if is_new_token {
            Ok(serde_json::json!({ "status": "ok", "token": token_plaintext, "port": port }))
        } else {
            Ok(serde_json::json!({ "status": "ok", "port": port }))
        }
    } else {
        mcp_server::stop_server(&mcp_state).await;
        let mut policy = storage_state.load_policy()?;
        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(false));
        storage_state.save_policy(&policy)?;

        Ok(serde_json::json!({ "status": "ok" }))
    }
}

#[tauri::command]
pub async fn mcp_get_status(
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    let policy = storage_state.load_policy()?;
    let enabled = policy.get("mcp_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let port = mcp_server::get_port(&storage_state);
    let running = mcp_state.is_running();

    Ok(serde_json::json!({
        "enabled": enabled,
        "port": port,
        "running": running
    }))
}

#[tauri::command]
pub async fn mcp_reset_token(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    let token = mcp_server::generate_token();
    let encrypted_b64 = mcp_server::encrypt_token(&credential_state, &token)?;

    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert("mcp_token_encrypted".into(), serde_json::json!(encrypted_b64));
    storage_state.save_policy(&policy)?;

    let token_hash = mcp_server::hash_token(&token);
    mcp_state.set_token_hash(token_hash);

    let was_running = mcp_state.is_running();
    if was_running {
        mcp_server::stop_server(&mcp_state).await;
        let port = mcp_server::get_port(&storage_state);
        mcp_server::start_server(app, port, token_hash).await?;
    }

    Ok(serde_json::json!({ "status": "ok", "token": token }))
}

#[tauri::command]
pub async fn mcp_get_port(
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<u16, String> {
    Ok(mcp_server::get_port(&storage_state))
}

#[tauri::command]
pub async fn mcp_set_port(
    storage_state: tauri::State<'_, Arc<StorageState>>,
    port: u16,
) -> Result<(), String> {
    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
    storage_state.save_policy(&policy)
}

#[tauri::command]
pub async fn mcp_get_sensitive_filter_config(
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
) -> Result<sensitive_filter::SensitiveFilterConfig, String> {
    Ok(filter_state.get_config())
}

#[tauri::command]
pub async fn mcp_set_sensitive_filter_config(
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    config: sensitive_filter::SensitiveFilterConfig,
) -> Result<(), String> {
    filter_state.update_config(config.clone());

    let mut policy = storage_state.load_policy()?;
    let config_value = serde_json::to_value(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    policy_as_object_mut(&mut policy)?.insert("sensitive_filter".into(), config_value);
    storage_state.save_policy(&policy)
}
