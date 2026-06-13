use crate::capture::CaptureState;
use crate::credential_manager::{get_cached_master_key, CredentialManagerState};
use crate::monitor::{start_monitor, stop_monitor, MonitorState};
use crate::storage::StorageState;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::{password_hash::SaltString, Argon2};
use std::fs::File;
use std::io::{Read, Write};
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

    let needs_vacuum = state.check_startup_vacuum_needed()?;

    Ok(StartupVacuumStatus {
        needs_vacuum,
        in_progress,
    })
}

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

#[tauri::command]
pub async fn storage_run_manual_vacuum(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
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
        // 1. Release resources
        tracing::info!("Migration: Releasing resources (stopping monitor and storage)");
        let _ = stop_monitor(
            monitor_state.clone(),
            capture_state.clone(),
            app_handle.clone(),
        )
        .await;
        state.shutdown()?;

        // 2. Get Master Key
        let master_key = get_cached_master_key(&credential_state).ok_or_else(|| {
            "Master key not unlocked. Please verify Windows Hello first.".to_string()
        })?;

        // 3. Encrypt Master Key with Argon2 + AES-GCM
        tracing::info!("Migration: Deriving backup key and encrypting master key");
        let salt = SaltString::generate(&mut rand::thread_rng());
        let argon2 = Argon2::default();
        let mut derived_key = [0u8; 32];
        argon2
            .hash_password_into(
                password.as_bytes(),
                salt.as_str().as_bytes(),
                &mut derived_key,
            )
            .map_err(|e| format!("Argon2 error: {}", e))?;

        let cipher =
            Aes256Gcm::new_from_slice(&derived_key).map_err(|e| format!("AES error: {}", e))?;
        let mut nonce_bytes = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let encrypted_master_key = cipher
            .encrypt(nonce, master_key.as_slice())
            .map_err(|e| format!("Encryption error: {}", e))?;

        // 4. Create ZIP
        let file = File::create(&export_path)
            .map_err(|e| format!("Failed to create export file: {}", e))?;
        let mut zip = zip::ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        // metadata.json
        zip.start_file("metadata.json", options)
            .map_err(|e| e.to_string())?;
        let metadata = serde_json::json!({
            "salt": salt.as_str(),
            "nonce": hex::encode(nonce_bytes),
        });
        zip.write_all(metadata.to_string().as_bytes())
            .map_err(|e| e.to_string())?;

        // master_key.enc
        zip.start_file("master_key.enc", options)
            .map_err(|e| e.to_string())?;
        zip.write_all(&encrypted_master_key)
            .map_err(|e| e.to_string())?;

        // --- Optimized: Single Pass File Collection ---
        tracing::info!("Migration: Scanning data directory for files");
        let data_dir = state.data_dir.lock().unwrap().clone();
        let mut files_to_process = Vec::new();

        let db_path = data_dir.join("screenshots.db");
        if db_path.exists() {
            files_to_process.push((db_path, "screenshots.db".to_string()));
        }

        let chroma_dir = data_dir.join("chroma_db");
        if chroma_dir.exists() {
            for entry in WalkDir::new(&chroma_dir).into_iter().filter_map(|e| e.ok()) {
                if entry.path().is_file() {
                    if let Ok(name) = entry.path().strip_prefix(&data_dir) {
                        files_to_process.push((
                            entry.path().to_owned(),
                            name.to_string_lossy().replace('\\', "/"),
                        ));
                    }
                }
            }
        }

        let screenshot_dir = data_dir.join("screenshots");
        let thumbs_dir = screenshot_dir.join("thumbs");
        if screenshot_dir.exists() {
            for entry in WalkDir::new(&screenshot_dir)
                .into_iter()
                .filter_entry(|e| e.path() != thumbs_dir) // Skip thumbs directory
                .filter_map(|e| e.ok())
            {
                if entry.path().is_file() {
                    if let Ok(name) = entry.path().strip_prefix(&data_dir) {
                        files_to_process.push((
                            entry.path().to_owned(),
                            name.to_string_lossy().replace('\\', "/"),
                        ));
                    }
                }
            }
        }

        let total_files = files_to_process.len();
        tracing::info!("Migration: Found {} files to export", total_files);
        let mut copied_files = 0;
        let emit_progress = |copied: usize, name: &str| {
            let _ = app_handle.emit(
                "backup-migration-progress",
                serde_json::json!({
                    "total_files": total_files,
                    "copied_files": copied,
                    "current_file": name,
                }),
            );
        };

        emit_progress(0, "Preparing files...");

        for (path, zip_name) in files_to_process {
            zip.start_file(&zip_name, options)
                .map_err(|e| e.to_string())?;
            let mut f = File::open(&path).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut zip).map_err(|e| e.to_string())?;

            copied_files += 1;
            if copied_files % 20 == 0 || copied_files == total_files {
                tracing::info!(
                    "Migration: Exported {}/{} files (current: {})",
                    copied_files,
                    total_files,
                    zip_name
                );
                emit_progress(copied_files, &zip_name);
            }
        }

        zip.finish().map_err(|e| e.to_string())?;

        tracing::info!("Migration: Data export completed successfully");
        Ok::<(), String>(())
    }
    .await;

    // Always re-initialize storage after export attempt (success or failure)
    tracing::info!("Migration: Re-initializing storage");
    let init_result = state.initialize();
    if let Err(ref e) = init_result {
        tracing::error!(
            "Migration: Failed to re-initialize storage after export: {}",
            e
        );
    }

    monitor_state
        .migration_lock
        .store(false, std::sync::atomic::Ordering::SeqCst);

    if was_running && init_result.is_ok() {
        tracing::info!("Migration: Restarting monitor after export");
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(e) = start_monitor(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!("Migration: Failed to restart monitor after export: {}", e);
        }
    }

    result.and(init_result)
}

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
        let _ = stop_monitor(
            monitor_state.clone(),
            capture_state.clone(),
            app_handle.clone(),
        )
        .await;
        state.shutdown()?;

        // 2. Open ZIP
        let file = File::open(&backup_zip_path)
            .map_err(|e| format!("Failed to open backup file: {}", e))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Invalid ZIP: {}", e))?;

        // 3. Read metadata and encrypted master key
        let mut metadata_str = String::new();
        {
            let mut metadata_file = archive
                .by_name("metadata.json")
                .map_err(|_| "metadata.json missing in backup".to_string())?;
            metadata_file
                .read_to_string(&mut metadata_str)
                .map_err(|e| e.to_string())?;
        }
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_str).map_err(|e| e.to_string())?;

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

        let mut enc_master_key = Vec::new();
        {
            let mut enc_key_file = archive
                .by_name("master_key.enc")
                .map_err(|_| "master_key.enc missing in backup".to_string())?;
            enc_key_file
                .read_to_end(&mut enc_master_key)
                .map_err(|e| e.to_string())?;
        }

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
            .decrypt(nonce, enc_master_key.as_slice())
            .map_err(|_| "Incorrect password or corrupted backup".to_string())?;

        if master_key.len() != 32 {
            return Err("Invalid master key length in backup".to_string());
        }

        // 5. Re-encrypt with local CNG
        tracing::info!("Migration: Re-encrypting master key with local Windows Hello");
        credential_state
            .import_master_key(&master_key)
            .map_err(|e| e.to_string())?;

        // 6. Replace data files
        let data_dir = state.data_dir.lock().unwrap().clone();

        let total_files = archive.len();
        tracing::info!("Migration: Starting extraction of {} entries", total_files);
        let mut copied_files = 0;

        let emit_progress = |copied: usize, name: &str| {
            let _ = app_handle.emit(
                "backup-migration-progress",
                serde_json::json!({
                    "total_files": total_files,
                    "copied_files": copied,
                    "current_file": name,
                }),
            );
        };

        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            let outpath = match file.enclosed_name() {
                Some(path) => path.to_owned(),
                None => continue,
            };

            let zip_name = outpath.to_string_lossy().to_string();
            if zip_name == "metadata.json" || zip_name == "master_key.enc" {
                copied_files += 1;
                continue;
            }

            let full_path = data_dir.join(&outpath);

            if (*file.name()).ends_with('/') {
                std::fs::create_dir_all(&full_path).map_err(|e| e.to_string())?;
            } else {
                if let Some(p) = full_path.parent() {
                    if !p.exists() {
                        std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
                    }
                }
                let mut outfile = File::create(&full_path).map_err(|e| e.to_string())?;
                std::io::copy(&mut file, &mut outfile).map_err(|e| e.to_string())?;
            }

            copied_files += 1;
            if copied_files % 50 == 0 || copied_files == total_files {
                tracing::info!(
                    "Migration: Imported {}/{} entries (current: {})",
                    copied_files,
                    total_files,
                    zip_name
                );
                emit_progress(copied_files, &zip_name);
            }
        }

        tracing::info!(
            "Migration: Data import completed successfully. Application restart recommended."
        );
        Ok::<(), String>(())
    }
    .await;

    tracing::info!("Migration: Re-initializing storage after import");
    let init_result = state.initialize();
    if let Err(ref e) = init_result {
        tracing::error!(
            "Migration: Failed to re-initialize storage after import: {}",
            e
        );
    }

    monitor_state
        .migration_lock
        .store(false, std::sync::atomic::Ordering::SeqCst);

    if was_running && init_result.is_ok() {
        tracing::info!("Migration: Restarting monitor after import");
        let monitor_state_for_start = app_handle.state::<MonitorState>();
        if let Err(e) = start_monitor(monitor_state_for_start, app_handle.clone()).await {
            tracing::error!("Migration: Failed to restart monitor after import: {}", e);
        }
    }

    result.and(init_result)
}
