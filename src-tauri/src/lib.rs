mod analysis;
mod autostart;
mod capture;
pub mod commands;
mod credential_manager;
pub mod error;
mod error_window;
mod logging;
mod mcp_server;
mod model_management;
mod monitor;
mod native_messaging;
mod python;
mod registry_config;
mod resource_utils;
mod reverse_ipc;
mod sensitive_filter;
mod storage;
mod updater;

use analysis::AnalysisState;
use autostart::{get_autostart_status, set_autostart};
use capture::CaptureState;
use credential_manager::CredentialManagerState;
use monitor::MonitorState;
use sensitive_filter::SensitiveFilterState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage::StorageState;
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

pub static IS_UPDATING: AtomicBool = AtomicBool::new(false);

async fn run_delete_queue_maintenance_loop(app_handle: tauri::AppHandle) {
    const OCR_BATCH_SIZE: i64 = 500;
    const SCREENSHOT_BATCH_SIZE: i64 = 100;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let storage = app_handle.state::<Arc<StorageState>>().inner().clone();

        let ocr_processed = match tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || storage.process_ocr_delete_queue_batch(OCR_BATCH_SIZE)
        })
        .await
        {
            Ok(Ok(count)) => count,
            Ok(Err(e)) => {
                tracing::warn!("[DELETE_QUEUE] OCR batch cleanup failed: {}", e);
                0
            }
            Err(e) => {
                tracing::warn!("[DELETE_QUEUE] OCR cleanup join error: {:?}", e);
                0
            }
        };

        let screenshot_candidates = match tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || storage.fetch_screenshot_delete_candidates(SCREENSHOT_BATCH_SIZE)
        })
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                tracing::warn!("[DELETE_QUEUE] Screenshot queue read failed: {}", e);
                Vec::new()
            }
            Err(e) => {
                tracing::warn!("[DELETE_QUEUE] Screenshot queue join error: {:?}", e);
                Vec::new()
            }
        };

        let mut finalized_screenshots = 0usize;
        if !screenshot_candidates.is_empty() {
            let image_hashes: Vec<String> = screenshot_candidates
                .iter()
                .map(|item| item.image_hash.clone())
                .collect();

            if !image_hashes.is_empty() {
                let monitor_state = app_handle.state::<MonitorState>();
                let payload = serde_json::json!({
                    "command": "delete_by_time_range",
                    "image_hashes": image_hashes,
                });
                if let Err(e) = monitor::forward_command_to_python(&monitor_state, payload).await {
                    tracing::warn!("[DELETE_QUEUE] Vector cleanup failed: {}", e);
                }
            }

            let data_dir = storage
                .data_dir
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();

            for item in &screenshot_candidates {
                let path = std::path::Path::new(&item.image_path);
                let abs_path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    data_dir.join(path)
                };

                if let Err(e) = std::fs::remove_file(&abs_path) {
                    let not_found = e.kind() == std::io::ErrorKind::NotFound;
                    if !not_found {
                        tracing::debug!(
                            "[DELETE_QUEUE] Failed to remove image file {}: {}",
                            abs_path.display(),
                            e
                        );
                    }
                }

                let thumb_path = StorageState::thumbnail_path_for(&abs_path);
                if let Err(e) = std::fs::remove_file(&thumb_path) {
                    let not_found = e.kind() == std::io::ErrorKind::NotFound;
                    if !not_found {
                        tracing::debug!(
                            "[DELETE_QUEUE] Failed to remove thumbnail {}: {}",
                            thumb_path.display(),
                            e
                        );
                    }
                }
            }

            let ids: Vec<i64> = screenshot_candidates.iter().map(|item| item.id).collect();
            finalized_screenshots = match tokio::task::spawn_blocking({
                let storage = storage.clone();
                move || storage.finalize_screenshot_delete_batch(&ids)
            })
            .await
            {
                Ok(Ok(count)) => count,
                Ok(Err(e)) => {
                    tracing::warn!("[DELETE_QUEUE] Screenshot finalize failed: {}", e);
                    0
                }
                Err(e) => {
                    tracing::warn!("[DELETE_QUEUE] Screenshot finalize join error: {:?}", e);
                    0
                }
            };
        }

        let vacuum_ran = match tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || storage.run_incremental_vacuum_if_idle(500, 500)
        })
        .await
        {
            Ok(Ok(ran)) => ran,
            Ok(Err(e)) => {
                tracing::warn!("[DELETE_QUEUE] incremental_vacuum check failed: {}", e);
                false
            }
            Err(e) => {
                tracing::warn!("[DELETE_QUEUE] incremental_vacuum join error: {:?}", e);
                false
            }
        };

        if ocr_processed > 0 || finalized_screenshots > 0 || vacuum_ran {
            tracing::info!(
                "[DELETE_QUEUE] cycle complete: ocr_processed={}, screenshots_finalized={}, vacuum_ran={}",
                ocr_processed,
                finalized_screenshots,
                vacuum_ran
            );
        }
    }
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
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::pause_monitor(state, cs).await;
                });
            }
            MENU_ID_RESUME => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::resume_monitor(state, cs).await;
                });
            }
            MENU_ID_RESTART => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::stop_monitor(state, cs).await;
                    let start_state = app_handle.state::<MonitorState>();
                    let _ = monitor::start_monitor(start_state, app_handle.clone()).await;
                });
            }
            MENU_ID_QUIT => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::stop_monitor(state, cs).await;
                    app_handle.exit(0);
                });
            }
            _ => {}
        })
        .build(&app_handle)?;

    Ok(())
}

pub fn get_data_dir() -> std::path::PathBuf {
    if let Some(dir) = registry_config::get_string("data_dir") {
        return std::path::PathBuf::from(dir);
    }

    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        dirs::data_local_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    });

    let cfg_path = std::path::PathBuf::from(&local_appdata)
        .join("CarbonPaper")
        .join("config.json");
    if cfg_path.exists() {
        if let Ok(s) = std::fs::read_to_string(&cfg_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(dd) = v.get("data_dir").and_then(|d| d.as_str()) {
                    let _ = registry_config::set_string("data_dir", dd);
                    let _ = std::fs::remove_file(&cfg_path);
                    return std::path::PathBuf::from(dd);
                }
            }
        }
    }

    std::path::PathBuf::from(local_appdata)
        .join("CarbonPaper")
        .join("data")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = get_data_dir();
    let _log_guard = logging::init_logging(&data_dir);

    let credential_state = Arc::new(CredentialManagerState::new(data_dir.clone()));
    let storage_state = Arc::new(StorageState::new(
        data_dir.clone(),
        credential_state.clone(),
    ));

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState::new())
        .manage(Arc::new(CaptureState::default()))
        .manage(AnalysisState::default())
        .manage(updater::UpdaterState::new())
        .manage(mcp_server::McpRuntimeState::new())
        .manage(Arc::new(SensitiveFilterState::default()))
        .manage(credential_state)
        .manage(storage_state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    if error_window::HAS_CRITICAL_ERROR.load(Ordering::Relaxed) {
                        std::process::exit(1);
                    }
                    if !IS_UPDATING.load(Ordering::Relaxed) {
                        api.prevent_close();
                        let _ = window.hide();
                        let _ = window.app_handle().emit("app-hidden", ());
                    }
                }
            }
        })
        .setup({
            let data_dir = data_dir.clone();
            move |app| {
                error_window::set_app_handle(app.handle().clone());
                error_window::install_panic_hook();

                build_tray(app)?;

                if let Some(window) = app.get_webview_window("main") {
                    let _ = apply_acrylic(&window, Some((0, 0, 0, 0)));
                }

                analysis::start_memory_sampler(app.handle().clone());
                logging::spawn_maintenance_task(data_dir.clone());

                tracing::info!(
                    r#"
  _____               _                    _____
 / ____|             | |                  |  __ \
| |       __ _  _ __ | |__    ___   _ __  | |__) |  __ _  _ __    ___  _ __
| |      / _` || '__|| '_ \  / _ \ | '_ \ |  ___/  / _` || '_ \  / _ \| '__|
| |____ | (_| || |   | |_) || (_) || | | || |     | (_| || |_) ||  __/| |
 \_____| \__,_||_|   |_.__/  \___/ |_| |_||_|      \__,_|| .__/  \___||_|
                                                         | |
                                                         |_|
    "#
                );

                let credential_state = app.state::<Arc<CredentialManagerState>>();

                let public_key_ready =
                    match credential_manager::load_public_key_from_file(&credential_state) {
                        Ok(public_key) => {
                            tracing::info!(
                                "Public key loaded from file, length: {}",
                                public_key.len()
                            );
                            true
                        }
                        Err(credential_manager::CredentialError::KeyNotFound) => {
                            tracing::info!("Public key file missing, exporting from CNG...");

                            match credential_manager::export_or_get_public_key(&credential_state) {
                                Ok(public_key) => {
                                    tracing::info!(
                                        "CNG public key exported, length: {}",
                                        public_key.len()
                                    );
                                    if let Err(e) = credential_manager::save_public_key_to_file(
                                        &credential_state,
                                        &public_key,
                                    ) {
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

                let storage = app.state::<Arc<StorageState>>();
                if public_key_ready {
                    if let Err(e) = storage.initialize() {
                        tracing::error!("Failed to initialize storage: {}", e);
                    } else {
                        let storage_clone = storage.inner().clone();
                        std::thread::spawn(move || {
                            StorageState::backfill_plaintext_process_names(storage_clone);
                        });

                        let app_handle_cleanup = app.handle().clone();
                        tauri::async_runtime::spawn(async move {
                            run_delete_queue_maintenance_loop(app_handle_cleanup).await;
                        });
                    }
                } else {
                    tracing::error!("Storage initialization deferred: public key unavailable");
                }

                if registry_config::get_bool("game_mode_enabled").unwrap_or(false) {
                    tracing::info!("Restoring game mode monitor on startup");
                    monitor::start_game_mode_monitor(app.handle().clone());
                }

                match native_messaging::sync_installed_extension() {
                    Ok(true) => tracing::info!("Browser extension synced to latest version"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("Extension sync check failed: {}", e),
                }

                {
                    let data_dir_clone = data_dir.clone();
                    let storage_for_nmh = storage.inner().clone();
                    let capture_for_nmh = app.state::<Arc<CaptureState>>().inner().clone();
                    let app_handle_for_nmh = app.handle().clone();
                    std::thread::spawn(move || {
                        match reverse_ipc::generate_nmh_auth_token(&data_dir_clone) {
                            Ok(token) => {
                                let mut nmh_server = reverse_ipc::NmhPipeServer::new();
                                if let Err(e) = nmh_server.start(
                                    storage_for_nmh,
                                    capture_for_nmh,
                                    app_handle_for_nmh,
                                    token,
                                ) {
                                    tracing::error!("Failed to start NMH pipe server: {}", e);
                                }
                                std::mem::forget(nmh_server);
                            }
                            Err(e) => {
                                tracing::error!("Failed to generate NMH auth token: {}", e);
                            }
                        }
                    });
                }

                {
                    let filter_state = app.state::<Arc<SensitiveFilterState>>();
                    filter_state.load_dicts(app.handle());
                    if let Ok(policy) = storage.load_policy() {
                        if let Some(filter_config) = policy.get("sensitive_filter") {
                            if let Ok(config) = serde_json::from_value::<sensitive_filter::SensitiveFilterConfig>(filter_config.clone()) {
                                filter_state.update_config(config);
                            }
                        }
                    }
                }

                {
                    let app_handle_mcp = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        use tauri::Manager;
                        let storage = app_handle_mcp.state::<Arc<StorageState>>();
                        let credential = app_handle_mcp.state::<Arc<CredentialManagerState>>();
                        let mcp_runtime = app_handle_mcp.state::<mcp_server::McpRuntimeState>();

                        if let Ok(policy) = storage.load_policy() {
                            if policy.get("mcp_enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                                match mcp_server::auto_start(
                                    app_handle_mcp.clone(),
                                    &credential,
                                    &storage,
                                    &mcp_runtime,
                                ).await {
                                    Ok(()) => tracing::info!("MCP server auto-started"),
                                    Err(e) => tracing::error!("MCP auto-start failed: {}", e),
                                }
                            }
                        }
                    });
                }

                python::auto_install_spacy_models(app.handle().clone());

                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::utility::greet,
            commands::utility::close_process,
            commands::utility::set_updating_flag,
            monitor::start_monitor,
            monitor::stop_monitor,
            monitor::pause_monitor,
            monitor::resume_monitor,
            monitor::get_monitor_status,
            monitor::execute_monitor_command,
            // 存储相关命令
            commands::storage::storage_get_timeline,
            commands::storage::storage_get_timeline_density,
            commands::storage::storage_search,
            commands::storage::storage_get_image,
            commands::storage::storage_get_thumbnail,
            commands::storage::storage_batch_get_thumbnails,
            commands::storage::storage_warmup_thumbnails,
            commands::storage::storage_get_screenshot_details,
            commands::storage::storage_delete_screenshot,
            commands::storage::storage_delete_by_time_range,
            commands::storage::storage_list_processes,
            commands::storage::storage_get_process_stats,
            commands::storage::storage_get_process_monthly_thumbnails,
            commands::storage::storage_soft_delete,
            commands::storage::storage_soft_delete_screenshots,
            commands::storage::storage_get_delete_queue_status,
            commands::storage::storage_save_screenshot,
            commands::storage::storage_set_policy,
            commands::storage::storage_get_policy,
            commands::storage::storage_get_public_key,
            commands::storage::storage_compute_link_scores,
            commands::storage::storage_encrypt_for_chromadb,
            commands::storage::storage_decrypt_from_chromadb,
            commands::storage::storage_update_category,
            commands::storage::storage_get_categories,
            commands::storage::storage_get_categories_from_db,
            commands::storage::storage_batch_get_categories,
            commands::migration::storage_get_startup_vacuum_status,
            commands::migration::storage_run_startup_vacuum_if_needed,
            commands::migration::storage_run_manual_vacuum,
            commands::migration::storage_check_hmac_migration_status,
            commands::migration::storage_run_hmac_migration,
            commands::migration::storage_hmac_migration_cancel,
            // 任务聚类命令
            commands::storage::storage_get_tasks,
            commands::storage::storage_get_related_screenshots,
            commands::storage::storage_get_task_screenshots,
            commands::storage::storage_update_task_label,
            commands::storage::storage_delete_task,
            commands::storage::storage_merge_tasks,
            commands::storage::storage_save_clustering_results,
            analysis::get_analysis_overview,
            // MCP 服务命令
            commands::mcp::mcp_set_enabled,
            commands::mcp::mcp_get_status,
            commands::mcp::mcp_reset_token,
            commands::mcp::mcp_get_port,
            commands::mcp::mcp_set_port,
            commands::mcp::mcp_get_sensitive_filter_config,
            commands::mcp::mcp_set_sensitive_filter_config,
            // 高级配置命令
            commands::utility::get_advanced_config,
            commands::utility::set_advanced_config,
            monitor::enumerate_gpus,
            commands::utility::toggle_game_mode,
            commands::utility::get_game_mode_status,
            // 数据迁移命令
            commands::migration::storage_list_plaintext_files,
            commands::migration::storage_migrate_plaintext,
            commands::migration::storage_migrate_data_dir,
            commands::migration::storage_migration_cancel,
            commands::migration::storage_delete_plaintext,
            // 凭证管理相关命令
            commands::credential::credential_initialize,
            commands::credential::credential_verify_user,
            commands::credential::credential_check_session,
            commands::credential::credential_lock_session,
            commands::credential::credential_set_foreground,
            commands::credential::credential_set_session_timeout,
            commands::credential::credential_get_session_timeout,
            get_autostart_status,
            set_autostart,
            python::check_python_status,
            python::check_python_venv,
            python::request_install_python,
            python::install_python_venv,
            python::check_deps_freshness,
            python::sync_python_deps,
            python::install_spacy_model,
            python::check_spacy_models,
            python::force_recheck_spacy_models,
            model_management::download_model,
            model_management::check_model_files,
            // Updater commands
            updater::updater_check,
            updater::updater_download,
            updater::updater_extract,
            updater::updater_apply,
            // Native messaging commands
            native_messaging::get_nm_host_status,
            native_messaging::register_nm_host_chrome,
            native_messaging::register_nm_host_edge,
            native_messaging::install_browser_extension,
            native_messaging::sync_extension_if_needed,
            commands::utility::check_extension_setup_needed,
            commands::utility::mark_extension_setup_done,
            commands::utility::check_clustering_setup_needed,
            commands::utility::mark_clustering_setup_done,
            commands::utility::get_extension_enhancement_config,
            commands::utility::set_extension_enhancement,
            // Error window commands
            commands::utility::get_log_dir,
            commands::utility::restart_app,
            commands::utility::trigger_test_error,
            commands::utility::exit_app,
            commands::utility::frontend_log,
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
