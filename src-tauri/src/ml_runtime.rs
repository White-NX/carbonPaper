use crate::ml_protocol::{
    read_response, write_request, MlOcrBlock, MlOcrTimings, MlProvider, MlRequest, MlResponse,
    ML_PROTOCOL_VERSION,
};
use crate::resource_utils::{
    file_in_local_appdata, file_in_resources, find_existing_file_in_resources,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{BufReader, BufWriter};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const MODEL_STATUS_CACHE_TTL: Duration = Duration::from_secs(60);
const MODEL_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MODEL_DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MODEL_ID: &str = "ppocrv5-ch-mobile";
const MODEL_REVISION: &str = "r1";
const BUNDLED_MODEL_RELATIVE_DIR: &str = "ocr-models/ppocrv5-ch-mobile-r1";
static MODEL_DOWNLOAD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static MODEL_STATUS_CACHE: OnceLock<Mutex<Option<CachedModelStatus>>> = OnceLock::new();
static MODEL_ASSET_SIZES: OnceLock<std::collections::HashMap<String, u64>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
pub struct MlRuntimeStatus {
    pub state: String,
    pub provider: String,
    pub model_id: String,
    pub worker_version: Option<String>,
    pub rapidocr_core_version: Option<String>,
    pub restart_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub last_error: Option<String>,
    pub last_elapsed_ms: Option<f64>,
    pub failed_screenshots: i64,
}

#[derive(Debug)]
pub struct MlOcrResult {
    pub blocks: Vec<MlOcrBlock>,
    pub timings: MlOcrTimings,
}

#[derive(Debug, Clone, Serialize)]
pub struct RustOcrModelStatus {
    pub model_id: String,
    pub revision: String,
    pub installed: bool,
    pub source: String,
    pub missing_files: Vec<String>,
    pub corrupt_files: Vec<String>,
    pub path: String,
}

#[derive(Debug, Clone)]
struct ResolvedOcrModel {
    source: &'static str,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct CachedModelStatus {
    checked_at: Instant,
    status: RustOcrModelStatus,
}

struct MlChild {
    provider: MlProvider,
    child: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    request_lock: Mutex<()>,
    _job: MlJobHandle,
}

struct MlJobHandle(HANDLE);

struct PendingMlChild(Option<Child>);

impl PendingMlChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child(&self) -> &Child {
        self.0.as_ref().expect("pending ML child is available")
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("pending ML child is available")
    }

    fn take(&mut self) -> Child {
        self.0.take().expect("pending ML child is available")
    }
}

impl Drop for PendingMlChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for MlJobHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

unsafe impl Send for MlJobHandle {}
unsafe impl Sync for MlJobHandle {}

impl MlChild {
    fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }

    fn request(&self, request: &MlRequest, body: &[u8]) -> Result<MlResponse, String> {
        let _request_guard = self.request_lock.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
            write_request(&mut *stdin, request, body)?;
        }
        let mut stdout = self.stdout.lock().unwrap_or_else(|e| e.into_inner());
        read_response(&mut *stdout)
    }
}

struct MlRuntimeInner {
    process: Option<Arc<MlChild>>,
    state: String,
    worker_version: Option<String>,
    rapidocr_core_version: Option<String>,
    restart_count: u64,
    success_count: u64,
    failure_count: u64,
    last_error: Option<String>,
    last_elapsed_ms: Option<f64>,
    directml_disabled_for_session: bool,
}

pub struct MlRuntimeState {
    inner: Mutex<MlRuntimeInner>,
    lifecycle_lock: Mutex<()>,
    request_gate: tokio::sync::Mutex<()>,
    retry_lock: tokio::sync::Mutex<()>,
    next_request_id: AtomicU64,
}

impl Default for MlRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl MlRuntimeState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MlRuntimeInner {
                process: None,
                state: "stopped".to_string(),
                worker_version: None,
                rapidocr_core_version: None,
                restart_count: 0,
                success_count: 0,
                failure_count: 0,
                last_error: None,
                last_elapsed_ms: None,
                directml_disabled_for_session: false,
            }),
            lifecycle_lock: Mutex::new(()),
            request_gate: tokio::sync::Mutex::new(()),
            retry_lock: tokio::sync::Mutex::new(()),
            next_request_id: AtomicU64::new(1),
        }
    }

    pub async fn run_ocr(
        self: &Arc<Self>,
        app: AppHandle,
        image_bytes: Vec<u8>,
        timeout: Duration,
        use_directml_beta: bool,
    ) -> Result<MlOcrResult, String> {
        // The worker protocol is single-request. Holding this gate across
        // provider selection, startup and the response prevents a concurrent
        // provider switch from killing an in-flight request.
        let _request_guard = self.request_gate.lock().await;
        let directml_disabled = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .directml_disabled_for_session;
        let provider = if use_directml_beta && !directml_disabled {
            MlProvider::DirectMl
        } else {
            MlProvider::Cpu
        };
        if use_directml_beta && directml_disabled {
            tracing::warn!(
                "[ML:DML] DirectML Beta is disabled for this session after an earlier failure; using CPU"
            );
        }
        let state_for_start = self.clone();
        let process_result =
            tokio::task::spawn_blocking(move || state_for_start.ensure_process(&app, provider))
                .await
                .map_err(|error| format!("ML worker startup task failed: {error}"))?;
        let process = match process_result {
            Ok(process) => process,
            Err(error) => {
                self.record_failure(&error, 0.0);
                if provider == MlProvider::DirectMl {
                    self.disable_directml_for_session(&error);
                }
                return Err(error);
            }
        };
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let request = MlRequest::Ocr {
            request_id,
            timeout_ms: timeout.as_millis().min(u64::MAX as u128) as u64,
            body_len: image_bytes.len(),
        };
        let started = Instant::now();
        tracing::info!(
            "[ML:OCR] submit request_id={} provider={:?} bytes={} timeout_ms={}",
            request_id,
            provider,
            image_bytes.len(),
            timeout.as_millis()
        );
        let process_for_request = process.clone();
        let request_task = tokio::task::spawn_blocking(move || {
            process_for_request.request(&request, &image_bytes)
        });
        let response = match tokio::time::timeout(timeout + CANCEL_GRACE, request_task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(format!("ML request task failed: {error}")),
            Err(_) => {
                tracing::error!(
                    "[ML:WATCHDOG] request_id={} did not stop after deadline and grace; killing worker",
                    request_id
                );
                process.kill();
                self.clear_process("watchdog_timeout");
                Err(format!(
                    "Rust OCR watchdog timeout after {} ms",
                    (timeout + CANCEL_GRACE).as_millis()
                ))
            }
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        match response {
            Ok(MlResponse::OcrComplete {
                request_id: response_id,
                blocks,
                timings,
            }) if response_id == request_id => {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.state = "ready".to_string();
                inner.success_count += 1;
                inner.last_error = None;
                inner.last_elapsed_ms = Some(elapsed_ms);
                tracing::info!(
                    "[ML:OCR] success request_id={} provider={:?} blocks={} child_ms={:.1} total_ms={:.1}",
                    request_id,
                    provider,
                    blocks.len(),
                    timings.request_total_ms,
                    elapsed_ms
                );
                Ok(MlOcrResult { blocks, timings })
            }
            Ok(MlResponse::Error {
                request_id: response_id,
                kind,
                message,
            }) if response_id == request_id => {
                let error = format!("Rust OCR {kind}: {message}");
                self.record_failure(&error, elapsed_ms);
                if provider == MlProvider::DirectMl {
                    self.disable_directml_for_session(&error);
                }
                Err(error)
            }
            Ok(other) => {
                let error = format!("unexpected ML response for request {request_id}: {other:?}");
                self.record_failure(&error, elapsed_ms);
                self.restart_process("protocol_mismatch");
                Err(error)
            }
            Err(error) => {
                self.record_failure(&error, elapsed_ms);
                if provider == MlProvider::DirectMl {
                    self.disable_directml_for_session(&error);
                }
                self.restart_process("request_failure");
                Err(error)
            }
        }
    }

    pub fn status(&self, failed_screenshots: i64) -> MlRuntimeStatus {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let exited = inner.process.as_ref().and_then(|process| {
            let mut child = process.child.lock().unwrap_or_else(|e| e.into_inner());
            match child.try_wait() {
                Ok(Some(status)) => Some(format!("Rust ML worker exited with {status}")),
                Ok(None) => None,
                Err(error) => Some(format!("Failed to query Rust ML worker: {error}")),
            }
        });
        if let Some(error) = exited {
            tracing::error!("[ML:WATCHDOG] {}", error);
            inner.process = None;
            inner.state = "failed".to_string();
            inner.failure_count += 1;
            inner.last_error = Some(truncate_error(&error));
        }
        let provider = inner
            .process
            .as_ref()
            .map(|process| match process.provider {
                MlProvider::Cpu => "cpu",
                MlProvider::DirectMl => "directml_beta",
            })
            .unwrap_or("none");
        MlRuntimeStatus {
            state: inner.state.clone(),
            provider: provider.to_string(),
            model_id: MODEL_ID.to_string(),
            worker_version: inner.worker_version.clone(),
            rapidocr_core_version: inner.rapidocr_core_version.clone(),
            restart_count: inner.restart_count,
            success_count: inner.success_count,
            failure_count: inner.failure_count,
            last_error: inner.last_error.clone(),
            last_elapsed_ms: inner.last_elapsed_ms,
            failed_screenshots,
        }
    }

    pub fn stop(&self) {
        let _lifecycle_guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.stop_locked();
    }

    fn stop_locked(&self) {
        let process = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.state = "stopping".to_string();
            inner.process.take()
        };
        if let Some(process) = process {
            // A worker may be stuck inside native inference. Restart/exit must
            // never wait indefinitely for a graceful protocol response.
            process.kill();
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.state = "stopped".to_string();
    }

    fn ensure_process(
        &self,
        app: &AppHandle,
        provider: MlProvider,
    ) -> Result<Arc<MlChild>, String> {
        let _lifecycle_guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(process) = &inner.process {
                if process.provider == provider {
                    return Ok(process.clone());
                }
            }
        }
        self.stop_locked();
        self.start_process(app, provider)
    }

    fn start_process(&self, app: &AppHandle, provider: MlProvider) -> Result<Arc<MlChild>, String> {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.state = "starting".to_string();
        }
        let executable = resolve_ml_executable(app)?;
        let model_dir = match resolve_model_directory(app) {
            Ok(model) => model.path,
            Err(error) => {
                invalidate_model_status_cache();
                return Err(error);
            }
        };
        let mut command = Command::new(&executable);
        command
            .arg("--model-dir")
            .arg(&model_dir)
            .arg("--threads")
            .arg("2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        if provider == MlProvider::DirectMl {
            command.arg("--directml");
        }
        tracing::info!(
            "[ML:SUPERVISOR] starting worker path={} model_dir={} provider={:?}",
            executable.display(),
            model_dir.display(),
            provider
        );
        let child = command
            .spawn()
            .map_err(|error| format!("failed to start Rust ML worker: {error}"))?;
        let mut pending_child = PendingMlChild::new(child);
        let job = assign_kill_on_close_job(pending_child.child())?;
        let stdin = pending_child
            .child_mut()
            .stdin
            .take()
            .ok_or("ML worker stdin unavailable")?;
        let stdout = pending_child
            .child_mut()
            .stdout
            .take()
            .ok_or("ML worker stdout unavailable")?;
        if let Some(stderr) = pending_child.child_mut().stderr.take() {
            std::thread::Builder::new()
                .name("carbonpaper-ml-log".to_string())
                .spawn(move || {
                    use std::io::BufRead;
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        tracing::info!("[ML:WORKER] {}", line);
                    }
                })
                .map_err(|error| format!("failed to start ML log reader: {error}"))?;
        }
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("carbonpaper-ml-handshake".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_response(&mut reader);
                let _ = ready_sender.send((ready, reader));
            })
            .map_err(|error| format!("failed to start ML handshake reader: {error}"))?;
        let (ready, stdout) = match ready_receiver.recv_timeout(STARTUP_TIMEOUT) {
            Ok(value) => value,
            Err(_) => {
                return Err("Rust ML worker startup timed out".to_string());
            }
        };
        let child = pending_child.take();
        let process = Arc::new(MlChild {
            provider,
            child: Mutex::new(child),
            stdin: Mutex::new(BufWriter::new(stdin)),
            stdout: Mutex::new(stdout),
            request_lock: Mutex::new(()),
            _job: job,
        });
        match ready {
            Ok(MlResponse::Ready {
                protocol_version,
                worker_version,
                rapidocr_core_version,
                provider: ready_provider,
                model_id,
            }) if protocol_version == ML_PROTOCOL_VERSION
                && ready_provider == provider
                && model_id == MODEL_ID =>
            {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.process = Some(process.clone());
                inner.state = "ready".to_string();
                inner.worker_version = Some(worker_version);
                inner.rapidocr_core_version = Some(rapidocr_core_version);
                inner.last_error = None;
                tracing::info!("[ML:SUPERVISOR] worker ready provider={:?}", provider);
                Ok(process)
            }
            Ok(response) => {
                process.kill();
                Err(format!("invalid Rust ML worker handshake: {response:?}"))
            }
            Err(error) => {
                process.kill();
                Err(format!("Rust ML worker startup failed: {error}"))
            }
        }
    }

    fn record_failure(&self, error: &str, elapsed_ms: f64) {
        tracing::error!(
            "[ML:OCR] failure elapsed_ms={:.1} error={}",
            elapsed_ms,
            error
        );
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.state = "degraded".to_string();
        inner.failure_count += 1;
        inner.last_error = Some(truncate_error(error));
        inner.last_elapsed_ms = Some(elapsed_ms);
    }

    fn disable_directml_for_session(&self, error: &str) {
        tracing::warn!(
            "[ML:DML] disabling temporary DirectML Beta provider for this session: {}",
            error
        );
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.directml_disabled_for_session = true;
    }

    fn restart_process(&self, reason: &str) {
        let _lifecycle_guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let process = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.restart_count += 1;
            inner.state = "restarting".to_string();
            inner.process.take()
        };
        if let Some(process) = process {
            tracing::warn!("[ML:WATCHDOG] stopping worker reason={}", reason);
            process.kill();
        }
    }

    fn clear_process(&self, reason: &str) {
        let _lifecycle_guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.restart_count += 1;
        inner.process = None;
        inner.state = "degraded".to_string();
        inner.last_error = Some(reason.to_string());
    }
}

impl Drop for MlRuntimeState {
    fn drop(&mut self) {
        self.stop();
    }
}

fn local_repair_model_directory() -> Result<PathBuf, String> {
    Ok(file_in_local_appdata()
        .ok_or("LOCALAPPDATA is unavailable")?
        .join("models")
        .join("ocr")
        .join(MODEL_ID))
}

fn model_directory_candidates(app: &AppHandle) -> Vec<ResolvedOcrModel> {
    let mut candidates = Vec::new();
    if let Some(path) = file_in_resources(app, BUNDLED_MODEL_RELATIVE_DIR) {
        candidates.push(ResolvedOcrModel {
            source: "bundled",
            path,
        });
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(ResolvedOcrModel {
                source: "portable",
                path: directory.join(BUNDLED_MODEL_RELATIVE_DIR),
            });
        }
    }
    if let Ok(path) = local_repair_model_directory() {
        candidates.push(ResolvedOcrModel {
            source: "local_repair",
            path,
        });
    }
    if cfg!(debug_assertions) {
        candidates.push(ResolvedOcrModel {
            source: "development",
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("pre-bundle")
                .join(BUNDLED_MODEL_RELATIVE_DIR),
        });
    }
    candidates
}

fn inspect_model_directory(directory: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    use rapidocr_core::config::PipelineConfig;
    use rapidocr_core::model::model_set_by_name;

    let model_set = model_set_by_name(MODEL_ID).ok_or("registered OCR model is missing")?;
    let mut missing_files = Vec::new();
    let mut corrupt_files = Vec::new();
    for asset in model_set.assets_for_pipeline(PipelineConfig::without_cls()) {
        let path = directory.join(asset.filename);
        if !path.is_file() {
            missing_files.push(asset.filename.to_string());
            continue;
        }
        if let Some(expected) = asset.sha256 {
            match sha256_file(&path) {
                Ok(actual) if actual == expected => {}
                Ok(_) | Err(_) => corrupt_files.push(asset.filename.to_string()),
            }
        }
    }
    Ok((missing_files, corrupt_files))
}

fn resolve_model_directory(app: &AppHandle) -> Result<ResolvedOcrModel, String> {
    let mut failures = Vec::new();
    for candidate in model_directory_candidates(app) {
        let (missing, corrupt) = inspect_model_directory(&candidate.path)?;
        if missing.is_empty() && corrupt.is_empty() {
            return Ok(candidate);
        }
        failures.push(format!(
            "{}:{} missing={:?} corrupt={:?}",
            candidate.source,
            candidate.path.display(),
            missing,
            corrupt
        ));
    }
    Err(format!(
        "PP-OCRv5 Mobile model is unavailable or corrupt; checked {}",
        failures.join("; ")
    ))
}

pub fn resolve_ocr_model_path(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_model_directory(app)
        .map(|resolved| resolved.path)
        .inspect_err(|_| invalidate_model_status_cache())
}

pub fn ocr_model_status(app: &AppHandle) -> Result<RustOcrModelStatus, String> {
    let cache = MODEL_STATUS_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(cached) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|cached| cached.checked_at.elapsed() < MODEL_STATUS_CACHE_TTL)
        .cloned()
    {
        return Ok(cached.status);
    }
    let status = inspect_model_status(app)?;
    cache_model_status(status.clone());
    Ok(status)
}

fn cache_model_status(status: RustOcrModelStatus) {
    let cache = MODEL_STATUS_CACHE.get_or_init(|| Mutex::new(None));
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(CachedModelStatus {
        checked_at: Instant::now(),
        status,
    });
}

fn invalidate_model_status_cache() {
    let cache = MODEL_STATUS_CACHE.get_or_init(|| Mutex::new(None));
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn expected_model_asset_size(filename: &str) -> Result<u64, String> {
    let sizes = MODEL_ASSET_SIZES.get_or_init(|| {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/release-assets/ocr-models.json"))
                .expect("OCR release asset manifest must be valid JSON");
        manifest["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some((
                    entry.get("name")?.as_str()?.to_string(),
                    entry.get("size")?.as_u64()?,
                ))
            })
            .collect()
    });
    sizes
        .get(filename)
        .copied()
        .ok_or_else(|| format!("model asset has no expected size: {filename}"))
}

fn inspect_model_status(app: &AppHandle) -> Result<RustOcrModelStatus, String> {
    let candidates = model_directory_candidates(app);
    for candidate in &candidates {
        let (missing_files, corrupt_files) = inspect_model_directory(&candidate.path)?;
        if missing_files.is_empty() && corrupt_files.is_empty() {
            return Ok(RustOcrModelStatus {
                model_id: MODEL_ID.to_string(),
                revision: MODEL_REVISION.to_string(),
                installed: true,
                source: candidate.source.to_string(),
                missing_files,
                corrupt_files,
                path: candidate.path.to_string_lossy().to_string(),
            });
        }
    }

    let preferred = candidates.into_iter().next().unwrap_or(ResolvedOcrModel {
        source: "unavailable",
        path: PathBuf::from(BUNDLED_MODEL_RELATIVE_DIR),
    });
    let (missing_files, corrupt_files) = inspect_model_directory(&preferred.path)?;
    Ok(RustOcrModelStatus {
        model_id: MODEL_ID.to_string(),
        revision: MODEL_REVISION.to_string(),
        installed: missing_files.is_empty() && corrupt_files.is_empty(),
        source: preferred.source.to_string(),
        missing_files,
        corrupt_files,
        path: preferred.path.to_string_lossy().to_string(),
    })
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    if development.exists() {
        return Ok(development);
    }
    Err("carbonpaper-ml.exe was not found; build or reinstall CarbonPaper".to_string())
}

fn truncate_error(error: &str) -> String {
    error.chars().take(500).collect()
}

fn assign_kill_on_close_job(child: &Child) -> Result<MlJobHandle, String> {
    unsafe {
        let handle = CreateJobObjectW(None, None)
            .map_err(|error| format!("failed to create ML job object: {error:?}"))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(error) = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(handle);
            return Err(format!("failed to configure ML job object: {error:?}"));
        }
        let process_handle = HANDLE(child.as_raw_handle() as *mut _);
        if let Err(error) = AssignProcessToJobObject(handle, process_handle) {
            let _ = CloseHandle(handle);
            return Err(format!(
                "failed to assign ML worker to job object: {error:?}"
            ));
        }
        Ok(MlJobHandle(handle))
    }
}

#[tauri::command]
pub fn get_ml_ocr_status(
    state: tauri::State<'_, Arc<MlRuntimeState>>,
    storage: tauri::State<'_, Arc<crate::storage::StorageState>>,
) -> MlRuntimeStatus {
    state.status(storage.count_failed_ocr().unwrap_or(0))
}

#[tauri::command]
pub fn restart_ml_ocr_worker(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: tauri::State<'_, Arc<MlRuntimeState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    state.stop();
    Ok(())
}

#[tauri::command]
pub async fn get_rust_ocr_model_status(app: AppHandle) -> Result<RustOcrModelStatus, String> {
    tokio::task::spawn_blocking(move || ocr_model_status(&app))
        .await
        .map_err(|error| format!("model status task failed: {error}"))?
}

#[tauri::command]
pub async fn download_rust_ocr_model(
    app: AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
) -> Result<RustOcrModelStatus, String> {
    use futures::StreamExt;
    use rapidocr_core::config::PipelineConfig;
    use rapidocr_core::model::model_set_by_name;

    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    invalidate_model_status_cache();
    if let Ok(status) = inspect_model_status(&app) {
        if status.installed {
            cache_model_status(status.clone());
            return Ok(status);
        }
    }
    if !crate::registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("Network features are disabled".to_string());
    }
    let download_lock = MODEL_DOWNLOAD_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _download_guard = download_lock
        .try_lock()
        .map_err(|_| "Rust OCR model download is already in progress".to_string())?;
    let directory = local_repair_model_directory()?;
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("failed to create OCR model directory: {error}"))?;
    let model_set = model_set_by_name(MODEL_ID).ok_or("registered OCR model is missing")?;
    let assets = model_set.assets_for_pipeline(PipelineConfig::without_cls());
    let client = reqwest::Client::builder()
        .user_agent(format!("CarbonPaper/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(MODEL_DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(MODEL_DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to create model download client: {error}"))?;
    let total_assets = assets.len();
    for (asset_index, asset) in assets.into_iter().enumerate() {
        let target = directory.join(asset.filename);
        let expected = asset
            .sha256
            .ok_or_else(|| format!("model asset has no checksum: {}", asset.filename))?;
        let expected_size = expected_model_asset_size(asset.filename)?;
        if target.is_file() {
            let target_for_hash = target.clone();
            let actual = tokio::task::spawn_blocking(move || sha256_file(&target_for_hash))
                .await
                .map_err(|error| format!("checksum task failed: {error}"))??;
            if actual == expected {
                continue;
            }
        }
        let part = target.with_extension(format!(
            "{}part",
            target
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!("{value}."))
                .unwrap_or_default()
        ));
        let _ = tokio::fs::remove_file(&part).await;
        let response = client
            .get(asset.url)
            .send()
            .await
            .map_err(|error| format!("failed to download {}: {error}", asset.filename))?
            .error_for_status()
            .map_err(|error| format!("model download failed for {}: {error}", asset.filename))?;
        let content_length = response.content_length();
        if let Some(content_length) = content_length {
            if content_length > expected_size {
                return Err(format!(
                    "download is larger than expected for {}: maximum {}, got {}",
                    asset.filename, expected_size, content_length
                ));
            }
        }
        let mut stream = response.bytes_stream();
        let download_result: Result<u64, String> = async {
            let mut file = tokio::fs::File::create(&part)
                .await
                .map_err(|error| format!("failed to create {}: {error}", part.display()))?;
            let mut downloaded = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|error| format!("model download stream failed: {error}"))?;
                tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                    .await
                    .map_err(|error| format!("failed to write {}: {error}", part.display()))?;
                downloaded += chunk.len() as u64;
                if downloaded > expected_size {
                    return Err(format!(
                        "download exceeded expected size for {}: expected {} bytes",
                        asset.filename, expected_size
                    ));
                }
                let _ = app.emit(
                    "rust-ocr-model-download-progress",
                    serde_json::json!({
                        "file": asset.filename,
                        "asset_index": asset_index + 1,
                        "asset_count": total_assets,
                        "downloaded": downloaded,
                        "total": content_length,
                    }),
                );
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .map_err(|error| format!("failed to flush {}: {error}", part.display()))?;
            Ok(downloaded)
        }
        .await;
        let downloaded = match download_result {
            Ok(downloaded) => downloaded,
            Err(error) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(error);
            }
        };
        if downloaded != expected_size {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!(
                "incomplete download for {}: expected {}, got {} bytes",
                asset.filename, expected_size, downloaded
            ));
        }
        let part_for_hash = part.clone();
        let actual = match tokio::task::spawn_blocking(move || sha256_file(&part_for_hash)).await {
            Ok(Ok(actual)) => actual,
            Ok(Err(error)) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(error);
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(format!("checksum task failed: {error}"));
            }
        };
        if actual != expected {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!(
                "checksum mismatch for {}: expected {}, got {}",
                asset.filename, expected, actual
            ));
        }
        if target.exists() {
            tokio::fs::remove_file(&target)
                .await
                .map_err(|error| format!("failed to replace {}: {error}", target.display()))?;
        }
        tokio::fs::rename(&part, &target)
            .await
            .map_err(|error| format!("failed to install {}: {error}", asset.filename))?;
    }
    let status = inspect_model_status(&app)?;
    cache_model_status(status.clone());
    Ok(status)
}

#[tauri::command]
pub async fn retry_failed_ocr(
    app: AppHandle,
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    storage: tauri::State<'_, Arc<crate::storage::StorageState>>,
    capture_state: tauri::State<'_, Arc<crate::capture::CaptureState>>,
    limit: Option<i64>,
) -> Result<serde_json::Value, String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    let ml_state = app.state::<Arc<MlRuntimeState>>().inner().clone();
    let _retry_guard = ml_state
        .retry_lock
        .try_lock()
        .map_err(|_| "OCR retry is already in progress".to_string())?;
    let ids = storage.list_failed_ocr_ids(limit.unwrap_or(10).clamp(1, 100))?;
    let mut completed = 0usize;
    let mut failed = 0usize;
    for screenshot_id in &ids {
        let record = match storage.get_screenshot_by_id(*screenshot_id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                failed += 1;
                continue;
            }
            Err(error) => {
                failed += 1;
                let _ = storage.set_ocr_status(
                    *screenshot_id,
                    "failed",
                    None,
                    Some(MODEL_ID),
                    None,
                    Some(&error),
                    None,
                );
                continue;
            }
        };
        let bytes = match storage.read_image_bytes(&record.image_path) {
            Ok((bytes, _)) => bytes,
            Err(error) => {
                failed += 1;
                let _ = storage.set_ocr_status(
                    *screenshot_id,
                    "failed",
                    None,
                    Some(MODEL_ID),
                    None,
                    Some(&error),
                    None,
                );
                continue;
            }
        };
        capture_state
            .in_flight_ocr_count
            .fetch_add(1, Ordering::SeqCst);
        {
            let mut cache = capture_state
                .ocr_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.insert(*screenshot_id, bytes.clone());
        }
        let result = crate::capture::process_ocr_inner(
            &app,
            &storage,
            *screenshot_id,
            &bytes,
            &record.image_hash,
            record.window_title.as_deref().unwrap_or(""),
            record.process_name.as_deref().unwrap_or(""),
            record
                .timestamp
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            crate::registry_config::get_u32("ocr_timeout_secs").unwrap_or(120),
            crate::capture::OcrRouteConfig::from_registry(),
        )
        .await;
        {
            let mut cache = capture_state
                .ocr_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.remove(screenshot_id);
        }
        capture_state
            .in_flight_ocr_count
            .fetch_sub(1, Ordering::SeqCst);
        match result {
            Ok(()) => completed += 1,
            Err(error) => {
                failed += 1;
                let _ = storage.set_ocr_status(
                    *screenshot_id,
                    "failed",
                    Some(
                        if crate::registry_config::get_bool("rust_ocr_enabled").unwrap_or(true) {
                            "rust"
                        } else {
                            "python"
                        },
                    ),
                    Some(MODEL_ID),
                    None,
                    Some(&error),
                    None,
                );
            }
        }
    }
    Ok(serde_json::json!({
        "requested": ids.len(),
        "completed": completed,
        "failed": failed,
        "remaining": storage.count_failed_ocr().unwrap_or(0),
    }))
}

pub async fn run_postprocess_recovery_loop(app: AppHandle) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if let Some(storage) = app.try_state::<Arc<crate::storage::StorageState>>() {
        match storage.recover_interrupted_ocr_postprocess() {
            Ok(recovered) if recovered > 0 => tracing::info!(
                "[ML:POSTPROCESS] deferred {} interrupted rows until explicit authentication",
                recovered
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                "[ML:POSTPROCESS] failed to recover interrupted rows: {}",
                error
            ),
        }
    }
    // Let storage and the Python monitor finish startup before the first pass.
    tokio::time::sleep(Duration::from_secs(10)).await;
    loop {
        interval.tick().await;
        if let Err(error) = drain_pending_postprocess(&app).await {
            tracing::debug!("[ML:POSTPROCESS] recovery pass deferred: {}", error);
        }
    }
}

async fn drain_pending_postprocess(app: &AppHandle) -> Result<(), String> {
    let storage = app
        .state::<Arc<crate::storage::StorageState>>()
        .inner()
        .clone();
    let capture = app
        .state::<Arc<crate::capture::CaptureState>>()
        .inner()
        .clone();
    let ids = storage.list_pending_ocr_postprocess_ids(10)?;
    for screenshot_id in ids {
        let Some(record) = storage.get_screenshot_by_id(screenshot_id)? else {
            continue;
        };
        let (image_bytes, _) = match storage.read_image_bytes_silent(&record.image_path) {
            Ok(value) => value,
            Err(crate::storage::BackgroundReadError::AuthRequired) => {
                storage.set_ocr_postprocess_status(
                    screenshot_id,
                    "waiting_for_auth",
                    Some("Waiting for user authentication"),
                )?;
                continue;
            }
            Err(error) => {
                let _ = storage.record_ocr_postprocess_retry(screenshot_id, &error.to_string());
                continue;
            }
        };
        let ocr_results = match storage.get_screenshot_ocr_results_silent(screenshot_id) {
            Ok(results) => results,
            Err(crate::storage::BackgroundReadError::AuthRequired) => {
                storage.set_ocr_postprocess_status(
                    screenshot_id,
                    "waiting_for_auth",
                    Some("Waiting for user authentication"),
                )?;
                continue;
            }
            Err(error) => {
                storage.record_ocr_postprocess_retry(screenshot_id, &error.to_string())?;
                continue;
            }
        }
        .into_iter()
        .map(|result| crate::storage::OcrResultInput {
            text: result.text,
            confidence: result.confidence,
            box_coords: result.box_coords,
        })
        .collect::<Vec<_>>();
        {
            let mut cache = capture
                .ocr_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.insert(screenshot_id, image_bytes);
        }
        let enqueue_result = crate::capture::enqueue_python_ocr_postprocess(
            app,
            screenshot_id,
            &record.image_hash,
            record.window_title.as_deref().unwrap_or(""),
            record.process_name.as_deref().unwrap_or(""),
            record
                .timestamp
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            &ocr_results,
        )
        .await;
        {
            let mut cache = capture
                .ocr_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cache.remove(&screenshot_id);
        }
        match enqueue_result {
            Ok(true) => storage.set_ocr_postprocess_status(screenshot_id, "queued", None)?,
            Ok(false) => storage.record_ocr_postprocess_retry(
                screenshot_id,
                "Python OCR postprocess queue is full",
            )?,
            Err(error) => storage.record_ocr_postprocess_retry(screenshot_id, &error)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidocr_core::config::PipelineConfig;
    use rapidocr_core::model::model_set_by_name;

    #[test]
    fn release_manifest_matches_rapidocr_core_model_set() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/release-assets/ocr-models.json"))
                .expect("OCR release asset manifest must be valid JSON");
        assert_eq!(manifest["model_id"], MODEL_ID);
        assert_eq!(manifest["revision"], MODEL_REVISION);

        let files = manifest["files"]
            .as_array()
            .expect("manifest files must be an array");
        let model_set = model_set_by_name(MODEL_ID).expect("registered OCR model must exist");
        let expected = model_set.assets_for_pipeline(PipelineConfig::without_cls());
        assert_eq!(files.len(), expected.len());
        for asset in expected {
            let entry = files
                .iter()
                .find(|entry| entry["name"] == asset.filename)
                .unwrap_or_else(|| panic!("manifest is missing {}", asset.filename));
            assert_eq!(entry["sha256"], asset.sha256.expect("asset checksum"));
            assert_eq!(
                entry["size"].as_u64(),
                Some(expected_model_asset_size(asset.filename).expect("asset size"))
            );
        }
    }

    #[test]
    fn model_directory_inspection_rejects_missing_and_corrupt_assets() {
        let directory = tempfile::tempdir().expect("temp model directory");
        let (missing, corrupt) = inspect_model_directory(directory.path()).expect("inspection");
        assert_eq!(missing.len(), 3);
        assert!(corrupt.is_empty());

        for filename in &missing {
            std::fs::write(directory.path().join(filename), b"not an onnx model")
                .expect("write corrupt model fixture");
        }
        let (missing, corrupt) = inspect_model_directory(directory.path()).expect("inspection");
        assert!(missing.is_empty());
        assert_eq!(corrupt.len(), 3);
    }
}
