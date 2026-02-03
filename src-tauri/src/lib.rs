mod autostart;
mod analysis;
mod monitor;
mod python;
mod resource_utils;
mod model_management;

use autostart::{get_autostart_status, set_autostart};
use analysis::AnalysisState;
use monitor::MonitorState;
use std::sync::Mutex;
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
                tauri::async_runtime::spawn(async move {
                    let _ = monitor::pause_monitor().await;
                });
            }
            MENU_ID_RESUME => {
                tauri::async_runtime::spawn(async move {
                    let _ = monitor::resume_monitor().await;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState {
            process: Mutex::new(None),
        })
        .manage(AnalysisState::default())
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
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, cwd| {
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
