//! Miscellaneous Tauri commands for application lifecycle and user preferences.
//!
//! Commands that can terminate the process, open local paths, or modify sensitive
//! runtime configuration validate the calling window and/or authenticated session.

use crate::{
    capture::CaptureState, monitor, monitor::MonitorState, registry_config, storage::StorageState,
    LightweightModeState, IS_QUITTING,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;

/// Forwards a frontend log message into the Rust tracing pipeline.
///
/// Authentication: not required. `level` selects the tracing level and `message` is
/// logged with a `Frontend:` prefix; returns JSON `null`.
#[tauri::command]
pub fn frontend_log(level: String, message: String) {
    match level.as_str() {
        "info" => tracing::info!("Frontend: {}", message),
        "warn" => tracing::warn!("Frontend: {}", message),
        "error" => tracing::error!("Frontend: {}", message),
        "debug" => tracing::debug!("Frontend: {}", message),
        "trace" => tracing::trace!("Frontend: {}", message),
        _ => tracing::info!("Frontend: {}", message),
    }
}

/// Returns the absolute CarbonPaper log-directory path as a JSON string.
///
/// Authentication: not required. Frontend: `hooks/useCriticalErrors.js`.
#[tauri::command]
pub fn get_log_dir() -> String {
    let data_dir = crate::get_data_dir();
    data_dir.join("logs").to_string_lossy().to_string()
}

/// Restarts the application process.
///
/// Authentication: main-window origin required. Returns only on failure.
/// Frontend: `hooks/useAppWindowState.js`.
#[tauri::command]
pub fn restart_app(app: tauri::AppHandle, window: tauri::Window) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    tauri::process::restart(&app.env())
}

/// Triggers a background panic to verify the critical-error UI.
///
/// Authentication: main-window origin and valid session required. Returns JSON `null`
/// after the panic is observed by the task join. Intended for diagnostics only.
#[tauri::command]
pub async fn trigger_test_error(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    let _ = tokio::task::spawn_blocking(|| {
        panic!("This is a test panic triggered from Rust!");
    })
    .await;
    Ok(())
}

/// Stops capture activity and exits the application cleanly.
///
/// Authentication: main-window origin required. Returns only if shutdown cannot proceed.
/// Frontend: `hooks/useAppWindowState.js`.
#[tauri::command]
pub async fn exit_app(
    app: tauri::AppHandle,
    window: tauri::Window,
    monitor_state: tauri::State<'_, MonitorState>,
    capture_state: tauri::State<'_, Arc<CaptureState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    IS_QUITTING.store(true, Ordering::Relaxed);
    monitor_state.stopping.store(true, Ordering::SeqCst);
    capture_state.stopped.store(true, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);
    if let Some(handle) = capture_state
        .capture_task
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        handle.abort();
    }
    capture_state.clear_wgc_session("app_exit_command");
    app.exit(0);
    Ok(())
}

/// Hides the main window while leaving background capture running.
///
/// Authentication: main-window origin required. Returns JSON `null`.
/// Frontend: `hooks/useAppWindowState.js`.
#[tauri::command]
pub fn hide_to_tray(window: tauri::Window) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::hide_main_window_to_tray(&window)
}

/// Applies the requested UI `language` to the tray and native surfaces.
///
/// Authentication: not required. Returns JSON `null`. Frontend: `i18n/index.js`.
#[tauri::command]
pub fn set_app_language(app: tauri::AppHandle, language: String) -> Result<(), String> {
    crate::set_app_language(&app, &language)
}

/// Terminates the process from an authorized security/recovery screen.
///
/// Authentication: main-window origin required. Returns only if termination fails.
#[tauri::command]
pub fn close_process(window: tauri::Window) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    std::process::exit(0);
}

/// Returns advanced runtime configuration as a JSON object.
///
/// Authentication: not required; the object contains preferences but no secrets.
/// Frontend: settings controllers.
#[tauri::command]
pub fn get_advanced_config() -> Result<serde_json::Value, String> {
    let cpu_limit_enabled = registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
    let cpu_limit_percent = registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
    let ocr_timeout_secs = registry_config::get_u32("ocr_timeout_secs").unwrap_or(120);
    let rust_ocr_dml_beta = registry_config::get_bool("rust_ocr_dml_beta").unwrap_or(false);
    let use_dml = registry_config::get_bool("use_dml").unwrap_or(false);
    let dml_device_id = registry_config::get_u32("dml_device_id").unwrap_or(0);
    let game_mode_enabled = registry_config::get_bool("game_mode_enabled").unwrap_or(true);
    let clustering_interval =
        registry_config::get_string("clustering_interval").unwrap_or_else(|| "1w".to_string());
    let clustering_enabled = registry_config::get_bool("clustering_enabled").unwrap_or(true);
    let classification_enabled =
        registry_config::get_bool("classification_enabled").unwrap_or(true);
    let smart_cluster_enabled = registry_config::get_bool("smart_cluster_enabled").unwrap_or(false);
    let clustering_allow_full_low_memory =
        registry_config::get_bool("clustering_allow_full_low_memory").unwrap_or(false);
    let network_enabled = registry_config::get_bool("network_enabled").unwrap_or(true);
    let use_onnx = registry_config::get_bool("use_onnx").unwrap_or(true);

    Ok(serde_json::json!({
        "cpu_limit_enabled": cpu_limit_enabled,
        "cpu_limit_percent": cpu_limit_percent,
        "ocr_timeout_secs": ocr_timeout_secs,
        "rust_ocr_dml_beta": rust_ocr_dml_beta,
        "use_dml": use_dml,
        "dml_device_id": dml_device_id,
        "game_mode_enabled": game_mode_enabled,
        "clustering_interval": clustering_interval,
        "clustering_enabled": clustering_enabled,
        "classification_enabled": classification_enabled,
        "smart_cluster_enabled": smart_cluster_enabled,
        "clustering_allow_full_low_memory": clustering_allow_full_low_memory,
        "network_enabled": network_enabled,
        "use_onnx": use_onnx,
    }))
}

/// Applies a partial advanced-configuration JSON object.
///
/// Authentication: required. Unknown keys are ignored; returns JSON `null`.
/// Frontend: settings controllers.
#[tauri::command]
pub fn set_advanced_config(
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    config: serde_json::Value,
) -> Result<(), String> {
    crate::commands::check_auth_required(&credential_state)?;
    if let Some(v) = config.get("cpu_limit_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("cpu_limit_enabled", v)?;
    }
    if let Some(v) = config.get("cpu_limit_percent").and_then(|v| v.as_u64()) {
        registry_config::set_u32("cpu_limit_percent", v as u32)?;
    }
    if let Some(v) = config.get("ocr_timeout_secs").and_then(|v| v.as_u64()) {
        let clamped = (v as u32).clamp(30, 600);
        registry_config::set_u32("ocr_timeout_secs", clamped)?;
    }
    if let Some(v) = config.get("rust_ocr_dml_beta").and_then(|v| v.as_bool()) {
        // Temporary migration setting. It intentionally does not mirror the
        // existing Python DML preference and will be removed when the Rust
        // runtime adopts the unified application DML configuration.
        registry_config::set_bool("rust_ocr_dml_beta", v)?;
    }
    if let Some(v) = config.get("use_dml").and_then(|v| v.as_bool()) {
        registry_config::set_bool("use_dml", v)?;
    }
    if let Some(v) = config.get("dml_device_id").and_then(|v| v.as_u64()) {
        registry_config::set_u32("dml_device_id", v as u32)?;
    }
    if let Some(v) = config.get("game_mode_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("game_mode_enabled", v)?;
    }
    if let Some(v) = config.get("clustering_interval").and_then(|v| v.as_str()) {
        registry_config::set_string("clustering_interval", v)?;
    }
    if let Some(v) = config.get("clustering_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("clustering_enabled", v)?;
    }
    if let Some(v) = config
        .get("classification_enabled")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("classification_enabled", v)?;
    }
    if let Some(v) = config
        .get("smart_cluster_enabled")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("smart_cluster_enabled", v)?;
    }
    if let Some(v) = config
        .get("clustering_allow_full_low_memory")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("clustering_allow_full_low_memory", v)?;
    }
    if let Some(v) = config.get("network_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("network_enabled", v)?;
    }
    if let Some(v) = config.get("use_onnx").and_then(|v| v.as_bool()) {
        registry_config::set_bool("use_onnx", v)?;
    }
    Ok(())
}

/// Enables or disables automatic game-mode resource suppression.
///
/// Authentication: required. `enabled` controls monitoring and may restart the monitor;
/// returns JSON `null`. Frontend: settings controllers.
#[tauri::command]
pub async fn toggle_game_mode(
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    crate::commands::check_auth_required(&credential_state)?;

    registry_config::set_bool("game_mode_enabled", enabled)?;
    if enabled {
        monitor::start_game_mode_monitor(app);
    } else {
        let state = app.state::<MonitorState>();
        let was_suppressed = state.game_mode_dml_suppressed.load(Ordering::SeqCst);
        monitor::stop_game_mode_monitor(&app);
        if was_suppressed {
            let _ = monitor::stop_monitor_impl(
                app.state::<MonitorState>(),
                app.state::<Arc<CaptureState>>(),
                app.clone(),
            )
            .await;
            let _ = monitor::start_monitor_impl(app.state::<MonitorState>(), app.clone()).await;
        }
    }
    Ok(())
}

/// Reports whether the browser-extension setup wizard should be shown.
///
/// Authentication: not required. Returns a JSON boolean.
#[tauri::command]
pub fn check_extension_setup_needed() -> Result<bool, String> {
    Ok(!registry_config::get_bool("extension_setup_done").unwrap_or(false))
}

/// Marks browser-extension setup as completed.
///
/// Authentication: not required. Returns JSON `null`.
#[tauri::command]
pub fn mark_extension_setup_done() -> Result<(), String> {
    registry_config::set_bool("extension_setup_done", true)
}

/// Reports whether clustering setup is needed for an existing screenshot database.
///
/// Authentication: not required. Returns a JSON boolean.
#[tauri::command]
pub async fn check_clustering_setup_needed(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    if registry_config::get_bool("clustering_setup_done").unwrap_or(false) {
        return Ok(false);
    }
    let count = state.count_screenshots_by_time_range(0.0, 9_999_999_999.0)?;
    Ok(count > 0)
}

/// Marks clustering setup as completed.
///
/// Authentication: not required. Returns JSON `null`.
#[tauri::command]
pub fn mark_clustering_setup_done() -> Result<(), String> {
    registry_config::set_bool("clustering_setup_done", true)
}

/// Smart cluster setup wizard — returns true if the wizard should be shown.
/// Returns false when either:
///   - The user previously permanently dismissed it, OR
///   - The model is already downloaded and the feature is configured
///
/// Authentication: not required. Returns a JSON boolean.
#[tauri::command]
pub fn check_smart_cluster_setup_needed() -> Result<bool, String> {
    if registry_config::get_bool("smart_cluster_setup_dismissed").unwrap_or(false) {
        return Ok(false);
    }
    if registry_config::get_bool("smart_cluster_setup_done").unwrap_or(false) {
        return Ok(false);
    }
    if registry_config::get_bool("smart_cluster_enabled").unwrap_or(false) {
        return Ok(false);
    }
    Ok(true)
}

/// Mark the smart cluster setup wizard as resolved.
/// If `dismissed_permanently` is true, the wizard will never re-appear on
/// future launches; the user can still trigger the download manually from
/// the settings page.
///
/// Authentication: not required. Returns JSON `null`.
/// Returns `{ "enabled": boolean }` for browser-extension enhancement.
///
/// Authentication: not required. Frontend: extension settings.
#[tauri::command]
pub fn mark_smart_cluster_setup_done(dismissed_permanently: bool) -> Result<(), String> {
    if dismissed_permanently {
        registry_config::set_bool("smart_cluster_setup_dismissed", true)?;
    } else {
        registry_config::set_bool("smart_cluster_setup_done", true)?;
    }
    Ok(())
}

/// Returns `{ "enabled": boolean }` for browser-extension enhancement.
///
/// Authentication: not required. Frontend: extension settings.
#[tauri::command]
pub fn get_extension_enhancement_config() -> Result<serde_json::Value, String> {
    let enabled = registry_config::get_bool("extension_enhanced_global").unwrap_or(false);
    Ok(serde_json::json!({
        "enabled": enabled,
    }))
}

/// Enables or disables browser-extension enhancement globally.
///
/// Authentication: required. Returns JSON `null`.
#[tauri::command]
pub fn set_extension_enhancement(
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    enabled: bool,
) -> Result<(), String> {
    crate::commands::check_auth_required(&credential_state)?;
    registry_config::set_bool("extension_enhanced_global", enabled)
}

/// One-time migration: the per-browser enhancement toggles
/// (extension_enhanced_chrome/edge) were replaced by a single global toggle.
/// If either old toggle was on, the user wanted enhancement — carry it over.
/// Called once from app setup.
pub fn migrate_extension_enhancement_config() {
    if registry_config::get_bool("extension_enhanced_global").is_some() {
        return; // already migrated (or set directly)
    }
    let chrome = registry_config::get_bool("extension_enhanced_chrome");
    let edge = registry_config::get_bool("extension_enhanced_edge");
    if chrome.is_none() && edge.is_none() {
        return; // fresh install, nothing to migrate
    }
    let enabled = migrated_enhancement_value(chrome, edge);
    if let Err(e) = registry_config::set_bool("extension_enhanced_global", enabled) {
        tracing::warn!("Failed to migrate extension enhancement toggle: {}", e);
        return;
    }
    let _ = registry_config::delete_value("extension_enhanced_chrome");
    let _ = registry_config::delete_value("extension_enhanced_edge");
    tracing::info!(
        "Migrated extension enhancement toggles (chrome={:?}, edge={:?}) -> global={}",
        chrome,
        edge,
        enabled
    );
}

/// Pure mapping from the old per-browser toggles to the new global one.
fn migrated_enhancement_value(chrome: Option<bool>, edge: Option<bool>) -> bool {
    chrome.unwrap_or(false) || edge.unwrap_or(false)
}

/// Live browser-extension sessions (NMH registrations), for the settings UI.
///
/// Authentication: not required. Returns the serialized session snapshot array.
#[tauri::command]
pub fn get_nmh_sessions() -> Result<serde_json::Value, String> {
    let sessions = crate::reverse_ipc::nmh_sessions_snapshot();
    serde_json::to_value(sessions).map_err(|e| e.to_string())
}

/// Returns current game-mode suppression and fullscreen-pause state.
///
/// Authentication: not required. Returns `{ "active", "permanent",
/// "fullscreen_paused" }` booleans.
#[tauri::command]
pub fn get_game_mode_status(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<MonitorState>();
    let active = state.game_mode_dml_suppressed.load(Ordering::SeqCst);
    let permanent = state
        .game_mode_permanently_suppressed
        .load(Ordering::SeqCst);
    let capture_state = app.state::<Arc<CaptureState>>();
    let fullscreen_paused = capture_state
        .game_mode_capture_paused
        .load(Ordering::SeqCst);
    serde_json::json!({
        "active": active,
        "permanent": permanent,
        "fullscreen_paused": fullscreen_paused,
    })
}

// Lightweight-mode commands.

/// Switches to lightweight mode by destroying the main window.
///
/// Authentication: not required. Returns JSON `null`.
#[tauri::command]
pub async fn switch_to_lightweight_mode(
    app: tauri::AppHandle,
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<(), String> {
    tracing::info!("Switching to lightweight mode");

    // Cancel any pending automatic mode switch.
    if let Some(timer) = lightweight_state.auto_switch_timer.lock().unwrap().take() {
        timer.abort();
    }

    // Destroy the main window while background services remain alive.
    if let Some(window) = app.get_webview_window("main") {
        window.destroy().map_err(|e| e.to_string())?;
        tracing::info!("Main window destroyed");
    }

    // Publish the new mode to shared state.
    *lightweight_state.is_lightweight.lock().unwrap() = true;

    // Notify the user because the main window is no longer visible.
    app.notification()
        .builder()
        .title("CarbonPaper")
        .body(crate::tray_text_lightweight_switched())
        .show()
        .ok();

    crate::refresh_tray_menu(&app);

    Ok(())
}

/// Switches to standard mode by recreating the main window.
///
/// Authentication: not required. Returns JSON `null`.
#[tauri::command]
pub async fn switch_to_standard_mode(
    app: tauri::AppHandle,
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<(), String> {
    tracing::info!("Switching to standard mode");

    // Cancel any pending automatic mode switch.
    if let Some(timer) = lightweight_state.auto_switch_timer.lock().unwrap().take() {
        timer.abort();
    }

    // Refuse to create a duplicate main window.
    if app.get_webview_window("main").is_some() {
        return Err("Window already exists".to_string());
    }

    // Recreate the standard UI.
    crate::create_main_window(&app).map_err(|e| e.to_string())?;

    // Publish the new mode to shared state.
    *lightweight_state.is_lightweight.lock().unwrap() = false;
    crate::refresh_tray_menu(&app);

    Ok(())
}

/// Returns whether the application is currently in lightweight mode.
///
/// Authentication: not required. Returns a JSON boolean.
#[tauri::command]
pub fn get_lightweight_status(
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<bool, String> {
    Ok(*lightweight_state.is_lightweight.lock().unwrap())
}

/// Returns lightweight-mode startup and auto-switch preferences.
///
/// Authentication: not required. Returns `start_with_window_hidden`,
/// `auto_lightweight_enabled`, and `auto_lightweight_delay_minutes`.
#[tauri::command]
pub fn get_lightweight_config() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "start_with_window_hidden": registry_config::get_bool("start_with_window_hidden").unwrap_or(false),
        "auto_lightweight_enabled": registry_config::get_bool("auto_lightweight_enabled").unwrap_or(false),
        "auto_lightweight_delay_minutes": registry_config::get_u32("auto_lightweight_delay_minutes").unwrap_or(5),
    }))
}

/// Applies a partial lightweight-mode configuration object.
///
/// Authentication: main-window origin and valid session required. Returns JSON `null`.
#[tauri::command]
pub fn set_lightweight_config(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    config: serde_json::Value,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;

    if let Some(start_hidden) = config
        .get("start_with_window_hidden")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("start_with_window_hidden", start_hidden)?;
    }

    if let Some(auto_enabled) = config
        .get("auto_lightweight_enabled")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("auto_lightweight_enabled", auto_enabled)?;
    }

    if let Some(delay) = config
        .get("auto_lightweight_delay_minutes")
        .and_then(|v| v.as_u64())
    {
        registry_config::set_u32("auto_lightweight_delay_minutes", delay as u32)?;
    }

    Ok(())
}

/// Opens a local directory or selects a local file in Windows Explorer.
///
/// Authentication: main-window origin and valid session required. `path` must already
/// exist; returns JSON `null`.
#[tauri::command]
pub fn open_path(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    path: String,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;

    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err("Path does not exist".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        let p_os = std::path::Path::new(&path);
        if p_os.is_file() {
            Command::new("explorer")
                .arg("/select,")
                .arg(p_os)
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            Command::new("explorer")
                .arg(p_os)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Fallback for macOS/Linux using open or xdg-open
        use std::process::Command;
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        Command::new(opener)
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::migrated_enhancement_value;

    #[test]
    fn test_migrated_enhancement_value() {
        assert!(migrated_enhancement_value(Some(true), None));
        assert!(migrated_enhancement_value(None, Some(true)));
        assert!(migrated_enhancement_value(Some(true), Some(false)));
        assert!(!migrated_enhancement_value(Some(false), Some(false)));
        assert!(!migrated_enhancement_value(None, None));
    }
}
