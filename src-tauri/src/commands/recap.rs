//! Authenticated recap API. Frontend: lib/recap_api.js.
use super::check_auth_required;
use crate::{
    ai::recap::{self, *},
    credential_manager::CredentialManagerState,
    storage::StorageState,
};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Authentication: required. API keys are omitted from the returned settings.
#[tauri::command]
pub async fn recap_get_settings(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<RecapSettings, String> {
    check_auth_required(&credential_state)?;
    Ok(storage_state
        .recap_read::<RecapSettings>("settings", "settings", None)?
        .unwrap_or_default()
        .with_app_language()
        .view())
}

/// Authentication: required. A missing screening key preserves the saved key.
#[tauri::command]
pub async fn recap_save_settings(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mut settings: RecapSettings,
) -> Result<RecapSettings, String> {
    check_auth_required(&credential_state)?;
    settings = settings.current_defaults().with_app_language();
    let generation = storage_state.db_generation();
    let old: RecapSettings = storage_state
        .recap_read("settings", "settings", None)?
        .unwrap_or_default();
    if settings.screening.api_key.is_none() {
        settings.screening.api_key = old.screening.api_key;
    }
    settings.screening.api_key = settings
        .screening
        .api_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    settings.screening.base_url = settings
        .screening
        .base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    settings.enabled_since = old.enabled_since.or_else(|| {
        settings
            .enabled
            .then(|| chrono::Local::now().date_naive().to_string())
    });
    settings.validate()?;
    app.state::<RecapRuntime>().cancel();
    storage_state.recap_write("settings", "settings", "", -1, generation, &settings)?;
    let _ = app.emit("recap-changed", ());
    Ok(settings.view())
}

/// Authentication: required. Returns saved source dates, newest first.
#[tauri::command]
pub async fn recap_list_days(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    check_auth_required(&credential_state)?;
    storage_state.recap_list_days()
}

/// Authentication: required. Invalidated source payloads are never returned.
#[tauri::command]
pub async fn recap_get_day(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    date: String,
    include_records: Option<bool>,
    include_attempts: Option<bool>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;
    tokio::task::spawn_blocking(move || {
        recap::read_day_view(
            &app,
            &date,
            include_records.unwrap_or(true),
            include_attempts.unwrap_or(true),
        )
    })
    .await
    .map_err(|_| "RECAP_WORKER_FAILED")?
}

/// Authentication: required. Pages the filtered, revision-checked source directory.
#[tauri::command]
pub async fn recap_get_records(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    date: String,
    batch_start_ms: Option<i64>,
    cursor: Option<String>,
) -> Result<RecapRecordPage, String> {
    check_auth_required(&credential_state)?;
    tokio::task::spawn_blocking(move || {
        recap::read_records(&app, &date, batch_start_ms, cursor.as_deref())
    })
    .await
    .map_err(|_| "RECAP_WORKER_FAILED")?
}

/// Authentication: required. Stream notifications only trigger this protected read.
#[tauri::command]
pub async fn recap_get_progress(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    date: String,
    include_attempts: Option<bool>,
) -> Result<Option<RecapProgress>, String> {
    check_auth_required(&credential_state)?;
    let mut progress = recap::read_progress(&app, &date)?;
    if !include_attempts.unwrap_or(true) {
        if let Some(progress) = progress.as_mut() {
            progress.attempts.clear();
        }
    }
    Ok(progress)
}

/// Authentication: required. Generates closed four-hour windows only.
#[tauri::command]
pub async fn recap_generate(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    date: String,
    force: Option<bool>,
) -> Result<RecapDay, String> {
    check_auth_required(&credential_state)?;
    recap::generate(app, date, force.unwrap_or(false), false).await
}

/// Authentication: not required. Only cancels an already-running recap.
#[tauri::command]
pub async fn recap_cancel(runtime: tauri::State<'_, RecapRuntime>) -> Result<bool, String> {
    Ok(runtime.cancel())
}

/// Authentication: required. Corrections are separate from generated payloads.
#[tauri::command]
pub async fn recap_correct(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    date: String,
    correction: Correction,
) -> Result<RecapDay, String> {
    check_auth_required(&credential_state)?;
    let generation = storage_state.db_generation();
    let day = recap::read_day(&app, &date)?;
    let mut history: CorrectionHistory = storage_state
        .recap_read("corrections", &date, None)?
        .unwrap_or_default();
    if matches!(correction, Correction::Undo) {
        history.current = history.undo.pop().ok_or("RECAP_NOTHING_TO_UNDO")?;
    } else {
        let previous = history.current.clone();
        let known = |id: &str| day.threads.iter().any(|t| t.id == id);
        let title = |s: String| -> Result<String, String> {
            let s = s.trim().to_string();
            if s.is_empty() || s.chars().count() > 120 {
                Err("RECAP_INVALID_CORRECTION".into())
            } else {
                Ok(s)
            }
        };
        match correction {
            Correction::Rename { task_id, title: t } => {
                if !known(&task_id) {
                    return Err("RECAP_INVALID_CORRECTION".into());
                }
                for a in day
                    .batches
                    .iter()
                    .flat_map(|b| &b.activities)
                    .filter(|a| a.task_id == task_id)
                {
                    for id in a
                        .member_ids
                        .iter()
                        .copied()
                        .chain(a.sources.iter().map(|s| s.id))
                    {
                        history.current.assignments.insert(id, task_id.clone());
                    }
                }
                history.current.names.insert(task_id, title(t)?);
            }
            Correction::Merge { from, into } => {
                if from == into || !known(&from) || !known(&into) {
                    return Err("RECAP_INVALID_CORRECTION".into());
                }
                let target = day.threads.iter().find(|t| t.id == into).unwrap();
                history
                    .current
                    .names
                    .insert(into.clone(), target.title.clone());
                for a in day
                    .batches
                    .iter()
                    .flat_map(|b| &b.activities)
                    .filter(|a| a.task_id == from || a.task_id == into)
                {
                    for id in a
                        .member_ids
                        .iter()
                        .copied()
                        .chain(a.sources.iter().map(|s| s.id))
                    {
                        history.current.assignments.insert(id, into.clone());
                    }
                }
                history.current.merges.insert(from, into);
            }
            Correction::Move {
                activity_id,
                task_id,
                title: t,
            } => {
                let a = day
                    .batches
                    .iter()
                    .flat_map(|b| &b.activities)
                    .find(|a| a.id == activity_id)
                    .ok_or("RECAP_INVALID_CORRECTION")?;
                let task = match task_id {
                    Some(id) if known(&id) => id,
                    Some(_) => return Err("RECAP_INVALID_CORRECTION".into()),
                    None => {
                        let id = format!("user-{}", hex::encode(rand::random::<[u8; 8]>()));
                        history
                            .current
                            .names
                            .insert(id.clone(), title(t.unwrap_or_default())?);
                        id
                    }
                };
                for id in a
                    .member_ids
                    .iter()
                    .copied()
                    .chain(a.sources.iter().map(|s| s.id))
                {
                    history.current.assignments.insert(id, task.clone());
                }
            }
            Correction::Undo => unreachable!(),
        }
        history.undo.push(previous);
        if history.undo.len() > 20 {
            history.undo.remove(0);
        }
    }
    // The storage trigger queues summary updates in the same transaction.
    storage_state.recap_write("corrections", &date, &date, -1, generation, &history)?;
    let _ = app.emit("recap-changed", &date);
    recap::read_day(&app, &date)
}
