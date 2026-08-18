//! Supervision, latest-only scheduling, caching, and screenshot association for Office.

use crate::office_protocol::{
    read_response, write_request, OfficeApplication, OfficeDocumentKind, OfficeDocumentRef,
    OfficeRequest, OfficeResponse, OFFICE_PROTOCOL_VERSION,
};
use crate::office_window::{find_native_document_window, validate_office_window_context};
use crate::resource_utils::find_existing_file_in_resources;
use crate::storage::{StorageState, STALE_DOCUMENT_REF_GENERATION};
use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, BufWriter};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(2500);
const SUCCESS_CACHE_TTL: Duration = Duration::from_secs(60);
const EMPTY_CACHE_TTL: Duration = Duration::from_secs(15);
const FAILURE_CACHE_BASE_TTL: Duration = Duration::from_secs(2);
const FAILURE_CACHE_MAX_TTL: Duration = Duration::from_secs(60);
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_PENDING_SCREENSHOTS: usize = 64;
/// Polling step while draining, matching the in-flight OCR wait in
/// `monitor.rs::stop_monitor_impl`.
const QUIESCE_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct OfficeWindowFingerprint {
    application: OfficeApplication,
    root_hwnd: i64,
    document_hwnd: i64,
    pid: u32,
    title: String,
}

/// Opaque token carried from foreground observation to screenshot persistence.
#[derive(Clone, Debug)]
pub struct OfficeCaptureContext {
    fingerprint: OfficeWindowFingerprint,
    generation: u64,
}

impl OfficeCaptureContext {
    pub fn document_hwnd(&self) -> i64 {
        self.fingerprint.document_hwnd
    }
}

#[derive(Clone, Debug)]
struct QueuedObservation {
    generation: u64,
    fingerprint: OfficeWindowFingerprint,
}

/// A screenshot waiting for the resolution of the window it was taken from.
///
/// The database generation is captured when the id is handed over, not when
/// the write finally happens: an id only identifies a screenshot within the
/// database it was issued by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingScreenshot {
    id: i64,
    db_generation: u64,
}

#[derive(Clone, Debug)]
struct PendingAssociation {
    fingerprint: OfficeWindowFingerprint,
    resolution: PendingResolution,
    screenshot: PendingScreenshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingResolution {
    Awaiting(u64),
    DeferredFailure,
}

/// Admission control for the observation pipeline.
///
/// `active` and `resolving` share one mutex so that `quiesce` cannot close the
/// gate in the window between the consumer reading `active` and announcing that
/// it started a resolution — that gap would let a resolution, and the write
/// that follows it, begin after the drain reported success.
#[derive(Default)]
struct RuntimeGate {
    active: bool,
    resolving: bool,
}

/// Holds the drain open for one association write.
///
/// Created while the observation lock is held, so a write is always counted
/// before the state that led to it becomes invisible; released when the
/// blocking task ends, including on panic.
struct PendingWriteGuard(Arc<AtomicU32>);

impl PendingWriteGuard {
    fn new(counter: &Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for PendingWriteGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Marks a resolution as running for as long as it runs, unwinding included.
/// A resolution that ended without clearing the flag would make every later
/// drain wait out its full timeout.
struct ResolutionGuard<'a>(&'a Mutex<RuntimeGate>);

impl Drop for ResolutionGuard<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .resolving = false;
    }
}

#[derive(Clone, Debug)]
enum CachedResolutionOutcome {
    Document(OfficeDocumentRef),
    ConfirmedEmpty,
    TransientFailure,
}

struct CachedResolution {
    generation: u64,
    outcome: CachedResolutionOutcome,
    refreshed_at: Instant,
    valid_for: Duration,
    consecutive_failures: u32,
}

impl CachedResolution {
    fn is_fresh(&self) -> bool {
        self.refreshed_at.elapsed() < self.valid_for
    }
}

struct ObservationState {
    latest_generation: u64,
    last_observed: Option<OfficeWindowFingerprint>,
    active_query: Option<QueuedObservation>,
    cache: HashMap<OfficeWindowFingerprint, CachedResolution>,
    pending_screenshots: VecDeque<PendingAssociation>,
}

enum QueueUpdate {
    None,
    Invalidate,
    Resolve(QueuedObservation),
}

struct OfficeChild {
    child: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    request_lock: Mutex<()>,
    _job: OfficeJobHandle,
}

impl OfficeChild {
    fn request(&self, request: &OfficeRequest) -> Result<OfficeResponse, String> {
        let _request_guard = self
            .request_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            let mut stdin = self.stdin.lock().unwrap_or_else(|error| error.into_inner());
            write_request(&mut *stdin, request)?;
        }
        let mut stdout = self
            .stdout
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        read_response(&mut *stdout)
    }

    fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

struct PendingOfficeChild(Option<Child>);

impl PendingOfficeChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child(&self) -> &Child {
        self.0.as_ref().expect("pending Office child is available")
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("pending Office child is available")
    }

    fn take(&mut self) -> Child {
        self.0.take().expect("pending Office child is available")
    }
}

impl Drop for PendingOfficeChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct OfficeJobHandle(HANDLE);

impl Drop for OfficeJobHandle {
    fn drop(&mut self) {
        // SAFETY: the RAII wrapper owns this Job Object handle exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: Job Object handles are thread-safe opaque kernel references; the
// wrapper enforces single-close ownership.
unsafe impl Send for OfficeJobHandle {}
// SAFETY: shared access does not expose mutable Rust memory.
unsafe impl Sync for OfficeJobHandle {}

struct SupervisorState {
    process: Option<Arc<OfficeChild>>,
    worker_version: Option<String>,
    restart_count: u64,
    last_error: Option<String>,
}

struct OfficeResolution {
    generation: u64,
    document_hwnd: i64,
    document: Option<OfficeDocumentRef>,
}

pub struct OfficeRuntimeState {
    observations: Mutex<ObservationState>,
    sender: tokio::sync::watch::Sender<Option<QueuedObservation>>,
    receiver: Mutex<Option<tokio::sync::watch::Receiver<Option<QueuedObservation>>>>,
    supervisor: Mutex<SupervisorState>,
    lifecycle_lock: Mutex<()>,
    request_gate: tokio::sync::Mutex<()>,
    next_request_id: AtomicU64,
    /// Whether observation, resolution and association are currently accepted.
    /// Lock order: acquired before `observations`, never after.
    gate: Mutex<RuntimeGate>,
    /// Association writes handed to the blocking pool but not yet finished.
    in_flight_writes: Arc<AtomicU32>,
}

impl Default for OfficeRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl OfficeRuntimeState {
    pub fn new() -> Self {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        Self {
            observations: Mutex::new(ObservationState {
                latest_generation: 0,
                last_observed: None,
                active_query: None,
                cache: HashMap::new(),
                pending_screenshots: VecDeque::new(),
            }),
            sender,
            receiver: Mutex::new(Some(receiver)),
            supervisor: Mutex::new(SupervisorState {
                process: None,
                worker_version: None,
                restart_count: 0,
                last_error: None,
            }),
            lifecycle_lock: Mutex::new(()),
            request_gate: tokio::sync::Mutex::new(()),
            next_request_id: AtomicU64::new(1),
            // Observation follows the capture loop: the gate opens when the
            // monitor starts, not when the runtime is constructed.
            gate: Mutex::new(RuntimeGate {
                active: false,
                resolving: false,
            }),
            in_flight_writes: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Start the single latest-only observation consumer.
    pub fn start(self: &Arc<Self>, app: AppHandle, storage: Arc<StorageState>) {
        let receiver = self
            .receiver
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some(mut receiver) = receiver else {
            return;
        };
        let runtime = self.clone();
        tauri::async_runtime::spawn(async move {
            while receiver.changed().await.is_ok() {
                let observation = receiver.borrow_and_update().clone();
                let Some(observation) = observation else {
                    continue;
                };
                let Some(resolution_guard) = runtime.begin_resolution() else {
                    continue;
                };
                let result = runtime.resolve(&app, &observation).await;
                runtime.complete_observation(app.clone(), storage.clone(), observation, result);
                drop(resolution_guard);
            }
        });
    }

    /// Reopen the gate closed by [`OfficeRuntimeState::quiesce`].
    ///
    /// Called when the capture loop is (re)started; observation only makes
    /// sense while screenshots are being taken.
    pub fn resume(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        gate.active = true;
    }

    pub fn is_active(&self) -> bool {
        self.gate
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
    }

    /// Stop observing, let in-flight resolutions and writes finish, then
    /// release the worker process.
    ///
    /// Must be awaited before the database is closed or replaced — by
    /// `stop_monitor_impl`, by backup export/import, and by the
    /// data-directory switch. Association writes carry screenshot ids that
    /// only mean something in the database they came from, and the worker has
    /// no reason to keep talking to Office once capture has stopped.
    ///
    /// Returns whether everything drained within `timeout`; on timeout the
    /// remaining writes are still rejected by the generation check in
    /// `storage/document_ref.rs`.
    pub async fn quiesce(&self, timeout: Duration) -> bool {
        {
            let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
            gate.active = false;
        }
        self.sender.send_replace(None);

        let deadline = tokio::time::Instant::now() + timeout;
        let drained = loop {
            let resolving = self
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .resolving;
            let writes = self.in_flight_writes.load(Ordering::SeqCst);
            if !resolving && writes == 0 {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    "[OFFICE:LIFECYCLE] drain timed out (resolving={}, pending writes={})",
                    resolving,
                    writes
                );
                break false;
            }
            tokio::time::sleep(QUIESCE_POLL_INTERVAL).await;
        };

        // Everything here belongs to the database that is about to go away:
        // pending ids, and cached resolutions keyed by windows whose next
        // screenshots will be numbered by a different database.
        {
            let mut state = self
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.last_observed = None;
            state.active_query = None;
            state.cache.clear();
            state.pending_screenshots.clear();
        }

        let process = {
            let mut supervisor = self
                .supervisor
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            supervisor.worker_version = None;
            supervisor.process.take()
        };
        if let Some(process) = process {
            tracing::info!("[OFFICE:LIFECYCLE] releasing worker after drain");
            if let Err(error) = tokio::task::spawn_blocking(move || process.kill()).await {
                tracing::warn!("[OFFICE:LIFECYCLE] worker shutdown task failed: {}", error);
            }
        }

        drained
    }

    /// Announce a resolution while the gate is open, so a concurrent drain
    /// waits for it. Returns `None` when the gate has already closed.
    fn begin_resolution(&self) -> Option<ResolutionGuard<'_>> {
        let mut gate = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        if !gate.active {
            return None;
        }
        gate.resolving = true;
        Some(ResolutionGuard(&self.gate))
    }

    /// Observe a foreground Office window without issuing COM on the caller thread.
    pub fn observe_window(
        &self,
        application: OfficeApplication,
        root_hwnd: i64,
        pid: u32,
        title: &str,
    ) -> Option<OfficeCaptureContext> {
        if !self.is_active() {
            return None;
        }
        let document_hwnd = find_native_document_window(root_hwnd, pid, application)?;
        let fingerprint = OfficeWindowFingerprint {
            application,
            root_hwnd,
            document_hwnd,
            pid,
            title: title.to_string(),
        };

        let (context, update) = {
            let mut state = self
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut update = QueueUpdate::None;

            if state.last_observed.as_ref() != Some(&fingerprint) {
                state.last_observed = Some(fingerprint.clone());
                if state
                    .active_query
                    .as_ref()
                    .is_some_and(|query| query.fingerprint != fingerprint)
                {
                    // The query is stale only for latest-window caching. Any
                    // screenshot already issued against this generation still
                    // owns its eventual result and must remain pending.
                    state.active_query = None;
                    next_generation(&mut state);
                    update = QueueUpdate::Invalidate;
                }
            }

            if let Some(active) = state
                .active_query
                .as_ref()
                .filter(|query| query.fingerprint == fingerprint)
            {
                (
                    OfficeCaptureContext {
                        fingerprint,
                        generation: active.generation,
                    },
                    update,
                )
            } else if let Some(cached) = state
                .cache
                .get(&fingerprint)
                .filter(|entry| entry.is_fresh())
            {
                (
                    OfficeCaptureContext {
                        fingerprint,
                        generation: cached.generation,
                    },
                    update,
                )
            } else {
                let generation = next_generation(&mut state);
                let observation = QueuedObservation {
                    generation,
                    fingerprint: fingerprint.clone(),
                };
                state.active_query = Some(observation.clone());
                (
                    OfficeCaptureContext {
                        fingerprint,
                        generation,
                    },
                    // A stale in-flight query may have set `update` to
                    // `Invalidate` above. The new observation must replace it;
                    // otherwise the watch channel is cleared without ever
                    // scheduling the newly active Office window.
                    QueueUpdate::Resolve(observation),
                )
            }
        };

        match update {
            QueueUpdate::None => {}
            QueueUpdate::Invalidate => {
                self.sender.send_replace(None);
            }
            QueueUpdate::Resolve(observation) => {
                self.sender.send_replace(Some(observation));
            }
        }
        Some(context)
    }

    /// Associate a newly persisted screenshot with the matching cached or pending result.
    ///
    /// `db_generation_before_save` is the generation the caller read just
    /// before the screenshot row was written; see `capture.rs`.
    pub fn associate_screenshot(
        &self,
        app: AppHandle,
        storage: Arc<StorageState>,
        screenshot_id: i64,
        context: OfficeCaptureContext,
        db_generation_before_save: u64,
    ) {
        if screenshot_id <= 0 || !self.is_active() {
            return;
        }
        // The insert sits between the caller's reading and this one. If they
        // agree, no swap crossed it and the id belongs to the database that is
        // open now; if they disagree, which database issued the id is unknown
        // and the association has to be dropped.
        if storage.db_generation() != db_generation_before_save {
            tracing::info!(
                "[OFFICE:STORAGE] dropped association for screenshot {}: storage changed while it was saved",
                screenshot_id
            );
            return;
        }
        let pending = PendingScreenshot {
            id: screenshot_id,
            db_generation: db_generation_before_save,
        };
        let persist = {
            let mut state = self
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(cached) = state
                .cache
                .get(&context.fingerprint)
                .filter(|entry| entry.generation == context.generation && entry.is_fresh())
            {
                match &cached.outcome {
                    CachedResolutionOutcome::Document(document) => Some((
                        document.clone(),
                        PendingWriteGuard::new(&self.in_flight_writes),
                    )),
                    CachedResolutionOutcome::TransientFailure => {
                        enqueue_pending_screenshot(
                            &mut state,
                            context.fingerprint.clone(),
                            PendingResolution::DeferredFailure,
                            pending,
                        );
                        None
                    }
                    CachedResolutionOutcome::ConfirmedEmpty => None,
                }
            } else if state.active_query.as_ref().is_some_and(|query| {
                query.generation == context.generation && query.fingerprint == context.fingerprint
            }) {
                enqueue_pending_screenshot(
                    &mut state,
                    context.fingerprint.clone(),
                    PendingResolution::Awaiting(context.generation),
                    pending,
                );
                None
            } else {
                None
            }
        };

        if let Some((document, write_guard)) = persist {
            persist_document_refs(app, storage, vec![pending], document, write_guard);
        }
    }

    fn complete_observation(
        &self,
        app: AppHandle,
        storage: Arc<StorageState>,
        observation: QueuedObservation,
        result: Result<OfficeResolution, String>,
    ) {
        let outcome = match &result {
            Ok(resolution)
                if resolution.generation == observation.generation
                    && resolution.document_hwnd == observation.fingerprint.document_hwnd =>
            {
                match &resolution.document {
                    Some(document) => CachedResolutionOutcome::Document(document.clone()),
                    None => CachedResolutionOutcome::ConfirmedEmpty,
                }
            }
            _ => CachedResolutionOutcome::TransientFailure,
        };
        let mut document_to_persist = None;
        let mut write_guard = None;
        let pending_ids = {
            let mut state = self
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let is_current = state.active_query.as_ref().is_some_and(|active| {
                active.generation == observation.generation
                    && active.fingerprint == observation.fingerprint
                    && state.latest_generation == observation.generation
            });
            let pending = match &outcome {
                CachedResolutionOutcome::Document(document) => {
                    let pending = take_pending_screenshots(
                        &mut state.pending_screenshots,
                        &observation.fingerprint,
                        observation.generation,
                    );
                    if !pending.is_empty() {
                        // Counted before the lock is released, so a drain that
                        // starts right now still waits for this write.
                        write_guard = Some(PendingWriteGuard::new(&self.in_flight_writes));
                        document_to_persist = Some(document.clone());
                    }
                    pending
                }
                CachedResolutionOutcome::ConfirmedEmpty => {
                    let pending = take_pending_screenshots(
                        &mut state.pending_screenshots,
                        &observation.fingerprint,
                        observation.generation,
                    );
                    if !pending.is_empty() {
                        tracing::debug!(
                            "[OFFICE:CACHE] discarded {} pending associations after confirming no document",
                            pending.len()
                        );
                    }
                    pending
                }
                CachedResolutionOutcome::TransientFailure => {
                    defer_pending_screenshots(
                        &mut state.pending_screenshots,
                        &observation.fingerprint,
                        observation.generation,
                    );
                    Vec::new()
                }
            };

            if !is_current {
                tracing::debug!(
                    "[OFFICE:CACHE] ignored stale generation {} for cache; completing {} pending associations",
                    observation.generation,
                    pending.len()
                );
            } else {
                state.active_query = None;
                let previous_failures = state
                    .cache
                    .get(&observation.fingerprint)
                    .map(|cached| cached.consecutive_failures)
                    .unwrap_or(0);
                let (outcome, valid_for, consecutive_failures) = match outcome {
                    CachedResolutionOutcome::Document(document) => (
                        CachedResolutionOutcome::Document(document),
                        SUCCESS_CACHE_TTL,
                        0,
                    ),
                    CachedResolutionOutcome::ConfirmedEmpty => {
                        (CachedResolutionOutcome::ConfirmedEmpty, EMPTY_CACHE_TTL, 0)
                    }
                    CachedResolutionOutcome::TransientFailure => {
                        if result.is_ok() {
                            tracing::debug!(
                                "[OFFICE:CACHE] document HWND changed during generation {}",
                                observation.generation
                            );
                        } else if let Err(error) = &result {
                            tracing::warn!(
                                "[OFFICE:RESOLVE] generation {} failed: {}",
                                observation.generation,
                                error
                            );
                        }
                        let failures = previous_failures.saturating_add(1);
                        (
                            CachedResolutionOutcome::TransientFailure,
                            failure_cache_ttl(failures),
                            failures,
                        )
                    }
                };
                insert_cache_entry(
                    &mut state,
                    observation.fingerprint,
                    CachedResolution {
                        generation: observation.generation,
                        outcome,
                        refreshed_at: Instant::now(),
                        valid_for,
                        consecutive_failures,
                    },
                );
            }
            pending
        };

        if let (Some(document), Some(write_guard)) = (document_to_persist, write_guard) {
            persist_document_refs(app, storage, pending_ids, document, write_guard);
        }
    }

    async fn resolve(
        self: &Arc<Self>,
        app: &AppHandle,
        observation: &QueuedObservation,
    ) -> Result<OfficeResolution, String> {
        let _request_guard = self.request_gate.lock().await;
        let runtime = self.clone();
        let app_for_start = app.clone();
        let process = tokio::task::spawn_blocking(move || runtime.ensure_process(&app_for_start))
            .await
            .map_err(|error| format!("Office worker startup task failed: {error}"))??;

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let request = OfficeRequest::Resolve {
            request_id,
            generation: observation.generation,
            application: observation.fingerprint.application,
            root_hwnd: observation.fingerprint.root_hwnd,
            document_hwnd: observation.fingerprint.document_hwnd,
            pid: observation.fingerprint.pid,
            title: observation.fingerprint.title.clone(),
        };
        let process_for_request = process.clone();
        let request_task =
            tokio::task::spawn_blocking(move || process_for_request.request(&request));
        let response = match tokio::time::timeout(REQUEST_TIMEOUT, request_task).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => Err(format!("Office request task failed: {error}")),
            Err(_) => {
                tracing::error!(
                    "[OFFICE:WATCHDOG] request {} exceeded {} ms; killing worker",
                    request_id,
                    REQUEST_TIMEOUT.as_millis()
                );
                process.kill();
                self.clear_process(&process, "watchdog_timeout");
                return Err(format!(
                    "Office watchdog timeout after {} ms",
                    REQUEST_TIMEOUT.as_millis()
                ));
            }
        };

        match response {
            Ok(OfficeResponse::Resolved {
                request_id: response_id,
                generation,
                document_hwnd,
                document,
                ..
            }) if response_id == request_id && generation == observation.generation => {
                if document_hwnd != observation.fingerprint.document_hwnd {
                    return Err(format!(
                        "Office worker returned unexpected document HWND {}",
                        document_hwnd
                    ));
                }
                if !validate_office_window_context(
                    observation.fingerprint.root_hwnd,
                    observation.fingerprint.document_hwnd,
                    observation.fingerprint.pid,
                    observation.fingerprint.application,
                    &observation.fingerprint.title,
                ) {
                    return Err("Office window context changed after COM resolution".to_string());
                }
                if document.as_ref().is_some_and(|reference| {
                    reference.application != observation.fingerprint.application
                }) {
                    return Err(
                        "Office worker returned a document for the wrong application".to_string(),
                    );
                }
                Ok(OfficeResolution {
                    generation,
                    document_hwnd,
                    document,
                })
            }
            Ok(OfficeResponse::Error {
                request_id: response_id,
                generation,
                kind,
                message,
                ..
            }) if response_id == request_id && generation == Some(observation.generation) => {
                if kind == "panic" {
                    process.kill();
                    self.clear_process(&process, "worker_panic");
                }
                Err(format!("Office {kind}: {message}"))
            }
            Ok(other) => {
                let error = format!(
                    "unexpected Office response for request {request_id}: {}",
                    response_kind(&other)
                );
                process.kill();
                self.clear_process(&process, "protocol_mismatch");
                Err(error)
            }
            Err(error) => {
                process.kill();
                self.clear_process(&process, "transport_failure");
                Err(error)
            }
        }
    }

    fn ensure_process(&self, app: &AppHandle) -> Result<Arc<OfficeChild>, String> {
        let _lifecycle_guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let existing = self
            .supervisor
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .process
            .clone();
        if let Some(process) = existing {
            let running = process
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .try_wait()
                .map_err(|error| format!("failed to inspect Office worker: {error}"))?
                .is_none();
            if running {
                return Ok(process);
            }
            let mut supervisor = self
                .supervisor
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if supervisor
                .process
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &process))
            {
                supervisor.process = None;
                supervisor.restart_count += 1;
            }
        }
        self.start_process(app)
    }

    fn start_process(&self, app: &AppHandle) -> Result<Arc<OfficeChild>, String> {
        let executable = resolve_office_executable(app)?;
        let mut command = Command::new(&executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        tracing::info!(
            "[OFFICE:SUPERVISOR] starting worker path={}",
            executable.display()
        );
        let child = command
            .spawn()
            .map_err(|error| format!("failed to start Office worker: {error}"))?;
        let mut pending = PendingOfficeChild::new(child);
        let job = assign_kill_on_close_job(pending.child())?;
        let stdin = pending
            .child_mut()
            .stdin
            .take()
            .ok_or("Office worker stdin unavailable")?;
        let stdout = pending
            .child_mut()
            .stdout
            .take()
            .ok_or("Office worker stdout unavailable")?;
        if let Some(stderr) = pending.child_mut().stderr.take() {
            std::thread::Builder::new()
                .name("carbonpaper-office-log".to_string())
                .spawn(move || {
                    use std::io::BufRead;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        tracing::info!("[OFFICE:WORKER] {line}");
                    }
                })
                .map_err(|error| format!("failed to start Office log reader: {error}"))?;
        }

        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("carbonpaper-office-handshake".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_response(&mut reader);
                let _ = ready_sender.send((ready, reader));
            })
            .map_err(|error| format!("failed to start Office handshake reader: {error}"))?;
        let (ready, stdout) = ready_receiver
            .recv_timeout(STARTUP_TIMEOUT)
            .map_err(|_| "Office worker startup timed out".to_string())?;

        let child = pending.take();
        let process = Arc::new(OfficeChild {
            child: Mutex::new(child),
            stdin: Mutex::new(BufWriter::new(stdin)),
            stdout: Mutex::new(stdout),
            request_lock: Mutex::new(()),
            _job: job,
        });
        match ready {
            Ok(OfficeResponse::Ready {
                protocol_version,
                worker_version,
            }) if protocol_version == OFFICE_PROTOCOL_VERSION => {
                let mut supervisor = self
                    .supervisor
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                supervisor.process = Some(process.clone());
                supervisor.worker_version = Some(worker_version);
                supervisor.last_error = None;
                tracing::info!("[OFFICE:SUPERVISOR] worker ready");
                Ok(process)
            }
            Ok(other) => {
                process.kill();
                Err(format!(
                    "invalid Office worker handshake: {}",
                    response_kind(&other)
                ))
            }
            Err(error) => {
                process.kill();
                Err(format!("Office worker startup failed: {error}"))
            }
        }
    }

    fn clear_process(&self, process: &Arc<OfficeChild>, reason: &str) {
        let mut supervisor = self
            .supervisor
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if supervisor
            .process
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, process))
        {
            supervisor.process = None;
            supervisor.restart_count += 1;
            supervisor.last_error = Some(reason.to_string());
        }
    }
}

fn response_kind(response: &OfficeResponse) -> &'static str {
    match response {
        OfficeResponse::Ready { .. } => "ready",
        OfficeResponse::Pong { .. } => "pong",
        OfficeResponse::Resolved { .. } => "resolved",
        OfficeResponse::Error { .. } => "error",
        OfficeResponse::ShuttingDown { .. } => "shutting_down",
    }
}

fn next_generation(state: &mut ObservationState) -> u64 {
    state.latest_generation = state.latest_generation.wrapping_add(1).max(1);
    state.latest_generation
}

fn failure_cache_ttl(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    let multiplier = 1u32 << exponent;
    FAILURE_CACHE_BASE_TTL
        .saturating_mul(multiplier)
        .min(FAILURE_CACHE_MAX_TTL)
}

fn enqueue_pending_screenshot(
    state: &mut ObservationState,
    fingerprint: OfficeWindowFingerprint,
    resolution: PendingResolution,
    screenshot: PendingScreenshot,
) {
    if state.pending_screenshots.iter().any(|entry| {
        entry.screenshot.id == screenshot.id
            && entry.screenshot.db_generation == screenshot.db_generation
    }) {
        return;
    }

    if state.pending_screenshots.len() >= MAX_PENDING_SCREENSHOTS {
        if let Some(evicted) = state.pending_screenshots.pop_front() {
            tracing::warn!(
                "[OFFICE:CACHE] pending association limit reached; evicted screenshot {}",
                evicted.screenshot.id
            );
        }
    }
    state.pending_screenshots.push_back(PendingAssociation {
        fingerprint,
        resolution,
        screenshot,
    });
}

fn defer_pending_screenshots(
    queue: &mut VecDeque<PendingAssociation>,
    fingerprint: &OfficeWindowFingerprint,
    generation: u64,
) {
    for entry in queue {
        if &entry.fingerprint == fingerprint
            && entry.resolution == PendingResolution::Awaiting(generation)
        {
            entry.resolution = PendingResolution::DeferredFailure;
        }
    }
}

fn take_pending_screenshots(
    queue: &mut VecDeque<PendingAssociation>,
    fingerprint: &OfficeWindowFingerprint,
    generation: u64,
) -> Vec<PendingScreenshot> {
    let mut pending = Vec::new();
    queue.retain(|entry| {
        let belongs_to_resolution = match entry.resolution {
            PendingResolution::DeferredFailure => true,
            PendingResolution::Awaiting(active) => active == generation,
        };
        if &entry.fingerprint == fingerprint && belongs_to_resolution {
            pending.push(entry.screenshot);
            false
        } else {
            true
        }
    });
    pending
}

fn insert_cache_entry(
    state: &mut ObservationState,
    fingerprint: OfficeWindowFingerprint,
    entry: CachedResolution,
) {
    if state.cache.len() >= MAX_CACHE_ENTRIES && !state.cache.contains_key(&fingerprint) {
        if let Some(oldest) = state
            .cache
            .iter()
            .min_by_key(|(_, cached)| cached.refreshed_at)
            .map(|(key, _)| key.clone())
        {
            state.cache.remove(&oldest);
        }
    }
    state.cache.insert(fingerprint, entry);
}

fn persist_document_refs(
    app: AppHandle,
    storage: Arc<StorageState>,
    screenshots: Vec<PendingScreenshot>,
    document: OfficeDocumentRef,
    write_guard: PendingWriteGuard,
) {
    if screenshots.is_empty() {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        // Released when this task ends, however it ends.
        let _write_guard = write_guard;
        for screenshot in screenshots {
            match storage.save_screenshot_document_ref_for_generation(
                screenshot.id,
                &document,
                screenshot.db_generation,
            ) {
                Ok(()) => {
                    if let Err(error) = app.emit(
                        "office-document-ref-updated",
                        serde_json::json!({ "screenshot_id": screenshot.id }),
                    ) {
                        tracing::debug!(
                            "[OFFICE:UI] failed to emit document update for {}: {}",
                            screenshot.id,
                            error
                        );
                    }
                }
                // Not a failure: the database this id belongs to is gone, so
                // the association has nowhere meaningful to land.
                Err(error) if error == STALE_DOCUMENT_REF_GENERATION => {
                    tracing::info!(
                        "[OFFICE:STORAGE] dropped association for screenshot {} after a storage switch",
                        screenshot.id
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        "[OFFICE:STORAGE] failed to associate screenshot {}: {}",
                        screenshot.id,
                        error
                    );
                }
            }
        }
    });
}

fn resolve_office_executable(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(path) = find_existing_file_in_resources(app, "carbonpaper-office.exe") {
        return Ok(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            let sibling = directory.join("carbonpaper-office.exe");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("carbonpaper-office.exe");
    if development.exists() {
        return Ok(development);
    }
    Err("carbonpaper-office.exe was not found; build or reinstall CarbonPaper".to_string())
}

fn assign_kill_on_close_job(child: &Child) -> Result<OfficeJobHandle, String> {
    // SAFETY: structure sizes and handles match the Job Object API contract;
    // every error path closes the newly created handle.
    unsafe {
        let handle = CreateJobObjectW(None, None)
            .map_err(|error| format!("failed to create Office job object: {error:?}"))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(error) = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(handle);
            return Err(format!("failed to configure Office job object: {error:?}"));
        }
        let process_handle = HANDLE(child.as_raw_handle() as *mut _);
        if let Err(error) = AssignProcessToJobObject(handle, process_handle) {
            let _ = CloseHandle(handle);
            return Err(format!(
                "failed to assign Office worker to job object: {error:?}"
            ));
        }
        Ok(OfficeJobHandle(handle))
    }
}

fn resume_target(document: &OfficeDocumentRef) -> Result<String, String> {
    let locator = document
        .locator
        .as_deref()
        .ok_or_else(|| "OFFICE_DOCUMENT_UNSAVED".to_string())?;
    match document.kind {
        OfficeDocumentKind::Unsaved => Err("OFFICE_DOCUMENT_UNSAVED".to_string()),
        OfficeDocumentKind::LocalFile => {
            if !std::path::Path::new(locator).is_file() {
                Err("OFFICE_DOCUMENT_MISSING".to_string())
            } else {
                Ok(locator.to_string())
            }
        }
        OfficeDocumentKind::CloudDocument => {
            let scheme = match document.application {
                OfficeApplication::Word => "ms-word:ofe|u|",
                OfficeApplication::Excel => "ms-excel:ofe|u|",
                OfficeApplication::PowerPoint => "ms-powerpoint:ofe|u|",
            };
            Ok(format!("{scheme}{locator}"))
        }
    }
}

/// Open the exact saved Office document associated with an authenticated screenshot.
#[tauri::command]
pub async fn office_resume_document(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    storage: tauri::State<'_, Arc<StorageState>>,
    screenshot_id: i64,
) -> Result<serde_json::Value, String> {
    if window.label() != "main" && window.label() != "snapshot-preview" {
        return Err("WINDOW_NOT_AUTHORIZED".to_string());
    }
    crate::commands::check_auth_required(&credential_state)?;
    if screenshot_id <= 0 {
        return Err("Invalid screenshot id".to_string());
    }
    let storage = storage.inner().clone();
    tokio::task::spawn_blocking(move || {
        let document = storage
            .get_screenshot_document_ref(screenshot_id)?
            .ok_or_else(|| "OFFICE_DOCUMENT_NOT_FOUND".to_string())?;
        let target = resume_target(&document)?;
        open::that_detached(&target)
            .map_err(|error| format!("Failed to resume Office document: {error}"))?;
        Ok(serde_json::json!({
            "status": "opened",
            "application": document.application,
            "display_name": document.display_name
        }))
    })
    .await
    .map_err(|error| format!("Office resume task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> OfficeWindowFingerprint {
        OfficeWindowFingerprint {
            application: OfficeApplication::Word,
            root_hwnd: 11,
            document_hwnd: 22,
            pid: 33,
            title: "Report.docx - Word".to_string(),
        }
    }

    #[test]
    fn resolution_is_refused_until_capture_starts() {
        let runtime = OfficeRuntimeState::new();
        assert!(!runtime.is_active(), "the gate opens with the capture loop");
        assert!(runtime.begin_resolution().is_none());

        runtime.resume();
        assert!(runtime.is_active());
        {
            let _resolving = runtime.begin_resolution().expect("gate is open");
            assert!(
                runtime
                    .gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .resolving
            );
        }
        assert!(
            !runtime
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .resolving,
            "the flag must clear even when the resolution unwinds"
        );
    }

    #[tokio::test]
    async fn draining_waits_for_an_association_write() {
        let runtime = OfficeRuntimeState::new();
        runtime.resume();

        let write = PendingWriteGuard::new(&runtime.in_flight_writes);
        assert!(
            !runtime.quiesce(Duration::from_millis(250)).await,
            "a write still in flight must hold the drain open"
        );

        drop(write);
        assert!(runtime.quiesce(Duration::from_millis(250)).await);
    }

    #[tokio::test]
    async fn draining_discards_state_tied_to_the_old_database() {
        let runtime = OfficeRuntimeState::new();
        runtime.resume();
        {
            let mut state = runtime
                .observations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.last_observed = Some(fingerprint());
            state.active_query = Some(QueuedObservation {
                generation: 1,
                fingerprint: fingerprint(),
            });
            state.pending_screenshots.push_back(PendingAssociation {
                fingerprint: fingerprint(),
                resolution: PendingResolution::Awaiting(1),
                screenshot: PendingScreenshot {
                    id: 42,
                    db_generation: 7,
                },
            });
            state.cache.insert(
                fingerprint(),
                CachedResolution {
                    generation: 1,
                    outcome: CachedResolutionOutcome::ConfirmedEmpty,
                    refreshed_at: Instant::now(),
                    valid_for: SUCCESS_CACHE_TTL,
                    consecutive_failures: 0,
                },
            );
        }

        assert!(runtime.quiesce(Duration::from_millis(250)).await);
        assert!(!runtime.is_active());

        let state = runtime
            .observations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(state.pending_screenshots.is_empty());
        assert!(state.cache.is_empty());
        assert!(state.active_query.is_none());
        assert!(state.last_observed.is_none());
    }

    #[test]
    fn cloud_resume_uses_the_office_protocol_handler() {
        let document = OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::PowerPoint,
            kind: OfficeDocumentKind::CloudDocument,
            display_name: "Roadmap.pptx".to_string(),
            locator: Some("https://tenant.example/Roadmap.pptx".to_string()),
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        };
        assert_eq!(
            resume_target(&document).unwrap(),
            "ms-powerpoint:ofe|u|https://tenant.example/Roadmap.pptx"
        );
    }

    #[test]
    fn unsaved_documents_cannot_be_resumed() {
        let document = OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::Word,
            kind: OfficeDocumentKind::Unsaved,
            display_name: "Document1".to_string(),
            locator: None,
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        };
        assert_eq!(
            resume_target(&document).unwrap_err(),
            "OFFICE_DOCUMENT_UNSAVED"
        );
    }

    #[test]
    fn office_failure_backoff_is_bounded() {
        assert_eq!(failure_cache_ttl(1), Duration::from_secs(2));
        assert_eq!(failure_cache_ttl(2), Duration::from_secs(4));
        assert_eq!(failure_cache_ttl(5), Duration::from_secs(32));
        assert_eq!(failure_cache_ttl(6), Duration::from_secs(60));
        assert_eq!(failure_cache_ttl(u32::MAX), Duration::from_secs(60));
    }
}
