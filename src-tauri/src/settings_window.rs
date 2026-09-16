//! The settings window and its small, authenticated window boundary.
//!
//! Monitor actions are executed by the existing main UI lifecycle owner. This
//! preserves manual-stop suppression without granting auxiliary windows access
//! to the main-only runtime commands or an arbitrary command proxy.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;

pub const LABEL: &str = "settings";
type ActionResult = Result<(), String>;

struct PendingAction {
    action: MonitorAction,
    claimed: bool,
    sender: oneshot::Sender<ActionResult>,
}

fn claim_action(pending: &mut HashMap<u64, PendingAction>, id: u64) -> Option<MonitorAction> {
    let request = pending.get_mut(&id)?;
    if request.claimed {
        return None;
    }
    request.claimed = true;
    Some(request.action.clone())
}

#[derive(Default)]
pub struct SettingsWindowState {
    opening: Mutex<()>,
    sequence: AtomicU64,
    pending: Mutex<HashMap<u64, PendingAction>>,
    busy: AtomicBool,
    auth_state: Mutex<Option<bool>>,
}

pub fn is_settings_ui(label: &str) -> bool {
    matches!(label, "main" | "settings")
}

/// Only use this guard for commands used by settings; main-only lifecycle
/// commands deliberately continue to use check_main_window.
pub fn check_settings_ui(window: &tauri::Window) -> Result<(), String> {
    if is_settings_ui(window.label()) {
        Ok(())
    } else {
        Err("WINDOW_NOT_AUTHORIZED".into())
    }
}

fn check_settings_window(window: &tauri::Window) -> Result<(), String> {
    if window.label() == LABEL {
        Ok(())
    } else {
        Err("WINDOW_NOT_AUTHORIZED".into())
    }
}

#[derive(Clone, Serialize)]
pub struct NavigationTarget {
    tab: String,
    section: Option<String>,
}

fn navigation_target(tab: Option<String>, section: Option<String>) -> NavigationTarget {
    let tab = tab
        .filter(|tab| {
            matches!(
                tab.as_str(),
                "general"
                    | "capture"
                    | "organize"
                    | "privacy"
                    | "maintenance"
                    | "advanced"
                    | "about"
            )
        })
        .unwrap_or_else(|| "general".into());
    let section = section.filter(|value| {
        value.len() <= 80
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    });
    NavigationTarget { tab, section }
}

fn window_size(available_width: f64, available_height: f64) -> (f64, f64, f64, f64) {
    let width = 960.0_f64.min((available_width - 48.0).max(320.0));
    let height = 720.0_f64.min((available_height - 80.0).max(320.0));
    (width, height, 760.0_f64.min(width), 520.0_f64.min(height))
}

#[tauri::command]
pub async fn open_settings_window(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<'_, SettingsWindowState>,
    tab: Option<String>,
    section: Option<String>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    let _opening = state.opening.lock().map_err(|e| e.to_string())?;
    let target = navigation_target(tab, section);
    crate::cancel_auto_lightweight_timer(&app);
    if let Some(existing) = app.get_webview_window(LABEL) {
        existing.unminimize().map_err(|e| e.to_string())?;
        existing.show().map_err(|e| e.to_string())?;
        existing.set_focus().map_err(|e| e.to_string())?;
        existing
            .emit("settings-navigate", &target)
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let monitor = window.current_monitor().ok().flatten();
    let (width, height, min_width, min_height) = monitor
        .as_ref()
        .map(|monitor| {
            let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
            window_size(size.width, size.height)
        })
        .unwrap_or_else(|| window_size(1300.0, 800.0));
    let url = format!(
        "index.html?window=settings&tab={}&section={}",
        target.tab,
        target.section.as_deref().unwrap_or("")
    );
    let settings =
        tauri::WebviewWindowBuilder::new(&app, LABEL, tauri::WebviewUrl::App(url.into()))
            .title("CarbonPaper")
            .inner_size(width, height)
            .min_inner_size(min_width, min_height)
            .resizable(true)
            .decorations(false)
            .transparent(false)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let position = monitor.position();
        let size = monitor.size();
        let _ = settings.set_position(tauri::PhysicalPosition::new(
            position.x + ((size.width as f64 - width * scale) / 2.0) as i32,
            position.y + ((size.height as f64 - height * scale) / 2.0) as i32,
        ));
    }
    settings.show().map_err(|e| e.to_string())?;
    settings.set_focus().map_err(|e| e.to_string())?;
    refresh_ui_foreground(&app);
    let _ = app.emit_to("main", "settings-window-changed", true);
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MonitorAction {
    Start,
    Stop,
    Pause,
    Resume,
    Restart,
    MaintenanceStop,
    MaintenanceStart,
}

#[tauri::command]
pub async fn settings_monitor_action(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<'_, SettingsWindowState>,
    action: MonitorAction,
) -> Result<(), String> {
    check_settings_window(&window)?;
    if app.get_webview_window("main").is_none() {
        return Err("SETTINGS_HOST_UNAVAILABLE".into());
    }
    let id = state.sequence.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = oneshot::channel();
    {
        let mut pending = state.pending.lock().map_err(|e| e.to_string())?;
        if !pending.is_empty() {
            return Err("MONITOR_ACTION_BUSY".into());
        }
        pending.insert(
            id,
            PendingAction {
                action,
                claimed: false,
                sender,
            },
        );
    }
    if let Err(error) = app.emit_to(
        "main",
        "settings-monitor-request",
        serde_json::json!({ "id": id }),
    ) {
        state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        return Err(error.to_string());
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), receiver).await;
    state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    result
        .map_err(|_| "MONITOR_ACTION_TIMEOUT".to_string())?
        .map_err(|_| "SETTINGS_HOST_UNAVAILABLE".to_string())?
}

#[tauri::command]
pub fn take_settings_monitor_action(
    window: tauri::Window,
    state: tauri::State<'_, SettingsWindowState>,
    id: u64,
) -> Result<Option<MonitorAction>, String> {
    crate::commands::check_main_window(&window)?;
    let mut pending = state.pending.lock().map_err(|e| e.to_string())?;
    Ok(claim_action(&mut pending, id))
}

#[tauri::command]
pub fn complete_settings_monitor_action(
    window: tauri::Window,
    state: tauri::State<'_, SettingsWindowState>,
    id: u64,
    error: Option<String>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    if let Some(request) = state.pending.lock().map_err(|e| e.to_string())?.remove(&id) {
        let _ = request.sender.send(error.map_or(Ok(()), Err));
    }
    Ok(())
}

#[tauri::command]
pub fn settings_set_busy(
    window: tauri::Window,
    state: tauri::State<'_, SettingsWindowState>,
    busy: bool,
) -> Result<(), String> {
    check_settings_window(&window)?;
    state.busy.store(busy, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub async fn close_settings_window(
    app: tauri::AppHandle,
    window: tauri::Window,
    lightweight: Option<bool>,
) -> Result<(), String> {
    check_settings_window(&window)?;
    let state = app.state::<SettingsWindowState>();
    if state.busy.load(Ordering::SeqCst) {
        return Err("SETTINGS_OPERATION_BUSY".into());
    }
    window.destroy().map_err(|e| e.to_string())?;
    if lightweight.unwrap_or(false) {
        // Destroyed is delivered before the window manager necessarily drops
        // its handle. Wait for removal before the lightweight-mode guard.
        for _ in 0..100 {
            if app.get_webview_window(LABEL).is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        crate::commands::utility::switch_to_lightweight_mode(
            app.clone(),
            app.state::<Arc<crate::LightweightModeState>>(),
        )
        .await?;
    }
    Ok(())
}

#[tauri::command]
pub fn settings_preferences_changed(
    app: tauri::AppHandle,
    window: tauri::Window,
    keys: Vec<String>,
) -> Result<(), String> {
    check_settings_ui(&window)?;
    if keys.len() > 20
        || keys.iter().any(|key| {
            !matches!(
                key.as_str(),
                "advanced"
                    | "power"
                    | "autostart"
                    | "session"
                    | "models"
                    | "records"
                    | "storage"
                    | "filters"
            )
        })
    {
        return Err("INVALID_SETTINGS_KEYS".into());
    }
    app.emit("settings-preferences-changed", keys)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_debug_preview(
    app: tauri::AppHandle,
    window: tauri::Window,
    credentials: tauri::State<'_, Arc<crate::CredentialManagerState>>,
    preview: String,
) -> Result<(), String> {
    check_settings_window(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    if !cfg!(debug_assertions)
        || !matches!(
            preview.as_str(),
            "error"
                | "security"
                | "ocr"
                | "update"
                | "critical-update"
                | "extension"
                | "clustering"
                | "smart-cluster"
                | "app-bound"
                | "app-bound-repair"
        )
    {
        return Err("PREVIEW_UNAVAILABLE".into());
    }
    app.emit_to("main", "settings-debug-preview", preview)
        .map_err(|e| e.to_string())
}

pub fn broadcast_auth_state(app: &tauri::AppHandle) {
    let Some(credentials) = app.try_state::<Arc<crate::CredentialManagerState>>() else {
        return;
    };
    let Some(state) = app.try_state::<SettingsWindowState>() else {
        return;
    };
    let valid = credentials.is_session_valid();
    let mut previous = state.auth_state.lock().unwrap_or_else(|e| e.into_inner());
    if *previous != Some(valid) {
        *previous = Some(valid);
        drop(previous);
        let _ = app.emit("auth-session-changed", valid);
    }
}

pub fn refresh_ui_foreground(app: &tauri::AppHandle) {
    let foreground = ["main", LABEL].iter().any(|label| {
        app.get_webview_window(label).is_some_and(|window| {
            window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
        })
    });
    if let Some(credentials) = app.try_state::<Arc<crate::CredentialManagerState>>() {
        credentials.set_foreground_state(foreground);
    }
    broadcast_auth_state(app);
}

pub fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    let app = window.app_handle();
    if window.label() == LABEL {
        match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.emit("settings-close-requested", false);
            }
            tauri::WindowEvent::Destroyed => {
                let state = app.state::<SettingsWindowState>();
                state.busy.store(false, Ordering::SeqCst);
                let _ = app.emit_to("main", "settings-window-changed", false);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    for _ in 0..100 {
                        if app.get_webview_window(LABEL).is_none() {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    refresh_ui_foreground(&app);
                    if app
                        .get_webview_window("main")
                        .is_some_and(|main| !main.is_visible().unwrap_or(true))
                    {
                        crate::start_auto_lightweight_timer(app);
                    }
                });
            }
            _ => {}
        }
    }
    if is_settings_ui(window.label())
        && matches!(
            event,
            tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Resized(_)
        )
    {
        refresh_ui_foreground(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn events_cannot_invent_or_replay_monitor_requests() {
        let mut pending = HashMap::new();
        assert!(claim_action(&mut pending, 4).is_none());
        let (sender, _receiver) = oneshot::channel();
        pending.insert(
            4,
            PendingAction {
                action: MonitorAction::Stop,
                claimed: false,
                sender,
            },
        );
        assert!(matches!(
            claim_action(&mut pending, 4),
            Some(MonitorAction::Stop)
        ));
        assert!(claim_action(&mut pending, 4).is_none());
    }
    #[test]
    fn auxiliary_windows_cannot_use_the_settings_boundary() {
        assert!(is_settings_ui("main"));
        assert!(is_settings_ui("settings"));
        for label in ["snapshot-preview", "error", "settings-preview", ""] {
            assert!(!is_settings_ui(label));
        }
    }
    #[test]
    fn navigation_accepts_only_local_sections() {
        let target = navigation_target(Some("unknown".into()), Some("https://example.com".into()));
        assert_eq!(target.tab, "general");
        assert!(target.section.is_none());
        assert_eq!(
            navigation_target(Some("advanced".into()), Some("ocr-repair".into()))
                .section
                .as_deref(),
            Some("ocr-repair")
        );
    }
    #[test]
    fn small_high_dpi_screens_can_fit_the_window() {
        let (width, height, min_width, min_height) = window_size(800.0, 540.0);
        assert!(width < 800.0 && height < 540.0);
        assert!(min_width <= width && min_height <= height);
    }
    #[test]
    fn monitor_bridge_rejects_arbitrary_commands() {
        assert!(serde_json::from_str::<MonitorAction>("\"stop\"").is_ok());
        assert!(serde_json::from_str::<MonitorAction>("\"exit_app\"").is_err());
    }
}
