//! System idle / activity detection.
//!
//! Combines three signals into a coarse "is the user actually using the
//! machine right now?" decision:
//!   1. Time since last keyboard/mouse input (`GetLastInputInfo`)
//!   2. Foreground window is a non-browser fullscreen application
//!      (reuses `capture::check_foreground_fullscreen`)
//!   3. AC power connected, read directly from `power::is_ac_power_connected`
//!
//! Emits a `system-idle-changed` Tauri event whenever the composite state
//! flips. Heavy ML work (smart cluster reranker, MiniLM capture indexing,
//! future LLM evaluators) gates on this signal so foreground apps and games are
//! never disturbed.
//!
//! **On the third signal.** This used to be read indirectly, as
//! `!PowerState.active` — a flag that only rises when the user has left power
//! saving mode *enabled* and the machine then loses AC. That made the gate mean
//! two different things depending on a setting nobody connected to background
//! inference: with power saving on, unplugging closed the gate (and stopped
//! capture with it, so no backlog accumulated); with power saving off,
//! unplugging left the gate wide open and a 118 MB model load ran on battery.
//! The condition is now the power state itself, so "background machine learning
//! does not run on battery" holds in both configurations. `power_saving_active`
//! stays observable in the diagnostic, because it explains *why* capture
//! stopped, which is a different question from why indexing did.

use crate::power::is_ac_power_connected;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Default idle threshold — user must be away from input for at least this
/// many seconds before background ML work is allowed to start.
pub const IDLE_THRESHOLD_SECS: u64 = 1800;

/// Polling cadence for the idle monitor loop.
const POLL_INTERVAL_SECS: u64 = 10;

pub struct IdleState {
    /// Last computed idle time in seconds (best-effort, updated every POLL_INTERVAL_SECS).
    pub idle_secs: AtomicU64,
    /// Whether the foreground window is a fullscreen exclusive app (e.g. game).
    pub fullscreen_exclusive: AtomicBool,
    /// Whether the machine is on AC power. Read from `GetSystemPowerStatus`,
    /// not inferred from the power-saving setting, and fail-safe `true` when
    /// Windows cannot answer — a desktop with no battery must not look unplugged.
    pub ac_connected: AtomicBool,
    /// Whether the composite idle gate is open.
    pub is_idle: AtomicBool,
    /// Set to true when the monitor task should stop. The currently-running
    /// task observes this and exits on the next poll; a fresh task created
    /// by `start_idle_monitor` resets it back to false under the same lock.
    shutdown_flag: Arc<AtomicBool>,
    /// Background monitor task handle. Held under the same lock as
    /// shutdown_flag so abort+swap+spawn is one atomic critical section.
    monitor_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl IdleState {
    pub fn new() -> Self {
        Self {
            idle_secs: AtomicU64::new(0),
            fullscreen_exclusive: AtomicBool::new(false),
            ac_connected: AtomicBool::new(true),
            is_idle: AtomicBool::new(false),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            monitor_task: Mutex::new(None),
        }
    }
}

/// Returns seconds since last keyboard/mouse input.
fn get_idle_seconds() -> u64 {
    // SAFETY: `info` has the required `cbSize`, points to writable initialized memory,
    // and Windows writes it synchronously without retaining the pointer.
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut info).as_bool() {
            let now = GetTickCount();
            // GetTickCount wraps every ~49.7 days; subtraction in u32 wraps too,
            // so we naturally get a valid delta within that window.
            let elapsed_ms = now.wrapping_sub(info.dwTime);
            (elapsed_ms / 1000) as u64
        } else {
            // Conservative: assume no idle when we can't measure, so background
            // ML stays gated off rather than running blindly.
            0
        }
    }
}

/// Determine whether the foreground window is a non-browser fullscreen app.
/// Returns true only when it's safe to assume the user is in an exclusive
/// session (game, video playback, presentation) that we must not disturb.
fn is_foreground_fullscreen_exclusive() -> bool {
    match crate::capture::check_foreground_fullscreen() {
        Some((process_name, _class, true)) => {
            // Treat browsers and CarbonPaper itself as "not exclusive" — those
            // are fine to compete with for compute.
            let pn = process_name.to_lowercase();
            let is_browser = matches!(
                pn.as_str(),
                "chrome.exe" | "msedge.exe" | "firefox.exe" | "brave.exe" | "opera.exe"
            );
            let is_self = pn == "carbonpaper.exe";
            !is_browser && !is_self
        }
        _ => false,
    }
}

/// The composite gate, in one place so the monitor loop and its tests cannot
/// drift apart. All three signals are required: the user has been away, no
/// exclusive fullscreen application owns the screen, and the machine is on
/// mains power.
fn composite_idle(idle_secs: u64, fullscreen: bool, ac_connected: bool) -> bool {
    idle_secs >= IDLE_THRESHOLD_SECS && !fullscreen && ac_connected
}

/// Start the idle monitor loop. Emits `system-idle-changed` on state flips.
///
/// Idempotent: if a previous monitor task is running it is signalled to
/// stop and aborted under the same lock as the new task is created, so
/// two callers racing on this function cannot leave orphan loops behind.
pub fn start_idle_monitor(app: AppHandle) {
    let idle_state = app.state::<Arc<IdleState>>().inner().clone();

    // Single critical section: tell whatever is running to exit, abort its
    // handle, reset the flag for the new task, then spawn + store. Two
    // concurrent calls serialise on this mutex and cannot leak tasks.
    let mut guard = idle_state
        .monitor_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    idle_state.shutdown_flag.store(true, Ordering::SeqCst);
    if let Some(handle) = guard.take() {
        handle.abort();
    }
    idle_state.shutdown_flag.store(false, Ordering::SeqCst);

    let composite_shutdown = idle_state.shutdown_flag.clone();
    let app_clone = app.clone();
    tracing::info!("Starting idle monitor (threshold={}s)", IDLE_THRESHOLD_SECS);

    let handle = tauri::async_runtime::spawn(async move {
        let mut last_emitted_idle: Option<bool> = None;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;

            if composite_shutdown.load(Ordering::SeqCst) {
                tracing::info!("Idle monitor exiting on shutdown flag");
                break;
            }

            // Win32 calls (GetLastInputInfo + foreground/monitor lookup +
            // GetSystemPowerStatus) are synchronous and can block briefly on
            // contended desktops; run them on a blocking thread so the async
            // runtime keeps dispatching reverse-IPC, monitor heartbeats etc.
            let probe = tokio::task::spawn_blocking(|| {
                let secs = get_idle_seconds();
                let fullscreen = is_foreground_fullscreen_exclusive();
                let ac_connected = is_ac_power_connected();
                (secs, fullscreen, ac_connected)
            })
            .await;

            let (idle_secs, fullscreen, ac_connected) = match probe {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("idle probe join failed: {}", e);
                    continue;
                }
            };

            // Recorded for the diagnostic only. Power saving stops capture when
            // it activates; the gate below is about whether background
            // inference may run, which is a separate question with a separate
            // answer on a machine whose owner turned power saving off.
            let power_saving_active = match app_clone.try_state::<Arc<crate::power::PowerState>>() {
                Some(power_state) => power_state.active.load(Ordering::SeqCst),
                None => false,
            };

            let is_idle = composite_idle(idle_secs, fullscreen, ac_connected);

            // Update state atomics
            let st = match app_clone.try_state::<Arc<IdleState>>() {
                Some(st) => st.inner().clone(),
                None => {
                    tracing::info!("IdleState gone; idle monitor exiting");
                    break;
                }
            };
            st.idle_secs.store(idle_secs, Ordering::SeqCst);
            st.fullscreen_exclusive.store(fullscreen, Ordering::SeqCst);
            st.ac_connected.store(ac_connected, Ordering::SeqCst);
            st.is_idle.store(is_idle, Ordering::SeqCst);

            // Emit event only on state flips to keep noise low.
            if last_emitted_idle != Some(is_idle) {
                let _ = app_clone.emit(
                    "system-idle-changed",
                    serde_json::json!({
                        "is_idle": is_idle,
                        "idle_secs": idle_secs,
                        "fullscreen_exclusive": fullscreen,
                        "ac_connected": ac_connected,
                        "power_saving_active": power_saving_active,
                    }),
                );
                tracing::info!(
                    "Idle state changed: is_idle={} idle_secs={} fullscreen={} ac={} power_saving_active={}",
                    is_idle,
                    idle_secs,
                    fullscreen,
                    ac_connected,
                    power_saving_active
                );
                last_emitted_idle = Some(is_idle);
            }
        }
    });

    *guard = Some(handle);
}

/// Signal the running idle monitor (if any) to stop. The spawned task
/// observes the flag on its next poll and exits cleanly; the join handle
/// is also aborted so a sleeping task is interrupted immediately.
#[allow(dead_code)]
pub fn stop_idle_monitor(idle_state: &Arc<IdleState>) {
    idle_state.shutdown_flag.store(true, Ordering::SeqCst);
    let mut guard = idle_state
        .monitor_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.take() {
        handle.abort();
    }
}

#[tauri::command]
pub fn get_idle_state(idle_state: tauri::State<'_, Arc<IdleState>>) -> serde_json::Value {
    serde_json::json!({
        "is_idle": idle_state.is_idle.load(Ordering::SeqCst),
        "idle_secs": idle_state.idle_secs.load(Ordering::SeqCst),
        "fullscreen_exclusive": idle_state.fullscreen_exclusive.load(Ordering::SeqCst),
        "ac_connected": idle_state.ac_connected.load(Ordering::SeqCst),
        "threshold_secs": IDLE_THRESHOLD_SECS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The composite rule, extracted so the three signals can be asserted
    /// without a desktop session. The monitor loop calls the same function.
    fn gate(idle_secs: u64, fullscreen: bool, ac_connected: bool) -> bool {
        composite_idle(idle_secs, fullscreen, ac_connected)
    }

    #[test]
    fn battery_closes_the_gate_regardless_of_the_power_saving_setting() {
        // The regression this pins down: `ac_connected` used to be derived from
        // `PowerState.active`, which stays false when the user has turned power
        // saving off. A laptop on battery then read as "AC connected" and ran a
        // 118 MB model load in the background. The power-saving setting is not
        // an input to this function at all anymore, which is the point.
        assert!(!gate(IDLE_THRESHOLD_SECS, false, false));
        assert!(!gate(IDLE_THRESHOLD_SECS * 10, false, false));
    }

    #[test]
    fn all_three_signals_are_required() {
        assert!(gate(IDLE_THRESHOLD_SECS, false, true));
        assert!(!gate(IDLE_THRESHOLD_SECS - 1, false, true));
        assert!(!gate(IDLE_THRESHOLD_SECS, true, true));
    }

    #[test]
    fn a_fresh_state_reads_as_on_ac_and_not_idle() {
        // `ac_connected` fails safe to true so a desktop whose power status
        // cannot be queried is not treated as a laptop on battery, while
        // `is_idle` fails safe to false so nothing heavy runs before the first
        // probe has been taken.
        let state = IdleState::new();
        assert!(state.ac_connected.load(Ordering::SeqCst));
        assert!(!state.is_idle.load(Ordering::SeqCst));
    }
}
