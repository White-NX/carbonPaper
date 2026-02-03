use std::{env, path::PathBuf};
use tauri::{AppHandle, Manager};

/// Convert a Path to a String suitable for use in `Command`.
/// Strips the Windows extended-length prefix "\\\\?\\" if present.
pub fn normalize_path_for_command(path: &std::path::Path) -> String {
    let s = path.as_os_str().to_string_lossy().to_string();
    if s.starts_with("\\\\?\\") {
        s[4..].to_string()
    } else {
        s
    }
}

/// Construct a path under the app's resource directory by joining `filename` to the resource dir.
/// Note: this function does NOT check whether the file exists; it just returns the constructed path
/// if the resource directory can be retrieved.
pub fn file_in_resources(app: &AppHandle, filename: &str) -> Option<PathBuf> {
    match app.path().resource_dir() {
        Ok(dir) => Some(dir.join(filename)),
        Err(_) => None,
    }
}

/// Check whether a file or directory actually exists inside the app's resource directory.
/// Returns `Some(PathBuf)` only when the resource exists on disk.
pub fn find_existing_file_in_resources(app: &AppHandle, filename: &str) -> Option<PathBuf> {
    match app.path().resource_dir() {
        Ok(dir) => {
            let candidate = dir.join(filename);
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Returns the path to the current user's Local AppData\CarbonPaper directory on Windows.
/// Example: C:\Users\USERNAME\AppData\Local\CarbonPaper
pub fn file_in_local_appdata() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("CarbonPaper"))
}

/// Check whether a file or directory actually exists inside the user's Local AppData\CarbonPaper directory.
/// Returns `Some(PathBuf)` only when the file exists on disk.
pub fn find_existing_file_in_appdata(filename: &str) -> Option<PathBuf> {
    if let Some(appdata_dir) = file_in_local_appdata() {
        let candidate = appdata_dir.join(filename);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub fn get_log_path() -> PathBuf {
    env::temp_dir().join("carbonpaper_install.log")
}