//! CarbonPaper's Tauri backend composition root.
//!
//! This crate wires native capture, encrypted storage, monitor/ML processes, IPC,
//! commands, tray behavior, and application lifecycle into the desktop runtime.

mod analysis;
mod autostart;
mod capture;
pub mod commands;
mod credential_manager;
pub mod error;
mod error_window;
mod i18n;
mod idle;
mod logging;
mod mcp_server;
mod mcp_token;
#[allow(dead_code)]
mod ml_contracts;
#[allow(dead_code)]
mod ml_protocol;
mod ml_runtime;
mod model_management;
mod monitor;
mod monitor_ipc;
mod native_messaging;
mod power;
mod python;
mod python_launcher;
mod registry_config;
mod resource_utils;
mod reverse_ipc;
mod reverse_ipc_protocol;
mod script_integrity;
mod sensitive_filter;
mod storage;
mod updater;

use analysis::AnalysisState;
use autostart::{get_autostart_status, set_autostart};
use capture::CaptureState;
use credential_manager::CredentialManagerState;
use idle::IdleState;
use monitor::MonitorState;
use power::PowerState;
use sensitive_filter::SensitiveFilterState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use storage::StorageState;
use tauri::menu::{MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_notification::NotificationExt;
use window_vibrancy::apply_acrylic;

const MENU_ID_OPEN: &str = "open";
const MENU_ID_TOGGLE_CAPTURE: &str = "toggle_capture";
const MENU_ID_RESTART: &str = "restart";
const MENU_ID_LIGHTWEIGHT: &str = "lightweight";
const MENU_ID_QUIT: &str = "quit";

pub static IS_UPDATING: AtomicBool = AtomicBool::new(false);
pub static IS_QUITTING: AtomicBool = AtomicBool::new(false);

type TrayMenuItem = MenuItem<tauri::Wry>;

pub struct TrayMenuState {
    pub open: TrayMenuItem,
    pub toggle_capture: TrayMenuItem,
    pub restart: TrayMenuItem,
    pub lightweight: TrayMenuItem,
    pub quit: TrayMenuItem,
}

#[derive(Clone, Copy)]
struct TrayTexts {
    open: &'static str,
    screenshot_running: &'static str,
    screenshot_paused: &'static str,
    screenshot_stopped: &'static str,
    restart: &'static str,
    lightweight: &'static str,
    lightweight_active: &'static str,
    quit: &'static str,
    open_error: &'static str,
    switched_lightweight: &'static str,
    auto_switched_lightweight: &'static str,
}

const TRAY_TEXTS_ZH: TrayTexts = TrayTexts {
    open: "打开界面",
    screenshot_running: "截图：运行中（点击暂停）",
    screenshot_paused: "截图：已暂停（点击恢复）",
    screenshot_stopped: "截图：未运行",
    restart: "重启截图",
    lightweight: "切换到轻量模式",
    lightweight_active: "轻量模式已开启",
    quit: "彻底退出",
    open_error: "无法打开界面",
    switched_lightweight: "已切换到轻量模式，通过托盘菜单可重新打开界面",
    auto_switched_lightweight: "已自动切换到轻量模式以节省内存",
};

const TRAY_TEXTS_EN: TrayTexts = TrayTexts {
    open: "Open Window",
    screenshot_running: "Screenshots: On (click to pause)",
    screenshot_paused: "Screenshots: Paused (click to resume)",
    screenshot_stopped: "Screenshots: Not Running",
    restart: "Restart Screenshots",
    lightweight: "Switch to Lightweight Mode",
    lightweight_active: "Lightweight Mode On",
    quit: "Quit Completely",
    open_error: "Failed to open window",
    switched_lightweight: "Switched to lightweight mode. Reopen the window from the tray menu.",
    auto_switched_lightweight: "Automatically switched to lightweight mode to save memory.",
};

fn normalize_app_language(language: &str) -> String {
    i18n::supported_locale(language)
}

fn tray_texts() -> &'static TrayTexts {
    let language = registry_config::get_string("language").unwrap_or_else(|| "zh-CN".to_string());
    match normalize_app_language(&language).as_str() {
        "en" => &TRAY_TEXTS_EN,
        _ => &TRAY_TEXTS_ZH,
    }
}

pub(crate) fn tray_text_lightweight_switched() -> &'static str {
    tray_texts().switched_lightweight
}

fn tray_text_auto_lightweight_switched() -> &'static str {
    tray_texts().auto_switched_lightweight
}

pub(crate) fn set_app_language(app: &tauri::AppHandle, language: &str) -> Result<(), String> {
    registry_config::set_string("language", &normalize_app_language(language))?;
    refresh_tray_menu(app);
    Ok(())
}

pub(crate) fn refresh_tray_menu(app: &tauri::AppHandle) {
    let Some(menu_state) = app.try_state::<TrayMenuState>() else {
        return;
    };

    let texts = tray_texts();
    let _ = menu_state.open.set_text(texts.open);
    let _ = menu_state.restart.set_text(texts.restart);
    let _ = menu_state.quit.set_text(texts.quit);

    let monitor_running = app
        .try_state::<MonitorState>()
        .map(|state| {
            let guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
            guard.is_some()
        })
        .unwrap_or(false);

    let capture_paused = app
        .try_state::<Arc<CaptureState>>()
        .map(|state| state.paused.load(Ordering::SeqCst))
        .unwrap_or(false);

    if monitor_running {
        let _ = menu_state.toggle_capture.set_enabled(true);
        let _ = menu_state.toggle_capture.set_text(if capture_paused {
            texts.screenshot_paused
        } else {
            texts.screenshot_running
        });
    } else {
        let _ = menu_state.toggle_capture.set_text(texts.screenshot_stopped);
        let _ = menu_state.toggle_capture.set_enabled(false);
    }

    let is_lightweight = app
        .try_state::<Arc<LightweightModeState>>()
        .map(|state| {
            *state
                .is_lightweight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        })
        .unwrap_or(false);

    let _ = menu_state.lightweight.set_text(if is_lightweight {
        texts.lightweight_active
    } else {
        texts.lightweight
    });
    let _ = menu_state.lightweight.set_enabled(!is_lightweight);
}

// 轻量模式状态管理
pub struct LightweightModeState {
    pub is_lightweight: Mutex<bool>,
    pub auto_switch_timer: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl LightweightModeState {
    pub fn new() -> Self {
        Self {
            is_lightweight: Mutex::new(false),
            auto_switch_timer: Mutex::new(None),
        }
    }
}

async fn run_delete_queue_maintenance_loop(app_handle: tauri::AppHandle) {
    const OCR_BATCH_SIZE: i64 = 500;
    const SCREENSHOT_BATCH_SIZE: i64 = 100;
    const POLICY_CHECK_INTERVAL_SECS: u64 = 60;

    let mut last_policy_check =
        std::time::Instant::now() - std::time::Duration::from_secs(POLICY_CHECK_INTERVAL_SECS);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        let storage = app_handle.state::<Arc<StorageState>>().inner().clone();

        let policy_pruned = if last_policy_check.elapsed()
            >= std::time::Duration::from_secs(POLICY_CHECK_INTERVAL_SECS)
        {
            last_policy_check = std::time::Instant::now();
            match tokio::task::spawn_blocking({
                let storage = storage.clone();
                move || storage.enforce_snapshot_storage_policy_once()
            })
            .await
            {
                Ok(Ok(Some(summary))) => {
                    tracing::info!("[POLICY] {}", summary);
                    true
                }
                Ok(Ok(None)) => false,
                Ok(Err(e)) => {
                    tracing::warn!("[POLICY] enforce failed: {}", e);
                    false
                }
                Err(e) => {
                    tracing::warn!("[POLICY] enforce join error: {:?}", e);
                    false
                }
            }
        } else {
            false
        };

        let ocr_processed = match tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || storage.process_ocr_delete_queue_batch(OCR_BATCH_SIZE)
        })
        .await
        {
            Ok(Ok(count)) => count,
            Ok(Err(e)) => {
                tracing::debug!("[DELETE_QUEUE] OCR batch cleanup failed: {}", e);
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

            // Vector embeddings can only be removed while the Python monitor is
            // reachable. Require its ack before destroying the image hashes the
            // cleanup needs; otherwise leave the queue entries for a later cycle.
            // The one exception is a monitor that is disabled by configuration and
            // not running, where waiting would stall retention forever.
            let monitor_state = app_handle.state::<MonitorState>();
            let monitor_running = monitor_state
                .process
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            let monitor_autostart =
                registry_config::get_bool("autoStartMonitor").unwrap_or(true);

            let vector_cleanup_done = if image_hashes.is_empty()
                || vector_cleanup_can_be_skipped(monitor_running, monitor_autostart)
            {
                true
            } else {
                let payload = serde_json::json!({
                    "command": "delete_by_time_range",
                    "image_hashes": image_hashes,
                });
                let result = monitor::forward_command_to_python(&monitor_state, payload).await;
                let acked = vector_cleanup_acked(&result);
                if !acked {
                    match result {
                        Ok(response) => tracing::warn!(
                            "[DELETE_QUEUE] Vector cleanup rejected, deferring finalize: {}",
                            response
                        ),
                        Err(e) => tracing::warn!(
                            "[DELETE_QUEUE] Vector cleanup unavailable, deferring finalize: {}",
                            e
                        ),
                    }
                }
                acked
            };

            if vector_cleanup_done {
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

        if ocr_processed > 0 || finalized_screenshots > 0 || vacuum_ran || policy_pruned {
            tracing::info!(
                "[DELETE_QUEUE] cycle complete: policy_pruned={}, ocr_processed={}, screenshots_finalized={}, vacuum_ran={}",
                policy_pruned,
                ocr_processed,
                finalized_screenshots,
                vacuum_ran
            );
        }
    }
}

fn vector_cleanup_can_be_skipped(monitor_running: bool, monitor_autostart: bool) -> bool {
    !monitor_running && !monitor_autostart
}

fn vector_cleanup_acked(result: &Result<serde_json::Value, String>) -> bool {
    matches!(
        result,
        Ok(response) if response.get("status").and_then(|s| s.as_str()) == Some("success")
    )
}

fn is_open_tray_click(button: MouseButton, button_state: MouseButtonState) -> bool {
    matches!(
        (button, button_state),
        (MouseButton::Left, MouseButtonState::Up)
    )
}

fn open_main_window(app: &tauri::AppHandle, show_ocr_model_repair: bool) {
    cancel_auto_lightweight_timer(app);

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        if show_ocr_model_repair {
            let _ = app.emit("show-ocr-model-repair", ());
        }
        if let Some(lightweight_state) = app.try_state::<Arc<LightweightModeState>>() {
            *lightweight_state
                .is_lightweight
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = false;
        }
        refresh_tray_menu(app);
        return;
    }

    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        match create_main_window(&app_handle) {
            Ok(()) => {
                let lightweight_state = app_handle.state::<Arc<LightweightModeState>>();
                *lightweight_state
                    .is_lightweight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = false;
                tracing::info!("Window recreated from lightweight mode");
                refresh_tray_menu(&app_handle);
                if show_ocr_model_repair {
                    let _ = app_handle.emit("show-ocr-model-repair", ());
                }
            }
            Err(e) => {
                tracing::error!("Failed to create main window: {}", e);
                let texts = tray_texts();
                let _ = app_handle
                    .notification()
                    .builder()
                    .title("CarbonPaper")
                    .body(&format!("{}: {}", texts.open_error, e))
                    .show();
            }
        }
    });
}

fn open_main_window_from_tray(app: &tauri::AppHandle) {
    open_main_window(app, false);
}

pub(crate) fn open_main_window_for_ocr_model_repair(app: &tauri::AppHandle) {
    open_main_window(app, true);
}

fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();
    let texts = tray_texts();

    let open_item = MenuItemBuilder::with_id(MENU_ID_OPEN, texts.open).build(&app_handle)?;
    let toggle_capture_item =
        MenuItemBuilder::with_id(MENU_ID_TOGGLE_CAPTURE, texts.screenshot_stopped)
            .enabled(false)
            .build(&app_handle)?;
    let restart_item =
        MenuItemBuilder::with_id(MENU_ID_RESTART, texts.restart).build(&app_handle)?;
    let lightweight_item =
        MenuItemBuilder::with_id(MENU_ID_LIGHTWEIGHT, texts.lightweight).build(&app_handle)?;
    let quit_item = MenuItemBuilder::with_id(MENU_ID_QUIT, texts.quit).build(&app_handle)?;

    let menu = MenuBuilder::new(&app_handle)
        .item(&open_item)
        .item(&toggle_capture_item)
        .item(&restart_item)
        .item(&lightweight_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let mut tray_builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    }
    let tray = tray_builder
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                if is_open_tray_click(button, button_state) {
                    open_main_window_from_tray(tray.app_handle());
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_ID_OPEN => {
                open_main_window_from_tray(app);
            }
            MENU_ID_TOGGLE_CAPTURE => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let monitor_running = {
                        let guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
                        guard.is_some()
                    };
                    if monitor_running {
                        if cs.paused.load(Ordering::SeqCst) {
                            let _ =
                                monitor::resume_monitor_impl(state, cs, app_handle.clone()).await;
                        } else {
                            let _ =
                                monitor::pause_monitor_impl(state, cs, app_handle.clone()).await;
                        }
                    } else {
                        refresh_tray_menu(&app_handle);
                    }
                });
            }
            MENU_ID_RESTART => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::stop_monitor_impl(state, cs, app_handle.clone()).await;
                    let start_state = app_handle.state::<MonitorState>();
                    let _ = monitor::start_monitor_impl(start_state, app_handle.clone()).await;
                });
            }
            MENU_ID_LIGHTWEIGHT => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let lightweight_state = app_handle.state::<Arc<LightweightModeState>>();
                    if let Err(e) = commands::utility::switch_to_lightweight_mode(
                        app_handle.clone(),
                        lightweight_state,
                    )
                    .await
                    {
                        tracing::error!("Failed to switch to lightweight mode from tray: {}", e);
                    }
                    refresh_tray_menu(&app_handle);
                });
            }
            MENU_ID_QUIT => {
                let app_handle = app.clone();
                IS_QUITTING.store(true, Ordering::Relaxed);
                cancel_auto_lightweight_timer(&app_handle);

                let state = app_handle.state::<MonitorState>();
                state.stopping.store(true, Ordering::SeqCst);

                let capture_state = app_handle.state::<Arc<CaptureState>>();
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
                capture_state.clear_wgc_session("app_quit");

                app_handle.exit(0);
            }
            _ => {}
        })
        .build(&app_handle)?;

    app.manage(TrayMenuState {
        open: open_item,
        toggle_capture: toggle_capture_item,
        restart: restart_item,
        lightweight: lightweight_item,
        quit: quit_item,
    });

    // 将托盘图标保存到应用状态中，防止被释放
    app.manage(tray);
    refresh_tray_menu(&app_handle);

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

// 动态创建主窗口
pub fn create_main_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::WebviewWindowBuilder;

    tracing::info!("Creating main window");

    let window =
        WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
            .title("carbonpaper")
            .inner_size(1300.0, 750.0)
            .decorations(false)
            .transparent(true)
            .visible(true)
            .build()?;

    // 应用 Acrylic 效果
    let _ = apply_acrylic(&window, Some((0, 0, 0, 0)));

    tracing::info!("Main window created successfully");
    Ok(())
}

// 启动自动切换到轻量模式的定时器
fn start_auto_lightweight_timer(app: tauri::AppHandle) {
    // 检查是否启用自动切换
    let auto_enabled = registry_config::get_bool("auto_lightweight_enabled").unwrap_or(false);
    if !auto_enabled {
        return;
    }

    let delay_minutes = registry_config::get_u32("auto_lightweight_delay_minutes").unwrap_or(5);

    tracing::info!("Starting auto-lightweight timer: {} minutes", delay_minutes);

    let lightweight_state = app.state::<Arc<LightweightModeState>>();

    // 取消之前的定时器（如果有）
    if let Some(old_timer) = lightweight_state.auto_switch_timer.lock().unwrap().take() {
        old_timer.abort();
    }

    // 启动新定时器
    let app_clone = app.clone();
    let timer = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(delay_minutes as u64 * 60)).await;

        tracing::info!("Auto-lightweight timer expired, checking window state");

        // 检查窗口是否仍然隐藏
        if let Some(window) = app_clone.get_webview_window("main") {
            if !window.is_visible().unwrap_or(true) {
                tracing::info!("Window still hidden, switching to lightweight mode");

                // 销毁窗口
                if let Err(e) = window.destroy() {
                    tracing::error!("Failed to destroy window: {}", e);
                    return;
                }

                // 更新状态
                let lightweight_state = app_clone.state::<Arc<LightweightModeState>>();
                *lightweight_state.is_lightweight.lock().unwrap() = true;
                refresh_tray_menu(&app_clone);

                // 发送通知
                let _ = app_clone
                    .notification()
                    .builder()
                    .title("CarbonPaper")
                    .body(tray_text_auto_lightweight_switched())
                    .show();

                tracing::info!("Successfully switched to lightweight mode");
            } else {
                tracing::info!("Window is visible, canceling auto-lightweight");
            }
        }
    });

    // 保存定时器句柄
    *lightweight_state.auto_switch_timer.lock().unwrap() = Some(timer);
}

pub(crate) fn hide_main_window_to_tray(window: &tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())?;
    let app = window.app_handle();
    let _ = app.emit("app-hidden", ());
    start_auto_lightweight_timer(app.clone());
    Ok(())
}

// 取消自动切换定时器
fn cancel_auto_lightweight_timer(app: &tauri::AppHandle) {
    let lightweight_state = app.state::<Arc<LightweightModeState>>();
    let mut timer_guard = lightweight_state.auto_switch_timer.lock().unwrap();
    if let Some(timer) = timer_guard.take() {
        timer.abort();
        tracing::info!("Auto-lightweight timer canceled");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let data_dir = get_data_dir();
    let _log_guard = logging::init_logging(&data_dir);

    // 检查是否应该隐藏启动
    let start_hidden = std::env::var("CARBONPAPER_START_HIDDEN").is_ok()
        || registry_config::get_bool("start_with_window_hidden").unwrap_or(false);

    if start_hidden {
        tracing::info!("Starting in lightweight mode (window hidden)");
    }

    let credential_state = Arc::new(CredentialManagerState::new(data_dir.clone()));
    let storage_state = Arc::new(StorageState::new(
        data_dir.clone(),
        credential_state.clone(),
    ));
    let lightweight_state = Arc::new(LightweightModeState::new());

    // 如果隐藏启动，标记为轻量模式
    if start_hidden {
        *lightweight_state.is_lightweight.lock().unwrap() = true;
    }

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState::new())
        .manage(Arc::new(ml_runtime::MlRuntimeState::new()))
        .manage(Arc::new(CaptureState::default()))
        .manage(AnalysisState::default())
        .manage(updater::UpdaterState::new())
        .manage(mcp_server::McpRuntimeState::new())
        .manage(Arc::new(SensitiveFilterState::default()))
        .manage(credential_state)
        .manage(storage_state)
        .manage(lightweight_state.clone())
        .manage(Arc::new(PowerState::new()))
        .manage(Arc::new(IdleState::new()))
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    if error_window::HAS_CRITICAL_ERROR.load(Ordering::Relaxed) {
                        std::process::exit(1);
                    }
                    if !IS_UPDATING.load(Ordering::Relaxed) {
                        api.prevent_close();
                        if let Err(e) = hide_main_window_to_tray(window) {
                            tracing::error!("Failed to hide main window to tray: {}", e);
                        }
                    }
                }
            }
        })
        .setup({
            let data_dir = data_dir.clone();
            let start_hidden = start_hidden;
            move |app| {
                error_window::set_app_handle(app.handle().clone());
                error_window::install_panic_hook();

                build_tray(app)?;

                if updater::is_update_smoke_test_enabled() {
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = window.destroy() {
                            tracing::warn!("Failed to destroy window for update smoke test: {}", e);
                        }
                    }
                    updater::maybe_run_update_smoke_test(app.handle().clone());
                    return Ok(());
                }

                // 只有非隐藏启动时才应用 acrylic 效果
                if !start_hidden {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = apply_acrylic(&window, Some((0, 0, 0, 0)));
                    }
                } else {
                    // 隐藏启动：销毁窗口以实现真正的轻量模式，释放 WebView 内存
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(e) = window.destroy() {
                            tracing::warn!("Failed to destroy window on hidden start: {}", e);
                        } else {
                            tracing::info!("Main window destroyed for lightweight mode");
                        }
                    }
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
                        match storage.discard_incomplete_ocr_postprocess() {
                            Ok(discarded) if discarded > 0 => tracing::info!(
                                "[ML:POSTPROCESS] discarded {} incomplete rows from the previous application process",
                                discarded
                            ),
                            Ok(_) => {}
                            Err(error) => tracing::warn!(
                                "[ML:POSTPROCESS] failed to discard incomplete startup rows: {}",
                                error
                            ),
                        }

                        let storage_clone = storage.inner().clone();
                        std::thread::spawn(move || {
                            StorageState::backfill_plaintext_process_names(storage_clone);
                        });

                        let app_handle_cleanup = app.handle().clone();
                        tauri::async_runtime::spawn(async move {
                            run_delete_queue_maintenance_loop(app_handle_cleanup).await;
                        });
                        let app_handle_postprocess = app.handle().clone();
                        tauri::async_runtime::spawn(async move {
                            ml_runtime::run_postprocess_retry_loop(app_handle_postprocess).await;
                        });
                    }
                } else {
                    tracing::error!("Storage initialization deferred: public key unavailable");
                }

                if registry_config::get_bool("game_mode_enabled").unwrap_or(false) {
                    tracing::info!("Restoring game mode monitor on startup");
                    monitor::start_game_mode_monitor(app.handle().clone());
                }

                // Start power monitor (power saving mode)
                power::start_power_monitor(app.handle().clone());
                idle::start_idle_monitor(app.handle().clone());

                match native_messaging::sync_installed_extension() {
                    Ok(true) => tracing::info!("Browser extension synced to latest version"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("Extension sync check failed: {}", e),
                }

                commands::utility::migrate_extension_enhancement_config();

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
                            if let Ok(config) = serde_json::from_value::<
                                sensitive_filter::SensitiveFilterConfig,
                            >(filter_config.clone())
                            {
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
                            if policy
                                .get("mcp_enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                            {
                                match mcp_server::auto_start(
                                    app_handle_mcp.clone(),
                                    &credential,
                                    &storage,
                                    &mcp_runtime,
                                )
                                .await
                                {
                                    Ok(()) => tracing::info!("MCP server auto-started"),
                                    Err(e) => tracing::error!("MCP auto-start failed: {}", e),
                                }
                            }
                        }
                    });
                }

                python::auto_install_spacy_models(app.handle().clone());

                ml_runtime::schedule_ocr_model_health_notification(app.handle().clone());

                // 轻量模式下自动启动监控
                if start_hidden
                    && registry_config::get_bool("lightweight_auto_start_monitor").unwrap_or(true)
                {
                    let app_handle = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app_handle.state::<MonitorState>();
                        let app_handle_clone = app_handle.clone();
                        if let Err(e) = monitor::start_monitor_impl(state, app_handle_clone).await {
                            tracing::error!("Failed to auto-start monitor: {}", e);
                        }
                    });
                }

                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::utility::close_process,
            commands::utility::set_app_language,
            monitor::start_monitor,
            monitor::get_monitor_autostart,
            monitor::set_monitor_autostart,
            monitor::stop_monitor,
            monitor::pause_monitor,
            monitor::resume_monitor,
            monitor::get_monitor_status,
            monitor::monitor_search_nl,
            monitor::monitor_update_filters,
            monitor::monitor_update_advanced_config,
            monitor::monitor_update_feature_config,
            monitor::monitor_run_clustering,
            monitor::monitor_get_clustering_status,
            monitor::monitor_set_clustering_interval,
            monitor::monitor_get_task_clusters,
            monitor::monitor_nl_cluster_query,
            monitor::monitor_nl_cluster_reranker_status,
            monitor::monitor_smart_cluster_worker_status,
            monitor::monitor_smart_cluster_drain_now,
            monitor::monitor_smart_cluster_stop_drain,
            monitor::monitor_smart_cluster_calibrate_preview,
            monitor::monitor_presidio_set_language,
            monitor::monitor_classify_debug,
            ml_runtime::get_ml_ocr_status,
            ml_runtime::restart_ml_ocr_worker,
            ml_runtime::get_rust_ocr_model_status,
            ml_runtime::download_rust_ocr_model,
            ml_runtime::take_ocr_model_repair_request,
            ml_runtime::debug_trigger_ocr_model_repair_notification,
            monitor::monitor_remove_local_anchors_by_process,
            // 安全告警调试触发（设置 → 高级 → 调试）
            script_integrity::debug_trigger_security_alert,
            // 存储相关命令
            commands::storage::storage_get_timeline,
            commands::storage::storage_get_timeline_density,
            commands::storage::storage_search,
            commands::storage::storage_get_image,
            commands::storage::storage_get_thumbnail,
            commands::storage::storage_batch_get_thumbnails,
            commands::storage::storage_warmup_thumbnails,
            commands::storage::storage_get_thumbnail_warmup_status,
            commands::storage::storage_cancel_thumbnail_warmup,
            commands::storage::storage_get_screenshot_details,
            commands::storage::storage_delete_screenshot,
            commands::storage::storage_delete_by_time_range,
            commands::storage::storage_list_processes,
            commands::storage::storage_get_process_stats,
            commands::storage::storage_get_process_monthly_thumbnails,
            commands::storage::storage_soft_delete,
            commands::storage::storage_soft_delete_screenshots,
            commands::storage::storage_get_delete_queue_status,
            commands::storage::storage_get_index_health,
            commands::storage::storage_retry_vector_indexing,
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
            commands::migration::storage_export_backup,
            commands::migration::storage_import_backup,
            // 任务聚类命令
            commands::storage::storage_get_tasks,
            commands::storage::storage_get_related_screenshots,
            commands::storage::storage_get_task_screenshots,
            commands::storage::storage_update_task_label,
            commands::storage::storage_delete_task,
            commands::storage::storage_remove_task_screenshot,
            commands::storage::storage_merge_tasks,
            commands::storage::storage_save_clustering_results,
            analysis::get_analysis_overview,
            // MCP 服务命令
            commands::mcp::mcp_set_enabled,
            commands::mcp::mcp_get_status,
            commands::mcp::mcp_ack_privacy_warning,
            commands::mcp::mcp_reset_token,
            commands::mcp::mcp_copy_token_to_clipboard,
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
            model_management::get_model_inventory,
            // Updater commands
            updater::updater_check,
            updater::updater_install,
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
            commands::utility::check_smart_cluster_setup_needed,
            commands::utility::mark_smart_cluster_setup_done,
            commands::utility::get_extension_enhancement_config,
            commands::utility::set_extension_enhancement,
            commands::utility::get_nmh_sessions,
            // Smart Cluster commands
            commands::smart_cluster::smart_cluster_list,
            commands::smart_cluster::smart_cluster_get,
            commands::smart_cluster::smart_cluster_get_examples,
            commands::smart_cluster::smart_cluster_create,
            commands::smart_cluster::smart_cluster_delete,
            commands::smart_cluster::smart_cluster_update_anchor,
            commands::smart_cluster::smart_cluster_update_threshold,
            commands::smart_cluster::smart_cluster_toggle_enabled,
            commands::smart_cluster::smart_cluster_assignments,
            commands::smart_cluster::smart_cluster_ocr_corpus,
            commands::smart_cluster::smart_cluster_get_summary,
            commands::smart_cluster::smart_cluster_upsert_summary,
            commands::smart_cluster::smart_cluster_delete_summary,
            commands::smart_cluster::smart_cluster_rescan,
            commands::smart_cluster::smart_cluster_rescan_all,
            commands::smart_cluster::smart_cluster_clear_assignments,
            commands::smart_cluster::smart_cluster_status,
            // Idle / power
            idle::get_idle_state,
            // Error window commands
            commands::utility::get_log_dir,
            commands::utility::restart_app,
            commands::utility::trigger_test_error,
            commands::utility::exit_app,
            commands::utility::hide_to_tray,
            commands::utility::frontend_log,
            // 轻量模式命令
            commands::utility::switch_to_lightweight_mode,
            commands::utility::switch_to_standard_mode,
            commands::utility::get_lightweight_status,
            commands::utility::get_lightweight_config,
            commands::utility::set_lightweight_config,
            commands::utility::open_path,
            // Power saving mode commands
            power::get_power_saving_status,
            power::set_power_saving_enabled,
        ]);

    #[cfg(desktop)]
    {
        // 单实例保护应该始终启用，无论窗口是否隐藏
        // 这样可以防止多个实例竞争共享资源（SQLite、命名管道等）
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 如果窗口存在，聚焦它
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.show();
                let _ = window.unminimize();
            } else {
                // 如果窗口不存在（轻量模式），切换回标准模式
                if let Some(lightweight_state) = app.try_state::<Arc<LightweightModeState>>() {
                    if *lightweight_state.is_lightweight.lock().unwrap() {
                        let _ = crate::create_main_window(&app);
                        *lightweight_state.is_lightweight.lock().unwrap() = false;
                    }
                }
            }
        }));
    }

    builder
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            // 阻止应用在所有窗口关闭时退出
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !IS_UPDATING.load(Ordering::Relaxed) && !IS_QUITTING.load(Ordering::Relaxed) {
                    api.prevent_exit();
                }
            }
        });
}

pub fn run_silent_install() {
    python::run_silent_install();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_left_button_release_opens_window_from_tray() {
        assert!(is_open_tray_click(MouseButton::Left, MouseButtonState::Up));
        assert!(!is_open_tray_click(
            MouseButton::Left,
            MouseButtonState::Down
        ));
        assert!(!is_open_tray_click(
            MouseButton::Right,
            MouseButtonState::Up
        ));
    }

    #[test]
    fn vector_cleanup_is_only_skipped_when_monitor_is_off_and_disabled() {
        assert!(vector_cleanup_can_be_skipped(false, false));
        assert!(!vector_cleanup_can_be_skipped(true, false));
        assert!(!vector_cleanup_can_be_skipped(false, true));
        assert!(!vector_cleanup_can_be_skipped(true, true));
    }

    #[test]
    fn vector_cleanup_requires_explicit_success_ack() {
        assert!(vector_cleanup_acked(&Ok(
            serde_json::json!({ "status": "success", "vector_deleted": 3 })
        )));
        assert!(!vector_cleanup_acked(&Ok(
            serde_json::json!({ "error": "vector store unavailable" })
        )));
        assert!(!vector_cleanup_acked(&Ok(serde_json::json!({}))));
        assert!(!vector_cleanup_acked(&Err("Monitor not started".to_string())));
    }
}

pub fn run_python_launcher(args: &[String]) -> i32 {
    python_launcher::run_python_launcher(args)
}

pub fn run_cng_unlock(key_file_path: &str, owner_hwnd: Option<isize>) {
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

    match credential_manager::decrypt_master_key_with_cng_for_window(&ciphertext, owner_hwnd) {
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
