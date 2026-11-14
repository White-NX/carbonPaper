//! Signed-update discovery, locked download verification, and native handoff.
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::registry_config;
use carbonpaper_app_bound::windows::{identity, update};

const UPDATE_CHECK_URL: &str =
    "https://github.com/White-NX/carbonPaper/releases/latest/download/latest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub signature: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub min_version: Option<String>,
}

fn manifest_signing_payload(manifest: &UpdateManifest) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        manifest.version,
        manifest.url,
        manifest.sha256.to_ascii_lowercase(),
        if manifest.critical { "1" } else { "0" },
        manifest.min_version.as_deref().unwrap_or("")
    )
}

/// Runtime environment variables never select update endpoints or trust roots.
fn update_signature_public_key_b64() -> Result<String, String> {
    option_env!("CARBONPAPER_UPDATE_PUBLIC_KEY")
        .map(str::to_string)
        .ok_or_else(|| "Update signature public key is not configured".to_string())
}

pub(crate) fn verify_update_manifest_signature(manifest: &UpdateManifest) -> Result<(), String> {
    let public_key = base64::engine::general_purpose::STANDARD
        .decode(update_signature_public_key_b64()?)
        .map_err(|e| format!("Invalid update public key encoding: {e}"))?;
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| "Update public key must be 32 bytes".to_string())?;
    verify_update_manifest_with_key(manifest, &public_key)
}

fn verify_update_manifest_with_key(
    manifest: &UpdateManifest,
    public_key: &[u8; 32],
) -> Result<(), String> {
    let key = VerifyingKey::from_bytes(public_key)
        .map_err(|e| format!("Invalid update public key: {e}"))?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(&manifest.signature)
        .map_err(|e| format!("Invalid update signature encoding: {e}"))?;
    let signature =
        Signature::from_slice(&signature).map_err(|e| format!("Invalid update signature: {e}"))?;
    key.verify(manifest_signing_payload(manifest).as_bytes(), &signature)
        .map_err(|_| "Update manifest signature verification failed".to_string())
}

/// Read-only release verification entry point. It never starts Tauri, accesses
/// credentials, changes the verification key, downloads, or installs an update.
pub fn verify_update_manifest_file(path: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Update manifest is too large".into());
    }
    let manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    verify_update_manifest_signature(&manifest)
}

fn safe_update_entry_path(name: &str) -> Result<PathBuf, String> {
    carbonpaper_app_bound::manifest::safe_relative_path(name)
        .map_err(|_| format!("Unsafe update archive path: {name}"))
}

pub struct UpdaterState {
    manifest: Mutex<Option<UpdateManifest>>,
    install_lock: tokio::sync::Mutex<()>,
}

impl UpdaterState {
    pub fn new() -> Self {
        Self {
            manifest: Mutex::new(None),
            install_lock: tokio::sync::Mutex::new(()),
        }
    }
}

fn staging_dir() -> Result<PathBuf, String> {
    let base = identity::local_app_data_directory()
        .map_err(|e| e.to_string())?
        // UAC execution locks can outlive the app. Keep their ancestors out of
        // its storage directory so a pending prompt cannot freeze data renames.
        .join("CarbonPaperUpdates");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    cleanup_previous_staging(&base);
    let directory = base.join(format!("update-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    Ok(directory)
}

fn cleanup_previous_staging(base: &Path) {
    let Ok(_paths) = update::DirectoryLocks::for_execution(base) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = name.strip_prefix("update-") else {
            continue;
        };
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let _ = update::remove_inactive_staging(&entry.path());
    }
}

fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let parts = s.trim_start_matches('v').split('.').collect::<Vec<_>>();
        (
            parts.first().and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0),
            parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0),
        )
    };
    parse(remote) > parse(current)
}

#[derive(Serialize)]
pub struct CheckResult {
    available: bool,
    version: Option<String>,
    notes: Option<String>,
    current_version: String,
    critical: bool,
}

#[derive(Clone, Serialize)]
struct DownloadProgress {
    downloaded: u64,
    content_length: u64,
}

#[tauri::command]
pub async fn updater_check(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<CheckResult, String> {
    if !registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("Network features are disabled".to_string());
    }
    let current_version = app.config().version.clone().unwrap_or_default();
    let manifest: UpdateManifest = reqwest::get(UPDATE_CHECK_URL)
        .await
        .map_err(|e| format!("Failed to fetch update manifest: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Failed to fetch update manifest: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse update manifest: {e}"))?;
    verify_update_manifest_signature(&manifest)?;
    let available = is_newer(&manifest.version, &current_version);
    let critical = manifest.critical
        || manifest
            .min_version
            .as_deref()
            .is_some_and(|version| is_newer(version, &current_version));
    let result = CheckResult {
        available,
        version: available.then(|| manifest.version.clone()),
        notes: available.then(|| manifest.notes.clone()).flatten(),
        current_version,
        critical,
    };
    if available {
        *state.manifest.lock().unwrap_or_else(|e| e.into_inner()) = Some(manifest);
    }
    Ok(result)
}

struct DownloadedUpdate {
    file: File,
    staging: PathBuf,
    _directories: update::DirectoryLocks,
}

fn verify_downloaded_file(file: &mut File, expected: &str) -> Result<(), String> {
    file.rewind().map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    if format!("{:x}", hash.finalize()) != expected.to_ascii_lowercase() {
        return Err("Update download hash verification failed".into());
    }
    file.rewind().map_err(|e| e.to_string())
}

async fn updater_download_impl(
    app: &AppHandle,
    manifest: &UpdateManifest,
) -> Result<DownloadedUpdate, String> {
    if !registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("Network features are disabled".to_string());
    }
    verify_update_manifest_signature(manifest)?;
    let staging = staging_dir()?;
    let mut directories = update::DirectoryLocks::default();
    directories.pin(&staging).map_err(|e| e.to_string())?;
    let path = staging.join("update.zip");
    let response = reqwest::Client::new()
        .get(&manifest.url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Download failed: {e}"))?;
    let content_length = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0)
        .open(&path)
        .map_err(|e| format!("Failed to create update file: {e}"))?;
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to read update: {e}"))?;
        if downloaded.saturating_add(chunk.len() as u64) > 2 * 1024 * 1024 * 1024 {
            return Err("Update archive exceeds the supported size".into());
        }
        output.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        let _ = app.emit(
            "updater-download-progress",
            DownloadProgress {
                downloaded,
                content_length,
            },
        );
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    // Reopen under a deny-write/delete lock, verify these actual disk bytes, and
    // retain this exact handle until extraction finishes.
    let mut file = update::locked_file(&path).map_err(|e| e.to_string())?;
    verify_downloaded_file(&mut file, &manifest.sha256)?;
    Ok(DownloadedUpdate {
        file,
        staging,
        _directories: directories,
    })
}

struct ExtractedUpdate {
    directory: PathBuf,
    helper_hash: String,
}

fn updater_extract_impl(download: &mut DownloadedUpdate) -> Result<ExtractedUpdate, String> {
    let directory = download.staging.join("extracted");
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    download
        ._directories
        .pin(&directory)
        .map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(&mut download.file)
        .map_err(|e| format!("Failed to read update archive: {e}"))?;
    archive
        .validate_header_consistency()
        .map_err(|e| format!("Inconsistent update ZIP headers: {e}"))?;
    let mut helper_hash = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?.into_owned();
        if entry.is_symlink() {
            return Err(format!("Update archive contains a symbolic link: {name}"));
        }
        // Directory names in ZIP files end with '/', unlike manifest paths.
        let relative = safe_update_entry_path(name.trim_end_matches('/'))?;
        let path = directory.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            download
                ._directories
                .pin(&path)
                .map_err(|e| e.to_string())?;
            continue;
        }
        let parent = path.parent().ok_or("Invalid update entry")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        download
            ._directories
            .pin(parent)
            .map_err(|e| e.to_string())?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .map_err(|e| format!("Failed to create update entry: {e}"))?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let length = entry.read(&mut buffer).map_err(|e| e.to_string())?;
            if length == 0 {
                break;
            }
            output
                .write_all(&buffer[..length])
                .map_err(|e| e.to_string())?;
            hash.update(&buffer[..length]);
        }
        output.sync_all().map_err(|e| e.to_string())?;
        if name == update::HELPER_NAME {
            helper_hash = Some(format!("{:x}", hash.finalize()));
        }
    }
    Ok(ExtractedUpdate {
        directory,
        helper_hash: helper_hash.ok_or("Update installer is missing")?,
    })
}

async fn stop_monitor(app: &AppHandle) {
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        crate::monitor::control(app, crate::monitor::MonitorAction::Stop),
    )
    .await;
}

struct OrdinaryInstallation {
    _paths: update::DirectoryLocks,
    elevate: bool,
    coordinator: update::CoordinatorContext,
}

fn ordinary_installation_access() -> Result<OrdinaryInstallation, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let coordinator = update::CoordinatorContext::new().map_err(|e| e.to_string())?;
    // An elevated app may be able to write Program Files while its restricted
    // coordinator cannot. Probe using the coordinator's actual permissions.
    let (paths, elevate) = coordinator
        .with_permissions(|| ordinary_installation_access_for(&current))
        .map_err(|e| e.to_string())??;
    Ok(OrdinaryInstallation {
        _paths: paths,
        elevate,
        coordinator,
    })
}

fn ordinary_installation_access_for(
    current: &Path,
) -> Result<(update::DirectoryLocks, bool), String> {
    if current
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| !name.eq_ignore_ascii_case("carbonpaper.exe"))
    {
        return Err("UPDATER_INSTALLER_REQUIRED".into());
    }
    let directory = current
        .parent()
        .ok_or("Application directory is unavailable")?;
    let mut locks = update::DirectoryLocks::default();
    locks
        .pin(directory)
        .map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
    // The native helper authenticates its caller against the installed release.
    // Copies without that signed package metadata need the official installer.
    let (release, _) = carbonpaper_app_bound::manifest::read_manifest(directory)
        .map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
    let mut executable =
        update::locked_file(current).map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
    identity::verify_file(
        &mut executable,
        release
            .files
            .get("carbonpaper.exe")
            .ok_or("UPDATER_INSTALLER_REQUIRED")?,
    )
    .map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
    let probe = directory.join(format!(
        ".carbonpaper-write-{:032x}",
        rand::random::<u128>()
    ));
    let probe_result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(
            windows::Win32::Storage::FileSystem::FILE_GENERIC_WRITE.0
                | windows::Win32::Storage::FileSystem::DELETE.0,
        )
        .custom_flags(windows::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE.0)
        .open(probe);
    let elevate = match probe_result {
        Ok(file) => {
            drop(file);
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            update::check_local_path(directory)
                .map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
            // Elevation is only useful for an existing administrator-owned
            // installation; mutable/custom ownership needs installer repair.
            identity::assert_ordinary_installation(directory)
                .map_err(|_| "UPDATER_INSTALLER_REQUIRED".to_string())?;
            true
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok((locks, elevate))
}

async fn updater_apply_impl(
    app: AppHandle,
    package: &ExtractedUpdate,
    version: &str,
    installation: Option<&OrdinaryInstallation>,
) -> Result<(), String> {
    let _runtime_control = crate::maintenance::runtime_control_guard()?;
    let Some(installation) = installation else {
        // The protected installer independently verifies the release manifest
        // and actual installed bytes. Cancellation leaves capture running.
        crate::app_bound::install_runtime(package.directory.clone(), false).await?;
        stop_monitor(&app).await;
        return crate::app_bound::schedule_restart(&app);
    };
    let elevate = installation.elevate;
    let _execution_paths =
        update::DirectoryLocks::for_execution(&package.directory).map_err(|e| e.to_string())?;
    if elevate {
        update::check_local_path(&package.directory).map_err(|e| e.to_string())?;
    }
    let helper = package.directory.join(update::HELPER_NAME);
    let mut executable = update::locked_file(&helper).map_err(|e| e.to_string())?;
    verify_downloaded_file(&mut executable, &package.helper_hash)?;
    let mut arguments = vec![
        "--apply-unprotected-update".into(),
        "--source".into(),
        package.directory.as_os_str().to_owned(),
        "--caller-pid".into(),
        std::process::id().to_string().into(),
        "--version".into(),
        version.into(),
    ];
    if elevate {
        arguments.push("--elevate".into());
    }
    let (mut child, pipe) = installation
        .coordinator
        .spawn_coordinator(
            &helper,
            &arguments,
            &package.directory,
            // A pending UAC prompt must retain the coordinator's executable locks,
            // even if the application closes or this command's future is dropped.
            !elevate,
        )
        .map_err(|e| format!("Failed to start update installer: {e}"))?;
    pipe.connect().await.map_err(|e| e.to_string())?;
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut response = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        reader.read_line(&mut response),
    )
    .await
    .map_err(|_| "Update preparation timed out".to_string())?
    .map_err(|e| e.to_string())?;
    if elevate && response.trim_end() == update::ELEVATING {
        response.clear();
        reader
            .read_line(&mut response)
            .await
            .map_err(|e| e.to_string())?;
    }
    if response.trim_end() == update::ELEVATION_CANCELLED {
        return Err(update::ELEVATION_CANCELLED.into());
    }
    if response.trim_end() != update::READY {
        return Err("Update preparation failed".into());
    }
    reader
        .get_mut()
        .write_all(b"COMMIT\n")
        .await
        .map_err(|e| e.to_string())?;
    reader.get_mut().flush().await.map_err(|e| e.to_string())?;
    // The child has its own locks and authenticated parent process handle. It
    // cannot apply the prepared update until this exact process has exited.
    // Detach only after the explicit commit decision; closing the process
    // handle on exit does not terminate the committed coordinator.
    child.detach();
    stop_monitor(&app).await;
    crate::IS_UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn updater_install(
    app: AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<
        '_,
        std::sync::Arc<crate::credential_manager::CredentialManagerState>,
    >,
    state: tauri::State<'_, UpdaterState>,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    let _install_guard = state
        .install_lock
        .try_lock()
        .map_err(|_| "UPDATE_IN_PROGRESS".to_string())?;
    let manifest = state
        .manifest
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or("No update manifest cached. Call updater_check first.")?;
    let current = app.config().version.clone().unwrap_or_default();
    if !is_newer(&manifest.version, &current) {
        return Err("Update version is not newer".into());
    }
    let protected = crate::app_bound::uses_protected_installation();
    let _application_paths = if protected {
        None
    } else {
        Some(ordinary_installation_access()?)
    };
    let _ = app.emit(
        "updater-phase",
        serde_json::json!({ "phase": "downloading" }),
    );
    let mut download = updater_download_impl(&app, &manifest).await?;
    let _ = app.emit(
        "updater-phase",
        serde_json::json!({ "phase": "extracting" }),
    );
    let package = updater_extract_impl(&mut download)?;
    let _ = app.emit("updater-phase", serde_json::json!({ "phase": "applying" }));
    updater_apply_impl(
        app,
        &package,
        &manifest.version,
        _application_paths.as_ref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn ordinary_updates_validate_package_metadata_before_testing_write_access() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("carbonpaper.exe");
        assert!(matches!(
            ordinary_installation_access_for(&current),
            Err(error) if error == "UPDATER_INSTALLER_REQUIRED"
        ));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn signed_manifest_verification_rejects_tampering() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut manifest = UpdateManifest {
            version: "1.2.3".into(),
            url: "https://github.com/White-NX/carbonPaper/releases/download/v1.2.3/app.zip".into(),
            sha256: "ab".repeat(32),
            signature: String::new(),
            notes: None,
            pub_date: None,
            critical: false,
            min_version: None,
        };
        manifest.signature = base64::engine::general_purpose::STANDARD.encode(
            signing_key
                .sign(manifest_signing_payload(&manifest).as_bytes())
                .to_bytes(),
        );
        assert!(verify_update_manifest_with_key(
            &manifest,
            &signing_key.verifying_key().to_bytes()
        )
        .is_ok());
        manifest.version = "1.2.4".into();
        assert!(verify_update_manifest_with_key(
            &manifest,
            &signing_key.verifying_key().to_bytes()
        )
        .is_err());
    }

    #[test]
    fn update_archive_paths_reject_escape_absolute_and_windows_aliases() {
        assert!(safe_update_entry_path("bin/carbonpaper.exe").is_ok());
        for name in [
            "../outside.exe",
            "/absolute.exe",
            r"C:\Windows\outside.exe",
            "a:stream",
            "a/CON.dll",
            "a./b",
        ] {
            assert!(safe_update_entry_path(name).is_err(), "{name}");
        }
    }

    #[test]
    fn download_verification_checks_disk_bytes_and_rewinds_the_same_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("update.zip");
        let expected = format!("{:x}", Sha256::digest(b"original"));
        std::fs::write(&path, b"replacement").unwrap();
        let mut file = update::locked_file(&path).unwrap();
        assert!(verify_downloaded_file(&mut file, &expected).is_err());
        drop(file);
        std::fs::write(&path, b"original").unwrap();
        let mut file = update::locked_file(&path).unwrap();
        verify_downloaded_file(&mut file, &expected).unwrap();
        assert_eq!(file.stream_position().unwrap(), 0);
        assert!(std::fs::write(&path, b"replacement").is_err());
        assert!(std::fs::rename(&path, directory.path().join("moved.zip")).is_err());
    }

    #[test]
    fn staging_cleanup_preserves_active_updates_and_unrelated_files() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join(format!("update-{:032x}", 1));
        let active = base.path().join(format!("update-{:032x}", 2));
        let unrelated = base.path().join("user-data");
        for path in [&old, &active, &unrelated] {
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("keep.txt"), b"data").unwrap();
        }
        let mut active_paths = update::DirectoryLocks::default();
        active_paths.pin(&active).unwrap();
        cleanup_previous_staging(base.path());
        assert!(!old.exists());
        assert!(active.join("keep.txt").exists());
        assert!(unrelated.join("keep.txt").exists());
    }
}
