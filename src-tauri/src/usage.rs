//! Observed foreground usage, independent of screenshot deduplication and OCR.
//! Input recency and foreground media playback are alternative activity signals.
mod native;

use crate::capture::CaptureState;
use crate::monitor::MonitorState;
use crate::storage::StorageState;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

pub(crate) const INPUT_GRACE_MS: u64 = 5 * 60 * 1000;
const MAX_OBSERVATION_GAP_MS: u64 = 5_000;
const CHECKPOINT_MS: u64 = 10_000;

pub(crate) fn foreground_title_matches(window_title: &str, page_title: &str) -> bool {
    let page = page_title.trim().to_lowercase();
    let window = window_title.trim().to_lowercase();
    !page.is_empty()
        && (window == page
            || window.starts_with(&format!("{page} - "))
            || window.starts_with(&format!("{page} — ")))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct UsageIdentity {
    pub process_name: String,
    pub process_path: String,
    pub window_title: String,
    #[serde(skip)]
    pub hwnd: isize,
    #[serde(skip)]
    pub pid: u32,
}

impl UsageIdentity {
    fn same_window(&self, other: &Self) -> bool {
        self.hwnd == other.hwnd
            && self.pid == other.pid
            && self.process_path.eq_ignore_ascii_case(&other.process_path)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UsageSpan {
    pub run_id: String,
    pub sequence: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub identity: UsageIdentity,
    pub input_active: bool,
    pub media_active: bool,
    pub end_reason: String,
}

#[derive(Clone, Debug)]
struct Observation {
    utc_ms: i64,
    tick_ms: u64,
    boundary: u64,
    idle_ms: Option<u64>,
    foreground: Option<UsageIdentity>,
    media_playing: bool,
    unavailable_reason: Option<&'static str>,
}

struct UsageMachine {
    run_id: String,
    sequence: i64,
    current: Option<UsageSpan>,
    previous: Option<(i64, u64, u64)>,
    checkpoint_tick: u64,
}

impl Default for UsageMachine {
    fn default() -> Self {
        Self {
            run_id: format!("{:032x}", rand::random::<u128>()),
            sequence: 0,
            current: None,
            previous: None,
            checkpoint_tick: 0,
        }
    }
}

impl UsageMachine {
    fn finish(&mut self, reason: &str) -> Option<UsageSpan> {
        let span = self.current.take().map(|mut span| {
            span.end_reason = reason.into();
            span
        });
        self.previous = None;
        self.run_id = format!("{:032x}", rand::random::<u128>());
        self.sequence = 0;
        span
    }

    fn observe(&mut self, observation: Observation) -> Vec<UsageSpan> {
        let mut writes = Vec::new();
        let discontinuity = self.previous.and_then(|(utc, tick, boundary)| {
            if observation.boundary != boundary {
                Some("session_changed")
            } else if observation.tick_ms < tick
                || observation.tick_ms - tick > MAX_OBSERVATION_GAP_MS
            {
                Some("observation_gap")
            } else if (observation.utc_ms - utc - (observation.tick_ms - tick) as i64).abs() > 2_000
            {
                Some("clock_changed")
            } else {
                None
            }
        });
        if let Some(reason) = discontinuity {
            if let Some(span) = self.finish(reason) {
                writes.push(span);
            }
        }
        self.previous = Some((
            observation.utc_ms,
            observation.tick_ms,
            observation.boundary,
        ));

        let input_active = observation
            .idle_ms
            .is_some_and(|idle| idle < INPUT_GRACE_MS);
        let active = observation.unavailable_reason.is_none()
            && observation.foreground.is_some()
            && (input_active || observation.media_playing);
        let same = active
            && self.current.as_ref().is_some_and(|span| {
                span.identity
                    .same_window(observation.foreground.as_ref().unwrap())
                    && span.input_active == input_active
                    && span.media_active == observation.media_playing
            });

        if !same {
            if let Some(mut span) = self.current.take() {
                let reason = observation.unavailable_reason.unwrap_or_else(|| {
                    if observation.foreground.is_none() {
                        "excluded"
                    } else if !active {
                        if observation.idle_ms.is_none() {
                            "input_unavailable"
                        } else {
                            "idle"
                        }
                    } else if !span
                        .identity
                        .same_window(observation.foreground.as_ref().unwrap())
                    {
                        "foreground_changed"
                    } else {
                        "activity_source_changed"
                    }
                });
                // Only the input grace can be backdated. Ending a two-hour film
                // must never rewind the interval to its two-hour-old last input.
                if reason == "idle" && span.input_active && !span.media_active {
                    let excess = observation
                        .idle_ms
                        .unwrap_or(0)
                        .saturating_sub(INPUT_GRACE_MS);
                    span.end_ms = (observation.utc_ms - excess as i64)
                        .clamp(span.end_ms, observation.utc_ms.max(span.end_ms));
                } else if discontinuity.is_none()
                    && observation.unavailable_reason.is_none()
                    && observation.idle_ms.is_some()
                {
                    span.end_ms = observation.utc_ms.max(span.end_ms);
                }
                span.end_reason = reason.into();
                writes.push(span);
            }
            if active {
                self.sequence += 1;
                self.current = Some(UsageSpan {
                    run_id: self.run_id.clone(),
                    sequence: self.sequence,
                    start_ms: observation.utc_ms,
                    end_ms: observation.utc_ms,
                    identity: observation.foreground.unwrap(),
                    input_active,
                    media_active: observation.media_playing,
                    end_reason: "observing".into(),
                });
                self.checkpoint_tick = observation.tick_ms;
                writes.push(self.current.as_ref().unwrap().clone());
            }
        } else {
            let span = self.current.as_mut().unwrap();
            span.end_ms = observation.utc_ms.max(span.end_ms);
            if observation.tick_ms.saturating_sub(self.checkpoint_tick) >= CHECKPOINT_MS {
                writes.push(span.clone());
                self.checkpoint_tick = observation.tick_ms;
            }
        }
        writes
    }
}

#[derive(Default)]
struct Recorder {
    generation: Option<u64>,
    revision: u64,
    tracking_started: bool,
    machine: UsageMachine,
}

#[derive(Default)]
pub(crate) struct UsageState {
    recorder: Mutex<Recorder>,
    stopped: AtomicBool,
    epoch: AtomicU64,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn recording_enabled(app: &AppHandle, capture: &CaptureState) -> bool {
    capture.accepts_frames(
        app.state::<MonitorState>().is_running(),
        crate::power::is_saving_power(app),
    ) && !crate::maintenance::is_active()
}

pub(crate) fn interrupt(app: &AppHandle, reason: &str) {
    let Some(state) = app.try_state::<Arc<UsageState>>() else {
        return;
    };
    state.epoch.fetch_add(1, Ordering::AcqRel);
    let storage = app.state::<Arc<StorageState>>();
    let mut recorder = state.recorder.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(span) = recorder.machine.finish(reason) {
        if let Some(generation) = recorder.generation {
            if let Err(error) = storage.save_usage_span(&span, generation, recorder.revision) {
                tracing::warn!("Usage boundary was not saved: {error}");
            }
        }
    }
}

pub(crate) fn start_monitor(app: AppHandle) {
    let state = app.state::<Arc<UsageState>>().inner().clone();
    let mut worker = state.worker.lock().unwrap_or_else(|e| e.into_inner());
    if worker.is_some() {
        return;
    }
    let worker_state = state.clone();
    *worker = Some(std::thread::spawn(move || {
        let mut probe = native::NativeProbe::new();
        loop {
            probe.pump_events();
            if worker_state.stopped.load(Ordering::Acquire) {
                break;
            }
            let storage = app.state::<Arc<StorageState>>();
            let capture = app.state::<Arc<CaptureState>>();
            if !storage.is_initialized() || !recording_enabled(&app, &capture) {
                interrupt(&app, "recording_interrupted");
                probe.wait();
                continue;
            }
            let epoch = worker_state.epoch.load(Ordering::Acquire);
            let mut observation = probe.observe(&capture);
            probe.pump_events();
            probe.apply_boundaries(&mut observation);
            let mut recorder = worker_state
                .recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // A probe collected before pause/stop/restore cannot reopen a span.
            if epoch != worker_state.epoch.load(Ordering::Acquire)
                || worker_state.stopped.load(Ordering::Acquire)
                || !recording_enabled(&app, &capture)
                || !storage.is_initialized()
            {
                drop(recorder);
                probe.wait();
                continue;
            }
            let generation = storage.db_generation();
            let revision = storage.usage_revision();
            if recorder.generation != Some(generation) || recorder.revision != revision {
                *recorder = Recorder {
                    generation: Some(generation),
                    revision,
                    ..Recorder::default()
                };
            }
            let result = (|| {
                if !recorder.tracking_started {
                    storage.begin_usage_tracking(observation.utc_ms, generation)?;
                    recorder.tracking_started = true;
                }
                for span in recorder.machine.observe(observation) {
                    storage.save_usage_span(&span, generation, revision)?;
                }
                Ok::<_, String>(())
            })();
            if let Err(error) = result {
                // Durable checkpoints remain bounded; missing storage is a gap.
                recorder.machine = UsageMachine::default();
                recorder.tracking_started = false;
                tracing::warn!("Usage observation was not saved: {error}");
            }
            drop(recorder);
            probe.wait();
        }
    }));
}

pub(crate) fn stop_monitor(app: &AppHandle) {
    if let Some(state) = app.try_state::<Arc<UsageState>>() {
        state.stopped.store(true, Ordering::Release);
        interrupt(app, "shutdown");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(time: u64, idle: Option<u64>, media: bool) -> Observation {
        Observation {
            utc_ms: 1_767_225_600_000 + time as i64,
            tick_ms: time,
            boundary: 0,
            idle_ms: idle,
            foreground: Some(UsageIdentity {
                process_name: "player.exe".into(),
                process_path: "C:/player.exe".into(),
                window_title: "Film".into(),
                hwnd: 1,
                pid: 1,
            }),
            media_playing: media,
            unavailable_reason: None,
        }
    }

    #[test]
    fn input_grace_closes_without_a_new_screenshot_and_return_starts_a_new_span() {
        let mut machine = UsageMachine::default();
        machine.observe(observation(0, Some(0), false));
        for time in (1_000..INPUT_GRACE_MS).step_by(1_000) {
            machine.observe(observation(time, Some(time), false));
        }
        let closed = machine.observe(observation(
            INPUT_GRACE_MS + 500,
            Some(INPUT_GRACE_MS + 500),
            false,
        ));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].end_ms - closed[0].start_ms, INPUT_GRACE_MS as i64);
        assert_eq!(closed[0].end_reason, "idle");
        let returned = machine.observe(observation(25 * 3_600_000, Some(0), false));
        assert_eq!(returned.len(), 1);
        assert_ne!(returned[0].run_id, closed[0].run_id);
        assert_eq!(returned[0].start_ms, 1_767_225_600_000 + 25 * 3_600_000);
    }

    #[test]
    fn two_hour_film_counts_without_input_and_stops_at_playback_end() {
        let mut machine = UsageMachine::default();
        machine.observe(observation(0, Some(INPUT_GRACE_MS), true));
        for time in (1_000..=2 * 3_600_000).step_by(1_000) {
            machine.observe(observation(time, Some(INPUT_GRACE_MS + time), true));
        }
        let closed = machine.observe(observation(
            2 * 3_600_000 + 1_000,
            Some(3 * 3_600_000),
            false,
        ));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].end_ms - closed[0].start_ms, 2 * 3_600_000 + 1_000);
        assert!(closed[0].media_active);
        assert!(!closed[0].input_active);
    }

    #[test]
    fn missing_input_is_not_evidence_of_activity_and_excluded_windows_close_usage() {
        let mut machine = UsageMachine::default();
        assert!(machine.observe(observation(0, None, false)).is_empty());
        machine.observe(observation(1_000, Some(0), false));
        let mut excluded = observation(2_000, Some(0), true);
        excluded.foreground = None;
        let closed = machine.observe(excluded);
        assert_eq!(closed[0].end_reason, "excluded");
        assert!(machine.current.is_none());
    }

    #[test]
    fn sleep_clock_changes_and_explicit_pause_preserve_boundaries() {
        for kind in ["sleep", "clock", "notification"] {
            let mut machine = UsageMachine::default();
            let first = machine.observe(observation(10_000, Some(0), true))[0].clone();
            let mut next = observation(11_000, Some(0), true);
            match kind {
                "sleep" => next.tick_ms += 24 * 3_600_000,
                "clock" => next.utc_ms += 24 * 3_600_000,
                _ => next.boundary = 1,
            }
            let spans = machine.observe(next);
            assert_eq!(spans.len(), 2);
            assert_eq!(spans[0].end_ms, first.end_ms);
            assert_ne!(spans[0].run_id, spans[1].run_id);
        }
        let mut machine = UsageMachine::default();
        let first = machine.observe(observation(0, Some(0), true))[0].clone();
        assert_eq!(machine.finish("paused").unwrap().end_reason, "paused");
        let next = machine.observe(observation(1_000, Some(0), true));
        assert_ne!(first.run_id, next[0].run_id);
    }
}
