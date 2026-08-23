//! Tauri commands for encrypted-storage maintenance, migration, and backup.
//!
//! Read-only startup probes run before login so the UI can decide which recovery
//! dialog to show. Any operation that mutates, exports, imports, or reveals user data
//! requires a valid authenticated session.

use crate::capture::CaptureState;
use crate::credential_manager::{get_cached_master_key, CredentialManagerState};
use crate::monitor::{start_monitor_impl, stop_monitor_impl, MonitorState};
use crate::storage::database_snapshot::{
    self, BACKUP_MANIFEST_FILE_NAME, BACKUP_MASTER_KEY_FILE_NAME, BACKUP_METADATA_FILE_NAME,
};
use crate::storage::StorageState;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::{password_hash::SaltString, Argon2};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use walkdir::WalkDir;
use zip::write::FileOptions;

#[derive(serde::Serialize)]
pub struct HmacMigrationStatus {
    pub needs_migration: bool,
    pub is_running: bool,
}

#[derive(serde::Serialize)]
pub struct StartupVacuumStatus {
    pub needs_vacuum: bool,
    pub in_progress: bool,
}

/// Reports whether HMAC records need migration and whether a migration is running.
///
/// Authentication: not required. Returns `{ "needs_migration": boolean,
/// "is_running": boolean }`. Frontend: `hooks/useHmacMigrationStatus.js` and
/// `components/HmacMigrationDialog.jsx`.
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

/// Reports whether startup database compaction is needed or already running.
///
/// Authentication: not required. Returns `{ "needs_vacuum": boolean,
/// "in_progress": boolean }`. Frontend: `components/StartupVacuumDialog.jsx`.
#[tauri::command]
pub async fn storage_get_startup_vacuum_status(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<StartupVacuumStatus, String> {
    let in_progress = state.is_startup_vacuum_in_progress();
    if in_progress {
        // Avoid touching the DB mutex while VACUUM holds it.
        return Ok(StartupVacuumStatus {
            needs_vacuum: true,
            in_progress,
        });
    }

    let state = state.inner().clone();
    let needs_vacuum = tokio::task::spawn_blocking(move || state.check_startup_vacuum_needed())
        .await
        .map_err(|error| format!("Startup VACUUM status task failed: {error}"))??;

    Ok(StartupVacuumStatus {
        needs_vacuum,
        in_progress,
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

/// Runs the one-time startup VACUUM when required.
///
/// Authentication: not required because it runs before the login flow and does not
/// expose records. Returns `{ "ran", "already_done", "already_running" }` booleans.
/// Frontend: `components/StartupVacuumDialog.jsx`.
#[tauri::command]
pub async fn storage_run_startup_vacuum_if_needed(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    let state = state.inner().clone();

    tokio::task::spawn_blocking(move || match state.run_startup_vacuum_if_needed() {
        Ok(ran) => Ok(serde_json::json!({
            "ran": ran,
            "already_done": !ran,
            "already_running": false
        })),
        Err(e) if e == "ALREADY_RUNNING" => Ok(serde_json::json!({
            "ran": false,
            "already_done": false,
            "already_running": true
        })),
        Err(e) => Err(e),
    })
    .await
    .map_err(|e| format!("VACUUM task panicked: {}", e))?
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
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;

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

/// Requests cancellation of the active HMAC migration.
///
/// Authentication: required. Returns `{ "status": "cancel_requested" | "idle",
/// "is_running": boolean }`.
#[tauri::command]
pub async fn storage_hmac_migration_cancel(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let in_progress = state.request_hmac_migration_cancel();
    Ok(serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "is_running": in_progress
    }))
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
    capture_state: tauri::State<'_, Arc<CaptureState>>,
    target: String,
    migrate_data_files: bool,
) -> Result<serde_json::Value, String> {
    super::check_auth_required(&credential_state)?;

    let was_running = monitor_state
        .process
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .is_some();
    monitor_state
        .migration_lock
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let office_runtime = app_handle
        .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
        .inner()
        .clone();
    let was_active = office_runtime.is_active();

    // Stop producers before acquiring the blocking maintenance gate. The gate
    // protects database connections; it is not a substitute for stopping a
    // monitor writer that can issue a new statement between copies.
    let _ = stop_monitor_impl(
        monitor_state.clone(),
        capture_state.clone(),
        app_handle.clone(),
    )
    .await;
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

    monitor_state
        .migration_lock
        .store(false, std::sync::atomic::Ordering::SeqCst);

    if was_running && storage_ready {
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(error) = start_monitor_impl(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!("Migration: Failed to restart monitor after data move: {error}");
        }
    }

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

/// Exports storage to a password-encrypted ZIP archive at `export_path`.
///
/// Authentication: required. `password` derives the backup key; returns JSON `null`
/// and emits `backup-migration-progress`. Frontend: `components/BackupMigrationDialog.jsx`.
#[tauri::command]
pub async fn storage_export_backup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<StorageState>>,
    monitor_state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    credential_state: State<'_, Arc<CredentialManagerState>>,
    password: String,
    export_path: String,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;

    tracing::info!("Migration: Starting data export to {}", export_path);

    let was_running = {
        let guard = monitor_state
            .process
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.is_some()
    };

    monitor_state
        .migration_lock
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let result = async {
        tracing::info!("Migration: Releasing resources (stopping monitor and storage)");
        let _ = stop_monitor_impl(
            monitor_state.clone(),
            capture_state.clone(),
            app_handle.clone(),
        )
        .await;
        app_handle
            .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
            .quiesce(std::time::Duration::from_secs(5))
            .await;
        let _database_maintenance = state.database_maintenance("backup_export");

        let result = (|| {
            // Capture the effective mode while the live connection still
            // exists.  Closing the last WAL connection can checkpoint sidecars.
            let journal_mode = state.prepare_database_snapshot_under_maintenance()?;
            let master_key = get_cached_master_key(&credential_state).ok_or_else(|| {
                "Master key not unlocked. Please verify Windows Hello first.".to_string()
            })?;
            let data_dir = state
                .data_dir
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let temporary_parent = data_dir.parent().unwrap_or(&data_dir);
            state.shutdown_under_maintenance()?;

            let export_snapshot = database_snapshot::create_database_snapshot(
                &data_dir,
                &journal_mode,
                temporary_parent,
            )?;
            let export_staging = export_snapshot.path();
            database_snapshot::copy_payload_tree(&data_dir, export_staging)?;

            let salt = SaltString::generate(&mut rand::thread_rng());
            let argon2 = Argon2::default();
            let mut derived_key = [0u8; 32];
            argon2
                .hash_password_into(
                    password.as_bytes(),
                    salt.as_str().as_bytes(),
                    &mut derived_key,
                )
                .map_err(|error| format!("Argon2 error: {error}"))?;
            let cipher = Aes256Gcm::new_from_slice(&derived_key)
                .map_err(|error| format!("AES error: {error}"))?;
            let mut nonce_bytes = [0u8; 12];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_bytes);
            let encrypted_master_key = cipher
                .encrypt(Nonce::from_slice(&nonce_bytes), master_key.as_slice())
                .map_err(|_| "Failed to encrypt backup master key".to_string())?;

            let metadata = serde_json::json!({
                "salt": salt.as_str(),
                "nonce": hex::encode(nonce_bytes),
            });
            std::fs::write(
                export_staging.join(BACKUP_METADATA_FILE_NAME),
                metadata.to_string(),
            )
            .map_err(|error| format!("Failed to stage backup metadata: {error}"))?;
            std::fs::write(
                export_staging.join(BACKUP_MASTER_KEY_FILE_NAME),
                encrypted_master_key,
            )
            .map_err(|error| format!("Failed to stage backup master key: {error}"))?;
            database_snapshot::write_manifest(
                &export_staging.join(BACKUP_MANIFEST_FILE_NAME),
                &export_snapshot.manifest,
            )?;

            let mut files_to_process = Vec::new();
            for entry in WalkDir::new(export_staging).follow_links(false).into_iter() {
                let entry = entry
                    .map_err(|error| format!("Failed to scan staged backup files: {error}"))?;
                if entry.file_type().is_symlink() {
                    return Err(format!(
                        "Symbolic link found in staged backup: {}",
                        entry.path().display()
                    ));
                }
                if entry.file_type().is_file() {
                    let relative = entry
                        .path()
                        .strip_prefix(export_staging)
                        .map_err(|error| format!("Failed to compute ZIP path: {error}"))?;
                    files_to_process.push((
                        entry.path().to_path_buf(),
                        relative.to_string_lossy().replace('\\', "/"),
                    ));
                }
            }
            files_to_process.sort_by(|left, right| left.1.cmp(&right.1));

            let total_files = files_to_process.len();
            let _ = app_handle.emit(
                "backup-migration-progress",
                serde_json::json!({
                    "total_files": total_files,
                    "copied_files": 0,
                    "current_file": "Preparing files...",
                }),
            );
            if let Some(parent) = Path::new(&export_path).parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "Failed to create export directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
            let file = File::create(&export_path)
                .map_err(|error| format!("Failed to create export file: {error}"))?;
            let mut zip = zip::ZipWriter::new(file);
            let options: FileOptions<'_, ()> =
                FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (index, (path, zip_name)) in files_to_process.iter().enumerate() {
                zip.start_file(zip_name, options)
                    .map_err(|error| format!("Failed to add {zip_name} to backup: {error}"))?;
                let mut input = File::open(path).map_err(|error| {
                    format!("Failed to read staged backup file {zip_name}: {error}")
                })?;
                std::io::copy(&mut input, &mut zip)
                    .map_err(|error| format!("Failed to write {zip_name} to backup: {error}"))?;
                if index == total_files - 1 || (index + 1) % 20 == 0 {
                    let _ = app_handle.emit(
                        "backup-migration-progress",
                        serde_json::json!({
                            "total_files": total_files,
                            "copied_files": index + 1,
                            "current_file": zip_name,
                        }),
                    );
                }
            }
            zip.finish()
                .map_err(|error| format!("Failed to finalize backup ZIP: {error}"))?;
            Ok::<(), String>(())
        })();

        tracing::info!("Migration: Re-initializing storage after export");
        let init_result = state.initialize_under_maintenance();
        (result, init_result)
    }
    .await;
    let (result, init_result) = result;

    monitor_state
        .migration_lock
        .store(false, std::sync::atomic::Ordering::SeqCst);

    if was_running && init_result.is_ok() {
        tracing::info!("Migration: Restarting monitor after export");
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(e) = start_monitor_impl(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!("Migration: Failed to restart monitor after export: {}", e);
        }
    }

    result.and(init_result)
}

/// Imports a password-encrypted backup ZIP from `backup_zip_path`.
///
/// Authentication: required. The backup master key is re-wrapped for the local Windows
/// credential before files are installed. Returns JSON `null` and emits progress events.
/// Frontend: `components/BackupMigrationDialog.jsx`.
#[tauri::command]
pub async fn storage_import_backup(
    app_handle: tauri::AppHandle,
    state: State<'_, Arc<StorageState>>,
    monitor_state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    credential_state: State<'_, Arc<CredentialManagerState>>,
    password: String,
    backup_zip_path: String,
) -> Result<(), String> {
    super::check_auth_required(&credential_state)?;

    tracing::info!("Migration: Starting data import from {}", backup_zip_path);

    let was_running = {
        let guard = monitor_state
            .process
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.is_some()
    };

    monitor_state
        .migration_lock
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let result = async {
        // 1. Prepare
        tracing::info!("Migration: Releasing resources for import");
        let _ = stop_monitor_impl(
            monitor_state.clone(),
            capture_state.clone(),
            app_handle.clone(),
        )
        .await;
        // The restored database numbers its screenshots independently, so an
        // association write that survived this point would attach a document
        // to whichever unrelated screenshot reuses the id.
        app_handle
            .state::<Arc<crate::office_runtime::OfficeRuntimeState>>()
            .quiesce(std::time::Duration::from_secs(5))
            .await;

        // Keep ANN sidecar publication and database replacement in one
        // lifecycle boundary. A builder may have captured the old database
        // generation before this import; disarming here invalidates its
        // publication token before the restored files are installed.
        let _derived_publish_guard = state.derived_generation_publish_guard();
        let _database_maintenance = state.database_maintenance("backup_import");
        app_handle
            .state::<Arc<crate::clip_ann::ClipAnnState>>()
            .disarm();
        let result = (|| {
            let data_dir = state
                .data_dir
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let temporary_parent = data_dir.parent().unwrap_or(&data_dir);
            let extracted = database_snapshot::extract_backup_archive(
                Path::new(&backup_zip_path),
                temporary_parent,
                |copied, total, name| {
                    let _ = app_handle.emit(
                        "backup-migration-progress",
                        serde_json::json!({
                            "total_files": total,
                            "copied_files": copied,
                            "current_file": name,
                        }),
                    );
                },
            )?;
            if extracted.legacy {
                tracing::info!("Migration: importing legacy DELETE-mode backup archive");
            }
            let metadata: serde_json::Value = serde_json::from_slice(&extracted.metadata)
                .map_err(|error| format!("Invalid metadata.json: {error}"))?;
            let salt_str = metadata["salt"]
                .as_str()
                .ok_or("salt missing in metadata")?;
            let nonce_hex = metadata["nonce"]
                .as_str()
                .ok_or("nonce missing in metadata")?;
            let nonce_bytes = hex::decode(nonce_hex).map_err(|e| e.to_string())?;
            if nonce_bytes.len() != 12 {
                return Err("Invalid nonce length in backup metadata".to_string());
            }
            let nonce = Nonce::from_slice(&nonce_bytes);

            // 4. Decrypt Master Key
            tracing::info!("Migration: Decrypting master key with provided password");
            let argon2 = Argon2::default();
            let mut derived_key = [0u8; 32];
            argon2
                .hash_password_into(password.as_bytes(), salt_str.as_bytes(), &mut derived_key)
                .map_err(|e| format!("Argon2 error: {}", e))?;

            let cipher =
                Aes256Gcm::new_from_slice(&derived_key).map_err(|e| format!("AES error: {}", e))?;
            let master_key = cipher
                .decrypt(nonce, extracted.encrypted_master_key.as_slice())
                .map_err(|_| "Incorrect password or corrupted backup".to_string())?;

            if master_key.len() != 32 {
                return Err("Invalid master key length in backup".to_string());
            }

            let database_key = state.database_key()?;
            database_snapshot::validate_database_snapshot(
                extracted.path(),
                &extracted.manifest,
                &database_key,
            )?;
            state.shutdown_under_maintenance()?;
            let credential_snapshot = credential_state.snapshot_import_state()?;
            let mut file_transaction =
                database_snapshot::RestoreFileTransaction::install(&data_dir, extracted.path())?;
            let apply_result = (|| {
                credential_state
                    .import_master_key(&master_key)
                    .map_err(|error| error.to_string())?;
                state.initialize_under_maintenance()?;
                Ok::<(), String>(())
            })();

            if let Err(error) = apply_result {
                let _ = state.shutdown_under_maintenance();
                let file_rollback = file_transaction.rollback();
                let credential_rollback =
                    credential_state.restore_import_state(credential_snapshot);
                let reopen = state.initialize_under_maintenance();
                let mut rollback_errors = Vec::new();
                if let Err(error) = file_rollback {
                    rollback_errors.push(error);
                }
                if let Err(error) = credential_rollback {
                    rollback_errors.push(error);
                }
                if let Err(error) = reopen {
                    rollback_errors.push(format!("failed to reopen previous database: {error}"));
                }
                return Err(if rollback_errors.is_empty() {
                    error
                } else {
                    format!("{error}; rollback failures: {}", rollback_errors.join("; "))
                });
            }
            if let Err(error) = file_transaction.commit() {
                tracing::warn!(
                    "Migration: restored data is active but rollback cleanup failed: {error}"
                );
            }
            Ok::<(), String>(())
        })();

        let init_result = if state.is_initialized() {
            Ok(())
        } else {
            state.initialize_under_maintenance()
        };
        if init_result.is_ok() {
            // A successful restore needs the replacement index rebuilt, while
            // a failed restore needs the rolled-back database re-armed.
            crate::clip_ann::spawn_startup_arm(app_handle.clone());
        }

        (result, init_result)
    }
    .await;
    let (result, init_result) = result;

    monitor_state
        .migration_lock
        .store(false, std::sync::atomic::Ordering::SeqCst);

    if was_running && init_result.is_ok() {
        tracing::info!("Migration: Restarting monitor after import");
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(e) = start_monitor_impl(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!("Migration: Failed to restart monitor after import: {}", e);
        }
    }

    result.and(init_result)
}
