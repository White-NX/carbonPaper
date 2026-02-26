use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use crate::resource_utils::{file_in_local_appdata, normalize_path_for_command};

const UPDATE_CHECK_URL: &str =
    "https://github.com/White-NX/carbonPaper/releases/latest/download/latest.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
}

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

#[derive(Serialize)]
pub struct CheckResult {
    available: bool,
    version: Option<String>,
    notes: Option<String>,
    current_version: String,
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
    let current_version = app.config().version.clone().unwrap_or_default();

    let response = reqwest::get(UPDATE_CHECK_URL)
        .await
        .map_err(|e| format!("Failed to fetch update manifest: {}", e))?;

    let manifest: UpdateManifest = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse update manifest: {}", e))?;

    let available = is_newer(&manifest.version, &current_version);

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
    };

    if available {
        *state.manifest.lock().unwrap() = Some(manifest);
    }

    Ok(result)
}

#[tauri::command]
pub async fn updater_download(
    app: AppHandle,
    state: tauri::State<'_, UpdaterState>,
) -> Result<(), String> {
    let manifest = state
        .manifest
        .lock()
        .unwrap()
        .clone()
        .ok_or("No update manifest cached. Call updater_check first.")?;

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

    let stream = response;
    let mut hasher = Sha256::new();

    // Read the response body in chunks
    let bytes = stream
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Process in chunks for progress reporting
    let chunk_size = 256 * 1024; // 256 KB chunks
    for chunk in bytes.chunks(chunk_size) {
        file.write_all(chunk)
            .map_err(|e| format!("Failed to write chunk: {}", e))?;
        hasher.update(chunk);
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
    let file = std::fs::File::open(&zip_path)
        .map_err(|e| format!("Failed to open zip: {}", e))?;
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
    // 1. Stop the Python monitor
    let _ = crate::monitor::stop_monitor(monitor_state, capture_state).await;

    // 2. Set the updating flag so close is allowed
    crate::IS_UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);

    // 3. Resolve paths
    let staging = staging_dir()?;
    let extract_dir = staging.join("extracted");

    if !extract_dir.exists() {
        return Err("Extracted update directory not found. Run updater_extract first.".to_string());
    }

    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;
    let app_dir = current_exe
        .parent()
        .ok_or("Failed to get app directory")?;

    let app_dir_str = normalize_path_for_command(app_dir);
    let extract_dir_str = normalize_path_for_command(&extract_dir);
    let staging_dir_str = normalize_path_for_command(&staging);
    let exe_name = current_exe
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

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
    Start-Process -FilePath (Join-Path '{app_dir}' '{exe_name}')

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
    std::fs::write(&ps_path, &ps_script)
        .map_err(|e| format!("Failed to write update script: {}", e))?;

    let ps_path_str = normalize_path_for_command(&ps_path);

    // 5. Spawn detached PowerShell process
    use std::process::Command;
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
