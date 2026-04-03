use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

use std::env;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::resource_utils::{
    file_in_local_appdata, file_in_resources,
    find_existing_file_in_resources,
    normalize_path_for_command,
    get_log_path,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri::Emitter;
/// Errors that can occur when locating the Python interpreter.
#[derive(Debug)]
pub enum FindPythonError {
    /// Failed to access the Windows Registry.
    RegistryAccessError(io::Error),

    /// The `Software\Python\PythonCore` registry key was not found.
    PythonCoreKeyNotFound,

    /// Python installations were found, but none matched the required version.
    NoMatchingVersionFound,
}

// 实现 Display trait，用于打印用户友好的错误信息
impl fmt::Display for FindPythonError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FindPythonError::RegistryAccessError(e) => {
                write!(
                    f,
                    "A system error occurred while accessing the Windows Registry: {}",
                    e
                )
            }
            FindPythonError::PythonCoreKeyNotFound => {
                write!(f, "Registry key 'Software\\Python\\PythonCore' was not found. Python may not be installed.")
            }
            FindPythonError::NoMatchingVersionFound => {
                write!(
                    f,
                    "Found Python installations, but none match version 3.12.10."
                )
            }
        }
    }
}

// 实现标准 Error trait，以便与其他错误处理库（如 anyhow）兼容
impl std::error::Error for FindPythonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FindPythonError::RegistryAccessError(e) => Some(e),
            _ => None,
        }
    }
}

// 允许从 `std::io::Error` 自动转换为我们的错误类型，方便使用 `?` 操作符
impl From<io::Error> for FindPythonError {
    fn from(err: io::Error) -> Self {
        FindPythonError::RegistryAccessError(err)
    }
}

/// 从Windows注册表中专门搜索 Python 3.12.10 的安装。
///
/// # Returns
/// - `Ok(String)`: 如果找到一个有效的 `python.exe` 路径。
/// - `Err(FindPythonError)`: 如果未找到，返回一个包含详细失败原因的错误。
const REQUIRED_PYTHON_VERSION: &str = "3.12.10";

fn probe_python_command(cmd: &str, args: &[&str]) -> Option<(String, String)> {
    let mut cmd_proc = std::process::Command::new(cmd);
    for arg in args {
        cmd_proc.arg(arg);
    }
    cmd_proc.arg("-c").arg("import sys; print(sys.version.split()[0]); print(sys.executable)");
    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }
    let output = cmd_proc.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
    let version = lines.next()?.to_string();
    let executable = lines.next()?.to_string();
    Some((version, executable))
}

fn probe_python_executable(python_exe_path: &Path) -> Option<String> {
    let mut cmd_proc = std::process::Command::new(normalize_path_for_command(python_exe_path));
    cmd_proc.arg("-c").arg("import sys; print(sys.version.split()[0])");
    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }
    let output = cmd_proc.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version == REQUIRED_PYTHON_VERSION {
        Some(version)
    } else {
        None
    }
}

fn find_python_3_12_from_registry() -> Result<String, FindPythonError> {
    tracing::info!("Searching for Python {} in Windows Registry...", REQUIRED_PYTHON_VERSION);

    let hives_to_check = [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ];
    let mut was_python_core_key_found = false;

    for hkey in &hives_to_check {
        // 尝试打开 PythonCore 键
        if let Ok(python_core_key) = hkey.open_subkey("Software\\Python\\PythonCore") {
            was_python_core_key_found = true; // 确认找到了至少一个Python安装根键

            for version_key_name in python_core_key.enum_keys().filter_map(Result::ok) {
                if version_key_name.starts_with("3.12") {
                    tracing::info!("Found a potential 3.12 key: '{}'", version_key_name);

                    if let Ok(version_key) = python_core_key.open_subkey(&version_key_name) {
                        if let Ok(install_path_key) = version_key.open_subkey("InstallPath") {
                            if let Ok(install_dir) = install_path_key.get_value::<String, _>("") {
                                let python_exe_path = PathBuf::from(install_dir).join("python.exe");
                                if python_exe_path.is_file() && probe_python_executable(&python_exe_path).is_some() {
                                    tracing::info!(
                                        "Verified python.exe matches {} at: {:?}",
                                        REQUIRED_PYTHON_VERSION,
                                        python_exe_path
                                    );
                                    // 找到了，立即成功返回
                                    return Ok(python_exe_path.display().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 如果循环结束还没有返回，说明查找失败。现在我们根据线索返回最准确的错误。
    if !was_python_core_key_found {
        Err(FindPythonError::PythonCoreKeyNotFound)
    } else {
        Err(FindPythonError::NoMatchingVersionFound)
    }
}

/// 判断系统是否能找到可用的 Python 解释器（要求 3.12.10）
/// # Returns
/// - `Ok(String)`: 返回找到的 Python 解释器路径
/// - `Err(String)`: 如果未找到，返回错误信息
#[tauri::command]
pub fn check_python_status() -> Result<String, String> {
    let possible_commands = ["python3", "python", "py"];

    // Iterate through possible commands to find a usable Python interpreter
    for cmd in possible_commands {
        let args = if cmd == "py" { vec!["-3.12"] } else { vec![] };
        if let Some((version, executable)) = probe_python_command(cmd, &args) {
            if version == REQUIRED_PYTHON_VERSION {
                let exe_path = PathBuf::from(&executable);
                if exe_path.is_file() {
                    return Ok(executable);
                }
            }
        }
    }

    // 尝试从注册表中查找 Python 3.12.x
    match find_python_3_12_from_registry() {
        Ok(path) => Ok(path),
        Err(_e) => Ok("".to_string()),
    }
}

#[tauri::command]
/// 检查是否存在预设的 Python 虚拟环境
/// # Returns
/// - `Ok(String)`: 返回 Python 版本信息
/// - `Err(String)`: 如果未找到，返回错误信息
pub fn check_python_venv(app: AppHandle) -> Result<String, String> {
    let venv_dir = get_venv_dir(&app);
    let python_path = venv_dir.join("Scripts").join("python.exe");
    if python_path.exists() {
        let mut cmd_proc = std::process::Command::new(normalize_path_for_command(&python_path));
        cmd_proc.arg("--version");
        #[cfg(windows)]
        {
            cmd_proc.creation_flags(0x08000000);
        }
        if let Ok(output) = cmd_proc.output() {
            if output.status.success() {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                return Ok(version);
            }
        }
    }
    Err("No usable Python venv found.".into())
}

/// Get the path to the Python virtual environment directory.
/// Prioritizes the appdata location, falling back to the resource directory.
pub fn get_venv_dir(_app: &AppHandle) -> PathBuf {
    // 始终返回 LocalAppData + ".venv" 路径，如果取不到也返回该路径
    let venv_path = file_in_local_appdata()
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()))
        .join(".venv");
    venv_path
}

const INSTALLER_NAME: &str = "python-3.12.10-amd64.exe";



// Run an elevated command while hiding the created window on Windows.
// Implementation uses PowerShell Start-Process -Verb RunAs -WindowStyle Hidden -Wait
// which still shows the UAC prompt but prevents the launched console window from appearing.
#[cfg(windows)]
fn run_elevated_hidden_cmd(file: &str, args: &[String]) -> io::Result<std::process::ExitStatus> {
    use std::process::Command;

    // Safely escape single quotes for PowerShell string literal
    let file_escaped = file.replace("'", "''");
    let arglist = args
        .iter()
        .map(|a| format!("'{}'", a.replace("'", "''")))
        .collect::<Vec<_>>()
        .join(",");

    let ps_cmd = format!(
        "$args = @({}); $p = Start-Process -FilePath '{}' -ArgumentList $args -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        arglist, file_escaped
    );

    let mut cmd_proc = Command::new("powershell");
    cmd_proc
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(ps_cmd);
    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }

    let output = cmd_proc.output()?;
    Ok(output.status)
}

#[cfg(not(windows))]
fn run_elevated_hidden_cmd(file: &str, args: &[String]) -> io::Result<std::process::ExitStatus> {
    // Fallback to runas::Command on non-Windows platforms
    let mut cmd = runas::Command::new(file);
    for a in args {
        cmd.arg(a);
    }
    match cmd.status() {
        Ok(s) => Ok(s),
        Err(e) => Err(io::Error::other(format!("runas failed: {}", e))),
    }
}

#[tauri::command]
/// 请求以管理员权限安装 Python 3.12.x
/// # Returns
/// - `Ok(String)`: 如果安装成功，返回成功信息
/// - `Err(String)`: 如果安装失败，返回错误信息
pub async fn request_install_python(app: AppHandle) -> Result<String, String> {
    let log_path = get_log_path();

    let _ = fs::remove_file(&log_path);

    // Try to locate installer in the app's resources (only if it exists)
    let installer_path_option = find_existing_file_in_resources(&app, INSTALLER_NAME);

    match installer_path_option {
        Some(installer_path) => {
            // 根据系统中是否已有 python 来决定是否添加 PrependPath
            let mut prepend_path = 1;
            for cmd in ["python3", "python"] {
                let mut cmd_proc = std::process::Command::new(cmd);
                cmd_proc.arg("--version");
                #[cfg(windows)]
                {
                    cmd_proc.creation_flags(0x08000000);
                }
                if let Ok(output) = cmd_proc.output() {
                    if output.status.success() {
                        prepend_path = 0; // Already exists a python in PATH
                    }
                }
            }

            let installer_args = vec![
                "/quiet".to_string(),
                "InstallAllUsers=1".to_string(),
                format!("PrependPath={}", prepend_path),
            ];
            let status = run_elevated_hidden_cmd(
                &normalize_path_for_command(&installer_path),
                &installer_args,
            )
            .map_err(|e| e.to_string())?;

            if status.success() {
                let _ = fs::remove_file(&log_path);
                Ok("Python installation completed successfully.".into())
            } else {
                let log_content = fs::read_to_string(&log_path)
                    .unwrap_or_else(|_| "Failed to read error log.".to_string());

                let _ = fs::remove_file(&log_path);

                let error_message = if log_content.trim().is_empty() {
                    "Installation was cancelled by the user or failed to start.".to_string()
                } else {
                    format!("Installation failed:\n---\n{}", log_content)
                };

                Err(error_message)
            }
        }
        None => Err(format!("Error: cannot find resource '{}'", INSTALLER_NAME)),
    }
}

#[tauri::command]
/// 安装 Python 虚拟环境并安装必要的依赖包
/// # Arguments
/// - `python_path`: 可选，指定用于创建 venv 的 Python 可执行文件路径（例如：C:\\Python310\\python.exe）
/// # Returns
/// - `Ok(String)`: 如果安装成功，返回成功信息
/// - `Err(String)`: 如果安装失败，返回错误信息
pub async fn install_python_venv(
    app: AppHandle,
    python_path: Option<String>,
) -> Result<String, String> {
    let log_path = get_log_path();

    let _ = fs::remove_file(&log_path);

    let log_file = Arc::new(Mutex::new(
        File::create(&log_path).map_err(|e| e.to_string())?,
    ));

    match perform_install_python_venv(log_file, &app, python_path.as_deref()) {
        Ok(_) => Ok("Python virtual environment and dependencies installed successfully.".into()),
        Err(e) => {
            let log_content = fs::read_to_string(&log_path)
                .unwrap_or_else(|_| "Failed to read error log.".to_string());

            let _ = fs::remove_file(&log_path);

            let error_message = format!("Installation failed: {}\n---\n{}", e, log_content);

            Err(error_message)
        }
    }
}

// Resource helper functions (path normalization and resource lookups) were moved to `src-tauri/src/resource_utils.rs`.
// They are imported at the top of this file from `crate::resource_utils`.
fn perform_install_python_venv(
    log_file: Arc<Mutex<File>>,
    app: &AppHandle,
    python_path: Option<&str>,
) -> io::Result<()> {
    tracing::info!("perform_install_python_venv started");
    let _ = app.emit(
        "install-log",
        json!({"source":"installer","line":"perform_install_python_venv started"}),
    );
    tracing::info!(
        "perform_install_python_venv: start; python_path={:?}",
        python_path
    );

    let venv_dir = get_venv_dir(app);

    // Track whether the venv was freshly created (not pre-existing/packaged)
    // so we can roll it back on pip failure for a clean retry.
    let mut freshly_created_venv = false;

    // 检查是否为打包好的虚拟环境
    if venv_dir.exists() && venv_dir.join("Scripts").join("python.exe").exists() {
        writeln!(
            log_file.lock().unwrap_or_else(|e| e.into_inner()),
            "Found packaged virtual env at: {:?}",
            venv_dir
        )?;
        let _ = app.emit("install-log", json!({"source":"installer","line": format!("Found packaged virtual env at: {:?}", venv_dir)}));
        tracing::info!(
            "perform_install_python_venv: using packaged venv at {:?}",
            venv_dir
        );
    } else {
        writeln!(
            log_file.lock().unwrap_or_else(|e| e.into_inner()),
            "Creating virtual environment at: {:?} by using {:?}",
            venv_dir,
            python_path
        )?;
        let _ = app.emit("install-log", json!({"source":"installer","line": format!("Creating virtual environment at: {:?} by using {:?}", venv_dir, python_path)}));
        tracing::info!(
            "perform_install_python_venv: creating venv at {:?} with python {:?}",
            venv_dir, python_path
        );

        // 如果本地 .venv 已存在，先删除
        if venv_dir.exists() {
            writeln!(
                log_file.lock().unwrap_or_else(|e| e.into_inner()),
                "Existing venv found at {:?}, removing...",
                venv_dir
            )?;
            let _ = app.emit("install-log", json!({"source":"installer","line": format!("Existing venv found at {:?}, removing...", venv_dir)}));
            fs::remove_dir_all(&venv_dir).map_err(|e| {
                writeln!(log_file.lock().unwrap_or_else(|e| e.into_inner()), "Failed to remove existing venv: {}", e).ok();
                let _ = app.emit("install-log", json!({"source":"installer","line": format!("Failed to remove existing venv: {}", e)}));
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to remove existing virtual environment: {}", e),
                )
            })?;
            writeln!(
                log_file.lock().unwrap_or_else(|e| e.into_inner()),
                "Removed existing venv at {:?}",
                venv_dir
            )?;
            let _ = app.emit("install-log", json!({"source":"installer","line": format!("Removed existing venv at {:?}", venv_dir)}));
            tracing::info!(
                "perform_install_python_venv: removed existing venv at {:?}",
                venv_dir
            );
        }

        // 使用指定 python 可执行文件或 PATH 中的 python
        let python_cmd = python_path.unwrap_or("python");
        writeln!(
            log_file.lock().unwrap_or_else(|e| e.into_inner()),
            "Using python executable: {}",
            python_cmd
        )?;
        let _ = app.emit("install-log", json!({"source":"installer","line": format!("Using python executable: {}", python_cmd)}));
        tracing::info!("perform_install_python_venv: python_cmd = {}", python_cmd);

        let venv_args = vec![
            "-m".to_string(),
            "venv".to_string(),
            normalize_path_for_command(&venv_dir),
        ];

        let mut venv_cmd = Command::new(python_cmd);
        venv_cmd.args(&venv_args);
        #[cfg(windows)]
        {
            venv_cmd.creation_flags(0x08000000);
        }
        let status = venv_cmd.status().map_err(|e| {
            writeln!(
                log_file.lock().unwrap_or_else(|e| e.into_inner()),
                "Failed to spawn venv process: {}",
                e
            )
            .ok();
            io::Error::other(
                format!("Failed to run '{} -m venv': {}", python_cmd, e),
            )
        })?;

        writeln!(
            log_file.lock().unwrap_or_else(|e| e.into_inner()),
            "Venv command exit status: {:?}",
            status
        )?;
        let _ = app.emit("install-log", json!({"source":"installer","line": format!("Venv command exit status: {:?}", status)}));

        if !status.success() {
            let exit_code = status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            writeln!(
                log_file.lock().unwrap_or_else(|e| e.into_inner()),
                "ERROR: creating virtualenv with '{}' failed (exit code {}).",
                python_cmd,
                exit_code
            )?;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!(
                    "Failed to create virtual environment using '{}'. See log for details.",
                    python_cmd
                ),
            ));
        }

        writeln!(
            log_file.lock().unwrap_or_else(|e| e.into_inner()),
            "Virtual environment created at: {:?} using {}.",
            venv_dir,
            python_cmd
        )?;
        let _ = app.emit("install-log", json!({"source":"installer","line": format!("Virtual environment created at: {:?} using {}", venv_dir, python_cmd)}));
        tracing::info!(
            "perform_install_python_venv: virtualenv created at {:?}, python={}",
            venv_dir, python_cmd
        );
        freshly_created_venv = true;
        if let Ok(mut f) = log_file.lock() {
            let _ = f.flush();
        }
    }

    {
        let mut f = log_file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(&mut *f, "Using virtual env at: {:?}", venv_dir)?;
        writeln!(&mut *f, "Installing required packages...")?;
    }
    tracing::info!("perform_install_python_venv: using venv at {:?}", venv_dir);

    let python_exec = venv_dir.join("Scripts").join("python.exe");
    let pip_exec = venv_dir.join("Scripts").join("pip.exe");
    if !python_exec.is_file() || !pip_exec.is_file() {
        let mut f = log_file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(
            &mut *f,
            "ERROR: venv is missing executables. python.exe exists: {}, pip.exe exists: {}",
            python_exec.is_file(),
            pip_exec.is_file()
        )?;
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Virtual environment appears incomplete; python.exe or pip.exe missing.",
        ));
    }
    let python_exec_cmd = normalize_path_for_command(&python_exec);

    // Capture full pip output so we can write detailed diagnostics to the install log
    let requirements_path = file_in_resources(app, "monitor/requirements.txt")
        .unwrap_or_else(|| PathBuf::from("monitor/requirements.txt"));
    {
        let mut f = log_file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(
            &mut *f,
            "Installing from requirements file at: {:?}",
            &requirements_path
        )?;
        writeln!(&mut *f, "Installing required packages (non-elevated)...")?;
    }
    tracing::info!(
        "perform_install_python_venv: running pip via {} -m pip",
        python_exec_cmd
    );

    let mut cmd_proc = Command::new(&python_exec_cmd);
    cmd_proc
        .arg("-u")
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("-r")
        .arg(normalize_path_for_command(&requirements_path))
        .arg("--progress-bar")
        .arg("off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }

    // spawn 子进程（不要用 .output()）
    let mut child = cmd_proc.spawn().map_err(|e| {
        let mut f = log_file.lock().unwrap_or_else(|e| e.into_inner());
        writeln!(&mut *f, "Failed to spawn pip process: {}", e).ok();
        tracing::error!(
            "perform_install_python_venv: failed to spawn pip process: {}",
            e
        );
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to run pip install: {}", e),
        )
    })?;

    // 取出 stdout/stderr 的管道
    let stdout = child.stdout.take().expect("failed to capture stdout");
    let stderr = child.stderr.take().expect("failed to capture stderr");

    // 为了在线程中写日志，需要把 log_file 放到 Arc<Mutex<_>> 中
    let log_file = Arc::new(Mutex::new(log_file));

    // 克隆 app（按原来能用的 emit 方式）
    let app_for_threads = app.clone();

    // stdout 线程：逐行读并 emit、写日志
    let log_clone = Arc::clone(&log_file);
    let app_clone = app_for_threads.clone();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().flatten() {
            if let Ok(arc_file) = log_clone.lock() {
                if let Ok(mut f) = arc_file.as_ref().lock() {
                    let _ = writeln!(&mut *f, "{}", line);
                    let _ = f.flush();
                }
            }
            let _ = app_clone.emit("install-log", json!({"source":"pip","line": line}));
        }
    });

    // stderr 线程：同理
    let log_clone = Arc::clone(&log_file);
    let app_clone = app_for_threads.clone();
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    if let Ok(arc_file) = log_clone.lock() {
                        if let Ok(mut f) = arc_file.as_ref().lock() {
                            let _ = writeln!(&mut *f, "{}", line);
                            let _ = f.flush();
                        }
                    }
                    let _ = app_clone.emit("install-log", json!({"source":"pip","line": line}));
                }
                Err(e) => {
                    if let Ok(arc_file) = log_clone.lock() {
                        if let Ok(mut f) = arc_file.as_ref().lock() {
                            let _ = writeln!(&mut *f, "Error reading pip stderr: {}", e);
                            let _ = f.flush();
                        }
                    }
                }
            }
        }
    });

    // 等待子进程结束
    let status = child.wait().map_err(|e| {
        // 记录并返回错误
        if let Ok(arc_file) = log_file.lock() {
            if let Ok(mut f) = arc_file.as_ref().lock() {
                let _ = writeln!(&mut *f, "Failed waiting for pip process: {}", e);
            }
        }
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed waiting for pip: {}", e),
        )
    })?;

    // 等待读线程完成（它们在 EOF 时会自然退出）
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    // 记录 exit status，并按原逻辑返回错误或成功
    {
        if let Ok(arc_file) = log_file.lock() {
            if let Ok(mut f) = arc_file.as_ref().lock() {
                let _ = writeln!(&mut *f, "pip install exit status: {:?}", status);
                let _ = f.flush();
            }
        }
    }

    if !status.success() {
        let exit_code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into());
        if let Ok(arc_file) = log_file.lock() {
            if let Ok(mut f) = arc_file.as_ref().lock() {
                let _ = writeln!(
                    &mut *f,
                    "ERROR: pip install failed with exit code {}.",
                    exit_code
                );
            }
        }
        // Rollback: remove freshly created venv so retries start clean
        if freshly_created_venv && venv_dir.exists() {
            if let Ok(arc_file) = log_file.lock() {
                if let Ok(mut f) = arc_file.as_ref().lock() {
                    let _ = writeln!(&mut *f, "Rolling back: removing freshly created venv at {:?}", venv_dir);
                }
            }
            let _ = app_for_threads.emit("install-log", json!({"source":"installer","line": format!("Rolling back: removing freshly created venv at {:?}", venv_dir)}));
            if let Err(e) = fs::remove_dir_all(&venv_dir) {
                if let Ok(arc_file) = log_file.lock() {
                    if let Ok(mut f) = arc_file.as_ref().lock() {
                        let _ = writeln!(&mut *f, "Warning: failed to remove venv during rollback: {}", e);
                    }
                }
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "Failed to install required packages (exit code {}). See log for details.",
                exit_code
            ),
        ));
    }

    if let Ok(arc_file) = log_file.lock() {
        if let Ok(mut f) = arc_file.as_ref().lock() {
            let _ = writeln!(&mut *f, "Required packages installed successfully.");
            let _ = f.flush();
        }
    }

    // onnxruntime and onnxruntime-directml cannot coexist; otherwise, DmlExecutionProvider will be lost.
    // Other dependencies (such as chromadb) may have indirectly installed onnxruntime; 
    // they need to be uninstalled and then onnxruntime-directml reinstalled.
    {
        if let Ok(arc_file) = log_file.lock() {
            if let Ok(mut f) = arc_file.as_ref().lock() {
                let _ = writeln!(&mut *f, "Fixing onnxruntime/onnxruntime-directml conflict...");
                let _ = f.flush();
            }
        }
        let _ = app_for_threads.emit("install-log", json!({"source":"installer","line": "Fixing onnxruntime/onnxruntime-directml conflict..."}));

        // Step 1: uninstall onnxruntime (non-directml)
        let mut uninstall_cmd = Command::new(&python_exec_cmd);
        uninstall_cmd
            .arg("-m").arg("pip").arg("uninstall").arg("onnxruntime").arg("-y");
        #[cfg(windows)]
        {
            uninstall_cmd.creation_flags(0x08000000);
        }
        let _ = uninstall_cmd.output();

        // Step 2: force-reinstall onnxruntime-directml (no-deps to avoid pulling onnxruntime back)
        let mut reinstall_cmd = Command::new(&python_exec_cmd);
        reinstall_cmd
            .arg("-m").arg("pip").arg("install")
            .arg("onnxruntime-directml==1.24.2")
            .arg("--force-reinstall").arg("--no-deps")
            .arg("-i").arg("https://mirrors.aliyun.com/pypi/simple/");
        #[cfg(windows)]
        {
            reinstall_cmd.creation_flags(0x08000000);
        }
        match reinstall_cmd.output() {
            Ok(output) => {
                let msg = String::from_utf8_lossy(&output.stdout);
                if let Ok(arc_file) = log_file.lock() {
                    if let Ok(mut f) = arc_file.as_ref().lock() {
                        let _ = writeln!(&mut *f, "onnxruntime-directml reinstall: {}", msg);
                        let _ = f.flush();
                    }
                }
            }
            Err(e) => {
                if let Ok(arc_file) = log_file.lock() {
                    if let Ok(mut f) = arc_file.as_ref().lock() {
                        let _ = writeln!(&mut *f, "Warning: failed to reinstall onnxruntime-directml: {}", e);
                        let _ = f.flush();
                    }
                }
            }
        }
    }

    if let Ok(arc_file) = log_file.lock() {
        if let Ok(mut f) = arc_file.as_ref().lock() {
            let _ = writeln!(
                &mut *f,
                "Virtual environment and dependencies installed successfully at: {:?}",
                venv_dir
            );
            let _ = f.flush();
        }
    }
    let _ = app_for_threads.emit("install-log", json!({"source":"installer","line": format!("Virtual environment and dependencies installed successfully at: {:?}", venv_dir)}));

    // Write requirements hash so future dependency freshness checks can detect changes
    if let Ok(hash) = compute_requirements_hash(&requirements_path) {
        if let Err(e) = write_requirements_hash(&venv_dir, &hash) {
            tracing::warn!("Failed to write requirements hash: {}", e);
        }
    }

    Ok(())
}

/// 执行 Python 静默安装的核心逻辑
/// # Returns
/// - `Ok(())`: 如果安装成功
/// - `Err(io::Error)`: 如果安装过程中发生错误
fn perform_python_install(log_file: &mut File) -> io::Result<()> {
    let mut prepend_path = 1;
    for cmd in ["python3", "python"] {
        let mut cmd_proc = std::process::Command::new(cmd);
        cmd_proc.arg("--version");
        #[cfg(windows)]
        {
            cmd_proc.creation_flags(0x08000000);
        }
        if let Ok(output) = cmd_proc.output() {
            if output.status.success() {
                prepend_path = 0; // Already exists a python in PATH
            }
        }
    }

    const INSTALLER_NAME: &str = "python-3.12.10-amd64.exe";

    let current_dir = env::current_dir()?;
    let installer_path = current_dir.join(INSTALLER_NAME);

    writeln!(
        log_file,
        "Starting Python installer from: {:?}",
        installer_path
    )?;

    let mut cmd_proc = Command::new(normalize_path_for_command(&installer_path));
    cmd_proc
        .arg("/quiet")
        .arg("InstallAllUsers=1")
        .arg(format!("PrependPath={}", prepend_path))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }
    let child = cmd_proc.spawn()?;

    let output = child.wait_with_output()?;

    log_file.write_all(&output.stdout)?;
    log_file.write_all(&output.stderr)?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Python installer failed",
        ));
    }

    Ok(())
}

/// 以静默方式安装 Python 3.12.x
/// 如果安装失败，会将错误写入日志文件并以非零状态码退出
pub fn run_silent_install() {
    let log_path = get_log_path();
    let mut log_file = File::create(&log_path).expect("Failed to create log file");

    if let Err(e) = perform_python_install(&mut log_file) {
        // 如果发生任何错误，将其写入日志文件
        writeln!(&mut log_file, "FATAL ERROR: {}", e).expect("Failed to write final error to log");
        // 以失败状态码退出，这样父进程的 `status.success()` 就会是 false
        std::process::exit(1);
    }
    std::process::exit(0);
}

// ==================== Requirements hash utilities ====================

const REQUIREMENTS_HASH_FILE: &str = ".requirements_hash";

fn compute_requirements_hash(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_stored_requirements_hash(venv_dir: &Path) -> Option<String> {
    let hash_path = venv_dir.join(REQUIREMENTS_HASH_FILE);
    fs::read_to_string(&hash_path).ok().map(|s| s.trim().to_string())
}

fn write_requirements_hash(venv_dir: &Path, hash: &str) -> io::Result<()> {
    let hash_path = venv_dir.join(REQUIREMENTS_HASH_FILE);
    fs::write(&hash_path, hash)
}

// ==================== Dependency freshness check ====================

#[tauri::command]
pub fn check_deps_freshness(app: AppHandle) -> Result<serde_json::Value, String> {
    let venv_dir = get_venv_dir(&app);
    if !venv_dir.exists() || !venv_dir.join("Scripts").join("python.exe").exists() {
        return Ok(json!({ "needs_update": false, "reason": "no_venv" }));
    }

    let requirements_path = file_in_resources(&app, "monitor/requirements.txt")
        .unwrap_or_else(|| PathBuf::from("monitor/requirements.txt"));
    if !requirements_path.exists() {
        return Ok(json!({ "needs_update": false, "reason": "no_requirements_file" }));
    }

    let current_hash = compute_requirements_hash(&requirements_path)
        .map_err(|e| format!("Failed to hash requirements.txt: {}", e))?;

    match read_stored_requirements_hash(&venv_dir) {
        Some(stored_hash) => {
            if stored_hash == current_hash {
                Ok(json!({ "needs_update": false, "reason": "hashes_match" }))
            } else {
                tracing::info!(
                    "Requirements hash mismatch: stored={}, current={}",
                    stored_hash, current_hash
                );
                Ok(json!({ "needs_update": true, "reason": "hash_mismatch" }))
            }
        }
        None => {
            tracing::info!("No stored requirements hash found, deps sync needed");
            Ok(json!({ "needs_update": true, "reason": "no_stored_hash" }))
        }
    }
}

// ==================== Dependency sync command ====================

#[tauri::command]
pub async fn sync_python_deps(app: AppHandle) -> Result<String, String> {
    let venv_dir = get_venv_dir(&app);
    let python_exec = venv_dir.join("Scripts").join("python.exe");
    if !python_exec.is_file() {
        return Err("Virtual environment python.exe not found".into());
    }
    let python_exec_cmd = normalize_path_for_command(&python_exec);

    let requirements_path = file_in_resources(&app, "monitor/requirements.txt")
        .unwrap_or_else(|| PathBuf::from("monitor/requirements.txt"));
    if !requirements_path.exists() {
        return Err("requirements.txt not found".into());
    }

    let app_for_emit = app.clone();

    // Run pip install -r requirements.txt with streaming output
    let mut cmd_proc = Command::new(&python_exec_cmd);
    cmd_proc
        .arg("-u")
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("-r")
        .arg(normalize_path_for_command(&requirements_path))
        .arg("--progress-bar")
        .arg("off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }

    let mut child = cmd_proc.spawn().map_err(|e| format!("Failed to spawn pip: {}", e))?;

    let stdout = child.stdout.take().expect("failed to capture stdout");
    let stderr = child.stderr.take().expect("failed to capture stderr");

    let app_clone_stdout = app_for_emit.clone();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                let _ = app_clone_stdout.emit("install-log", json!({"source":"pip","line": line}));
            }
        }
    });

    let app_clone_stderr = app_for_emit.clone();
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                let _ = app_clone_stderr.emit("install-log", json!({"source":"pip","line": line}));
            }
        }
    });

    let status = child.wait().map_err(|e| format!("Failed waiting for pip: {}", e))?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    if !status.success() {
        let exit_code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
        return Err(format!("pip install failed (exit code {})", exit_code));
    }

    let _ = app_for_emit.emit("install-log", json!({"source":"installer","line": "Fixing onnxruntime/onnxruntime-directml conflict..."}));

    // ONNX runtime conflict resolution
    {
        let mut uninstall_cmd = Command::new(&python_exec_cmd);
        uninstall_cmd
            .arg("-m").arg("pip").arg("uninstall").arg("onnxruntime").arg("-y");
        #[cfg(windows)]
        {
            uninstall_cmd.creation_flags(0x08000000);
        }
        let _ = uninstall_cmd.output();

        let mut reinstall_cmd = Command::new(&python_exec_cmd);
        reinstall_cmd
            .arg("-m").arg("pip").arg("install")
            .arg("onnxruntime-directml==1.24.2")
            .arg("--force-reinstall").arg("--no-deps")
            .arg("-i").arg("https://mirrors.aliyun.com/pypi/simple/");
        #[cfg(windows)]
        {
            reinstall_cmd.creation_flags(0x08000000);
        }
        match reinstall_cmd.output() {
            Ok(output) => {
                let msg = String::from_utf8_lossy(&output.stdout);
                tracing::info!("onnxruntime-directml reinstall: {}", msg);
            }
            Err(e) => {
                tracing::warn!("Failed to reinstall onnxruntime-directml: {}", e);
            }
        }
    }

    // Write updated hash on success
    match compute_requirements_hash(&requirements_path) {
        Ok(hash) => {
            if let Err(e) = write_requirements_hash(&venv_dir, &hash) {
                tracing::warn!("Failed to write requirements hash after sync: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("Failed to compute requirements hash after sync: {}", e);
        }
    }

    let _ = app_for_emit.emit("install-log", json!({"source":"installer","line": "Dependency sync completed successfully."}));
    Ok("Dependencies synced successfully.".into())
}

// ==================== spaCy model management ====================

/// Install a spaCy model package (e.g. `zh_core_web_sm`, `en_core_web_sm`)
/// using pip from the existing venv.  Streams progress via `install-log` events.
#[tauri::command]
pub async fn install_spacy_model(
    app: AppHandle,
    model_name: String,
) -> Result<String, String> {
    // Validate model name to prevent injection
    let allowed = ["zh_core_web_sm", "en_core_web_sm"];
    if !allowed.contains(&model_name.as_str()) {
        return Err(format!("Unsupported spaCy model: {}", model_name));
    }

    let venv_dir = get_venv_dir(&app);
    let python_exec = venv_dir.join("Scripts").join("python.exe");
    if !python_exec.is_file() {
        let msg = format!("Virtual environment python.exe not found at {:?}", python_exec);
        tracing::error!("install_spacy_model: {}", msg);
        return Err(msg);
    }
    let python_exec_cmd = normalize_path_for_command(&python_exec);

    // For transformer models, install spacy-transformers first
    if model_name.contains("trf") {
        tracing::info!("install_spacy_model: installing spacy-transformers prerequisite");
        let _ = app.emit("install-log", json!({
            "source": "spacy",
            "line": "Installing spacy-transformers prerequisite..."
        }));

        let mut trf_cmd = Command::new(&python_exec_cmd);
        trf_cmd
            .arg("-u")
            .arg("-m")
            .arg("pip")
            .arg("install")
            .arg("spacy-transformers")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            trf_cmd.creation_flags(0x08000000);
        }

        match trf_cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    tracing::info!("install_spacy_model: spacy-transformers installed successfully");
                    let _ = app.emit("install-log", json!({
                        "source": "spacy",
                        "line": "spacy-transformers installed."
                    }));
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!("install_spacy_model: spacy-transformers install failed: {}", stderr.trim());
                    let _ = app.emit("install-log", json!({
                        "source": "spacy",
                        "line": format!("Warning: spacy-transformers install failed, continuing anyway...")
                    }));
                }
            }
            Err(e) => {
                tracing::warn!("install_spacy_model: failed to run pip install spacy-transformers: {}", e);
            }
        }
    }

    tracing::info!("install_spacy_model: installing {} via {}", model_name, python_exec_cmd);
    let _ = app.emit("install-log", json!({
        "source": "spacy",
        "line": format!("Installing spaCy model: {}...", model_name)
    }));

    // Primary method: `python -m spacy download <model>`
    // This is the official way — spaCy resolves the compatible model version
    // and downloads from GitHub releases automatically.
    let mut cmd_proc = Command::new(&python_exec_cmd);
    cmd_proc
        .arg("-u")
        .arg("-m")
        .arg("spacy")
        .arg("download")
        .arg(&model_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        cmd_proc.creation_flags(0x08000000);
    }

    tracing::info!("install_spacy_model: spawning `python -m spacy download {}`", model_name);
    let _ = app.emit("install-log", json!({
        "source": "spacy",
        "line": format!("Running: python -m spacy download {}...", model_name)
    }));

    let mut child = cmd_proc.spawn().map_err(|e| {
        let msg = format!("Failed to spawn spacy download: {}", e);
        tracing::error!("install_spacy_model: {}", msg);
        msg
    })?;

    let stdout = child.stdout.take().expect("failed to capture stdout");
    let stderr = child.stderr.take().expect("failed to capture stderr");

    let app_clone_stdout = app.clone();
    let model_name_for_log = model_name.clone();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                tracing::info!("install_spacy_model[{}] stdout: {}", model_name_for_log, line);
                let _ = app_clone_stdout.emit("install-log", json!({"source":"spacy","line": line}));
            }
        }
    });

    let app_clone_stderr = app.clone();
    let model_name_for_log2 = model_name.clone();
    let stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines_clone = Arc::clone(&stderr_lines);
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                tracing::warn!("install_spacy_model[{}] stderr: {}", model_name_for_log2, line);
                let _ = app_clone_stderr.emit("install-log", json!({"source":"spacy","line": line}));
                if let Ok(mut lines) = stderr_lines_clone.lock() {
                    lines.push(line);
                }
            }
        }
    });

    let status = child.wait().map_err(|e| {
        let msg = format!("Failed waiting for spacy download: {}", e);
        tracing::error!("install_spacy_model: {}", msg);
        msg
    })?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    let exit_code_1 = status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
    tracing::info!("install_spacy_model: `spacy download {}` exit code: {}", model_name, exit_code_1);

    if status.success() {
        tracing::info!("install_spacy_model: {} installed successfully via spacy download", model_name);
        let _ = crate::registry_config::set_bool(&spacy_reg_key(&model_name), true);
        let _ = app.emit("install-log", json!({
            "source": "spacy",
            "line": format!("spaCy model {} installed successfully.", model_name)
        }));
        return Ok(format!("{} installed successfully", model_name));
    }

    // Collect stderr from first attempt for diagnostics
    let first_stderr = stderr_lines.lock()
        .map(|lines| lines.join("\n"))
        .unwrap_or_default();
    tracing::warn!(
        "install_spacy_model: `spacy download` failed (exit code {}), stderr:\n{}",
        exit_code_1, first_stderr
    );

    // Fallback: query installed spaCy version, then pip install the matching GitHub wheel
    let _ = app.emit("install-log", json!({
        "source": "spacy",
        "line": format!("`spacy download` failed (exit code {}), trying direct GitHub wheel...", exit_code_1)
    }));

    // Get the installed spaCy version to determine the compatible model version
    tracing::info!("install_spacy_model: querying spaCy version for fallback wheel URL");
    let mut ver_cmd = Command::new(&python_exec_cmd);
    ver_cmd
        .arg("-c")
        .arg("import spacy; print(spacy.__version__)");
    #[cfg(windows)]
    {
        ver_cmd.creation_flags(0x08000000);
    }
    let ver_output = ver_cmd.output().map_err(|e| {
        let msg = format!("Failed to query spaCy version: {}", e);
        tracing::error!("install_spacy_model: {}", msg);
        msg
    })?;

    let spacy_version = String::from_utf8_lossy(&ver_output.stdout).trim().to_string();
    if spacy_version.is_empty() || !ver_output.status.success() {
        let ver_stderr = String::from_utf8_lossy(&ver_output.stderr);
        tracing::error!("install_spacy_model: could not determine spaCy version. stderr: {}", ver_stderr);
        return Err(format!(
            "Failed to install {} (`spacy download` exit code {}). Could not determine spaCy version for fallback.\n\
             Last output:\n{}",
            model_name, exit_code_1,
            first_stderr.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    tracing::info!("install_spacy_model: detected spaCy version: {}", spacy_version);

    // Extract major.minor from spaCy version (e.g. "3.7.5" → "3.7")
    let spacy_major_minor: String = {
        let parts: Vec<&str> = spacy_version.split('.').collect();
        if parts.len() >= 2 {
            format!("{}.{}", parts[0], parts[1])
        } else {
            spacy_version.clone()
        }
    };

    // spaCy model versions track spaCy major.minor, with patch starting at 0
    // e.g. spaCy 3.7.x → model 3.7.0, spaCy 3.8.x → model 3.8.0
    let model_version = format!("{}.0", spacy_major_minor);
    let whl_url = format!(
        "https://gh-proxy.com/https://github.com/explosion/spacy-models/releases/download/{model}-{ver}/{model}-{ver}-py3-none-any.whl",
        model = model_name,
        ver = model_version,
    );
    tracing::info!("install_spacy_model: fallback URL: {}", whl_url);
    let _ = app.emit("install-log", json!({
        "source": "spacy",
        "line": format!("Downloading from GitHub via gh-proxy: {}-{}...", model_name, model_version)
    }));

    let mut fallback_cmd = Command::new(&python_exec_cmd);
    fallback_cmd
        .arg("-u")
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg(&whl_url)
        .arg("--progress-bar")
        .arg("off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        fallback_cmd.creation_flags(0x08000000);
    }

    let mut child2 = fallback_cmd.spawn().map_err(|e| {
        let msg = format!("Fallback spawn failed: {}", e);
        tracing::error!("install_spacy_model: {}", msg);
        msg
    })?;

    let stdout2 = child2.stdout.take().expect("failed to capture stdout");
    let stderr2 = child2.stderr.take().expect("failed to capture stderr");

    let app_clone2 = app.clone();
    let model_name_for_log3 = model_name.clone();
    let stdout_handle2 = thread::spawn(move || {
        let reader = BufReader::new(stdout2);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                tracing::info!("install_spacy_model[{}] fallback stdout: {}", model_name_for_log3, line);
                let _ = app_clone2.emit("install-log", json!({"source":"spacy","line": line}));
            }
        }
    });

    let app_clone3 = app.clone();
    let model_name_for_log4 = model_name.clone();
    let stderr_lines2: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines2_clone = Arc::clone(&stderr_lines2);
    let stderr_handle2 = thread::spawn(move || {
        let reader = BufReader::new(stderr2);
        for line_res in reader.lines() {
            if let Ok(line) = line_res {
                tracing::warn!("install_spacy_model[{}] fallback stderr: {}", model_name_for_log4, line);
                let _ = app_clone3.emit("install-log", json!({"source":"spacy","line": line}));
                if let Ok(mut lines) = stderr_lines2_clone.lock() {
                    lines.push(line);
                }
            }
        }
    });

    let status2 = child2.wait().map_err(|e| {
        let msg = format!("Fallback wait failed: {}", e);
        tracing::error!("install_spacy_model: {}", msg);
        msg
    })?;
    let _ = stdout_handle2.join();
    let _ = stderr_handle2.join();

    let exit_code_2 = status2.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
    tracing::info!("install_spacy_model: fallback pip exit code: {}", exit_code_2);

    if status2.success() {
        tracing::info!("install_spacy_model: {} installed successfully from GitHub wheel", model_name);
        let _ = crate::registry_config::set_bool(&spacy_reg_key(&model_name), true);
        let _ = app.emit("install-log", json!({
            "source": "spacy",
            "line": format!("spaCy model {} installed successfully (from GitHub).", model_name)
        }));
        Ok(format!("{} installed successfully", model_name))
    } else {
        let fallback_stderr = stderr_lines2.lock()
            .map(|lines| lines.join("\n"))
            .unwrap_or_default();
        tracing::error!(
            "install_spacy_model: both attempts failed for {}.\n\
             `spacy download` exit code: {}, GitHub wheel exit code: {}\n\
             `spacy download` stderr:\n{}\n\
             GitHub wheel stderr:\n{}",
            model_name, exit_code_1, exit_code_2, first_stderr, fallback_stderr
        );

        // Build a user-facing error with the last few stderr lines
        let last_lines: String = {
            let combined = if !fallback_stderr.is_empty() { &fallback_stderr } else { &first_stderr };
            let lines: Vec<&str> = combined.lines().collect();
            let start = if lines.len() > 10 { lines.len() - 10 } else { 0 };
            lines[start..].join("\n")
        };
        Err(format!(
            "Failed to install {} (`spacy download` exit code {}, GitHub wheel exit code {}).\nLast output:\n{}",
            model_name, exit_code_1, exit_code_2, last_lines
        ))
    }
}

/// Probe a single spaCy model by spawning Python to try importing it.
fn check_single_spacy_model(python_exec_cmd: &str, model: &str) -> bool {
    let mut cmd = Command::new(python_exec_cmd);
    cmd.arg("-c").arg(format!("import {}; print('ok')", model));
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    match cmd.output() {
        Ok(output) => {
            let ok = output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "ok";
            if !ok {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(
                    "check_single_spacy_model: {} not available (exit={:?}, stderr={})",
                    model, output.status.code(), stderr.trim()
                );
            }
            ok
        }
        Err(e) => {
            tracing::warn!("check_single_spacy_model: failed to probe {}: {}", model, e);
            false
        }
    }
}

/// Registry key name for a spaCy model install cache.
fn spacy_reg_key(model: &str) -> String {
    format!("spacy_model_{}_installed", model)
}

/// Check which spaCy models are installed. Uses registry cache when available;
/// only spawns Python processes when the cache key is missing.
#[tauri::command]
pub fn check_spacy_models(app: AppHandle) -> Result<serde_json::Value, String> {
    use crate::registry_config;

    let models = ["zh_core_web_sm", "en_core_web_sm"];

    let venv_dir = get_venv_dir(&app);
    let python_exec = venv_dir.join("Scripts").join("python.exe");
    let have_python = python_exec.is_file();

    let mut result = serde_json::Map::new();

    for model in &models {
        let reg_key = spacy_reg_key(model);
        let installed = match registry_config::get_bool(&reg_key) {
            Some(cached) => {
                tracing::debug!("check_spacy_models: {} cached={}", model, cached);
                cached
            }
            None => {
                if !have_python {
                    tracing::debug!("check_spacy_models: no python, {} = false", model);
                    false
                } else {
                    let python_exec_cmd = normalize_path_for_command(&python_exec);
                    let probed = check_single_spacy_model(&python_exec_cmd, model);
                    let _ = registry_config::set_bool(&reg_key, probed);
                    tracing::info!("check_spacy_models: probed {} = {}, cached", model, probed);
                    probed
                }
            }
        };
        result.insert(model.to_string(), json!({ "installed": installed }));
    }

    tracing::info!(
        "check_spacy_models: zh_sm={}, en_sm={}",
        result.get("zh_core_web_sm").and_then(|v| v.get("installed")).and_then(|v| v.as_bool()).unwrap_or(false),
        result.get("en_core_web_sm").and_then(|v| v.get("installed")).and_then(|v| v.as_bool()).unwrap_or(false),
    );

    Ok(serde_json::Value::Object(result))
}

/// Clear cached spaCy model status and re-probe via Python.
#[tauri::command]
pub fn force_recheck_spacy_models(app: AppHandle) -> Result<serde_json::Value, String> {
    use crate::registry_config;

    let models = ["zh_core_web_sm", "en_core_web_sm"];

    // Clear cache
    for model in &models {
        let reg_key = spacy_reg_key(model);
        // set to false first to clear, then we will overwrite with the real value
        let _ = registry_config::set_bool(&reg_key, false);
    }

    let venv_dir = get_venv_dir(&app);
    let python_exec = venv_dir.join("Scripts").join("python.exe");
    if !python_exec.is_file() {
        return Ok(json!({
            "zh_core_web_sm": { "installed": false },
            "en_core_web_sm": { "installed": false }
        }));
    }
    let python_exec_cmd = normalize_path_for_command(&python_exec);

    let mut result = serde_json::Map::new();
    for model in &models {
        let installed = check_single_spacy_model(&python_exec_cmd, model);
        let reg_key = spacy_reg_key(model);
        let _ = registry_config::set_bool(&reg_key, installed);
        tracing::info!("force_recheck_spacy_models: {} = {}", model, installed);
        result.insert(model.to_string(), json!({ "installed": installed }));
    }

    Ok(serde_json::Value::Object(result))
}

/// Background auto-install of missing spaCy models at app startup.
pub fn auto_install_spacy_models(app: AppHandle) {
    use crate::registry_config;

    thread::spawn(move || {
        let models = ["zh_core_web_sm", "en_core_web_sm"];

        let venv_dir = get_venv_dir(&app);
        let python_exec = venv_dir.join("Scripts").join("python.exe");
        if !python_exec.is_file() {
            tracing::debug!("auto_install_spacy_models: no venv python found, skipping");
            return;
        }
        let python_exec_cmd = normalize_path_for_command(&python_exec);

        for model in &models {
            let reg_key = spacy_reg_key(model);
            let installed = match registry_config::get_bool(&reg_key) {
                Some(cached) => cached,
                None => {
                    let probed = check_single_spacy_model(&python_exec_cmd, model);
                    let _ = registry_config::set_bool(&reg_key, probed);
                    probed
                }
            };

            if installed {
                tracing::info!("auto_install_spacy_models: {} already installed", model);
                continue;
            }

            tracing::info!("auto_install_spacy_models: {} not installed, starting install", model);
            let _ = app.emit("spacy-model-status", json!({
                "model": model,
                "status": "installing"
            }));

            let app_clone = app.clone();
            let model_name = model.to_string();
            // Use a blocking call within this thread (models install sequentially)
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match rt {
                Ok(rt) => rt.block_on(install_spacy_model(app_clone, model_name.clone())),
                Err(e) => Err(format!("Failed to create runtime: {}", e)),
            };

            match result {
                Ok(_) => {
                    let _ = registry_config::set_bool(&reg_key, true);
                    tracing::info!("auto_install_spacy_models: {} installed successfully", model);
                    let _ = app.emit("spacy-model-status", json!({
                        "model": model,
                        "status": "installed"
                    }));
                }
                Err(e) => {
                    tracing::error!("auto_install_spacy_models: {} install failed: {}", model, e);
                    let _ = app.emit("spacy-model-status", json!({
                        "model": model,
                        "status": "failed",
                        "error": e
                    }));
                }
            }
        }
        tracing::info!("auto_install_spacy_models: finished");
    });
}
