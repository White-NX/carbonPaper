use tauri::AppHandle;

#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE};
#[cfg(windows)]
use winreg::RegKey;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE_NAME: &str = "CarbonPaper";

#[cfg(windows)]
fn read_run_value() -> Result<Option<String>, String> {
    let read_key = |hive| -> Result<Option<String>, String> {
        let hive_key = RegKey::predef(hive);
        match hive_key.open_subkey_with_flags(RUN_KEY, KEY_READ) {
            Ok(subkey) => match subkey.get_value::<String, _>(RUN_VALUE_NAME) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("Failed to read autostart value: {}", e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to open autostart registry key: {}", e)),
        }
    };

    // Prefer HKLM; fall back to HKCU to detect user-level entries
    let machine = read_key(HKEY_LOCAL_MACHINE)?;
    if machine.is_some() {
        return Ok(machine);
    }
    read_key(HKEY_CURRENT_USER)
}

#[cfg(windows)]
fn set_autostart_windows(enabled: bool) -> Result<bool, String> {
    // 直接获取可执行文件完整路径，若失败再回退到 current_exe
    let exe_path_buf = std::env::current_exe()
        .map_err(|e| format!("Cannot get executable path: {}", e))?;
    let exe_path = exe_path_buf
        .to_string_lossy()
        .to_string()
        .replace('"', "");
    let run_value = format!("\"{}\" --autostart", exe_path);

    if enabled {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (subkey, _) = hkcu
            .create_subkey(RUN_KEY)
            .map_err(|e| format!("Failed to create/open autostart registry key: {}", e))?;
        subkey
            .set_value(RUN_VALUE_NAME, &run_value)
            .map_err(|e| format!("Failed to write autostart value: {}", e))?;
    } else {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
            Ok(subkey) => match subkey.delete_value(RUN_VALUE_NAME) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Failed to delete autostart value: {}", e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Failed to open autostart registry key: {}", e)),
        }
    }

    Ok(read_run_value()?.is_some())
}

#[tauri::command]
pub fn get_autostart_status() -> Result<bool, String> {
    #[cfg(windows)]
    {
        return read_run_value().map(|v| v.is_some());
    }

    #[cfg(not(windows))]
    {
        Err("Autostart management is only implemented for Windows".into())
    }
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let _ = app;
        return set_autostart_windows(enabled);
    }

    #[cfg(not(windows))]
    {
        let _ = enabled;
        let _ = app;
        Err("Autostart management is only implemented for Windows".into())
    }
}
