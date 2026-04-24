use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;
use crate::{IS_UPDATING, registry_config, monitor, capture::CaptureState, monitor::MonitorState, storage::StorageState, LightweightModeState};

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

#[tauri::command]
pub fn get_log_dir() -> String {
    let data_dir = crate::get_data_dir();
    data_dir.join("logs").to_string_lossy().to_string()
}

#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    tauri::process::restart(&app.env());
}

#[tauri::command]
pub async fn trigger_test_error() {
    let _ = tokio::task::spawn_blocking(|| {
        panic!("This is a test panic triggered from Rust!");
    }).await;
}

#[tauri::command]
pub fn exit_app() {
    std::process::exit(1);
}

#[tauri::command]
pub fn set_updating_flag(updating: bool) {
    IS_UPDATING.store(updating, Ordering::Relaxed);
}

#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
pub fn close_process() {
    std::process::exit(0);
}

#[tauri::command]
pub fn get_advanced_config() -> Result<serde_json::Value, String> {
    let cpu_limit_enabled = registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
    let cpu_limit_percent = registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
    let capture_on_ocr_busy = registry_config::get_bool("capture_on_ocr_busy").unwrap_or(false);
    let ocr_queue_limit_enabled =
        registry_config::get_bool("ocr_queue_limit_enabled").unwrap_or(true);
    let ocr_queue_max_size = registry_config::get_u32("ocr_queue_max_size").unwrap_or(1);
    let use_dml = registry_config::get_bool("use_dml").unwrap_or(false);
    let dml_device_id = registry_config::get_u32("dml_device_id").unwrap_or(0);
    let game_mode_enabled = registry_config::get_bool("game_mode_enabled").unwrap_or(true);
    let clustering_interval = registry_config::get_string("clustering_interval").unwrap_or_else(|| "1w".to_string());

    Ok(serde_json::json!({
        "cpu_limit_enabled": cpu_limit_enabled,
        "cpu_limit_percent": cpu_limit_percent,
        "capture_on_ocr_busy": capture_on_ocr_busy,
        "ocr_queue_limit_enabled": ocr_queue_limit_enabled,
        "ocr_queue_max_size": ocr_queue_max_size,
        "use_dml": use_dml,
        "dml_device_id": dml_device_id,
        "game_mode_enabled": game_mode_enabled,
        "clustering_interval": clustering_interval,
    }))
}

#[tauri::command]
pub fn set_advanced_config(config: serde_json::Value) -> Result<(), String> {
    if let Some(v) = config.get("cpu_limit_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("cpu_limit_enabled", v)?;
    }
    if let Some(v) = config.get("cpu_limit_percent").and_then(|v| v.as_u64()) {
        registry_config::set_u32("cpu_limit_percent", v as u32)?;
    }
    if let Some(v) = config.get("capture_on_ocr_busy").and_then(|v| v.as_bool()) {
        registry_config::set_bool("capture_on_ocr_busy", v)?;
    }
    if let Some(v) = config
        .get("ocr_queue_limit_enabled")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("ocr_queue_limit_enabled", v)?;
    }
    if let Some(v) = config.get("ocr_queue_max_size").and_then(|v| v.as_u64()) {
        registry_config::set_u32("ocr_queue_max_size", v as u32)?;
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
    Ok(())
}

#[tauri::command]
pub async fn toggle_game_mode(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    registry_config::set_bool("game_mode_enabled", enabled)?;
    if enabled {
        monitor::start_game_mode_monitor(app);
    } else {
        let state = app.state::<MonitorState>();
        let was_suppressed = state
            .game_mode_dml_suppressed
            .load(Ordering::SeqCst);
        monitor::stop_game_mode_monitor(&app);
        if was_suppressed {
            let _ = monitor::stop_monitor(
                app.state::<MonitorState>(),
                app.state::<Arc<CaptureState>>(),
            )
            .await;
            let _ = monitor::start_monitor(app.state::<MonitorState>(), app.clone()).await;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn check_extension_setup_needed() -> Result<bool, String> {
    Ok(!registry_config::get_bool("extension_setup_done").unwrap_or(false))
}

#[tauri::command]
pub fn mark_extension_setup_done() -> Result<(), String> {
    registry_config::set_bool("extension_setup_done", true)
}

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

#[tauri::command]
pub fn mark_clustering_setup_done() -> Result<(), String> {
    registry_config::set_bool("clustering_setup_done", true)
}

#[tauri::command]
pub fn get_extension_enhancement_config() -> Result<serde_json::Value, String> {
    let chrome = registry_config::get_bool("extension_enhanced_chrome").unwrap_or(false);
    let edge = registry_config::get_bool("extension_enhanced_edge").unwrap_or(false);
    Ok(serde_json::json!({
        "chrome": chrome,
        "edge": edge,
    }))
}

#[tauri::command]
pub fn set_extension_enhancement(browser: String, enabled: bool) -> Result<(), String> {
    match browser.as_str() {
        "chrome" => registry_config::set_bool("extension_enhanced_chrome", enabled),
        "edge" => registry_config::set_bool("extension_enhanced_edge", enabled),
        _ => Err(format!("Unknown browser: {}", browser)),
    }
}

#[tauri::command]
pub fn get_game_mode_status(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<MonitorState>();
    let active = state
        .game_mode_dml_suppressed
        .load(Ordering::SeqCst);
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

// ==================== 轻量模式相关命令 ====================

/// 切换到轻量模式：销毁主窗口
#[tauri::command]
pub async fn switch_to_lightweight_mode(
    app: tauri::AppHandle,
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<(), String> {
    tracing::info!("Switching to lightweight mode");

    // 取消自动切换定时器（如果有）
    if let Some(timer) = lightweight_state.auto_switch_timer.lock().unwrap().take() {
        timer.abort();
    }

    // 销毁主窗口
    if let Some(window) = app.get_webview_window("main") {
        window.destroy().map_err(|e| e.to_string())?;
        tracing::info!("Main window destroyed");
    }

    // 标记为轻量模式
    *lightweight_state.is_lightweight.lock().unwrap() = true;

    // 发送通知
    app.notification()
        .builder()
        .title("CarbonPaper")
        .body("已切换到轻量模式，通过托盘菜单可重新打开界面")
        .show()
        .ok();

    Ok(())
}

/// 切换到标准模式：重建主窗口
#[tauri::command]
pub async fn switch_to_standard_mode(
    app: tauri::AppHandle,
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<(), String> {
    tracing::info!("Switching to standard mode");

    // 取消自动切换定时器（如果有）
    if let Some(timer) = lightweight_state.auto_switch_timer.lock().unwrap().take() {
        timer.abort();
    }

    // 检查窗口是否已存在
    if app.get_webview_window("main").is_some() {
        return Err("Window already exists".to_string());
    }

    // 重建窗口
    crate::create_main_window(&app).map_err(|e| e.to_string())?;

    // 标记为标准模式
    *lightweight_state.is_lightweight.lock().unwrap() = false;

    Ok(())
}

/// 获取当前轻量模式状态
#[tauri::command]
pub fn get_lightweight_status(
    lightweight_state: tauri::State<'_, Arc<LightweightModeState>>,
) -> Result<bool, String> {
    Ok(*lightweight_state.is_lightweight.lock().unwrap())
}

/// 获取轻量模式配置
#[tauri::command]
pub fn get_lightweight_config() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "start_with_window_hidden": registry_config::get_bool("start_with_window_hidden").unwrap_or(false),
        "auto_lightweight_enabled": registry_config::get_bool("auto_lightweight_enabled").unwrap_or(false),
        "auto_lightweight_delay_minutes": registry_config::get_u32("auto_lightweight_delay_minutes").unwrap_or(5),
    }))
}

/// 设置轻量模式配置
#[tauri::command]
pub fn set_lightweight_config(config: serde_json::Value) -> Result<(), String> {
    if let Some(start_hidden) = config.get("start_with_window_hidden").and_then(|v| v.as_bool()) {
        registry_config::set_bool("start_with_window_hidden", start_hidden)?;
    }

    if let Some(auto_enabled) = config.get("auto_lightweight_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("auto_lightweight_enabled", auto_enabled)?;
    }

    if let Some(delay) = config.get("auto_lightweight_delay_minutes").and_then(|v| v.as_u64()) {
        registry_config::set_u32("auto_lightweight_delay_minutes", delay as u32)?;
    }

    Ok(())
}
