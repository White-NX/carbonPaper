mod autostart;
mod analysis;
mod credential_manager;
mod monitor;
mod python;
mod resource_utils;
mod model_management;
mod reverse_ipc;
mod storage;

use autostart::{get_autostart_status, set_autostart};
use analysis::AnalysisState;
use credential_manager::CredentialManagerState;
use monitor::MonitorState;
use storage::StorageState;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Emitter;
use tauri::Manager;

const MENU_ID_OPEN: &str = "open";
const MENU_ID_PAUSE: &str = "pause";
const MENU_ID_RESUME: &str = "resume";
const MENU_ID_RESTART: &str = "restart";
const MENU_ID_QUIT: &str = "quit";

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
) -> Result<Vec<storage::ScreenshotRecord>, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;
    
    // 如果传入的是毫秒级时间戳，转换为秒
    let start_ts = if start_time > 10_000_000_000.0 { start_time / 1000.0 } else { start_time };
    let end_ts = if end_time > 10_000_000_000.0 { end_time / 1000.0 } else { end_time };
    
    state.get_screenshots_by_time_range(start_ts, end_ts)
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
    
    println!("[storage_get_image] id={:?}, path={:?}", id, path);
    
    let image_path = if let Some(id) = id {
        let record = state.get_screenshot_by_id(id)?;
        println!("[storage_get_image] Found record: {:?}", record.as_ref().map(|r| &r.image_path));
        record.map(|r| r.image_path)
    } else {
        path
    };
    
    println!("[storage_get_image] Final image_path={:?}", image_path);
    
    match image_path {
        Some(path) => {
            // 使用 StorageManager::read_image 读取加密图片
            match state.read_image(&path) {
                Ok((data, mime_type)) => {
                    println!("[storage_get_image] Successfully read image, mime={}", mime_type);
                    Ok(serde_json::json!({
                        "status": "success",
                        "data": data,
                        "mime_type": mime_type
                    }))
                }
                Err(e) => {
                    println!("[storage_get_image] Failed to read image: {}", e);
                    Err(e)
                }
            }
        }
        None => {
            println!("[storage_get_image] Image not found - no path available");
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
    screenshot_id: i64,
) -> Result<serde_json::Value, String> {
    let deleted = state.delete_screenshot(screenshot_id)?;
    Ok(serde_json::json!({
        "status": "success",
        "deleted": deleted
    }))
}

#[tauri::command]
async fn storage_delete_by_time_range(
    state: tauri::State<'_, Arc<StorageState>>,
    start_time: f64,
    end_time: f64,
) -> Result<serde_json::Value, String> {
    let deleted_count = state.delete_screenshots_by_time_range(start_time, end_time)?;
    Ok(serde_json::json!({
        "status": "success",
        "deleted_count": deleted_count
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

// ==================== 凭证管理相关命令 ====================

#[tauri::command]
async fn credential_initialize(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        // 创建或获取凭证
        let public_key = credential_manager::create_or_get_credential(&credential_state)
            .await
            .map_err(|e| format!("Failed to initialize credentials: {}", e))?;

        // 通过 Windows Hello 解锁/创建主密钥
        credential_manager::ensure_master_key_ready(&credential_state)
            .await
            .map_err(|e| format!("Failed to unlock master key: {}", e))?;

        // 认证成功，更新会话时间
        credential_state.update_auth_time();
        
        // 保存公钥到文件
        credential_manager::save_public_key_to_file(&credential_state, &public_key)
            .map_err(|e| format!("Failed to save public key: {}", e))?;
        
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
        // 通过 Windows Hello 解锁主密钥
        credential_manager::ensure_master_key_ready(&state)
            .await
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

fn get_data_dir() -> std::path::PathBuf {
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        dirs::data_local_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    });
    std::path::PathBuf::from(local_appdata).join("CarbonPaper").join("data")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 创建凭证管理器状态
    let data_dir = get_data_dir();
    let credential_state = Arc::new(CredentialManagerState::new("CarbonPaper", data_dir.clone()));
    let storage_state = Arc::new(StorageState::new(data_dir.clone(), credential_state.clone()));
    
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState::new())
        .manage(AnalysisState::default())
        .manage(credential_state)
        .manage(storage_state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                let _ = window.app_handle().emit("app-hidden", ());
            }
        })
        .setup(|app| {
            build_tray(app)?;
            analysis::start_memory_sampler(app.handle().clone());
            
            // 初始化凭据管理器（加载公钥或首次创建）
            let credential_state = app.state::<Arc<CredentialManagerState>>();
            let cred_state_clone = credential_state.inner().clone();

            // 公钥用于弱数据库加密与行级封装
            let public_key_ready = match credential_manager::load_public_key_from_file(&credential_state) {
                Ok(public_key) => {
                    println!("[lib] Public key loaded from file, length: {}", public_key.len());
                    true
                }
                Err(credential_manager::CredentialError::KeyNotFound) => {
                    println!("[lib] Public key file missing, creating credential (this may show Windows Hello prompt)...");

                    // 使用 tokio::runtime::Handle 而非 block_on 来避免嵌套运行时问题
                    let handle = tauri::async_runtime::handle();
                    let result = std::thread::spawn(move || {
                        handle.block_on(async {
                            credential_manager::create_or_get_credential(&cred_state_clone).await
                        })
                    })
                    .join();

                    match result {
                        Ok(Ok(public_key)) => {
                            println!("[lib] Credential ready, public key length: {}", public_key.len());
                            let save_state = credential_state.inner().clone();
                            if let Err(e) = credential_manager::save_public_key_to_file(&save_state, &public_key) {
                                eprintln!("[lib] Failed to save public key: {}", e);
                            }
                            true
                        }
                        Ok(Err(e)) => {
                            eprintln!("[lib] Failed to create credential: {:?}", e);
                            false
                        }
                        Err(_) => {
                            eprintln!("[lib] Credential creation thread panicked");
                            false
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[lib] Failed to load public key: {:?}", e);
                    false
                }
            };

            // 初始化存储（弱加密，不需要认证）
            let storage = app.state::<Arc<StorageState>>();
            if public_key_ready {
                if let Err(e) = storage.initialize() {
                    eprintln!("Failed to initialize storage: {}", e);
                }
            } else {
                eprintln!("[lib] Storage initialization deferred: public key unavailable");
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            close_process,
            monitor::start_monitor,
            monitor::stop_monitor,
            monitor::pause_monitor,
            monitor::resume_monitor,
            monitor::get_monitor_status,
            monitor::execute_monitor_command,
            // 新增存储相关命令
            storage_get_timeline,
            storage_search,
            storage_get_image,
            storage_get_screenshot_details,
            storage_delete_screenshot,
            storage_delete_by_time_range,
            storage_list_processes,
            storage_save_screenshot,
            storage_get_public_key,
            storage_encrypt_for_chromadb,
            storage_decrypt_from_chromadb,
            // 数据迁移命令
            storage_list_plaintext_files,
            storage_migrate_plaintext,
            storage_delete_plaintext,
            // 凭证管理相关命令
            credential_initialize,
            credential_verify_user,
            credential_check_session,
            credential_lock_session,
            credential_set_foreground,
            analysis::get_analysis_overview,
            get_autostart_status,
            set_autostart,
            python::check_python_status,
            python::check_python_venv,
            python::request_install_python,
            python::install_python_venv,
            model_management::download_model,
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
