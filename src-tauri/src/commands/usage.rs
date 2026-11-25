//! Authenticated Usage Statistics API. Frontend: lib/usage_api.js.
use super::check_auth_required;
use crate::{
    credential_manager::CredentialManagerState,
    storage::StorageState,
    usage::{
        types::{UsageAppDetails, UsageDay},
        UsageRuntime,
    },
};
use std::sync::Arc;

/// Authentication: required. Aggregates one recorded civil day and seven-day
/// totals; only distinct application identities are decrypted for this view.
#[tauri::command]
pub async fn usage_get_day(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    usage: tauri::State<'_, Arc<UsageRuntime>>,
    date: String,
) -> Result<UsageDay, String> {
    check_auth_required(&credential_state)?;
    let credentials = credential_state.inner().clone();
    let storage = storage_state.inner().clone();
    let usage = usage.inner().clone();
    tokio::task::spawn_blocking(move || {
        check_auth_required(&credentials)?;
        let generation = storage.db_generation();
        let day = storage.usage_day(
            &date,
            usage.snapshot(),
            chrono::Utc::now().timestamp_millis(),
        )?;
        check_auth_required(&credentials)?;
        if generation != storage.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        Ok(day)
    })
    .await
    .map_err(|_| "USAGE_QUERY_FAILED")?
}

/// Authentication: required. Decrypts window/site details only for the selected
/// application and date; it never hydrates screenshot or OCR rows.
#[tauri::command]
pub async fn usage_get_app_details(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    usage: tauri::State<'_, Arc<UsageRuntime>>,
    date: String,
    app_id: String,
) -> Result<UsageAppDetails, String> {
    check_auth_required(&credential_state)?;
    let credentials = credential_state.inner().clone();
    let storage = storage_state.inner().clone();
    let usage = usage.inner().clone();
    tokio::task::spawn_blocking(move || {
        check_auth_required(&credentials)?;
        let generation = storage.db_generation();
        let details = storage.usage_app_details(
            &date,
            &app_id,
            usage.snapshot(),
            chrono::Utc::now().timestamp_millis(),
        )?;
        check_auth_required(&credentials)?;
        if generation != storage.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        Ok(details)
    })
    .await
    .map_err(|_| "USAGE_QUERY_FAILED")?
}
