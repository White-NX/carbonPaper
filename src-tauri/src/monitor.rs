//! Python monitor process orchestration and resource-control policy.
//!
//! This module owns monitor startup, authenticated named-pipe communication, Job Object
//! limits, game-mode suppression, restart behavior, and frontend lifecycle events.

use crate::capture::CaptureState;
#[cfg(test)]
use crate::monitor_ipc::parse_ipc_response;
use crate::monitor_ipc::{
    generate_auth_token, generate_random_pipe_name, inject_ipc_auth, send_ipc_request_on_client,
};
use crate::resource_utils::{find_existing_file_in_resources, normalize_path_for_command};
use crate::reverse_ipc::{
    generate_reverse_ipc_auth_token, generate_reverse_pipe_name, ReverseIpcServer,
};
use crate::storage::StorageState;
use std::collections::HashSet;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::Mutex as AsyncMutex;

use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Performance::*;

#[derive(Clone, Debug)]
struct MonitorRecoveryState {
    state: String,
    policy: String,
    restart_available: bool,
    last_exit_code: Option<String>,
    last_error: Option<String>,
    last_crashed_at_ms: Option<u64>,
    crash_count: u64,
}

impl Default for MonitorRecoveryState {
    fn default() -> Self {
        Self {
            state: "stopped".to_string(),
            policy: "manual_restart".to_string(),
            restart_available: true,
            last_exit_code: None,
            last_error: None,
            last_crashed_at_ms: None,
            crash_count: 0,
        }
    }
}

impl MonitorRecoveryState {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "policy": self.policy,
            "restart_available": self.restart_available,
            "last_exit_code": self.last_exit_code,
            "last_error": self.last_error,
            "last_crashed_at_ms": self.last_crashed_at_ms,
            "crash_count": self.crash_count,
        })
    }
}

pub struct MonitorState {
    pub process: Mutex<Option<Child>>,
    pub pipe_name: Mutex<Option<String>>,
    pub auth_token: Mutex<Option<String>>,
    pub request_counter: AtomicU64,
    /// Reverse IPC server instance for receiving storage requests from Python
    pub reverse_ipc: Mutex<Option<ReverseIpcServer>>,
    /// Reverse IPC pipe name
    pub reverse_pipe_name: Mutex<Option<String>>,
    /// Job Object handle to manage monitor process and its children
    pub job_handle: Mutex<Option<JobHandle>>,
    /// Game mode: whether DirectML is currently suppressed due to game mode
    pub game_mode_dml_suppressed: AtomicBool,
    /// Game mode: whether the monitor is permanently suppressed due to game mode (until next restart)
    pub game_mode_permanently_suppressed: AtomicBool,
    /// Game mode: background task handle for monitoring game mode changes (so we can stop it when monitor stops)
    pub game_mode_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    /// Set to true during intentional stop_monitor to suppress the watcher's monitor-exited event
    pub stopping: AtomicBool,
    /// Prevents the monitor from restarting during migration tasks
    pub migration_lock: AtomicBool,
    recovery: Mutex<MonitorRecoveryState>,
    python_ipc_client: AsyncMutex<Option<PersistentIpcClient>>,
}

struct PersistentIpcClient {
    pipe_name: String,
    client: NamedPipeClient,
    requests: u64,
}

impl MonitorState {
    pub fn new() -> Self {
        Self {
            process: Mutex::new(None),
            pipe_name: Mutex::new(None),
            auth_token: Mutex::new(None),
            request_counter: AtomicU64::new(0),
            reverse_ipc: Mutex::new(None),
            reverse_pipe_name: Mutex::new(None),
            job_handle: Mutex::new(None),
            game_mode_dml_suppressed: AtomicBool::new(false),
            game_mode_permanently_suppressed: AtomicBool::new(false),
            game_mode_task: Mutex::new(None),
            stopping: AtomicBool::new(false),
            migration_lock: AtomicBool::new(false),
            recovery: Mutex::new(MonitorRecoveryState::default()),
            python_ipc_client: AsyncMutex::new(None),
        }
    }
}

pub struct JobHandle(HANDLE);

impl JobHandle {
    pub fn new(handle: HANDLE) -> Self {
        Self(handle)
    }
}

impl Deref for JobHandle {
    type Target = HANDLE;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns the live Job Object handle and closes it
        // exactly once after the monitor state releases the wrapper.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: Windows Job Object handles are opaque kernel references that may be moved
// between threads; Rust ownership still guarantees a single close.
unsafe impl Send for JobHandle {}
// SAFETY: concurrent Job Object operations do not expose aliased Rust memory and are
// supported by the Windows kernel handle model.
unsafe impl Sync for JobHandle {}

// Retry policy for monitor startup and named-pipe connection establishment.
const MAX_RETRIES: u32 = 10;
const RETRY_DELAY_MS: u64 = 100;
const STARTUP_MAX_WAIT_MS: u64 = 15_000;
const STARTUP_LOG_TAIL_LINES: usize = 50;

use serde_json::Value;

const MAX_MONITOR_COMMAND_PAYLOAD_BYTES: usize = 256 * 1024;

fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn monitor_recovery_snapshot(state: &MonitorState) -> Value {
    state
        .recovery
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .to_json()
}

fn stopped_monitor_status(state: &MonitorState) -> Value {
    serde_json::json!({
        "paused": false,
        "stopped": true,
        "interval": 0,
        "recovery": monitor_recovery_snapshot(state),
    })
}

fn set_monitor_recovery_starting(state: &MonitorState) {
    let mut recovery = state.recovery.lock().unwrap_or_else(|e| e.into_inner());
    recovery.state = "starting".to_string();
    recovery.restart_available = false;
    recovery.last_error = None;
}

fn set_monitor_recovery_running(state: &MonitorState) {
    let mut recovery = state.recovery.lock().unwrap_or_else(|e| e.into_inner());
    recovery.state = "running".to_string();
    recovery.restart_available = false;
    recovery.last_error = None;
    recovery.last_exit_code = None;
    recovery.last_crashed_at_ms = None;
}

fn set_monitor_recovery_stopped(state: &MonitorState) {
    let mut recovery = state.recovery.lock().unwrap_or_else(|e| e.into_inner());
    recovery.state = "stopped".to_string();
    recovery.restart_available = true;
    recovery.last_error = None;
    recovery.last_exit_code = None;
    recovery.last_crashed_at_ms = None;
}

fn set_monitor_recovery_failed(state: &MonitorState, error: String) -> Value {
    let mut recovery = state.recovery.lock().unwrap_or_else(|e| e.into_inner());
    recovery.state = "failed".to_string();
    recovery.restart_available = true;
    recovery.last_error = Some(error);
    recovery.to_json()
}

fn set_monitor_recovery_crashed(
    state: &MonitorState,
    exit_code: String,
    error: Option<String>,
) -> Value {
    let mut recovery = state.recovery.lock().unwrap_or_else(|e| e.into_inner());
    recovery.state = "crashed".to_string();
    recovery.restart_available = true;
    recovery.last_exit_code = Some(exit_code);
    recovery.last_error = error;
    recovery.last_crashed_at_ms = Some(current_epoch_ms());
    recovery.crash_count = recovery.crash_count.saturating_add(1);
    recovery.to_json()
}

fn cleanup_monitor_runtime_after_unexpected_exit(state: &MonitorState) {
    {
        let mut guard = state.reverse_ipc.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut server) = *guard {
            server.stop();
        }
        *guard = None;
    }
    {
        let mut guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state
            .reverse_pipe_name
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state.job_handle.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    if let Ok(mut guard) = state.python_ipc_client.try_lock() {
        *guard = None;
    }
}

// Windows error code for ERROR_PIPE_BUSY
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

// 内部函数：尝试连接到管道，支持重试
async fn connect_to_pipe(
    pipe_name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    let mut last_error = String::new();
    let full_pipe_name = format!(r"\\.\pipe\{}", pipe_name);

    let start = std::time::Instant::now();
    for attempt in 0..MAX_RETRIES {
        match ClientOptions::new().open(&full_pipe_name) {
            Ok(client) => {
                if attempt > 0 && start.elapsed().as_secs() >= 5 {
                    tracing::warn!(
                        "[DIAG:PIPE] Connected after {} retries, {:?} elapsed",
                        attempt,
                        start.elapsed()
                    );
                }
                return Ok(client);
            }
            Err(e) => {
                // Check if it's ERROR_PIPE_BUSY (231)
                let is_pipe_busy = e.raw_os_error() == Some(ERROR_PIPE_BUSY);

                if start.elapsed().as_secs() >= 5 {
                    tracing::warn!(
                        "[DIAG:PIPE] Attempt {}/{} failed: {} (elapsed {:?})",
                        attempt + 1,
                        MAX_RETRIES,
                        e,
                        start.elapsed()
                    );
                }

                if is_pipe_busy && attempt < MAX_RETRIES - 1 {
                    // Wait and retry
                    tokio::time::sleep(tokio::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }

                last_error = format!("Failed to connect to pipe: {}. Is monitor running?", e);
            }
        }
    }

    tracing::warn!(
        "[DIAG:PIPE] All {} retries failed after {:?}",
        MAX_RETRIES,
        start.elapsed()
    );
    Err(last_error)
}

pub async fn send_ipc_request_reused(
    state: &MonitorState,
    pipe_name: &str,
    auth_token: &str,
    seq_no: u64,
    req: Value,
) -> Result<Value, String> {
    let mut req = inject_ipc_auth(req, auth_token, seq_no);

    let command_name = req
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    let requested_timeout_secs = req
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .map(|v| v.saturating_add(1));
    let default_timeout_secs = match command_name.as_str() {
        "stop" => 2,
        "status" | "pause" | "resume" | "continue" => 5,
        _ => 30,
    };
    let ipc_timeout_secs = requested_timeout_secs
        .unwrap_or(default_timeout_secs)
        .clamp(1, 605);
    let ipc_started = std::time::Instant::now();
    let keepalive = command_name != "stop";
    if let Some(obj) = req.as_object_mut() {
        obj.insert("_ipc_keepalive".to_string(), Value::Bool(keepalive));
    }

    let mut persistent = {
        let mut guard = state.python_ipc_client.lock().await;
        match guard.take() {
            Some(existing) if existing.pipe_name == pipe_name => existing,
            _ => {
                drop(guard);
                let client = connect_to_pipe(pipe_name).await?;
                tracing::debug!(
                    "[DIAG:IPC] persistent connection established pipe={}",
                    pipe_name
                );
                PersistentIpcClient {
                    pipe_name: pipe_name.to_string(),
                    client,
                    requests: 0,
                }
            }
        }
    };

    let result = send_ipc_request_on_client(&mut persistent.client, &req, ipc_timeout_secs).await;

    match &result {
        Ok(_) if keepalive => {
            persistent.requests = persistent.requests.saturating_add(1);
            if persistent.requests % 100 == 0 {
                tracing::debug!(
                    "[DIAG:IPC] persistent request done command={} seq_no={} reused_count={} elapsed={}ms",
                    command_name,
                    seq_no,
                    persistent.requests,
                    ipc_started.elapsed().as_millis()
                );
            }
            let pipe_still_current = {
                let guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
                guard.as_deref() == Some(pipe_name)
            };
            let mut guard = state.python_ipc_client.lock().await;
            if pipe_still_current && guard.is_none() {
                *guard = Some(persistent);
            } else {
                tracing::debug!(
                    "[DIAG:IPC] dropping reusable connection command={} pipe_current={} newer_client={}",
                    command_name,
                    pipe_still_current,
                    guard.is_some()
                );
            }
        }
        Ok(_) => {
            tracing::debug!(
                "[DIAG:IPC] closing persistent connection after command={}",
                command_name
            );
        }
        Err(e) => {
            tracing::warn!(
                "[DIAG:IPC] persistent connection discarded command={} seq_no={} error={}",
                command_name,
                seq_no,
                e
            );
        }
    }

    result
}

/// Updates the Job Object CPU cap at runtime.
fn set_job_cpu_limit(handle: HANDLE, enabled: bool, percent: u32) -> Result<(), String> {
    // SAFETY: `handle` is a live Job Object owned by monitor state; `cpu_info` has the
    // exact layout and byte size required by `JobObjectCpuRateControlInformation`.
    unsafe {
        let mut cpu_info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
        if enabled && percent > 0 {
            cpu_info.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            cpu_info.Anonymous.CpuRate = percent * 100;
        }
        // ControlFlags = 0 means disabled
        SetInformationJobObject(
            handle,
            JobObjectCpuRateControlInformation,
            &cpu_info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
        )
        .map_err(|e| format!("Failed to set CPU limit: {:?}", e))?;
        Ok(())
    }
}

/// RAII guard that restores the configured Job Object CPU cap on drop.
struct CpuLimitGuard {
    // Store the value rather than HANDLE so the guard can cross async thread boundaries.
    job_handle_raw: isize,
}

// SAFETY: the stored value names a Job Object kept alive by `MonitorState` for the
// guard's lifetime; moving the integer between threads does not transfer ownership.
unsafe impl Send for CpuLimitGuard {}

impl Drop for CpuLimitGuard {
    fn drop(&mut self) {
        let handle = HANDLE(self.job_handle_raw as *mut std::ffi::c_void);
        let enabled = crate::registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
        let percent = crate::registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
        let _ = set_job_cpu_limit(handle, enabled, percent);
    }
}

fn validate_monitor_command_payload(payload: &Value) -> Result<(), String> {
    let payload_size = serde_json::to_vec(payload)
        .map_err(|e| format!("Invalid monitor payload: {}", e))?
        .len();
    if payload_size > MAX_MONITOR_COMMAND_PAYLOAD_BYTES {
        return Err("Monitor command payload too large".to_string());
    }
    Ok(())
}

fn apply_monitor_side_effects(
    command: &str,
    payload: &Value,
    capture_state: Option<&CaptureState>,
    storage: Option<&StorageState>,
) {
    match command {
        "update_filters" => {
            let Some(capture_state) = capture_state else {
                return;
            };
            let Some(storage) = storage else {
                return;
            };
            // Update Rust-side exclusion settings
            let filters = payload
                .get("filters")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let processes = filters
                .get("processes")
                .or_else(|| payload.get("processes"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                });
            let titles = filters
                .get("titles")
                .or_else(|| payload.get("titles"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                });
            let ignore_protected = filters
                .get("ignore_protected")
                .or_else(|| payload.get("ignore_protected"))
                .and_then(|v| v.as_bool());

            {
                let data_dir = storage
                    .data_dir
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                capture_state.update_exclusion_settings(processes, titles, ignore_protected);
                capture_state.save_exclusion_settings(&data_dir);
            }
        }
        "update_advanced_config" => {
            let Some(capture_state) = capture_state else {
                return;
            };
            let ocr_timeout_secs = payload
                .get("ocr_timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| {
                    crate::registry_config::get_u32("ocr_timeout_secs").unwrap_or(120) as u64
                })
                .clamp(30, 600) as u32;

            capture_state
                .ocr_timeout_secs
                .store(ocr_timeout_secs, Ordering::SeqCst);
        }
        _ => {}
    }
}

async fn dispatch_typed_monitor_command(
    state: &MonitorState,
    capture_state: Option<&CaptureState>,
    storage: Option<&StorageState>,
    payload: Value,
) -> Result<Value, String> {
    validate_monitor_command_payload(&payload)?;
    let command = payload
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    apply_monitor_side_effects(command, &payload, capture_state, storage);
    forward_command_to_python(&state, payload).await
}

async fn authenticated_monitor_command(
    credential_state: &crate::credential_manager::CredentialManagerState,
    state: &MonitorState,
    payload: Value,
) -> Result<Value, String> {
    crate::commands::check_auth_required(credential_state)?;
    dispatch_typed_monitor_command(state, None, None, payload).await
}

#[tauri::command]
pub async fn monitor_search_nl(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    query: String,
    limit: Option<u32>,
    offset: Option<u32>,
    process_names: Option<Vec<String>>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    fuzzy: Option<bool>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "search_nl",
            "query": query,
            "limit": limit.unwrap_or(20).min(200),
            "offset": offset.unwrap_or(0),
            "process_names": process_names.unwrap_or_default(),
            "start_time": start_time,
            "end_time": end_time,
            "fuzzy": fuzzy.unwrap_or(true),
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_update_filters(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    storage: State<'_, Arc<StorageState>>,
    filters: Value,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let payload = serde_json::json!({
        "command": "update_filters",
        "filters": filters,
    });
    dispatch_typed_monitor_command(&state, Some(&capture_state), Some(&storage), payload).await
}

#[tauri::command]
pub async fn monitor_update_advanced_config(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    ocr_timeout_secs: u32,
    clustering_allow_full_low_memory: bool,
) -> Result<Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    let payload = serde_json::json!({
        "command": "update_advanced_config",
        "ocr_timeout_secs": ocr_timeout_secs,
        "clustering_allow_full_low_memory": clustering_allow_full_low_memory,
    });
    dispatch_typed_monitor_command(&state, Some(&capture_state), None, payload).await
}

#[tauri::command]
pub async fn monitor_update_feature_config(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    clustering_enabled: bool,
    classification_enabled: bool,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "update_feature_config",
            "clustering_enabled": clustering_enabled,
            "classification_enabled": classification_enabled,
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_run_clustering(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    clustering_mode: Option<String>,
    manual: Option<bool>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "run_clustering",
            "start_time": start_time,
            "end_time": end_time,
            "clustering_mode": clustering_mode.unwrap_or_else(|| "auto".to_string()),
            "manual": manual.unwrap_or(false),
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_get_clustering_status(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "get_clustering_status" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_set_clustering_interval(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    interval: String,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "set_clustering_interval", "interval": interval }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_get_task_clusters(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "get_tasks" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_nl_cluster_query(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    query: String,
    n_results: Option<u32>,
    enable_rerank: Option<bool>,
    rerank_variant: Option<String>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "nl_cluster_query",
            "query": query,
            "n_results": n_results.unwrap_or(30).min(200),
            "enable_rerank": enable_rerank.unwrap_or(false),
            "rerank_variant": rerank_variant.unwrap_or_else(|| "q4f16".to_string()),
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_nl_cluster_reranker_status(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "nl_cluster_reranker_status" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_smart_cluster_worker_status(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "smart_cluster_worker_status" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_smart_cluster_drain_now(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "smart_cluster_drain_now" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_smart_cluster_stop_drain(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "smart_cluster_stop_drain" }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_smart_cluster_calibrate_preview(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    query: String,
    n_results: Option<u32>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "smart_cluster_calibrate_preview",
            "query": query,
            "n_results": n_results.unwrap_or(30).min(200),
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_presidio_set_language(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    language: String,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({ "command": "presidio_set_language", "language": language }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_classify_debug(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    title: Option<String>,
    ocr_text: Option<String>,
    process_name: Option<String>,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "classify_debug",
            "title": title.unwrap_or_default(),
            "ocr_text": ocr_text.unwrap_or_default(),
            "process_name": process_name.unwrap_or_default(),
        }),
    )
    .await
}

#[tauri::command]
pub async fn monitor_remove_local_anchors_by_process(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, MonitorState>,
    category: String,
    process_name: String,
) -> Result<Value, String> {
    authenticated_monitor_command(
        &credential_state,
        &state,
        serde_json::json!({
            "command": "remove_local_anchors_by_process",
            "category": category,
            "process_name": process_name,
        }),
    )
    .await
}

// 内部函数：发送仅包含 command 的 IPC 命令 (兼容旧接口)
async fn send_ipc_command_internal(
    state: &State<'_, MonitorState>,
    cmd: &str,
) -> Result<String, String> {
    let req = serde_json::json!({ "command": cmd });
    forward_command_to_python(state, req)
        .await
        .map(|v| v.to_string())
}

/// Forward an arbitrary JSON command to the Python process via IPC.
/// Used by lib.rs callers that need to send commands but don't need
/// Rust-side capture/storage state updates.
pub async fn forward_command_to_python(
    state: &MonitorState,
    payload: Value,
) -> Result<Value, String> {
    let (pipe_name, auth_token) = {
        let pipe_guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
        let token_guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
        match (&*pipe_guard, &*token_guard) {
            (Some(name), Some(token)) => (name.clone(), token.clone()),
            _ => return Err("Monitor not started".to_string()),
        }
    };

    let seq_no = state.request_counter.fetch_add(1, Ordering::SeqCst);

    let command = payload
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let needs_full_cpu = command == "search_nl";

    let _cpu_guard = if needs_full_cpu {
        let guard = state.job_handle.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref job) = *guard {
            let _ = set_job_cpu_limit(**job, false, 0);
            Some(CpuLimitGuard {
                job_handle_raw: (**job).0 as isize,
            })
        } else {
            None
        }
    } else {
        None
    };

    send_ipc_request_reused(state, &pipe_name, &auth_token, seq_no, payload).await
}

fn create_job_object(cpu_limit_enabled: bool, cpu_limit_percent: u32) -> Result<JobHandle, String> {
    // SAFETY: information structures match their Job Object information classes and
    // remain alive for each call. The new handle is closed on all error paths or moved
    // into `JobHandle`, which enforces single-close ownership.
    unsafe {
        // Create the Job Object before applying resource and lifetime policies.
        let handle = CreateJobObjectW(None, None)
            .map_err(|e| format!("Failed to create job object: {:?}", e))?;

        // Apply the CPU cap only when enabled.
        if cpu_limit_enabled && cpu_limit_percent > 0 {
            let mut cpu_info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
            cpu_info.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            cpu_info.Anonymous.CpuRate = cpu_limit_percent * 100; // percent * 100 = per-ten-thousand

            if let Err(e) = SetInformationJobObject(
                handle,
                JobObjectCpuRateControlInformation,
                &cpu_info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            ) {
                let _ = CloseHandle(handle);
                return Err(format!("Failed to set CPU limit: {:?}", e));
            }
        }

        // Ensure children terminate when CarbonPaper closes the last Job handle.
        let mut limit_info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limit_info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if let Err(e) = SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limit_info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(handle); // 确保清理资源
            return Err(format!("Failed to set Kill-on-Close limit: {:?}", e));
        }

        Ok(JobHandle::new(handle))
    }
}

fn read_pyvenv_home(venv_dir: &Path) -> Option<PathBuf> {
    let cfg = std::fs::read_to_string(venv_dir.join("pyvenv.cfg")).ok()?;
    for line in cfg.lines() {
        let (key, value) = line.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("home") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

fn find_python_runtime_dll(venv_dir: &Path, python_executable: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(parent) = python_executable.parent() {
        candidates.push(parent.join("python312.dll"));
        candidates.push(parent.join("python3.dll"));
    }
    candidates.push(venv_dir.join("Scripts").join("python312.dll"));
    candidates.push(venv_dir.join("Scripts").join("python3.dll"));
    candidates.push(venv_dir.join("python312.dll"));
    candidates.push(venv_dir.join("python3.dll"));
    if let Some(home) = read_pyvenv_home(venv_dir) {
        candidates.push(home.join("python312.dll"));
        candidates.push(home.join("python3.dll"));
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "Python runtime DLL not found for launcher".to_string())
}

fn append_known_native_dll_dirs(dirs: &mut Vec<PathBuf>, site_packages: &Path) {
    let mut push = |parts: &[&str]| {
        let mut path = site_packages.to_path_buf();
        for part in parts {
            path.push(part);
        }
        dirs.push(path);
    };

    push(&["torch", "lib"]);
    push(&["onnxruntime", "capi"]);
    push(&["numpy.libs"]);
    push(&["scipy.libs"]);
    push(&["sklearn", ".libs"]);
    push(&["scikit_learn.libs"]);
    push(&["Pillow.libs"]);
    push(&["cv2"]);
    push(&["tokenizers"]);
    push(&["safetensors"]);
}

fn python_launcher_dll_dirs(venv_dir: &Path, python_dll: &Path) -> String {
    let site_packages = venv_dir.join("Lib").join("site-packages");
    let mut dirs = vec![
        venv_dir.to_path_buf(),
        venv_dir.join("Scripts"),
        venv_dir.join("DLLs"),
        site_packages.clone(),
    ];
    append_known_native_dll_dirs(&mut dirs, &site_packages);
    if let Some(home) = read_pyvenv_home(venv_dir) {
        dirs.push(home);
    }
    if let Some(parent) = python_dll.parent() {
        dirs.push(parent.to_path_buf());
    }

    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter_map(|path| {
            if !path.exists() {
                return None;
            }
            let resolved = path.canonicalize().unwrap_or(path);
            let key = resolved.to_string_lossy().to_lowercase();
            if !seen.insert(key) {
                return None;
            }
            Some(resolved.to_string_lossy().to_string())
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Starts the Python monitor subprocess for screenshot capture and OCR.
pub async fn start_monitor_impl(
    state: State<'_, MonitorState>,
    app: AppHandle,
) -> Result<String, String> {
    if state.migration_lock.load(Ordering::SeqCst) {
        return Err("Cannot start monitor: Migration is currently in progress".to_string());
    }

    // Check if required model files are complete
    if let Ok(model_status) = crate::model_management::check_model_files().await {
        if let Some(obj) = model_status.as_object() {
            let has_incomplete = obj.values().any(|m| {
                m.get("complete").and_then(|c| c.as_bool()) == Some(false)
                    && m.get("required").and_then(|r| r.as_bool()) != Some(false)
            });
            if has_incomplete {
                return Err(
                    "Model files are incomplete. Please download required models first."
                        .to_string(),
                );
            }
        }
    }
    let resolved_model_runtime = crate::model_management::resolve_required_model_runtime().ok();
    if let Some(resolved) = &resolved_model_runtime {
        if resolved.used_pytorch_fallback {
            tracing::warn!(
                "ONNX models are incomplete; falling back to existing PyTorch models for monitor startup"
            );
            let _ = app.emit(
                "app-toast",
                serde_json::json!({
                    "type": "info",
                    "title": "模型运行时已回退",
                    "message": "未检测到完整 ONNX 模型，已使用本机已有的 PyTorch 模型启动监控。联网后可在设置中下载 ONNX 模型以降低内存占用。",
                }),
            );
        }
    }

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<failed to get cwd: {}>", e));

    // Reset the stopping flag for a fresh start
    state.stopping.store(false, Ordering::SeqCst);
    set_monitor_recovery_starting(&state);

    let stdout_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // 寻找 python 脚本路径。
    // 优先使用打包好的 monitor.pyz（生产环境唯一允许的入口）；
    // 仅在 dev 构建（cfg!(debug_assertions)）下，找不到 .pyz 时才回退到散落的 main.py。
    // .pyz 找到后会在下面经过 SHA-256 完整性校验（release 模式必校验）。
    let (script_path, is_pyz) = {
        let mut pyz_candidates: Vec<PathBuf> = Vec::new();
        let mut py_candidates: Vec<PathBuf> = Vec::new();

        // .pyz 候选（按优先级）
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                pyz_candidates.push(exe_dir.join("monitor.pyz"));
                pyz_candidates.push(exe_dir.join("monitor").join("monitor.pyz"));
            }
        }
        if let Some(p) = find_existing_file_in_resources(&app, "monitor.pyz") {
            pyz_candidates.push(p);
        }

        // .py 候选（仅 dev 回退用）
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                py_candidates.push(exe_dir.join("monitor").join("main.py"));
            }
        }
        if let Some(p) = find_existing_file_in_resources(&app, "monitor/main.py") {
            py_candidates.push(p);
        }
        py_candidates.push(PathBuf::from("monitor/main.py")); // CWD 是项目根
        py_candidates.push(PathBuf::from("../monitor/main.py")); // CWD 是 src-tauri

        if let Some(p) = pyz_candidates.iter().find(|p| p.exists()) {
            (normalize_path_for_command(p), true)
        } else if cfg!(debug_assertions) {
            // Dev 模式：允许散落 main.py 回退（不做完整性校验）
            match py_candidates.iter().find(|p| p.exists()) {
                Some(p) => (normalize_path_for_command(p), false),
                None => {
                    let tried_pyz: Vec<String> = pyz_candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect();
                    let tried_py: Vec<String> = py_candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect();
                    return Err(format!(
                        "Could not find monitor.pyz nor monitor/main.py. CWD: {}. Tried pyz: {:?}; py: {:?}",
                        cwd, tried_pyz, tried_py
                    ));
                }
            }
        } else {
            // Release 模式：必须有 .pyz，不允许回退
            let tried: Vec<String> = pyz_candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            crate::script_integrity::log_security_event(
                &app,
                "pyz_missing",
                &format!("CWD={}, tried={:?}", cwd, tried),
            );
            let _ = app.emit(
                "security-alert",
                serde_json::json!({
                    "code": "MONITOR_PYZ_MISSING",
                    "message": "Required monitor.pyz is missing. Startup blocked."
                }),
            );
            return Err(format!(
                "Required monitor.pyz not found. CWD: {}. Tried: {:?}",
                cwd, tried
            ));
        }
    };

    let script_abs = std::path::Path::new(&script_path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<failed to canonicalize script: {}>", e));

    // 完整性校验闸门：release 构建 + 使用 .pyz 时，必须通过 SHA-256 校验才能启动。
    // 失败时：写安全日志 + 推送 security-alert 事件 + 返回 Err 拒绝启动。
    if is_pyz && !cfg!(debug_assertions) {
        let pyz_path_obj = std::path::Path::new(&script_path);
        if let Err(reason) = crate::script_integrity::verify_monitor_pyz(pyz_path_obj) {
            tracing::error!("monitor.pyz integrity check failed: {}", reason);
            crate::script_integrity::log_security_event(
                &app,
                "pyz_integrity_fail",
                &format!("path={} reason={}", script_abs, reason),
            );
            let _ = app.emit(
                "security-alert",
                serde_json::json!({
                    "code": "MONITOR_PYZ_TAMPERED",
                    "message": "Monitor script integrity check failed. Startup blocked.",
                    "detail": reason,
                }),
            );
            return Err(format!("Monitor integrity check failed: {}", reason));
        }
        tracing::info!("monitor.pyz integrity verified: {}", script_abs);
    }

    // 生成随机的管道名和认证 token
    let pipe_name = generate_random_pipe_name();
    let auth_token = generate_auth_token();

    let (python_executable_for_error, python_exists_for_error) = {
        let mut process_guard = state.process.lock().unwrap_or_else(|e| e.into_inner());

        // 如果已经有进程句柄，先检查是否还在运行
        if let Some(child) = process_guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    // 已退出
                    *process_guard = None;
                }
                Ok(None) => {
                    return Ok("Monitor is already running".into());
                }
                Err(_) => {
                    *process_guard = None;
                }
            }
        }

        // 使用 python.rs 中的 check_python_venv(app) 获取 venv python 路径
        let python_executable = match crate::python::check_python_venv(app.clone()) {
            Ok(_) => {
                // 获取 venv 目录路径
                let venv_dir = crate::python::get_venv_dir(&app);
                let python_path = if cfg!(target_os = "windows") {
                    let pythonw = venv_dir.join("Scripts").join("pythonw.exe");
                    if pythonw.exists() {
                        pythonw
                    } else {
                        venv_dir.join("Scripts").join("python.exe")
                    }
                } else {
                    venv_dir.join("bin").join("python")
                };
                normalize_path_for_command(&python_path)
            }
            Err(e) => {
                return Err(format!("No usable Python venv found: {}", e));
            }
        };

        let python_exists = std::path::Path::new(&python_executable).exists();
        let venv_dir = crate::python::get_venv_dir(&app);
        let python_path = PathBuf::from(&python_executable);
        let python_canonical = python_path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve python executable: {}", e))?;
        let venv_canonical = venv_dir
            .canonicalize()
            .map_err(|e| format!("Failed to resolve Python venv directory: {}", e))?;
        if !python_canonical.starts_with(&venv_canonical) {
            return Err(format!(
                "Python executable is outside the expected venv: {}",
                python_canonical.display()
            ));
        }
        let python_dll = find_python_runtime_dll(&venv_canonical, &python_canonical)?;
        let launcher_executable =
            std::env::current_exe().map_err(|e| format!("Failed to resolve launcher: {}", e))?;
        let launcher_dll_dirs = python_launcher_dll_dirs(&venv_canonical, &python_dll);
        let scripts_dir = venv_canonical.join("Scripts");
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let system32_dir = std::path::Path::new(&system_root).join("System32");
        let windows_dir = PathBuf::from(&system_root);
        let child_path = std::env::join_paths([&scripts_dir, &system32_dir, &windows_dir])
            .map_err(|e| format!("Failed to build monitor PATH: {}", e))?;

        // 生成反向 IPC 管道名并启动服务器
        let reverse_pipe_name = generate_reverse_pipe_name();
        let reverse_ipc_auth_token = generate_reverse_ipc_auth_token();

        // 启动反向 IPC 服务器（用于接收 Python 的存储请求）
        {
            let storage = app.state::<Arc<StorageState>>();
            let storage_arc = (*storage).clone();
            let capture_state = app.state::<Arc<crate::capture::CaptureState>>();
            let ocr_cache = capture_state.ocr_image_cache.clone();

            let mut reverse_server =
                ReverseIpcServer::new(&reverse_pipe_name, reverse_ipc_auth_token.clone());
            if let Err(e) = reverse_server.start(storage_arc, ocr_cache, app.clone()) {
                tracing::error!("Failed to start reverse IPC server: {}", e);
            }

            // 存储反向 IPC 服务器和管道名
            {
                let mut guard = state.reverse_ipc.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(reverse_server);
            }
            {
                let mut guard = state
                    .reverse_pipe_name
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *guard = Some(reverse_pipe_name.clone());
            }
        }

        tracing::info!(
            "Starting monitor: launcher={} python={} python_dll={} (exists={}) script={} script_abs={} cwd={} pipe={} storage_pipe={}",
            launcher_executable.display(), python_executable, python_dll.display(), python_exists, script_path, script_abs, cwd, pipe_name[0..30].to_string(), reverse_pipe_name[0..30].to_string()
        );

        // Start the Python process with stdout/stderr piped,
        // and pass the pipe name and auth token as command line arguments (instead of environment variables)
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;

        let job = {
            let cpu_limit_enabled =
                crate::registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
            let cpu_limit_percent =
                crate::registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
            create_job_object(cpu_limit_enabled, cpu_limit_percent)
                .map_err(|e| format!("Failed to create Job Object: {}", e))?
        };

        let mut cmd_proc = Command::new(&launcher_executable);
        // Pipe stdout/stderr so we can stream logs to the frontend.
        //
        // 安全相关参数：
        //   -I              isolated mode: 忽略 PYTHONPATH/HOME、用户 site-packages、sys.path 注入
        //   -B              不写 .pyc（防止 __pycache__ 旁路）
        //   -X utf8         强制 UTF-8 模式（替代 PYTHONIOENCODING，后者会被 -I 忽略）
        //   -u              unbuffered stdio（原有）
        //
        // env_remove：再次确认 Python 不会读到污染过的 PYTHON* 环境变量
        // （`-I` 蕴含 `-E` 已经隔离了，这里是 belt-and-suspenders）
        cmd_proc
            .arg("--python-launcher")
            .arg("-I")
            .arg("-B")
            .arg("-X")
            .arg("utf8")
            .arg("-u")
            .arg(&script_path)
            .arg("--pipe-name")
            .arg(&pipe_name)
            .arg("--auth-token")
            .arg(&auth_token)
            .arg("--storage-pipe")
            .arg(&reverse_pipe_name)
            .env_remove("PYTHONPATH")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONSTARTUP")
            .env_remove("PYTHONUSERBASE")
            .env_remove("PYTHONINSPECT")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("PATH", child_path)
            .env("CARBON_PARENT_PID", std::process::id().to_string())
            .env("CARBONPAPER_LAUNCHER_PYTHON_EXE", &python_executable)
            .env("CARBONPAPER_LAUNCHER_PYTHON_DLL", &python_dll)
            .env("CARBONPAPER_LAUNCHER_DLL_DIRS", &launcher_dll_dirs)
            .env("CARBONPAPER_REVERSE_IPC_TOKEN", &reverse_ipc_auth_token)
            .env("PROFILING_ENABLED", "1");

        match crate::ml_runtime::resolve_ocr_model_path(&app) {
            Ok(path) => {
                cmd_proc.env("CARBONPAPER_OCR_MODEL_DIR", path);
            }
            Err(error) => {
                tracing::warn!(
                    "Bundled PP-OCRv5 Mobile model is unavailable; monitor will start with OCR degraded until the model is repaired: {}",
                    error
                );
            }
        }
        cmd_proc.env(
            "CARBONPAPER_REQUIRE_OCR_MODEL",
            (!cfg!(debug_assertions)).to_string(),
        );

        // Pass the current storage.data_dir to the child process to ensure that Python uses the correct data directory at startup.
        if let Some(storage_state) = app.try_state::<Arc<StorageState>>() {
            let dd = storage_state
                .data_dir
                .lock()
                .unwrap()
                .to_string_lossy()
                .to_string();
            cmd_proc.env("CARBONPAPER_DATA_DIR", dd);
        }

        let use_onnx = resolved_model_runtime
            .as_ref()
            .map(|resolved| resolved.runtime.use_onnx())
            .unwrap_or_else(|| crate::registry_config::get_bool("use_onnx").unwrap_or(true));

        // Sync persisted feature toggles into the Python monitor at startup.
        cmd_proc
            .env(
                "CARBONPAPER_CLUSTERING_ENABLED",
                crate::registry_config::get_bool("clustering_enabled")
                    .unwrap_or(true)
                    .to_string(),
            )
            .env(
                "CARBONPAPER_CLASSIFICATION_ENABLED",
                crate::registry_config::get_bool("classification_enabled")
                    .unwrap_or(true)
                    .to_string(),
            )
            .env(
                "CARBONPAPER_CLUSTERING_ALLOW_FULL_LOW_MEMORY",
                crate::registry_config::get_bool("clustering_allow_full_low_memory")
                    .unwrap_or(false)
                    .to_string(),
            )
            .env("CARBONPAPER_USE_ONNX", use_onnx.to_string())
            .env(
                "CARBONPAPER_OCR_TIMEOUT_SECS",
                crate::registry_config::get_u32("ocr_timeout_secs")
                    .unwrap_or(120)
                    .clamp(30, 600)
                    .to_string(),
            );

        if let Some(resolved) = &resolved_model_runtime {
            let paths = &resolved.paths;
            cmd_proc
                .env("MODEL_PATH", paths.clip_path.to_string_lossy().to_string())
                .env(
                    "BGE_MODEL_PATH",
                    paths.bge_path.to_string_lossy().to_string(),
                )
                .env(
                    "MINILM_MODEL_PATH",
                    paths.minilm_path.to_string_lossy().to_string(),
                );
        }

        // Pass DirectML configuration
        if crate::registry_config::get_bool("use_dml").unwrap_or(false) {
            // 检查游戏模式是否抑制了 DML（临时或永久）
            let suppressed = state.game_mode_dml_suppressed.load(Ordering::SeqCst)
                || state
                    .game_mode_permanently_suppressed
                    .load(Ordering::SeqCst);
            if !suppressed {
                // 先枚举可用 GPU，如果完全没有可用显卡则跳过 DML
                let gpus = enumerate_gpus_internal().unwrap_or_default();
                if gpus.is_empty() {
                    tracing::warn!(
                        "No compatible GPU detected, skipping DirectML (falling back to CPU)"
                    );
                } else {
                    cmd_proc.env("CARBONPAPER_USE_DML", "1");
                    let mut device_id =
                        crate::registry_config::get_u32("dml_device_id").unwrap_or(0);
                    // 校验 device_id 是否仍然有效，无效则回退到第一张可用卡
                    if !gpus
                        .iter()
                        .any(|g| g.get("id").and_then(|v| v.as_u64()) == Some(device_id as u64))
                    {
                        let fallback_id =
                            gpus[0].get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        tracing::warn!(
                            "DML device_id {} no longer exists, falling back to {}",
                            device_id,
                            fallback_id
                        );
                        device_id = fallback_id;
                        let _ = crate::registry_config::set_u32("dml_device_id", device_id);
                    }
                    cmd_proc.env("CARBONPAPER_DML_DEVICE_ID", device_id.to_string());
                }
            } else {
                tracing::info!("Game mode: DML suppressed, starting Python without DML");
            }
        }

        cmd_proc
            .env(
                "TRACEMALLOC_SNAPSHOT_DIR",
                "D:\\carbon_tracemalloc_snapshots",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
            cmd_proc.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let mut child = cmd_proc
            .spawn()
            .map_err(|e| format!("Failed to start python: {}", e))?;

        // Add the child to the Job Object for lifetime and CPU-limit enforcement.
        // SAFETY: `child` owns a live process handle, `job` owns a live Job Object, and
        // both remain valid for the synchronous assignment call.
        unsafe {
            let process_handle = HANDLE(child.as_raw_handle() as _);
            AssignProcessToJobObject(*job, process_handle)
                .expect("Failed to assign process to job");
        }

        // 存储管道名和认证 token
        {
            let mut guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(pipe_name.clone());
        }
        {
            let mut guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(auth_token.clone());
        }
        // 存储 Job handle 到 state 中（这样在停止时可以正确关闭）
        {
            let mut guard = state.job_handle.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(job);
        }

        // 重置请求计数器
        state.request_counter.store(0, Ordering::SeqCst);

        // If process stdout/stderr were piped, spawn threads to read and forward lines to the frontend
        let app_clone = app.clone();
        let stdout_cache_clone = stdout_cache.clone();
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(out);
                for l in reader.lines().flatten() {
                    {
                        let mut cache =
                            stdout_cache_clone.lock().unwrap_or_else(|e| e.into_inner());
                        cache.push(l.clone());
                        if cache.len() > STARTUP_LOG_TAIL_LINES {
                            let overflow = cache.len() - STARTUP_LOG_TAIL_LINES;
                            cache.drain(0..overflow);
                        }
                    }
                    // Print to console as well as emit to frontend, so logs are visible in terminal
                    tracing::info!(target: "monitor.stdout", "{}", l);
                    let _ = app_clone.emit(
                        "monitor-log",
                        serde_json::json!({"source":"stdout","line": l}),
                    );
                }
            });
        }

        let app_clone = app.clone();
        let stderr_cache_clone = stderr_cache.clone();
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(err);
                for l in reader.lines().flatten() {
                    {
                        let mut cache =
                            stderr_cache_clone.lock().unwrap_or_else(|e| e.into_inner());
                        cache.push(l.clone());
                        if cache.len() > STARTUP_LOG_TAIL_LINES {
                            let overflow = cache.len() - STARTUP_LOG_TAIL_LINES;
                            cache.drain(0..overflow);
                        }
                    }
                    // Parse Python log level from format "[LEVEL] ..." and use matching tracing macro
                    if l.starts_with("[DEBUG]") {
                        tracing::debug!(target: "monitor.stderr", "{}", l);
                    } else if l.starts_with("[INFO]") {
                        tracing::info!(target: "monitor.stderr", "{}", l);
                    } else if l.starts_with("[ERROR]") || l.starts_with("[CRITICAL]") {
                        tracing::error!(target: "monitor.stderr", "{}", l);
                    } else if l.starts_with("[WARNING]") {
                        tracing::warn!(target: "monitor.stderr", "{}", l);
                    } else {
                        // Unrecognized format (e.g. raw Python tracebacks) — default to warn
                        tracing::warn!(target: "monitor.stderr", "{}", l);
                    }
                    let _ = app_clone.emit(
                        "monitor-log",
                        serde_json::json!({"source":"stderr","line": l}),
                    );
                }
            });
        }

        *process_guard = Some(child);

        (python_executable, python_exists)
    };

    // 监控子进程退出，及时通知前端
    {
        let app_clone = app.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let state = app_clone.state::<MonitorState>();
            let mut guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *guard = None;
                        drop(guard);
                        // Don't emit monitor-exited during intentional stop
                        if !state.stopping.load(Ordering::SeqCst) {
                            let code = status
                                .code()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            cleanup_monitor_runtime_after_unexpected_exit(&state);
                            let recovery = set_monitor_recovery_crashed(&state, code.clone(), None);
                            crate::refresh_tray_menu(&app_clone);
                            let _ = app_clone.emit("monitor-recovery", recovery.clone());
                            let _ = app_clone.emit(
                                "monitor-exited",
                                serde_json::json!({"code": code, "recovery": recovery}),
                            );
                        }
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        *guard = None;
                        drop(guard);
                        // Don't emit monitor-exited during intentional stop
                        if !state.stopping.load(Ordering::SeqCst) {
                            cleanup_monitor_runtime_after_unexpected_exit(&state);
                            let recovery = set_monitor_recovery_crashed(
                                &state,
                                "unknown".to_string(),
                                Some(e.to_string()),
                            );
                            crate::refresh_tray_menu(&app_clone);
                            let _ = app_clone.emit("monitor-recovery", recovery.clone());
                            let _ = app_clone.emit(
                                "monitor-exited",
                                serde_json::json!({
                                    "code": "unknown",
                                    "error": e.to_string(),
                                    "recovery": recovery,
                                }),
                            );
                        }
                        break;
                    }
                }
            } else {
                break;
            }
        });
    }

    // 启动后等待管道就绪，避免前端一直处于 waiting
    let mut last_error: Option<String> = None;
    let started_at = tokio::time::Instant::now();
    while started_at.elapsed().as_millis() < STARTUP_MAX_WAIT_MS as u128 {
        match connect_to_pipe(&pipe_name).await {
            Ok(_) => {
                // 管道可连接，说明服务已就绪 — 启动 Rust 截图循环
                set_monitor_recovery_running(&state);
                spawn_capture_loop(&app);
                crate::refresh_tray_menu(&app);
                return Ok("Monitor started".into());
            }
            Err(e) => {
                last_error = Some(e);
            }
        }

        // 检查进程是否已经退出
        {
            let mut guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *guard = None;
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let message = format!("Monitor exited during startup (code: {})", code);
                        set_monitor_recovery_failed(&state, message.clone());
                        return Err(message);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        *guard = None;
                        let message = format!("Monitor startup check failed: {}", e);
                        set_monitor_recovery_failed(&state, message.clone());
                        return Err(message);
                    }
                }
            } else {
                let message = "Monitor process handle missing during startup".to_string();
                set_monitor_recovery_failed(&state, message.clone());
                return Err(message);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    {
        let mut guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    let stderr_tail = {
        let cache = stderr_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.is_empty() {
            "<empty>".to_string()
        } else {
            cache.join("\n")
        }
    };

    let message = format!(
        "Monitor startup timed out: {} | cwd: {} | script: {} | script_abs: {} | python: {} (exists: {}) | pipe: {} | stderr_tail: {}",
        last_error.unwrap_or_else(|| "pipe unavailable".to_string()),
        cwd,
        script_path,
        script_abs,
        python_executable_for_error,
        python_exists_for_error,
        pipe_name,
        stderr_tail
    );
    set_monitor_recovery_failed(&state, message.clone());
    Err(message)
}

#[tauri::command]
pub async fn start_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    start_monitor_impl(state, app).await
}

#[tauri::command]
pub fn get_monitor_autostart() -> bool {
    crate::registry_config::get_bool("autoStartMonitor").unwrap_or(true)
}

#[tauri::command]
pub fn set_monitor_autostart(
    window: tauri::Window,
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    enabled: bool,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;
    crate::registry_config::set_bool("autoStartMonitor", enabled)
}

/// Spawn the Rust-side capture loop using CaptureState
fn spawn_capture_loop(app: &AppHandle) {
    let capture_state = app.state::<Arc<CaptureState>>();
    let storage = app.state::<Arc<StorageState>>();
    let _monitor_state = app.state::<MonitorState>();

    // Reset capture state for new session
    capture_state.stopped.store(false, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);
    capture_state.in_flight_ocr_count.store(0, Ordering::SeqCst);
    capture_state.clear_wgc_session("spawn_capture_loop_reset");
    capture_state
        .startup_pending_cleanup_cancelled
        .store(false, Ordering::SeqCst);

    // Load exclusion settings from disk
    {
        let data_dir = storage
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        capture_state.load_exclusion_settings(&data_dir);
    }

    // Load advanced config from registry
    {
        let ocr_timeout_secs = crate::registry_config::get_u32("ocr_timeout_secs")
            .unwrap_or(120)
            .clamp(30, 600);
        capture_state
            .ocr_timeout_secs
            .store(ocr_timeout_secs, Ordering::SeqCst);
        capture_state
            .ocr_cold_start_pending
            .store(true, Ordering::SeqCst);
    }

    let cs = capture_state.inner().clone();
    let st = storage.inner().clone();
    {
        let cleanup_storage = st.clone();
        let cleanup_capture_state = cs.clone();
        tauri::async_runtime::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                cleanup_storage.abort_startup_pending_screenshots(|| {
                    cleanup_capture_state
                        .startup_pending_cleanup_cancelled
                        .load(Ordering::SeqCst)
                })
            })
            .await;
            match result {
                Ok(Ok(aborted)) if aborted > 0 => {
                    tracing::info!(
                        "[DIAG:STARTUP] aborted {} stale pending screenshots",
                        aborted
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("[DIAG:STARTUP] pending cleanup failed: {}", e),
                Err(e) => tracing::warn!("[DIAG:STARTUP] pending cleanup task failed: {}", e),
            }
        });
    }
    // MonitorState is not Arc-wrapped in Tauri managed state, but we access it via AppHandle
    // We need to pass the AppHandle so the capture loop can access MonitorState
    let app_handle = app.clone();

    let handle = tauri::async_runtime::spawn(async move {
        let _ms = app_handle.state::<MonitorState>();
        // Use AssertUnwindSafe + catch_unwind to detect panics in the capture loop
        let result = std::panic::AssertUnwindSafe(crate::capture::run_capture_loop(
            cs,
            st,
            app_handle.clone(),
        ));
        match futures::FutureExt::catch_unwind(result).await {
            Ok(()) => {
                // Normal exit
            }
            Err(_panic_payload) => {
                // The global panic hook (installed via error_window::install_panic_hook)
                // already handles showing the error overlay, so we just log here.
                tracing::error!("Capture loop panicked (error overlay shown by global hook)");
            }
        }
    });

    let mut guard = capture_state
        .capture_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);

    tracing::info!("Rust capture loop spawned");
}

/// Stops the Python monitor subprocess and the Rust capture loop.
pub async fn stop_monitor_impl(
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    // 1. Stop the Rust capture loop
    capture_state.stopped.store(true, Ordering::SeqCst);
    capture_state.paused.store(false, Ordering::SeqCst);

    // Signal the watcher thread to suppress monitor-exited event
    state.stopping.store(true, Ordering::SeqCst);

    // Abort the capture task
    {
        let mut guard = capture_state
            .capture_task
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    // Explicitly release WGC/D3D capture resources even when capture task is force-aborted.
    capture_state.clear_wgc_session("stop_monitor");

    // Wait for in-flight OCR tasks to complete (with timeout)
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while capture_state.in_flight_ocr_count.load(Ordering::SeqCst) > 0 {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "Timed out waiting for {} in-flight OCR tasks",
                capture_state.in_flight_ocr_count.load(Ordering::SeqCst)
            );
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // 2. Best-effort stop signal to Python. Do not let a broken worker or pipe
    // handler hold the UI in LOADING during an intentional terminate/restart.
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        send_ipc_command_internal(&state, "stop"),
    )
    .await;

    // Terminate the monitor process without blocking indefinitely. Child
    // processes are also covered by the Job Object when its handle is dropped.
    let child_to_stop = {
        let mut process_guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
        process_guard.take()
    };
    if let Some(mut child) = child_to_stop {
        let _ = child.kill();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
                Ok(None) => {
                    tracing::warn!("Timed out waiting for monitor process to exit after kill");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Failed to wait for monitor process after kill: {}", e);
                    break;
                }
            }
        }
    }

    // 停止反向 IPC 服务器
    {
        let mut guard = state.reverse_ipc.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut server) = *guard {
            server.stop();
        }
        *guard = None;
    }

    // 清理状态
    {
        let mut guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state.python_ipc_client.lock().await;
        *guard = None;
    }
    {
        let mut guard = state.reverse_ipc.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state
            .reverse_pipe_name
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state
            .reverse_pipe_name
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    {
        let mut guard = state.job_handle.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    set_monitor_recovery_stopped(&state);
    crate::refresh_tray_menu(&app);
    let _ = app.emit("monitor-stopped", serde_json::json!({"intentional": true}));

    Ok("Monitor stopped".into())
}

#[tauri::command]
pub async fn stop_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    stop_monitor_impl(state, capture_state, app).await
}

/// Pauses screenshot capture without stopping the Python process.
pub async fn pause_monitor_impl(
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    // Pause Rust capture loop
    capture_state.paused.store(true, Ordering::SeqCst);
    // Also forward to Python so OCR worker pauses
    let result = send_ipc_command_internal(&state, "pause").await;
    crate::refresh_tray_menu(&app);
    result
}

/// Resumes screenshot capture after a pause.
#[tauri::command]
pub async fn pause_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    pause_monitor_impl(state, capture_state, app).await
}

pub async fn resume_monitor_impl(
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    // Resume Rust capture loop
    capture_state.paused.store(false, Ordering::SeqCst);
    // Also forward to Python so OCR worker resumes
    let result = send_ipc_command_internal(&state, "resume").await;
    crate::refresh_tray_menu(&app);
    result
}

#[tauri::command]
pub async fn resume_monitor(
    window: tauri::Window,
    state: State<'_, MonitorState>,
    capture_state: State<'_, Arc<CaptureState>>,
    app: AppHandle,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    resume_monitor_impl(state, capture_state, app).await
}

#[tauri::command]
pub async fn get_monitor_status(state: State<'_, MonitorState>) -> Result<String, String> {
    if state.stopping.load(Ordering::SeqCst) {
        return Ok(stopped_monitor_status(&state).to_string());
    }

    match forward_command_to_python(&state, serde_json::json!({ "command": "status" })).await {
        Ok(mut status) => {
            if let Some(obj) = status.as_object_mut() {
                obj.insert("recovery".to_string(), monitor_recovery_snapshot(&state));
            }
            Ok(status.to_string())
        }
        Err(e) => {
            // 如果进程未运行，则返回“stopped”而不是抛错，避免前端冷启动误报
            let mut running = false;
            {
                let mut guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(child) = guard.as_mut() {
                    match child.try_wait() {
                        Ok(Some(_)) => {
                            *guard = None;
                        }
                        Ok(None) => {
                            running = true;
                        }
                        Err(_) => {
                            *guard = None;
                        }
                    }
                }
            }

            if !running {
                return Ok(stopped_monitor_status(&state).to_string());
            }

            Err(e)
        }
    }
}

// GPU enumeration and game-mode resource suppression.

/// Enumerates hardware GPU adapters, excluding software renderers.
pub fn enumerate_gpus_internal() -> Result<Vec<serde_json::Value>, String> {
    // SAFETY: DXGI returns reference-counted COM interfaces whose lifetimes are managed
    // by windows-rs; adapter indices are enumerated until Windows reports exhaustion.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

        let mut gpus = Vec::new();
        let mut i: u32 = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            let desc = adapter
                .GetDesc1()
                .map_err(|e| format!("Failed to get adapter desc: {:?}", e))?;
            // Exclude software renderers from user-selectable acceleration devices.
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                let name = String::from_utf16_lossy(
                    &desc.Description[..desc
                        .Description
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.Description.len())],
                );
                gpus.push(serde_json::json!({
                    "id": i,
                    "name": name.trim().to_string(),
                }));
            }
            i += 1;
        }
        Ok(gpus)
    }
}

#[tauri::command]
pub fn enumerate_gpus() -> Result<Vec<serde_json::Value>, String> {
    enumerate_gpus_internal()
}

/// Queries system-wide dedicated-memory use for one GPU via Windows PDH.
fn query_gpu_memory_usage(device_id: u32) -> Result<f64, String> {
    // SAFETY: DXGI COM interfaces are managed by windows-rs; the PDH path is
    // NUL-terminated, output pointers reference correctly typed stack storage, and the
    // query handle is closed on every path after it is opened.
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(device_id)
            .map_err(|e| format!("Failed to enum adapter {}: {:?}", device_id, e))?;

        let desc = adapter
            .GetDesc1()
            .map_err(|e| format!("Failed to get adapter desc: {:?}", e))?;

        let total_vram = desc.DedicatedVideoMemory;
        if total_vram == 0 {
            return Ok(0.0);
        }

        // Build the PDH adapter instance name from the DXGI LUID.
        let luid = desc.AdapterLuid;
        let counter_path = format!(
            "\\GPU Adapter Memory(luid_0x{:08X}_0x{:08X}_phys_0)\\Dedicated Usage",
            luid.HighPart as u32, luid.LowPart
        );
        let counter_path_w: Vec<u16> = counter_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // Open and populate the PDH query.
        let mut query = 0isize;
        let status = PdhOpenQueryW(None, 0, &mut query);
        if status != 0 {
            return Err(format!("PdhOpenQuery failed: 0x{:08X}", status));
        }

        let mut counter = 0isize;
        let status = PdhAddEnglishCounterW(
            query,
            windows::core::PCWSTR(counter_path_w.as_ptr()),
            0,
            &mut counter,
        );
        if status != 0 {
            PdhCloseQuery(query);
            return Err(format!(
                "PdhAddEnglishCounter failed for '{}': 0x{:08X}",
                counter_path, status
            ));
        }

        // Collect once to establish the counter baseline before formatting the value.
        let status = PdhCollectQueryData(query);
        if status != 0 {
            PdhCloseQuery(query);
            return Err(format!("PdhCollectQueryData failed: 0x{:08X}", status));
        }

        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status = PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, None, &mut value);
        PdhCloseQuery(query);

        if status != 0 {
            return Err(format!(
                "PdhGetFormattedCounterValue failed: 0x{:08X}",
                status
            ));
        }

        let dedicated_usage = value.Anonymous.largeValue as u64;
        let ratio = dedicated_usage as f64 / total_vram as f64;
        Ok(ratio.clamp(0.0, 1.0))
    }
}

/// 启动游戏模式监控循环（GPU 负载 + 全屏非浏览器检测）
pub fn start_game_mode_monitor(app: AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    // 停止已有的监控任务
    {
        let mut guard = monitor_state
            .game_mode_task
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    tracing::info!("Game mode: starting monitor (GPU polling 10s, fullscreen polling 3s)");

    let app_clone = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        // 始终监控 GPU 0（主显卡/游戏显卡）
        const MONITOR_DEVICE_ID: u32 = 0;
        // 频繁切换计数：记录最近的触发时间戳
        let mut trigger_timestamps: Vec<std::time::Instant> = Vec::new();

        // Fullscreen polling runs every 3s, GPU polling runs every 10s.
        // We use a 3s tick and run GPU check every ~3rd tick.
        let mut gpu_tick_counter: u32 = 0;
        const GPU_CHECK_INTERVAL_TICKS: u32 = 3; // 3 * 3s ≈ 9s (close to original 10s)

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            // ── Fullscreen non-browser detection ──
            {
                let capture_state = app_clone.state::<Arc<CaptureState>>();
                let was_paused = capture_state
                    .game_mode_capture_paused
                    .load(Ordering::SeqCst);

                let should_pause = match crate::capture::check_foreground_fullscreen() {
                    Some((process_name, _window_class, true)) if !process_name.is_empty() => {
                        // Fullscreen detected with known process — pause only if it's NOT a
                        // browser (hardcoded list or a live extension NMH session, which
                        // covers Chromium forks the list doesn't know about)
                        !(crate::capture::is_browser_process(&process_name)
                            || crate::reverse_ipc::has_nmh_session_for_exe(&process_name))
                    }
                    Some((process_name, window_class, true)) if process_name.is_empty() => {
                        // Fullscreen but process name unavailable (likely elevated/protected).
                        // Pause unless it's a known system window (desktop, taskbar, etc.)
                        if crate::capture::is_system_window_class(&window_class) {
                            false
                        } else {
                            tracing::info!(
                                "Game mode: fullscreen window with inaccessible process (class: '{}'), treating as game",
                                window_class
                            );
                            true
                        }
                    }
                    _ => false,
                };

                if should_pause != was_paused {
                    capture_state
                        .game_mode_capture_paused
                        .store(should_pause, Ordering::SeqCst);
                    if should_pause {
                        tracing::info!(
                            "Game mode: non-browser fullscreen app detected, pausing capture"
                        );
                    } else {
                        tracing::info!("Game mode: fullscreen app exited, resuming capture");
                    }
                    let _ = app_clone.emit("game-mode-status", serde_json::json!({
                        "active": app_clone.state::<MonitorState>().game_mode_dml_suppressed.load(Ordering::SeqCst),
                        "permanent": app_clone.state::<MonitorState>().game_mode_permanently_suppressed.load(Ordering::SeqCst),
                        "fullscreen_paused": should_pause,
                    }));
                }
            }

            // ── GPU memory polling (every ~9s) ──
            gpu_tick_counter += 1;
            if gpu_tick_counter < GPU_CHECK_INTERVAL_TICKS {
                continue;
            }
            gpu_tick_counter = 0;

            // 检查 DML 是否仍然启用
            if !crate::registry_config::get_bool("use_dml").unwrap_or(false) {
                continue;
            }

            let state = app_clone.state::<MonitorState>();

            // 如果已经被永久关闭，不再轮询
            if state
                .game_mode_permanently_suppressed
                .load(Ordering::SeqCst)
            {
                continue;
            }

            let usage = match query_gpu_memory_usage(MONITOR_DEVICE_ID) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("Game mode: failed to query GPU 0 memory: {}", e);
                    continue;
                }
            };

            let currently_suppressed = state.game_mode_dml_suppressed.load(Ordering::SeqCst);
            tracing::debug!(
                "Game mode: GPU 0 memory usage {:.1}%, DML suppressed: {}",
                usage * 100.0,
                currently_suppressed
            );

            if !currently_suppressed && usage >= 0.50 {
                // 记录触发时间，检查频率限制
                let now = std::time::Instant::now();
                trigger_timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);
                trigger_timestamps.push(now);

                if trigger_timestamps.len() >= 3 {
                    // 1 分钟内触发 3 次以上，永久关闭 DML 直到程序重启
                    tracing::warn!(
                        "Game mode: triggered {} times in 60s, permanently disabling DML until app restart",
                        trigger_timestamps.len()
                    );
                    state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
                    state
                        .game_mode_permanently_suppressed
                        .store(true, Ordering::SeqCst);
                    let _ = app_clone.emit(
                        "game-mode-status",
                        serde_json::json!({
                            "active": true,
                            "usage": usage,
                            "permanent": true,
                        }),
                    );

                    // 重启 Python（不带 DML）
                    let _ = stop_monitor_impl(
                        app_clone.state::<MonitorState>(),
                        app_clone.state::<Arc<CaptureState>>(),
                        app_clone.clone(),
                    )
                    .await;
                    let _ =
                        start_monitor_impl(app_clone.state::<MonitorState>(), app_clone.clone())
                            .await;
                    continue;
                }

                tracing::info!(
                    "Game mode: GPU 0 memory usage {:.1}% >= 50%, suppressing DML",
                    usage * 100.0
                );
                state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
                let _ = app_clone.emit(
                    "game-mode-status",
                    serde_json::json!({"active": true, "usage": usage}),
                );

                // 重启 Python（不带 DML）
                let _ = stop_monitor_impl(
                    app_clone.state::<MonitorState>(),
                    app_clone.state::<Arc<CaptureState>>(),
                    app_clone.clone(),
                )
                .await;
                let _ =
                    start_monitor_impl(app_clone.state::<MonitorState>(), app_clone.clone()).await;
            } else if currently_suppressed && usage <= 0.40 {
                // 记录触发时间（恢复也计入）
                let now = std::time::Instant::now();
                trigger_timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);
                trigger_timestamps.push(now);

                if trigger_timestamps.len() >= 3 {
                    tracing::warn!(
                        "Game mode: triggered {} times in 60s, permanently disabling DML until app restart",
                        trigger_timestamps.len()
                    );
                    state
                        .game_mode_permanently_suppressed
                        .store(true, Ordering::SeqCst);
                    let _ = app_clone.emit(
                        "game-mode-status",
                        serde_json::json!({
                            "active": true,
                            "usage": usage,
                            "permanent": true,
                        }),
                    );
                    // DML 已经被抑制，不需要再重启
                    continue;
                }

                tracing::info!(
                    "Game mode: GPU 0 memory usage {:.1}% <= 40%, restoring DML",
                    usage * 100.0
                );
                state
                    .game_mode_dml_suppressed
                    .store(false, Ordering::SeqCst);
                let _ = app_clone.emit(
                    "game-mode-status",
                    serde_json::json!({"active": false, "usage": usage}),
                );

                // 重启 Python（恢复 DML）
                let _ = stop_monitor_impl(
                    app_clone.state::<MonitorState>(),
                    app_clone.state::<Arc<CaptureState>>(),
                    app_clone.clone(),
                )
                .await;
                let _ =
                    start_monitor_impl(app_clone.state::<MonitorState>(), app_clone.clone()).await;
            }
        }
    });

    let mut guard = monitor_state
        .game_mode_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);
}

/// 停止游戏模式监控
pub fn stop_game_mode_monitor(app: &AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    let mut guard = monitor_state
        .game_mode_task
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.take() {
        handle.abort();
    }

    // 重置所有游戏模式状态
    monitor_state
        .game_mode_permanently_suppressed
        .store(false, Ordering::SeqCst);
    let was_suppressed = monitor_state
        .game_mode_dml_suppressed
        .swap(false, Ordering::SeqCst);

    // 重置全屏暂停状态
    let capture_state = app.state::<Arc<CaptureState>>();
    let was_fullscreen_paused = capture_state
        .game_mode_capture_paused
        .swap(false, Ordering::SeqCst);

    if was_suppressed || was_fullscreen_paused {
        let _ = app.emit(
            "game-mode-status",
            serde_json::json!({
                "active": false,
                "usage": 0.0,
                "fullscreen_paused": false,
            }),
        );
    }
    tracing::info!("Game mode: monitor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_launcher_dll_dirs_uses_known_native_whitelist() {
        let temp = tempfile::tempdir().unwrap();
        let venv_dir = temp.path();
        let site_packages = venv_dir.join("Lib").join("site-packages");
        let scripts_dir = venv_dir.join("Scripts");
        let torch_lib = site_packages.join("torch").join("lib");
        let numpy_libs = site_packages.join("numpy.libs");
        let unlisted_native_dir = site_packages.join("unlisted_package").join("native");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::create_dir_all(&torch_lib).unwrap();
        std::fs::create_dir_all(&numpy_libs).unwrap();
        std::fs::create_dir_all(&unlisted_native_dir).unwrap();

        let python_dll = scripts_dir.join("python312.dll");
        std::fs::write(&python_dll, b"").unwrap();

        let entries = python_launcher_dll_dirs(venv_dir, &python_dll)
            .split(';')
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();

        assert!(entries.contains(&site_packages.canonicalize().unwrap()));
        assert!(entries.contains(&torch_lib.canonicalize().unwrap()));
        assert!(entries.contains(&numpy_libs.canonicalize().unwrap()));
        assert!(!entries.contains(&unlisted_native_dir.canonicalize().unwrap()));
    }

    #[test]
    fn test_inject_ipc_auth_for_object_payload() {
        let req = serde_json::json!({"command": "status"});
        let enriched = inject_ipc_auth(req, "token-abc", 99);

        assert_eq!(enriched["command"], "status");
        assert_eq!(enriched["_auth_token"], "token-abc");
        assert_eq!(enriched["_seq_no"], 99);
    }

    #[test]
    fn test_inject_ipc_auth_for_non_object_payload() {
        let req = serde_json::json!(["status"]);
        let enriched = inject_ipc_auth(req.clone(), "token-abc", 99);
        assert_eq!(enriched, req);
    }

    #[test]
    fn test_parse_ipc_response_success() {
        let bytes = br#"{"status":"success","data":{"ok":true}}"#;
        let parsed = parse_ipc_response(bytes).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["data"]["ok"], true);
    }

    #[test]
    fn test_parse_ipc_response_invalid_json() {
        let bytes = br#"{"status":"success""#;
        let err = parse_ipc_response(bytes).unwrap_err();
        assert!(err.contains("Invalid JSON response"));
    }

    #[test]
    fn test_monitor_recovery_crash_snapshot_uses_manual_restart_policy() {
        let state = MonitorState::new();
        set_monitor_recovery_running(&state);

        let recovery =
            set_monitor_recovery_crashed(&state, "9".to_string(), Some("pipe failed".to_string()));

        assert_eq!(recovery["state"], "crashed");
        assert_eq!(recovery["policy"], "manual_restart");
        assert_eq!(recovery["restart_available"], true);
        assert_eq!(recovery["last_exit_code"], "9");
        assert_eq!(recovery["last_error"], "pipe failed");
        assert_eq!(recovery["crash_count"], 1);
        assert!(recovery["last_crashed_at_ms"].as_u64().unwrap_or(0) > 0);
    }
}
