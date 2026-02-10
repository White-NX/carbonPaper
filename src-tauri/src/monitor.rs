use crate::resource_utils::normalize_path_for_command;
use crate::reverse_ipc::{generate_reverse_pipe_name, ReverseIpcServer};
use crate::storage::StorageState;
use rand::Rng;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;

use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::*;

pub struct MonitorState {
    pub process: Mutex<Option<Child>>,
    pub pipe_name: Mutex<Option<String>>,
    pub auth_token: Mutex<Option<String>>,
    pub request_counter: AtomicU64,
    /// 反向 IPC 服务器（用于接收 Python 的存储请求）
    pub reverse_ipc: Mutex<Option<ReverseIpcServer>>,
    /// 反向 IPC 管道名
    pub reverse_pipe_name: Mutex<Option<String>>,
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
        }
    }
}

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

    for attempt in 0..MAX_RETRIES {
        match ClientOptions::new().open(&full_pipe_name) {
            Ok(client) => return Ok(client),
            Err(e) => {
                // Check if it's ERROR_PIPE_BUSY (231)
                let is_pipe_busy = e.raw_os_error() == Some(ERROR_PIPE_BUSY);

                if is_pipe_busy && attempt < MAX_RETRIES - 1 {
                    // Wait and retry
                    tokio::time::sleep(tokio::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                    continue;
                }

                last_error = format!("Failed to connect to pipe: {}. Is monitor running?", e);
            }
        }
    }

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
    // 寻找 python 脚本路径，支持多种可能的工作目录
    let possible_paths = [
        "monitor/main.py",    // 如果 CWD 是项目根目录
        "../monitor/main.py", // 如果 CWD 是 src-tauri (开发模式)
    ];

    let script_path = possible_paths
        .iter()
        .find(|p| Path::new(p).exists())
        .map(|s| s.to_string());

    let script_path = match script_path {
        Some(p) => p,
        None => {
            return Err(format!(
                "Could not find monitor/main.py. CWD: {}. Tried: {:?}",
                cwd, possible_paths
            ));
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
                eprintln!("Failed to start reverse IPC server: {}", e);
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

        println!(
            "Starting monitor using python: {} (exists: {}) | script: {} | script_abs: {} | cwd: {} | pipe: {} | storage_pipe: {}",
            python_executable, python_exists, script_path, script_abs, cwd, pipe_name, reverse_pipe_name
        );

        // 启动 Python 进程
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;

        let job = unsafe {
            // FUCK ONNX
            // 通过限制python子服务CPU占用率，降低对系统的影响，同时保证RapidOCR的速度
            let handle = CreateJobObjectW(None, None).expect("Failed to create job");

            // 设置 CPU 限制
            let mut cpu_info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
            cpu_info.ControlFlags =
                JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
            cpu_info.Anonymous.CpuRate = 1500; // 15%

            SetInformationJobObject(
                handle,
                JobObjectCpuRateControlInformation,
                &cpu_info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
            )
            .expect("Failed to set CPU limit");

            // 设置“父进程退出则子进程自杀”
            let mut limit_info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limit_info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limit_info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .expect("Failed to set Kill-on-Close limit");

            handle
        };
        let mut cmd_proc = Command::new(&python_executable);
        // Pipe stdout/stderr so we can stream logs to the frontend
        // 通过命令行参数传递管道名和认证 token（不使用环境变量）
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
            .env("PROFILING_ENABLED", "1")
            //.env("OMP_NUM_THREADS", "1")
            //.env("MKL_NUM_THREADS", "1")
            //.env("OPENBLAS_NUM_THREADS", "1")
            //.env("NUMPY_NUM_THREADS", "1")
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
        // 存储管道名和认证 token
        {
            let mut guard = state.pipe_name.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(pipe_name.clone());
        }
        {
            let mut guard = state.auth_token.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(auth_token);
        }
        // 重置请求计数器
        state.request_counter.store(0, Ordering::SeqCst);

        // 将子进程加入 Job 对象，以便随主进程退出时自动清理以及限制CPU使用
        unsafe {
            let process_handle = HANDLE(child.as_raw_handle() as _);
            AssignProcessToJobObject(job, process_handle).expect("Failed to assign process to job");
        }

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
                        println!("monitor stdout: {}", l);
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
                        // Print errors to stderr and also emit to frontend
                        eprintln!("monitor stderr: {}", l);
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
