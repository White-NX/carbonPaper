//! Low-volume operational logging for unattended background work.
//!
//! This module stores only task names, counters, states, and monotonic timings.
//! User content and identifiers never enter this state.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SUMMARY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkSource {
    Staged,
    Archive,
}

impl WorkSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Archive => "archive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationPhase {
    Idle,
    InFlight,
    AckPending,
}

impl ClassificationPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::InFlight => "in_flight",
            Self::AckPending => "ack_pending",
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveWork {
    name: String,
    source: WorkSource,
    started: Instant,
    processed: u64,
}

#[derive(Debug)]
struct ActivityState {
    index: Option<ActiveWork>,
    classification: Option<ActiveWork>,
    classification_phase: ClassificationPhase,
    maintenance: Option<ActiveWork>,
    global_maintenance: Option<ActiveWork>,
    last_progress: Instant,
    last_blocked: HashMap<String, String>,
    no_work: bool,
    classification_committed: u64,
    classification_ack_pending: u64,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            index: None,
            classification: None,
            classification_phase: ClassificationPhase::Idle,
            maintenance: None,
            global_maintenance: None,
            last_progress: Instant::now(),
            last_blocked: HashMap::new(),
            no_work: false,
            classification_committed: 0,
            classification_ack_pending: 0,
        }
    }
}

static STATE: OnceLock<Mutex<ActivityState>> = OnceLock::new();

fn state() -> &'static Mutex<ActivityState> {
    STATE.get_or_init(|| Mutex::new(ActivityState::default()))
}

fn active(name: &str, source: WorkSource) -> ActiveWork {
    ActiveWork {
        name: name.to_string(),
        source,
        started: Instant::now(),
        processed: 0,
    }
}

pub fn index_start(name: &'static str, source: WorkSource) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.index = Some(active(name, source));
    state.last_progress = Instant::now();
    state.no_work = false;
}

pub fn index_progress(processed: u64) {
    if processed == 0 {
        return;
    }
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(index) = &mut state.index {
        index.processed = index.processed.saturating_add(processed);
        state.last_progress = Instant::now();
    }
}

pub fn index_processed() -> u64 {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .index
        .as_ref()
        .map_or(0, |index| index.processed)
}

pub fn index_finish() {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .index = None;
}

pub fn classification_started() {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.classification = Some(active("classification", WorkSource::Staged));
    state.classification_phase = ClassificationPhase::InFlight;
    state.last_progress = Instant::now();
    state.no_work = false;
}

fn reconcile_classification_state(state: &mut ActivityState) {
    if state.classification_phase == ClassificationPhase::InFlight {
        return;
    }
    state.classification = None;
    state.classification_phase = classification_phase_for_pending(state.classification_ack_pending);
}

pub fn classification_dispatch_rejected() {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.classification_phase = ClassificationPhase::Idle;
    reconcile_classification_state(&mut state);
}

fn classification_phase_for_pending(ack_pending: u64) -> ClassificationPhase {
    if ack_pending > 0 {
        ClassificationPhase::AckPending
    } else {
        ClassificationPhase::Idle
    }
}

pub fn classification_deferred() {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.classification_phase = ClassificationPhase::Idle;
    reconcile_classification_state(&mut state);
}

pub fn classification_committed(first_commit: bool, ack_pending: u64) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if first_commit {
        state.classification_committed = state.classification_committed.saturating_add(1);
    }
    state.classification_ack_pending = ack_pending;
    state.classification_phase = ClassificationPhase::Idle;
    reconcile_classification_state(&mut state);
    if first_commit {
        state.last_progress = Instant::now();
    }
}

/// Reconcile classification activity against the durable staging lease.
///
/// A pending receipt alone cannot tell us whether an in-flight dispatch is
/// still valid: the monitor may have accepted a job whose callback was lost.
/// Keep the in-memory activity while the staging database still owns a live
/// lease, and clear it once the durable lease is gone.
pub fn classification_pending(ack_pending: u64, live_classification_lease: bool) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.classification_ack_pending = ack_pending;
    if !live_classification_lease {
        state.classification_phase = ClassificationPhase::Idle;
    }
    reconcile_classification_state(&mut state);
}

pub fn classification_acknowledged(count: u64) {
    if count == 0 {
        return;
    }
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.classification_ack_pending = state.classification_ack_pending.saturating_sub(count);
    reconcile_classification_state(&mut state);
    state.last_progress = Instant::now();
}

pub fn maintenance_start(name: &'static str) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.maintenance = Some(active(name, WorkSource::Staged));
    state.no_work = false;
}

pub fn maintenance_progress(processed: u64) {
    if processed == 0 {
        return;
    }
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(maintenance) = &mut state.maintenance {
        maintenance.processed = maintenance.processed.saturating_add(processed);
        state.last_progress = Instant::now();
    }
}

pub fn maintenance_finish() -> (u64, u128) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let finished = state.maintenance.take();
    finished.map_or((0, 0), |work| {
        (work.processed, work.started.elapsed().as_millis())
    })
}

pub fn global_maintenance_start(name: &str) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    state.global_maintenance = Some(active(name, WorkSource::Archive));
    state.no_work = false;
}

pub fn global_maintenance_finish() -> (u64, u128) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    let finished = state.global_maintenance.take();
    finished.map_or((0, 0), |work| {
        (work.processed, work.started.elapsed().as_millis())
    })
}

fn update_blocked(blocked: &mut HashMap<String, String>, task: &str, reason: &str) -> bool {
    if blocked.get(task).is_some_and(|current| current == reason) {
        return false;
    }
    blocked.insert(task.to_string(), reason.to_string());
    true
}

pub fn blocked(task: &str, reason: &str) {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    if !update_blocked(&mut state.last_blocked, task, reason) {
        return;
    }
    state.no_work = false;
    tracing::info!("[BACKGROUND] event=blocked task={} reason={}", task, reason);
}

pub fn clear_blocked(task: &str) {
    state()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .last_blocked
        .remove(task);
}

fn clear_classification_blocked(state: &mut ActivityState) {
    if state.classification_phase == ClassificationPhase::Idle
        && state.classification_ack_pending == 0
    {
        state.last_blocked.remove("classification");
    }
}

pub fn clear_classification_blocked_if_idle() {
    let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
    clear_classification_blocked(&mut state);
}

fn is_idle(state: &ActivityState) -> bool {
    state.index.is_none()
        && state.classification.is_none()
        && state.classification_ack_pending == 0
        && state.maintenance.is_none()
        && state.global_maintenance.is_none()
        && state.last_blocked.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedTransition {
    task: String,
    reason: String,
}

fn apply_scheduler_state(
    state: &mut ActivityState,
    blocked: impl IntoIterator<Item = (String, String)>,
) -> (Vec<BlockedTransition>, bool) {
    state
        .last_blocked
        .retain(|task, _| task == "classification");
    let mut transitions = Vec::new();
    let mut has_blocked = false;
    for (task, reason) in blocked {
        has_blocked = true;
        if update_blocked(&mut state.last_blocked, &task, &reason) {
            transitions.push(BlockedTransition { task, reason });
        }
    }
    if has_blocked {
        state.no_work = false;
    }
    let became_idle = !state.no_work && is_idle(state);
    if became_idle {
        state.no_work = true;
    }
    (transitions, became_idle)
}

pub fn scheduler_no_work(blocked: impl IntoIterator<Item = (String, String)>) {
    let (transitions, became_idle) = {
        let mut state = state().lock().unwrap_or_else(|error| error.into_inner());
        apply_scheduler_state(&mut state, blocked)
    };
    for transition in transitions {
        tracing::info!(
            "[BACKGROUND] event=blocked task={} reason={}",
            transition.task,
            transition.reason
        );
    }
    if became_idle {
        tracing::info!("[BACKGROUND] event=idle reason=no_work");
    }
}

fn describe(active: Option<&ActiveWork>) -> String {
    active.map_or_else(
        || "idle".to_string(),
        |work| {
            format!(
                "{}:{}:processed={}:elapsed_s={}",
                work.name,
                work.source.as_str(),
                work.processed,
                work.started.elapsed().as_secs(),
            )
        },
    )
}

fn describe_classification(state: &ActivityState) -> String {
    state.classification.as_ref().map_or_else(
        || state.classification_phase.as_str().to_string(),
        |work| {
            format!(
                "{}:elapsed_s={}",
                state.classification_phase.as_str(),
                work.started.elapsed().as_secs(),
            )
        },
    )
}

fn describe_maintenance(state: &ActivityState) -> String {
    state.global_maintenance.as_ref().map_or_else(
        || describe(state.maintenance.as_ref()),
        |work| {
            format!(
                "{}:elapsed_s={}",
                work.name,
                work.started.elapsed().as_secs()
            )
        },
    )
}

pub fn emit_summary() {
    let state = state().lock().unwrap_or_else(|error| error.into_inner());
    tracing::info!(
        "[BACKGROUND] event=summary index={} classification={} maintenance={} since_progress_s={}",
        describe(state.index.as_ref()),
        describe_classification(&state),
        describe_maintenance(&state),
        state.last_progress.elapsed().as_secs(),
    );
    if state.classification_committed > 0 || state.classification_ack_pending > 0 {
        tracing::info!(
            "[BACKGROUND] event=classification_summary committed={} ack_pending={}",
            state.classification_committed,
            state.classification_ack_pending,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_source_names_are_stable() {
        assert_eq!(WorkSource::Staged.as_str(), "staged");
        assert_eq!(WorkSource::Archive.as_str(), "archive");
    }

    #[test]
    fn classification_phases_distinguish_commit_acknowledgement() {
        assert_eq!(ClassificationPhase::InFlight.as_str(), "in_flight");
        assert_eq!(ClassificationPhase::AckPending.as_str(), "ack_pending");
        assert_eq!(
            classification_phase_for_pending(0),
            ClassificationPhase::Idle
        );
        assert_eq!(
            classification_phase_for_pending(1),
            ClassificationPhase::AckPending
        );
    }

    #[test]
    fn classification_state_preserves_in_flight_over_pending_receipts() {
        let mut state = ActivityState::default();
        state.classification = Some(active("classification", WorkSource::Staged));
        state.classification_phase = ClassificationPhase::InFlight;
        state.classification_ack_pending = 2;
        reconcile_classification_state(&mut state);
        assert_eq!(state.classification_phase, ClassificationPhase::InFlight);
        assert!(state.classification.is_some());

        state.classification_phase = ClassificationPhase::Idle;
        reconcile_classification_state(&mut state);
        assert_eq!(state.classification_phase, ClassificationPhase::AckPending);
        assert!(state.classification.is_none());
    }

    #[test]
    fn blocked_reason_changes_are_tracked_per_task() {
        let mut blocked = HashMap::new();
        assert_eq!(
            update_blocked(&mut blocked, "semantic_index", "waiting_for_idle"),
            true
        );
        assert_eq!(
            update_blocked(&mut blocked, "semantic_index", "waiting_for_idle"),
            false
        );
        assert_eq!(
            update_blocked(&mut blocked, "clip_index", "waiting_for_idle"),
            true
        );
        assert_eq!(
            update_blocked(&mut blocked, "semantic_index", "retry_wait"),
            true
        );
    }

    #[test]
    fn no_work_requires_every_background_subsystem_to_be_idle() {
        let mut state = ActivityState::default();
        assert!(is_idle(&state));
        state.classification_ack_pending = 1;
        assert!(!is_idle(&state));
        state.classification_ack_pending = 0;
        state.maintenance = Some(active("staging_reconcile", WorkSource::Staged));
        assert!(!is_idle(&state));
        state.maintenance = None;
        state.global_maintenance = Some(active("migration", WorkSource::Archive));
        assert!(!is_idle(&state));
        state.global_maintenance = None;
        state
            .last_blocked
            .insert("classification".to_string(), "waiting_for_idle".to_string());
        assert!(!is_idle(&state));
        state.last_blocked.clear();
        assert!(is_idle(&state));
    }

    #[test]
    fn classification_blocked_reason_clears_only_when_fully_idle() {
        let mut state = ActivityState::default();
        state
            .last_blocked
            .insert("classification".to_string(), "waiting_for_idle".to_string());
        state.classification_phase = ClassificationPhase::AckPending;
        state.classification_ack_pending = 1;
        assert_ne!(state.classification_phase, ClassificationPhase::Idle);
        assert!(state.last_blocked.contains_key("classification"));

        state.classification_phase = ClassificationPhase::Idle;
        state.classification_ack_pending = 0;
        clear_classification_blocked(&mut state);
        assert!(!state.last_blocked.contains_key("classification"));
    }

    #[test]
    fn scheduler_blocked_state_rearms_no_work_transition() {
        let mut state = ActivityState::default();
        state.no_work = true;

        let (transitions, became_idle) = apply_scheduler_state(
            &mut state,
            [("semantic_index".to_string(), "retry_wait".to_string())],
        );
        assert_eq!(
            transitions,
            vec![BlockedTransition {
                task: "semantic_index".to_string(),
                reason: "retry_wait".to_string(),
            }]
        );
        assert!(!became_idle);
        assert!(!state.no_work);
        assert_eq!(
            state.last_blocked.get("semantic_index").map(String::as_str),
            Some("retry_wait")
        );

        let (transitions, became_idle) = apply_scheduler_state(&mut state, std::iter::empty());
        assert!(transitions.is_empty());
        assert!(became_idle);
        assert!(state.no_work);
        assert!(state.last_blocked.is_empty());
    }
}
