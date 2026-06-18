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
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    #[serde(default)]
    pub critical: bool,
    #[serde(default)]
    pub min_version: Option<String>,
}

/// Shared state for the update checker, caching the latest update manifest.
pub struct UpdaterState {
    manifest: Mutex<Option<UpdateManifest>>,
}

impl UpdaterState {
    pub fn new() -> Self {
        Self {
            manifest: Mutex::new(None),
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
            updater_download(app.clone(), app.state::<UpdaterState>()).await?;

            write_update_smoke_status(
                "running",
                "extract_started",
                &current_version,
                check.version.as_deref(),
                None,
            );
            updater_extract().await?;

            write_update_smoke_status(
                "running",
                "apply_started",
                &current_version,
                check.version.as_deref(),
                None,
            );
            updater_apply(
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
    use super::validate_loopback_update_url;

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

#[tauri::command]
pub async fn updater_download(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<(), String> {
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

#[tauri::command]
pub async fn updater_extract() -> Result<(), String> {
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

        let name = entry.name().to_string();

        // Security: skip entries with path traversal
        if name.contains("..") {
            continue;
        }

        let out_path = extract_dir.join(&name);

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

#[tauri::command]
pub async fn updater_apply(
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
        crate::monitor::stop_monitor(monitor_state, capture_state, app.clone()),
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
