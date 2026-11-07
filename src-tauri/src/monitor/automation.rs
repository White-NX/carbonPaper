//! Application-owned capture intent and recovery. No WebView participates in
//! admission, and destroying a window does not reset a stop or retry limit.

use super::{ControlSource, MonitorAction, MonitorState};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

pub(crate) const RESTART_LIMIT: &str = "CAPTURE_RESTART_LIMIT";
pub(super) const MODELS_REQUIRED: &str =
    "Model files are incomplete. Please download required models first.";
const MAX_UNEXPECTED_STOPS: usize = 3;
const UNEXPECTED_STOP_WINDOW: Duration = Duration::from_secs(10 * 60);
const RECHECK_INTERVAL: Duration = Duration::from_secs(30);
const MAINTENANCE_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct AutomationState {
    pub policy: Mutex<Policy>,
    pub starting: AtomicBool,
    pub revision: AtomicU64,
    started: AtomicBool,
    wake: Notify,
}

#[derive(Default)]
pub(super) struct Policy {
    pub manual_stop: bool,
    pub manual_pause: bool,
    pub error: Option<String>,
    unexpected_stops: VecDeque<Instant>,
}

#[derive(Clone, Copy)]
pub(super) struct Conditions {
    pub running: bool,
    pub storage_ready: bool,
    pub onboarding_held: bool,
    pub auto_start: bool,
    pub power_blocked: bool,
    pub maintenance_active: bool,
    pub maintenance_resume_pending: bool,
}

impl Policy {
    pub fn automatic_action(&self, conditions: Conditions) -> Option<MonitorAction> {
        if conditions.maintenance_active {
            return None;
        }
        if conditions.running {
            if conditions.power_blocked {
                return Some(MonitorAction::Stop);
            }
            if conditions.maintenance_resume_pending {
                return Some(MonitorAction::ResumeAfterPowerSaving);
            }
            return None;
        }
        if conditions.storage_ready
            && !conditions.onboarding_held
            && conditions.auto_start
            && !conditions.power_blocked
            && !self.manual_stop
            && self.error.is_none()
        {
            Some(MonitorAction::Start)
        } else {
            None
        }
    }

    pub fn user_action(&mut self, action: MonitorAction, paused: bool) {
        match action {
            MonitorAction::Start | MonitorAction::Restart => self.request_start(),
            MonitorAction::Stop => {
                self.manual_stop = true;
                self.manual_pause = false;
                self.error = None;
            }
            MonitorAction::Pause => self.manual_pause = true,
            MonitorAction::Resume => self.manual_pause = false,
            MonitorAction::TogglePause => self.manual_pause = !paused,
            MonitorAction::ResumeAfterPowerSaving => {}
        }
    }

    pub fn request_start(&mut self) {
        self.manual_stop = false;
        self.manual_pause = false;
        self.error = None;
        self.unexpected_stops.clear();
    }

    pub fn finish_action(&mut self, result: &Result<String, String>, source: ControlSource) {
        if source == ControlSource::Maintenance {
            return;
        }
        match result {
            Ok(_) if source == ControlSource::User => self.error = None,
            Ok(_) => {}
            Err(error) if is_waiting_condition(error) => {}
            Err(error) => self.error = Some(error.clone()),
        }
    }

    pub fn unexpected_stop(&mut self, now: Instant) {
        self.unexpected_stops
            .retain(|at| now.saturating_duration_since(*at) < UNEXPECTED_STOP_WINDOW);
        self.unexpected_stops.push_back(now);
        if self.unexpected_stops.len() >= MAX_UNEXPECTED_STOPS {
            self.error = Some(RESTART_LIMIT.into());
        }
    }
}

fn is_waiting_condition(error: &str) -> bool {
    matches!(
        error,
        crate::power::POWER_SAVING_ACTIVE
            | crate::maintenance::MAINTENANCE_IN_PROGRESS
            | crate::storage_startup::STORAGE_INITIALIZING
            | MODELS_REQUIRED
    )
}

pub(super) fn conditions(app: &AppHandle, monitor: &MonitorState) -> Conditions {
    let running = monitor.is_running();
    Conditions {
        running,
        storage_ready: crate::storage_startup::is_ready(app),
        onboarding_held: !running && crate::onboarding::capture_held(),
        auto_start: super::get_monitor_autostart(),
        power_blocked: crate::power::is_saving_power(app),
        maintenance_active: crate::maintenance::is_active(),
        maintenance_resume_pending: monitor.maintenance_resume_pending(),
    }
}

pub(crate) fn wake(app: &AppHandle) {
    if let Some(monitor) = app.try_state::<MonitorState>() {
        monitor.automation.wake.notify_one();
    }
}

pub(crate) fn start(app: AppHandle) {
    let monitor = app.state::<MonitorState>();
    if monitor.automation.started.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            if crate::IS_QUITTING.load(Ordering::Relaxed)
                || crate::IS_UPDATING.load(Ordering::Relaxed)
            {
                break;
            }
            let monitor = app.state::<MonitorState>();
            let action = monitor
                .automation
                .policy
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .automatic_action(conditions(&app, &monitor));
            let mut maintenance_blocked = false;
            if let Some(action) = action {
                if let Err(error) = super::control_automatic(&app, action).await {
                    maintenance_blocked = error == crate::maintenance::MAINTENANCE_IN_PROGRESS;
                    if !is_waiting_condition(&error) {
                        tracing::error!("Automatic capture control failed: {error}");
                    }
                }
            }
            // Native changes wake immediately. The fallback covers external
            // preferences/files and a maintenance guard released just after
            // its final runtime action.
            let delay = if maintenance_blocked || crate::maintenance::is_active() {
                MAINTENANCE_RECHECK_INTERVAL
            } else {
                RECHECK_INTERVAL
            };
            tokio::select! {
                _ = monitor.automation.wake.notified() => {},
                _ = tokio::time::sleep(delay) => {},
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ready() -> Conditions {
        Conditions {
            running: false,
            storage_ready: true,
            onboarding_held: false,
            auto_start: true,
            power_blocked: false,
            maintenance_active: false,
            maintenance_resume_pending: false,
        }
    }

    #[test]
    fn cold_start_waits_for_native_readiness_and_user_choices() {
        let policy = Policy::default();
        assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
        for blocked in [
            Conditions {
                storage_ready: false,
                ..ready()
            },
            Conditions {
                onboarding_held: true,
                ..ready()
            },
            Conditions {
                auto_start: false,
                ..ready()
            },
            Conditions {
                power_blocked: true,
                ..ready()
            },
            Conditions {
                maintenance_active: true,
                ..ready()
            },
        ] {
            assert_eq!(policy.automatic_action(blocked), None);
        }
    }

    #[test]
    fn manual_stop_survives_reopens_and_power_transitions_until_explicit_start() {
        let mut policy = Policy::default();
        policy.user_action(MonitorAction::Stop, false);
        for conditions in [
            ready(),
            Conditions {
                power_blocked: true,
                ..ready()
            },
            Conditions {
                storage_ready: false,
                ..ready()
            },
            ready(),
        ] {
            assert_eq!(policy.automatic_action(conditions), None);
        }
        policy.user_action(MonitorAction::Start, false);
        assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
    }

    #[tokio::test]
    async fn a_queued_automatic_start_rechecks_intent_after_a_manual_stop() {
        let monitor = Arc::new(MonitorState::new());
        let control = monitor.control.lock().await;
        let worker = monitor.clone();
        let queued = tokio::spawn(async move {
            let _control = worker.control.lock().await;
            let action = worker
                .automation
                .policy
                .lock()
                .unwrap()
                .automatic_action(ready());
            action
        });
        monitor
            .automation
            .policy
            .lock()
            .unwrap()
            .user_action(MonitorAction::Stop, false);
        drop(control);
        assert_eq!(queued.await.unwrap(), None);
    }

    #[test]
    fn repeated_unexpected_exits_stop_recovery_until_an_explicit_retry() {
        let mut policy = Policy::default();
        let now = Instant::now();
        for offset in 0..2 {
            policy.unexpected_stop(now + Duration::from_secs(offset));
            assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
            policy.finish_action(&Ok("started".into()), ControlSource::Automatic);
        }
        policy.unexpected_stop(now + Duration::from_secs(2));
        assert_eq!(policy.error.as_deref(), Some(RESTART_LIMIT));
        assert_eq!(policy.automatic_action(ready()), None);
        policy.request_start();
        assert!(policy.error.is_none());
        assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
        policy.unexpected_stop(now + Duration::from_secs(3));
        assert!(policy.error.is_none());
    }

    #[test]
    fn isolated_exits_outside_the_window_do_not_trip_the_retry_limit() {
        let mut policy = Policy::default();
        let now = Instant::now();
        for offset in 0..4 {
            policy.unexpected_stop(now + UNEXPECTED_STOP_WINDOW * offset);
            assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
        }
    }

    #[test]
    fn an_automatic_action_finishing_late_does_not_clear_a_new_retry_limit() {
        let mut policy = Policy::default();
        let now = Instant::now();
        for _ in 0..MAX_UNEXPECTED_STOPS {
            policy.unexpected_stop(now);
        }
        policy.finish_action(&Ok("started".into()), ControlSource::Automatic);
        assert_eq!(policy.error.as_deref(), Some(RESTART_LIMIT));
        assert_eq!(policy.automatic_action(ready()), None);
    }

    #[test]
    fn missing_models_and_temporary_admission_blocks_wait_for_native_changes() {
        let mut policy = Policy::default();
        for error in [
            MODELS_REQUIRED,
            crate::power::POWER_SAVING_ACTIVE,
            crate::storage_startup::STORAGE_INITIALIZING,
            crate::maintenance::MAINTENANCE_IN_PROGRESS,
        ] {
            policy.finish_action(&Err(error.into()), ControlSource::Automatic);
            assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
        }
        policy.finish_action(
            &Err("capture startup failed".into()),
            ControlSource::Automatic,
        );
        assert_eq!(policy.automatic_action(ready()), None);
        policy.request_start();
        assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
    }

    #[test]
    fn battery_stop_preserves_pause_and_resumes_only_when_autostart_is_enabled() {
        let mut policy = Policy::default();
        policy.user_action(MonitorAction::Pause, false);
        assert_eq!(
            policy.automatic_action(Conditions {
                running: true,
                power_blocked: true,
                ..ready()
            }),
            Some(MonitorAction::Stop)
        );
        policy.finish_action(&Ok("stopped".into()), ControlSource::Automatic);
        assert!(policy.manual_pause);
        assert_eq!(policy.automatic_action(ready()), Some(MonitorAction::Start));
        assert_eq!(
            policy.automatic_action(Conditions {
                auto_start: false,
                ..ready()
            }),
            None
        );
    }

    #[test]
    fn maintenance_restore_does_not_clear_manual_intent_or_a_recovery_limit() {
        let mut policy = Policy::default();
        policy.user_action(MonitorAction::Stop, false);
        policy.error = Some(RESTART_LIMIT.into());
        policy.finish_action(&Ok("restored".into()), ControlSource::Maintenance);
        assert!(policy.manual_stop);
        assert_eq!(policy.error.as_deref(), Some(RESTART_LIMIT));
        assert_eq!(
            policy.automatic_action(Conditions {
                running: true,
                auto_start: false,
                maintenance_resume_pending: true,
                ..ready()
            }),
            Some(MonitorAction::ResumeAfterPowerSaving)
        );
    }
}
