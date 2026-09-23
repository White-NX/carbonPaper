//! Local, task-specific admission and cost history for resumable automatic work.
//!
//! Clock values are supplied by the caller so hysteresis, missing telemetry and
//! changes of execution budget can be tested without a desktop or a model.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SHORT_IDLE_SECS: u64 = 60;
pub const CPU_RATE_PERCENT: u32 = 5;
pub const SAMPLE_LIMIT: usize = 64;
pub const MIN_SAMPLES: usize = 20;
pub const MIN_COLD_SAMPLES: usize = 3;
pub const UNIT_P95_MS: f64 = 500.0;
pub const COLD_P95_MS: f64 = 1000.0;
// User policy: available - (predicted additional peak * 1.15) >= 0.8 GiB.
pub const MEMORY_RESERVE_BYTES: u64 = (8 * 1024 * 1024 * 1024_u64).div_ceil(10);
pub const SMALL_ANN_ROWS: u64 = 20_000;
pub const SMALL_ANN_BYTES: u64 = 64 * 1024 * 1024;
pub const IO_CHUNK_BYTES: usize = 256 * 1024;
pub const IO_BYTES_PER_SECOND: u64 = 2 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingMode {
    #[default]
    Auto,
    IdleOnly,
}

impl SchedulingMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "idle_only" => Ok(Self::IdleOnly),
            _ => Err("background_scheduling_mode must be auto or idle_only".into()),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProfile {
    #[default]
    Paused,
    Idle,
    Background,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnvironment {
    pub logical_cores: usize,
    pub physical_memory_bytes: u64,
    pub runtime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CostKey {
    pub task: String,
    pub operation: String,
    pub model: String,
    /// The bucket is part of the identity; a small input never qualifies a large one.
    pub input_bucket: u64,
    pub parameters: String,
}

impl CostKey {
    pub fn new(task: &str, operation: &str, model: &str, size: u64, parameters: &str) -> Self {
        Self {
            task: task.into(),
            operation: operation.into(),
            model: model.into(),
            input_bucket: size.max(1).checked_next_power_of_two().unwrap_or(u64::MAX),
            parameters: parameters.into(),
        }
    }

    pub fn cold(&self) -> Self {
        Self {
            operation: "cold_load".into(),
            input_bucket: 1,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSample {
    /// Execution only: excludes queue time and voluntary duty-cycle / I/O waits.
    pub elapsed_ms: f64,
    pub cpu_ms: f64,
    pub peak_private_bytes: u64,
    pub additional_peak_bytes: u64,
    pub cpu_rate_percent: u32,
}

impl CostSample {
    fn valid(&self) -> bool {
        self.elapsed_ms.is_finite()
            && self.elapsed_ms > 0.0
            && self.cpu_ms.is_finite()
            && self.cpu_ms >= 0.0
            && self.peak_private_bytes > 0
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CostHistory {
    samples: VecDeque<CostSample>,
    pub revoked: bool,
    pub revocations: u64,
    /// Unrestricted manual results are diagnostics, never qualifying samples.
    pub last_unrestricted: Option<CostSample>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Qualification {
    pub key: CostKey,
    pub eligible: bool,
    pub samples: usize,
    pub p95_ms: Option<f64>,
    pub expected_additional_peak_bytes: u64,
    pub peak_private_bytes: u64,
    pub revoked: bool,
}

impl CostHistory {
    fn p95_ms(&self) -> Option<f64> {
        let mut values: Vec<f64> = self.samples.iter().map(|s| s.elapsed_ms).collect();
        values.sort_by(f64::total_cmp);
        values
            .get((values.len() * 95).div_ceil(100).saturating_sub(1))
            .copied()
    }

    fn eligible(&self, cold: bool) -> bool {
        !self.revoked
            && self.samples.len() >= if cold { MIN_COLD_SAMPLES } else { MIN_SAMPLES }
            && self
                .p95_ms()
                .is_some_and(|p95| p95 <= if cold { COLD_P95_MS } else { UNIT_P95_MS })
    }

    fn additional_peak(&self) -> u64 {
        self.samples
            .iter()
            .map(|s| s.additional_peak_bytes)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceBook {
    pub environment: ExecutionEnvironment,
    // A list keeps JSON keys ordinary strings and bounds the local metadata payload.
    histories: Vec<(CostKey, CostHistory)>,
}

impl PerformanceBook {
    pub fn new(environment: ExecutionEnvironment) -> Self {
        Self {
            environment,
            histories: Vec::new(),
        }
    }

    pub fn restore(environment: ExecutionEnvironment, json: Option<&str>) -> Self {
        json.and_then(|value| serde_json::from_str::<Self>(value).ok())
            .filter(|book| {
                book.environment == environment
                    && book.histories.len() <= 512
                    && book.histories.iter().all(|(_, history)| {
                        history.samples.len() <= SAMPLE_LIMIT
                            && history.samples.iter().all(|sample| {
                                sample.valid() && sample.cpu_rate_percent == CPU_RATE_PERCENT
                            })
                    })
            })
            .unwrap_or_else(|| Self::new(environment))
    }

    pub fn record(&mut self, key: CostKey, sample: CostSample, profile: ExecutionProfile) {
        if !sample.valid() {
            return;
        }
        if !self.histories.iter().any(|(k, _)| k == &key) {
            if self.histories.len() >= 512 {
                self.histories.remove(0);
            }
            self.histories.push((key.clone(), CostHistory::default()));
        }
        let history = &mut self
            .histories
            .iter_mut()
            .find(|(k, _)| k == &key)
            .unwrap()
            .1;
        if sample.cpu_rate_percent != CPU_RATE_PERCENT || profile == ExecutionProfile::Manual {
            history.last_unrestricted = Some(sample);
            return;
        }
        let cold = key.operation == "cold_load";
        let limit = if cold { COLD_P95_MS } else { UNIT_P95_MS };
        let prior = history.p95_ms().unwrap_or(limit);
        if profile == ExecutionProfile::Background && sample.elapsed_ms > limit.max(prior * 2.0) {
            history.samples.clear();
            history.revoked = true;
            history.revocations += 1;
            return;
        }
        // Only A can revalidate a revoked operation. No old samples survive revocation.
        if history.revoked && profile != ExecutionProfile::Idle {
            return;
        }
        history.samples.push_back(sample);
        while history.samples.len() > SAMPLE_LIMIT {
            history.samples.pop_front();
        }
        if profile == ExecutionProfile::Idle
            && history.samples.len() >= if cold { MIN_COLD_SAMPLES } else { MIN_SAMPLES }
        {
            history.revoked = false;
        }
    }

    pub fn revoke(&mut self, key: &CostKey) {
        if let Some((_, history)) = self.histories.iter_mut().find(|(k, _)| k == key) {
            history.samples.clear();
            history.revoked = true;
            history.revocations += 1;
        }
    }

    pub fn eligible(&self, key: &CostKey) -> bool {
        self.histories
            .iter()
            .find(|(k, _)| k == key)
            .is_some_and(|(_, h)| h.eligible(key.operation == "cold_load"))
    }

    pub fn task_eligible(&self, task: &str) -> bool {
        if task == "smart_cluster" {
            return self.histories.iter().any(|(key, h)| {
                key.task == task && key.operation == "scoring_unit" && h.eligible(false)
            });
        }
        if task == "ann_build" {
            return [
                "ann_insert",
                "ann_serialize",
                "ann_restore",
                "ann_validate",
                "ann_checksum",
                "ann_io",
            ]
            .iter()
            .all(|op| {
                self.histories
                    .iter()
                    .any(|(key, h)| key.task == task && key.operation == *op && h.eligible(false))
            });
        }
        self.histories
            .iter()
            .any(|(key, h)| key.task == task && key.operation != "cold_load" && h.eligible(false))
    }

    pub fn additional_peak(&self, key: &CostKey) -> u64 {
        self.histories
            .iter()
            .find(|(k, _)| k == key)
            .map_or(0, |(_, h)| h.additional_peak())
    }

    pub fn qualifications(&self) -> Vec<Qualification> {
        self.histories
            .iter()
            .map(|(key, h)| Qualification {
                key: key.clone(),
                eligible: h.eligible(key.operation == "cold_load"),
                samples: h.samples.len(),
                p95_ms: h.p95_ms(),
                expected_additional_peak_bytes: h.additional_peak(),
                peak_private_bytes: h
                    .samples
                    .iter()
                    .map(|s| s.peak_private_bytes)
                    .max()
                    .unwrap_or(0),
                revoked: h.revoked,
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ResourceSample {
    pub at_ms: u64,
    pub total_cpu_percent: Option<f32>,
    pub busiest_core_percent: Option<f32>,
    pub available_memory_bytes: Option<u64>,
    pub disk_latency_ms: Option<f64>,
}

pub fn memory_admits(available: u64, additional_peak: u64) -> bool {
    let extra = (u128::from(additional_peak) * 115).div_ceil(100);
    u128::from(available) >= extra + u128::from(MEMORY_RESERVE_BYTES)
}

#[derive(Debug, Default)]
pub struct ResourcePolicy {
    pub latest: ResourceSample,
    low_since: Option<u64>,
    disk_low_since: Option<u64>,
    stable_since: Option<u64>,
    cooldown_until: u64,
    busy_core_samples: u8,
    saturated_core_samples: u8,
    slow_disk_samples: u8,
}

impl ResourcePolicy {
    pub fn observe(&mut self, mut sample: ResourceSample) {
        let now = sample.at_ms;
        if now.saturating_sub(self.latest.at_ms) > 2000 {
            self.low_since = None;
            self.disk_low_since = None;
        }
        let cpu = sample
            .total_cpu_percent
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        let core = sample
            .busiest_core_percent
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        sample.total_cpu_percent = cpu;
        sample.busiest_core_percent = core;
        sample.disk_latency_ms = sample
            .disk_latency_ms
            .filter(|d| d.is_finite() && *d >= 0.0);
        self.busy_core_samples = if core.is_some_and(|c| c >= 85.0) {
            self.busy_core_samples.saturating_add(1)
        } else {
            0
        };
        self.saturated_core_samples = if core.is_some_and(|c| c >= 95.0) {
            self.saturated_core_samples.saturating_add(1)
        } else {
            0
        };
        self.slow_disk_samples = if sample.disk_latency_ms.is_some_and(|d| d >= 30.0) {
            self.slow_disk_samples.saturating_add(1)
        } else {
            0
        };
        if cpu.is_some_and(|c| c >= 50.0)
            || self.saturated_core_samples >= 2
            || self.slow_disk_samples >= 2
        {
            self.cooldown_until = now.saturating_add(15_000);
            self.low_since = None;
            self.stable_since = None;
            self.disk_low_since = None;
        }
        if now >= self.cooldown_until
            && cpu.is_some_and(|c| c < 30.0)
            && core.is_some()
            && self.busy_core_samples < 2
        {
            self.low_since.get_or_insert(now);
        } else {
            self.low_since = None;
        }
        if sample
            .disk_latency_ms
            .is_some_and(|d| d.is_finite() && (0.0..10.0).contains(&d))
            && now >= self.cooldown_until
        {
            self.disk_low_since.get_or_insert(now);
        } else {
            self.disk_low_since = None;
        }
        self.latest = sample;
    }

    pub fn admit(&self, now: u64, additional_peak: u64, disk: bool) -> Option<&'static str> {
        if now.saturating_sub(self.latest.at_ms) > 2000 {
            return Some("resource_metrics_unavailable");
        }
        if now < self.cooldown_until {
            return Some("resource_cooldown");
        }
        if self
            .low_since
            .is_none_or(|since| now.saturating_sub(since) < 10_000)
        {
            return Some("waiting_for_cpu");
        }
        self.memory_and_disk(now, additional_peak, disk, true)
    }

    pub fn retain(&self, now: u64, additional_peak: u64, disk: bool) -> Option<&'static str> {
        if now.saturating_sub(self.latest.at_ms) > 2000
            || self.latest.total_cpu_percent.is_none()
            || self.latest.busiest_core_percent.is_none()
        {
            return Some("resource_metrics_unavailable");
        }
        if now < self.cooldown_until
            || self.latest.total_cpu_percent.is_some_and(|c| c >= 50.0)
            || self.saturated_core_samples >= 2
        {
            return Some("resource_pressure");
        }
        self.memory_and_disk(now, additional_peak, disk, false)
    }

    fn memory_and_disk(
        &self,
        now: u64,
        peak: u64,
        disk: bool,
        admission: bool,
    ) -> Option<&'static str> {
        if !self
            .latest
            .available_memory_bytes
            .is_some_and(|bytes| memory_admits(bytes, peak))
        {
            return Some("waiting_for_memory");
        }
        if disk
            && (self.latest.disk_latency_ms.is_none()
                || self.slow_disk_samples >= 2
                || (admission
                    && self
                        .disk_low_since
                        .is_none_or(|since| now.saturating_sub(since) < 10_000)))
        {
            return Some("waiting_for_disk");
        }
        None
    }

    pub fn duty_percent(&mut self, now: u64) -> u32 {
        let since = *self.stable_since.get_or_insert(now);
        (20 + (now.saturating_sub(since) / 60_000) as u32 * 10).min(50)
    }
    pub fn reset_duty(&mut self) {
        self.stable_since = None;
    }
    pub fn defer_after_pressure(&mut self, now: u64) {
        self.cooldown_until = self.cooldown_until.max(now.saturating_add(15_000));
        self.low_since = None;
        self.disk_low_since = None;
        self.stable_since = None;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AdmissionSignals {
    pub enabled: bool,
    pub authorized: bool,
    pub ac_connected: bool,
    pub protected_session: bool,
    pub maintenance: bool,
    pub foreground: bool,
    pub idle_secs: u64,
}

impl AdmissionSignals {
    pub fn hard_gate(self) -> Option<&'static str> {
        if !self.enabled {
            Some("disabled")
        } else if !self.authorized {
            Some("waiting_for_unlock")
        } else if !self.ac_connected {
            Some("waiting_for_ac_power")
        } else if self.protected_session {
            Some("waiting_for_fullscreen")
        } else if self.maintenance {
            Some("maintenance")
        } else if self.foreground {
            Some("foreground_request")
        } else {
            None
        }
    }
}

pub fn choose_profile(
    signals: AdmissionSignals,
    mode: SchedulingMode,
    qualified: bool,
    resources: &ResourcePolicy,
    now: u64,
) -> Result<ExecutionProfile, &'static str> {
    if let Some(reason) = signals.hard_gate() {
        return Err(reason);
    }
    if signals.idle_secs >= SHORT_IDLE_SECS {
        return Ok(ExecutionProfile::Idle);
    }
    if mode == SchedulingMode::IdleOnly {
        return Err("waiting_for_idle");
    }
    if !qualified {
        return Err("waiting_for_evaluation");
    }
    if let Some(reason) = resources.admit(now, 0, false) {
        return Err(reason);
    }
    Ok(ExecutionProfile::Background)
}

/// The profile never upgrades/downgrades in place: A must relinquish its whole
/// lease on input before the scheduler can admit a new B lease with B budgets.
#[derive(Debug)]
pub struct ExecutionLease {
    pub task: String,
    pub profile: ExecutionProfile,
    pub duty_percent: u32,
    reason: Mutex<Option<&'static str>>,
    revoked_at: Mutex<Option<Instant>>,
    unit_deadline: Mutex<Option<Instant>>,
    cpu_micros: AtomicU64,
}

impl ExecutionLease {
    pub fn new(task: &str, profile: ExecutionProfile, duty_percent: u32) -> Arc<Self> {
        Arc::new(Self {
            task: task.into(),
            profile,
            duty_percent,
            reason: Mutex::new(None),
            revoked_at: Mutex::new(None),
            unit_deadline: Mutex::new(None),
            cpu_micros: AtomicU64::new(0),
        })
    }
    pub fn revoke(&self, reason: &'static str) {
        let mut current = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if current.is_none() {
            *current = Some(reason);
            *self.revoked_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
    }
    pub fn yield_latency_ms(&self) -> Option<f64> {
        self.revoked_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|at| at.elapsed().as_secs_f64() * 1000.0)
    }
    pub fn reason(&self) -> Option<&'static str> {
        *self.reason.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn check(&self) -> Result<(), String> {
        if self
            .unit_deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.revoke("cost_overrun");
        }
        self.reason()
            .map_or(Ok(()), |reason| Err(format!("background_paused: {reason}")))
    }
    pub fn limit_unit(&self, budget: Option<Duration>) {
        *self.unit_deadline.lock().unwrap_or_else(|e| e.into_inner()) =
            budget.map(|budget| Instant::now() + budget);
    }
    pub fn allow_cold_load(&self) {
        if let Some(deadline) = self
            .unit_deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            *deadline += Duration::from_secs(1);
        }
    }
    pub async fn rest(&self, execution: Duration) -> Result<(), String> {
        if self.profile == ExecutionProfile::Background {
            let duty_rest =
                execution.mul_f64((100 - self.duty_percent) as f64 / self.duty_percent as f64);
            // Short requests can burst within a Windows quota period. Account
            // actual CPU time too, so toggling a job limit at handoff cannot
            // turn many tiny requests into more than 5% of the machine.
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as f64;
            let cpu_window = Duration::from_secs_f64(
                self.cpu_micros.load(Ordering::Relaxed) as f64
                    / 1_000_000.0
                    / (cores * CPU_RATE_PERCENT as f64 / 100.0),
            );
            let rest = duty_rest.max(cpu_window.saturating_sub(execution));
            let end = Instant::now() + rest;
            while Instant::now() < end {
                self.check()?;
                tokio::time::sleep(
                    end.saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                )
                .await;
            }
        }
        self.check()
    }
    pub fn account_cpu(&self, cpu_ms: f64) {
        if cpu_ms.is_finite() && cpu_ms > 0.0 {
            self.cpu_micros
                .fetch_add((cpu_ms * 1000.0).ceil() as u64, Ordering::Relaxed);
        }
    }
}

tokio::task_local! { pub static EXECUTION: Arc<ExecutionLease>; }

pub fn current_execution() -> Option<Arc<ExecutionLease>> {
    EXECUTION.try_with(Arc::clone).ok()
}
pub fn check_current() -> Result<(), String> {
    current_execution().map_or(Ok(()), |lease| lease.check())
}
pub fn is_background() -> bool {
    current_execution().is_some_and(|lease| lease.profile == ExecutionProfile::Background)
}
pub fn is_pause(error: &str) -> bool {
    error.contains("background_paused:")
        || error.starts_with("cancelled:")
        || error.contains("embed failed: cancelled:")
        || matches!(
            error,
            "app_bound_disabled"
                | "app_bound_lease_expired"
                | "app_bound_task_retired"
                | "app_bound_dataset_mismatch"
                | "staging generation changed"
                | "staged source changed"
                | "Staged input changed before embedding commit"
        )
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct PauseStatistics {
    pub total: u64,
    pub reasons: BTreeMap<String, u64>,
    pub last_yield_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env() -> ExecutionEnvironment {
        ExecutionEnvironment {
            logical_cores: 4,
            physical_memory_bytes: 8 << 30,
            runtime: "test".into(),
        }
    }
    fn key() -> CostKey {
        CostKey::new("text", "encode", "v1", 128, "cpu:threads=1")
    }
    fn sample(ms: f64) -> CostSample {
        CostSample {
            elapsed_ms: ms,
            cpu_ms: ms / 2.0,
            peak_private_bytes: 300 << 20,
            additional_peak_bytes: 16 << 20,
            cpu_rate_percent: 5,
        }
    }
    fn signals(idle_secs: u64) -> AdmissionSignals {
        AdmissionSignals {
            enabled: true,
            authorized: true,
            ac_connected: true,
            protected_session: false,
            maintenance: false,
            foreground: false,
            idle_secs,
        }
    }
    fn sample_resources(at_ms: u64, cpu: f32, core: f32) -> ResourceSample {
        ResourceSample {
            at_ms,
            total_cpu_percent: Some(cpu),
            busiest_core_percent: Some(core),
            available_memory_bytes: Some(4 << 30),
            disk_latency_ms: Some(2.0),
        }
    }
    fn stable() -> ResourcePolicy {
        let mut p = ResourcePolicy::default();
        for i in 0..=10 {
            p.observe(sample_resources(i * 1000, 10.0, 25.0));
        }
        p
    }

    #[test]
    fn revoked_staged_leases_are_pauses_while_integrity_and_model_errors_are_failures() {
        use carbonpaper_app_bound::protocol::BrokerError;
        for error in [
            BrokerError::Disabled.code(),
            BrokerError::LeaseExpired.code(),
            BrokerError::Retired.code(),
            BrokerError::DatasetMismatch.code(),
            "staging generation changed",
            "Staged input changed before embedding commit",
        ] {
            assert!(is_pause(error), "{error}");
        }
        assert!(!is_pause("app_bound_access_denied"));
        assert!(!is_pause(BrokerError::Integrity.code()));
        assert!(!is_pause("model_mismatch: invalid tensor"));
    }

    #[test]
    fn revised_memory_rule_has_fifteen_percent_margin_and_fixed_point_eight_gib_floor() {
        assert!(memory_admits(MEMORY_RESERVE_BYTES, 0));
        assert!(!memory_admits(MEMORY_RESERVE_BYTES - 1, 0));
        assert!(memory_admits(MEMORY_RESERVE_BYTES + 1150, 1000));
        assert!(!memory_admits(MEMORY_RESERVE_BYTES + 1149, 1000));
        assert!(!memory_admits(u64::MAX, u64::MAX));
    }

    #[test]
    fn missing_metrics_and_load_flapping_cannot_keep_b_admitted() {
        let mut policy = stable();
        let mut missing = sample_resources(11_000, 10.0, 20.0);
        missing.total_cpu_percent = Some(f32::NAN);
        policy.observe(missing);
        assert_eq!(
            policy.retain(11_000, 0, false),
            Some("resource_metrics_unavailable")
        );
        for i in 12..42 {
            policy.observe(sample_resources(
                i * 1000,
                if i % 5 == 0 { 40.0 } else { 10.0 },
                40.0,
            ));
            assert!(policy.admit(i * 1000, 0, false).is_some());
        }
    }

    #[test]
    fn duty_ramps_only_until_a_pause_resets_the_run() {
        let mut policy = stable();
        assert_eq!(policy.duty_percent(10_000), 20);
        assert_eq!(policy.duty_percent(70_000), 30);
        assert_eq!(policy.duty_percent(130_000), 40);
        assert_eq!(policy.duty_percent(190_000), 50);
        assert_eq!(policy.duty_percent(900_000), 50);
        policy.reset_duty();
        assert_eq!(policy.duty_percent(901_000), 20);
    }
    #[test]
    fn only_capped_valid_samples_qualify_and_input_scales_do_not_transfer() {
        let mut b = PerformanceBook::new(env());
        for _ in 0..20 {
            let mut s = sample(10.0);
            s.cpu_rate_percent = 100;
            b.record(key(), s, ExecutionProfile::Idle);
        }
        assert!(!b.eligible(&key()));
        for _ in 0..19 {
            b.record(key(), sample(200.0), ExecutionProfile::Idle);
        }
        assert!(!b.eligible(&key()));
        b.record(key(), sample(200.0), ExecutionProfile::Idle);
        assert!(b.eligible(&key()));
        assert!(!b.eligible(&CostKey {
            input_bucket: 256,
            ..key()
        }));
        assert!(!b.eligible(&key().cold()));
        for _ in 0..3 {
            b.record(key().cold(), sample(900.0), ExecutionProfile::Idle);
        }
        assert!(b.eligible(&key().cold()));
        let mut changed = env();
        changed.logical_cores = 2;
        assert!(
            !PerformanceBook::restore(changed, Some(&serde_json::to_string(&b).unwrap()))
                .eligible(&key())
        );
    }
    #[test]
    fn slow_work_is_ineligible_and_overruns_require_fresh_idle_validation() {
        let mut b = PerformanceBook::new(env());
        for _ in 0..64 {
            b.record(key(), sample(700.0), ExecutionProfile::Idle);
        }
        assert!(!b.eligible(&key()));
        for _ in 0..64 {
            b.record(key(), sample(100.0), ExecutionProfile::Idle);
        }
        assert!(b.eligible(&key()));
        b.record(key(), sample(900.0), ExecutionProfile::Background);
        assert!(!b.eligible(&key()));
        for _ in 0..64 {
            b.record(key(), sample(100.0), ExecutionProfile::Background);
        }
        assert!(!b.eligible(&key()));
        for _ in 0..20 {
            b.record(key(), sample(100.0), ExecutionProfile::Idle);
        }
        assert!(b.eligible(&key()));
    }
    #[test]
    fn typing_allows_qualified_b_but_unknown_work_waits_for_a() {
        let r = stable();
        assert_eq!(
            choose_profile(signals(0), SchedulingMode::Auto, true, &r, 10_000),
            Ok(ExecutionProfile::Background)
        );
        assert_eq!(
            choose_profile(signals(0), SchedulingMode::Auto, false, &r, 10_000),
            Err("waiting_for_evaluation")
        );
        assert_eq!(
            choose_profile(signals(60), SchedulingMode::Auto, false, &r, 10_000),
            Ok(ExecutionProfile::Idle)
        );
        assert_eq!(
            choose_profile(signals(0), SchedulingMode::IdleOnly, true, &r, 10_000),
            Err("waiting_for_idle")
        );
        for s in [
            AdmissionSignals {
                ac_connected: false,
                ..signals(600)
            },
            AdmissionSignals {
                foreground: true,
                ..signals(600)
            },
            AdmissionSignals {
                authorized: false,
                ..signals(600)
            },
            AdmissionSignals {
                protected_session: true,
                ..signals(600)
            },
            AdmissionSignals {
                enabled: false,
                ..signals(600)
            },
        ] {
            assert!(choose_profile(s, SchedulingMode::Auto, true, &r, 10_000).is_err());
        }
    }
    #[test]
    fn pressure_requires_cooldown_then_ten_new_low_samples_and_missing_disk_blocks_io() {
        let mut r = stable();
        assert_eq!(r.admit(10_000, 0, true), None);
        r.observe(sample_resources(11_000, 51.0, 60.0));
        for i in 12..36 {
            r.observe(sample_resources(i * 1000, 5.0, 10.0));
            assert!(r.admit(i * 1000, 0, false).is_some());
        }
        r.observe(sample_resources(36_000, 5.0, 10.0));
        assert_eq!(r.admit(36_000, 0, false), None);
        r.latest.disk_latency_ms = None;
        assert_eq!(r.admit(36_000, 0, true), Some("waiting_for_disk"));
        assert_eq!(
            r.retain(39_000, 0, false),
            Some("resource_metrics_unavailable")
        );
    }
    #[test]
    fn memory_pressure_requires_a_fresh_cooldown_and_admission_window() {
        let mut resources = stable();
        assert!(resources.retain(10_000, 4 << 30, false).is_some());
        resources.defer_after_pressure(10_000);
        for second in 11..35 {
            resources.observe(sample_resources(second * 1000, 5.0, 10.0));
            assert!(resources.admit(second * 1000, 0, false).is_some());
        }
        resources.observe(sample_resources(35_000, 5.0, 10.0));
        assert!(resources.admit(35_000, 0, false).is_none());
    }

    #[test]
    fn busy_single_core_and_a_to_b_transition_are_not_hidden_by_low_total_cpu() {
        let mut r = stable();
        r.observe(sample_resources(11_000, 15.0, 96.0));
        assert!(r.retain(11_000, 0, false).is_none());
        r.observe(sample_resources(12_000, 15.0, 97.0));
        assert!(r.retain(12_000, 0, false).is_some());
        let lease = ExecutionLease::new("text", ExecutionProfile::Idle, 100);
        lease.revoke("input_resumed");
        assert!(lease.check().is_err());
        assert_eq!(lease.profile, ExecutionProfile::Idle);
    }
}
