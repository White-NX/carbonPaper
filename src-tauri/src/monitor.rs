use crate::resource_utils::{find_existing_file_in_resources, normalize_path_for_command};
use crate::reverse_ipc::{generate_reverse_pipe_name, ReverseIpcServer};
use crate::storage::StorageState;
use rand::Rng;
use std::ops::Deref;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;

use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Performance::*;
use windows::core::Interface;

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
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

// 重试配置
const MAX_RETRIES: u32 = 10;
const RETRY_DELAY_MS: u64 = 100;
const STARTUP_MAX_WAIT_MS: u64 = 15_000;
const STARTUP_LOG_TAIL_LINES: usize = 50;

use serde_json::Value;

// Windows error code for ERROR_PIPE_BUSY
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

// 生成随机的管道名和认证 token
fn generate_random_pipe_name() -> String {
    let mut rng = rand::thread_rng();
    let random_suffix: String = (0..32)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect();
    format!("carbon_monitor_{}", random_suffix)
}

fn generate_auth_token() -> String {
    let mut rng = rand::thread_rng();
    (0..64)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

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

// 内部函数：发送 IPC 请求 (通用) - 添加认证和序列号
async fn send_ipc_request(
    pipe_name: &str,
    auth_token: &str,
    seq_no: u64,
    mut req: Value,
) -> Result<Value, String> {
    // 在请求中添加认证信息
    if let Some(obj) = req.as_object_mut() {
        obj.insert(
            "_auth_token".to_string(),
            Value::String(auth_token.to_string()),
        );
        obj.insert("_seq_no".to_string(), Value::Number(seq_no.into()));
    }

    let mut client = connect_to_pipe(pipe_name).await?;

    let req_bytes = req.to_string().into_bytes();
    if let Err(e) = client.write_all(&req_bytes).await {
        return Err(format!("Write error: {}", e));
    }

    let mut buf = Vec::new();
    match client.read_to_end(&mut buf).await {
        Ok(_) => {
            let resp_str = String::from_utf8_lossy(&buf);
            match serde_json::from_str::<Value>(&resp_str) {
                Ok(v) => Ok(v),
                Err(e) => Err(format!("Invalid JSON response: {}. Data: {}", e, resp_str)),
            }
        }
        Err(e) => Err(format!("Read error: {}", e)),
    }
}

/// 动态设置 Job Object 的 CPU 限制
fn set_job_cpu_limit(handle: HANDLE, enabled: bool, percent: u32) -> Result<(), String> {
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

/// RAII guard：在 drop 时自动恢复 Job Object 的 CPU 限制
struct CpuLimitGuard {
    job_handle_raw: isize, // 存储原始值，避免 HANDLE (*mut c_void) 导致 !Send
}

unsafe impl Send for CpuLimitGuard {}

impl Drop for CpuLimitGuard {
    fn drop(&mut self) {
        let handle = HANDLE(self.job_handle_raw as *mut std::ffi::c_void);
        let enabled = crate::registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
        let percent = crate::registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
        let _ = set_job_cpu_limit(handle, enabled, percent);
    }
}

#[tauri::command]
pub async fn execute_monitor_command(
    state: State<'_, MonitorState>,
    payload: Value,
) -> Result<Value, String> {
    let pipe_name = {
        let guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some(name) => name.clone(),
            None => return Err("Monitor not started".to_string()),
        }
    };

    let auth_token = {
        let guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some(token) => token.clone(),
            None => return Err("Monitor not authenticated".to_string()),
        }
    };

    let seq_no = state.request_counter.fetch_add(1, Ordering::SeqCst);

    let command = payload
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let needs_full_cpu = command == "search_nl";

    // 用 RAII guard 确保即使 future 被取消也能恢复 CPU 限制
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

    send_ipc_request(&pipe_name, &auth_token, seq_no, payload).await
}

// 内部函数：发送仅包含 command 的 IPC 命令 (兼容旧接口)
async fn send_ipc_command_internal(
    state: &State<'_, MonitorState>,
    cmd: &str,
) -> Result<String, String> {
    let req = serde_json::json!({ "command": cmd });

    let pipe_name = {
        let guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some(name) => name.clone(),
            None => return Err("Monitor not started".to_string()),
        }
    };

    let auth_token = {
        let guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some(token) => token.clone(),
            None => return Err("Monitor not authenticated".to_string()),
        }
    };

    let seq_no = state.request_counter.fetch_add(1, Ordering::SeqCst);

    match send_ipc_request(&pipe_name, &auth_token, seq_no, req).await {
        Ok(v) => Ok(v.to_string()),
        Err(e) => Err(e),
    }
}

fn create_job_object(cpu_limit_enabled: bool, cpu_limit_percent: u32) -> Result<JobHandle, String> {
    unsafe {
        // 创建 Job Object
        let handle = CreateJobObjectW(None, None)
            .map_err(|e| format!("Failed to create job object: {:?}", e))?;

        // 设置 CPU 限制（仅在启用时）
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
                let _ = CloseHandle(handle); // 确保清理资源
                return Err(format!("Failed to set CPU limit: {:?}", e));
            }
        }

        // 设置"父进程退出则子进程自杀"
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

#[tauri::command]
pub async fn start_monitor(
    state: State<'_, MonitorState>,
    app: AppHandle,
) -> Result<String, String> {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<failed to get cwd: {}>", e));
    let stdout_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_cache: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // 寻找 python 脚本路径，按优先级尝试多种方式
    let script_path = {
        let mut candidates: Vec<PathBuf> = Vec::new();

        // 1. 基于可执行文件所在目录查找（生产环境 + 开机自启动）
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                candidates.push(exe_dir.join("monitor").join("main.py"));
            }
        }

        // 2. 基于 Tauri 资源目录查找（生产环境）
        if let Some(res_path) = find_existing_file_in_resources(&app, "monitor/main.py") {
            candidates.push(res_path);
        }

        // 3. 基于 CWD 的相对路径（开发模式）
        candidates.push(PathBuf::from("monitor/main.py")); // CWD 是项目根目录
        candidates.push(PathBuf::from("../monitor/main.py")); // CWD 是 src-tauri

        match candidates.iter().find(|p| p.exists()) {
            Some(p) => normalize_path_for_command(p),
            None => {
                let tried: Vec<String> =
                    candidates.iter().map(|p| p.display().to_string()).collect();
                return Err(format!(
                    "Could not find monitor/main.py. CWD: {}. Tried: {:?}",
                    cwd, tried
                ));
            }
        }
    };

    let script_abs = std::path::Path::new(&script_path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("<failed to canonicalize script: {}>", e));

    // 生成随机的管道名和认证 token（在外部作用域声明）
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

        // 生成反向 IPC 管道名并启动服务器
        let reverse_pipe_name = generate_reverse_pipe_name();

        // 启动反向 IPC 服务器（用于接收 Python 的存储请求）
        {
            let storage = app.state::<Arc<StorageState>>();
            let storage_arc = (*storage).clone();

            let mut reverse_server = ReverseIpcServer::new(&reverse_pipe_name);
            if let Err(e) = reverse_server.start(storage_arc) {
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
            "Starting monitor: python={} (exists={}) script={} script_abs={} cwd={} pipe={} storage_pipe={}",
            python_executable, python_exists, script_path, script_abs, cwd, pipe_name, reverse_pipe_name
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

        let mut cmd_proc = Command::new(&python_executable);
        // Pipe stdout/stderr so we can stream logs to the frontend
        cmd_proc
            .arg("-u")
            .arg(&script_path)
            .arg("--pipe-name")
            .arg(&pipe_name)
            .arg("--auth-token")
            .arg(&auth_token)
            .arg("--storage-pipe")
            .arg(&reverse_pipe_name)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PROFILING_ENABLED", "1");

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

        // Pass DirectML configuration
        if crate::registry_config::get_bool("use_dml").unwrap_or(false) {
            // 检查游戏模式是否抑制了 DML（临时或永久）
            let suppressed = state.game_mode_dml_suppressed.load(Ordering::SeqCst)
                || state.game_mode_permanently_suppressed.load(Ordering::SeqCst);
            if !suppressed {
                // 先枚举可用 GPU，如果完全没有可用显卡则跳过 DML
                let gpus = enumerate_gpus_internal().unwrap_or_default();
                if gpus.is_empty() {
                    tracing::warn!("No compatible GPU detected, skipping DirectML (falling back to CPU)");
                } else {
                    cmd_proc.env("CARBONPAPER_USE_DML", "1");
                    let mut device_id = crate::registry_config::get_u32("dml_device_id").unwrap_or(0);
                    // 校验 device_id 是否仍然有效，无效则回退到第一张可用卡
                    if !gpus.iter().any(|g| g.get("id").and_then(|v| v.as_u64()) == Some(device_id as u64)) {
                        let fallback_id = gpus[0].get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        tracing::warn!("DML device_id {} no longer exists, falling back to {}", device_id, fallback_id);
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

        // 将子进程加入 Job 对象，以便随主进程退出时自动清理以及限制CPU使用
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
            *guard = Some(auth_token);
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
                for line in reader.lines() {
                    if let Ok(l) = line {
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
                }
            });
        }

        let app_clone = app.clone();
        let stderr_cache_clone = stderr_cache.clone();
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(err);
                for line in reader.lines() {
                    if let Ok(l) = line {
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
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let _ = app_clone.emit("monitor-exited", serde_json::json!({"code": code}));
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        *guard = None;
                        drop(guard);
                        let _ = app_clone.emit(
                            "monitor-exited",
                            serde_json::json!({"code": "unknown", "error": e.to_string()}),
                        );
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
                // 管道可连接，说明服务已就绪
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
                        return Err(format!("Monitor exited during startup (code: {})", code));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        *guard = None;
                        return Err(format!("Monitor startup check failed: {}", e));
                    }
                }
            } else {
                return Err("Monitor process handle missing during startup".into());
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

    Err(format!(
        "Monitor startup timed out: {} | cwd: {} | script: {} | script_abs: {} | python: {} (exists: {}) | pipe: {} | stderr_tail: {}",
        last_error.unwrap_or_else(|| "pipe unavailable".to_string()),
        cwd,
        script_path,
        script_abs,
        python_executable_for_error,
        python_exists_for_error,
        pipe_name,
        stderr_tail
    ))
}

#[tauri::command]
pub async fn stop_monitor(state: State<'_, MonitorState>) -> Result<String, String> {
    // 尝试通过 IPC 发送结束信号
    let _ = send_ipc_command_internal(&state, "stop").await;

    // 等待进程退出
    let mut process_guard = state.process.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = process_guard.take() {
        // 给一点时间让它自己退出
        // 这里是同步阻塞，稍微不太好，但在 destroy 阶段通常可以接受
        // 或者直接 kill
        let _ = child.kill();
        let _ = child.wait();
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

    Ok("Monitor stopped".into())
}

#[tauri::command]
pub async fn pause_monitor(state: State<'_, MonitorState>) -> Result<String, String> {
    send_ipc_command_internal(&state, "pause").await
}

#[tauri::command]
pub async fn resume_monitor(state: State<'_, MonitorState>) -> Result<String, String> {
    send_ipc_command_internal(&state, "resume").await
}

#[tauri::command]
pub async fn get_monitor_status(state: State<'_, MonitorState>) -> Result<String, String> {
    match send_ipc_command_internal(&state, "status").await {
        Ok(status) => Ok(status),
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
                let stopped = serde_json::json!({
                    "paused": false,
                    "stopped": true,
                    "interval": 0
                });
                return Ok(stopped.to_string());
            }

            Err(e)
        }
    }
}

// ==================== GPU 枚举与游戏模式 ====================

/// 枚举系统中的 GPU 设备（排除软件渲染器）
pub fn enumerate_gpus_internal() -> Result<Vec<serde_json::Value>, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

        let mut gpus = Vec::new();
        let mut i: u32 = 0;
        loop {
            match factory.EnumAdapters1(i) {
                Ok(adapter) => {
                    let desc = adapter
                        .GetDesc1()
                        .map_err(|e| format!("Failed to get adapter desc: {:?}", e))?;
                    // 排除软件渲染器
                    if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                        let name = String::from_utf16_lossy(
                            &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
                        );
                        gpus.push(serde_json::json!({
                            "id": i,
                            "name": name.trim().to_string(),
                        }));
                    }
                    i += 1;
                }
                Err(_) => break,
            }
        }
        Ok(gpus)
    }
}

#[tauri::command]
pub fn enumerate_gpus() -> Result<Vec<serde_json::Value>, String> {
    enumerate_gpus_internal()
}

/// 查询指定 GPU 的系统级显存占用率（使用 Windows Performance Counter，与任务管理器一致）
fn query_gpu_memory_usage(device_id: u32) -> Result<f64, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| format!("Failed to create DXGI factory: {:?}", e))?;

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

        // 使用 LUID 构造 Performance Counter 路径
        let luid = desc.AdapterLuid;
        let counter_path = format!(
            "\\GPU Adapter Memory(luid_0x{:08X}_0x{:08X}_phys_0)\\Dedicated Usage",
            luid.HighPart as u32, luid.LowPart
        );
        let counter_path_w: Vec<u16> = counter_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // PDH 查询
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

        // 需要收集两次数据，第一次建立基线
        let status = PdhCollectQueryData(query);
        if status != 0 {
            PdhCloseQuery(query);
            return Err(format!("PdhCollectQueryData failed: 0x{:08X}", status));
        }

        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status = PdhGetFormattedCounterValue(
            counter,
            PDH_FMT_LARGE,
            None,
            &mut value,
        );
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

/// 启动游戏模式 GPU 监控循环
pub fn start_game_mode_monitor(app: AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    // 停止已有的监控任务
    {
        let mut guard = monitor_state.game_mode_task.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            handle.abort();
        }
    }

    tracing::info!("Game mode: starting GPU memory monitor (polling every 10s, checking GPU 0)");

    let app_clone = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        // 始终监控 GPU 0（主显卡/游戏显卡）
        const MONITOR_DEVICE_ID: u32 = 0;
        // 频繁切换计数：记录最近的触发时间戳
        let mut trigger_timestamps: Vec<std::time::Instant> = Vec::new();

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            // 检查 DML 是否仍然启用
            if !crate::registry_config::get_bool("use_dml").unwrap_or(false) {
                continue;
            }

            let state = app_clone.state::<MonitorState>();

            // 如果已经被永久关闭，不再轮询
            if state.game_mode_permanently_suppressed.load(Ordering::SeqCst) {
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
            tracing::debug!("Game mode: GPU 0 memory usage {:.1}%, DML suppressed: {}", usage * 100.0, currently_suppressed);

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
                    state.game_mode_permanently_suppressed.store(true, Ordering::SeqCst);
                    let _ = app_clone.emit("game-mode-status", serde_json::json!({
                        "active": true,
                        "usage": usage,
                        "permanent": true,
                    }));

                    // 重启 Python（不带 DML）
                    let _ = stop_monitor(app_clone.state::<MonitorState>()).await;
                    let _ = start_monitor(app_clone.state::<MonitorState>(), app_clone.clone()).await;
                    continue;
                }

                tracing::info!("Game mode: GPU 0 memory usage {:.1}% >= 50%, suppressing DML", usage * 100.0);
                state.game_mode_dml_suppressed.store(true, Ordering::SeqCst);
                let _ = app_clone.emit("game-mode-status", serde_json::json!({"active": true, "usage": usage}));

                // 重启 Python（不带 DML）
                let _ = stop_monitor(app_clone.state::<MonitorState>()).await;
                let _ = start_monitor(app_clone.state::<MonitorState>(), app_clone.clone()).await;
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
                    state.game_mode_permanently_suppressed.store(true, Ordering::SeqCst);
                    let _ = app_clone.emit("game-mode-status", serde_json::json!({
                        "active": true,
                        "usage": usage,
                        "permanent": true,
                    }));
                    // DML 已经被抑制，不需要再重启
                    continue;
                }

                tracing::info!("Game mode: GPU 0 memory usage {:.1}% <= 40%, restoring DML", usage * 100.0);
                state.game_mode_dml_suppressed.store(false, Ordering::SeqCst);
                let _ = app_clone.emit("game-mode-status", serde_json::json!({"active": false, "usage": usage}));

                // 重启 Python（恢复 DML）
                let _ = stop_monitor(app_clone.state::<MonitorState>()).await;
                let _ = start_monitor(app_clone.state::<MonitorState>(), app_clone.clone()).await;
            }
        }
    });

    let mut guard = monitor_state.game_mode_task.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(handle);
}

/// 停止游戏模式监控
pub fn stop_game_mode_monitor(app: &AppHandle) {
    let monitor_state = app.state::<MonitorState>();

    let mut guard = monitor_state.game_mode_task.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.take() {
        handle.abort();
    }

    // 重置所有游戏模式状态
    monitor_state.game_mode_permanently_suppressed.store(false, Ordering::SeqCst);
    let was_suppressed = monitor_state.game_mode_dml_suppressed.swap(false, Ordering::SeqCst);
    if was_suppressed {
        let _ = app.emit("game-mode-status", serde_json::json!({"active": false, "usage": 0.0}));
    }
    tracing::info!("Game mode: monitor stopped");
}
