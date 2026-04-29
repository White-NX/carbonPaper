use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::registry_config;
use windows::Win32::System::Power::GetSystemPowerStatus;

/// Power saving mode state
pub struct PowerState {
    /// Whether power saving mode is enabled (user setting)
    pub enabled: AtomicBool,
    /// Whether power saving is currently active (AC disconnected)
    pub active: AtomicBool,
    /// Task handle for power monitoring loop
    pub monitor_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl PowerState {
    pub fn new() -> Self {
        let enabled = registry_config::get_bool("power_saving_mode_enabled").unwrap_or(true);
        Self {
            enabled: AtomicBool::new(enabled),
            active: AtomicBool::new(false),
            monitor_task: Mutex::new(None),
        }
    }
}

/// Check if AC power is connected (not on battery)
fn is_ac_power_connected() -> bool {
    unsafe {
        let mut status = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            // ACLineStatus: 0 = offline, 1 = online, 255 = unknown
            status.ACLineStatus == 1
        } else {
            // If we can't determine, assume AC is connected (fail-safe)
            true
        }
    }
}

/// Start the power monitoring loop
pub fn start_power_monitor(app: AppHandle) {
    let power_state = app.state::<Arc<PowerState>>();

    // Stop existing monitor if any
    {
        let mut guard = power_state.monitor_task.lock().unwrap();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    tracing::info!("Starting power monitor");

    let app_clone = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut last_ac_connected = is_ac_power_connected();

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Clone for spawned tasks
            let app_for_spawn = app_clone.clone();
            let power_state = app_clone.state::<Arc<PowerState>>();
            let enabled = power_state.enabled.load(Ordering::SeqCst);
            if !enabled {
                // Power saving mode disabled, reset and continue
                if power_state.active.load(Ordering::SeqCst) {
                    power_state.active.store(false, Ordering::SeqCst);
                    let _ = app_clone.emit("power-saving-changed", serde_json::json!({
                        "enabled": false,
                        "active": false,
                    }));
                }
                continue;
            }

            let current_ac_connected = is_ac_power_connected();

            // AC power disconnected -> activate power saving mode
            if !current_ac_connected && last_ac_connected {
                tracing::info!("Power: AC disconnected, activating power saving mode");

                // Stop monitor if running
                let monitor_state = app_clone.state::<crate::monitor::MonitorState>();
                let capture_state = app_clone.state::<Arc<crate::capture::CaptureState>>();
                let _ = crate::monitor::stop_monitor(monitor_state, capture_state).await;

                power_state.active.store(true, Ordering::SeqCst);
                let _ = app_clone.emit("power-saving-changed", serde_json::json!({
                    "enabled": true,
                    "active": true,
                }));

                let _ = app_clone.notification()
                    .builder()
                    .title("CarbonPaper")
                    .body("已切换到节能模式，交流电源已断开")
                    .show();
            }
            // AC power connected -> deactivate power saving mode
            else if current_ac_connected && !last_ac_connected {
                tracing::info!("Power: AC connected, deactivating power saving mode");

                // Resume monitor if auto-start is enabled
                let auto_start = registry_config::get_bool("autoStartMonitor").unwrap_or(true);
                if auto_start {
                    tauri::async_runtime::spawn(async move {
                        let monitor_state = app_for_spawn.state::<crate::monitor::MonitorState>();
                        if let Err(e) = crate::monitor::start_monitor(monitor_state, app_for_spawn.clone()).await {
                            tracing::error!("Failed to start monitor after power restored: {}", e);
                        }
                    });
                }

                power_state.active.store(false, Ordering::SeqCst);
                let _ = app_clone.emit("power-saving-changed", serde_json::json!({
                    "enabled": true,
                    "active": false,
                }));

                let _ = app_clone.notification()
                    .builder()
                    .title("CarbonPaper")
                    .body("已退出节能模式，交流电源已恢复")
                    .show();
            }

            last_ac_connected = current_ac_connected;
        }
    });

    let mut guard = power_state.monitor_task.lock().unwrap();
    *guard = Some(handle);
}

/// Stop the power monitoring loop
pub fn stop_power_monitor(app: &AppHandle) {
    let power_state = app.state::<Arc<PowerState>>();

    let mut guard = power_state.monitor_task.lock().unwrap();
    if let Some(handle) = guard.take() {
        handle.abort();
        tracing::info!("Power monitor stopped");
    }
}

// ==================== Tauri Commands ====================

#[tauri::command]
pub fn get_power_saving_status(power_state: tauri::State<'_, Arc<PowerState>>) -> serde_json::Value {
    serde_json::json!({
        "enabled": power_state.enabled.load(Ordering::SeqCst),
        "active": power_state.active.load(Ordering::SeqCst),
        "ac_connected": is_ac_power_connected(),
    })
}

#[tauri::command]
pub fn set_power_saving_enabled(power_state: tauri::State<'_, Arc<PowerState>>, enabled: bool) -> Result<(), String> {
    registry_config::set_bool("power_saving_mode_enabled", enabled)?;
    power_state.enabled.store(enabled, Ordering::SeqCst);

    // If disabling while active, reset active state
    if !enabled {
        power_state.active.store(false, Ordering::SeqCst);
    }

    tracing::info!("Power saving mode enabled: {}", enabled);
    Ok(())
}
