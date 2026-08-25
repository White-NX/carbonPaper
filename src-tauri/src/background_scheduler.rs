//! Unified scheduling for automatic semantic/cluster work.
//!
//! The task implementations still own their transactional queues. This module
//! owns the one decision that used to be duplicated four times: which slice is
//! allowed to claim the single semantic worker next.

use crate::credential_manager::CredentialManagerState;
use crate::idle::IdleState;
use crate::monitor::MonitorState;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{BackgroundTaskState, DerivedIndexKind, StorageState};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

pub const TASK_SEMANTIC_INDEX: &str = "semantic_index";
pub const TASK_CLIP_INDEX: &str = "clip_index";
pub const TASK_SMART_CLUSTER: &str = "smart_cluster";
pub const TASK_PYTHON_CLUSTERING: &str = "python_clustering";

/// Automatic work may wait behind a user-requested pass for this long before
/// becoming eligible to reclaim the head of the queue.
pub const AUTO_AGING_LIMIT: Duration = Duration::from_secs(10 * 60);
const TICK_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    SemanticIndex,
    ClipIndex,
    SmartCluster,
    PythonClustering,
}

impl BackgroundTaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SemanticIndex => TASK_SEMANTIC_INDEX,
            Self::ClipIndex => TASK_CLIP_INDEX,
            Self::SmartCluster => TASK_SMART_CLUSTER,
            Self::PythonClustering => TASK_PYTHON_CLUSTERING,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            TASK_SEMANTIC_INDEX => Some(Self::SemanticIndex),
            TASK_CLIP_INDEX => Some(Self::ClipIndex),
            TASK_SMART_CLUSTER => Some(Self::SmartCluster),
            TASK_PYTHON_CLUSTERING => Some(Self::PythonClustering),
            _ => None,
        }
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
    pub skipped_reason: Option<String>,
}

impl ScheduledSliceResult {
    pub(crate) fn complete(has_more: bool) -> Self {
        Self {
            completed: true,
            has_more,
            skipped_reason: None,
        }
    }

    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        Self {
            completed: false,
            has_more: true,
            skipped_reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundSchedulerStatus {
    pub enabled: bool,
    pub running_task: Option<String>,
    pub tasks: Vec<BackgroundTaskState>,
    pub queue_depths: serde_json::Value,
    pub blocked_reason: Option<String>,
    pub next_retry_at_ms: Option<i64>,
    pub worker_restart_count: u64,
    pub monitor_restart_degraded: bool,
}

struct SchedulerRuntime {
    stop: AtomicBool,
    wake: Notify,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    running_task: Mutex<Option<String>>,
    blocked_reason: Mutex<Option<String>>,
    service_seq: AtomicU64,
    worker_restart_count: AtomicU64,
    worker_restart_attempts: Mutex<VecDeque<i64>>,
    /// Automatic monitor recovery remains degraded until a manual start or a
    /// new process. A rolling window by itself would silently resume a worker
    /// that has already failed repeatedly in this process.
    monitor_restart_degraded: AtomicBool,
}

impl Default for SchedulerRuntime {
    fn default() -> Self {
        Self {
            stop: AtomicBool::new(false),
            wake: Notify::new(),
            task: Mutex::new(None),
            running_task: Mutex::new(None),
            blocked_reason: Mutex::new(None),
            service_seq: AtomicU64::new(0),
            worker_restart_count: AtomicU64::new(0),
            worker_restart_attempts: Mutex::new(VecDeque::new()),
            monitor_restart_degraded: AtomicBool::new(false),
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
        migrate_legacy_clustering_config(&app);
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
            for kind in [TASK_SEMANTIC_INDEX, TASK_CLIP_INDEX] {
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
        *task = Some(tauri::async_runtime::spawn(async move {
            scheduler_loop(app, runtime).await;
        }));
    }

    pub fn stop(&self) {
        self.runtime.stop.store(true, Ordering::SeqCst);
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

    /// Release the in-process monitor degradation after an explicit manual
    /// start. The durable Python task is released at the same boundary so it
    /// cannot remain parked on the old degraded state.
    pub fn clear_monitor_restart_degraded(&self, app: &AppHandle) {
        self.runtime
            .monitor_restart_degraded
            .store(false, Ordering::SeqCst);
        self.runtime
            .worker_restart_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Some(storage) = app.try_state::<Arc<StorageState>>() {
            let _ = storage.resume_background_task(TASK_PYTHON_CLUSTERING, now_ms());
        }
        self.wake();
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
        // This is the externally visible admission path. Even when the row is
        // already queued, a caller may be retrying after changing an admission
        // condition, so preserve the prompt wake-up semantics. The internal
        // backlog reconciliation deliberately bypasses this method.
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
            .clone();
        let depths = queue_depths(app.try_state::<Arc<StorageState>>());
        let enabled = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|state| state.background_processing_enabled())
            .unwrap_or(false);
        BackgroundSchedulerStatus {
            enabled,
            running_task,
            tasks,
            queue_depths: depths,
            blocked_reason,
            next_retry_at_ms,
            worker_restart_count: self.runtime.worker_restart_count.load(Ordering::Relaxed),
            monitor_restart_degraded: self
                .runtime
                .monitor_restart_degraded
                .load(Ordering::Relaxed),
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

fn clustering_interval_secs(key: &str) -> u64 {
    match key {
        "1d" => 86_400,
        "1m" => 2_592_000,
        "6m" => 15_552_000,
        _ => 604_800,
    }
}

fn migrate_legacy_clustering_config(app: &AppHandle) {
    let Some(storage) = app.try_state::<Arc<StorageState>>() else {
        return;
    };
    if storage
        .background_scheduler_task(TASK_PYTHON_CLUSTERING)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }

    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let path = data_dir.join("clustering_config.json");
    let mut last_run_ms = 0i64;
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&contents) {
            if let Some(interval) = config.get("interval").and_then(|value| value.as_str()) {
                if matches!(interval, "1d" | "1w" | "1m" | "6m") {
                    let _ = crate::registry_config::set_string("clustering_interval", interval);
                }
            }
            last_run_ms = config
                .get("last_run")
                .and_then(|value| value.as_f64())
                .map(|seconds| (seconds.max(0.0) * 1_000.0) as i64)
                .unwrap_or(0);
        }
    }
    let now = now_ms();
    if storage
        .enqueue_background_task(TASK_PYTHON_CLUSTERING, false, now)
        .is_ok()
        && last_run_ms > 0
    {
        let _ = storage.mark_background_task_succeeded(TASK_PYTHON_CLUSTERING, false, last_run_ms);
    }
    if path.exists() {
        let migrated = data_dir.join("clustering_config.json.migrated");
        if let Err(error) = std::fs::rename(&path, &migrated) {
            tracing::debug!("[SCHEDULER] legacy clustering config rename failed: {error}");
        }
    }
}

fn queue_depths(storage: Option<tauri::State<'_, Arc<StorageState>>>) -> serde_json::Value {
    let Some(storage) = storage else {
        return serde_json::json!({});
    };
    let semantic = storage
        .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
        .map(|backlog| backlog.claimable)
        .unwrap_or(0);
    let clip = storage
        .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
        .map(|backlog| backlog.claimable)
        .unwrap_or(0);
    let smart = storage.count_smart_cluster_pending().unwrap_or(0).max(0) as u64;
    serde_json::json!({
        TASK_SEMANTIC_INDEX: semantic,
        TASK_CLIP_INDEX: clip,
        TASK_SMART_CLUSTER: smart,
    })
}

fn gate_reason(app: &AppHandle, manual: bool) -> Option<&'static str> {
    let credential = app.state::<Arc<CredentialManagerState>>();
    if manual {
        if !credential.is_session_valid() && !credential.background_authorized() {
            return Some("waiting_for_unlock");
        }
    } else if !credential.background_authorized() {
        return Some("waiting_for_unlock");
    }
    if crate::maintenance::is_active() {
        return Some("maintenance");
    }
    if manual {
        if app
            .state::<Arc<SemanticRuntimeState>>()
            .foreground_waiting()
        {
            return Some("foreground_request");
        }
        return None;
    }
    let idle = app.state::<Arc<IdleState>>();
    if !idle.ac_connected.load(Ordering::Relaxed) {
        return Some("waiting_for_ac_power");
    }
    if idle.fullscreen_exclusive.load(Ordering::Relaxed) {
        return Some("waiting_for_fullscreen");
    }
    if !idle.is_idle.load(Ordering::Relaxed) {
        return Some("waiting_for_idle");
    }
    if app
        .state::<Arc<SemanticRuntimeState>>()
        .foreground_waiting()
    {
        return Some("foreground_request");
    }
    None
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
            .map(|backlog| backlog.claimable)
            .unwrap_or(0);
        let clip = count_storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
            .map(|backlog| backlog.claimable)
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
    if semantic > 0 {
        let _ = storage.enqueue_background_task_if_changed(TASK_SEMANTIC_INDEX, false, now_ms());
    }
    if clip > 0 {
        let _ = storage.enqueue_background_task_if_changed(TASK_CLIP_INDEX, false, now_ms());
    }
    if smart > 0 {
        let _ = storage.enqueue_background_task_if_changed(TASK_SMART_CLUSTER, false, now_ms());
    }

    if crate::registry_config::get_bool("clustering_enabled").unwrap_or(true) {
        let interval_key = crate::registry_config::get_string("clustering_interval")
            .unwrap_or_else(|| "1w".to_string());
        let interval_ms = clustering_interval_secs(&interval_key).saturating_mul(1_000) as i64;
        let due = storage
            .background_scheduler_task(TASK_PYTHON_CLUSTERING)
            .ok()
            .flatten()
            .map(|task| {
                task.status == "parked"
                    || (!matches!(task.status.as_str(), "running" | "degraded")
                        && task
                            .last_completed_at_ms
                            .map(|last| now_ms().saturating_sub(last) >= interval_ms)
                            .unwrap_or(true))
            })
            .unwrap_or(true);
        if due {
            let _ =
                storage.enqueue_background_task_if_changed(TASK_PYTHON_CLUSTERING, false, now_ms());
        }
    }
}

async fn execute_slice(
    app: &AppHandle,
    kind: BackgroundTaskKind,
    manual: bool,
    runtime: &SchedulerRuntime,
) -> Result<ScheduledSliceResult, String> {
    // Python's HDBSCAN run is intentionally non-preemptible, but it does not
    // own the Rust semantic model slot for the duration of that computation.
    // Holding BACKGROUND_PASS_GUARD here would make a foreground search wait
    // for the whole clustering run. The Rust adapters below still claim the
    // guard for every request that actually drives the shared semantic worker.
    if kind == BackgroundTaskKind::PythonClustering {
        // The registry is the application-side source of truth. A queued row
        // can outlive a settings change, and the Python config update may fail
        // when the monitor is stopped, so reject disabled work before starting
        // (or restarting) that process.
        if !crate::registry_config::get_bool("clustering_enabled").unwrap_or(true) {
            return Ok(ScheduledSliceResult::skipped("disabled"));
        }
        let monitor_running = {
            let monitor = app.state::<MonitorState>();
            let running = monitor
                .process
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            running
        };
        if !monitor_running {
            if runtime.monitor_restart_degraded.load(Ordering::SeqCst) {
                return Err("monitor_unavailable: restart limit reached".to_string());
            }
            if !crate::registry_config::get_bool("autoStartMonitor").unwrap_or(true) {
                return Err("monitor_unavailable".to_string());
            }
            let now = now_ms();
            {
                let mut attempts = runtime
                    .worker_restart_attempts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                while attempts
                    .front()
                    .is_some_and(|attempt| now.saturating_sub(*attempt) >= 3_600_000)
                {
                    attempts.pop_front();
                }
                if attempts.len() >= 3 {
                    runtime
                        .monitor_restart_degraded
                        .store(true, Ordering::SeqCst);
                    return Err("monitor_unavailable: restart limit reached".to_string());
                }
                attempts.push_back(now);
            }
            runtime.worker_restart_count.fetch_add(1, Ordering::SeqCst);
            crate::monitor::start_monitor_impl(app.state::<MonitorState>(), app.clone()).await?;
        }
        let monitor = app.state::<MonitorState>();
        let response = crate::monitor::forward_command_to_python(
            &monitor,
            serde_json::json!({
                "command": "run_scheduled_clustering",
                "timeout_secs": 1800,
                "manual": manual,
            }),
        )
        .await?;
        if response.get("status").and_then(|v| v.as_str()) == Some("success") {
            let result_status = response
                .get("result")
                .and_then(|value| value.get("status"))
                .and_then(|value| value.as_str());
            if result_status == Some("already_running") {
                Ok(ScheduledSliceResult::skipped("clustering_already_running"))
            } else if result_status == Some("disabled") {
                Ok(ScheduledSliceResult::skipped("disabled"))
            } else if result_status == Some("waiting_for_index") {
                // Python deliberately leaves the work untouched until the
                // Rust semantic index has produced vectors. Treat this as a
                // deferred slice, not a completed interval run; otherwise a
                // fresh `last_completed_at` would hide the backlog for the
                // whole configured clustering interval.
                Ok(ScheduledSliceResult::skipped("waiting_for_index"))
            } else {
                Ok(ScheduledSliceResult::complete(false))
            }
        } else {
            Err(response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("python clustering failed")
                .to_string())
        }
    } else {
        let Ok(_worker_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
            return Ok(ScheduledSliceResult::skipped("semantic_worker_busy"));
        };
        match kind {
            BackgroundTaskKind::SemanticIndex => {
                crate::minilm_index::run_scheduled_slice(app, manual).await
            }
            BackgroundTaskKind::ClipIndex => {
                crate::clip_index::run_scheduled_slice(app, manual).await
            }
            BackgroundTaskKind::SmartCluster => {
                crate::smart_cluster_scoring::run_scheduled_slice(app, manual).await
            }
            BackgroundTaskKind::PythonClustering => unreachable!(),
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
        if should_refresh_backlog(wake_reason) {
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
        let selected =
            select_next_runnable_task(&tasks, now, AUTO_AGING_LIMIT.as_millis() as i64, |task| {
                gate_reason(&app, task.manual_pending).is_none()
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
                continue;
            };
            let manual = tasks
                .iter()
                .find(|task| task.task_kind == kind.as_str())
                .map(|task| task.manual_pending)
                .unwrap_or(false);
            (kind, manual)
        };
        if let Some(reason) = gate_reason(&app, manual) {
            *runtime
                .blocked_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(reason.to_string());
            continue;
        }
        // A task that was previously blocked by an admission gate is now
        // admitted. Clear that transient reason before the slice starts so a
        // later retry/failure cannot be rendered as the stale gate state.
        *runtime
            .blocked_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
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
        let result = execute_slice(&app, kind, manual, &runtime).await;
        *runtime
            .running_task
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        match result {
            Ok(slice) if slice.skipped_reason.as_deref() == Some("disabled") => {
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some("disabled".to_string());
                if let Err(error) = storage.park_background_task(kind.as_str(), "feature_disabled")
                {
                    tracing::debug!("[SCHEDULER] failed to park {}: {error}", kind.as_str());
                }
            }
            Ok(slice) if slice.skipped_reason.is_some() => {
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
                *runtime
                    .blocked_reason
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                if let Err(error) =
                    storage.mark_background_task_succeeded(kind.as_str(), slice.has_more, now_ms())
                {
                    tracing::debug!("[SCHEDULER] completion persist failed: {error}");
                }
            }
            Err(error) => {
                let normalized = error.to_ascii_lowercase();
                if normalized.contains("clustering_already_running") {
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some("clustering_already_running".to_string());
                    let _ = storage.defer_background_task(
                        kind.as_str(),
                        now_ms().saturating_add(TICK_INTERVAL.as_millis() as i64),
                        "clustering_already_running",
                    );
                    continue;
                }
                if normalized.contains("auth_required")
                    || normalized.contains("authentication required")
                {
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some("waiting_for_unlock".to_string());
                    let _ = storage.defer_background_task(
                        kind.as_str(),
                        now_ms().saturating_add(TICK_INTERVAL.as_millis() as i64),
                        "waiting_for_unlock",
                    );
                    continue;
                }
                if normalized.contains("monitor_unavailable")
                    || normalized.contains("monitor not started")
                {
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some("monitor_unavailable".to_string());
                }
                if normalized.contains("restart limit reached") {
                    let _ = storage.mark_background_task_degraded(
                        kind.as_str(),
                        "monitor restart limit reached; manual start required",
                    );
                    continue;
                }
                let failures = tasks
                    .iter()
                    .find(|task| task.task_kind == kind.as_str())
                    .map(|task| task.failure_count.saturating_add(1))
                    .unwrap_or(1);
                let retry_at = now_ms().saturating_add(retry_delay(failures).as_millis() as i64);
                let _ =
                    storage.mark_background_task_failed(kind.as_str(), failures, retry_at, &error);
                if !normalized.contains("monitor_unavailable")
                    && !normalized.contains("monitor not started")
                {
                    *runtime
                        .blocked_reason
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some("retry_wait".to_string());
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
