mod autostart;
mod analysis;
mod credential_manager;
mod logging;
mod monitor;
mod python;
mod registry_config;
mod resource_utils;
mod model_management;
mod reverse_ipc;
mod storage;
mod updater;

use autostart::{get_autostart_status, set_autostart};
use analysis::AnalysisState;
use credential_manager::CredentialManagerState;
use monitor::MonitorState;
use storage::StorageState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Emitter;
use tauri::Manager;
use window_vibrancy::apply_acrylic;

const MENU_ID_OPEN: &str = "open";
const MENU_ID_PAUSE: &str = "pause";
const MENU_ID_RESUME: &str = "resume";
const MENU_ID_RESTART: &str = "restart";
const MENU_ID_QUIT: &str = "quit";

static IS_UPDATING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn set_updating_flag(updating: bool) {
    IS_UPDATING.store(updating, Ordering::Relaxed);
}
fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();

    let menu = MenuBuilder::new(&app_handle)
        .item(&MenuItemBuilder::with_id(MENU_ID_OPEN, "打开界面").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_PAUSE, "暂停截图").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_RESUME, "恢复截图").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_RESTART, "重启截图").build(&app_handle)?)
        .separator()
        .item(&MenuItemBuilder::with_id(MENU_ID_QUIT, "彻底退出").build(&app_handle)?)
        .build()?;

    let mut tray_builder = TrayIconBuilder::new().menu(&menu);
    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    }
    tray_builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_ID_OPEN => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            MENU_ID_PAUSE => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let _ = monitor::pause_monitor(state).await;
                });
            }
            MENU_ID_RESUME => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let _ = monitor::resume_monitor(state).await;
                });
            }
            MENU_ID_RESTART => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let _ = monitor::stop_monitor(state).await;
                    let start_state = app_handle.state::<MonitorState>();
                    let _ = monitor::start_monitor(start_state, app_handle.clone()).await;
                });
            }
            MENU_ID_QUIT => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let _ = monitor::stop_monitor(state).await;
                    app_handle.exit(0);
                });
            }
            _ => {}
        })
        .build(&app_handle)?;

    Ok(())
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn close_process() {
    std::process::exit(0);
}

// ==================== 存储相关命令 ====================

/// 检查认证状态，如果未认证返回错误
fn check_auth_required(credential_state: &CredentialManagerState) -> Result<(), String> {
    if !credential_state.is_session_valid() {
        return Err("AUTH_REQUIRED".to_string());
    }
    Ok(())
}

#[tauri::command]
async fn storage_get_timeline(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    start_time: f64,
    end_time: f64,
    max_records: Option<i64>,
) -> Result<Vec<storage::ScreenshotRecord>, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    // 如果传入的是毫秒级时间戳，转换为秒
    let start_ts = if start_time > 10_000_000_000.0 { start_time / 1000.0 } else { start_time };
    let end_ts = if end_time > 10_000_000_000.0 { end_time / 1000.0 } else { end_time };

    state.get_screenshots_by_time_range_limited(start_ts, end_ts, max_records.or(Some(500)))
}

#[tauri::command]
async fn storage_search(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    query: String,
    limit: Option<i32>,
    offset: Option<i32>,
    fuzzy: Option<bool>,
    process_names: Option<Vec<String>>,
    start_time: Option<f64>,
    end_time: Option<f64>,
) -> Result<Vec<storage::SearchResult>, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    state.search_text(
        &query,
        limit.unwrap_or(20),
        offset.unwrap_or(0),
        fuzzy.unwrap_or(true),
        process_names,
        start_time,
        end_time,
    )
}

#[tauri::command]
async fn storage_get_image(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    tracing::debug!("id={:?}, path={:?}", id, path);

    let image_path = if let Some(id) = id {
        let record = state.get_screenshot_by_id(id)?;
        tracing::debug!("Found record: {:?}", record.as_ref().map(|r| &r.image_path));
        record.map(|r| r.image_path)
    } else {
        path
    };

    tracing::debug!("Final image_path={:?}", image_path);

    match image_path {
        Some(path) => {
            // 使用 StorageManager::read_image 读取加密图片
            match state.read_image(&path) {
                Ok((data, mime_type)) => {
                    tracing::debug!("Successfully read image, mime={}", mime_type);
                    Ok(serde_json::json!({
                        "status": "success",
                        "data": data,
                        "mime_type": mime_type
                    }))
                }
                Err(e) => {
                    tracing::error!("Failed to read image: {}", e);
                    Err(e)
                }
            }
        }
        None => {
            tracing::warn!("Image not found - no path available");
            Err("Image not found".to_string())
        }
    }
}

#[tauri::command]
async fn storage_get_screenshot_details(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: i64,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    let record = state.get_screenshot_by_id(id)?;
    let ocr_results = state.get_screenshot_ocr_results(id)?;
    
    Ok(serde_json::json!({
        "status": "success",
        "record": record,
        "ocr_results": ocr_results
    }))
}

#[tauri::command]
async fn storage_delete_screenshot(
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
) -> Result<serde_json::Value, String> {
    let image_hash = match state.get_screenshot_by_id(screenshot_id)? {
        Some(record) => Some(record.image_hash),
        None => None,
    };

    let deleted = state.delete_screenshot(screenshot_id)?;
    let mut vector_deleted: Option<i64> = None;

    if deleted {
        if let Some(hash) = image_hash {
            let payload = serde_json::json!({
                "command": "delete_screenshot",
                "screenshot_id": screenshot_id,
                "image_hash": hash
            });
            match monitor::execute_monitor_command(monitor_state, payload).await {
                Ok(resp) => {
                    vector_deleted = resp.get("vector_deleted").and_then(|v| v.as_i64());
                }
                Err(e) => {
                    tracing::error!("Vector delete failed: {}", e);
                }
            }
        }
    }
    Ok(serde_json::json!({
        "status": "success",
        "deleted": deleted,
        "vector_deleted": vector_deleted
    }))
}

#[tauri::command]
async fn storage_delete_by_time_range(
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    start_time: f64,
    end_time: f64,
) -> Result<serde_json::Value, String> {
    let start_ts = start_time / 1000.0;
    let end_ts = end_time / 1000.0;
    let image_hashes = match state.get_screenshots_by_time_range(start_ts, end_ts) {
        Ok(records) => records.into_iter().map(|r| r.image_hash).collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!("Failed to load hashes: {}", e);
            Vec::new()
        }
    };

    let deleted_count = state.delete_screenshots_by_time_range(start_time, end_time)?;
    let mut vector_deleted: Option<i64> = None;

    if !image_hashes.is_empty() {
        let payload = serde_json::json!({
            "command": "delete_by_time_range",
            "start_time": start_time,
            "end_time": end_time,
            "image_hashes": image_hashes
        });

        match monitor::execute_monitor_command(monitor_state, payload).await {
            Ok(resp) => {
                vector_deleted = resp.get("vector_deleted").and_then(|v| v.as_i64());
            }
            Err(e) => {
                tracing::error!("Vector delete failed: {}", e);
            }
        }
    }
    Ok(serde_json::json!({
        "status": "success",
        "deleted_count": deleted_count,
        "vector_deleted": vector_deleted
    }))
}

#[tauri::command]
async fn storage_list_processes(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<serde_json::Value>, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    let processes = state.list_distinct_processes()?;
    Ok(processes.into_iter().map(|(name, count)| {
        serde_json::json!({
            "process_name": name,
            "count": count
        })
    }).collect())
}

#[tauri::command]
async fn storage_save_screenshot(
    state: tauri::State<'_, Arc<StorageState>>,
    request: storage::SaveScreenshotRequest,
) -> Result<storage::SaveScreenshotResponse, String> {
    state.save_screenshot(&request)
}

#[tauri::command]
async fn storage_get_public_key(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    let key = state.get_public_key()?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &key))
}

// ==================== 存储策略（policy）命令 ====================

#[tauri::command]
async fn storage_set_policy(
    state: tauri::State<'_, Arc<StorageState>>,
    policy: serde_json::Value,
) -> Result<serde_json::Value, String> {
    state
        .save_policy(&policy)
        .map_err(|e| format!("Failed to save policy: {}", e))?;
    Ok(policy)
}

#[tauri::command]
async fn storage_get_policy(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    state.load_policy().map_err(|e| format!("Failed to load policy: {}", e))
}

#[tauri::command]
async fn storage_encrypt_for_chromadb(
    state: tauri::State<'_, Arc<StorageState>>,
    plaintext: String,
) -> Result<String, String> {
    state.encrypt_for_chromadb(&plaintext)
}

#[tauri::command]
async fn storage_decrypt_from_chromadb(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    encrypted: String,
) -> Result<String, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    state.decrypt_from_chromadb(&encrypted)
}

// ==================== 数据迁移命令 ====================

/// 列出所有未加密的明文截图文件
#[tauri::command]
async fn storage_list_plaintext_files(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    state.list_plaintext_screenshots()
}

/// 迁移所有明文截图文件（加密并删除原文件）
/// 需要认证
#[tauri::command]
async fn storage_migrate_plaintext(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<storage::MigrationResult, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    state.migrate_plaintext_screenshots()
}

/// 新：整体迁移 data 目录（copy + remove 实现将在 storage.rs 中完成）
#[tauri::command]
async fn storage_migrate_data_dir(
    state: tauri::State<'_, Arc<StorageState>>,
    app_handle: tauri::AppHandle,
    target: String,
    migrate_data_files: Option<bool>,
) -> Result<serde_json::Value, String> {
    // 调用 storage impl 的阻塞迁移方法，使用 spawn_blocking 避免阻塞 async runtime
    let state_clone = state.inner().clone();
    let app = app_handle.clone();
    let t = target.clone();
    let should_migrate = migrate_data_files.unwrap_or(true);
    let join_handle = tauri::async_runtime::spawn_blocking(move || {
        state_clone.migrate_data_dir_blocking(app, t, should_migrate)
    });

    match join_handle.await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Migration task join failed: {:?}", e)),
    }
}

/// 取消正在进行的迁移
#[tauri::command]
async fn storage_migration_cancel(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    let in_progress = state.request_migration_cancel();
    Ok(serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "in_progress": in_progress
    }))
}

/// 删除所有明文截图文件（不迁移，直接删除）
/// 需要认证
#[tauri::command]
async fn storage_delete_plaintext(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    let deleted = state.delete_plaintext_screenshots()?;
    Ok(serde_json::json!({
        "status": "success",
        "deleted_count": deleted
    }))
}

#[tauri::command]
fn get_advanced_config() -> Result<serde_json::Value, String> {
    let cpu_limit_enabled = registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
    let cpu_limit_percent = registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
    let capture_on_ocr_busy = registry_config::get_bool("capture_on_ocr_busy").unwrap_or(false);
    let ocr_queue_limit_enabled = registry_config::get_bool("ocr_queue_limit_enabled").unwrap_or(true);
    let ocr_queue_max_size = registry_config::get_u32("ocr_queue_max_size").unwrap_or(1);
    let use_dml = registry_config::get_bool("use_dml").unwrap_or(false);
    let dml_device_id = registry_config::get_u32("dml_device_id").unwrap_or(0);
    let game_mode_enabled = registry_config::get_bool("game_mode_enabled").unwrap_or(true);

    Ok(serde_json::json!({
        "cpu_limit_enabled": cpu_limit_enabled,
        "cpu_limit_percent": cpu_limit_percent,
        "capture_on_ocr_busy": capture_on_ocr_busy,
        "ocr_queue_limit_enabled": ocr_queue_limit_enabled,
        "ocr_queue_max_size": ocr_queue_max_size,
        "use_dml": use_dml,
        "dml_device_id": dml_device_id,
        "game_mode_enabled": game_mode_enabled,
    }))
}

#[tauri::command]
fn set_advanced_config(config: serde_json::Value) -> Result<(), String> {
    if let Some(v) = config.get("cpu_limit_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("cpu_limit_enabled", v)?;
    }
    if let Some(v) = config.get("cpu_limit_percent").and_then(|v| v.as_u64()) {
        registry_config::set_u32("cpu_limit_percent", v as u32)?;
    }
    if let Some(v) = config.get("capture_on_ocr_busy").and_then(|v| v.as_bool()) {
        registry_config::set_bool("capture_on_ocr_busy", v)?;
    }
    if let Some(v) = config.get("ocr_queue_limit_enabled").and_then(|v| v.as_bool()) {
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
    Ok(())
}

#[tauri::command]
async fn credential_initialize(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        // 尝试从文件加载公钥（已有安装，不触发任何弹窗）
        // load_public_key_from_file 内部会自动缓存公钥
        match credential_manager::load_public_key_from_file(&credential_state) {
            Ok(_) => {}
            Err(_) => {
                // 公钥文件不存在 → 首次安装，从 CNG 导出公钥
                let pk = credential_manager::export_or_get_public_key(&credential_state)
                    .map_err(|e| format!("Failed to initialize credentials: {}", e))?;

                credential_manager::save_public_key_to_file(&credential_state, &pk)
                    .map_err(|e| format!("Failed to save public key: {}", e))?;
            }
        };

        // 仅在首次使用时生成主密钥（不触发任何弹窗）
        // 已有主密钥文件时跳过，解锁由 credential_verify_user 负责
        credential_manager::ensure_master_key_created(&credential_state)
            .map_err(|e| format!("Failed to create master key: {}", e))?;

        // 初始化存储
        storage_state.initialize()?;

        Ok("Credentials initialized successfully".to_string())
    }
    
    #[cfg(not(windows))]
    {
        Err("Windows Hello is only available on Windows".to_string())
    }
}

#[tauri::command]
async fn credential_verify_user(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    #[cfg(windows)]
    {
        // 始终强制执行 CNG 私钥解密操作，触发"Windows 安全中心"对话框
        // 不使用缓存，确保每次解锁都需要用户输入凭据
        credential_manager::force_verify_and_unlock_master_key(&state)
            .map_err(|e| format!("Verification failed: {}", e))?;

        // 认证成功，更新会话时间
        state.update_auth_time();

        Ok(true)
    }

    #[cfg(not(windows))]
    {
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// 检查当前认证会话是否有效
#[tauri::command]
async fn credential_check_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    Ok(state.is_session_valid())
}

/// 使认证会话失效（手动锁定）
#[tauri::command]
async fn credential_lock_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    state.invalidate_session();
    Ok(())
}

/// 通知应用进入前台/后台（由前端调用）
#[tauri::command]
async fn credential_set_foreground(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    in_foreground: bool,
) -> Result<(), String> {
    state.set_foreground_state(in_foreground);
    Ok(())
}

/// 设置会话超时时间（秒），并尝试持久化到注册表
#[tauri::command]
async fn credential_set_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    timeout: i64,
) -> Result<(), String> {
    state.set_session_timeout(timeout);
    // 尝试写入注册表作为持久化机制
    if let Err(e) = crate::registry_config::set_string("session_timeout_secs", &timeout.to_string()) {
        tracing::error!("Failed to persist session_timeout_secs: {}", e);
        // 不要因为持久化失败而使设置失败——仍将会话超时应用于内存状态
    }
    Ok(())
}

/// 获取当前会话超时时间（秒）
#[tauri::command]
async fn credential_get_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<i64, String> {
    Ok(state.get_session_timeout())
}

#[tauri::command]
async fn toggle_game_mode(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    registry_config::set_bool("game_mode_enabled", enabled)?;
    if enabled {
        monitor::start_game_mode_monitor(app);
    } else {
        monitor::stop_game_mode_monitor(&app);
        // 如果 DML 被抑制了，需要重启 Python 恢复 DML
        let state = app.state::<MonitorState>();
        let was_suppressed = state.game_mode_dml_suppressed.load(std::sync::atomic::Ordering::SeqCst);
        if was_suppressed {
            let _ = monitor::stop_monitor(app.state::<MonitorState>()).await;
            let _ = monitor::start_monitor(app.state::<MonitorState>(), app.clone()).await;
        }
    }
    Ok(())
}

#[tauri::command]
fn check_dml_setup_needed() -> Result<bool, String> {
    Ok(!registry_config::get_bool("dml_setup_done").unwrap_or(false))
}

#[tauri::command]
fn mark_dml_setup_done() -> Result<(), String> {
    registry_config::set_bool("dml_setup_done", true)
}

#[tauri::command]
fn get_game_mode_status(
    app: tauri::AppHandle,
) -> serde_json::Value {
    let state = app.state::<MonitorState>();
    let active = state.game_mode_dml_suppressed.load(std::sync::atomic::Ordering::SeqCst);
    let permanent = state.game_mode_permanently_suppressed.load(std::sync::atomic::Ordering::SeqCst);
    serde_json::json!({
        "active": active,
        "permanent": permanent,
    })
}

fn get_data_dir() -> std::path::PathBuf {
    // 优先从注册表读取 data_dir（HKCU\Software\CarbonPaper）
    if let Some(dir) = registry_config::get_string("data_dir") {
        return std::path::PathBuf::from(dir);
    }

    // 兼容旧版：尝试从 config.json 迁移到注册表
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        dirs::data_local_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    });

    let cfg_path = std::path::PathBuf::from(&local_appdata).join("CarbonPaper").join("config.json");
    if cfg_path.exists() {
        if let Ok(s) = std::fs::read_to_string(&cfg_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(dd) = v.get("data_dir").and_then(|d| d.as_str()) {
                    // 写入注册表并删除旧配置文件
                    let _ = registry_config::set_string("data_dir", dd);
                    let _ = std::fs::remove_file(&cfg_path);
                    return std::path::PathBuf::from(dd);
                }
            }
        }
    }

    std::path::PathBuf::from(local_appdata).join("CarbonPaper").join("data")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 创建凭证管理器状态
    let data_dir = get_data_dir();
    let _log_guard = logging::init_logging(&data_dir);  // 最早初始化日志系统

    let credential_state = Arc::new(CredentialManagerState::new(data_dir.clone()));
    let storage_state = Arc::new(StorageState::new(data_dir.clone(), credential_state.clone()));
    
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState::new())
        .manage(AnalysisState::default())
        .manage(updater::UpdaterState::new())
        .manage(credential_state)
        .manage(storage_state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if !IS_UPDATING.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.hide();
                    let _ = window.app_handle().emit("app-hidden", ());
                }
            }
        })
        .setup({
            let data_dir = data_dir.clone();
            move |app| {
            build_tray(app)?;

            // 应用亚克力磨砂透明效果
            if let Some(window) = app.get_webview_window("main") {
                let _ = apply_acrylic(&window, Some((0, 0, 0, 0)));
            }

            analysis::start_memory_sampler(app.handle().clone());
            logging::spawn_maintenance_task(data_dir.clone());
            
            // 初始化凭据管理器（加载公钥或首次创建）
            let credential_state = app.state::<Arc<CredentialManagerState>>();

            // 公钥用于弱数据库加密与行级封装
            let public_key_ready = match credential_manager::load_public_key_from_file(&credential_state) {
                Ok(public_key) => {
                    tracing::info!("Public key loaded from file, length: {}", public_key.len());
                    true
                }
                Err(credential_manager::CredentialError::KeyNotFound) => {
                    tracing::info!("Public key file missing, exporting from CNG...");

                    match credential_manager::export_or_get_public_key(&credential_state) {
                        Ok(public_key) => {
                            tracing::info!("CNG public key exported, length: {}", public_key.len());
                            if let Err(e) = credential_manager::save_public_key_to_file(&credential_state, &public_key) {
                                tracing::error!("Failed to save public key: {}", e);
                            }
                            true
                        }
                        Err(e) => {
                            tracing::error!("Failed to export CNG public key: {:?}", e);
                            false
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to load public key: {:?}", e);
                    false
                }
            };

            // 初始化存储（弱加密，不需要认证）
            let storage = app.state::<Arc<StorageState>>();
            if public_key_ready {
                if let Err(e) = storage.initialize() {
                    tracing::error!("Failed to initialize storage: {}", e);
                }
            } else {
                tracing::error!("Storage initialization deferred: public key unavailable");
            }

            // 如果游戏模式已启用且 DML 已启用，启动 GPU 监控
            if registry_config::get_bool("game_mode_enabled").unwrap_or(false)
                && registry_config::get_bool("use_dml").unwrap_or(false)
            {
                tracing::info!("Restoring game mode monitor on startup");
                monitor::start_game_mode_monitor(app.handle().clone());
            }

            Ok(())
        }})
        .invoke_handler(tauri::generate_handler![
            greet,
            close_process,
            set_updating_flag,
            monitor::start_monitor,
            monitor::stop_monitor,
            monitor::pause_monitor,
            monitor::resume_monitor,
            monitor::get_monitor_status,
            monitor::execute_monitor_command,
            // 存储相关命令
            storage_get_timeline,
            storage_search,
            storage_get_image,
            storage_get_screenshot_details,
            storage_delete_screenshot,
            storage_delete_by_time_range,
            storage_list_processes,
            storage_save_screenshot,
            storage_set_policy,
            storage_get_policy,
            storage_get_public_key,
            storage_encrypt_for_chromadb,
            storage_decrypt_from_chromadb,
            analysis::get_analysis_overview,
            // 高级配置命令
            get_advanced_config,
            set_advanced_config,
            monitor::enumerate_gpus,
            toggle_game_mode,
            get_game_mode_status,
            check_dml_setup_needed,
            mark_dml_setup_done,
            // 数据迁移命令
            storage_list_plaintext_files,
            storage_migrate_plaintext,
            storage_migrate_data_dir,
            storage_migration_cancel,
            storage_delete_plaintext,
            // 凭证管理相关命令
            credential_initialize,
            credential_verify_user,
            credential_check_session,
            credential_lock_session,
            credential_set_foreground,
            credential_set_session_timeout,
            credential_get_session_timeout,
            get_autostart_status,
            set_autostart,
            python::check_python_status,
            python::check_python_venv,
            python::request_install_python,
            python::install_python_venv,
            python::check_deps_freshness,
            python::sync_python_deps,
            model_management::download_model,
            // Updater commands
            updater::updater_check,
            updater::updater_download,
            updater::updater_extract,
            updater::updater_apply,
        ]);

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }));
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub fn run_silent_install() {
    python::run_silent_install();
}

/// 子进程入口：执行 CNG 解密并将结果输出到 stdout
/// 协议：
///   成功: exit 0, stdout = hex(master_key)
///   失败: exit 1, stderr = 错误信息
///   用户取消: exit 2, stderr = "UserCancelled"
pub fn run_cng_unlock(key_file_path: &str) {
    use std::process::exit;

    let file_data = match std::fs::read(key_file_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to read master key file: {}", e);
            exit(1);
        }
    };

    let ciphertext = match credential_manager::decode_master_key_file(&file_data) {
        Ok(ct) => ct,
        Err(e) => {
            eprintln!("Failed to decode master key file: {}", e);
            exit(1);
        }
    };

    match credential_manager::decrypt_master_key_with_cng(&ciphertext) {
        Ok(master_key) => {
            print!("{}", hex::encode(&master_key));
            exit(0);
        }
        Err(credential_manager::CredentialError::UserCancelled) => {
            eprintln!("UserCancelled");
            exit(2);
        }
        Err(e) => {
            eprintln!("{}", e);
            exit(1);
        }
    }
}
