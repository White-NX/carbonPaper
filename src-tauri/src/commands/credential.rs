use std::sync::Arc;
use crate::credential_manager::{self, CredentialManagerState};
use crate::storage::StorageState;

#[tauri::command]
pub async fn credential_initialize(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        match credential_manager::load_public_key_from_file(&credential_state) {
            Ok(_) => {}
            Err(_) => {
                let pk = credential_manager::export_or_get_public_key(&credential_state)
                    .map_err(|e| format!("Failed to initialize credentials: {}", e))?;

                credential_manager::save_public_key_to_file(&credential_state, &pk)
                    .map_err(|e| format!("Failed to save public key: {}", e))?;
            }
        };

        credential_manager::ensure_master_key_created(&credential_state)
            .map_err(|e| format!("Failed to create master key: {}", e))?;

        storage_state.initialize()?;

        Ok("Credentials initialized successfully".to_string())
    }

    #[cfg(not(windows))]
    {
        let _ = &credential_state;
        let _ = &storage_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

#[tauri::command]
pub async fn credential_verify_user(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    #[cfg(windows)]
    {
        credential_manager::force_verify_and_unlock_master_key(&state)
            .map_err(|e| format!("Verification failed: {}", e))?;

        state.update_auth_time();
        storage_state.try_dedup_migration();
        storage_state.try_bitmap_index_migration();

        Ok(true)
    }

    #[cfg(not(windows))]
    {
        let _ = &state;
        let _ = &storage_state;
        Err("Windows Hello is only available on Windows".to_string())
    }
}

#[tauri::command]
pub async fn credential_check_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    Ok(state.is_session_valid())
}

#[tauri::command]
pub async fn credential_lock_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    state.invalidate_session();
    Ok(())
}

#[tauri::command]
pub async fn credential_set_foreground(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    in_foreground: bool,
) -> Result<(), String> {
    state.set_foreground_state(in_foreground);
    Ok(())
}

#[tauri::command]
pub async fn credential_set_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    timeout: i64,
) -> Result<(), String> {
    state.set_session_timeout(timeout);
    if let Err(e) = crate::registry_config::set_string("session_timeout_secs", &timeout.to_string())
    {
        tracing::error!("Failed to persist session_timeout_secs: {}", e);
    }
    Ok(())
}

#[tauri::command]
pub async fn credential_get_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<i64, String> {
    Ok(state.get_session_timeout())
}
