//! Tauri commands for encrypted-storage maintenance, migration, and backup.
//!
//! Read-only startup probes run before login so the UI can decide which recovery
//! dialog to show. Any operation that mutates, exports, imports, or reveals user data
//! requires a valid authenticated session.

use crate::credential_manager::{get_cached_master_key, CredentialManagerState};
use crate::monitor::{control_in_maintenance, MonitorAction, MonitorState};
use crate::storage::StorageState;
use std::path::Path;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

mod hmac_startup;
pub(crate) use hmac_startup::spawn_auto_hmac_migration;
pub use hmac_startup::HmacMigrationRuntime;

#[derive(serde::Serialize)]
pub struct HmacMigrationStatus {
    pub needs_migration: bool,
    #[serde(flatten)]
    progress: hmac_startup::HmacProgress,
}

/// Reports whether HMAC records need migration and whether a migration is running.
///
/// Authentication: not required. Returns `{ "needs_migration": boolean,
/// "is_running": boolean }`. Frontend: `hooks/useHmacMigrationStatus.js` and
/// `components/HmacMigrationDialog.jsx`.
#[tauri::command]
pub async fn storage_check_hmac_migration_status(
    state: tauri::State<'_, Arc<StorageState>>,
    runtime: tauri::State<'_, Arc<HmacMigrationRuntime>>,
) -> Result<HmacMigrationStatus, String> {
    let state = state.inner().clone();
    let needs_migration = tokio::task::spawn_blocking(move || state.check_hmac_migration_status())
        .await
        .map_err(|error| error.to_string())??;

    Ok(HmacMigrationStatus {
        needs_migration,
        progress: runtime.snapshot(),
    })
}

/// Reports only whether a startup or manual VACUUM currently owns the database.
///
/// This is the settings-page probe. It is an atomic read and deliberately does
/// not inspect the startup sentinel or acquire the database mutex.
#[tauri::command]
pub fn storage_is_startup_vacuum_in_progress(state: tauri::State<'_, Arc<StorageState>>) -> bool {
    state.is_startup_vacuum_in_progress()
}

/// Runs an explicitly requested database VACUUM.
///
/// Authentication: required. Returns `{ "ok": boolean, "already_running": boolean }`.
/// Frontend: `components/settings/useAdvancedSectionController.js`.
#[tauri::command]
pub async fn storage_run_manual_vacuum(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let state = state.inner().clone();

    tokio::task::spawn_blocking(move || match state.run_manual_vacuum() {
        Ok(()) => Ok(serde_json::json!({
            "ok": true,
            "already_running": false
        })),
        Err(e) if e == "ALREADY_RUNNING" => Ok(serde_json::json!({
            "ok": false,
            "already_running": true
        })),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| format!("Manual VACUUM task panicked: {}", e))?
}

/// Starts HMAC migration and emits `hmac-migration-progress` events.
///
/// Authentication: required. Returns JSON `null` when complete and emits
/// `hmac-migration-complete`. Frontend: `components/HmacMigrationDialog.jsx`.
#[tauri::command]
pub async fn storage_run_hmac_migration(
    app_handle: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;
    hmac_startup::run_manual(app_handle).await
}

/// Lists screenshot files that still exist as plaintext paths.
///
/// Authentication: required. Returns a JSON array of path strings.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_list_plaintext_files(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    super::check_auth_required(&credential_state)?;

    state.list_plaintext_screenshots()
}

/// Encrypts legacy plaintext screenshot files in place.
///
/// Authentication: required. Returns `{ "total_files", "migrated", "skipped",
/// "errors" }`. Frontend: `lib/monitor_api.js`.
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

/// Moves storage to `target`, optionally including screenshot data files.
///
/// Authentication: required. `migrate_data_files` controls whether only configuration
/// or all data moves. Returns the migration result object and emits progress events.
/// Frontend: `components/settings/storage/useStorageMigration.js`.
#[tauri::command]
pub async fn storage_migrate_data_dir(
    app_handle: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    target: String,
    migrate_data_files: bool,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let Some(maintenance) = crate::maintenance::enter("migrate_data_dir") else {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };

    let was_running = monitor_state.is_running();

    let office_runtime = app_handle
        .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .inner()
        .clone();
    let was_active = office_runtime.is_active();

    // Stop producers before acquiring the blocking maintenance gate. The gate
    // protects database connections; it is not a substitute for stopping a
    // monitor writer that can issue a new statement between copies.
    let _ = control_in_maintenance(&app_handle, MonitorAction::Stop, &maintenance).await;
    office_runtime
        .quiesce(std::time::Duration::from_secs(5))
        .await;

    let state = state.inner().clone();
    let state_after_migration = Arc::clone(&state);
    let migration_app_handle = app_handle.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        state.migrate_data_dir_blocking(migration_app_handle, target, migrate_data_files)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e));
    let storage_ready = state_after_migration.is_initialized();

    // Resume Office observation only after the selected database is open again.
    if was_active && storage_ready {
        office_runtime.resume();
    }

    if was_running && storage_ready {
        if let Err(error) =
            control_in_maintenance(&app_handle, MonitorAction::Start, &maintenance).await
        {
            tracing::error!("Migration: Failed to restart monitor after data move: {error}");
        }
    }

    crate::monitor::wake_automation(&app_handle);
    spawn_auto_hmac_migration(app_handle);
    outcome?
}

/// Requests cancellation of an active data-directory migration.
///
/// Authentication: required. Returns `{ "status": "cancel_requested" | "idle",
/// "in_progress": boolean }`. Frontend: `components/settings/MigrationProgressDialog.jsx`.
#[tauri::command]
pub fn storage_migration_cancel(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let in_progress = state.request_migration_cancel();
    Ok(serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "in_progress": in_progress
    }))
}

/// Deletes legacy plaintext screenshot files after encrypted migration.
///
/// Authentication: required. Returns `{ "deleted": number }`.
/// Frontend: `lib/monitor_api.js`.
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

/// Exports a fully password-encrypted, portable backup at export_path.
/// Authentication: required. Emits backup-migration-progress and supports cancellation.
#[tauri::command]
pub async fn storage_export_backup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<StorageState>>,
    monitor_state: State<'_, MonitorState>,
    credential_state: State<'_, Arc<CredentialManagerState>>,
    password: String,
    export_path: String,
) -> Result<crate::storage::backup::BackupSummary, String> {
    super::check_auth_required(&credential_state)?;
    let started = std::time::Instant::now();
    let Some(maintenance) = crate::maintenance::enter("backup_export") else {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };
    let _operation = state.backup.begin();
    emit_backup_progress(&app_handle, "prepare", 0, 0);
    let was_running = monitor_state.is_running();
    let office_runtime = app_handle
        .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .inner()
        .clone();
    let was_active = office_runtime.is_active();
    control_in_maintenance(&app_handle, MonitorAction::Stop, &maintenance).await.map_err(|error| {
        tracing::error!(operation = "export", phase = "prepare", error = %error, "Backup failed to stop capture");
        error
    })?;
    office_runtime
        .quiesce(std::time::Duration::from_secs(5))
        .await;
    let worker_state = state.inner().clone();
    let credentials = credential_state.inner().clone();
    let mut events = BackupProgressEmitter::new(app_handle.clone());
    let password = zeroize::Zeroizing::new(password);
    let outcome = tokio::task::spawn_blocking(move || {
        let _database_maintenance = worker_state.database_maintenance("backup_export");
        let result = (|| {
            worker_state.backup.check()?;
            let master_key =
                zeroize::Zeroizing::new(get_cached_master_key(&credentials).ok_or_else(|| {
                    "AUTH_REQUIRED: Please verify Windows Hello before backing up".to_string()
                })?);
            let database_key = worker_state.database_key()?;
            let data_dir = worker_state
                .data_dir
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let snapshot = worker_state.create_backup_snapshot_under_maintenance(
                &database_key,
                data_dir.parent().unwrap_or(&data_dir),
                |done, total| events.emit("snapshot", done, total),
            )?;
            crate::storage::backup::write_portable(
                snapshot.path(),
                &data_dir,
                &credentials,
                &database_key,
                &master_key,
                &password,
                Path::new(&export_path),
                &worker_state.backup,
                |phase, done, total| events.emit(phase, done, total),
            )
        })();
        events.log_failure(&result);
        // Export retains the resident store, generation, caches and processing
        // authority. No initialization or recovery sweep is needed afterward.
        let initialize = if worker_state.is_initialized() {
            Ok(())
        } else {
            Err("Storage is unavailable after backup export".into())
        };
        (result, initialize)
    })
    .await;
    let (result, initialize) = match outcome {
        Ok(value) => value,
        Err(error) => (
            Err(format!("Backup export task failed: {error}")),
            if state.is_initialized() {
                Ok(())
            } else {
                Err("Storage is unavailable after backup export".into())
            },
        ),
    };
    if was_active && initialize.is_ok() {
        office_runtime.resume();
    }
    if was_running && initialize.is_ok() {
        if let Err(error) =
            control_in_maintenance(&app_handle, MonitorAction::Start, &maintenance).await
        {
            tracing::error!("Failed to restart monitor after backup export: {error}");
        }
    }
    crate::monitor::wake_automation(&app_handle);
    let result = result.and_then(|summary| initialize.map(|()| summary));
    match &result {
        Ok(summary) => tracing::info!(
            operation = "export",
            outcome = "success",
            elapsed_ms = started.elapsed().as_millis() as u64,
            missing_screenshots = summary.missing_screenshots,
            repaired_paths = summary.repaired_paths,
            "Backup completed"
        ),
        Err(error) if error.contains("BACKUP_CANCELLED") => tracing::info!(
            operation = "export",
            outcome = "cancelled",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Backup cancelled"
        ),
        Err(error) => tracing::error!(operation = "export", outcome = "failed", error = %error,
            elapsed_ms = started.elapsed().as_millis() as u64, "Backup failed"),
    }
    result
}

/// Imports a portable backup, or an old ZIP while its original CNG key is available.
/// Authentication: required. Conversion is completed and validated in staging.
#[tauri::command]
pub async fn storage_import_backup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<StorageState>>,
    monitor_state: State<'_, MonitorState>,
    credential_state: State<'_, Arc<CredentialManagerState>>,
    password: String,
    backup_zip_path: String,
) -> Result<crate::storage::backup::BackupSummary, String> {
    super::check_auth_required(&credential_state)?;
    let Some(maintenance) = crate::maintenance::enter("backup_import") else {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };
    let _operation = state.backup.begin();
    let was_running = monitor_state.is_running();
    let office_runtime = app_handle
        .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .inner()
        .clone();
    let was_active = office_runtime.is_active();
    control_in_maintenance(&app_handle, MonitorAction::Stop, &maintenance).await?;
    office_runtime
        .quiesce(std::time::Duration::from_secs(5))
        .await;
    app_handle
        .state::<Arc<crate::clip_ann::ClipAnnState>>()
        .disarm();
    let worker_state = state.inner().clone();
    let credentials = credential_state.inner().clone();
    let mut events = BackupProgressEmitter::new(app_handle.clone());
    let password = zeroize::Zeroizing::new(password);
    let outcome = tokio::task::spawn_blocking(move || {
        let _derived_publish_guard = worker_state.derived_generation_publish_guard();
        let _database_maintenance = worker_state.database_maintenance("backup_import");
        let data_dir = worker_state
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let database_key = worker_state.database_key();

        let result = (|| {
            let database_key = database_key.as_ref().map_err(Clone::clone)?;
            let prepared = crate::storage::backup::prepare_import(
                Path::new(&backup_zip_path),
                data_dir.parent().unwrap_or(&data_dir),
                &credentials,
                database_key,
                &password,
                &worker_state.backup,
                |phase, done, total| events.emit(phase, done, total),
            )?;
            crate::storage::backup::install_prepared(&worker_state, &prepared)?;
            Ok(crate::storage::backup::BackupSummary {
                missing_screenshots: prepared.extracted.manifest.missing_screenshots.len() as u64,
                repaired_paths: 0,
            })
        })();
        events.log_failure(&result);
        let initialize = if worker_state.is_initialized() {
            Ok(())
        } else {
            // install_prepared already reopens a successfully rolled-back
            // store. A closed store means recovery failed and must stay closed.
            Err("Backup recovery did not reopen storage".to_string())
        };
        (result, initialize)
    })
    .await;
    let (result, initialize) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let mut failure = format!("Backup import task failed: {error}");
            if let Err(error) = state.shutdown() {
                failure.push_str(&format!("; Database close failed: {error}"));
            }
            (
                Err(failure),
                Err("Backup recovery did not reopen storage".to_string()),
            )
        }
    };
    if was_active && initialize.is_ok() {
        office_runtime.resume();
    }
    if initialize.is_ok() {
        crate::clip_ann::spawn_startup_arm(app_handle.clone());
    }
    if was_running && initialize.is_ok() {
        if let Err(error) =
            control_in_maintenance(&app_handle, MonitorAction::Start, &maintenance).await
        {
            tracing::error!("Failed to restart monitor after backup import: {error}");
        }
    }
    crate::monitor::wake_automation(&app_handle);
    spawn_auto_hmac_migration(app_handle);
    let result = result.and_then(|summary| initialize.map(|()| summary));
    if let Err(error) = &result {
        if error.contains("BACKUP_CANCELLED") {
            tracing::info!(
                operation = "import",
                outcome = "cancelled",
                "Backup cancelled"
            );
        } else {
            tracing::error!(operation = "import", outcome = "failed", error = %error, "Backup failed");
        }
    }
    result
}

/// Cancels backup preparation while preserving the live database and credentials.
#[tauri::command]
pub fn storage_backup_cancel(
    credential_state: State<'_, Arc<CredentialManagerState>>,
    state: State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    super::check_auth_required(&credential_state)?;
    Ok(state.backup.request_cancel())
}

fn emit_backup_progress(app: &tauri::AppHandle, phase: &str, done: u64, total: u64) {
    let _ = app.emit(
        "backup-migration-progress",
        serde_json::json!({
            "phase": phase, "copied_files": done, "total_files": total,
        }),
    );
}

/// Bound UI traffic on large archives while retaining phase transitions and
/// final counts. Progress is observational; cancellation is checked separately
/// for every database step, record and archive block.
struct BackupProgressEmitter {
    app: tauri::AppHandle,
    phase: String,
    phase_started: std::time::Instant,
    last_emitted: std::time::Instant,
}

impl BackupProgressEmitter {
    fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            phase: "prepare".into(),
            phase_started: std::time::Instant::now(),
            last_emitted: std::time::Instant::now(),
        }
    }

    fn emit(&mut self, phase: &str, done: u64, total: u64) {
        let changed = self.phase != phase;
        if changed {
            self.phase = phase.into();
            self.phase_started = std::time::Instant::now();
            tracing::info!(phase, "Backup phase started");
        }
        if changed
            || (total > 0 && done >= total)
            || self.last_emitted.elapsed() >= std::time::Duration::from_millis(100)
        {
            emit_backup_progress(&self.app, phase, done, total);
            self.last_emitted = std::time::Instant::now();
        }
    }

    fn log_failure<T>(&self, result: &Result<T, String>) {
        if let Err(error) = result {
            if error.contains("BACKUP_CANCELLED") {
                tracing::info!(phase = %self.phase, elapsed_ms = self.phase_started.elapsed().as_millis() as u64,
                    outcome = "cancelled", "Backup phase cancelled");
            } else {
                tracing::error!(phase = %self.phase, elapsed_ms = self.phase_started.elapsed().as_millis() as u64,
                    outcome = "failed", error = %error, "Backup phase failed");
            }
        }
    }
}
