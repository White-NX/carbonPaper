//! 注册表配置管理模块
//!
//! 使用 HKEY_CURRENT_USER\Software\CarbonPaper 存储应用配置，无需管理员权限。

use winreg::enums::*;
use winreg::RegKey;

const SUBKEY: &str = r"Software\CarbonPaper";

/// 打开（或创建）配置注册表项
fn open_app_key(write: bool) -> Result<RegKey, String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if write {
        let (key, _) = hkcu
            .create_subkey(SUBKEY)
            .map_err(|e| format!("Failed to create registry key: {}", e))?;
        Ok(key)
    } else {
        hkcu.open_subkey(SUBKEY)
            .map_err(|e| format!("Failed to open registry key: {}", e))
    }
}

/// 读取字符串配置项，不存在则返回 None
pub fn get_string(name: &str) -> Option<String> {
    let key = open_app_key(false).ok()?;
    key.get_value::<String, _>(name).ok()
}

/// 写入字符串配置项
pub fn set_string(name: &str, value: &str) -> Result<(), String> {
    let key = open_app_key(true)?;
    key.set_value(name, &value)
        .map_err(|e| format!("Failed to set registry value '{}': {}", name, e))
}

/// 读取布尔配置项，不存在则返回 None
pub fn get_bool(name: &str) -> Option<bool> {
    let key = open_app_key(false).ok()?;
    let val: u32 = key.get_value(name).ok()?;
    Some(val != 0)
}

/// 写入布尔配置项（存储为 DWORD 0/1）
pub fn set_bool(name: &str, value: bool) -> Result<(), String> {
    let key = open_app_key(true)?;
    let dword: u32 = if value { 1 } else { 0 };
    key.set_value(name, &dword)
        .map_err(|e| format!("Failed to set registry value '{}': {}", name, e))
}

/// 读取 u32 配置项，不存在则返回 None
pub fn get_u32(name: &str) -> Option<u32> {
    let key = open_app_key(false).ok()?;
    key.get_value::<u32, _>(name).ok()
}

/// 写入 u32 配置项
pub fn set_u32(name: &str, value: u32) -> Result<(), String> {
    let key = open_app_key(true)?;
    key.set_value(name, &value)
        .map_err(|e| format!("Failed to set registry value '{}': {}", name, e))
}
