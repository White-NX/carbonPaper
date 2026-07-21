//! Desktop-side lifecycle and request routing for the isolated semantic ML worker.
//!
//! This state is intentionally separate from `MlRuntimeState`: semantic inference never
//! shares the OCR queue, process, watchdog, or failure domain.

use crate::ml_protocol::{
    read_response, write_request, MlImageInput, MlProvider, MlRequest, MlResponse, MlSemanticModel,
    MlSemanticTimings, ML_PROTOCOL_VERSION,
};
use crate::resource_utils::{file_in_local_appdata, find_existing_file_in_resources};
use crate::semantic_models::{descriptor, expected_model_fingerprint};
use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::io::{BufReader, BufWriter};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize)]
pub struct SemanticRuntimeStatus {
    pub state: String,
    pub provider: String,
    pub worker_version: Option<String>,
    pub ort_version: Option<String>,
    pub runtime_path: Option<String>,
    pub loaded_model: Option<String>,
    pub model_revision: Option<String>,
    pub model_fingerprint: Option<String>,
    pub restart_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub last_error_kind: Option<String>,
    pub last_error: Option<String>,
    pub last_elapsed_ms: Option<f64>,
    pub directml_disabled_for_session: bool,
}

#[derive(Debug)]
pub struct SemanticEmbeddingResult {
    pub model: MlSemanticModel,
    pub dimensions: usize,
    pub vectors: Vec<Vec<f32>>,
    pub timings: MlSemanticTimings,
}

#[derive(Debug)]
pub struct SemanticRerankResult {
    pub model: MlSemanticModel,
    pub scores: Vec<f32>,
    pub timings: MlSemanticTimings,
}

struct SemanticMlChild {
    provider: MlProvider,
    supported_models: Vec<MlSemanticModel>,
    child: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    request_lock: Mutex<()>,
    _job: SemanticJobHandle,
}

struct SemanticJobHandle(HANDLE);

struct PendingSemanticChild(Option<Child>);

impl PendingSemanticChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child(&self) -> &Child {
        self.0
            .as_ref()
            .expect("pending semantic child is available")
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0
            .as_mut()
            .expect("pending semantic child is available")
    }

    fn take(&mut self) -> Child {
        self.0.take().expect("pending semantic child is available")
    }
}

impl Drop for PendingSemanticChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SemanticJobHandle {
    fn drop(&mut self) {
        // SAFETY: this RAII wrapper exclusively owns a valid Job Object handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: Windows Job Object handles are kernel references and the wrapper owns the
// single CloseHandle call.
unsafe impl Send for SemanticJobHandle {}
// SAFETY: shared handle access does not expose mutable Rust memory.
unsafe impl Sync for SemanticJobHandle {}

impl SemanticMlChild {
    fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }

    fn request(&self, request: &MlRequest, body: &[u8]) -> Result<MlResponse, String> {
        let _guard = self
            .request_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            let mut stdin = self.stdin.lock().unwrap_or_else(|error| error.into_inner());
            write_request(&mut *stdin, request, body)?;
        }
        let mut stdout = self
            .stdout
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        read_response(&mut *stdout)
    }
}

struct SemanticRuntimeInner {
    process: Option<Arc<SemanticMlChild>>,
    directml_supported_models: Option<Vec<MlSemanticModel>>,
    state: String,
    worker_version: Option<String>,
    ort_version: Option<String>,
    runtime_path: Option<String>,
    loaded_model: Option<MlSemanticModel>,
    restart_count: u64,
    success_count: u64,
    failure_count: u64,
    last_error_kind: Option<String>,
    last_error: Option<String>,
    last_elapsed_ms: Option<f64>,
    directml_disabled_for_session: bool,
}

pub struct SemanticRuntimeState {
    inner: Mutex<SemanticRuntimeInner>,
    lifecycle_lock: Mutex<()>,
    request_gate: tokio::sync::Mutex<()>,
    next_request_id: AtomicU64,
}

impl Default for SemanticRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticRuntimeState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SemanticRuntimeInner {
                process: None,
                directml_supported_models: None,
                state: "stopped".to_string(),
                worker_version: None,
                ort_version: None,
                runtime_path: None,
                loaded_model: None,
                restart_count: 0,
                success_count: 0,
                failure_count: 0,
                last_error_kind: None,
                last_error: None,
                last_elapsed_ms: None,
                directml_disabled_for_session: false,
            }),
            lifecycle_lock: Mutex::new(()),
            request_gate: tokio::sync::Mutex::new(()),
            next_request_id: AtomicU64::new(1),
        }
    }

    pub async fn embed_text(
        self: &Arc<Self>,
        app: AppHandle,
        model: MlSemanticModel,
        texts: Vec<String>,
        timeout: Duration,
        prefer_directml: bool,
    ) -> Result<SemanticEmbeddingResult, String> {
        let request = MlRequest::EmbedText {
            request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
            timeout_ms: duration_ms(timeout),
            model,
            texts,
        };
        let response = self
            .send_with_fallback(app, request, Arc::new(Vec::new()), timeout, prefer_directml)
            .await?;
        match response {
            MlResponse::EmbeddingComplete {
                model: response_model,
                dimensions,
                vectors,
                timings,
                ..
            } if response_model == model => Ok(SemanticEmbeddingResult {
                model,
                dimensions,
                vectors,
                timings,
            }),
            other => Err(format!(
                "protocol: unexpected semantic embedding response: {other:?}"
            )),
        }
    }

    pub async fn embed_image(
        self: &Arc<Self>,
        app: AppHandle,
        model: MlSemanticModel,
        images: Vec<MlImageInput>,
        body: Vec<u8>,
        timeout: Duration,
        prefer_directml: bool,
    ) -> Result<SemanticEmbeddingResult, String> {
        let request = MlRequest::EmbedImage {
            request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
            timeout_ms: duration_ms(timeout),
            model,
            images,
            body_len: body.len(),
        };
        let response = self
            .send_with_fallback(app, request, Arc::new(body), timeout, prefer_directml)
            .await?;
        match response {
            MlResponse::EmbeddingComplete {
                model: response_model,
                dimensions,
                vectors,
                timings,
                ..
            } if response_model == model => Ok(SemanticEmbeddingResult {
                model,
                dimensions,
                vectors,
                timings,
            }),
            other => Err(format!(
                "protocol: unexpected semantic image response: {other:?}"
            )),
        }
    }

    pub async fn rerank(
        self: &Arc<Self>,
        app: AppHandle,
        query: String,
        documents: Vec<String>,
        timeout: Duration,
        prefer_directml: bool,
    ) -> Result<SemanticRerankResult, String> {
        let model = MlSemanticModel::BgeRerankerV2M3;
        let request = MlRequest::Rerank {
            request_id: self.next_request_id.fetch_add(1, Ordering::Relaxed),
            timeout_ms: duration_ms(timeout),
            model,
            query,
            documents,
        };
        let response = self
            .send_with_fallback(app, request, Arc::new(Vec::new()), timeout, prefer_directml)
            .await?;
        match response {
            MlResponse::RerankComplete {
                model: response_model,
                scores,
                timings,
                ..
            } if response_model == model => Ok(SemanticRerankResult {
                model,
                scores,
                timings,
            }),
            other => Err(format!("protocol: unexpected reranker response: {other:?}")),
        }
    }

    async fn send_with_fallback(
        self: &Arc<Self>,
        app: AppHandle,
        request: MlRequest,
        body: Arc<Vec<u8>>,
        timeout: Duration,
        prefer_directml: bool,
    ) -> Result<MlResponse, String> {
        let _request_guard = self.request_gate.lock().await;
        // One deadline spans provider selection, the first attempt, and any CPU
        // fallback, so the caller never waits materially longer than its own timeout.
        let deadline = Instant::now() + timeout;
        let model = request_model(&request);
        let provider = match self.select_provider(&app, prefer_directml, model).await {
            Ok(provider) => provider,
            Err(error) => {
                self.disable_directml_for_session(&error);
                self.restart_process("directml_startup_fallback");
                return self
                    .send_once(&app, MlProvider::Cpu, request, body, deadline)
                    .await;
            }
        };
        let first = self
            .send_once(&app, provider, request.clone(), body.clone(), deadline)
            .await;
        if provider == MlProvider::DirectMl {
            if let Err(error) = &first {
                if should_fallback_from_directml(error) {
                    self.disable_directml_for_session(error);
                    self.restart_process("directml_fallback");
                    return self
                        .send_once(&app, MlProvider::Cpu, request, body, deadline)
                        .await;
                }
            }
        }
        first
    }

    async fn select_provider(
        self: &Arc<Self>,
        app: &AppHandle,
        prefer_directml: bool,
        model: Option<MlSemanticModel>,
    ) -> Result<MlProvider, String> {
        if let Some(provider) = self.cached_provider_for_request(prefer_directml, model) {
            return Ok(provider);
        }

        let state_for_start = self.clone();
        let app_for_start = app.clone();
        tokio::task::spawn_blocking(move || {
            state_for_start.ensure_process(&app_for_start, MlProvider::DirectMl)
        })
        .await
        .map_err(|error| format!("worker_stopped: semantic startup task failed: {error}"))??;

        self.cached_provider_for_request(prefer_directml, model)
            .ok_or_else(|| {
                "protocol: DirectML worker did not publish its supported model set".to_string()
            })
    }

    fn cached_provider_for_request(
        &self,
        prefer_directml: bool,
        model: Option<MlSemanticModel>,
    ) -> Option<MlProvider> {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        route_provider(
            prefer_directml,
            inner.directml_disabled_for_session,
            model,
            inner.directml_supported_models.as_deref(),
        )
    }

    async fn send_once(
        self: &Arc<Self>,
        app: &AppHandle,
        provider: MlProvider,
        mut request: MlRequest,
        body: Arc<Vec<u8>>,
        deadline: Instant,
    ) -> Result<MlResponse, String> {
        let request_id = request.request_id();
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(format!(
                "timeout: semantic request {request_id} has no remaining budget for {provider:?}"
            ));
        }
        let state_for_start = self.clone();
        let app_for_start = app.clone();
        let process = tokio::task::spawn_blocking(move || {
            state_for_start.ensure_process(&app_for_start, provider)
        })
        .await
        .map_err(|error| format!("worker_stopped: semantic startup task failed: {error}"))??;
        let request_model = request_model(&request);
        if let Some(model) = request_model {
            if !process.supported_models.contains(&model) {
                return Err(format!(
                    "provider_unavailable: {provider:?} worker does not support {}",
                    descriptor(model).model_id
                ));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timeout: semantic request {request_id} has no remaining budget for {provider:?}"
            ));
        }
        // The worker sees the remaining budget, not the caller's original timeout,
        // so a fallback retry cannot restart the clock.
        set_request_timeout(&mut request, remaining);
        let expected = expected_response(&request);
        let started = Instant::now();
        let process_for_request = process.clone();
        let task =
            tokio::task::spawn_blocking(move || process_for_request.request(&request, &body));
        let response = match tokio::time::timeout(remaining + CANCEL_GRACE, task).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => Err(format!(
                "worker_stopped: semantic request task failed: {error}"
            )),
            Err(_) => {
                process.kill();
                self.clear_process("timeout", "semantic worker exceeded deadline");
                Err(format!(
                    "timeout: semantic request {request_id} exceeded {} ms plus grace",
                    remaining.as_millis()
                ))
            }
        };
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        match response {
            Ok(MlResponse::Error {
                request_id: response_id,
                kind,
                message,
            }) if response_id == request_id => {
                let error = format!("{kind}: {message}");
                self.record_failure(&kind, &message, elapsed_ms);
                Err(error)
            }
            Ok(response)
                if response_matches_request(expected, request_id, request_model, &response) =>
            {
                self.record_success(request_model, elapsed_ms);
                Ok(response)
            }
            Ok(response) => {
                let error = format!(
                    "protocol: unexpected semantic response for request {request_id}: {response:?}"
                );
                self.record_failure("protocol", &error, elapsed_ms);
                self.restart_process("protocol_mismatch");
                Err(error)
            }
            Err(error) => {
                let (kind, message) = split_error(&error);
                self.record_failure(kind, message, elapsed_ms);
                if request_failure_requires_restart(kind) {
                    self.restart_process("request_failure");
                }
                Err(error)
            }
        }
    }

    pub fn status(&self) -> SemanticRuntimeStatus {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let exited = inner.process.as_ref().and_then(|process| {
            let mut child = process
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match child.try_wait() {
                Ok(Some(status)) => Some(format!("semantic ML worker exited with {status}")),
                Ok(None) => None,
                Err(error) => Some(format!("failed to query semantic ML worker: {error}")),
            }
        });
        if let Some(error) = exited {
            inner.process = None;
            inner.loaded_model = None;
            inner.state = "failed".to_string();
            inner.failure_count += 1;
            inner.last_error_kind = Some("worker_stopped".to_string());
            inner.last_error = Some(truncate_error(&error));
        }
        let provider = inner
            .process
            .as_ref()
            .map(|process| match process.provider {
                MlProvider::Cpu => "cpu",
                MlProvider::DirectMl => "directml",
            })
            .unwrap_or("none");
        let loaded_descriptor = inner.loaded_model.map(descriptor);
        SemanticRuntimeStatus {
            state: inner.state.clone(),
            provider: provider.to_string(),
            worker_version: inner.worker_version.clone(),
            ort_version: inner.ort_version.clone(),
            runtime_path: inner.runtime_path.clone(),
            loaded_model: loaded_descriptor.map(|model| model.model_id.to_string()),
            model_revision: loaded_descriptor.map(|model| model.revision.to_string()),
            model_fingerprint: inner
                .loaded_model
                .map(expected_model_fingerprint)
                .map(str::to_string),
            restart_count: inner.restart_count,
            success_count: inner.success_count,
            failure_count: inner.failure_count,
            last_error_kind: inner.last_error_kind.clone(),
            last_error: inner.last_error.clone(),
            last_elapsed_ms: inner.last_elapsed_ms,
            directml_disabled_for_session: inner.directml_disabled_for_session,
        }
    }

    pub fn stop(&self) {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.stop_locked();
    }

    fn stop_locked(&self) {
        let process = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.state = "stopping".to_string();
            inner.loaded_model = None;
            inner.process.take()
        };
        if let Some(process) = process {
            process.kill();
        }
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .state = "stopped".to_string();
    }

    fn ensure_process(
        &self,
        app: &AppHandle,
        provider: MlProvider,
    ) -> Result<Arc<SemanticMlChild>, String> {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(process) = &inner.process {
                if process.provider == provider {
                    return Ok(process.clone());
                }
            }
        }
        self.stop_locked();
        self.start_process(app, provider)
    }

    fn start_process(
        &self,
        app: &AppHandle,
        provider: MlProvider,
    ) -> Result<Arc<SemanticMlChild>, String> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .state = "starting".to_string();
        let result = self.start_process_inner(app, provider);
        if let Err(error) = &result {
            let (kind, message) = split_error(error);
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|failure| failure.into_inner());
            inner.process = None;
            inner.loaded_model = None;
            inner.state = "failed".to_string();
            inner.failure_count += 1;
            inner.last_error_kind = Some(kind.to_string());
            inner.last_error = Some(truncate_error(message));
        }
        result
    }

    fn start_process_inner(
        &self,
        app: &AppHandle,
        provider: MlProvider,
    ) -> Result<Arc<SemanticMlChild>, String> {
        let executable = resolve_semantic_executable(app)?;
        let ort_dylib = resolve_ort_dylib(app, &executable)?;
        let runtime_dir = ort_dylib.parent().ok_or_else(|| {
            "provider_unavailable: ONNX Runtime DLL has no parent directory".to_string()
        })?;
        let runtime_path =
            prepend_runtime_search_path(runtime_dir, std::env::var_os("PATH").as_deref())?;
        let appdata = file_in_local_appdata()
            .ok_or_else(|| "model_missing: local app data directory is unavailable".to_string())?;
        let models_root = appdata.join("models");
        let onnx_models_root = appdata.join("models-onnx");
        let mut command = Command::new(&executable);
        command
            .arg("--models-root")
            .arg(&models_root)
            .arg("--onnx-models-root")
            .arg(&onnx_models_root)
            .arg("--ort-dylib")
            .arg(&ort_dylib)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        command.env("PATH", runtime_path);
        if provider == MlProvider::DirectMl {
            command.arg("--directml");
            if let Ok(device_id) = std::env::var("CARBONPAPER_DML_DEVICE_ID") {
                command.arg("--dml-device-id").arg(device_id);
            }
        }
        tracing::info!(
            "[ML:SEMANTIC:SUPERVISOR] starting path={} runtime={} provider={provider:?}",
            executable.display(),
            ort_dylib.display()
        );
        let child = command
            .spawn()
            .map_err(|error| format!("worker_stopped: failed to start semantic worker: {error}"))?;
        let mut pending = PendingSemanticChild::new(child);
        let job = assign_kill_on_close_job(pending.child())?;
        let stdin = pending
            .child_mut()
            .stdin
            .take()
            .ok_or("worker_stopped: semantic worker stdin unavailable")?;
        let stdout = pending
            .child_mut()
            .stdout
            .take()
            .ok_or("worker_stopped: semantic worker stdout unavailable")?;
        if let Some(stderr) = pending.child_mut().stderr.take() {
            std::thread::Builder::new()
                .name("carbonpaper-semantic-log".to_string())
                .spawn(move || {
                    use std::io::BufRead;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        tracing::info!("[ML:SEMANTIC:WORKER] {line}");
                    }
                })
                .map_err(|error| format!("worker_stopped: failed to start log reader: {error}"))?;
        }

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("carbonpaper-semantic-handshake".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let ready = read_response(&mut reader);
                let _ = sender.send((ready, reader));
            })
            .map_err(|error| format!("worker_stopped: failed to start handshake: {error}"))?;
        let (ready, stdout) = receiver
            .recv_timeout(STARTUP_TIMEOUT)
            .map_err(|_| "timeout: semantic worker startup timed out".to_string())?;
        match ready {
            Ok(MlResponse::SemanticReady {
                protocol_version,
                worker_version,
                ort_version,
                provider: ready_provider,
                supported_models,
            }) if protocol_version == ML_PROTOCOL_VERSION && ready_provider == provider => {
                let child = pending.take();
                let process = Arc::new(SemanticMlChild {
                    provider,
                    supported_models: supported_models.clone(),
                    child: Mutex::new(child),
                    stdin: Mutex::new(BufWriter::new(stdin)),
                    stdout: Mutex::new(stdout),
                    request_lock: Mutex::new(()),
                    _job: job,
                });
                let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
                inner.process = Some(process.clone());
                if provider == MlProvider::DirectMl {
                    inner.directml_supported_models = Some(supported_models);
                }
                inner.state = "ready".to_string();
                inner.worker_version = Some(worker_version);
                inner.ort_version = Some(ort_version);
                inner.runtime_path = Some(ort_dylib.display().to_string());
                inner.loaded_model = None;
                inner.last_error_kind = None;
                inner.last_error = None;
                Ok(process)
            }
            Ok(response) => Err(format!(
                "protocol: invalid semantic worker handshake: {response:?}"
            )),
            Err(error) => Err(format!(
                "worker_stopped: semantic worker startup failed: {error}"
            )),
        }
    }

    fn record_success(&self, model: Option<MlSemanticModel>, elapsed_ms: f64) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.state = "ready".to_string();
        inner.success_count += 1;
        if model.is_some() {
            inner.loaded_model = model;
        }
        inner.last_error_kind = None;
        inner.last_error = None;
        inner.last_elapsed_ms = Some(elapsed_ms);
    }

    fn record_failure(&self, kind: &str, message: &str, elapsed_ms: f64) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.state = "degraded".to_string();
        inner.failure_count += 1;
        inner.last_error_kind = Some(kind.to_string());
        inner.last_error = Some(truncate_error(message));
        inner.last_elapsed_ms = Some(elapsed_ms);
    }

    fn disable_directml_for_session(&self, error: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|failure| failure.into_inner());
        inner.directml_disabled_for_session = true;
        inner.last_error_kind = Some("provider_unavailable".to_string());
        inner.last_error = Some(truncate_error(error));
    }

    fn restart_process(&self, reason: &str) {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let process = {
            let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
            inner.restart_count += 1;
            inner.state = "restarting".to_string();
            inner.loaded_model = None;
            inner.process.take()
        };
        if let Some(process) = process {
            tracing::warn!("[ML:SEMANTIC:WATCHDOG] stopping worker reason={reason}");
            process.kill();
        }
    }

    fn clear_process(&self, kind: &str, message: &str) {
        let _guard = self
            .lifecycle_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.restart_count += 1;
        inner.process = None;
        inner.loaded_model = None;
        inner.state = "degraded".to_string();
        inner.last_error_kind = Some(kind.to_string());
        inner.last_error = Some(truncate_error(message));
    }
}

impl Drop for SemanticRuntimeState {
    fn drop(&mut self) {
        self.stop();
    }
}

fn request_model(request: &MlRequest) -> Option<MlSemanticModel> {
    match request {
        MlRequest::EmbedText { model, .. }
        | MlRequest::EmbedImage { model, .. }
        | MlRequest::InspectTokenization { model, .. }
        | MlRequest::Rerank { model, .. } => Some(*model),
        MlRequest::Unload { .. } => None,
        _ => None,
    }
}

fn set_request_timeout(request: &mut MlRequest, timeout: Duration) {
    match request {
        MlRequest::Ocr { timeout_ms, .. }
        | MlRequest::EmbedText { timeout_ms, .. }
        | MlRequest::EmbedImage { timeout_ms, .. }
        | MlRequest::Rerank { timeout_ms, .. } => *timeout_ms = duration_ms(timeout),
        MlRequest::Ping { .. }
        | MlRequest::InspectTokenization { .. }
        | MlRequest::SemanticStatus { .. }
        | MlRequest::Unload { .. }
        | MlRequest::Shutdown { .. } => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedResponse {
    Pong,
    Ocr,
    Embedding,
    Tokenization,
    Rerank,
    Status,
    Unloaded,
    ShuttingDown,
}

fn expected_response(request: &MlRequest) -> ExpectedResponse {
    match request {
        MlRequest::Ping { .. } => ExpectedResponse::Pong,
        MlRequest::Ocr { .. } => ExpectedResponse::Ocr,
        MlRequest::EmbedText { .. } | MlRequest::EmbedImage { .. } => ExpectedResponse::Embedding,
        MlRequest::InspectTokenization { .. } => ExpectedResponse::Tokenization,
        MlRequest::Rerank { .. } => ExpectedResponse::Rerank,
        MlRequest::SemanticStatus { .. } => ExpectedResponse::Status,
        MlRequest::Unload { .. } => ExpectedResponse::Unloaded,
        MlRequest::Shutdown { .. } => ExpectedResponse::ShuttingDown,
    }
}

/// A response only counts as a success when its id, variant, and model all match
/// the request; anything else is a protocol violation even if the id lines up.
fn response_matches_request(
    expected: ExpectedResponse,
    request_id: u64,
    model: Option<MlSemanticModel>,
    response: &MlResponse,
) -> bool {
    if response_request_id(response) != Some(request_id) {
        return false;
    }
    match (expected, response) {
        (ExpectedResponse::Pong, MlResponse::Pong { .. })
        | (ExpectedResponse::Ocr, MlResponse::OcrComplete { .. })
        | (ExpectedResponse::Status, MlResponse::SemanticStatus { .. })
        | (ExpectedResponse::Unloaded, MlResponse::Unloaded { .. })
        | (ExpectedResponse::ShuttingDown, MlResponse::ShuttingDown { .. }) => true,
        (ExpectedResponse::Embedding, MlResponse::EmbeddingComplete { model: actual, .. })
        | (
            ExpectedResponse::Tokenization,
            MlResponse::TokenizationComplete { model: actual, .. },
        )
        | (ExpectedResponse::Rerank, MlResponse::RerankComplete { model: actual, .. }) => {
            Some(*actual) == model
        }
        _ => false,
    }
}

fn route_provider(
    prefer_directml: bool,
    directml_disabled_for_session: bool,
    model: Option<MlSemanticModel>,
    directml_supported_models: Option<&[MlSemanticModel]>,
) -> Option<MlProvider> {
    if !prefer_directml || directml_disabled_for_session {
        return Some(MlProvider::Cpu);
    }
    let Some(model) = model else {
        return Some(MlProvider::DirectMl);
    };
    directml_supported_models.map(|supported_models| {
        if supported_models.contains(&model) {
            MlProvider::DirectMl
        } else {
            MlProvider::Cpu
        }
    })
}

fn response_request_id(response: &MlResponse) -> Option<u64> {
    match response {
        MlResponse::Pong { request_id }
        | MlResponse::OcrComplete { request_id, .. }
        | MlResponse::EmbeddingComplete { request_id, .. }
        | MlResponse::TokenizationComplete { request_id, .. }
        | MlResponse::RerankComplete { request_id, .. }
        | MlResponse::SemanticStatus { request_id, .. }
        | MlResponse::Unloaded { request_id }
        | MlResponse::Error { request_id, .. }
        | MlResponse::ShuttingDown { request_id } => Some(*request_id),
        MlResponse::Ready { .. } | MlResponse::SemanticReady { .. } => None,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().clamp(1, u64::MAX as u128) as u64
}

/// Error kinds emitted by the protocol layer, the semantic engine, and this
/// supervisor. `split_error` only trusts these prefixes; anything else is
/// classified as `protocol`, which deliberately sits outside the DirectML
/// fallback list so a malformed message can never disable DirectML or mask a
/// caller bug as an inference failure.
const KNOWN_ERROR_KINDS: &[&str] = &[
    "invalid_request",
    "limit_exceeded",
    "timeout",
    "cancelled",
    "model_missing",
    "model_mismatch",
    "provider_unavailable",
    "inference",
    "protocol",
    "transport",
    "worker_stopped",
];

fn split_error(error: &str) -> (&str, &str) {
    match error.split_once(": ") {
        Some((kind, message)) if KNOWN_ERROR_KINDS.contains(&kind) => (kind, message),
        _ => ("protocol", error),
    }
}

fn should_fallback_from_directml(error: &str) -> bool {
    matches!(
        split_error(error).0,
        "provider_unavailable" | "inference" | "timeout" | "worker_stopped" | "transport"
    )
}

/// `invalid_request` and `limit_exceeded` on this path can only come from local
/// validation that rejects a request before any byte reaches the pipe; the worker
/// and its loaded model are still healthy, so restarting would only throw away a
/// warm model. Worker-reported errors of the same kinds arrive as
/// `MlResponse::Error` and never reach this classification.
fn request_failure_requires_restart(kind: &str) -> bool {
    !matches!(kind, "invalid_request" | "limit_exceeded")
}

fn truncate_error(error: &str) -> String {
    error.chars().take(500).collect()
}

fn prepend_runtime_search_path(
    runtime_dir: &Path,
    inherited_path: Option<&OsStr>,
) -> Result<OsString, String> {
    let mut paths = vec![runtime_dir.to_path_buf()];
    if let Some(inherited_path) = inherited_path {
        paths.extend(std::env::split_paths(inherited_path));
    }
    std::env::join_paths(paths).map_err(|error| {
        format!(
            "provider_unavailable: failed to construct semantic runtime DLL search path: {error}"
        )
    })
}

fn resolve_semantic_executable(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(path) = find_existing_file_in_resources(app, "carbonpaper-semantic-worker.exe") {
        return Ok(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            let sibling = directory.join("carbonpaper-semantic-worker.exe");
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    let debug = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("semantic-worker")
        .join("target")
        .join("debug")
        .join("carbonpaper-semantic-worker.exe");
    if debug.is_file() {
        return Ok(debug);
    }
    Err("worker_stopped: carbonpaper-semantic-worker.exe was not found".to_string())
}

fn resolve_ort_dylib(app: &AppHandle, executable: &std::path::Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("CARBONPAPER_ORT_DYLIB_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(directory) = executable.parent() {
        candidates.push(directory.join("onnxruntime.dll"));
    }
    if let Some(path) = find_existing_file_in_resources(app, "onnxruntime/1.24.2/onnxruntime.dll") {
        candidates.push(path);
    }
    if let Some(appdata) = file_in_local_appdata() {
        candidates.push(
            appdata
                .join("models-onnx")
                .join("runtime")
                .join("1.24.2")
                .join("onnxruntime.dll"),
        );
        // One-release development/legacy fallback. Production packaging installs the
        // pinned runtime independently of Python before semantic cutover.
        candidates.push(
            appdata
                .join(".venv")
                .join("Lib")
                .join("site-packages")
                .join("onnxruntime")
                .join("capi")
                .join("onnxruntime.dll"),
        );
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "provider_unavailable: pinned ONNX Runtime 1.24.2 DLL was not found".to_string()
        })
}

fn assign_kill_on_close_job(child: &Child) -> Result<SemanticJobHandle, String> {
    // SAFETY: all Windows handles are checked before use and ownership is transferred to
    // `SemanticJobHandle` only after successful assignment.
    unsafe {
        let job = CreateJobObjectW(None, None)
            .map_err(|error| format!("worker_stopped: failed to create Job Object: {error}"))?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(error) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(job);
            return Err(format!(
                "worker_stopped: failed to configure Job Object: {error}"
            ));
        }
        let process_handle = HANDLE(child.as_raw_handle() as *mut _);
        if let Err(error) = AssignProcessToJobObject(job, process_handle) {
            let _ = CloseHandle(job);
            return Err(format!(
                "worker_stopped: failed to assign semantic worker to Job Object: {error}"
            ));
        }
        Ok(SemanticJobHandle(job))
    }
}

#[tauri::command]
pub fn get_ml_semantic_status(
    state: tauri::State<'_, Arc<SemanticRuntimeState>>,
) -> SemanticRuntimeStatus {
    state.status()
}

#[tauri::command]
pub fn restart_ml_semantic_worker(
    window: tauri::Window,
    state: tauri::State<'_, Arc<SemanticRuntimeState>>,
) -> Result<SemanticRuntimeStatus, String> {
    crate::commands::check_main_window(&window)?;
    state.stop();
    Ok(state.status())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_runtime_is_stopped_and_does_not_claim_a_provider() {
        let runtime = SemanticRuntimeState::new();
        let status = runtime.status();
        assert_eq!(status.state, "stopped");
        assert_eq!(status.provider, "none");
        assert!(status.loaded_model.is_none());
    }

    #[test]
    fn response_ids_cover_semantic_success_variants() {
        let response = MlResponse::Unloaded { request_id: 7 };
        assert_eq!(response_request_id(&response), Some(7));
    }

    #[test]
    fn cpu_only_model_is_routed_to_cpu_from_worker_capabilities() {
        let supported_models = [MlSemanticModel::ChineseClip, MlSemanticModel::BgeSmallZh];
        assert_eq!(
            route_provider(
                true,
                false,
                Some(MlSemanticModel::MinilmL12),
                Some(&supported_models),
            ),
            Some(MlProvider::Cpu)
        );
        assert_eq!(
            route_provider(
                true,
                false,
                Some(MlSemanticModel::BgeRerankerV2M3),
                Some(&supported_models),
            ),
            Some(MlProvider::Cpu)
        );
    }

    #[test]
    fn cpu_only_route_keeps_directml_available_for_supported_models() {
        let runtime = SemanticRuntimeState::new();
        runtime
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .directml_supported_models = Some(vec![
            MlSemanticModel::ChineseClip,
            MlSemanticModel::BgeSmallZh,
        ]);

        assert_eq!(
            runtime.cached_provider_for_request(true, Some(MlSemanticModel::MinilmL12)),
            Some(MlProvider::Cpu)
        );
        assert!(!runtime.status().directml_disabled_for_session);
        assert_eq!(
            runtime.cached_provider_for_request(true, Some(MlSemanticModel::ChineseClip)),
            Some(MlProvider::DirectMl)
        );
        assert_eq!(
            runtime.cached_provider_for_request(true, Some(MlSemanticModel::BgeSmallZh)),
            Some(MlProvider::DirectMl)
        );
    }

    #[test]
    fn directml_fallback_is_limited_to_provider_and_runtime_failures() {
        for error in [
            "provider_unavailable: DirectML is unavailable",
            "inference: DirectML kernel failed",
            "timeout: DirectML request timed out",
            "worker_stopped: DirectML worker exited",
            "transport: failed to read ML frame length: pipe closed",
        ] {
            assert!(should_fallback_from_directml(error), "{error}");
        }

        for error in [
            "invalid_request: unsupported operation",
            "limit_exceeded: batch is too large",
            "model_missing: model has not been downloaded",
            "model_mismatch: model checksum differs",
            "cancelled: request was cancelled",
            "protocol: response id differs",
            // Unprefixed or unrecognized messages must classify as protocol, not
            // inference, so they can never disable DirectML for the session.
            "semantic text batch must contain 1..=32 items",
            "ML request body length mismatch: header=1, actual=2",
        ] {
            assert!(!should_fallback_from_directml(error), "{error}");
        }
    }

    #[test]
    fn local_validation_failures_do_not_restart_the_worker() {
        assert!(!request_failure_requires_restart("invalid_request"));
        assert!(!request_failure_requires_restart("limit_exceeded"));
        for kind in [
            "timeout",
            "transport",
            "protocol",
            "worker_stopped",
            "inference",
            "model_missing",
            "model_mismatch",
        ] {
            assert!(request_failure_requires_restart(kind), "{kind}");
        }
    }

    #[test]
    fn unrecognized_error_kinds_are_classified_as_protocol() {
        assert_eq!(
            split_error("inference: kernel failed"),
            ("inference", "kernel failed")
        );
        assert_eq!(
            split_error("transport: failed to write ML frame: broken pipe"),
            ("transport", "failed to write ML frame: broken pipe")
        );
        assert_eq!(
            split_error("semantic text batch must contain 1..=32 items").0,
            "protocol"
        );
        assert_eq!(
            split_error("ML timeout must be within 1..=600000 ms: 600001").0,
            "protocol"
        );
    }

    #[test]
    fn success_requires_matching_response_variant_and_model() {
        let request = MlRequest::EmbedText {
            request_id: 9,
            timeout_ms: 1_000,
            model: MlSemanticModel::BgeSmallZh,
            texts: vec!["text".to_string()],
        };
        let expected = expected_response(&request);
        let model = request_model(&request);
        let timings = MlSemanticTimings {
            model_load_ms: 0.0,
            preprocess_ms: 0.0,
            inference_ms: 0.0,
            request_total_ms: 0.0,
        };

        let matching = MlResponse::EmbeddingComplete {
            request_id: 9,
            model: MlSemanticModel::BgeSmallZh,
            dimensions: 2,
            vectors: vec![vec![0.0, 1.0]],
            timings: timings.clone(),
        };
        assert!(response_matches_request(expected, 9, model, &matching));

        let wrong_model = MlResponse::EmbeddingComplete {
            request_id: 9,
            model: MlSemanticModel::MinilmL12,
            dimensions: 2,
            vectors: vec![vec![0.0, 1.0]],
            timings,
        };
        assert!(!response_matches_request(expected, 9, model, &wrong_model));

        let wrong_variant = MlResponse::ShuttingDown { request_id: 9 };
        assert!(!response_matches_request(
            expected,
            9,
            model,
            &wrong_variant
        ));

        let wrong_id = MlResponse::Unloaded { request_id: 8 };
        let unload = MlRequest::Unload { request_id: 9 };
        assert!(!response_matches_request(
            expected_response(&unload),
            9,
            None,
            &wrong_id
        ));
    }

    #[test]
    fn retry_budget_is_rewritten_into_the_request_frame() {
        let mut request = MlRequest::EmbedText {
            request_id: 1,
            timeout_ms: 600_000,
            model: MlSemanticModel::BgeSmallZh,
            texts: vec!["text".to_string()],
        };
        set_request_timeout(&mut request, Duration::from_millis(1_500));
        match request {
            MlRequest::EmbedText { timeout_ms, .. } => assert_eq!(timeout_ms, 1_500),
            other => panic!("unexpected request variant: {other:?}"),
        }
    }

    #[test]
    fn semantic_runtime_directory_is_first_in_worker_path() {
        let runtime_dir = Path::new(r"C:\CarbonPaper\onnxruntime\1.24.2");
        let inherited = OsStr::new(r"C:\Windows\System32;C:\Windows");
        let joined = prepend_runtime_search_path(runtime_dir, Some(inherited)).unwrap();
        let paths = std::env::split_paths(&joined).collect::<Vec<_>>();
        assert_eq!(paths.first().map(PathBuf::as_path), Some(runtime_dir));
        assert!(paths
            .iter()
            .any(|path| path == Path::new(r"C:\Windows\System32")));
    }
}
