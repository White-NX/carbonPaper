use crate::resource_utils::normalize_path_for_command;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;

pub struct MonitorState {
    pub process: Mutex<Option<Child>>,
}

// 默认管道名，需与 monitor/monitor/ipc_pipe.py 保持一致
const PIPE_NAME: &str = r"\\.\pipe\carbon_monitor_secure";

// 重试配置
const MAX_RETRIES: u32 = 10;
const RETRY_DELAY_MS: u64 = 100;
const STARTUP_MAX_WAIT_MS: u64 = 15_000;
const STARTUP_LOG_TAIL_LINES: usize = 50;

use serde_json::Value;

// Windows error code for ERROR_PIPE_BUSY
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

// 内部函数：尝试连接到管道，支持重试
async fn connect_to_pipe() -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    let mut last_error = String::new();

    for attempt in 0..MAX_RETRIES {
        match ClientOptions::new().open(PIPE_NAME) {
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

// 内部函数：发送 IPC 请求 (通用)
async fn send_ipc_request(req: Value) -> Result<Value, String> {
    let mut client = connect_to_pipe().await?;

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
pub async fn execute_monitor_command(payload: Value) -> Result<Value, String> {
    send_ipc_request(payload).await
}

// 内部函数：发送仅包含 command 的 IPC 命令 (兼容旧接口)
async fn send_ipc_command_internal(cmd: &str) -> Result<String, String> {
    let req = serde_json::json!({ "command": cmd });
    match send_ipc_request(req).await {
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

    let (python_executable_for_error, python_exists_for_error) = {
        let mut process_guard = state.process.lock().unwrap();

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
        let pipe_name = PIPE_NAME.to_string();

        println!(
            "Starting monitor using python: {} (exists: {}) | script: {} | script_abs: {} | cwd: {} | pipe: {}",
            python_executable, python_exists, script_path, script_abs, cwd, pipe_name
        );

        // 启动 Python 进程
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;

        let mut cmd_proc = Command::new(&python_executable);
        // Pipe stdout/stderr so we can stream logs to the frontend
        cmd_proc
            .arg("-u")
            .arg(&script_path)
            .env("CARBON_MONITOR_PIPE", "carbon_monitor_secure") // @todo 此处的carbon_monitor_seruce应当是随机生成的。
            .env("PYTHONIOENCODING", "utf-8")
            .env("PROFILING_ENABLED", "1")
            .env("OMP_NUM_THREADS", "1")
            .env("MKL_NUM_THREADS", "1")
            .env("OPENBLAS_NUM_THREADS", "1")
            .env("NUMPY_NUM_THREADS", "1") // 通过限制线程数，降低子服务对系统的影响
            .env(
                "TRACEMALLOC_SNAPSHOT_DIR",
                "D:\\carbon_tracemalloc_snapshots",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
            cmd_proc.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        let mut child = cmd_proc
            .spawn()
            .map_err(|e| format!("Failed to start python: {}", e))?;

        // If process stdout/stderr were piped, spawn threads to read and forward lines to the frontend
        let app_clone = app.clone();
        let stdout_cache_clone = stdout_cache.clone();
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(out);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        {
                            let mut cache = stdout_cache_clone.lock().unwrap();
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
                            let mut cache = stderr_cache_clone.lock().unwrap();
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
            let mut guard = state.process.lock().unwrap();
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *guard = None;
                        drop(guard);
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let _ = app_clone.emit(
                            "monitor-exited",
                            serde_json::json!({"code": code}),
                        );
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
        match connect_to_pipe().await {
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
            let mut guard = state.process.lock().unwrap();
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *guard = None;
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        return Err(format!(
                            "Monitor exited during startup (code: {})",
                            code
                        ));
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
        let mut guard = state.process.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    let stderr_tail = {
        let cache = stderr_cache.lock().unwrap();
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
        PIPE_NAME,
        stderr_tail
    ))
}

#[tauri::command]
pub async fn stop_monitor(state: State<'_, MonitorState>) -> Result<String, String> {
    // 尝试通过 IPC 发送结束信号
    let _ = send_ipc_command_internal("stop").await;

    // 等待进程退出
    let mut process_guard = state.process.lock().unwrap();
    if let Some(mut child) = process_guard.take() {
        // 给一点时间让它自己退出
        // 这里是同步阻塞，稍微不太好，但在 destroy 阶段通常可以接受
        // 或者直接 kill
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok("Monitor stopped".into())
}

#[tauri::command]
pub async fn pause_monitor() -> Result<String, String> {
    send_ipc_command_internal("pause").await
}

#[tauri::command]
pub async fn resume_monitor() -> Result<String, String> {
    send_ipc_command_internal("resume").await
}

#[tauri::command]
pub async fn get_monitor_status(state: State<'_, MonitorState>) -> Result<String, String> {
    match send_ipc_command_internal("status").await {
        Ok(status) => Ok(status),
        Err(e) => {
            // 如果进程未运行，则返回“stopped”而不是抛错，避免前端冷启动误报
            let mut running = false;
            {
                let mut guard = state.process.lock().unwrap();
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
