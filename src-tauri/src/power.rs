//! AC-power monitoring and automatic power-saving state transitions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Notify;

use crate::credential_manager::CredentialManagerState;
use crate::monitor::MonitorAction;
use crate::registry_config;
use windows::Win32::System::Power::GetSystemPowerStatus;

pub(crate) const POWER_SAVING_ACTIVE: &str = "POWER_SAVING_ACTIVE";
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(5);

// Capture postprocessing shares the semantic worker with explicit user queries.
// Carry its power policy to queued requests, cold startup and in-flight watches
// without cancelling a foreground query or a manually requested index run.
tokio::task_local! { pub(crate) static CAPTURE_WORK: Arc<PowerState>; }

pub(crate) fn current_capture_power() -> Option<Arc<PowerState>> {
    CAPTURE_WORK.try_with(Arc::clone).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
struct PowerStatus {
    enabled: bool,
    active: bool,
    ac_connected: bool,
}

impl PowerStatus {
    fn new(enabled: bool, ac_connected: bool) -> Self {
        Self {
            enabled,
            active: enabled && !ac_connected,
            ac_connected,
        }
    }

    fn monitor_action(
        self,
        previous: Option<Self>,
        running: bool,
        auto_start: bool,
    ) -> Option<MonitorAction> {
        if self.active && running {
            // Reconcile every pass: a failed stop must not consume the only AC edge.
            Some(MonitorAction::Stop)
        } else if !self.active && previous.is_some_and(|status| status.active) && auto_start {
            Some(MonitorAction::Start)
        } else {
            None
        }
    }
}

pub struct PowerState {
    pub enabled: AtomicBool,
    /// Last published state; admission always checks the current power source.
    pub active: AtomicBool,
    monitor_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    wake: Notify,
}

impl PowerState {
    pub fn new() -> Self {
        let enabled = registry_config::get_bool("power_saving_mode_enabled").unwrap_or(true);
        Self::with_status(PowerStatus::new(enabled, is_ac_power_connected()))
    }

    fn with_status(status: PowerStatus) -> Self {
        Self {
            enabled: AtomicBool::new(status.enabled),
            active: AtomicBool::new(status.active),
            monitor_task: Mutex::new(None),
            wake: Notify::new(),
        }
    }

    fn status(&self) -> PowerStatus {
        PowerStatus::new(self.enabled.load(Ordering::SeqCst), is_ac_power_connected())
    }

    pub(crate) fn is_active(&self) -> bool {
        self.enabled.load(Ordering::SeqCst) && !is_ac_power_connected()
    }

    fn publish(&self, app: &AppHandle, status: PowerStatus) {
        self.active.store(status.active, Ordering::SeqCst);
        let _ = app.emit("power-saving-changed", status);
    }
}

/// Unknown power status is not evidence of battery use (including desktops
/// without a battery). Only an explicit offline reading closes the AC gate.
fn ac_connected_from_status(ac_line_status: u8) -> bool {
    ac_line_status != 0
}

pub(crate) fn is_ac_power_connected() -> bool {
    // SAFETY: status is writable storage of the exact Win32 structure type;
    // GetSystemPowerStatus writes it synchronously and retains no pointer.
    unsafe {
        let mut status = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            ac_connected_from_status(status.ACLineStatus)
        } else {
            true
        }
    }
}

/// Read the live source, not the monitor's five-second-old cached flag.
pub(crate) fn is_saving_power(app: &AppHandle) -> bool {
    app.try_state::<Arc<PowerState>>()
        .is_some_and(|state| state.is_active())
}

pub(crate) fn check_capture_allowed(app: &AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<Arc<PowerState>>() {
        let status = state.status();
        if status.active {
            state.publish(app, status);
            state.wake.notify_one();
            return Err(POWER_SAVING_ACTIVE.into());
        }
    }
    Ok(())
}

pub fn start_power_monitor(app: AppHandle) {
    let power_state = app.state::<Arc<PowerState>>().inner().clone();
    let mut task = power_state
        .monitor_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = task.take() {
        handle.abort();
    }

    tracing::info!("Starting power monitor");
    let state = power_state.clone();
    *task = Some(tauri::async_runtime::spawn(async move {
        let mut previous: Option<PowerStatus> = None;
        let mut resume_pending = false;
        loop {
            let status = state.status();
            if previous != Some(status) || state.active.load(Ordering::SeqCst) != status.active {
                // Publish BEFORE stopping. The stopped event can otherwise
                // make frontend auto-start race the power-saving event.
                state.publish(&app, status);
                tracing::info!(
                    "Power policy: enabled={} ac_connected={} active={}",
                    status.enabled,
                    status.ac_connected,
                    status.active
                );
                if let Some(scheduler) =
                    app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
                {
                    scheduler.observe_activity(&app);
                    scheduler.wake();
                }
            }

            let running = app.state::<crate::monitor::MonitorState>().is_running();
            let auto_start = registry_config::get_bool("autoStartMonitor").unwrap_or(true)
                && !crate::onboarding::capture_held();
            if status.active || !auto_start || running {
                resume_pending = false;
            }
            let action = status
                .monitor_action(previous, running, auto_start)
                .or_else(|| resume_pending.then_some(MonitorAction::Start));
            resume_pending = false;
            if let Some(action) = action {
                if let Err(error) = crate::monitor::control(&app, action).await {
                    resume_pending = matches!(action, MonitorAction::Start)
                        && error == crate::maintenance::MAINTENANCE_IN_PROGRESS;
                    tracing::warn!("Power policy could not update capture: {error}");
                }
            }
            if status.active {
                // The capture drain preserves already saved frames. Release
                // the resident OCR worker too, including a long-running OCR.
                let ml = app
                    .state::<Arc<crate::ml_runtime::MlRuntimeState>>()
                    .inner()
                    .clone();
                let power = state.clone();
                let _ = tokio::task::spawn_blocking(move || ml.stop_for_power_saving(&power)).await;
            }

            if previous.is_some_and(|old| old.active != status.active) && status.enabled {
                let _ = app
                    .notification()
                    .builder()
                    .title("CarbonPaper")
                    .body(if status.active {
                        "已切换到节能模式，交流电源已断开"
                    } else {
                        "已退出节能模式，交流电源已恢复"
                    })
                    .show();
            }
            previous = Some(status);
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {},
                _ = state.wake.notified() => {},
            }
        }
    }));
}

// ==================== Tauri Commands ====================

#[tauri::command]
pub fn get_power_saving_status(
    power_state: tauri::State<'_, Arc<PowerState>>,
) -> serde_json::Value {
    serde_json::to_value(power_state.status()).expect("power status is serializable")
}

#[tauri::command]
pub fn set_power_saving_enabled(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    power_state: tauri::State<'_, Arc<PowerState>>,
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credential_state)?;

    registry_config::set_bool("power_saving_mode_enabled", enabled)?;
    power_state.enabled.store(enabled, Ordering::SeqCst);
    power_state.publish(&app, power_state.status());
    // Enabling while already unplugged must apply without another AC edge.
    power_state.wake.notify_one();
    tracing::info!("Power saving mode enabled: {enabled}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_on_battery_publishes_active_and_stops_existing_capture() {
        let status = PowerStatus::new(true, false);
        let state = PowerState::with_status(status);
        assert!(state.active.load(Ordering::SeqCst));
        assert!(matches!(
            status.monitor_action(None, true, true),
            Some(MonitorAction::Stop)
        ));
        assert!(status.monitor_action(None, false, true).is_none());
    }

    #[test]
    fn enabling_on_battery_and_a_missed_stop_do_not_require_an_ac_edge() {
        let disabled = PowerStatus::new(false, false);
        let enabled = PowerStatus::new(true, false);
        assert!(matches!(
            enabled.monitor_action(Some(disabled), true, true),
            Some(MonitorAction::Stop)
        ));
        assert!(matches!(
            enabled.monitor_action(Some(enabled), true, true),
            Some(MonitorAction::Stop)
        ));
    }

    #[test]
    fn releasing_power_saving_respects_autostart_and_does_not_start_on_first_ac_probe() {
        let battery = PowerStatus::new(true, false);
        for released in [PowerStatus::new(true, true), PowerStatus::new(false, false)] {
            assert!(matches!(
                released.monitor_action(Some(battery), false, true),
                Some(MonitorAction::Start)
            ));
            assert!(released
                .monitor_action(Some(battery), false, false)
                .is_none());
            assert!(released
                .monitor_action(Some(released), false, true)
                .is_none());
            assert!(released.monitor_action(None, false, true).is_none());
        }
    }

    #[test]
    fn only_explicit_offline_status_counts_as_battery_power() {
        assert!(!ac_connected_from_status(0));
        assert!(ac_connected_from_status(1));
        assert!(ac_connected_from_status(255));
    }
}
