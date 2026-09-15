//! Persistent HNSW acceleration for CLIP image search.
//!
//! SQLite remains authoritative. A generation is an immutable mmap base at a
//! covered data epoch; the latest changed subject per epoch is scanned exactly
//! as a bounded tail, including tombstones that suppress stale base entries.

use crate::ann_format::{
    validate_ann_recovered_probe, validate_ann_search_result, FlatFileWriter, Header,
    MappedFlatIndex, ANN_ALGORITHM, ANN_CONNECTIVITY, ANN_EXPANSION_ADD,
    ANN_IMPLEMENTATION_VERSION, ANN_METRIC, ANN_QUANTIZATION, FORMAT_VERSION,
};
use crate::clip_migration::{
    CLIP_DIMENSIONS, CLIP_EMBEDDING_VERSION, CLIP_MODEL_ID, CLIP_VECTOR_SPACE_REVISION,
};
use crate::registry_config;
use crate::resource_utils::find_existing_file_in_resources;
use crate::storage::{
    next_generation_id, DerivedAnnBuildState, DerivedAnnGeneration, DerivedAnnSnapshotRow,
    DerivedAnnTailRow, DerivedIndexKind, ScoredSubject, StorageState,
};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDateTime, Utc};
use memmap2::MmapOptions;
use sha2::{Digest, Sha256};
use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::watch;
use usearch::{Index, MetricKind, ScalarKind};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
};

mod resumable;
pub(crate) use resumable::run_scheduled_slice;

const ANN_FILE_FORMAT_VERSION: u32 = 1;
const MAX_TAIL_ROWS: u64 = 20_000;
const REBUILD_TAIL_ROWS: u64 = 5_000;
const REBUILD_TAIL_PERCENT: u64 = 5;
const MIN_REBUILD_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
pub(crate) const ANN_MAX_CANDIDATES: usize = 4096;
const ANN_MODES: &[&str] = &["off", "on"];
const DEFAULT_ANN_MODE: &str = "on";
const CREATE_NO_WINDOW: u32 = 0x08000000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
const ANN_BUILDER_MEMORY_LIMIT_BYTES: usize = 1024 * 1024 * 1024;
const ANN_FAILURE_NOTIFY_THRESHOLD: u32 = 3;
const ANN_PERMANENT_FAILURE_BACKOFF: Duration = Duration::from_secs(24 * 60 * 60);
const ANN_TRANSIENT_BACKOFF: &[Duration] = &[
    Duration::from_secs(15 * 60),
    Duration::from_secs(60 * 60),
    Duration::from_secs(6 * 60 * 60),
    Duration::from_secs(24 * 60 * 60),
];
const ANN_ERROR_MAX_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnnFailurePolicy {
    code: &'static str,
    delay: Duration,
    circuit_open: bool,
    notify: bool,
}

fn parse_ann_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").map(|value| value.and_utc())
        })
        .ok()
}

fn ann_retry_due(state: Option<&DerivedAnnBuildState>, now: DateTime<Utc>, force: bool) -> bool {
    if force {
        return true;
    }
    match state {
        None => true,
        Some(state) => parse_ann_timestamp(&state.next_retry_at)
            .map(|next_retry| next_retry <= now)
            // Corrupt persisted state must fail closed. The authenticated
            // manual retry remains available to repair the cycle.
            .unwrap_or(false),
    }
}

fn classify_ann_failure(error: &str, consecutive_failures: u32) -> AnnFailurePolicy {
    let lower = error.to_ascii_lowercase();
    let permanent = if lower.contains("carbonpaper-ml.exe was not found") {
        Some("builder_missing")
    } else if lower.contains("ann builder job") {
        Some("job_object_failed")
    } else if lower.contains("out of memory")
        || lower.contains("not enough memory")
        || lower.contains("cannot allocate memory")
        || lower.contains("memory allocation")
        || lower.contains("std::bad_alloc")
        || lower.contains("os error 8")
        || lower.contains("os error 1455")
        || lower.contains("-1073741801")
        || lower.contains("-1073741523")
        || lower.contains("3221225495")
        || lower.contains("3221225773")
    {
        Some("out_of_memory")
    } else if lower.contains("disk full")
        || lower.contains("no space left")
        || lower.contains("not enough space")
        || lower.contains("os error 112")
    {
        Some("disk_full")
    } else if lower.contains("access is denied")
        || lower.contains("permission denied")
        || lower.contains("os error 5")
    {
        Some("permission_denied")
    } else if lower.contains("model contract does not match")
        || lower.contains("format contract mismatch")
    {
        Some("contract_mismatch")
    } else {
        None
    };

    if let Some(code) = permanent {
        return AnnFailurePolicy {
            code,
            delay: ANN_PERMANENT_FAILURE_BACKOFF,
            circuit_open: true,
            notify: true,
        };
    }

    let delay = ANN_TRANSIENT_BACKOFF
        .get(consecutive_failures.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(ANN_PERMANENT_FAILURE_BACKOFF);
    AnnFailurePolicy {
        code: if lower.contains("ann builder failed") {
            "builder_failed"
        } else if lower.contains("snapshot changed") {
            "snapshot_changed"
        } else {
            "build_failed"
        },
        delay,
        circuit_open: consecutive_failures >= ANN_FAILURE_NOTIFY_THRESHOLD,
        notify: consecutive_failures >= ANN_FAILURE_NOTIFY_THRESHOLD,
    }
}

fn truncate_ann_error(error: &str) -> String {
    error.chars().take(ANN_ERROR_MAX_CHARS).collect()
}

fn record_ann_build_failure(
    app: &AppHandle,
    storage: &StorageState,
    state: &ClipAnnState,
    lifecycle_token: u64,
    error: &str,
) -> Result<DerivedAnnBuildState, String> {
    state.record_error_if_current(lifecycle_token, error.to_string());
    let previous = storage.get_derived_ann_build_state(DerivedIndexKind::ClipImage)?;
    let consecutive_failures = previous
        .as_ref()
        .map(|state| state.consecutive_failures.saturating_add(1))
        .unwrap_or(1);
    let policy = classify_ann_failure(error, consecutive_failures);
    let failed_at = Utc::now();
    let next_retry_at = failed_at
        + ChronoDuration::from_std(policy.delay).unwrap_or_else(|_| ChronoDuration::hours(24));
    let error = truncate_ann_error(error);
    let update = storage.record_derived_ann_build_failure(
        DerivedIndexKind::ClipImage,
        consecutive_failures,
        &failed_at.to_rfc3339(),
        &next_retry_at.to_rfc3339(),
        policy.code,
        &error,
        policy.circuit_open,
        policy.notify,
    )?;
    tracing::warn!(
        "[CLIP:ANN] build failure code={} count={} next_retry_at={} circuit_open={}",
        policy.code,
        consecutive_failures,
        update.state.next_retry_at,
        policy.circuit_open
    );
    if update.should_notify {
        let _ = app.emit(
            "app-toast",
            serde_json::json!({
                "id": format!("clip-ann-build-{}", failed_at.to_rfc3339()),
                "type": "error",
                "titleKey": "notifications.ann_build.title",
                "messageKey": "notifications.ann_build.body",
                "details": policy.code,
                "ackCommand": "clip_ann_ack_failure_notification",
                "timestamp": failed_at.timestamp_millis(),
            }),
        );
    }
    Ok(update.state)
}

pub fn enabled() -> bool {
    let configured =
        registry_config::get_string("clip_ann").map(|value| value.trim().to_ascii_lowercase());
    match configured.as_deref() {
        Some(value) if ANN_MODES.contains(&value) => value == "on",
        _ => DEFAULT_ANN_MODE == "on",
    }
}

pub struct ClipAnnState {
    resumable_worker: tokio::sync::Mutex<Option<resumable::BuildWorker>>,
    generation: RwLock<Option<Arc<GenerationReader>>>,
    build_lock: tokio::sync::Mutex<()>,
    last_error: RwLock<Option<String>>,
    arm: Mutex<ArmCoordinator>,
    arm_tx: watch::Sender<ArmStatus>,
}

impl Default for ClipAnnState {
    fn default() -> Self {
        let (arm_tx, _) = watch::channel(ArmStatus::Idle);
        Self {
            resumable_worker: tokio::sync::Mutex::new(None),
            generation: RwLock::new(None),
            build_lock: tokio::sync::Mutex::const_new(()),
            last_error: RwLock::new(None),
            arm: Mutex::new(ArmCoordinator::default()),
            arm_tx,
        }
    }
}

#[derive(Default)]
struct ArmCoordinator {
    token: u64,
    status: ArmStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArmStatus {
    Idle,
    Loading,
    Ready,
    Missing,
    Failed,
}

impl Default for ArmStatus {
    fn default() -> Self {
        Self::Idle
    }
}

struct GenerationReader {
    manifest: DerivedAnnGeneration,
    flat: MappedFlatIndex,
    data_dir: PathBuf,
    // Drop the USearch view before the backing mmap it aliases.
    index: Index,
    _ann_map: memmap2::Mmap,
    _ann_file: fs::File,
}

struct PreparedGeneration {
    reader: Option<GenerationReader>,
}

impl PreparedGeneration {
    fn new(reader: GenerationReader) -> Self {
        Self {
            reader: Some(reader),
        }
    }

    fn reader(&self) -> &GenerationReader {
        self.reader
            .as_ref()
            .expect("prepared generation reader is present")
    }

    fn into_reader(mut self) -> GenerationReader {
        self.reader
            .take()
            .expect("prepared generation reader is present")
    }
}

impl Drop for PreparedGeneration {
    fn drop(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        let directory = reader.data_dir.join("derived-indexes");
        let flat_path = directory.join(&reader.manifest.flat_file_name);
        let ann_path = directory.join(&reader.manifest.ann_file_name);
        drop(reader);
        let _ = fs::remove_file(flat_path);
        let _ = fs::remove_file(ann_path);
    }
}

struct TopCandidate {
    score: f32,
    subject_key: String,
}

impl PartialEq for TopCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.subject_key == other.subject_key
    }
}

impl Eq for TopCandidate {}

impl PartialOrd for TopCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopCandidate {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.subject_key.cmp(&self.subject_key))
    }
}

struct BoundedTopK {
    want: usize,
    heap: BinaryHeap<Reverse<TopCandidate>>,
}

impl BoundedTopK {
    fn new(want: usize) -> Self {
        Self {
            want,
            heap: BinaryHeap::with_capacity(want),
        }
    }

    fn push(&mut self, score: f32, subject_key: &str) {
        if self.want == 0 {
            return;
        }
        if self.heap.len() < self.want {
            self.heap.push(Reverse(TopCandidate {
                score,
                subject_key: subject_key.to_string(),
            }));
            return;
        }
        let replace = self
            .heap
            .peek()
            .map(|worst| {
                score
                    .total_cmp(&worst.0.score)
                    .then_with(|| worst.0.subject_key.as_str().cmp(subject_key))
                    .is_gt()
            })
            .unwrap_or(true);
        if replace {
            let _ = self.heap.pop();
            self.heap.push(Reverse(TopCandidate {
                score,
                subject_key: subject_key.to_string(),
            }));
        }
    }

    fn into_sorted(mut self) -> Vec<ScoredSubject> {
        let mut entries: Vec<TopCandidate> = self
            .heap
            .drain()
            .map(|Reverse(candidate)| candidate)
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        entries
            .into_iter()
            .map(|candidate| ScoredSubject {
                subject_key: candidate.subject_key,
                score: candidate.score,
            })
            .collect()
    }
}

impl GenerationReader {
    fn matches_storage(&self, storage: &StorageState) -> bool {
        let data_dir = storage
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.data_dir == *data_dir
    }
}

#[derive(Debug, Clone)]
pub struct ClipAnnQuery {
    pub candidates: Vec<ScoredSubject>,
    pub mode: &'static str,
    pub tail_rows: usize,
}

impl ClipAnnState {
    fn generation_snapshot_with_tail<F>(
        &self,
        storage: &StorageState,
        load_tail: F,
    ) -> Result<Option<(Arc<GenerationReader>, Vec<DerivedAnnTailRow>)>, String>
    where
        F: FnOnce(u64) -> Result<Vec<DerivedAnnTailRow>, String>,
    {
        let slot = self
            .generation
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let Some(generation) = slot.as_ref() else {
            return Ok(None);
        };
        if !generation.matches_storage(storage) || generation.manifest.row_count == 0 {
            return Ok(None);
        }
        // Keep the reader slot pinned until its epoch-dependent tail has been
        // materialized. Publication takes the write side of this same lock
        // across manifest pruning and reader replacement.
        let tail = load_tail(generation.manifest.covered_epoch)?;
        Ok(Some((Arc::clone(generation), tail)))
    }

    pub fn query(
        &self,
        storage: &StorageState,
        query: &[f32],
        k: usize,
    ) -> Result<Option<ClipAnnQuery>, String> {
        if !enabled() || query.len() != CLIP_DIMENSIONS || k == 0 {
            return Ok(None);
        }
        let (generation, tail) =
            match self.generation_snapshot_with_tail(storage, |covered_epoch| {
                storage.list_derived_ann_tail(
                    DerivedIndexKind::ClipImage,
                    covered_epoch,
                    MAX_TAIL_ROWS,
                )
            }) {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return Ok(None),
                Err(error) if error.starts_with("ann_tail_too_large:") => return Ok(None),
                Err(error) => return Err(error),
            };
        let changed: HashSet<&str> = tail.iter().map(|row| row.subject_key.as_str()).collect();
        let candidate_count = ann_candidate_count(generation.manifest.row_count as usize, k);
        let matches = generation
            .index
            .search(query, candidate_count)
            .map_err(|error| format!("ann_search_failed:{error}"))?;

        let mut merged: HashMap<String, f32> =
            HashMap::with_capacity(matches.keys.len() + tail.len());
        for key in matches.keys {
            let Some(ordinal) = key
                .checked_sub(1)
                .and_then(|value| usize::try_from(value).ok())
            else {
                return Err("ann_search_returned_invalid_key".to_string());
            };
            let subject_key = generation.flat.key(ordinal)?;
            if changed.contains(subject_key) {
                continue;
            }
            let score = dot_product(query, generation.flat.vector(ordinal)?);
            merged.insert(subject_key.to_string(), score);
        }
        for DerivedAnnTailRow {
            subject_key,
            vector,
        } in &tail
        {
            if let Some(vector) = vector {
                if vector.len() == query.len() {
                    merged.insert(subject_key.clone(), dot_product(query, vector));
                }
            } else {
                merged.remove(subject_key);
            }
        }
        let mut candidates: Vec<ScoredSubject> = merged
            .into_iter()
            .map(|(subject_key, score)| ScoredSubject { subject_key, score })
            .collect();
        candidates.sort_unstable_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.subject_key.cmp(&b.subject_key))
        });
        candidates.truncate(k);
        Ok(Some(ClipAnnQuery {
            candidates,
            mode: "ann",
            tail_rows: tail.len(),
        }))
    }

    pub fn has_generation(&self) -> bool {
        self.generation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    #[cfg(test)]
    fn pin_generation(&self) -> Option<Arc<GenerationReader>> {
        self.generation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn exact_from_generation(
        &self,
        storage: &StorageState,
        query: &[f32],
        k: usize,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<ClipAnnQuery>, String> {
        if !enabled() || query.len() != CLIP_DIMENSIONS || k == 0 {
            return Ok(None);
        }
        let (generation, tail) =
            match self.generation_snapshot_with_tail(storage, |covered_epoch| {
                storage.list_derived_ann_tail(
                    DerivedIndexKind::ClipImage,
                    covered_epoch,
                    MAX_TAIL_ROWS,
                )
            }) {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return Ok(None),
                Err(_) => return Ok(None),
            };
        let candidates = exact_candidates(&generation, &tail, query, k, deadline)?;
        Ok(Some(ClipAnnQuery {
            candidates,
            mode: "flat_exact",
            tail_rows: tail.len(),
        }))
    }

    pub(crate) fn disarm(&self) {
        if let Ok(mut worker) = self.resumable_worker.try_lock() {
            worker.take();
        }
        let mut generation = self
            .generation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        *generation = None;
        arm.token = arm.token.wrapping_add(1);
        arm.status = ArmStatus::Idle;
        self.arm_tx.send_replace(ArmStatus::Idle);
    }

    pub(crate) fn reclaim_builder(&self, memory_pressure: bool) {
        if let Ok(mut slot) = self.resumable_worker.try_lock() {
            if slot
                .as_ref()
                .is_some_and(|worker| memory_pressure || worker.expired())
            {
                slot.take();
            }
        }
    }

    fn begin_arm(&self) -> Option<u64> {
        let generation = self
            .generation
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if generation.is_some() {
            arm.status = ArmStatus::Ready;
            self.arm_tx.send_replace(ArmStatus::Ready);
            return None;
        }
        if arm.status == ArmStatus::Loading {
            return None;
        }
        arm.token = arm.token.wrapping_add(1);
        let token = arm.token;
        arm.status = ArmStatus::Loading;
        self.arm_tx.send_replace(ArmStatus::Loading);
        Some(token)
    }

    fn arm_is_current(&self, token: u64) -> bool {
        let arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        arm.token == token && arm.status == ArmStatus::Loading
    }

    fn lifecycle_token(&self) -> u64 {
        self.arm
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .token
    }

    fn finish_arm(&self, token: u64, status: ArmStatus) -> bool {
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token || arm.status != ArmStatus::Loading {
            return false;
        }
        arm.status = status;
        self.arm_tx.send_replace(status);
        true
    }

    fn install_from_arm(
        &self,
        token: u64,
        storage: &StorageState,
        generation: GenerationReader,
    ) -> bool {
        if !generation.matches_storage(storage) {
            return false;
        }
        // `generation -> arm` is the lifecycle lock order used by `disarm`.
        // Keeping both locks through token validation and publication closes
        // the window where a data-directory migration could invalidate the
        // token and then have this old reader written back afterwards.
        let mut slot = self
            .generation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token || arm.status != ArmStatus::Loading {
            return false;
        }
        *slot = Some(Arc::new(generation));
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        arm.status = ArmStatus::Ready;
        self.arm_tx.send_replace(ArmStatus::Ready);
        true
    }

    fn publish_from_lifecycle(
        &self,
        token: u64,
        storage: &StorageState,
        generation: PreparedGeneration,
    ) -> Result<bool, String> {
        if !generation.reader().matches_storage(storage) {
            return Ok(false);
        }
        let mut slot = self
            .generation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token {
            return Ok(false);
        }
        // Queries hold the read side through tail materialization, so this DB
        // transaction and the in-memory reader switch are one publication
        // boundary from their perspective.
        storage.record_derived_ann_generation(&generation.reader().manifest)?;
        *slot = Some(Arc::new(generation.into_reader()));
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        arm.status = ArmStatus::Ready;
        self.arm_tx.send_replace(ArmStatus::Ready);
        Ok(true)
    }

    fn fail_arm(&self, token: u64, error: String) -> bool {
        let mut generation = self
            .generation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token || arm.status != ArmStatus::Loading {
            return false;
        }
        *generation = None;
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(error);
        arm.status = ArmStatus::Failed;
        self.arm_tx.send_replace(ArmStatus::Failed);
        true
    }

    fn record_error_if_current(&self, token: u64, error: String) -> bool {
        let generation = self
            .generation
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token {
            return false;
        }
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(error);
        if generation.is_none() {
            arm.status = ArmStatus::Failed;
            self.arm_tx.send_replace(ArmStatus::Failed);
        }
        true
    }

    #[cfg(test)]
    fn install_for_test(&self, generation: GenerationReader) {
        let mut slot = self
            .generation
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let mut arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        *slot = Some(Arc::new(generation));
        arm.status = ArmStatus::Ready;
        self.arm_tx.send_replace(ArmStatus::Ready);
    }

    pub async fn wait_for_startup_arm(&self, timeout: Duration) -> bool {
        if !enabled() || self.has_generation() {
            return self.has_generation();
        }
        let mut receiver = self.arm_tx.subscribe();
        let wait = async {
            loop {
                match *receiver.borrow_and_update() {
                    ArmStatus::Ready => return true,
                    ArmStatus::Idle | ArmStatus::Missing | ArmStatus::Failed => return false,
                    ArmStatus::Loading => {}
                }
                if receiver.changed().await.is_err() {
                    return false;
                }
            }
        };
        tokio::time::timeout(timeout, wait).await.unwrap_or(false)
    }

    pub fn status(&self) -> (&'static str, Option<u64>, Option<String>) {
        if !enabled() {
            return ("disabled", None, None);
        }
        let generation = self
            .generation
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let error = self
            .last_error
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        match generation.as_ref() {
            Some(reader) => ("armed", Some(reader.manifest.generation), error),
            None => {
                let status = match *self.arm_tx.borrow() {
                    ArmStatus::Loading => "arming",
                    ArmStatus::Failed => "failed",
                    ArmStatus::Idle | ArmStatus::Missing | ArmStatus::Ready => "exact_fallback",
                };
                (status, None, error)
            }
        }
    }
}

fn exact_candidates(
    generation: &GenerationReader,
    tail: &[DerivedAnnTailRow],
    query: &[f32],
    k: usize,
    deadline: Option<std::time::Instant>,
) -> Result<Vec<ScoredSubject>, String> {
    let changed: HashSet<&str> = tail.iter().map(|row| row.subject_key.as_str()).collect();
    let mut top = BoundedTopK::new(k);
    for ordinal in 0..generation.flat.rows() {
        if ordinal % 4096 == 0
            && deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err("query_deadline_exceeded_during_flat_exact".to_string());
        }
        let key = generation.flat.key(ordinal)?;
        if !changed.contains(key) {
            top.push(dot_product(query, generation.flat.vector(ordinal)?), key);
        }
    }
    for (index, row) in tail.iter().enumerate() {
        if index % 1024 == 0
            && deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err("query_deadline_exceeded_during_flat_exact".to_string());
        }
        if let Some(vector) = &row.vector {
            if vector.len() == query.len() {
                top.push(dot_product(query, vector), &row.subject_key);
            }
        }
    }
    Ok(top.into_sorted())
}

pub fn spawn_startup_arm(app: AppHandle) {
    if !enabled() {
        tracing::info!("[CLIP:ANN] disabled by registry switch");
        return;
    }
    let state = app.state::<Arc<ClipAnnState>>().inner().clone();
    let Some(arm_token) = state.begin_arm() else {
        return;
    };
    #[derive(Debug)]
    enum StartupArmOutcome {
        Installed {
            generation: u64,
            rows: u64,
            expansion_search: u32,
        },
        Missing,
        Discarded,
    }
    tauri::async_runtime::spawn(async move {
        let _operation_guard = state.build_lock.lock().await;
        if !state.arm_is_current(arm_token) {
            return;
        }
        let storage = app.state::<Arc<StorageState>>().inner().clone();
        let load_storage = storage.clone();
        let load_state = state.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _publish_guard = load_storage.derived_generation_publish_guard();
            if load_storage.is_migration_in_progress() {
                return Err("ANN arm deferred during data migration".to_string());
            }
            if !load_state.arm_is_current(arm_token) {
                return Ok(StartupArmOutcome::Discarded);
            }
            match load_current_generation(&load_storage)? {
                Some(reader) => {
                    let outcome = StartupArmOutcome::Installed {
                        generation: reader.manifest.generation,
                        rows: reader.manifest.row_count,
                        expansion_search: reader.manifest.expansion_search,
                    };
                    if load_state.install_from_arm(arm_token, &load_storage, reader) {
                        Ok(outcome)
                    } else {
                        Ok(StartupArmOutcome::Discarded)
                    }
                }
                None => Ok(StartupArmOutcome::Missing),
            }
        })
        .await;
        match result {
            Ok(Ok(StartupArmOutcome::Installed {
                generation,
                rows,
                expansion_search,
            })) => {
                tracing::info!(
                    "[CLIP:ANN] armed generation={} rows={} ef_search={}",
                    generation,
                    rows,
                    expansion_search
                );
            }
            Ok(Ok(StartupArmOutcome::Missing)) => {
                tracing::info!("[CLIP:ANN] no persistent generation; exact fallback active");
                state.finish_arm(arm_token, ArmStatus::Missing);
                drop(_operation_guard);
                spawn_missing_generation_bootstrap(app, state);
                return;
            }
            Ok(Ok(StartupArmOutcome::Discarded)) => {}
            Ok(Err(error)) => {
                tracing::warn!("[CLIP:ANN] startup arm failed: {error}");
                state.fail_arm(arm_token, error);
                let _ = maybe_rebuild(&app, false).await;
            }
            Err(error) => {
                state.fail_arm(arm_token, format!("ANN startup task failed: {error}"));
            }
        }
    });
}

/// Arrange a one-time bootstrap for an already-migrated installation. The
/// Chroma migration path calls [`bootstrap_in_maintenance`] directly while it
/// already owns the maintenance/capture pause; this coordinator is only for
/// the case where the legacy SQLite rows were present before ANN support was
/// introduced.
fn spawn_missing_generation_bootstrap(app: AppHandle, state: Arc<ClipAnnState>) {
    tauri::async_runtime::spawn(async move {
        // Do not compete with the CLIP Chroma copy. If it is needed, that copy
        // builds the ANN in its existing maintenance window. The migration
        // auto-triggers retry maintenance contention, so another startup
        // migration that races this ANN-only task waits rather than being
        // postponed to the next launch.
        loop {
            if state.has_generation() {
                return;
            }
            let storage = app.state::<Arc<StorageState>>().inner().clone();
            let clip_done = storage
                .is_auto_migration_done(DerivedIndexKind::ClipImage, CLIP_VECTOR_SPACE_REVISION)
                .unwrap_or(false);
            if startup_bootstrap_ready(clip_done, crate::maintenance::is_active()) {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        if state.has_generation() {
            return;
        }
        let result = maybe_rebuild(&app, false).await;
        match result {
            Ok(true) => tracing::info!("[CLIP:ANN] bootstrap queued the initial generation"),
            Ok(false) => tracing::info!("[CLIP:ANN] bootstrap found no query-visible vectors"),
            Err(error) => tracing::warn!("[CLIP:ANN] bootstrap failed: {error}"),
        }
    });
}

fn startup_bootstrap_ready(clip_done: bool, maintenance_active: bool) -> bool {
    clip_done && !maintenance_active
}

/// Migration and retry callers only enqueue; the durable task owns all builds.
pub async fn bootstrap_in_maintenance(app: &AppHandle) -> Result<bool, String> {
    maybe_rebuild(app, false).await
}

pub async fn maybe_rebuild(app: &AppHandle, force: bool) -> Result<bool, String> {
    if !enabled() {
        return Ok(false);
    }
    app.state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        .enqueue(
            app,
            crate::background_scheduler::BackgroundTaskKind::AnnBuild,
            force,
        )?;
    Ok(true)
}

fn rebuild_needed(storage: &StorageState, force: bool) -> Result<bool, String> {
    let existing = storage.get_derived_ann_generation(DerivedIndexKind::ClipImage)?;
    if force || existing.is_none() {
        return storage.has_query_visible_embeddings(DerivedIndexKind::ClipImage);
    }
    let existing = existing.unwrap();
    let tail =
        storage.derived_ann_tail_count(DerivedIndexKind::ClipImage, existing.covered_epoch)?;
    let threshold =
        REBUILD_TAIL_ROWS.max(existing.row_count.saturating_mul(REBUILD_TAIL_PERCENT) / 100);
    if tail < threshold {
        return Ok(false);
    }
    let created = chrono::DateTime::parse_from_rfc3339(&existing.created_at)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&existing.created_at, "%Y-%m-%d %H:%M:%S")
                .map(|value| value.and_utc().fixed_offset())
        })
        .ok();
    Ok(created
        .and_then(|value| {
            chrono::Utc::now()
                .signed_duration_since(value.with_timezone(&chrono::Utc))
                .to_std()
                .ok()
        })
        .map(|age| age >= MIN_REBUILD_INTERVAL)
        .unwrap_or(true))
}

fn write_ann_snapshot_page(
    writer: &mut FlatFileWriter,
    dimensions: u32,
    written_rows: &mut u64,
    page: &[DerivedAnnSnapshotRow],
) -> Result<(), String> {
    for row in page {
        if row.dimensions != dimensions {
            return Err("ANN snapshot contains mixed dimensions".to_string());
        }
    }
    let keys: Vec<&str> = page.iter().map(|row| row.subject_key.as_str()).collect();
    let vectors: Vec<&[u8]> = page.iter().map(|row| row.vector_f32.as_slice()).collect();
    writer.push_keys(&keys)?;
    writer.push_vector_bytes(&vectors)?;
    *written_rows = written_rows.saturating_add(page.len() as u64);
    Ok(())
}

fn load_current_generation(storage: &StorageState) -> Result<Option<GenerationReader>, String> {
    let Some(manifest) = storage.get_derived_ann_generation(DerivedIndexKind::ClipImage)? else {
        return Ok(None);
    };
    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    open_generation(&data_dir, manifest).map(Some)
}

fn open_generation(
    data_dir: &Path,
    manifest: DerivedAnnGeneration,
) -> Result<GenerationReader, String> {
    validate_manifest(&manifest)?;
    let directory = data_dir.join("derived-indexes");
    let flat_path = directory.join(&manifest.flat_file_name);
    let ann_path = directory.join(&manifest.ann_file_name);
    if sha256_file(&flat_path)? != manifest.flat_checksum_sha256
        || sha256_file(&ann_path)? != manifest.ann_checksum_sha256
    {
        return Err("ANN generation checksum mismatch".to_string());
    }
    open_generation_validated(data_dir, manifest)
}

fn open_generation_validated(
    data_dir: &Path,
    manifest: DerivedAnnGeneration,
) -> Result<GenerationReader, String> {
    validate_manifest(&manifest)?;
    let directory = data_dir.join("derived-indexes");
    let flat_path = directory.join(&manifest.flat_file_name);
    let ann_path = directory.join(&manifest.ann_file_name);
    let flat = MappedFlatIndex::open(&flat_path)?;
    if flat.header.generation != manifest.generation
        || flat.header.covered_epoch != manifest.covered_epoch
        || flat.header.row_count != manifest.row_count
        || flat.header.dimensions != manifest.dimensions
        || flat.header.expansion_search != manifest.expansion_search
    {
        return Err("ANN flat header does not match manifest".to_string());
    }
    let ann_file = fs::File::open(&ann_path)
        .map_err(|error| format!("Failed to open ANN graph {}: {error}", ann_path.display()))?;
    let ann_map = unsafe { MmapOptions::new().map(&ann_file) }
        .map_err(|error| format!("Failed to mmap ANN graph {}: {error}", ann_path.display()))?;
    let metadata = Index::metadata_from_buffer(&ann_map)
        .map_err(|error| format!("ANN metadata read failed: {error}"))?;
    let options: usearch::IndexOptions = metadata.into();
    let index = Index::new(&options).map_err(|error| format!("ANN view init failed: {error}"))?;
    unsafe { index.view_from_buffer(&ann_map) }
        .map_err(|error| format!("ANN buffer view failed: {error}"))?;
    apply_expansion_search(&index, manifest.expansion_search as usize)?;
    if index.expansion_search() != manifest.expansion_search as usize
        || index.dimensions() != manifest.dimensions as usize
        || index.size() != manifest.row_count as usize
        || index.connectivity() != manifest.connectivity as usize
        || index.metric_kind() != MetricKind::IP
        || index.scalar_kind() != ScalarKind::I8
    {
        return Err("ANN graph contract does not match manifest".to_string());
    }
    if manifest.row_count > 0 {
        let probe = flat.vector(0)?;
        if !index.contains(1) {
            return Err("ANN self-test is missing the probe ordinal".to_string());
        }
        let mut recovered = vec![0.0f32; manifest.dimensions as usize];
        let vectors_found = index
            .get(1, &mut recovered)
            .map_err(|error| format!("ANN probe recovery failed: {error}"))?;
        let recovered_cosine = validate_ann_recovered_probe(probe, &recovered, vectors_found)?;
        let result = index
            .search(probe, 1)
            .map_err(|error| format!("ANN self-test failed: {error}"))?;
        validate_ann_search_result(&result.keys, &result.distances, manifest.row_count)?;
        tracing::info!(
            "[CLIP:ANN] self-test probe_key=1 returned_key={} distance={:.6} recovered_cosine={:.6}",
            result.keys[0],
            result.distances[0],
            recovered_cosine
        );
    }
    Ok(GenerationReader {
        manifest,
        flat,
        data_dir: data_dir.to_path_buf(),
        index,
        _ann_map: ann_map,
        _ann_file: ann_file,
    })
}

fn apply_expansion_search(index: &Index, expansion_search: usize) -> Result<(), String> {
    index.change_expansion_search(expansion_search);
    if index.expansion_search() != expansion_search {
        return Err("ANN expansion_search was not restored".to_string());
    }
    Ok(())
}

fn validate_manifest(manifest: &DerivedAnnGeneration) -> Result<(), String> {
    if manifest.index_kind != DerivedIndexKind::ClipImage
        || manifest.model_id != CLIP_MODEL_ID
        || manifest.model_revision != CLIP_VECTOR_SPACE_REVISION
        || manifest.embedding_version != CLIP_EMBEDDING_VERSION
        || manifest.dimensions as usize != CLIP_DIMENSIONS
        || manifest.sidecar_format_version != FORMAT_VERSION
        || manifest.ann_format_version != ANN_FILE_FORMAT_VERSION
        || manifest.algorithm != ANN_ALGORITHM
        || manifest.implementation_version != ANN_IMPLEMENTATION_VERSION
        || manifest.metric != ANN_METRIC
        || manifest.quantization != ANN_QUANTIZATION
        || manifest.connectivity != ANN_CONNECTIVITY
        || manifest.expansion_add != ANN_EXPANSION_ADD
    {
        return Err("ANN generation model or format contract mismatch".to_string());
    }
    if manifest.flat_file_name != format!("clip_image-{}.cpdvec", manifest.generation)
        || manifest.ann_file_name != format!("clip_image-{}.cpdann", manifest.generation)
    {
        return Err("ANN generation file names do not match the generation".to_string());
    }
    Ok(())
}

struct PendingBuilder(Option<Child>);

impl PendingBuilder {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child(&self) -> &Child {
        self.0.as_ref().expect("pending ANN builder is available")
    }

    fn take(&mut self) -> Child {
        self.0.take().expect("pending ANN builder is available")
    }
}

impl Drop for PendingBuilder {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct BuilderJob(HANDLE);
// SAFETY: a Job Object is a kernel reference. The owned wrapper can move
// between scheduler threads; Drop remains its only CloseHandle owner.
unsafe impl Send for BuilderJob {}

impl Drop for BuilderJob {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn create_builder_job() -> Result<BuilderJob, String> {
    unsafe {
        let handle = CreateJobObjectW(None, None)
            .map_err(|error| format!("Failed to create ANN builder job: {error:?}"))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = ANN_BUILDER_MEMORY_LIMIT_BYTES;
        if let Err(error) = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(handle);
            return Err(format!("Failed to constrain ANN builder job: {error:?}"));
        }
        Ok(BuilderJob(handle))
    }
}

fn assign_builder_job(job: &BuilderJob, child: &Child) -> Result<(), String> {
    unsafe {
        let process_handle = HANDLE(child.as_raw_handle() as *mut _);
        AssignProcessToJobObject(job.0, process_handle)
            .map_err(|error| format!("Failed to assign ANN builder job: {error:?}"))
    }
}

#[tauri::command]
pub async fn clip_ann_retry_now(
    app: AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
) -> Result<bool, String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    maybe_rebuild(&app, true).await
}

fn ann_failure_toast(state: &DerivedAnnBuildState) -> serde_json::Value {
    serde_json::json!({
        "id": format!("clip-ann-build-{}", state.last_failure_at),
        "type": "error",
        "titleKey": "notifications.ann_build.title",
        "messageKey": "notifications.ann_build.body",
        "details": state.last_error_code,
        "timestamp": parse_ann_timestamp(&state.last_failure_at)
            .map(|value| value.timestamp_millis())
            .unwrap_or_else(|| Utc::now().timestamp_millis()),
    })
}

#[tauri::command]
pub fn clip_ann_take_failure_notification(
    window: tauri::Window,
    storage: tauri::State<'_, Arc<StorageState>>,
) -> Result<Option<serde_json::Value>, String> {
    crate::commands::check_main_window(&window)?;
    storage
        .take_derived_ann_build_notification(DerivedIndexKind::ClipImage)
        .map(|state| state.as_ref().map(ann_failure_toast))
}

#[tauri::command]
pub fn clip_ann_ack_failure_notification(
    window: tauri::Window,
    storage: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    storage.mark_derived_ann_build_notification_sent(DerivedIndexKind::ClipImage)
}

fn resolve_ml_executable(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(path) = find_existing_file_in_resources(app, "carbonpaper-ml.exe") {
        return Ok(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join("carbonpaper-ml.exe");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("carbonpaper-ml.exe");
    development
        .exists()
        .then_some(development)
        .ok_or_else(|| "carbonpaper-ml.exe was not found for ANN construction".to_string())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn ann_candidate_count(rows: usize, requested: usize) -> usize {
    requested
        .saturating_mul(4)
        .max(requested.saturating_add(64))
        .min(rows)
        .min(ANN_MAX_CANDIDATES)
}

pub(crate) fn expansion_search(rows: usize) -> usize {
    (rows / 600).max(3 * 800).clamp(96, ANN_MAX_CANDIDATES)
}

fn dot_product(query: &[f32], row: &[f32]) -> f32 {
    query.iter().zip(row).map(|(a, b)| a * b).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;

    fn test_storage(data_dir: &Path) -> StorageState {
        let credential = Arc::new(CredentialManagerState::new(data_dir.to_path_buf()));
        StorageState::new(data_dir.to_path_buf(), credential)
    }

    fn test_manifest(generation: u64, rows: u64, dimensions: u32) -> DerivedAnnGeneration {
        DerivedAnnGeneration {
            index_kind: DerivedIndexKind::ClipImage,
            generation,
            covered_epoch: 1,
            flat_file_name: format!("clip_image-{generation}.cpdvec"),
            flat_checksum_sha256: String::new(),
            ann_file_name: format!("clip_image-{generation}.cpdann"),
            ann_checksum_sha256: String::new(),
            row_count: rows,
            dimensions,
            model_id: CLIP_MODEL_ID.to_string(),
            model_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
            embedding_version: CLIP_EMBEDDING_VERSION,
            sidecar_format_version: FORMAT_VERSION,
            ann_format_version: ANN_FILE_FORMAT_VERSION,
            algorithm: ANN_ALGORITHM.to_string(),
            implementation_version: ANN_IMPLEMENTATION_VERSION.to_string(),
            metric: ANN_METRIC.to_string(),
            quantization: ANN_QUANTIZATION.to_string(),
            connectivity: ANN_CONNECTIVITY,
            expansion_add: ANN_EXPANSION_ADD,
            expansion_search: 96,
            created_at: String::new(),
        }
    }

    fn test_reader(
        data_dir: &Path,
        generation: u64,
        keys: &[&str],
        vectors: &[Vec<f32>],
    ) -> GenerationReader {
        assert_eq!(keys.len(), vectors.len());
        let dimensions = vectors.first().map(Vec::len).unwrap_or(2) as u32;
        let directory = data_dir.join("derived-indexes");
        fs::create_dir_all(&directory).unwrap();
        let flat_path = directory.join(format!("clip_image-{generation}.cpdvec"));
        let ann_path = directory.join(format!("clip_image-{generation}.cpdann"));
        let key_bytes = keys.iter().map(|key| key.len() as u64).sum();
        let header = Header::for_snapshot(
            generation,
            1,
            keys.len() as u64,
            dimensions,
            "clip_image",
            CLIP_MODEL_ID,
            CLIP_VECTOR_SPACE_REVISION,
            96,
            key_bytes,
        )
        .unwrap();
        let mut writer = FlatFileWriter::create(&flat_path, header).unwrap();
        for key in keys {
            writer.push_key(key).unwrap();
        }
        for vector in vectors {
            writer.push_vector(vector).unwrap();
        }
        writer.finish().unwrap();
        let flat = MappedFlatIndex::open(&flat_path).unwrap();

        let mut options = usearch::IndexOptions::default();
        options.dimensions = dimensions as usize;
        options.metric = MetricKind::IP;
        options.quantization = ScalarKind::I8;
        options.connectivity = ANN_CONNECTIVITY as usize;
        options.expansion_add = ANN_EXPANSION_ADD as usize;
        options.expansion_search = 96;
        let built = Index::new(&options).unwrap();
        built.reserve(keys.len()).unwrap();
        for (ordinal, vector) in vectors.iter().enumerate() {
            built.add(ordinal as u64 + 1, vector).unwrap();
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&ann_path)
            .unwrap();
        file.set_len(built.serialized_length() as u64).unwrap();
        let mut ann_map = unsafe { MmapOptions::new().map_mut(&file) }.unwrap();
        built.save_to_buffer(&mut ann_map).unwrap();
        ann_map.flush().unwrap();
        drop(built);
        let ann_map = ann_map.make_read_only().unwrap();
        let metadata = Index::metadata_from_buffer(&ann_map).unwrap();
        let index = Index::new(&metadata.into()).unwrap();
        unsafe { index.view_from_buffer(&ann_map) }.unwrap();
        apply_expansion_search(&index, 96).unwrap();

        GenerationReader {
            manifest: test_manifest(generation, keys.len() as u64, dimensions),
            flat,
            data_dir: data_dir.to_path_buf(),
            index,
            _ann_map: ann_map,
            _ann_file: file,
        }
    }

    #[test]
    fn candidate_count_is_bounded_and_overfetches() {
        assert_eq!(ann_candidate_count(10, 2), 10);
        assert!(ann_candidate_count(100_000, 200) >= 800);
        assert_eq!(ann_candidate_count(1_000_000, 20_000), ANN_MAX_CANDIDATES);
    }

    #[test]
    fn expansion_covers_production_page_sizes() {
        assert_eq!(expansion_search(50_000), 2400);
        assert!(expansion_search(600_000) >= 2400);
    }

    #[test]
    fn startup_bootstrap_waits_for_clip_migration_and_an_idle_maintenance_guard() {
        assert!(!startup_bootstrap_ready(false, false));
        assert!(!startup_bootstrap_ready(true, true));
        assert!(startup_bootstrap_ready(true, false));
    }

    #[test]
    fn transient_failures_back_off_and_open_after_three_attempts() {
        let first = classify_ann_failure("temporary builder failure", 1);
        let second = classify_ann_failure("temporary builder failure", 2);
        let third = classify_ann_failure("temporary builder failure", 3);
        let fourth = classify_ann_failure("temporary builder failure", 4);

        assert_eq!(first.delay, Duration::from_secs(15 * 60));
        assert!(!first.circuit_open);
        assert!(!first.notify);
        assert_eq!(second.delay, Duration::from_secs(60 * 60));
        assert!(!second.circuit_open);
        assert_eq!(third.delay, Duration::from_secs(6 * 60 * 60));
        assert!(third.circuit_open);
        assert!(third.notify);
        assert_eq!(fourth.delay, Duration::from_secs(24 * 60 * 60));
        assert!(fourth.circuit_open);
    }

    #[test]
    fn deterministic_failures_open_the_circuit_immediately() {
        for (error, code) in [
            (
                "carbonpaper-ml.exe was not found for ANN construction",
                "builder_missing",
            ),
            ("Failed to constrain ANN builder job", "job_object_failed"),
            ("ANN builder failed: out of memory", "out_of_memory"),
            ("Failed to write graph: os error 112", "disk_full"),
        ] {
            let policy = classify_ann_failure(error, 1);
            assert_eq!(policy.code, code);
            assert_eq!(policy.delay, ANN_PERMANENT_FAILURE_BACKOFF);
            assert!(policy.circuit_open);
            assert!(policy.notify);
        }
    }

    #[test]
    fn retry_gate_respects_deadline_but_force_bypasses_it() {
        let now = Utc::now();
        let state = DerivedAnnBuildState {
            index_kind: DerivedIndexKind::ClipImage,
            consecutive_failures: 1,
            last_failure_at: now.to_rfc3339(),
            next_retry_at: (now + ChronoDuration::minutes(15)).to_rfc3339(),
            last_error_code: "build_failed".to_string(),
            last_error: "temporary".to_string(),
            circuit_open: false,
            notification_sent: false,
        };
        assert!(!ann_retry_due(Some(&state), now, false));
        assert!(ann_retry_due(Some(&state), now, true));
        assert!(ann_retry_due(
            Some(&state),
            now + ChronoDuration::minutes(16),
            false
        ));

        let mut corrupt = state;
        corrupt.next_retry_at = "not-a-timestamp".to_string();
        assert!(!ann_retry_due(Some(&corrupt), now, false));
        assert!(ann_retry_due(Some(&corrupt), now, true));
    }

    #[test]
    fn manifest_rejects_parent_paths_and_generation_mismatches() {
        let mut manifest = test_manifest(7, 1, CLIP_DIMENSIONS as u32);
        manifest.flat_file_name = "../evil.cpdvec".to_string();
        assert!(validate_manifest(&manifest).is_err());

        let mut manifest = test_manifest(7, 1, CLIP_DIMENSIONS as u32);
        manifest.flat_file_name = "clip_image-8.cpdvec".to_string();
        assert!(validate_manifest(&manifest).is_err());

        let mut manifest = test_manifest(7, 1, CLIP_DIMENSIONS as u32);
        manifest.ann_file_name = "clip_image-8.cpdann".to_string();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn bounded_topk_matches_full_sort_including_tied_keys() {
        let rows = [
            (0.5, "z"),
            (0.9, "b"),
            (0.9, "a"),
            (0.4, "c"),
            (0.9, "d"),
            (0.9, "c"),
            (-0.1, "aa"),
        ];
        let mut full: Vec<ScoredSubject> = rows
            .iter()
            .map(|(score, subject_key)| ScoredSubject {
                subject_key: (*subject_key).to_string(),
                score: *score,
            })
            .collect();
        full.sort_unstable_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.subject_key.cmp(&b.subject_key))
        });
        full.truncate(4);

        let mut bounded = BoundedTopK::new(4);
        for (score, subject_key) in rows {
            bounded.push(score, subject_key);
        }
        assert_eq!(bounded.into_sorted(), full);
    }

    #[test]
    fn disarm_prevents_an_old_startup_token_from_installing() {
        let temp = tempfile::tempdir().unwrap();
        let storage = test_storage(temp.path());
        let state = ClipAnnState::default();
        let token = state.begin_arm().unwrap();
        state.disarm();

        assert!(!state.install_from_arm(
            token,
            &storage,
            test_reader(temp.path(), 11, &["a"], &[vec![1.0, 0.0]])
        ));
        assert!(!state.has_generation());
    }

    #[test]
    fn disarm_prevents_an_old_rebuild_token_from_installing() {
        let temp = tempfile::tempdir().unwrap();
        let storage = test_storage(temp.path());
        let state = ClipAnnState::default();
        let token = state.lifecycle_token();
        state.disarm();
        let prepared =
            PreparedGeneration::new(test_reader(temp.path(), 12, &["a"], &[vec![1.0, 0.0]]));
        let flat_path = temp
            .path()
            .join("derived-indexes")
            .join(&prepared.reader().manifest.flat_file_name);
        let ann_path = temp
            .path()
            .join("derived-indexes")
            .join(&prepared.reader().manifest.ann_file_name);

        assert!(!state
            .publish_from_lifecycle(token, &storage, prepared)
            .unwrap());
        assert!(!state.has_generation());
        assert!(!flat_path.exists());
        assert!(!ann_path.exists());
    }

    #[test]
    fn database_replacement_discards_rebuild_waiting_on_publish_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(test_storage(temp.path()));
        let state = Arc::new(ClipAnnState::default());
        let lifecycle_token = state.lifecycle_token();
        let replacement_guard = storage.derived_generation_publish_guard();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let build_storage = storage.clone();
        let build_state = state.clone();
        let build_path = temp.path().to_path_buf();
        let builder = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _publish_guard = build_storage.derived_generation_publish_guard();
            acquired_tx.send(()).unwrap();
            let prepared =
                PreparedGeneration::new(test_reader(&build_path, 15, &["old"], &[vec![1.0, 0.0]]));
            let flat_path = build_path
                .join("derived-indexes")
                .join(&prepared.reader().manifest.flat_file_name);
            let ann_path = build_path
                .join("derived-indexes")
                .join(&prepared.reader().manifest.ann_file_name);
            let published = build_state
                .publish_from_lifecycle(lifecycle_token, &build_storage, prepared)
                .unwrap();
            (published, flat_path, ann_path)
        });

        started_rx.recv().unwrap();
        assert!(acquired_rx.recv_timeout(Duration::from_millis(50)).is_err());
        state.disarm();
        drop(replacement_guard);

        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (published, flat_path, ann_path) = builder.join().unwrap();
        assert!(!published);
        assert!(!state.has_generation());
        assert!(!flat_path.exists());
        assert!(!ann_path.exists());
        assert!(state.begin_arm().is_some());
    }

    #[test]
    fn reader_switch_waits_for_old_tail_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(test_storage(temp.path()));
        let state = Arc::new(ClipAnnState::default());
        state.install_for_test(test_reader(temp.path(), 20, &["old"], &[vec![1.0, 0.0]]));
        let replacement = test_reader(
            temp.path(),
            21,
            &["old", "new"],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        );

        let (tail_started_tx, tail_started_rx) = std::sync::mpsc::channel();
        let (release_tail_tx, release_tail_rx) = std::sync::mpsc::channel();
        let query_state = state.clone();
        let query_storage = storage.clone();
        let query = std::thread::spawn(move || {
            let (reader, tail) = query_state
                .generation_snapshot_with_tail(&query_storage, |covered_epoch| {
                    tail_started_tx.send(covered_epoch).unwrap();
                    release_tail_rx.recv().unwrap();
                    Ok(vec![DerivedAnnTailRow {
                        subject_key: "new".to_string(),
                        vector: Some(vec![0.0, 1.0]),
                    }])
                })
                .unwrap()
                .unwrap();
            (reader.manifest.generation, tail)
        });

        assert_eq!(tail_started_rx.recv().unwrap(), 1);
        let (install_started_tx, install_started_rx) = std::sync::mpsc::channel();
        let (install_done_tx, install_done_rx) = std::sync::mpsc::channel();
        let install_state = state.clone();
        let installer = std::thread::spawn(move || {
            install_started_tx.send(()).unwrap();
            install_state.install_for_test(replacement);
            install_done_tx.send(()).unwrap();
        });
        install_started_rx.recv().unwrap();
        assert!(install_done_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());

        release_tail_tx.send(()).unwrap();
        let (generation, tail) = query.join().unwrap();
        assert_eq!(generation, 20);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "new");
        install_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        installer.join().unwrap();
        assert_eq!(state.pin_generation().unwrap().manifest.generation, 21);
    }

    #[test]
    fn migration_guard_cannot_overlap_reader_installation() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(test_storage(temp.path()));
        let state = Arc::new(ClipAnnState::default());
        let token = state.begin_arm().unwrap();
        let (reader_ready_tx, reader_ready_rx) = std::sync::mpsc::channel();
        let (install_tx, install_rx) = std::sync::mpsc::channel();
        let (migration_acquired_tx, migration_acquired_rx) = std::sync::mpsc::channel();
        let install_storage = storage.clone();
        let install_state = state.clone();
        let install_path = temp.path().to_path_buf();
        let installer = std::thread::spawn(move || {
            let _publish_guard = install_storage.derived_generation_publish_guard();
            let reader = test_reader(&install_path, 13, &["a"], &[vec![1.0, 0.0]]);
            reader_ready_tx.send(()).unwrap();
            install_rx.recv().unwrap();
            assert!(install_state.install_from_arm(token, &install_storage, reader));
        });

        reader_ready_rx.recv().unwrap();
        let migration_storage = storage.clone();
        let migration = std::thread::spawn(move || {
            let _publish_guard = migration_storage.derived_generation_publish_guard();
            migration_acquired_tx.send(()).unwrap();
        });
        assert!(migration_acquired_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());
        install_tx.send(()).unwrap();
        installer.join().unwrap();
        migration_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        migration.join().unwrap();
        assert!(state.has_generation());
    }

    #[test]
    fn flat_exact_stops_after_the_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let reader = test_reader(
            temp.path(),
            14,
            &["a", "b"],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        );
        let error = exact_candidates(
            &reader,
            &[],
            &[1.0, 0.0],
            1,
            Some(std::time::Instant::now()),
        )
        .unwrap_err();
        assert_eq!(error, "query_deadline_exceeded_during_flat_exact");
    }

    #[test]
    fn disarm_does_not_invalidate_an_in_flight_reader() {
        let temp = tempfile::tempdir().unwrap();
        let flat_path = temp.path().join("flat.cpdvec");
        let ann_path = temp.path().join("graph.cpdann");
        let header = Header::for_snapshot(
            1,
            1,
            1,
            2,
            "clip_image",
            CLIP_MODEL_ID,
            CLIP_VECTOR_SPACE_REVISION,
            96,
            1,
        )
        .unwrap();
        let mut writer = FlatFileWriter::create(&flat_path, header).unwrap();
        writer.push_key("a").unwrap();
        writer.push_vector(&[1.0, 0.0]).unwrap();
        writer.finish().unwrap();
        let flat = MappedFlatIndex::open(&flat_path).unwrap();
        let mut options = usearch::IndexOptions::default();
        options.dimensions = 2;
        options.metric = MetricKind::IP;
        options.quantization = ScalarKind::I8;
        options.connectivity = ANN_CONNECTIVITY as usize;
        options.expansion_add = ANN_EXPANSION_ADD as usize;
        options.expansion_search = 96;
        let built = Index::new(&options).unwrap();
        built.reserve(1).unwrap();
        built.add(1, &[1.0, 0.0]).unwrap();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&ann_path)
            .unwrap();
        file.set_len(built.serialized_length() as u64).unwrap();
        let mut ann_map = unsafe { MmapOptions::new().map_mut(&file) }.unwrap();
        built.save_to_buffer(&mut ann_map).unwrap();
        ann_map.flush().unwrap();
        drop(built);
        let ann_map = ann_map.make_read_only().unwrap();
        let metadata = Index::metadata_from_buffer(&ann_map).unwrap();
        let index = Index::new(&metadata.into()).unwrap();
        unsafe { index.view_from_buffer(&ann_map) }.unwrap();

        let state = ClipAnnState::default();
        state.install_for_test(GenerationReader {
            manifest: DerivedAnnGeneration {
                index_kind: DerivedIndexKind::ClipImage,
                generation: 1,
                covered_epoch: 1,
                flat_file_name: "flat.cpdvec".to_string(),
                flat_checksum_sha256: String::new(),
                ann_file_name: "graph.cpdann".to_string(),
                ann_checksum_sha256: String::new(),
                row_count: 1,
                dimensions: 2,
                model_id: CLIP_MODEL_ID.to_string(),
                model_revision: CLIP_VECTOR_SPACE_REVISION.to_string(),
                embedding_version: CLIP_EMBEDDING_VERSION,
                sidecar_format_version: FORMAT_VERSION,
                ann_format_version: ANN_FILE_FORMAT_VERSION,
                algorithm: ANN_ALGORITHM.to_string(),
                implementation_version: ANN_IMPLEMENTATION_VERSION.to_string(),
                metric: ANN_METRIC.to_string(),
                quantization: ANN_QUANTIZATION.to_string(),
                connectivity: ANN_CONNECTIVITY,
                expansion_add: ANN_EXPANSION_ADD,
                expansion_search: 96,
                created_at: String::new(),
            },
            flat,
            data_dir: temp.path().to_path_buf(),
            index,
            _ann_map: ann_map,
            _ann_file: file,
        });
        let pinned = state.pin_generation().unwrap();
        state.disarm();
        assert!(!state.has_generation());
        assert_eq!(pinned.flat.key(0).unwrap(), "a");
        assert_eq!(pinned.index.search(&[1.0, 0.0], 1).unwrap().keys, vec![1]);
    }

    #[test]
    fn restored_view_reapplies_expansion_search() {
        let mut options = usearch::IndexOptions::default();
        options.dimensions = 2;
        options.metric = MetricKind::IP;
        options.quantization = ScalarKind::I8;
        let index = Index::new(&options).unwrap();
        apply_expansion_search(&index, 777).unwrap();
        assert_eq!(index.expansion_search(), 777);
    }
}
