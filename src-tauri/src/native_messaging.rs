//! Native Messaging host registration module
//!
//! Handles Chrome/Edge Native Messaging host manifest generation and
//! Windows Registry key management for NM host discovery.

use serde_json::json;
use std::path::PathBuf;

const NM_HOST_NAME: &str = "com.carbonpaper.nmh";
const CHROME_REG_KEY: &str = r"Software\Google\Chrome\NativeMessagingHosts\com.carbonpaper.nmh";
const EDGE_REG_KEY: &str = r"Software\Microsoft\Edge\NativeMessagingHosts\com.carbonpaper.nmh";
/// Stable extension ID derived from the fixed key in browser-extension/manifest.json
const EXTENSION_ID: &str = "pbghfcdpjkpeipjaffdfmocejaafbcfd";

/// Generate the NM host manifest JSON
fn generate_nm_manifest(exe_path: &str, extension_ids: &[&str]) -> serde_json::Value {
    let allowed_origins: Vec<String> = extension_ids
        .iter()
        .map(|id| format!("chrome-extension://{}/", id))
        .collect();

    json!({
        "name": NM_HOST_NAME,
        "description": "CarbonPaper Browser Extension Native Messaging Host",
        "path": exe_path,
        "type": "stdio",
        "allowed_origins": allowed_origins
    })
}

/// Get the path to the NMH executable
fn get_nmh_exe_path() -> Result<PathBuf, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;
    let exe_dir = current_exe.parent()
        .ok_or_else(|| "Failed to get exe directory".to_string())?;
    Ok(exe_dir.join("carbonpaper-nmh.exe"))
}

/// Get the fixed extension install directory (%LOCALAPPDATA%\carbonpaper\)
fn get_extension_install_dir() -> PathBuf {
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(local_appdata).join("carbonpaper")
}

/// Write the NM host manifest file to the fixed install directory
fn write_nm_manifest(extension_ids: &[&str]) -> Result<PathBuf, String> {
    let nmh_exe = get_nmh_exe_path()?;
    if !nmh_exe.exists() {
        return Err(format!("NMH executable not found at {:?}", nmh_exe));
    }

    let exe_path_str = nmh_exe.to_string_lossy().replace('/', "\\");
    let manifest = generate_nm_manifest(&exe_path_str, extension_ids);
    let manifest_str = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;

    let install_dir = get_extension_install_dir();
    let manifest_path = install_dir.join("nm_host_manifest.json");

    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("Failed to create install dir: {}", e))?;
    std::fs::write(&manifest_path, manifest_str)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    tracing::info!("NM host manifest written to {:?}", manifest_path);
    Ok(manifest_path)
}

/// Register NM host in Windows Registry for a specific browser
fn register_nm_host(reg_key_path: &str, manifest_path: &PathBuf) -> Result<(), String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(reg_key_path)
        .map_err(|e| format!("Failed to create registry key {}: {}", reg_key_path, e))?;

    let manifest_path_str = manifest_path.to_string_lossy().to_string();
    key.set_value("", &manifest_path_str)
        .map_err(|e| format!("Failed to set registry value: {}", e))?;

    tracing::info!("NM host registered at HKCU\\{}", reg_key_path);
    Ok(())
}

/// Unregister NM host from Windows Registry
fn unregister_nm_host(reg_key_path: &str) -> Result<(), String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.delete_subkey(reg_key_path) {
        Ok(_) => {
            tracing::info!("NM host unregistered from HKCU\\{}", reg_key_path);
            Ok(())
        }
        Err(e) => {
            // Not an error if key doesn't exist
            tracing::debug!("Registry key removal: {}", e);
            Ok(())
        }
    }
}

/// Check if NM host is registered for a browser
fn is_nm_host_registered(reg_key_path: &str) -> bool {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(reg_key_path).is_ok()
}

/// Copy browser extension files to a stable path in the install directory
fn copy_extension_to_data_dir() -> Result<PathBuf, String> {
    let install_dir = get_extension_install_dir();
    let dest_dir = install_dir.join("extension");

    let source_dir = find_extension_source_dir()?;

    // Copy files
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("Failed to create extension dir: {}", e))?;

    copy_dir_recursive(&source_dir, &dest_dir)?;

    tracing::info!("Extension files copied to {:?}", dest_dir);
    Ok(dest_dir)
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create dir {:?}: {}", dst, e))?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| format!("Failed to read dir {:?}: {}", src, e))?
    {
        let entry = entry.map_err(|e| format!("Dir entry error: {}", e))?;
        let file_type = entry.file_type().map_err(|e| format!("File type error: {}", e))?;
        let dest_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)
                .map_err(|e| format!("Failed to copy {:?}: {}", entry.path(), e))?;
        }
    }

    Ok(())
}

/// Find the browser-extension source directory (bundled with the app)
fn find_extension_source_dir() -> Result<std::path::PathBuf, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;
    let app_dir = current_exe.parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| "Failed to get app directory".to_string())?;

    let possible_sources = [
        app_dir.join("browser-extension"),
        current_exe.parent().unwrap().join("browser-extension"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("browser-extension"),
    ];

    possible_sources.iter()
        .find(|p| p.exists())
        .cloned()
        .ok_or_else(|| "Browser extension source directory not found".to_string())
}

/// Read the "version" field from a manifest.json file
fn read_manifest_version(dir: &std::path::Path) -> Option<String> {
    let manifest_path = dir.join("manifest.json");
    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("version").and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Sync the installed extension copy if the bundled source has a newer version.
/// Returns true if sync was performed, false if not needed.
pub fn sync_installed_extension() -> Result<bool, String> {
    let installed_dir = get_extension_install_dir().join("extension");

    // Only sync if user has previously installed the extension
    if !installed_dir.exists() {
        tracing::debug!("Extension not installed (directory missing), skipping sync");
        return Ok(false);
    }

    let source_dir = match find_extension_source_dir() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::debug!("Extension source not found, skipping sync: {}", e);
            return Ok(false);
        }
    };

    let source_version = read_manifest_version(&source_dir);
    let installed_version = read_manifest_version(&installed_dir);

    match (&source_version, &installed_version) {
        (Some(src_ver), Some(inst_ver)) if src_ver == inst_ver => {
            tracing::debug!("Extension already up-to-date (v{})", inst_ver);
            Ok(false)
        }
        (Some(src_ver), inst_ver_opt) => {
            tracing::info!(
                "Syncing extension: installed={} -> source={}",
                inst_ver_opt.as_deref().unwrap_or("unknown"),
                src_ver
            );
            copy_dir_recursive(&source_dir, &installed_dir)?;
            tracing::info!("Extension synced to v{}", src_ver);
            Ok(true)
        }
        (None, _) => {
            tracing::warn!("Source extension has no version in manifest, skipping sync");
            Ok(false)
        }
    }
}

// ==================== Tauri Commands ====================

#[tauri::command]
pub async fn get_nm_host_status() -> Result<serde_json::Value, String> {
    Ok(json!({
        "chrome": is_nm_host_registered(CHROME_REG_KEY),
        "edge": is_nm_host_registered(EDGE_REG_KEY),
    }))
}

#[tauri::command]
pub async fn register_nm_host_chrome() -> Result<(), String> {
    // Use wildcard for allowed_origins to support any extension ID during development
    let manifest_path = write_nm_manifest(&[EXTENSION_ID])?;
    register_nm_host(CHROME_REG_KEY, &manifest_path)
}

#[tauri::command]
pub async fn register_nm_host_edge() -> Result<(), String> {
    let manifest_path = write_nm_manifest(&[EXTENSION_ID])?;
    register_nm_host(EDGE_REG_KEY, &manifest_path)
}

#[tauri::command]
pub async fn install_browser_extension(browser: String) -> Result<serde_json::Value, String> {
    // 1. Copy extension files
    let ext_path = copy_extension_to_data_dir()?;

    // 2. Write manifest and register
    let manifest_path = write_nm_manifest(&[EXTENSION_ID])?;

    let (reg_key, extensions_url) = match browser.as_str() {
        "chrome" => (CHROME_REG_KEY, "chrome://extensions"),
        "edge" => (EDGE_REG_KEY, "edge://extensions"),
        _ => return Err(format!("Unsupported browser: {}", browser)),
    };

    register_nm_host(reg_key, &manifest_path)?;

    // 3. Open the browser extensions page using the browser executable directly
    //    open::that() can't handle chrome:// or edge:// protocol URLs
    open_browser_extensions_page(&browser, extensions_url);

    Ok(json!({
        "status": "success",
        "extension_path": ext_path.to_string_lossy(),
        "message": format!("Extension files installed. Please load the unpacked extension from: {}", ext_path.display())
    }))
}

/// Open the browser's extensions page by launching the browser executable directly.
/// Falls back to opening the extension folder if the browser isn't found.
fn open_browser_extensions_page(browser: &str, url: &str) {
    let exe_names: &[&str] = match browser {
        "edge" => &["msedge.exe", "msedge"],
        "chrome" => &["chrome.exe", "chrome"],
        _ => &[],
    };

    // Try to find the browser in common locations
    let program_files = std::env::var("ProgramFiles").unwrap_or_default();
    let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();

    let search_dirs: Vec<PathBuf> = match browser {
        "edge" => vec![
            PathBuf::from(&program_files_x86).join(r"Microsoft\Edge\Application"),
            PathBuf::from(&program_files).join(r"Microsoft\Edge\Application"),
            PathBuf::from(&local_appdata).join(r"Microsoft\Edge\Application"),
        ],
        "chrome" => vec![
            PathBuf::from(&program_files).join(r"Google\Chrome\Application"),
            PathBuf::from(&program_files_x86).join(r"Google\Chrome\Application"),
            PathBuf::from(&local_appdata).join(r"Google\Chrome\Application"),
        ],
        _ => vec![],
    };

    // Search for the browser executable
    for dir in &search_dirs {
        for exe in exe_names {
            let full_path = dir.join(exe);
            if full_path.exists() {
                match std::process::Command::new(&full_path).arg(url).spawn() {
                    Ok(_) => {
                        tracing::info!("Opened {} with {:?}", url, full_path);
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to launch {:?}: {}", full_path, e);
                    }
                }
            }
        }
    }

    // Fallback: try exe name directly (may be in PATH)
    for exe in exe_names {
        if let Ok(_) = std::process::Command::new(exe).arg(url).spawn() {
            tracing::info!("Opened {} via PATH with {}", url, exe);
            return;
        }
    }

    tracing::warn!("Could not find {} browser executable, skipping extensions page open", browser);
}

#[tauri::command]
pub async fn sync_extension_if_needed() -> Result<bool, String> {
    sync_installed_extension()
}
