use std::sync::Arc;
use tauri::Emitter;
use crate::storage::StorageState;

#[derive(serde::Serialize)]
pub struct HmacMigrationStatus {
    pub needs_migration: bool,
    pub is_running: bool,
}

#[tauri::command]
pub async fn storage_check_hmac_migration_status(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<HmacMigrationStatus, String> {
    let needs_migration = state.check_hmac_migration_status()?;
    let is_running = state.is_hmac_migration_in_progress();
    
    Ok(HmacMigrationStatus {
        needs_migration,
        is_running,
    })
}

#[tauri::command]
pub async fn storage_run_hmac_migration(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    let state = state.inner().clone();
    
    if state.is_hmac_migration_in_progress() {
        return Err("ALREADY_RUNNING".to_string());
    }

    tokio::task::spawn_blocking(move || {
        let app_handle_clone = app_handle.clone();
        let result = state.run_hmac_migration(move |phase, processed, total| {
            let _ = app_handle_clone.emit(
                "hmac-migration-progress",
                serde_json::json!({
                    "phase": phase,
                    "processed": processed,
                    "total": total
                }),
            );
        });

        if result.is_ok() {
            let _ = app_handle.emit("hmac-migration-complete", ());
        }
        result
    })
    .await
    .map_err(|e| format!("Migration task panicked: {}", e))?
}

#[tauri::command]
pub async fn storage_hmac_migration_cancel(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    let in_progress = state.request_hmac_migration_cancel();
    Ok(serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "is_running": in_progress
    }))
}

#[tauri::command]
pub async fn storage_list_plaintext_files(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    state.list_plaintext_screenshots()
}

#[tauri::command]
pub async fn storage_migrate_plaintext(
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    let res = tokio::task::spawn_blocking(move || state.migrate_plaintext_screenshots())
        .await
        .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({
        "total_files": res.total_files,
        "migrated": res.migrated,
        "skipped": res.skipped,
        "errors": res.errors
    }))
}

#[tauri::command]
pub async fn storage_migrate_data_dir(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, Arc<StorageState>>,
    target: String,
    migrate_data_files: bool,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        state.migrate_data_dir_blocking(app_handle, target, migrate_data_files)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

#[tauri::command]
pub fn storage_migration_cancel(state: tauri::State<'_, Arc<StorageState>>) -> serde_json::Value {
    let in_progress = state.request_migration_cancel();
    serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "in_progress": in_progress
    })
}

#[tauri::command]
pub async fn storage_delete_plaintext(
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    let count = tokio::task::spawn_blocking(move || state.delete_plaintext_screenshots())
        .await
        .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({ "deleted": count }))
}
