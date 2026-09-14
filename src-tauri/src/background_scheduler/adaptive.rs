use super::*;
use crate::background_policy::{self as policy, *};
use crate::background_resources::ResourceSampler;
use std::collections::BTreeMap;

struct ActiveExecution {
    lease: Arc<ExecutionLease>,
    staged: bool,
    db_generation: u64,
    manual_generation: u64,
    additional_peak: u64,
    disk: bool,
}

pub(super) struct AdaptiveRuntime {
    pub resources: Mutex<ResourcePolicy>,
    pub book: Mutex<Option<PerformanceBook>>,
    active: Mutex<Option<ActiveExecution>>,
    pub pauses: Mutex<PauseStatistics>,
    pub pending: AtomicBool,
    sampler_generation: AtomicU64,
    dirty: AtomicBool,
    waiting_for_idle: Mutex<std::collections::HashSet<String>>,
    clock: Instant,
}

impl Default for AdaptiveRuntime {
    fn default() -> Self {
        Self {
            resources: Mutex::new(ResourcePolicy::default()),
            book: Mutex::new(None),
            active: Mutex::new(None),
            pauses: Mutex::new(PauseStatistics::default()),
            pending: AtomicBool::new(false),
            sampler_generation: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            waiting_for_idle: Mutex::new(std::collections::HashSet::new()),
            clock: Instant::now(),
        }
    }
}

pub(super) fn configured_mode() -> SchedulingMode {
    crate::registry_config::get_string("background_scheduling_mode")
        .and_then(|value| SchedulingMode::parse(&value).ok())
        .unwrap_or_default()
}

fn signals(app: &AppHandle, staged: bool) -> AdmissionSignals {
    let idle = app.state::<Arc<IdleState>>();
    let storage = app.state::<Arc<StorageState>>();
    AdmissionSignals {
        enabled: storage.background_processing_enabled(),
        authorized: storage.is_background_authorized()
            || (staged && storage.background_processing_enabled()),
        ac_connected: idle.ac_connected.load(Ordering::Relaxed),
        protected_session: idle.fullscreen_exclusive.load(Ordering::Relaxed)
            || app
                .try_state::<Arc<crate::capture::CaptureState>>()
                .is_some_and(|s| s.game_mode_capture_paused.load(Ordering::Relaxed))
            || app
                .try_state::<crate::monitor::MonitorState>()
                .is_some_and(|s| s.is_dml_suppressed()),
        maintenance: crate::maintenance::is_active(),
        foreground: app
            .state::<Arc<SemanticRuntimeState>>()
            .foreground_waiting(),
        idle_secs: idle.idle_secs.load(Ordering::Relaxed),
    }
}

impl AdaptiveRuntime {
    pub(super) fn cancel_active(&self, reason: &'static str) {
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            active.lease.revoke(reason);
        }
    }
    pub(super) fn now(&self) -> u64 {
        self.clock.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    pub(super) fn start(runtime: Arc<SchedulerRuntime>, app: AppHandle) {
        let generation = runtime
            .adaptive
            .sampler_generation
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        // PDH and sysinfo are synchronous. This dedicated low-frequency thread
        // never takes the UI/semantic execution slot and never stores user data.
        let _ = std::thread::Builder::new()
            .name("carbonpaper-background-resources".into())
            .spawn(move || {
                let mut sampler = ResourceSampler::new();
                let environment = sampler.environment();
                let storage = app.state::<Arc<StorageState>>().inner().clone();
                let json = storage.background_performance_history().ok().flatten();
                *runtime
                    .adaptive
                    .book
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(PerformanceBook::restore(environment, json.as_deref()));
                let mut sampled_at = Instant::now();
                let mut flushed_at = Instant::now();
                while !runtime.stop.load(Ordering::SeqCst)
                    && runtime.adaptive.sampler_generation.load(Ordering::SeqCst) == generation
                {
                    if sampled_at.elapsed() >= Duration::from_secs(1) {
                        let sample = sampler.sample(runtime.adaptive.now());
                        runtime
                            .adaptive
                            .resources
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .observe(sample);
                        sampled_at = Instant::now();
                    }
                    runtime.adaptive.poll_active(&app, &runtime);
                    if flushed_at.elapsed() >= Duration::from_secs(30) {
                        runtime.adaptive.flush(&storage);
                        flushed_at = Instant::now();
                    }
                    std::thread::sleep(if runtime.adaptive.pending.load(Ordering::Relaxed) {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_secs(1)
                    });
                }
                runtime.adaptive.flush(&storage);
            });
    }

    fn flush(&self, storage: &StorageState) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        let json = self
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|b| serde_json::to_string(b).ok());
        if json.is_some_and(|json| storage.save_background_performance_history(&json).is_err()) {
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn admission(
        &self,
        app: &AppHandle,
        kind: BackgroundTaskKind,
        staged: bool,
    ) -> Result<ExecutionProfile, &'static str> {
        let activity = signals(app, staged);
        if activity.idle_secs >= SHORT_IDLE_SECS {
            self.waiting_for_idle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(kind.as_str());
        } else if self
            .waiting_for_idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(kind.as_str())
        {
            return Err(activity.hard_gate().unwrap_or("waiting_for_evaluation"));
        }
        let qualified = self
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|b| b.task_eligible(kind.as_str()));
        choose_profile(
            activity,
            configured_mode(),
            kind == BackgroundTaskKind::PythonClustering,
            qualified,
            &self.resources.lock().unwrap_or_else(|e| e.into_inner()),
            self.now(),
        )
    }

    pub(super) fn begin(
        &self,
        app: &AppHandle,
        kind: BackgroundTaskKind,
        staged: bool,
        manual_generation: u64,
    ) -> Result<Arc<ExecutionLease>, &'static str> {
        let profile = self.admission(app, kind, staged)?;
        let duty = if profile == ExecutionProfile::Background {
            self.resources
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .duty_percent(self.now())
        } else {
            self.resources
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reset_duty();
            100
        };
        let lease = ExecutionLease::new(kind.as_str(), profile, duty);
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = Some(ActiveExecution {
            lease: lease.clone(),
            staged,
            db_generation: app.state::<Arc<StorageState>>().db_generation(),
            manual_generation,
            additional_peak: 0,
            disk: false,
        });
        Ok(lease)
    }

    fn poll_active(&self, app: &AppHandle, runtime: &SchedulerRuntime) {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        let Some(active) = active.as_ref() else {
            return;
        };
        let signals = signals(app, active.staged);
        let reason = signals.hard_gate().or_else(|| {
            if app.state::<Arc<StorageState>>().db_generation() != active.db_generation {
                Some("database_changed")
            } else if runtime.manual_request_generation.load(Ordering::SeqCst)
                != active.manual_generation
            {
                Some("manual_request_pending")
            } else if app
                .state::<Arc<SemanticRuntimeState>>()
                .external_background_waiting()
            {
                Some("external_background_request")
            } else if active.lease.profile == ExecutionProfile::Idle
                && signals.idle_secs
                    < if active.lease.task == TASK_PYTHON_CLUSTERING {
                        LEGACY_CLUSTER_IDLE_SECS
                    } else {
                        SHORT_IDLE_SECS
                    }
            {
                Some("input_resumed")
            } else if active.lease.profile == ExecutionProfile::Background {
                if configured_mode() == SchedulingMode::IdleOnly {
                    Some("waiting_for_idle")
                } else {
                    self.resources
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .retain(self.now(), active.additional_peak, active.disk)
                }
            } else {
                None
            }
        });
        if let Some(reason) = reason {
            active.lease.revoke(reason);
        }
    }

    pub(super) fn finish(&self, lease: &ExecutionLease) {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(reason) = lease.reason() {
            if matches!(
                reason,
                "waiting_for_memory" | "waiting_for_disk" | "resource_pressure"
            ) {
                self.resources
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .defer_after_pressure(self.now());
            }
            if matches!(
                reason,
                "waiting_for_evaluation"
                    | "waiting_for_resident_model"
                    | "cost_overrun"
                    | "waiting_for_idle"
            ) {
                self.waiting_for_idle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(lease.task.clone());
            }
            self.resources
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reset_duty();
            let mut pauses = self.pauses.lock().unwrap_or_else(|e| e.into_inner());
            pauses.total += 1;
            pauses.last_yield_ms = lease.yield_latency_ms();
            *pauses.reasons.entry(reason.to_string()).or_default() += 1;
        }
    }

    pub(super) fn profile(&self, manual: bool) -> ExecutionProfile {
        if manual {
            ExecutionProfile::Manual
        } else {
            self.active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map_or(ExecutionProfile::Paused, |active| active.lease.profile)
        }
    }
}

impl BackgroundSchedulerState {
    pub(crate) fn observe_activity(&self, app: &AppHandle) {
        self.runtime.adaptive.poll_active(app, &self.runtime);
    }
    pub(crate) fn unit(
        self: &Arc<Self>,
        operation: &str,
        model: &str,
        size: u64,
        parameters: &str,
    ) -> Result<Option<UnitMeasurement>, String> {
        let Some(lease) = policy::current_execution() else {
            return Ok(None);
        };
        let key = CostKey::new(&lease.task, operation, model, size, parameters);
        self.admit_operation(&key, false, false)?;
        if lease.profile == ExecutionProfile::Background {
            lease.limit_unit(Some(Duration::from_secs(1)));
        }
        // Host preparation is bounded to one record. Its CPU measurement is
        // conservative when unrelated host work overlaps this interval.
        // SAFETY: the pseudo-handle is borrowed and process_usage only reads it.
        let usage = unsafe {
            crate::background_resources::process_usage(
                windows::Win32::System::Threading::GetCurrentProcess(),
            )
        };
        Ok(Some(UnitMeasurement {
            scheduler: self.clone(),
            key,
            lease,
            start: Instant::now(),
            before: usage,
        }))
    }
    pub(crate) fn has_pending_work(&self) -> bool {
        self.runtime.adaptive.pending.load(Ordering::Relaxed)
    }

    pub(crate) fn operation_key(
        &self,
        operation: &str,
        model: &str,
        size: u64,
        parameters: &str,
    ) -> Option<CostKey> {
        policy::current_execution()
            .map(|lease| CostKey::new(&lease.task, operation, model, size, parameters))
    }

    pub(crate) fn admit_operation(
        &self,
        key: &CostKey,
        cold: bool,
        disk: bool,
    ) -> Result<(), String> {
        let Some(lease) = policy::current_execution() else {
            return Ok(());
        };
        lease.check()?;
        let book = self
            .runtime
            .adaptive
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let peak = book.as_ref().map_or(0, |book| {
            book.additional_peak(key).saturating_add(if cold {
                book.additional_peak(&key.cold())
            } else {
                0
            })
        });
        if lease.profile == ExecutionProfile::Background {
            let reason = if book.as_ref().is_none_or(|book| !book.eligible(key)) {
                Some("waiting_for_evaluation")
            } else if cold && book.as_ref().is_none_or(|book| !book.eligible(&key.cold())) {
                Some("waiting_for_resident_model")
            } else {
                self.runtime
                    .adaptive
                    .resources
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .admit(self.runtime.adaptive.now(), peak, disk || cold)
            };
            if let Some(reason) = reason {
                lease.revoke(reason);
                return lease.check();
            }
            if cold {
                lease.allow_cold_load();
            }
        }
        drop(book);
        if let Some(active) = self
            .runtime
            .adaptive
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            active.additional_peak = peak;
            active.disk = disk || cold;
        }
        Ok(())
    }

    pub(crate) fn record_cost(&self, key: CostKey, sample: CostSample, profile: ExecutionProfile) {
        if let Some(book) = self
            .runtime
            .adaptive
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            book.record(key, sample, profile);
            self.runtime.adaptive.dirty.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn revoke_cost(&self, key: &CostKey) {
        if let Some(book) = self
            .runtime
            .adaptive
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            book.revoke(key);
            self.runtime.adaptive.dirty.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn qualifications(&self) -> Vec<Qualification> {
        self.runtime
            .adaptive
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(Vec::new, PerformanceBook::qualifications)
    }

    pub(crate) fn qualification_summary(&self) -> BTreeMap<String, bool> {
        let book = self
            .runtime
            .adaptive
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        [
            TASK_SEMANTIC_INDEX,
            TASK_CLIP_INDEX,
            TASK_SMART_CLUSTER,
            TASK_PYTHON_CLUSTERING,
            TASK_VECTOR_SYNC,
            TASK_ANN_BUILD,
        ]
        .into_iter()
        .map(|task| {
            (
                task.into(),
                book.as_ref().is_some_and(|book| book.task_eligible(task)),
            )
        })
        .collect()
    }
}

pub(crate) struct UnitMeasurement {
    scheduler: Arc<BackgroundSchedulerState>,
    key: CostKey,
    lease: Arc<ExecutionLease>,
    start: Instant,
    before: Option<crate::background_resources::ProcessUsage>,
}

impl UnitMeasurement {
    pub(crate) fn complete(self) {
        if self.lease.check().is_err() {
            return;
        }
        // SAFETY: the pseudo-handle is borrowed and process_usage only reads it.
        let after = unsafe {
            crate::background_resources::process_usage(
                windows::Win32::System::Threading::GetCurrentProcess(),
            )
        };
        if let (Some(before), Some(after)) = (self.before, after) {
            self.scheduler.record_cost(
                self.key.clone(),
                CostSample {
                    elapsed_ms: self.start.elapsed().as_secs_f64() * 1000.0,
                    cpu_ms: (after.cpu_ms - before.cpu_ms).max(0.0),
                    peak_private_bytes: if after.peak_private_bytes > before.peak_private_bytes {
                        after.peak_private_bytes
                    } else {
                        after.private_bytes.max(before.private_bytes)
                    },
                    additional_peak_bytes: (if after.peak_private_bytes > before.peak_private_bytes
                    {
                        after.peak_private_bytes
                    } else {
                        after.private_bytes
                    })
                    .saturating_sub(before.private_bytes),
                    cpu_rate_percent: CPU_RATE_PERCENT,
                },
                self.lease.profile,
            );
        }
    }
}

impl Drop for UnitMeasurement {
    fn drop(&mut self) {
        if self.lease.reason() == Some("cost_overrun") {
            self.scheduler.revoke_cost(&self.key);
        }
        self.lease.limit_unit(None);
    }
}
