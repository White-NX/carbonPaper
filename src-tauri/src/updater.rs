use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use crate::registry_config;
use crate::resource_utils::{file_in_local_appdata, normalize_path_for_command};

const UPDATE_CHECK_URL: &str =
    "https://github.com/White-NX/carbonPaper/releases/latest/download/latest.json";
const UPDATE_SMOKE_TEST_ENV: &str = "CARBONPAPER_UPDATE_SMOKE_TEST";
const UPDATE_SMOKE_MANIFEST_URL_ENV: &str = "CARBONPAPER_UPDATE_MANIFEST_URL";
const UPDATE_SMOKE_RESULT_ENV: &str = "CARBONPAPER_UPDATE_SMOKE_RESULT";
const UPDATE_SMOKE_EXPECTED_VERSION_ENV: &str = "CARBONPAPER_UPDATE_SMOKE_EXPECTED_VERSION";
const UPDATE_SMOKE_EXPECTED_MANIFEST_VERSION_ENV: &str =
    "CARBONPAPER_UPDATE_SMOKE_EXPECTED_MANIFEST_VERSION";
const UPDATE_SMOKE_REQUIRE_APPLIED_ENV: &str = "CARBONPAPER_UPDATE_SMOKE_REQUIRE_APPLIED";
const UPDATE_SMOKE_APPLIED_ENV: &str = "CARBONPAPER_UPDATE_SMOKE_APPLIED";

/// Metadata about an available update (version, download URL, SHA256 hash, release notes).
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

fn verify_update_manifest_signature(manifest: &UpdateManifest) -> Result<(), String> {
    if is_update_smoke_test_enabled() {
        return Ok(());
    }

    let public_key_b64 = option_env!("CARBONPAPER_UPDATE_PUBLIC_KEY")
        .ok_or_else(|| "Update signature public key is not configured".to_string())?;
    let public_key = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .map_err(|e| format!("Invalid update public key encoding: {}", e))?;
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| "Update public key must be 32 bytes".to_string())?;
    verify_update_manifest_with_key(manifest, &public_key)
}

fn verify_update_manifest_with_key(
    manifest: &UpdateManifest,
    public_key: &[u8; 32],
) -> Result<(), String> {
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|e| format!("Invalid update public key: {}", e))?;

    let signature = base64::engine::general_purpose::STANDARD
        .decode(&manifest.signature)
        .map_err(|e| format!("Invalid update signature encoding: {}", e))?;
    let signature = Signature::from_slice(&signature)
        .map_err(|e| format!("Invalid update signature: {}", e))?;
    verifying_key
        .verify(manifest_signing_payload(manifest).as_bytes(), &signature)
        .map_err(|_| "Update manifest signature verification failed".to_string())
}

fn safe_update_entry_path(name: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    let path = std::path::Path::new(name);
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("Unsafe update archive path: {}", name));
    }
    Ok(path.to_path_buf())
}

/// Shared state for the update checker, caching the latest update manifest.
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

pub(crate) fn is_update_smoke_test_enabled() -> bool {
    std::env::var(UPDATE_SMOKE_TEST_ENV).ok().as_deref() == Some("1")
}

fn validate_loopback_update_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| format!("Invalid update smoke test URL '{}': {}", raw, e))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "Update smoke test URL must use http or https, got '{}'",
                scheme
            ))
        }
    }

    let host = url
        .host_str()
        .ok_or_else(|| "Update smoke test URL must include a host".to_string())?;
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]";
    if !is_loopback {
        return Err(format!(
            "Update smoke test URL must point at localhost, 127.0.0.1, or ::1, got '{}'",
            host
        ));
    }

    Ok(())
}

fn update_check_url() -> Result<String, String> {
    if is_update_smoke_test_enabled() {
        let url = std::env::var(UPDATE_SMOKE_MANIFEST_URL_ENV).map_err(|_| {
            format!(
                "{} must be set when {}=1",
                UPDATE_SMOKE_MANIFEST_URL_ENV, UPDATE_SMOKE_TEST_ENV
            )
        })?;
        validate_loopback_update_url(&url)?;
        return Ok(url);
    }

    Ok(UPDATE_CHECK_URL.to_string())
}

fn write_update_smoke_status(
    status: &str,
    phase: &str,
    current_version: &str,
    target_version: Option<&str>,
    error: Option<&str>,
) {
    let Ok(path) = std::env::var(UPDATE_SMOKE_RESULT_ENV) else {
        return;
    };
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let payload = serde_json::json!({
        "status": status,
        "phase": phase,
        "current_version": current_version,
        "target_version": target_version,
        "error": error,
        "pid": std::process::id(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    let serialized = serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|_| format!(r#"{{"status":"{}","phase":"{}"}}"#, status, phase));
    let _ = std::fs::write(path, serialized);
}

pub(crate) fn maybe_run_update_smoke_test(app: AppHandle) {
    if !is_update_smoke_test_enabled() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let current_version = app.config().version.clone().unwrap_or_default();
        let expected_version = std::env::var(UPDATE_SMOKE_EXPECTED_VERSION_ENV).ok();
        let expected_manifest_version = std::env::var(UPDATE_SMOKE_EXPECTED_MANIFEST_VERSION_ENV)
            .ok()
            .or_else(|| expected_version.clone());
        let require_applied = std::env::var(UPDATE_SMOKE_REQUIRE_APPLIED_ENV)
            .ok()
            .as_deref()
            == Some("1");
        let update_applied = std::env::var(UPDATE_SMOKE_APPLIED_ENV).ok().as_deref() == Some("1");

        if expected_version.as_deref() == Some(current_version.as_str())
            && (!require_applied || update_applied)
        {
            tracing::info!(
                "[UPDATE_SMOKE] expected version {} is running",
                current_version
            );
            write_update_smoke_status(
                "success",
                "updated_app_started",
                &current_version,
                expected_manifest_version.as_deref(),
                None,
            );
            app.exit(0);
            return;
        }

        tracing::info!(
            "[UPDATE_SMOKE] starting update smoke test current_version={} target_version={:?} final_app_version={:?} require_applied={}",
            current_version,
            expected_manifest_version,
            expected_version,
            require_applied
        );
        write_update_smoke_status(
            "running",
            "check_started",
            &current_version,
            expected_manifest_version.as_deref(),
            None,
        );

        let result = async {
            let check = updater_check(app.clone(), app.state::<UpdaterState>()).await?;
            if !check.available {
                return Err(format!(
                    "No update available for smoke test (current version {})",
                    check.current_version
                ));
            }
            if let Some(expected) = expected_manifest_version.as_deref() {
                if check.version.as_deref() != Some(expected) {
                    return Err(format!(
                        "Smoke test manifest points to {:?}, expected {}",
                        check.version, expected
                    ));
                }
            }

            write_update_smoke_status(
                "running",
                "download_started",
                &current_version,
                check.version.as_deref(),
                None,
            );
            updater_download_impl(app.clone(), &app.state::<UpdaterState>()).await?;

            write_update_smoke_status(
                "running",
                "extract_started",
                &current_version,
                check.version.as_deref(),
                None,
            );
            updater_extract_impl().await?;

            write_update_smoke_status(
                "running",
                "apply_started",
                &current_version,
                check.version.as_deref(),
                None,
            );
            updater_apply_impl(
                app.clone(),
                app.state::<crate::monitor::MonitorState>(),
                app.state::<std::sync::Arc<crate::capture::CaptureState>>(),
            )
            .await
        }
        .await;

        if let Err(e) = result {
            tracing::error!("[UPDATE_SMOKE] failed: {}", e);
            write_update_smoke_status(
                "failure",
                "failed",
                &current_version,
                expected_manifest_version.as_deref(),
                Some(&e),
            );
            app.exit(1);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        manifest_signing_payload, safe_update_entry_path, validate_loopback_update_url,
        verify_update_manifest_with_key, UpdateManifest,
    };
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn update_smoke_url_allows_loopback_hosts() {
        assert!(validate_loopback_update_url("http://127.0.0.1:8787/latest.json").is_ok());
        assert!(validate_loopback_update_url("http://localhost:8787/latest.json").is_ok());
        assert!(validate_loopback_update_url("http://[::1]:8787/latest.json").is_ok());
    }

    #[test]
    fn update_smoke_url_rejects_non_loopback_hosts() {
        assert!(validate_loopback_update_url("https://example.com/latest.json").is_err());
        assert!(validate_loopback_update_url("file:///tmp/latest.json").is_err());
    }

    #[test]
    fn signed_manifest_verification_rejects_tampering() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
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
        let signature = signing_key.sign(manifest_signing_payload(&manifest).as_bytes());
        manifest.signature = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

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
    fn update_archive_paths_reject_escape_and_absolute_names() {
        assert!(safe_update_entry_path("bin/carbonpaper.exe").is_ok());
        assert!(safe_update_entry_path("../outside.exe").is_err());
        assert!(safe_update_entry_path("/absolute.exe").is_err());
        assert!(safe_update_entry_path(r"C:\\Windows\\outside.exe").is_err());
    }
}

/// Get the staging directory for update files
fn staging_dir() -> Result<PathBuf, String> {
    let base = file_in_local_appdata().ok_or("Cannot resolve LOCALAPPDATA")?;
    let dir = base.join("update_staging");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create staging dir: {}", e))?;
    Ok(dir)
}

/// Simple semver "is newer" comparison (major.minor.patch)
fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let s = s.trim_start_matches('v');
        let parts: Vec<&str> = s.split('.').collect();
        let major = parts.first().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = parts.get(2).and_then(|p| p.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    parse(remote) > parse(current)
}

/// Result of checking for updates (whether one is available, version info).
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
    if !is_update_smoke_test_enabled()
        && !registry_config::get_bool("network_enabled").unwrap_or(true)
    {
        return Err("Network features are disabled".to_string());
    }
    let current_version = app.config().version.clone().unwrap_or_default();

    let response = reqwest::get(update_check_url()?)
        .await
        .map_err(|e| format!("Failed to fetch update manifest: {}", e))?
        .error_for_status()
        .map_err(|e| format!("Failed to fetch update manifest: {}", e))?;

    let manifest: UpdateManifest = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse update manifest: {}", e))?;
    verify_update_manifest_signature(&manifest)?;

    let available = is_newer(&manifest.version, &current_version);

    let mut is_critical = manifest.critical;
    if let Some(min_ver) = &manifest.min_version {
        if is_newer(min_ver, &current_version) {
            is_critical = true;
        }
    }

    let result = CheckResult {
        available,
        version: if available {
            Some(manifest.version.clone())
        } else {
            None
        },
        notes: if available {
            manifest.notes.clone()
        } else {
            None
        },
        current_version,
        critical: is_critical,
    };

    if available {
        *state.manifest.lock().unwrap_or_else(|e| e.into_inner()) = Some(manifest);
    }

    Ok(result)
}

async fn updater_download_impl(app: AppHandle, state: &UpdaterState) -> Result<(), String> {
    if !is_update_smoke_test_enabled()
        && !registry_config::get_bool("network_enabled").unwrap_or(true)
    {
        return Err("Network features are disabled".to_string());
    }
    let manifest = state
        .manifest
        .lock()
        .unwrap()
        .clone()
        .ok_or("No update manifest cached. Call updater_check first.")?;
    if is_update_smoke_test_enabled() {
        validate_loopback_update_url(&manifest.url)?;
    }

    let staging = staging_dir()?;
    let zip_path = staging.join("update.zip");

    // Download with progress
    let client = reqwest::Client::new();
    let response = client
        .get(&manifest.url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("Download failed: {}", e))?;

    let content_length = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;

    let mut file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("Failed to create zip file: {}", e))?;

    let mut hasher = Sha256::new();

    // Read the response body as a stream for real progress and memory efficiency
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Failed to read chunk: {}", e))?;
        file.write_all(&chunk)
            .map_err(|e| format!("Failed to write chunk: {}", e))?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        let _ = app.emit(
            "updater-download-progress",
            DownloadProgress {
                downloaded,
                content_length,
            },
        );
    }

    drop(file);

    // Verify SHA256
    let hash = format!("{:x}", hasher.finalize());
    if hash != manifest.sha256.to_lowercase() {
        let _ = std::fs::remove_file(&zip_path);
        return Err(format!(
            "SHA256 mismatch: expected {}, got {}",
            manifest.sha256, hash
        ));
    }

    tracing::info!("Update downloaded and verified: {}", zip_path.display());
    Ok(())
}

async fn updater_extract_impl() -> Result<(), String> {
    let staging = staging_dir()?;
    let zip_path = staging.join("update.zip");
    let extract_dir = staging.join("extracted");

    // Clean previous extraction
    if extract_dir.exists() {
        std::fs::remove_dir_all(&extract_dir)
            .map_err(|e| format!("Failed to clean extract dir: {}", e))?;
    }
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Failed to create extract dir: {}", e))?;

    // Extract zip
    let file = std::fs::File::open(&zip_path).map_err(|e| format!("Failed to open zip: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read zip archive: {}", e))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {}", e))?;

        if entry
            .unix_mode()
            .map(|mode| mode & 0o170000 == 0o120000)
            .unwrap_or(false)
        {
            return Err(format!(
                "Update archive contains a symbolic link: {}",
                entry.name()
            ));
        }

        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| format!("Unsafe update archive path: {}", entry.name()))?;
        let enclosed = safe_update_entry_path(enclosed.to_string_lossy().as_ref())?;
        let name = enclosed.to_string_lossy().to_string();
        let out_path = extract_dir.join(enclosed);

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed to create dir {}: {}", name, e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent dir: {}", e))?;
            }
            let mut out_file = std::fs::File::create(&out_path)
                .map_err(|e| format!("Failed to create file {}: {}", name, e))?;
            std::io::copy(&mut entry, &mut out_file)
                .map_err(|e| format!("Failed to extract {}: {}", name, e))?;
        }
    }

    tracing::info!("Update extracted to: {}", extract_dir.display());
    Ok(())
}

async fn updater_apply_impl(
    app: AppHandle,
    monitor_state: tauri::State<'_, crate::monitor::MonitorState>,
    capture_state: tauri::State<'_, std::sync::Arc<crate::capture::CaptureState>>,
) -> Result<(), String> {
    tracing::info!("Update apply: preparing update");

    let staging = staging_dir()?;
    let extract_dir = staging.join("extracted");

    if !extract_dir.exists() {
        return Err("Extracted update directory not found. Run updater_extract first.".to_string());
    }

    let current_exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?;
    let app_dir = current_exe.parent().ok_or("Failed to get app directory")?;

    let app_dir_str = normalize_path_for_command(app_dir);
    let extract_dir_str = normalize_path_for_command(&extract_dir);
    let staging_dir_str = normalize_path_for_command(&staging);
    let exe_name = current_exe
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    crate::IS_UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);

    tracing::info!("Update apply: stopping monitor before update");
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        crate::monitor::stop_monitor_impl(monitor_state, capture_state, app.clone()),
    )
    .await
    {
        Ok(Ok(message)) => tracing::info!("Update apply: monitor stopped: {}", message),
        Ok(Err(e)) => {
            tracing::warn!(
                "Update apply: monitor stop failed, continuing update: {}",
                e
            )
        }
        Err(_) => tracing::warn!("Update apply: monitor stop timed out, continuing update"),
    }

    // 4. Generate PowerShell update script
    let ps_script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$logFile = Join-Path $env:LOCALAPPDATA 'CarbonPaper\update_error.log'

try {{
    # Wait for the app to exit (up to 30 seconds)
    $timeout = 30
    $elapsed = 0
    while ($elapsed -lt $timeout) {{
        $proc = Get-Process -Name '{exe_name_no_ext}' -ErrorAction SilentlyContinue
        if (-not $proc) {{ break }}
        Start-Sleep -Seconds 1
        $elapsed++
    }}

    # Force-kill if still running
    $proc = Get-Process -Name '{exe_name_no_ext}' -ErrorAction SilentlyContinue
    if ($proc) {{
        $proc | Stop-Process -Force
        Start-Sleep -Seconds 2
    }}

    # Kill NMH processes (browser extension native messaging host)
    Stop-Process -Name 'carbonpaper-nmh' -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1

    # Test write access; if denied, re-launch with elevation
    $testFile = Join-Path '{app_dir}' '.update_test'
    try {{
        [IO.File]::WriteAllText($testFile, 'test')
        Remove-Item $testFile -Force
    }} catch {{
        # Re-launch self elevated
        $scriptPath = Join-Path '{staging_dir}' 'update.ps1'
        Start-Process powershell.exe -ArgumentList "-ExecutionPolicy Bypass -File `"$scriptPath`"" -Verb RunAs
        exit
    }}

    # Copy all files from extracted dir to app dir
    Copy-Item -Path '{extract_dir}\*' -Destination '{app_dir}' -Recurse -Force

    # Start the updated app
    $updatedExe = Join-Path '{app_dir}' '{exe_name}'
    if ($env:CARBONPAPER_UPDATE_SMOKE_TEST -eq '1') {{
        $env:CARBONPAPER_UPDATE_SMOKE_APPLIED = '1'
        Start-Process -FilePath $updatedExe -WindowStyle Hidden
    }} else {{
        Start-Process -FilePath $updatedExe
    }}

    # Cleanup staging directory
    Start-Sleep -Seconds 2
    Remove-Item -Path '{staging_dir}' -Recurse -Force -ErrorAction SilentlyContinue

}} catch {{
    $_ | Out-File $logFile -Append
    exit 1
}}
"#,
        exe_name_no_ext = exe_name.trim_end_matches(".exe"),
        app_dir = app_dir_str,
        extract_dir = extract_dir_str,
        staging_dir = staging_dir_str,
        exe_name = exe_name,
    );

    // Write the PowerShell script
    let ps_path = staging.join("update.ps1");
    tracing::info!("Update apply: writing script to {}", ps_path.display());
    std::fs::write(&ps_path, &ps_script)
        .map_err(|e| format!("Failed to write update script: {}", e))?;

    let ps_path_str = normalize_path_for_command(&ps_path);

    // 5. Spawn detached PowerShell process
    use std::process::Command;
    tracing::info!("Update apply: launching update script");
    Command::new("powershell.exe")
        .args([
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
            &ps_path_str,
        ])
        .spawn()
        .map_err(|e| format!("Failed to spawn update process: {}", e))?;

    // 6. Exit the app
    tracing::info!("Update apply: exiting app for update");
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
    monitor_state: tauri::State<'_, crate::monitor::MonitorState>,
    capture_state: tauri::State<'_, std::sync::Arc<crate::capture::CaptureState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    let _install_guard = state
        .install_lock
        .try_lock()
        .map_err(|_| "UPDATE_IN_PROGRESS".to_string())?;
    let _ = app.emit(
        "updater-phase",
        serde_json::json!({ "phase": "downloading" }),
    );
    updater_download_impl(app.clone(), &state).await?;
    let _ = app.emit(
        "updater-phase",
        serde_json::json!({ "phase": "extracting" }),
    );
    updater_extract_impl().await?;
    let _ = app.emit("updater-phase", serde_json::json!({ "phase": "applying" }));
    updater_apply_impl(app, monitor_state, capture_state).await
}
