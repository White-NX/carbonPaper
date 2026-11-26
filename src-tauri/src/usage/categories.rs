//! Cached application classification. The recorder only performs memory lookups;
//! executable metadata and local inference belong to a separate idle task.
use super::{
    metadata::{self, PreparedApplication},
    types::UsageAppIdentity,
    UsageRuntime,
};
use crate::{
    classification::scoring,
    storage::{StorageState, StoredCategory},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{sync::Notify, time::Instant};

pub(crate) type AppKey = [u8; 32];
const MAX_APPS: usize = 128;
const MAX_PENDING: usize = 32;
const NEGATIVE_CACHE_MS: i64 = 86_400_000;
const UNKNOWN: &str = "未分类";
const CATEGORIES: &[&str] = &[
    "编程开发",
    "学习教育",
    "影音娱乐",
    "社交通讯",
    "办公文档",
    "网页浏览",
    "游戏",
    "设计创作",
    "系统工具",
    "阅读资讯",
    UNKNOWN,
];

pub(crate) fn validate_app_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 260 || id.trim() != id || id.contains(['\\', '/', '\0', ':']) {
        return Err("USAGE_INVALID_APP".into());
    }
    Ok(())
}
pub(crate) fn validate_category(category: &str) -> Result<(), String> {
    if !CATEGORIES.contains(&category) {
        return Err("USAGE_INVALID_CATEGORY".into());
    }
    Ok(())
}
pub(crate) fn override_key(process: &str) -> AppKey {
    Sha256::digest(format!(
        "CarbonPaper-UsageCategory-Override-v1\0{}",
        process.to_lowercase()
    ))
    .into()
}
pub(crate) fn app_key(app: &UsageAppIdentity) -> AppKey {
    let path = app.process_path.replace('\\', "/").to_lowercase();
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    let mut hash = Sha256::new();
    hash.update(b"CarbonPaper-UsageCategory-Application-v1\0");
    hash.update(app.process_name.to_lowercase().as_bytes());
    hash.update([0]);
    hash.update(path.as_bytes());
    hash.finalize().into()
}
pub(crate) fn builtin_category(app: &UsageAppIdentity) -> Option<&'static str> {
    let process = app.process_name.to_lowercase();
    scoring::process_category(&app.process_name)
        .or_else(|| crate::capture::is_browser_process(&app.process_name).then_some("网页浏览"))
        .or_else(|| scoring::application_name_category(&app.name))
        .or_else(|| {
            scoring::application_name_category(process.strip_suffix(".exe").unwrap_or(&process))
        })
}
pub(crate) fn effective_category(
    stored: &str,
    manual: Option<&str>,
    automatic: Option<&str>,
) -> String {
    manual.or(automatic).unwrap_or(stored).to_owned()
}

struct Entry {
    app: Arc<UsageAppIdentity>,
    category: String,
    automatic_category: String,
    manual: bool,
    builtin: bool,
    job_id: Option<u64>,
    complete: bool,
    checked_ms: i64,
    pid: Option<u32>,
}
struct Job {
    id: u64,
    generation: u64,
    key: AppKey,
    app: Arc<UsageAppIdentity>,
    lookup_done: bool,
    cached: Option<StoredCategory>,
    prepared: Option<PreparedApplication>,
    retry_at: Instant,
}
enum Preparation {
    Cached,
    Prepared(PreparedApplication),
    Deferred,
}
#[derive(Default)]
struct CacheState {
    generation: u64,
    entries: HashMap<AppKey, Entry>,
    order: VecDeque<AppKey>,
    manual: HashMap<AppKey, String>,
    manual_order: VecDeque<AppKey>,
    pending: VecDeque<Job>,
    next_job_id: u64,
    manual_revision: u64,
}
#[derive(Default)]
pub(crate) struct CategoryCache {
    inner: Mutex<CacheState>,
    wake: Notify,
}
impl CategoryCache {
    fn state(&self, generation: u64) -> std::sync::MutexGuard<'_, CacheState> {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation < generation {
            *state = CacheState {
                generation,
                ..CacheState::default()
            };
        }
        state
    }
    fn insert(state: &mut CacheState, key: AppKey, app: Arc<UsageAppIdentity>) {
        if state.entries.contains_key(&key) {
            return;
        }
        let category = builtin_category(&app);
        let automatic_category = category.unwrap_or(UNKNOWN).to_owned();
        let manual = state.manual.get(&override_key(&app.process_name));
        state.entries.insert(
            key,
            Entry {
                app,
                category: manual.unwrap_or(&automatic_category).clone(),
                automatic_category,
                manual: manual.is_some(),
                builtin: category.is_some(),
                job_id: None,
                complete: category.is_some() || manual.is_some(),
                checked_ms: 0,
                pid: None,
            },
        );
        state.order.push_back(key);
        if state.order.len() > MAX_APPS {
            if let Some(old) = state.order.pop_front() {
                state.entries.remove(&old);
                state.pending.retain(|job| job.key != old);
            }
        }
    }
    pub(crate) fn category(
        &self,
        generation: u64,
        app: Arc<UsageAppIdentity>,
        pid: Option<u32>,
    ) -> String {
        let key = app_key(&app);
        let mut state = self.state(generation);
        if state.generation != generation {
            return builtin_category(&app).unwrap_or(UNKNOWN).into();
        }
        // A history query must not evict the recorder's hot identities or cycle
        // through a full queue just to return an unassigned category.
        if pid.is_none()
            && !state.entries.contains_key(&key)
            && (state.entries.len() >= MAX_APPS || state.pending.len() >= MAX_PENDING)
        {
            return state
                .manual
                .get(&override_key(&app.process_name))
                .cloned()
                .unwrap_or_else(|| builtin_category(&app).unwrap_or(UNKNOWN).into());
        }
        Self::insert(&mut state, key, app.clone());
        if pid.is_some() {
            state.order.retain(|old| *old != key);
            state.order.push_back(key);
        }
        let has_capacity = state.pending.len() < MAX_PENDING;
        let job_id = state.next_job_id;
        let entry = state.entries.get_mut(&key).unwrap();
        let manual = entry.manual;
        if pid.is_some() && entry.pid != pid && !entry.builtin {
            entry.complete = false; // Verify executable changes off the recorder thread.
        }
        if pid.is_some() {
            entry.pid = pid;
        }
        if !manual
            && entry.automatic_category == UNKNOWN
            && entry.complete
            && !cache_is_fresh(
                None,
                entry.checked_ms,
                chrono::Utc::now().timestamp_millis(),
            )
        {
            entry.complete = false;
        }
        let enqueue =
            !manual && !entry.builtin && !entry.complete && entry.job_id.is_none() && has_capacity;
        if enqueue {
            entry.job_id = Some(job_id);
        }
        let category = entry.category.clone();
        if enqueue {
            state.next_job_id += 1;
            state.pending.push_back(Job {
                id: job_id,
                generation,
                key,
                app,
                lookup_done: false,
                cached: None,
                prepared: None,
                retry_at: Instant::now(),
            });
            self.wake.notify_one();
        }
        category
    }
    pub(crate) fn publish(
        &self,
        generation: u64,
        app: &Arc<UsageAppIdentity>,
        category: Option<String>,
        checked_ms: i64,
    ) {
        let key = app_key(app);
        let mut state = self.state(generation);
        if state.generation != generation {
            return;
        }
        Self::publish_entry(&mut state, key, category.as_deref(), checked_ms);
    }
    fn publish_entry(state: &mut CacheState, key: AppKey, category: Option<&str>, checked_ms: i64) {
        if let Some(entry) = state.entries.get_mut(&key) {
            if !entry.builtin && checked_ms >= entry.checked_ms {
                entry.automatic_category = category.unwrap_or(UNKNOWN).into();
                if !entry.manual {
                    entry.category = entry.automatic_category.clone();
                }
                entry.checked_ms = checked_ms;
            }
        }
    }
    pub(crate) fn manual_revision(&self, generation: u64) -> u64 {
        self.state(generation).manual_revision
    }
    /// Resolve persisted rules directly, including fresh NULL results. Only
    /// existing memory entries are hydrated, so cold history cannot churn the
    /// bounded recorder cache or enqueue another database lookup.
    pub(crate) fn history_category(
        &self,
        generation: u64,
        app: Arc<UsageAppIdentity>,
        manual: Option<&str>,
        cached: Option<&(Option<String>, i64)>,
        manual_revision: u64,
        now_ms: i64,
    ) -> String {
        let key = app_key(&app);
        let fresh = cached.filter(|(category, checked_ms)| {
            cache_is_fresh(category.as_deref(), *checked_ms, now_ms)
        });
        let builtin = builtin_category(&app);
        let mut state = self.state(generation);
        if state.generation != generation {
            return builtin.unwrap_or(UNKNOWN).into();
        }
        if state.entries.contains_key(&key) {
            // A query admitted before a manual edit must not restore old rules
            // after its database read releases the connection.
            if state.manual_revision == manual_revision {
                Self::set_manual(&mut state, &app.process_name, manual, false);
            }
            if let Some((category, checked_ms)) = cached {
                Self::publish_entry(&mut state, key, category.as_deref(), *checked_ms);
            }
            let entry = state.entries.get_mut(&key).unwrap();
            if fresh.is_some() && entry.pid.is_none() {
                entry.complete = true;
                entry.job_id = None;
                state.pending.retain(|job| job.key != key);
            }
            if manual.is_some() || builtin.is_some() || fresh.is_some() {
                return state.entries[&key].category.clone();
            }
        }
        drop(state);
        if manual.is_some() || builtin.is_some() || fresh.is_some() {
            return manual
                .or(builtin)
                .or_else(|| fresh.and_then(|(category, _)| category.as_deref()))
                .unwrap_or(UNKNOWN)
                .into();
        }
        self.category(generation, app, None)
    }
    pub(crate) fn manual_changed(&self, generation: u64, id: &str, category: Option<&str>) {
        let mut state = self.state(generation);
        if state.generation != generation {
            return;
        }
        state.manual_revision += 1;
        Self::set_manual(&mut state, id, category, true);
        self.wake.notify_one();
    }
    fn set_manual(state: &mut CacheState, id: &str, category: Option<&str>, explicit_change: bool) {
        let key = override_key(id);
        let previous = if let Some(category) = category {
            state.manual.insert(key, category.to_owned())
        } else {
            state.manual.remove(&key)
        };
        let changed = previous.as_deref() != category;
        if category.is_some() && changed {
            state.manual_order.retain(|old| *old != key);
            state.manual_order.push_back(key);
            if state.manual_order.len() > MAX_APPS {
                if let Some(old) = state.manual_order.pop_front() {
                    state.manual.remove(&old);
                }
            }
        } else if category.is_none() {
            state.manual_order.retain(|old| *old != key);
        }
        // A bounded override map can evict a rule while its hot entry survives.
        // Explicit edits must still update that entry, including clearing a rule.
        if changed || explicit_change {
            for entry in state
                .entries
                .values_mut()
                .filter(|entry| override_key(&entry.app.process_name) == key)
            {
                entry.category = category.unwrap_or(&entry.automatic_category).into();
                entry.manual = category.is_some();
                entry.job_id = None;
                if !entry.builtin {
                    entry.complete = category.is_some();
                }
            }
            state
                .pending
                .retain(|job| override_key(&job.app.process_name) != key);
        }
    }
    fn next_job(&self, generation: u64) -> Option<Job> {
        let mut state = self.state(generation);
        if state.generation != generation {
            return None;
        }
        for _ in 0..state.pending.len() {
            let job = state.pending.pop_front()?;
            if job.retry_at <= Instant::now() {
                return Some(job);
            }
            state.pending.push_back(job);
        }
        None
    }
    fn defer(&self, job: Job, delay: Duration) {
        self.defer_inner(job, delay, false);
    }
    fn defer_gated(&self, job: Job, delay: Duration) {
        self.defer_inner(job, delay, true);
    }
    fn defer_inner(&self, mut job: Job, delay: Duration, gate_closed: bool) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != job.generation
            || !state
                .entries
                .get(&job.key)
                .is_some_and(|e| e.job_id == Some(job.id))
        {
            return;
        }
        job.retry_at = Instant::now() + delay;
        if gate_closed {
            // Admission is shared by every job. Recheck it once at the deadline,
            // rather than running the same native gates for the whole queue.
            for pending in &mut state.pending {
                if !job.lookup_done || pending.lookup_done {
                    pending.retry_at = pending.retry_at.max(job.retry_at);
                }
            }
        }
        if state.pending.len() < MAX_PENDING {
            state.pending.push_back(job);
        } else if let Some(entry) = state.entries.get_mut(&job.key) {
            entry.job_id = None;
        }
    }
    fn finish(&self, job: &Job, category: Option<String>, checked_ms: i64) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != job.generation {
            return;
        }
        if let Some(entry) = state.entries.get_mut(&job.key) {
            if entry.job_id != Some(job.id) || entry.manual {
                return;
            }
            entry.job_id = None;
            entry.complete = true;
            if !entry.builtin && checked_ms >= entry.checked_ms {
                entry.automatic_category = category.unwrap_or_else(|| UNKNOWN.into());
                entry.category = entry.automatic_category.clone();
            }
            entry.checked_ms = entry.checked_ms.max(checked_ms);
        }
    }
    fn finish_manual(&self, job: &Job, category: &str) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != job.generation
            || !state
                .entries
                .get(&job.key)
                .is_some_and(|entry| entry.job_id == Some(job.id) && !entry.manual)
        {
            return;
        }
        // Validate and publish under the same lock so a late database lookup
        // cannot replace a manual edit or an explicit reset made while it ran.
        state.manual_revision += 1;
        Self::set_manual(&mut state, &job.app.process_name, Some(category), false);
    }
    async fn wait_for_work(&self, generation: u64) {
        let notified = self.wake.notified();
        let retry_at = self
            .state(generation)
            .pending
            .iter()
            .map(|job| job.retry_at)
            .min();
        if let Some(retry_at) = retry_at {
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep_until(retry_at) => {},
            }
        } else {
            notified.await;
        }
    }
    fn job_is_current(&self, job: &Job) -> bool {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.generation == job.generation
            && state
                .entries
                .get(&job.key)
                .is_some_and(|entry| entry.job_id == Some(job.id) && !entry.manual)
    }
    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().pending.len()
    }
}

pub(crate) fn gate_reason(
    app: &AppHandle,
    storage: &StorageState,
    generation: u64,
) -> Option<&'static str> {
    if generation != storage.db_generation() {
        return Some("USAGE_SOURCE_CHANGED");
    }
    if crate::IS_QUITTING.load(Ordering::Relaxed) || crate::IS_UPDATING.load(Ordering::Relaxed) {
        return Some("shutdown");
    }
    if crate::maintenance::is_active() || storage.is_database_maintenance_pending() {
        return Some("maintenance");
    }
    if !storage.is_background_authorized() {
        return Some("waiting_for_unlock");
    }
    let capture = app.state::<Arc<crate::capture::CaptureState>>();
    let protected = capture.game_mode_capture_paused.load(Ordering::Relaxed)
        || app
            .state::<crate::monitor::MonitorState>()
            .is_dml_suppressed()
        || crate::capture::check_foreground_fullscreen()
            .is_none_or(|(_, class, full)| full && !crate::capture::is_system_window_class(&class));
    admission_reason(
        crate::power::is_ac_power_connected(),
        protected,
        crate::idle::input_idle_ms(),
        app.state::<Arc<crate::semantic_runtime::SemanticRuntimeState>>()
            .foreground_waiting(),
    )
}
fn admission_reason(
    ac: bool,
    protected: bool,
    idle_ms: Option<u64>,
    foreground: bool,
) -> Option<&'static str> {
    if !ac {
        Some("waiting_for_ac_power")
    } else if protected {
        Some("waiting_for_fullscreen")
    } else if foreground {
        Some("foreground_request")
    } else if idle_ms.is_none_or(|idle| idle < crate::background_policy::SHORT_IDLE_SECS * 1000) {
        Some("waiting_for_idle")
    } else {
        None
    }
}
fn confident_category(result: &scoring::Classification) -> Option<String> {
    let scores = result.debug.get("top_scores")?.as_array()?;
    let best = scores.first()?.get("score")?.as_f64()?;
    let next = scores
        .get(1)
        .and_then(|s| s.get("score"))
        .and_then(|s| s.as_f64())
        .unwrap_or(0.0);
    (result.category != UNKNOWN
        && validate_category(&result.category).is_ok()
        && result.score >= 0.60
        && best - next >= 0.08)
        .then(|| result.category.clone())
}
fn cache_matches(record: &StoredCategory, fingerprint: &[u8], now_ms: i64) -> bool {
    record.fingerprint == fingerprint
        && cache_is_fresh(record.category.as_deref(), record.checked_ms, now_ms)
}
fn cache_is_fresh(category: Option<&str>, checked_ms: i64, now_ms: i64) -> bool {
    category.is_some() || now_ms.saturating_sub(checked_ms) < NEGATIVE_CACHE_MS
}
fn inference_retry_delay(error: &str, revoked: bool) -> Duration {
    Duration::from_secs(
        if revoked || crate::classification::scheduling_deferred(error) {
            5
        } else {
            300
        },
    )
}

pub(super) async fn run(app: AppHandle, runtime: Arc<UsageRuntime>) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let mut admission_generation = storage.db_generation();
    let mut admission_retry_at = Instant::now();
    while !runtime.shutdown.load(Ordering::Acquire) {
        let cache = &storage.usage_categories;
        let Some(mut job) = cache.next_job(storage.db_generation()) else {
            cache.wait_for_work(storage.db_generation()).await;
            continue;
        };
        if !storage.is_initialized()
            || !crate::storage_startup::is_ready(&app)
            || !storage.is_silent_read_authorized()
            || crate::maintenance::is_active()
            || storage.is_database_maintenance_pending()
        {
            cache.defer_gated(job, Duration::from_secs(5));
            continue;
        }
        if !job.lookup_done {
            let task_storage = storage.clone();
            let task_app = job.app.clone();
            let generation = job.generation;
            match tokio::task::spawn_blocking(move || {
                task_storage.read_usage_category(generation, &task_app)
            })
            .await
            {
                Ok(Ok((manual, cached))) => {
                    if !cache.job_is_current(&job) {
                        continue;
                    }
                    if let Some(category) = manual {
                        cache.finish_manual(&job, &category);
                        continue;
                    }
                    if let Some(record) = &cached {
                        cache.publish(
                            generation,
                            &job.app,
                            record.category.clone(),
                            record.checked_ms,
                        );
                    }
                    job.cached = cached;
                    job.lookup_done = true;
                }
                _ => {
                    cache.defer(job, Duration::from_secs(5));
                    continue;
                }
            }
        }
        if !cache.job_is_current(&job) {
            continue;
        }
        if admission_generation != job.generation {
            admission_generation = job.generation;
            admission_retry_at = Instant::now();
        }
        let delay = admission_retry_at.saturating_duration_since(Instant::now());
        if !delay.is_zero() {
            cache.defer(job, delay);
            continue;
        }
        if gate_reason(&app, &storage, job.generation).is_some() {
            admission_retry_at = Instant::now() + Duration::from_secs(5);
            cache.defer_gated(job, Duration::from_secs(5));
            continue;
        }
        if job.prepared.is_none() {
            let task_app = job.app.clone();
            let task_storage = storage.clone();
            let task_handle = app.clone();
            let task_cached = job.cached.clone();
            let generation = job.generation;
            match tokio::task::spawn_blocking(move || {
                if gate_reason(&task_handle, &task_storage, generation).is_some() {
                    return Preparation::Deferred;
                }
                let fingerprint = metadata::fingerprint(&task_app.process_path);
                if task_cached.as_ref().is_some_and(|record| {
                    cache_matches(record, &fingerprint, chrono::Utc::now().timestamp_millis())
                }) {
                    return Preparation::Cached;
                }
                metadata::prepare(&task_app, fingerprint, || {
                    gate_reason(&task_handle, &task_storage, generation).is_none()
                })
                .map_or(Preparation::Deferred, Preparation::Prepared)
            })
            .await
            {
                Ok(Preparation::Prepared(prepared)) => job.prepared = Some(prepared),
                Ok(Preparation::Cached) => {
                    let record = job.cached.as_ref().unwrap();
                    cache.finish(&job, record.category.clone(), record.checked_ms);
                    continue;
                }
                Ok(Preparation::Deferred) => {
                    admission_retry_at = Instant::now() + Duration::from_secs(5);
                    cache.defer_gated(job, Duration::from_secs(5));
                    continue;
                }
                Err(_) => {
                    cache.defer(job, Duration::from_secs(60));
                    continue;
                }
            }
        }
        let prepared = job.prepared.as_ref().unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let category = if let Some(category) = prepared.category.as_ref() {
            Some(category.clone())
        } else {
            if !crate::registry_config::get_bool("classification_enabled").unwrap_or(true) {
                cache.defer(job, Duration::from_secs(300));
                continue;
            }
            let (lease, result) =
                infer_application(&app, &storage, &job, prepared.input(&job.app)).await;
            match result {
                Ok(result) => confident_category(&result),
                Err(error) => {
                    let delay = inference_retry_delay(&error, lease.reason().is_some());
                    tracing::debug!("[USAGE] Application classification deferred: {}", error);
                    cache.defer(job, delay);
                    continue;
                }
            }
        };
        if gate_reason(&app, &storage, job.generation).is_some() || !cache.job_is_current(&job) {
            cache.defer(job, Duration::from_secs(5));
            continue;
        }
        let record = StoredCategory {
            category,
            fingerprint: prepared.fingerprint.clone(),
            checked_ms: now,
        };
        let task_storage = storage.clone();
        let task_app = job.app.clone();
        let task_record = record.clone();
        let generation = job.generation;
        match tokio::task::spawn_blocking(move || {
            task_storage.save_usage_inferred_category(generation, &task_app, &task_record)
        })
        .await
        {
            Ok(Ok(())) => {
                cache.finish(&job, record.category, now);
                let _ = app.emit("usage-categories-changed", ());
            }
            _ => cache.defer(job, Duration::from_secs(5)),
        }
    }
}

async fn infer_application(
    app: &AppHandle,
    storage: &StorageState,
    job: &Job,
    input: scoring::Input,
) -> (
    Arc<crate::background_policy::ExecutionLease>,
    Result<scoring::Classification, String>,
) {
    let lease = crate::background_policy::ExecutionLease::new(
        "usage_classification",
        crate::background_policy::ExecutionProfile::Idle,
        100,
    );
    let watch = async {
        loop {
            if let Some(reason) = gate_reason(app, storage, job.generation) {
                lease.revoke(reason);
                return;
            }
            if !storage.usage_categories.job_is_current(job) {
                lease.revoke("cancelled");
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let inference = crate::background_policy::EXECUTION.scope(
        lease.clone(),
        crate::classification::classify_application(app, input, job.generation),
    );
    let result = {
        tokio::pin!(watch, inference);
        tokio::select! {
            result = &mut inference => result,
            _ = &mut watch => inference.await,
        }
    };
    (lease, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> Arc<UsageAppIdentity> {
        Arc::new(UsageAppIdentity {
            process_name: "unknown.exe".into(),
            name: "Unknown".into(),
            process_path: "D:/Apps/unknown.exe".into(),
            icon: None,
        })
    }
    #[test]
    fn fullscreen_battery_and_input_close_application_inference_admission() {
        assert_eq!(
            admission_reason(true, true, Some(3_600_000), false),
            Some("waiting_for_fullscreen")
        );
        assert_eq!(
            admission_reason(false, false, Some(3_600_000), false),
            Some("waiting_for_ac_power")
        );
        assert_eq!(
            admission_reason(true, false, Some(60_000), true),
            Some("foreground_request")
        );
        assert_eq!(
            admission_reason(true, false, Some(59_999), false),
            Some("waiting_for_idle")
        );
        assert_eq!(
            admission_reason(true, false, None, false),
            Some("waiting_for_idle")
        );
        assert_eq!(admission_reason(true, false, Some(60_000), false), None);
    }
    #[test]
    fn repeated_observations_deduplicate_jobs_and_negative_results_do_not_repeat_inference() {
        let cache = CategoryCache::default();
        let app = app();
        for _ in 0..120 {
            assert_eq!(cache.category(1, app.clone(), Some(7)), UNKNOWN);
        }
        let job = cache.next_job(1).unwrap();
        assert!(cache.next_job(1).is_none());
        cache.finish(&job, None, chrono::Utc::now().timestamp_millis());
        for _ in 0..120 {
            cache.category(1, app.clone(), Some(7));
        }
        assert!(cache.next_job(1).is_none());
        cache.manual_changed(1, &app.process_name, Some("办公文档"));
        cache.manual_changed(1, &app.process_name, None);
        cache.category(1, app, Some(7));
        assert!(cache.next_job(1).is_some());
    }
    #[test]
    fn stale_results_cannot_repopulate_a_new_archives_cache() {
        let cache = CategoryCache::default();
        let app = app();
        cache.category(1, app.clone(), None);
        let old = cache.next_job(1).unwrap();
        cache.category(2, app.clone(), None);
        cache.publish(2, &app, Some("游戏".into()), 200);
        cache.publish(1, &app, Some("办公文档".into()), 300);
        cache.finish(&old, Some("办公文档".into()), 300);
        assert_eq!(cache.category(2, app, None), "游戏");
    }
    #[test]
    fn ambiguous_semantic_scores_stay_unassigned() {
        let result = |score, runner_up| scoring::Classification {
            category: "游戏".into(),
            score,
            debug: serde_json::json!({"top_scores":[{"score":score},{"score":runner_up}]}),
        };
        assert!(confident_category(&result(0.62, 0.60)).is_none());
        assert!(confident_category(&result(0.59, 0.20)).is_none());
        assert_eq!(
            confident_category(&result(0.82, 0.50)).as_deref(),
            Some("游戏")
        );
    }
    #[test]
    fn file_changes_invalidate_cached_results_and_negative_results_expire() {
        let mut record = StoredCategory {
            category: Some("游戏".into()),
            fingerprint: vec![1],
            checked_ms: 10,
        };
        assert!(cache_matches(&record, &[1], NEGATIVE_CACHE_MS + 10));
        assert!(!cache_matches(&record, &[2], 11));
        record.category = None;
        assert!(cache_matches(&record, &[1], NEGATIVE_CACHE_MS + 9));
        assert!(!cache_matches(&record, &[1], NEGATIVE_CACHE_MS + 10));
    }
    #[tokio::test(start_paused = true)]
    async fn deferred_jobs_sleep_until_the_earliest_retry_and_new_work_wakes_them() {
        let cache = CategoryCache::default();
        cache.category(1, app(), Some(7));
        let job = cache.next_job(1).unwrap();
        cache.defer(job, Duration::from_secs(300));
        // Consume the enqueue notification, as the worker does on its first wait.
        cache.wake.notified().await;
        assert!(
            tokio::time::timeout(Duration::from_secs(299), cache.wait_for_work(1))
                .await
                .is_err()
        );
        cache.wait_for_work(1).await;
        let job = cache.next_job(1).unwrap();
        cache.defer(job, Duration::from_secs(300));
        let second = Arc::new(UsageAppIdentity {
            process_name: "another-unknown.exe".into(),
            ..app().as_ref().clone()
        });
        {
            let waiting = cache.wait_for_work(1);
            tokio::pin!(waiting);
            assert!(futures::poll!(waiting.as_mut()).is_pending());
            cache.category(1, second, None);
            waiting.await;
        }
        let second_job = cache.next_job(1).unwrap();
        cache.defer(second_job, Duration::from_secs(5));
        assert!(
            tokio::time::timeout(Duration::from_secs(4), cache.wait_for_work(1))
                .await
                .is_err()
        );
        cache.wait_for_work(1).await;
        assert!(cache.next_job(1).is_some());
        assert!(cache.next_job(1).is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn admission_backoff_does_not_delay_initial_manual_and_cache_lookups() {
        let cache = CategoryCache::default();
        for index in 0..3 {
            cache.category(
                1,
                Arc::new(UsageAppIdentity {
                    process_name: format!("unknown-{index}.exe"),
                    ..app().as_ref().clone()
                }),
                None,
            );
        }
        let mut first = cache.next_job(1).unwrap();
        let mut second = cache.next_job(1).unwrap();
        first.lookup_done = true;
        second.lookup_done = true;
        cache.defer(second, Duration::ZERO);
        cache.defer_gated(first, Duration::from_secs(5));
        let initial_lookup = cache.next_job(1).unwrap();
        assert!(!initial_lookup.lookup_done);
        assert!(cache.next_job(1).is_none());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(cache.next_job(1).unwrap().lookup_done);
        assert!(cache.next_job(1).unwrap().lookup_done);
    }
    #[test]
    fn lock_contention_retries_promptly_and_real_errors_keep_the_long_backoff() {
        assert_eq!(
            inference_retry_delay("classification_busy", false),
            Duration::from_secs(5)
        );
        assert_eq!(
            inference_retry_delay("waiting_for_idle", true),
            Duration::from_secs(5)
        );
        assert_eq!(
            inference_retry_delay("model unavailable", false),
            Duration::from_secs(300)
        );
    }
    #[test]
    fn manual_edits_update_snapshots_and_cancel_results_even_after_the_override_is_reset() {
        let cache = CategoryCache::default();
        let app = app();
        cache.category(1, app.clone(), Some(7));
        let old_job = cache.next_job(1).unwrap();
        cache.publish(1, &app, Some("游戏".into()), 10);
        cache.manual_changed(1, &app.process_name, Some("办公文档"));
        assert_eq!(cache.category(1, app.clone(), Some(7)), "办公文档");
        cache.manual_changed(1, &app.process_name, Some("设计创作"));
        assert_eq!(cache.category(1, app.clone(), Some(7)), "设计创作");
        cache.manual_changed(1, &app.process_name, None);
        assert_eq!(cache.category(1, app.clone(), Some(7)), "游戏");
        let new_job = cache.next_job(1).unwrap();
        cache.finish(&old_job, Some("影音娱乐".into()), 20);
        assert!(cache.job_is_current(&new_job));
        assert_eq!(cache.category(1, app.clone(), Some(7)), "游戏");
        cache.finish(&new_job, Some("编程开发".into()), 30);
        assert_eq!(cache.category(1, app, Some(7)), "编程开发");
    }
    #[test]
    fn history_hydration_cannot_restore_a_rule_read_before_a_manual_edit() {
        let cache = CategoryCache::default();
        let app = app();
        cache.category(1, app.clone(), Some(7));
        let revision = cache.manual_revision(1);
        cache.manual_changed(1, &app.process_name, Some("办公文档"));
        cache.history_category(1, app.clone(), None, Some(&(None, 10)), revision, 11);
        assert_eq!(cache.category(1, app, Some(7)), "办公文档");
    }
    #[test]
    fn hot_manual_categories_survive_rule_eviction_and_automatic_publication() {
        let cache = CategoryCache::default();
        let app = app();
        cache.category(1, app.clone(), Some(7));
        cache.manual_changed(1, &app.process_name, Some("办公文档"));
        for index in 0..MAX_APPS {
            cache.manual_changed(1, &format!("other-{index}.exe"), Some("游戏"));
        }
        cache.publish(1, &app, Some("设计创作".into()), 20);
        assert_eq!(cache.category(1, app.clone(), Some(8)), "办公文档");
        assert!(cache.next_job(1).is_none());
        cache.manual_changed(1, &app.process_name, None);
        assert_eq!(cache.category(1, app, Some(8)), "设计创作");
        assert!(cache.next_job(1).is_some());
    }
    #[test]
    fn late_manual_lookups_cannot_replace_edits_or_reapply_a_reset_rule() {
        let cache = CategoryCache::default();
        let app = app();
        cache.category(1, app.clone(), Some(7));
        let old_job = cache.next_job(1).unwrap();
        cache.manual_changed(1, &app.process_name, Some("设计创作"));
        cache.finish_manual(&old_job, "办公文档");
        assert_eq!(cache.category(1, app.clone(), Some(7)), "设计创作");
        cache.manual_changed(1, &app.process_name, None);
        cache.finish_manual(&old_job, "办公文档");
        assert_eq!(cache.category(1, app.clone(), Some(7)), UNKNOWN);
        let new_job = cache.next_job(1).unwrap();
        cache.finish_manual(&new_job, "办公文档");
        assert_eq!(cache.category(1, app, Some(7)), "办公文档");
        assert!(!cache.job_is_current(&new_job));
        assert!(cache.next_job(1).is_none());
    }
    #[test]
    fn evicting_a_manual_rule_does_not_prevent_an_explicit_reset_of_its_hot_entry() {
        let cache = CategoryCache::default();
        let app = Arc::new(UsageAppIdentity {
            process_name: "code.exe".into(),
            name: "Visual Studio Code".into(),
            process_path: "D:/Apps/code.exe".into(),
            icon: None,
        });
        cache.category(1, app.clone(), Some(7));
        cache.manual_changed(1, &app.process_name, Some("办公文档"));
        for index in 0..MAX_APPS {
            cache.manual_changed(1, &format!("other-{index}.exe"), Some("游戏"));
        }
        assert_eq!(cache.category(1, app.clone(), Some(7)), "办公文档");
        cache.manual_changed(1, &app.process_name, None);
        assert_eq!(cache.category(1, app, Some(7)), "编程开发");
    }
}
