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
    collections::{HashMap, HashSet, VecDeque},
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

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
    manual
        .or_else(|| {
            (stored.is_empty() || stored == UNKNOWN)
                .then_some(automatic)
                .flatten()
        })
        .unwrap_or(stored)
        .to_owned()
}

struct Entry {
    app: Arc<UsageAppIdentity>,
    category: String,
    builtin: bool,
    queued: bool,
    complete: bool,
    checked_ms: i64,
    pid: Option<u32>,
}
struct Job {
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
    manual: HashSet<AppKey>,
    manual_order: VecDeque<AppKey>,
    pending: VecDeque<Job>,
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
        state.entries.insert(
            key,
            Entry {
                app,
                category: category.unwrap_or(UNKNOWN).into(),
                builtin: category.is_some(),
                queued: false,
                complete: category.is_some(),
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
        Self::insert(&mut state, key, app.clone());
        let manual = state.manual.contains(&override_key(&app.process_name));
        let has_capacity = state.pending.len() < MAX_PENDING;
        let entry = state.entries.get_mut(&key).unwrap();
        if pid.is_some() && entry.pid.is_some() && entry.pid != pid && !entry.builtin {
            entry.complete = false; // Verify executable changes off the recorder thread.
        }
        if pid.is_some() {
            entry.pid = pid;
        }
        if entry.category == UNKNOWN
            && entry.complete
            && chrono::Utc::now().timestamp_millis() - entry.checked_ms >= NEGATIVE_CACHE_MS
        {
            entry.complete = false;
        }
        let enqueue = !manual && !entry.builtin && !entry.complete && !entry.queued && has_capacity;
        if enqueue {
            entry.queued = true;
        }
        let category = entry.category.clone();
        if enqueue {
            state.pending.push_back(Job {
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
        category: String,
        checked_ms: i64,
    ) {
        let key = app_key(app);
        let mut state = self.state(generation);
        if state.generation != generation {
            return;
        }
        Self::insert(&mut state, key, app.clone());
        let entry = state.entries.get_mut(&key).unwrap();
        if !entry.builtin && checked_ms >= entry.checked_ms {
            entry.category = category;
            entry.checked_ms = checked_ms;
        }
    }
    pub(crate) fn manual_changed(&self, generation: u64, id: &str, manual: bool) {
        let mut state = self.state(generation);
        if state.generation != generation {
            return;
        }
        let key = override_key(id);
        let changed = if manual {
            state.manual.insert(key)
        } else {
            state.manual.remove(&key)
        };
        if manual && changed {
            state.manual_order.push_back(key);
            if state.manual_order.len() > MAX_APPS {
                if let Some(old) = state.manual_order.pop_front() {
                    state.manual.remove(&old);
                }
            }
        } else if !manual {
            state.manual_order.retain(|old| *old != key);
        }
        if changed {
            for entry in state
                .entries
                .values_mut()
                .filter(|entry| override_key(&entry.app.process_name) == key)
            {
                entry.queued = false;
                if !entry.builtin {
                    entry.complete = manual;
                }
            }
            state
                .pending
                .retain(|job| override_key(&job.app.process_name) != key);
        }
        if !manual && changed {
            self.wake.notify_one();
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
    fn defer(&self, mut job: Job, delay: Duration) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != job.generation
            || !state.entries.get(&job.key).is_some_and(|e| e.queued)
        {
            return;
        }
        job.retry_at = Instant::now() + delay;
        if state.pending.len() < MAX_PENDING {
            state.pending.push_back(job);
        } else if let Some(entry) = state.entries.get_mut(&job.key) {
            entry.queued = false;
        }
    }
    fn finish(&self, job: &Job, category: Option<String>, checked_ms: i64) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation != job.generation {
            return;
        }
        if let Some(entry) = state.entries.get_mut(&job.key) {
            entry.queued = false;
            entry.complete = true;
            entry.checked_ms = checked_ms;
            if !entry.builtin {
                entry.category = category.unwrap_or_else(|| UNKNOWN.into());
            }
        }
    }
    fn has_pending(&self) -> bool {
        !self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .is_empty()
    }
    fn is_manual(&self, job: &Job) -> bool {
        let state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.generation != job.generation
            || state.manual.contains(&override_key(&job.app.process_name))
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
        && (record.category.is_some()
            || now_ms.saturating_sub(record.checked_ms) < NEGATIVE_CACHE_MS)
}

pub(super) async fn run(app: AppHandle, runtime: Arc<UsageRuntime>) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    while !runtime.shutdown.load(Ordering::Acquire) {
        let cache = &storage.usage_categories;
        if !storage.is_initialized()
            || !crate::storage_startup::is_ready(&app)
            || !storage.is_silent_read_authorized()
            || crate::maintenance::is_active()
            || storage.is_database_maintenance_pending()
        {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let notified = cache.wake.notified();
        let Some(mut job) = cache.next_job(storage.db_generation()) else {
            if cache.has_pending() {
                let _ = tokio::time::timeout(Duration::from_secs(2), notified).await;
            } else {
                notified.await;
            }
            continue;
        };
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
                    cache.manual_changed(generation, &job.app.process_name, manual);
                    if manual {
                        cache.finish(&job, None, chrono::Utc::now().timestamp_millis());
                        continue;
                    }
                    if let Some(category) = cached.as_ref().and_then(|c| c.category.clone()) {
                        cache.publish(
                            generation,
                            &job.app,
                            category,
                            cached.as_ref().unwrap().checked_ms,
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
        if cache.is_manual(&job) {
            continue;
        }
        if gate_reason(&app, &storage, job.generation).is_some() {
            cache.defer(job, Duration::from_secs(5));
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
                    cache.defer(job, Duration::from_secs(5));
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
                    let delay = if lease.reason().is_some() { 5 } else { 300 };
                    tracing::debug!("[USAGE] Application classification deferred: {}", error);
                    cache.defer(job, Duration::from_secs(delay));
                    continue;
                }
            }
        };
        if gate_reason(&app, &storage, job.generation).is_some() || cache.is_manual(&job) {
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
            if storage.usage_categories.is_manual(job) {
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
        cache.manual_changed(1, &app.process_name, true);
        cache.manual_changed(1, &app.process_name, false);
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
        cache.publish(2, &app, "游戏".into(), 200);
        cache.publish(1, &app, "办公文档".into(), 300);
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
}
