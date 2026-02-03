use crate::resource_utils::{file_in_local_appdata, file_in_resources, find_existing_file_in_resources, get_log_path};
use anyhow::{anyhow, Context, Result};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::json;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::AppHandle;
use tauri::Emitter; // 或者你当前代码中实际使用的类型

use tokio::sync::Semaphore;
use tokio::task;

/// percent-encode 用于 path segment
const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'#')
    .add(b'?')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'[')
    .add(b']')
    .add(b'\'')
    .add(b'%');

fn build_file_url(repo: &str, relpath: &str) -> String {
    let encoded = relpath
        .split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SEGMENT_ENCODE_SET).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("https://hf-mirror.com/{}/resolve/main/{}", repo, encoded)
}

/// 简易宏：从字符串字面量创建 `Vec<String>`，调用处无需逐个 `.to_string()`。
macro_rules! svec {
    ($($x:expr),* $(,)?) => {
        vec![$($x.to_string()),*]
    };
}
/// 在阻塞线程中启动 aria2c，并把 stdout/stderr 每行通过 app.emit 发送给前端，同时写入 log_file。
///
/// - `aria2_path`：aria2c 可执行文件的路径
/// - `url`：要下载的 URL
/// - `target_dir`：aria2 的 -d 参数
/// - `outfile`：aria2 的 -o 参数（文件名，不含目录）
/// - `app`：Tauri 的 AppHandle 或者你的 `app` 类型（需实现 emit）
/// - `log_file`：用于写入日志的共享文件（Arc<Mutex<File>>）
fn run_aria2_and_emit_blocking(
    aria2_path: PathBuf,
    url: String,
    target_dir: PathBuf,
    outfile: String,
    app: AppHandle,
    log_file: Arc<Mutex<File>>,
) -> Result<()> {
    // spawn aria2c
    let mut child = std::process::Command::new(aria2_path)
        .arg("--continue=true")
        .arg("--enable-rpc=false")
        .arg("--max-connection-per-server=4")
        .arg("--split=4")
        .arg("-d")
        .arg(&target_dir)
        .arg("-o")
        .arg(&outfile)
        .arg(&url)
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn aria2c for url {}", url))?;

    // 取出 stdout/stderr 的管道
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture aria2c stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture aria2c stderr"))?;

    // clone 用于线程
    let log_for_stdout = Arc::clone(&log_file);
    let log_for_stderr = Arc::clone(&log_file);
    let app_for_stdout = app.clone();
    let app_for_stderr = app.clone();
    let outfile_clone_for_stdout = outfile.clone();
    let outfile_clone_for_stderr = outfile.clone();

    // stdout 线程：逐行读并 emit、写日志
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    // 写到共享日志文件
                    if let Ok(mut f) = log_for_stdout.lock() {
                        let _ = writeln!(&mut *f, "{}", line);
                        let _ = f.flush();
                    }
                    // 发送给前端；如果你的 app API 不是直接 emit，请替换为你项目中可用的 emit 写法
                    let _ = app_for_stdout.emit(
                        "install-log",
                        json!({
                            "source": "aria2",
                            "file": outfile_clone_for_stdout,
                            "line": line,
                            "level": "stdout"
                        }),
                    );
                }
                Err(e) => {
                    if let Ok(mut f) = log_for_stdout.lock() {
                        let _ = writeln!(&mut *f, "Error reading aria2 stdout: {}", e);
                        let _ = f.flush();
                    }
                }
            }
        }
    });

    // stderr 线程：同理
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    if let Ok(mut f) = log_for_stderr.lock() {
                        let _ = writeln!(&mut *f, "{}", line);
                        let _ = f.flush();
                    }
                    let _ = app_for_stderr.emit(
                        "install-log",
                        json!({
                            "source": "aria2",
                            "file": outfile_clone_for_stderr,
                            "line": line,
                            "level": "stderr"
                        }),
                    );
                }
                Err(e) => {
                    if let Ok(mut f) = log_for_stderr.lock() {
                        let _ = writeln!(&mut *f, "Error reading aria2 stderr: {}", e);
                        let _ = f.flush();
                    }
                }
            }
        }
    });

    // 等待子进程结束（阻塞）
    let status = child.wait().map_err(|e| {
        // 记录并返回错误
        if let Ok(mut f) = log_file.lock() {
            let _ = writeln!(&mut *f, "Failed waiting for aria2c process: {}", e);
            let _ = f.flush();
        }
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed waiting for aria2c: {}", e),
        )
    })?;

    // 等待读取线程结束
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    if !status.success() {
        return Err(anyhow!("aria2c exited with status {:?}", status.code()));
    }

    Ok(())
}

/// 下载 Hugging Face 仓库中的指定文件列表，使用 aria2 并发下载，通过 Tauri 事件发送日志。
/// - `app`：Tauri 的 AppHandle，用于发送事件
/// - `aria2_path`：aria2c 可执行文件路径
/// - `download_path`：下载文件的目标根目录（文件将直接保存在此目录及其子目录中）
/// - `files`：要下载的文件相对路径列表（相对于仓库根目录）
/// - `concurrency`：最大并发下载数
async fn perform_download_hf_repo(
    app: AppHandle,
    aria2_path: PathBuf,
    download_path: PathBuf, // 修改点1：参数名变更
    repo: &str,
    files: Vec<String>,
    concurrency: usize,
    log_file: Arc<Mutex<File>>,
) -> Result<PathBuf> {
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::with_capacity(files.len());

    print!("Starting download of {} files from repo {}...\n", files.len(), repo);

    // 如果 repo 字符串需要在循环中使用且原函数没有对其所有权的处理，
    // 在这里由于 build_file_url 是在 spawn 之前调用的，直接使用 &str 是安全的。

    for relpath in files {
        let aria2_path = aria2_path.clone();
        // 修改点2：克隆 download_path 而不是 cache_base
        let download_path = download_path.clone(); 
        let app = app.clone();
        let log_file = Arc::clone(&log_file);
        let sem = Arc::clone(&sem);

        // 修改点3：直接基于 download_path 拼接目标路径
        // 如果 relpath 是 "bert/config.json"，target 就是 "download_path/bert/config.json"
        let target = download_path.join(&relpath);
        
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        
        let parent_dir = target
            .parent()
            .ok_or_else(|| anyhow!("无法确定文件父目录"))?
            .to_path_buf();
            
        let outfile = target
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("非法文件名"))?
            .to_string();

        // 跳过已存在文件（简单判断）
        // 这里保留了原来的逻辑：如果目标路径下已有文件且大小>0，则视为已下载/已缓存
        if let Ok(md) = tokio::fs::metadata(&target).await {
            if md.len() > 0 {
                // 直接跳过
                continue;
            }
        }

        // 构建下载 URL
        // 注意：repo 变量在这里使用，因为是在 tokio::spawn 外部，引用是有效的
        let url = build_file_url(repo, &relpath); 

        // spawn 异步任务
        let handle = tokio::spawn(async move {
            // acquire semaphore permit
            let _permit = sem.acquire_owned().await.unwrap();

            // 在阻塞线程池里执行 aria2 子进程
            let res = task::spawn_blocking(move || {
                run_aria2_and_emit_blocking(aria2_path, url, parent_dir, outfile, app, log_file)
            })
            .await
            .map_err(|e| anyhow!("spawn_blocking join error: {}", e))?;

            res
        });

        handles.push(handle);
    }

    // 等待所有任务
    let mut any_err: Option<anyhow::Error> = None;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if any_err.is_none() {
                    any_err = Some(e);
                }
            }
            Err(join_err) => {
                if any_err.is_none() {
                    any_err = Some(anyhow!("下载任务被取消或崩溃: {}", join_err));
                }
            }
        }
    }

    if let Some(e) = any_err {
        Err(anyhow!("部分文件下载失败：{}", e))
    } else {
        // 修改点4：返回 download_path
        Ok(download_path) 
    }
}

#[tauri::command]
/// Download model files from Hugging Face repository using aria2 with concurrency and logging.
/// - `app`: Tauri AppHandle for emitting events
/// - `files`: List of file paths (relative to repo root) to download
/// - `concurrency`: Optional maximum number of concurrent downloads
pub async fn download_model(
    app: AppHandle,
    files: Option<Vec<String>>,
    repo: Option<&str>,
    concurrency: Option<usize>,
) -> Result<String, String> {
    // prepare aria2 path
    /*let aria2 = aria2_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("aria2c"));
    */
    let aria2 = find_existing_file_in_resources(&app, "aria2c.exe")
        .ok_or_else(|| "aria2c executable not found in resources; The program installation may be incomplete.".to_string())?;

    // prepare cache dir
    let mut download_path = file_in_local_appdata()
        .ok_or_else(|| "Could not determine resource directory.".to_string())?
        .join("models");
    //download_path.push("model_cache");
    std::fs::create_dir_all(&download_path).map_err(|e| e.to_string())?;

    // prepare log file under log path
    let log_dir = get_log_path();
    // delete log file if exists
    let log_path = log_dir;
    if log_path.exists() {
        std::fs::remove_file(&log_path).map_err(|e| e.to_string())?;
    }
    let f = File::create(&log_path).map_err(|e| e.to_string())?;
    let log_file = Arc::new(Mutex::new(f));

    let conc = concurrency.unwrap_or(8);
    let repo = repo.unwrap_or("OFA-Sys/chinese-clip-vit-base-patch16");
    let files = files.unwrap_or_else(|| svec![
        "vocab.txt",
        "pytorch_model.bin",
        "config.json",
        "preprocessor_config.json",
    ]);

    match perform_download_hf_repo(app.clone(), aria2, download_path, repo, files, conc, log_file).await {
        Ok(path) => Ok(path.to_string_lossy().to_string()),
        Err(e) => Err(format!("download_model error: {}", e)),
    }
}
