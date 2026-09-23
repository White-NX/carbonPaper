//! Unified scheduling for automatic semantic/cluster work.
//!
//! The task implementations still own their transactional queues. This module
//! owns the one decision that used to be duplicated four times: which slice is
//! allowed to claim the single semantic worker next.

use crate::background_activity::{self, WorkSource};
use crate::credential_manager::CredentialManagerState;
use crate::idle::IdleState;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{BackgroundTaskState, DerivedIndexKind, StorageState};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

mod adaptive;
use crate::background_policy::{
    self, ExecutionProfile, PauseStatistics, Qualification, SchedulingMode,
};
use adaptive::AdaptiveRuntime;

pub const TASK_SEMANTIC_INDEX: &str = "semantic_index";
pub const TASK_CLIP_INDEX: &str = "clip_index";
pub const TASK_SMART_CLUSTER: &str = "smart_cluster";
pub const TASK_ANN_BUILD: &str = "ann_build";

/// Automatic work may wait behind a user-requested pass for this long before
/// becoming eligible to reclaim the head of the queue.
pub const AUTO_AGING_LIMIT: Duration = Duration::from_secs(10 * 60);
pub(crate) const CLIP_AUTO_QUANTUM: Duration = Duration::from_secs(60);
pub(crate) const MINILM_AUTO_QUANTUM: Duration = Duration::from_secs(30);
const TICK_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    SemanticIndex,
    ClipIndex,
    SmartCluster,
    AnnBuild,
}

impl BackgroundTaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SemanticIndex => TASK_SEMANTIC_INDEX,
            Self::ClipIndex => TASK_CLIP_INDEX,
            Self::SmartCluster => TASK_SMART_CLUSTER,
            Self::AnnBuild => TASK_ANN_BUILD,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            TASK_SEMANTIC_INDEX => Some(Self::SemanticIndex),
            TASK_CLIP_INDEX => Some(Self::ClipIndex),
            TASK_SMART_CLUSTER => Some(Self::SmartCluster),
            TASK_ANN_BUILD => Some(Self::AnnBuild),
            _ => None,
        }
    }

    const fn automatic_quantum(self) -> Option<Duration> {
        match self {
            Self::ClipIndex => Some(CLIP_AUTO_QUANTUM),
            Self::SemanticIndex => Some(MINILM_AUTO_QUANTUM),
            Self::SmartCluster | Self::AnnBuild => None,
        }
    }
}

fn task_feature_enabled_with_config(
    kind: BackgroundTaskKind,
    manual: bool,
    smart_cluster_enabled: bool,
) -> bool {
    match kind {
        _ if manual => true,
        BackgroundTaskKind::SemanticIndex => smart_cluster_enabled,
        BackgroundTaskKind::SmartCluster => smart_cluster_enabled,
        BackgroundTaskKind::ClipIndex | BackgroundTaskKind::AnnBuild => true,
    }
}

pub(crate) fn task_feature_enabled(kind: BackgroundTaskKind, manual: bool) -> bool {
    task_feature_enabled_with_config(
        kind,
        manual,
        crate::registry_config::get_bool("smart_cluster_enabled").unwrap_or(false),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomaticSliceStopReason {
    BudgetExpired,
    ManualRequestPending,
    ExternalBackgroundRequest,
}

impl AutomaticSliceStopReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExpired => "quantum_expired",
            Self::ManualRequestPending => "manual_request_pending",
            Self::ExternalBackgroundRequest => "external_background_request",
        }
    }
}

/// The lease-history note recorded beside a deferred reason code when an
/// automatic pass hands its claimed jobs back without attempting them. The
/// code is durable machine state; this is its human-readable companion, so it
/// must stay honest about which side asked the pass to stand down.
pub(crate) fn deferred_release_note(reason: &str) -> &'static str {
    match reason {
        code if code == AutomaticSliceStopReason::BudgetExpired.as_str() => {
            "the automatic model quantum reached its time budget"
        }
        code if code == AutomaticSliceStopReason::ManualRequestPending.as_str() => {
            "a manual request is waiting for the scheduler"
        }
        code if code == AutomaticSliceStopReason::ExternalBackgroundRequest.as_str() => {
            "an external background request is waiting for the semantic worker"
        }
        _ => "another background model request owned the semantic worker",
    }
}

/// Process-local admission lease for one automatic single-model quantum.
///
/// Durable scheduler state still moves from queued to running exactly once for
/// the whole quantum. The task implementation checks this lease between model
/// requests; the adaptive execution lease also cancels in-flight ONNX work.
#[derive(Clone)]
pub(crate) struct AutomaticSliceContext {
    started: Instant,
    deadline: Instant,
    manual_generation_at_admission: u64,
    manual_generation: Arc<AtomicU64>,
}

impl AutomaticSliceContext {
    pub(crate) fn new(
        budget: Duration,
        manual_generation: Arc<AtomicU64>,
        manual_generation_at_admission: u64,
    ) -> Self {
        let started = Instant::now();
        Self {
            started,
            deadline: started + budget,
            manual_generation_at_admission,
            manual_generation,
        }
    }

    /// Return the reason no further model request should be submitted.
    ///
    /// The time budget is ignored until the task has made some business-queue
    /// progress. Maintenance work may legitimately consume the initial budget,
    /// but an admitted quantum must still get one chance to advance its queue.
    pub(crate) fn stop_reason(
        &self,
        semantic: &SemanticRuntimeState,
        made_progress: bool,
    ) -> Option<AutomaticSliceStopReason> {
        if self.manual_generation.load(Ordering::SeqCst) != self.manual_generation_at_admission {
            return Some(AutomaticSliceStopReason::ManualRequestPending);
        }
        if semantic.external_background_waiting() {
            return Some(AutomaticSliceStopReason::ExternalBackgroundRequest);
        }
        if made_progress && Instant::now() >= self.deadline {
            return Some(AutomaticSliceStopReason::BudgetExpired);
        }
        None
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerCandidate {
    pub kind: BackgroundTaskKind,
    pub ready_since_ms: i64,
    pub last_served_seq: u64,
    pub manual_pending: bool,
}

/// Pure ordering policy, kept separate from Tauri state so FIFO/aging behavior
/// can be tested without a desktop runtime or a database.
pub fn select_next_task(
    tasks: &[BackgroundTaskState],
    now_ms: i64,
    aging_limit_ms: i64,
) -> Option<BackgroundTaskKind> {
    let eligible: Vec<SchedulerCandidate> = tasks
        .iter()
        .filter(|task| task.is_eligible(now_ms))
        .filter_map(|task| {
            Some(SchedulerCandidate {
                kind: BackgroundTaskKind::parse(&task.task_kind)?,
                ready_since_ms: task.ready_since_ms,
                last_served_seq: task.last_served_seq,
                manual_pending: task.manual_pending,
            })
        })
        .collect();
    if eligible.is_empty() {
        return None;
    }

    let aged_automatic_exists = eligible.iter().any(|task| {
        !task.manual_pending && now_ms.saturating_sub(task.ready_since_ms) >= aging_limit_ms
    });

    let mut ranked: Vec<SchedulerCandidate> = if aged_automatic_exists {
        // An aged automatic task must actually cross the manual-priority
        // boundary. Sorting all candidates by FIFO would still let an old,
        // continuously re-requested manual row starve a newer automatic row.
        eligible
            .into_iter()
            .filter(|task| {
                !task.manual_pending && now_ms.saturating_sub(task.ready_since_ms) >= aging_limit_ms
            })
            .collect()
    } else {
        eligible
    };
    ranked.sort_by(|a, b| {
        // Manual requests win until an automatic task reaches the starvation
        // bound. Once that happens all tasks use the same FIFO/round-robin
        // ordering, which lets the aged task break through a busy UI.
        if !aged_automatic_exists {
            match b.manual_pending.cmp(&a.manual_pending) {
                CmpOrdering::Equal => {}
                ordering => return ordering,
            }
        }
        a.ready_since_ms
            .cmp(&b.ready_since_ms)
            .then_with(|| a.last_served_seq.cmp(&b.last_served_seq))
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
    });
    ranked.first().map(|task| task.kind)
}

/// Return the task kinds that are eligible and admitted by their current
/// runtime gates. Keeping this filtering separate from the ordering policy lets
/// a blocked automatic task fall through to a runnable manual request instead
/// of holding the head of the queue hostage.
pub fn select_next_runnable_task(
    tasks: &[BackgroundTaskState],
    now_ms: i64,
    aging_limit_ms: i64,
    mut is_runnable: impl FnMut(&BackgroundTaskState) -> bool,
) -> Option<(BackgroundTaskKind, bool)> {
    let runnable: Vec<BackgroundTaskState> = tasks
        .iter()
        .filter(|task| task.is_eligible(now_ms))
        .filter(|task| is_runnable(task))
        .cloned()
        .collect();
    // "Process now" is an explicit foreground admission. Once its gates pass,
    // do not make the user wait behind an aged automatic index row; the latter
    // still retains its starvation protection when no such request exists.
    let kind = runnable
        .iter()
        .find(|task| task.task_kind == TASK_SMART_CLUSTER && task.manual_pending)
        .map(|_| BackgroundTaskKind::SmartCluster)
        .or_else(|| select_next_task(&runnable, now_ms, aging_limit_ms))?;
    let manual = runnable
        .iter()
        .find(|task| task.task_kind == kind.as_str())
        .map(|task| task.manual_pending)
        .unwrap_or(false);
    Some((kind, manual))
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduledSliceResult {
    pub completed: bool,
    pub has_more: bool,
    pub processed: u64,
    pub skipped_reason: Option<String>,
}

impl ScheduledSliceResult {
    pub(crate) fn complete(has_more: bool) -> Self {
        Self {
            completed: true,
            has_more,
            processed: 0,
            skipped_reason: None,
        }
    }

    pub(crate) fn with_processed(mut self, processed: u64) -> Self {
        self.processed = processed;
        self
    }

    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        Self {
            completed: false,
            has_more: true,
            processed: 0,
            skipped_reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSchedulerStatus {
    pub enabled: bool,
    pub running_task: Option<String>,
    pub running_manual: bool,
    pub tasks: Vec<BackgroundTaskState>,
    pub queue_depths: serde_json::Value,
    pub blocked_reason: Option<String>,
    pub next_retry_at_ms: Option<i64>,
    /// Earliest retry timestamp among manually requested tasks.
    pub manual_retry_at_ms: Option<i64>,
    pub execution_profile: ExecutionProfile,
    pub scheduling_mode: SchedulingMode,
    pub qualifications: Vec<Qualification>,
    pub task_background_eligible: std::collections::BTreeMap<String, bool>,
    pub backlog_age_ms: std::collections::BTreeMap<String, i64>,
    pub pauses: PauseStatistics,
}

struct SchedulerRuntime {
    adaptive: AdaptiveRuntime,
    stop: AtomicBool,
    wake: Notify,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    running_task: Mutex<Option<String>>,
    running_manual: AtomicBool,
    blocked_reason: Mutex<Option<String>>,
    service_seq: AtomicU64,
    manual_request_generation: Arc<AtomicU64>,
}

impl Default for SchedulerRuntime {
    fn default() -> Self {
        Self {
            adaptive: AdaptiveRuntime::default(),
            stop: AtomicBool::new(false),
            wake: Notify::new(),
            task: Mutex::new(None),
            running_task: Mutex::new(None),
            running_manual: AtomicBool::new(false),
            blocked_reason: Mutex::new(None),
            service_seq: AtomicU64::new(0),
            manual_request_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Tauri-managed scheduler state. It is intentionally cheap to clone through
/// `Arc`; all mutable runtime fields are behind small locks/atomics.
pub struct BackgroundSchedulerState {
    runtime: Arc<SchedulerRuntime>,
}

impl Default for BackgroundSchedulerState {
    fn default() -> Self {
        Self {
            runtime: Arc::new(SchedulerRuntime::default()),
        }
    }
}

impl BackgroundSchedulerState {
    pub fn start(&self, app: AppHandle) {
        let mut task = self.runtime.task.lock().unwrap_or_else(|e| e.into_inner());
        if task
            .as_ref()
            .is_some_and(|handle| !handle.inner().is_finished())
        {
            // Credential initialization is intentionally idempotent and the
            // unlock UI may call it again before every Hello prompt. Restarting
            // here would abort an admitted slice half-way through and leave its
            // durable row in `running` until the next application launch.
            self.runtime.stop.store(false, Ordering::SeqCst);
            self.wake();
            return;
        }
        // A previous loop may have exited on its own (or been aborted by
        // shutdown) while leaving a finished handle in the slot.
        task.take();
        self.runtime.stop.store(false, Ordering::SeqCst);
        if let Some(storage) = app.try_state::<Arc<StorageState>>() {
            if let Err(error) = storage.recover_background_scheduler_tasks() {
                tracing::warn!("[SCHEDULER] startup recovery failed: {error}");
            }
            if let Ok(tasks) = storage.background_scheduler_tasks() {
                let max_seq = tasks
                    .iter()
                    .map(|task| task.last_served_seq)
                    .max()
                    .unwrap_or(0);
                self.runtime
                    .service_seq
                    .fetch_max(max_seq, Ordering::SeqCst);
            }
            // Older databases can contain a real derived-index backlog without
            // a scheduler row because those workers predate the unified ledger.
            // Seed only missing rows; an existing completed row must retain its
            // durable completion timestamp and interval semantics.
            for kind in [TASK_SEMANTIC_INDEX, TASK_CLIP_INDEX, TASK_ANN_BUILD] {
                if storage
                    .background_scheduler_task(kind)
                    .ok()
                    .flatten()
                    .is_none()
                {
                    let _ = storage.enqueue_background_task(kind, false, now_ms());
                }
            }
        }
        let runtime = self.runtime.clone();
        AdaptiveRuntime::start(runtime.clone(), app.clone());
        *task = Some(tauri::async_runtime::spawn(async move {
            scheduler_loop(app, runtime).await;
        }));
    }

    pub fn stop(&self) {
        self.runtime.stop.store(true, Ordering::SeqCst);
        self.runtime.adaptive.cancel_active("shutdown");
        self.runtime.wake.notify_waiters();
        if let Some(handle) = self
            .runtime
            .task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            handle.abort();
        }
    }

    pub fn wake(&self) {
        self.runtime.wake.notify_one();
    }

    pub fn enqueue(
        &self,
        app: &AppHandle,
        kind: BackgroundTaskKind,
        manual: bool,
    ) -> Result<(), String> {
        let Some(storage) = app.try_state::<Arc<StorageState>>() else {
            return Err("Storage is not initialized".to_string());
        };
        let result = storage.enqueue_background_task(kind.as_str(), manual, now_ms());
        if let Err(error) = &result {
            tracing::debug!("[SCHEDULER] enqueue {} failed: {error}", kind.as_str());
        }
        result?;
        if manual {
            self.runtime
                .manual_request_generation
                .fetch_add(1, Ordering::SeqCst);
        }
        // This is the externally visible admission path. Even when the row is
        // already queued, a caller may be retrying after changing an admission
        // condition, so preserve the prompt wake-up semantics. The internal
        // backlog reconciliation deliberately bypasses this method.
        if manual {
            *self
                .runtime
                .blocked_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.wake();
        Ok(())
    }

    pub fn status(&self, app: &AppHandle) -> BackgroundSchedulerStatus {
        let tasks = app
            .try_state::<Arc<StorageState>>()
            .and_then(|state| state.background_scheduler_tasks().ok())
            .unwrap_or_default();
        let next_retry_at_ms = tasks
            .iter()
            .filter(|task| task.status == "retry_wait")
            .map(|task| task.next_attempt_at_ms)
            .min();
        let manual_retry_at_ms = tasks
            .iter()
            .filter(|task| task.manual_pending && task.status == "retry_wait")
            .map(|task| task.next_attempt_at_ms)
            .min();
        let running_task = self
            .runtime
            .running_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let blocked_reason = self
            .runtime
            .blocked_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| {
                tasks
                    .iter()
                    .find(|task| task.status == "failed")
                    .map(|_| "failed".to_string())
            });
        let depths = queue_depths(app.try_state::<Arc<StorageState>>());
        let enabled = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|state| state.background_processing_enabled())
            .unwrap_or(false);
        BackgroundSchedulerStatus {
            enabled,
            running_task,
            running_manual: self.runtime.running_manual.load(Ordering::Relaxed),
            backlog_age_ms: app
                .try_state::<Arc<StorageState>>()
                .and_then(|storage| storage.background_backlog_ages(now_ms()).ok())
                .unwrap_or_default(),
            tasks,
            queue_depths: depths,
            blocked_reason,
            next_retry_at_ms,
            manual_retry_at_ms,
            execution_profile: self
                .runtime
                .adaptive
                .profile(self.runtime.running_manual.load(Ordering::Relaxed)),
            scheduling_mode: adaptive::configured_mode(),
            qualifications: self.qualifications(),
            task_background_eligible: self.qualification_summary(),
            pauses: self
                .runtime
                .adaptive
                .pauses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn retry_delay(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(16);
    let seconds = 60u64.saturating_mul(1u64 << exponent);
    Duration::from_secs(seconds).min(MAX_RETRY_DELAY)
}

fn queue_depths(storage: Option<tauri::State<'_, Arc<StorageState>>>) -> serde_json::Value {
    let Some(storage) = storage else {
        return serde_json::json!({});
    };
    let semantic = storage
        .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
        .map(|backlog| backlog.ready)
        .unwrap_or(0);
    let clip = storage
        .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
        .map(|backlog| backlog.ready)
        .unwrap_or(0);
    let smart = storage.count_smart_cluster_pending().unwrap_or(0).max(0) as u64;
    serde_json::json!({
        TASK_SEMANTIC_INDEX: semantic,
        TASK_CLIP_INDEX: clip,
        TASK_SMART_CLUSTER: smart,
    })
}

pub(crate) fn gate_reason(app: &AppHandle, manual: bool) -> Option<&'static str> {
    let credential = app.state::<Arc<CredentialManagerState>>();
    if credential.silent_read_auth_required() {
        return Some("waiting_for_verification");
    }
    if manual {
        if !credential.is_session_valid() && !credential.background_authorized() {
            return Some("waiting_for_unlock");
        }
    } else if !credential.background_authorized() {
        return Some("waiting_for_unlock");
    }
    environment_gate_reason(
        app,
        if manual {
            EnvironmentPolicy::Immediate
        } else {
            EnvironmentPolicy::IdleOnly
        },
    )
}

pub(crate) fn gate_reason_for_kind(
    app: &AppHandle,
    manual: bool,
    kind: BackgroundTaskKind,
) -> Option<&'static str> {
    if let Some(lease) = background_policy::current_execution() {
        if lease.task == kind.as_str() {
            return lease.reason();
        }
    }
    if !manual {
        let storage = app.state::<Arc<StorageState>>();
        let staged = storage.background_processing_enabled()
            && crate::processing_stage::ready_for_kind(&storage, kind);
        if let Some(scheduler) = app.try_state::<Arc<BackgroundSchedulerState>>() {
            // Staging authorization is scoped again by the broker at claim and
            // commit. Only its ready work can use this admission exception.
            let reason = scheduler
                .runtime
                .adaptive
                .admission(app, kind, staged)
                .err();
            if !matches!(
                reason,
                Some("waiting_for_unlock" | "waiting_for_verification")
            ) || kind != BackgroundTaskKind::SmartCluster
            {
                return reason;
            }
        }
        if storage.background_processing_enabled()
            && crate::processing_stage::ready_for_kind(&storage, kind)
        {
            return environment_gate_reason(app, EnvironmentPolicy::IdleOnly);
        }
        if kind == BackgroundTaskKind::SmartCluster
            && storage.background_processing_enabled()
            && !storage.is_background_authorized()
        {
            use carbonpaper_app_bound::protocol::Consumer;
            if storage.processing_stage.has_ready(Consumer::SmartCluster) {
                return Some("waiting_for_index");
            }
            if storage
                .processing_stage
                .owned_screenshot_ids(Consumer::SmartCluster)
                .is_ok_and(|ids| !ids.is_empty())
            {
                return Some("retry_wait");
            }
        }
    }
    gate_reason(app, manual)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvironmentPolicy {
    IdleOnly,
    /// Resumable work admitted through a profile-specific scheduler lease.
    Automatic,
    /// Capture postprocessing and explicit requests may run during user activity,
    /// while still yielding to maintenance and foreground semantic queries.
    Immediate,
}

impl EnvironmentPolicy {
    fn gate_reason(
        self,
        maintenance_active: bool,
        idle: &IdleState,
        semantic: &SemanticRuntimeState,
    ) -> Option<&'static str> {
        if maintenance_active {
            return Some("maintenance");
        }
        if self != Self::Immediate {
            if !idle.ac_connected.load(Ordering::Relaxed) {
                return Some("waiting_for_ac_power");
            }
            if idle.fullscreen_exclusive.load(Ordering::Relaxed) {
                return Some("waiting_for_fullscreen");
            }
            if (self == Self::IdleOnly && !idle.is_idle.load(Ordering::Relaxed))
                || (self == Self::Automatic
                    && idle.idle_secs.load(Ordering::Relaxed) < background_policy::SHORT_IDLE_SECS)
            {
                return Some("waiting_for_idle");
            }
        }
        if semantic.foreground_waiting() {
            return Some("foreground_request");
        }
        None
    }
}

pub(crate) fn environment_gate_reason(
    app: &AppHandle,
    policy: EnvironmentPolicy,
) -> Option<&'static str> {
    if policy == EnvironmentPolicy::Automatic {
        if let Some(lease) = background_policy::current_execution() {
            return lease.reason().or_else(|| {
                EnvironmentPolicy::Immediate.gate_reason(
                    crate::maintenance::is_active(),
                    &app.state::<Arc<IdleState>>(),
                    &app.state::<Arc<SemanticRuntimeState>>(),
                )
            });
        }
    }
    policy.gate_reason(
        crate::maintenance::is_active(),
        &app.state::<Arc<IdleState>>(),
        &app.state::<Arc<SemanticRuntimeState>>(),
    )
}

async fn refresh_backlog(app: &AppHandle) {
    let Some(storage) = app.try_state::<Arc<StorageState>>() else {
        return;
    };
    let storage = storage.inner().clone();
    let count_storage = storage.clone();
    let counts = tokio::task::spawn_blocking(move || {
        let semantic = count_storage
            .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
            .map(|backlog| backlog.ready)
            .unwrap_or(0);
        let clip = count_storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
            .map(|backlog| backlog.ready)
            .unwrap_or(0);
        let smart = count_storage
            .count_smart_cluster_pending()
            .unwrap_or(0)
            .max(0) as u64;
        (semantic, clip, smart)
    })
    .await;
    let Ok((semantic, clip, smart)) = counts else {
        return;
    };
    // This reconciliation already runs inside the scheduler loop. Persist any
    // newly discovered work directly: using the public enqueue path here would
    // notify the same loop while it is running, leave a Notify permit behind,
    // and turn an admission-gated queue into a self-waking hot loop.
    // Automatic text indexing supplies Smart Cluster candidate retrieval.
    // Pending ledger rows resume when the feature is enabled again.
    let semantic_consumers =
        crate::registry_config::get_bool("smart_cluster_enabled").unwrap_or(false);
    if semantic > 0 && semantic_consumers {
        let _ = storage.enqueue_background_task_if_changed(TASK_SEMANTIC_INDEX, false, now_ms());
    }
    if clip > 0 {
        let _ = storage.enqueue_background_task_if_changed(TASK_CLIP_INDEX, false, now_ms());
    }
    // A disabled smart cluster feature must not enqueue scoring work, even when
    // pending rows exist. The registry is the application-side source of truth.
    if smart > 0 && crate::registry_config::get_bool("smart_cluster_enabled").unwrap_or(false) {
        let _ = storage.enqueue_background_task_if_changed(TASK_SMART_CLUSTER, false, now_ms());
    }
}

fn task_source(app: &AppHandle, kind: BackgroundTaskKind, manual: bool) -> WorkSource {
    if (!manual || kind == BackgroundTaskKind::SmartCluster) && prefer_staged(app, kind, manual) {
        WorkSource::Staged
    } else {
        WorkSource::Archive
    }
}

pub(crate) fn prefer_staged(app: &AppHandle, kind: BackgroundTaskKind, manual: bool) -> bool {
    let storage = app.state::<Arc<StorageState>>();
    if !crate::processing_stage::ready_for_kind(&storage, kind) {
        return false;
    }
    // Reserve every fourth automatic turn for archive debt. Old records
    // remain at the head of each archive queue even under continuous capture.
    manual
        || !storage.is_background_authorized()
        || app
            .try_state::<Arc<BackgroundSchedulerState>>()
            .is_none_or(|scheduler| scheduler.runtime.service_seq.load(Ordering::Relaxed) % 4 != 0)
}

async fn execute_slice(
    app: &AppHandle,
    kind: BackgroundTaskKind,
    manual: bool,
    automatic_context: Option<&AutomaticSliceContext>,
) -> Result<ScheduledSliceResult, String> {
    // A queued row can outlive a settings change. Check every dispatch path,
    // including automatic model quanta, before loading a model. Explicit
    // maintenance remains available when disabled.
    if !task_feature_enabled(kind, manual) {
        return Ok(ScheduledSliceResult::skipped("disabled"));
    }
    if kind == BackgroundTaskKind::AnnBuild {
        return crate::clip_ann::run_scheduled_slice(app, manual).await;
    }
    if !manual
        && matches!(
            kind,
            BackgroundTaskKind::SemanticIndex | BackgroundTaskKind::ClipIndex
        )
    {
        let context = automatic_context.expect("automatic index tasks have a quantum context");
        match kind {
            BackgroundTaskKind::SemanticIndex => {
                crate::minilm_index::run_scheduled_slice(app, false, Some(context)).await
            }
            BackgroundTaskKind::ClipIndex => {
                crate::clip_index::run_scheduled_slice(app, false, Some(context)).await
            }
            BackgroundTaskKind::SmartCluster | BackgroundTaskKind::AnnBuild => {
                unreachable!()
            }
        }
    } else {
        let Ok(_worker_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
            return Ok(ScheduledSliceResult::skipped("semantic_worker_busy"));
        };
        match kind {
            BackgroundTaskKind::SemanticIndex => {
                crate::minilm_index::run_scheduled_slice(app, manual, None).await
            }
            BackgroundTaskKind::ClipIndex => {
                crate::clip_index::run_scheduled_slice(app, manual, None).await
            }
            BackgroundTaskKind::SmartCluster => {
                crate::smart_cluster_scoring::run_scheduled_slice(app, manual).await
            }
            BackgroundTaskKind::AnnBuild => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerWake {
    Startup,
    Tick,
    Notification,
}

async fn wait_for_scheduler_wake(
    first_pass: &mut bool,
    ticker: &mut tokio::time::Interval,
    wake: &Notify,
) -> SchedulerWake {
    if std::mem::take(first_pass) {
        return SchedulerWake::Startup;
    }
    tokio::select! {
        _ = ticker.tick() => SchedulerWake::Tick,
        _ = wake.notified() => SchedulerWake::Notification,
    }
}

fn should_refresh_backlog(reason: SchedulerWake) -> bool {
    !matches!(reason, SchedulerWake::Notification)
}

async fn scheduler_loop(app: AppHandle, runtime: Arc<SchedulerRuntime>) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume Tokio's immediate first tick, then run one startup pass without
    // waiting. Later passes require either the fallback tick or a real external
    // notification; backlog reconciliation must never wake this loop itself.
    ticker.tick().await;
    let mut first_pass = true;
    loop {
        let wake_reason =
            wait_for_scheduler_wake(&mut first_pass, &mut ticker, &runtime.wake).await;
        if runtime.stop.load(Ordering::SeqCst) {
            break;
        }
        // Snapshot before reading the durable queue. A manual request arriving
        // after this point either appears in that read or changes the
        // generation observed by an automatic quantum at its first request
        // boundary. Taking the snapshot after the durable claim would leave a
        // race where a just-arrived manual request looked older than the
        // automatic work already selected from a stale queue read.
        let manual_generation_at_scan = runtime.manual_request_generation.load(Ordering::SeqCst);
        if should_refresh_backlog(wake_reason) {
            let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
            let low_memory = runtime
                .adaptive
                .resources
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .latest
                .available_memory_bytes
                .is_some_and(|bytes| !background_policy::memory_admits(bytes, 0));
            semantic.reclaim_idle_model(app.clone(), low_memory).await;
            app.state::<Arc<crate::clip_ann::ClipAnnState>>()
                .reclaim_builder(low_memory);
            refresh_backlog(&app).await;
        }
        let Some(storage_state) = app.try_state::<Arc<StorageState>>() else {
            continue;
        };
        let storage = storage_state.inner().clone();
        let tasks = match tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || storage.background_scheduler_tasks()
        })
        .await
        {
            Ok(Ok(tasks)) => tasks,
            Ok(Err(error)) => {
                tracing::debug!("[SCHEDULER] state read failed: {error}");
                continue;
            }
            Err(error) => {
                tracing::debug!("[SCHEDULER] state task failed: {error}");
                continue;
            }
        };
        let now = now_ms();
        runtime.adaptive.pending.store(
            tasks
                .iter()
                .any(|t| matches!(t.status.as_str(), "queued" | "running" | "retry_wait")),
            Ordering::Relaxed,
        );
        let selected =
            select_next_runnable_task(&tasks, now, AUTO_AGING_LIMIT.as_millis() as i64, |task| {
                BackgroundTaskKind::parse(&task.task_kind).is_some_and(|kind| {
                    gate_reason_for_kind(&app, task.manual_pending, kind).is_none()
                }) && (task.manual_pending
                    || !app
                        .state::<Arc<SemanticRuntimeState>>()
                        .external_background_waiting())
            });
        let (kind, manual) = if let Some(selected) = selected {
            selected
        } else {
            // Preserve a useful blocked reason when every eligible task is
            // gated, while avoiding a blocked automatic row preventing a
            // runnable manual row from being considered above.
            let Some(kind) = select_next_task(&tasks, now, AUTO_AGING_LIMIT.as_millis() as i64)
            else {
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                let inactive = tasks.iter().filter_map(|task| {
                    let reason = match task.status.as_str() {
                        "retry_wait" => "retry_wait",
                        "failed" => "failed",
                        "parked" => "disabled",
                        _ => return None,
                    };
                    Some((task.task_kind.clone(), reason.to_string()))
                });
                background_activity::scheduler_no_work(inactive);
                continue;
            };
            let manual = tasks
                .iter()
                .find(|task| task.task_kind == kind.as_str())
                .map(|task| task.manual_pending)
                .unwrap_or(false);
            (kind, manual)
        };
        if !manual
            && app
                .state::<Arc<SemanticRuntimeState>>()
                .external_background_waiting()
        {
            *runtime
                .blocked_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some("external_background_request".to_string());
            background_activity::blocked(kind.as_str(), "external_background_request");
            continue;
        }
        if let Some(reason) = gate_reason_for_kind(&app, manual, kind) {
            *runtime
                .blocked_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(reason.to_string());
            background_activity::blocked(kind.as_str(), reason);
            continue;
        }
        // A task that was previously blocked by an admission gate is now
        // admitted. Clear that transient reason before the slice starts so a
        // later retry/failure cannot be rendered as the stale gate state.
        *runtime
            .blocked_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        background_activity::clear_blocked(kind.as_str());
        let seq = runtime.service_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let claimed =
            match storage.mark_background_task_started(kind.as_str(), seq, manual, now_ms()) {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::debug!("[SCHEDULER] failed to claim {}: {error}", kind.as_str());
                    continue;
                }
            };
        if !claimed {
            tracing::debug!(
                "[SCHEDULER] {} was cancelled or became ineligible before claim",
                kind.as_str()
            );
            continue;
        }
        *runtime
            .running_task
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(kind.as_str().to_string());
        runtime.running_manual.store(manual, Ordering::SeqCst);
        let source = task_source(&app, kind, manual);
        let started = Instant::now();
        background_activity::index_start(kind.as_str(), source);
        tracing::info!(
            "[BACKGROUND] event=start run={} task={} source={} mode={}",
            seq,
            kind.as_str(),
            source.as_str(),
            if manual { "manual" } else { "automatic" },
        );
        let automatic_context =
            (!manual)
                .then(|| kind.automatic_quantum())
                .flatten()
                .map(|budget| {
                    AutomaticSliceContext::new(
                        budget,
                        runtime.manual_request_generation.clone(),
                        manual_generation_at_scan,
                    )
                });
        let lease = if manual {
            None
        } else {
            match runtime.adaptive.begin(
                &app,
                kind,
                source == WorkSource::Staged,
                manual_generation_at_scan,
            ) {
                Ok(lease) => Some(lease),
                Err(reason) => {
                    let _ = storage.defer_background_task(
                        kind.as_str(),
                        now_ms().saturating_add(250),
                        reason,
                    );
                    background_activity::index_finish();
                    *runtime
                        .running_task
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = None;
                    continue;
                }
            }
        };
        let execution = execute_slice(&app, kind, manual, automatic_context.as_ref());
        let result = if let Some(lease) = &lease {
            let result = background_policy::EXECUTION
                .scope(lease.clone(), execution)
                .await;
            if lease.profile == ExecutionProfile::Background {
                let _ = lease.rest(started.elapsed()).await;
            }
            runtime.adaptive.finish(lease);
            if let Some(reason) = lease.reason() {
                // A revoked slice may have committed earlier records; leave
                // their progress intact without advancing business completion.
                Ok(ScheduledSliceResult::skipped(reason))
            } else {
                result
            }
        } else {
            execution.await
        };
        let result = result.map(|mut slice| {
            slice.processed = slice.processed.max(background_activity::index_processed());
            slice
        });
        let processed = result.as_ref().map_or_else(
            |_| background_activity::index_processed(),
            |slice| slice.processed,
        );
        background_activity::index_finish();
        runtime.running_manual.store(false, Ordering::SeqCst);
        *runtime
            .running_task
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        match result {
            Ok(slice) if slice.skipped_reason.as_deref() == Some("disabled") => {
                tracing::info!(
                    "[BACKGROUND] event=end run={} task={} processed={} has_more={} outcome=disabled elapsed_ms={}",
                    seq,
                    kind.as_str(),
                    processed,
                    slice.has_more,
                    started.elapsed().as_millis(),
                );
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some("disabled".to_string());
                background_activity::blocked(kind.as_str(), "disabled");
                if let Err(error) = storage.park_background_task(kind.as_str(), "feature_disabled")
                {
                    tracing::debug!("[SCHEDULER] failed to park {}: {error}", kind.as_str());
                }
            }
            Ok(slice) if slice.skipped_reason.is_some() => {
                tracing::info!(
                    "[BACKGROUND] event=end run={} task={} processed={} has_more={} outcome=deferred reason={} elapsed_ms={}",
                    seq,
                    kind.as_str(),
                    processed,
                    slice.has_more,
                    slice.skipped_reason.as_deref().unwrap_or("deferred"),
                    started.elapsed().as_millis(),
                );
                background_activity::blocked(
                    kind.as_str(),
                    slice.skipped_reason.as_deref().unwrap_or("deferred"),
                );
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = slice.skipped_reason.clone();
                let _ = storage.defer_background_task(
                    kind.as_str(),
                    now_ms().saturating_add(TICK_INTERVAL.as_millis() as i64),
                    slice.skipped_reason.as_deref().unwrap_or("deferred"),
                );
            }
            Ok(slice) => {
                tracing::info!(
                    "[BACKGROUND] event=end run={} task={} processed={} has_more={} outcome=success elapsed_ms={}",
                    seq,
                    kind.as_str(),
                    processed,
                    slice.has_more,
                    started.elapsed().as_millis(),
                );
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                if let Err(error) =
                    storage.mark_background_task_succeeded(kind.as_str(), slice.has_more, now_ms())
                {
                    tracing::debug!("[SCHEDULER] completion persist failed: {error}");
                }
                // Completion is itself a scheduling boundary. Re-read the
                // ledger immediately so another ready task can run without
                // waiting for the two-second fallback tick. For an automatic
                // quantum with remaining work this is also what applies its
                // freshly updated `ready_since` against older tasks.
                runtime.wake.notify_one();
            }
            Err(error) => {
                if background_policy::is_pause(&error) {
                    let _ = storage.defer_background_task(
                        kind.as_str(),
                        now_ms().saturating_add(250),
                        "background_paused",
                    );
                    continue;
                }
                tracing::warn!(
                    "[BACKGROUND] event=end run={} task={} processed={} has_more=true outcome=failed elapsed_ms={}",
                    seq,
                    kind.as_str(),
                    processed,
                    started.elapsed().as_millis(),
                );
                let normalized = error.to_ascii_lowercase();
                if normalized.contains("auth_required")
                    || normalized.contains("authentication required")
                {
                    // CredentialManagerState records native CNG denials at the
                    // failed unwrap. A task's generic AUTH_REQUIRED must not
                    // revoke read access for an otherwise authenticated app.
                    let credential = app.state::<Arc<CredentialManagerState>>();
                    let reason = credential
                        .protected_read_wait_reason()
                        .unwrap_or("waiting_for_unlock");
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(reason.to_string());
                    background_activity::blocked(kind.as_str(), reason);
                    let _ = storage.defer_background_task(
                        kind.as_str(),
                        now_ms().saturating_add(TICK_INTERVAL.as_millis() as i64),
                        reason,
                    );
                    continue;
                }
                let failures = tasks
                    .iter()
                    .find(|task| task.task_kind == kind.as_str())
                    .map(|task| task.failure_count.saturating_add(1))
                    .unwrap_or(1);
                let smart_cluster_policy = (kind == BackgroundTaskKind::SmartCluster).then(|| {
                    crate::smart_cluster_scoring::scheduled_failure_policy(&error, failures)
                });
                let deterministic_index_failure = matches!(
                    kind,
                    BackgroundTaskKind::SemanticIndex | BackgroundTaskKind::ClipIndex
                )
                    && crate::semantic_runtime::is_deterministic_worker_failure(&error);
                let terminal_ann_failure = kind == BackgroundTaskKind::AnnBuild
                    && storage
                        .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
                        .ok()
                        .flatten()
                        .is_some_and(|state| state.circuit_open);
                if deterministic_index_failure
                    || terminal_ann_failure
                    || smart_cluster_policy.is_some_and(|policy| policy.terminal)
                {
                    let failure_code = smart_cluster_policy.map(|policy| policy.code).unwrap_or(
                        if deterministic_index_failure {
                            "index_worker_failure"
                        } else {
                            "task_failed"
                        },
                    );
                    let terminal_failed = storage
                        .mark_background_task_terminal_failure(kind.as_str(), failures, &error)
                        .unwrap_or(false);
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = if terminal_failed {
                        Some("failed".to_string())
                    } else {
                        None
                    };
                    if terminal_failed {
                        background_activity::blocked(kind.as_str(), "failed");
                    }
                    if kind == BackgroundTaskKind::SmartCluster && manual {
                        let worker = app
                            .state::<Arc<crate::smart_cluster_scoring::SmartClusterWorkerState>>();
                        if terminal_failed {
                            worker.record_scheduled_failure(&app, true);
                        } else {
                            worker.record_manual_requeued(&app);
                        }
                    }
                    tracing::error!(
                        "[SCHEDULER] {} slice reached terminal failure code={} after {} attempt(s): {}",
                        kind.as_str(),
                        failure_code,
                        failures,
                        error
                    );
                    continue;
                }
                let retry_at = if kind == BackgroundTaskKind::AnnBuild {
                    storage
                        .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
                        .ok()
                        .flatten()
                        .and_then(|state| {
                            chrono::DateTime::parse_from_rfc3339(&state.next_retry_at).ok()
                        })
                        .map(|at| at.timestamp_millis())
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    now_ms().saturating_add(retry_delay(failures).as_millis() as i64)
                });
                let _ =
                    storage.mark_background_task_failed(kind.as_str(), failures, retry_at, &error);
                if kind == BackgroundTaskKind::SmartCluster && manual {
                    let worker =
                        app.state::<Arc<crate::smart_cluster_scoring::SmartClusterWorkerState>>();
                    worker.record_scheduled_failure(&app, false);
                }
                if !normalized.contains("monitor_unavailable")
                    && !normalized.contains("monitor not started")
                {
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some("retry_wait".to_string());
                    background_activity::blocked(kind.as_str(), "retry_wait");
                }
                tracing::warn!(
                    "[SCHEDULER] {} slice failed (retry {}): {}",
                    kind.as_str(),
                    failures,
                    error
                );
            }
        }
    }
}

/// Metadata-only command used by settings while the UI may be locked.
#[tauri::command]
pub async fn background_scheduler_status(
    app: AppHandle,
    scheduler: tauri::State<'_, Arc<BackgroundSchedulerState>>,
) -> Result<BackgroundSchedulerStatus, String> {
    Ok(scheduler.status(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_postprocess_runs_during_activity_while_bulk_work_waits() {
        let idle = IdleState::new();
        let semantic = SemanticRuntimeState::new();
        for (ac_connected, fullscreen, bulk_reason) in [
            (true, false, "waiting_for_idle"),
            (false, false, "waiting_for_ac_power"),
            (true, true, "waiting_for_fullscreen"),
        ] {
            idle.ac_connected.store(ac_connected, Ordering::Relaxed);
            idle.fullscreen_exclusive
                .store(fullscreen, Ordering::Relaxed);
            assert_eq!(
                EnvironmentPolicy::Immediate.gate_reason(false, &idle, &semantic),
                None,
            );
            assert_eq!(
                EnvironmentPolicy::IdleOnly.gate_reason(false, &idle, &semantic),
                Some(bulk_reason),
            );
        }
    }

    #[test]
    fn postprocess_and_bulk_work_keep_maintenance_and_foreground_guards() {
        let idle = IdleState::new();
        idle.is_idle.store(true, Ordering::Relaxed);
        let semantic = Arc::new(SemanticRuntimeState::new());
        for policy in [EnvironmentPolicy::Immediate, EnvironmentPolicy::IdleOnly] {
            assert_eq!(
                policy.gate_reason(true, &idle, &semantic),
                Some("maintenance")
            );
            let foreground = semantic.foreground_lease();
            assert_eq!(
                policy.gate_reason(false, &idle, &semantic),
                Some("foreground_request"),
            );
            drop(foreground);
            assert_eq!(policy.gate_reason(false, &idle, &semantic), None);
        }
    }

    fn task(kind: &str, ready: i64, manual: bool, seq: u64) -> BackgroundTaskState {
        BackgroundTaskState {
            task_kind: kind.to_string(),
            ready_since_ms: ready,
            next_attempt_at_ms: 0,
            failure_count: 0,
            last_served_seq: seq,
            last_error: None,
            last_completed_at_ms: None,
            status: "queued".to_string(),
            manual_pending: manual,
            manual_in_flight: false,
        }
    }

    #[test]
    fn manual_requests_win_until_automatic_work_ages() {
        let tasks = vec![
            task(TASK_SEMANTIC_INDEX, 0, false, 1),
            task(TASK_CLIP_INDEX, 100, true, 2),
        ];
        assert_eq!(
            select_next_task(&tasks, 200, 1_000),
            Some(BackgroundTaskKind::ClipIndex)
        );
        assert_eq!(
            select_next_task(&tasks, 2_000, 1_000),
            Some(BackgroundTaskKind::SemanticIndex)
        );
    }

    #[test]
    fn an_aged_automatic_task_preempts_an_older_manual_row() {
        // A manual request may remain pending across several clicks. Once an
        // automatic row reaches the starvation bound, its age must win even
        // when the manual row was created first.
        let tasks = vec![
            task(TASK_CLIP_INDEX, 0, true, 1),
            task(TASK_SEMANTIC_INDEX, 100, false, 2),
        ];
        assert_eq!(
            select_next_task(&tasks, 1_100, 1_000),
            Some(BackgroundTaskKind::SemanticIndex)
        );
    }

    #[test]
    fn a_blocked_aged_automatic_task_does_not_hide_a_manual_smart_cluster_drain() {
        let tasks = vec![
            task(TASK_CLIP_INDEX, 0, false, 1),
            task(TASK_SMART_CLUSTER, 900, true, 2),
        ];
        let selected = select_next_runnable_task(&tasks, 2_000, 1_000, |task| task.manual_pending);
        assert_eq!(selected, Some((BackgroundTaskKind::SmartCluster, true)));
    }

    #[test]
    fn an_explicit_smart_cluster_drain_precedes_aged_automatic_work() {
        let tasks = vec![
            task(TASK_CLIP_INDEX, 0, false, 1),
            task(TASK_SMART_CLUSTER, 900, true, 2),
        ];
        assert_eq!(
            select_next_runnable_task(&tasks, 2_000, 1_000, |_| true),
            Some((BackgroundTaskKind::SmartCluster, true))
        );
    }

    #[test]
    fn ineligible_retry_is_skipped() {
        let mut retry = task(TASK_CLIP_INDEX, 0, false, 0);
        retry.status = "retry_wait".to_string();
        retry.next_attempt_at_ms = 10_000;
        assert_eq!(select_next_task(&[retry], 9_999, 1_000), None);
    }

    #[test]
    fn retry_backoff_is_bounded() {
        assert_eq!(retry_delay(1), Duration::from_secs(60));
        assert_eq!(retry_delay(2), Duration::from_secs(120));
        assert_eq!(retry_delay(99), MAX_RETRY_DELAY);
    }

    #[test]
    fn automatic_semantic_work_requires_smart_clusters() {
        for kind in [
            BackgroundTaskKind::SemanticIndex,
            BackgroundTaskKind::SmartCluster,
        ] {
            assert!(!task_feature_enabled_with_config(kind, false, false));
            assert!(task_feature_enabled_with_config(kind, false, true));
        }
        assert!(task_feature_enabled_with_config(
            BackgroundTaskKind::ClipIndex,
            false,
            false
        ));
        assert!(task_feature_enabled_with_config(
            BackgroundTaskKind::AnnBuild,
            false,
            false
        ));
    }

    #[test]
    fn manual_maintenance_remains_available_when_features_are_disabled() {
        for kind in [
            BackgroundTaskKind::SemanticIndex,
            BackgroundTaskKind::ClipIndex,
            BackgroundTaskKind::SmartCluster,
            BackgroundTaskKind::AnnBuild,
        ] {
            assert!(task_feature_enabled_with_config(kind, true, false));
        }
    }

    #[test]
    fn only_automatic_single_model_indexers_receive_a_quantum() {
        assert_eq!(
            BackgroundTaskKind::ClipIndex.automatic_quantum(),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            BackgroundTaskKind::SemanticIndex.automatic_quantum(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(BackgroundTaskKind::SmartCluster.automatic_quantum(), None);
    }

    #[test]
    fn an_expired_quantum_still_admits_its_first_model_batch() {
        let generation = Arc::new(AtomicU64::new(0));
        let mut context = AutomaticSliceContext::new(Duration::ZERO, generation, 0);
        context.deadline = context.started;
        let semantic = SemanticRuntimeState::new();

        assert_eq!(context.stop_reason(&semantic, false), None);
        assert_eq!(
            context.stop_reason(&semantic, true),
            Some(AutomaticSliceStopReason::BudgetExpired)
        );
    }

    #[test]
    fn a_quantum_context_uses_the_admission_generation_snapshot() {
        let generation = Arc::new(AtomicU64::new(4));
        let context = AutomaticSliceContext::new(Duration::from_secs(60), generation, 3);
        let semantic = SemanticRuntimeState::new();
        assert_eq!(
            context.stop_reason(&semantic, false),
            Some(AutomaticSliceStopReason::ManualRequestPending)
        );
    }

    #[test]
    fn a_manual_request_stops_an_admitted_automatic_quantum() {
        let generation = Arc::new(AtomicU64::new(7));
        let context = AutomaticSliceContext::new(Duration::from_secs(60), generation.clone(), 7);
        let semantic = SemanticRuntimeState::new();
        generation.fetch_add(1, Ordering::SeqCst);

        assert_eq!(
            context.stop_reason(&semantic, false),
            Some(AutomaticSliceStopReason::ManualRequestPending)
        );
    }

    #[test]
    fn an_external_background_request_stops_before_the_next_model_batch() {
        let generation = Arc::new(AtomicU64::new(0));
        let context = AutomaticSliceContext::new(Duration::from_secs(60), generation, 0);
        let semantic = Arc::new(SemanticRuntimeState::new());
        let _lease = semantic.external_background_lease();

        assert_eq!(
            context.stop_reason(&semantic, false),
            Some(AutomaticSliceStopReason::ExternalBackgroundRequest)
        );
    }

    #[test]
    fn smart_cluster_failure_policy_opens_on_deterministic_errors_or_budget() {
        let mismatch = crate::smart_cluster_scoring::scheduled_failure_policy(
            "model_mismatch: reranker returned 3 scores",
            1,
        );
        assert_eq!(mismatch.code, "model_mismatch");
        assert!(mismatch.terminal);

        let first = crate::smart_cluster_scoring::scheduled_failure_policy(
            "rerank_failed: worker returned an inference error",
            1,
        );
        assert_eq!(first.code, "inference_failed");
        assert!(!first.terminal);

        let exhausted = crate::smart_cluster_scoring::scheduled_failure_policy(
            "rerank_failed: worker returned an inference error",
            crate::smart_cluster_scoring::MAX_SCHEDULED_FAILURES,
        );
        assert!(exhausted.terminal);
    }

    #[tokio::test]
    async fn scheduler_waits_after_the_startup_pass_until_a_real_wake() {
        let wake = Notify::new();
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.tick().await;
        let mut first_pass = true;

        assert_eq!(
            wait_for_scheduler_wake(&mut first_pass, &mut ticker, &wake).await,
            SchedulerWake::Startup
        );

        assert!(tokio::time::timeout(
            Duration::from_millis(25),
            wait_for_scheduler_wake(&mut first_pass, &mut ticker, &wake),
        )
        .await
        .is_err());

        wake.notify_one();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_scheduler_wake(&mut first_pass, &mut ticker, &wake),
            )
            .await
            .expect("external notification should wake the scheduler"),
            SchedulerWake::Notification
        );
        assert!(!should_refresh_backlog(SchedulerWake::Notification));
        assert!(should_refresh_backlog(SchedulerWake::Startup));
        assert!(should_refresh_backlog(SchedulerWake::Tick));
    }
}
