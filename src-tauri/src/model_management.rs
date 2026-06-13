use crate::registry_config;
use crate::resource_utils::{file_in_local_appdata, find_existing_file_in_resources, get_log_path};
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

fn file_exists_nonempty(path: &std::path::Path) -> bool {
    path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

fn missing_files(base: &std::path::Path, files: &[&str]) -> Vec<String> {
    files
        .iter()
        .filter(|file| !file_exists_nonempty(&base.join(file)))
        .map(|file| file.to_string())
        .collect()
}

fn missing_files_with_alternative(
    base: &std::path::Path,
    files: &[&str],
    alternatives: &[&str],
    display_missing: &str,
) -> Vec<String> {
    let mut missing = missing_files(base, files);
    if !alternatives
        .iter()
        .any(|file| file_exists_nonempty(&base.join(file)))
    {
        missing.push(display_missing.to_string());
    }
    missing
}

fn insert_status_with_fallback(
    result: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    primary_base: &std::path::Path,
    legacy_base: Option<&std::path::Path>,
    required: bool,
    check_missing: impl Fn(&std::path::Path) -> Vec<String>,
) {
    let primary_missing = check_missing(primary_base);
    let legacy_missing = legacy_base.map(|base| check_missing(base));
    let complete = primary_missing.is_empty()
        || legacy_missing
            .as_ref()
            .map(|m| m.is_empty())
            .unwrap_or(false);

    if complete {
        result.insert(
            key.to_string(),
            json!({ "complete": true, "required": required }),
        );
    } else {
        result.insert(
            key.to_string(),
            json!({
                "complete": false,
                "required": required,
                "missing_files": primary_missing
            }),
        );
    }
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
        std::io::Error::other(format!("Failed waiting for aria2c: {}", e))
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
    download_path: PathBuf,
    repo: &str,
    files: Vec<String>,
    concurrency: usize,
    log_file: Arc<Mutex<File>>,
) -> Result<PathBuf> {
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::with_capacity(files.len());

    println!(
        "Starting download of {} files from repo {}...",
        files.len(),
        repo
    );

    for relpath in files {
        let aria2_path = aria2_path.clone();
        let download_path = download_path.clone();
        let app = app.clone();
        let log_file = Arc::clone(&log_file);
        let sem = Arc::clone(&sem);

        let target = download_path.join(&relpath);

        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
            let canonical_parent = parent
                .canonicalize()
                .map_err(|e| anyhow!("Failed to canonicalize parent dir: {}", e))?;
            let canonical_root = download_path
                .canonicalize()
                .map_err(|e| anyhow!("Failed to canonicalize download path: {}", e))?;
            if !canonical_parent.starts_with(&canonical_root) {
                return Err(anyhow!(
                    "Directory traversal detected via symbolic link / boundary escape"
                ));
            }
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

        if let Ok(md) = tokio::fs::metadata(&target).await {
            if md.len() > 0 {
                continue;
            }
        }

        let url = build_file_url(repo, &relpath);

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();

            let res = task::spawn_blocking(move || {
                run_aria2_and_emit_blocking(aria2_path, url, parent_dir, outfile, app, log_file)
            })
            .await
            .map_err(|e| anyhow!("spawn_blocking join error: {}", e))?;

            res
        });

        handles.push(handle);
    }

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
        Ok(download_path)
    }
}

fn is_safe_relative_path(rel: &str) -> bool {
    use std::path::{Component, Path};
    !rel.trim().is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn is_valid_repo(repo: &str) -> bool {
    let trimmed = repo.trim();
    if trimmed.is_empty() {
        return false;
    }
    !trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[tauri::command]
/// Download model files from Hugging Face repository using aria2 with concurrency and logging.
/// - `app`: Tauri AppHandle for emitting events
/// - `files`: List of file paths (relative to repo root) to download
/// - `subdir`: Optional subdirectory under `models/` to download into
/// - `concurrency`: Optional maximum number of concurrent downloads
pub async fn download_model(
    app: AppHandle,
    files: Option<Vec<String>>,
    repo: Option<&str>,
    subdir: Option<&str>,
    concurrency: Option<usize>,
    model_runtime: Option<&str>,
) -> Result<String, String> {
    if !registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("Network features are disabled".to_string());
    }

    let aria2 = find_existing_file_in_resources(&app, "aria2c.exe").ok_or_else(|| {
        "aria2c executable not found in resources; The program installation may be incomplete."
            .to_string()
    })?;

    // Validate inputs
    if let Some(sub) = subdir {
        if !is_safe_relative_path(sub) {
            return Err("Invalid subdir relative path".to_string());
        }
    }
    if let Some(r) = repo {
        if !is_valid_repo(r) {
            return Err("Invalid model repository name".to_string());
        }
    }

    // prepare cache dir. Core ONNX models are isolated from PyTorch weights to
    // avoid mixing incompatible config/tokenizer files in the same directory.
    let use_onnx = registry_config::get_bool("use_onnx").unwrap_or(true);
    let runtime = model_runtime.unwrap_or("").trim().to_ascii_lowercase();
    let use_onnx_root =
        runtime == "onnx" || (runtime.is_empty() && use_onnx && subdir.is_none() && repo.is_none());
    let mut download_path = file_in_local_appdata()
        .ok_or_else(|| "Could not determine resource directory.".to_string())?
        .join(if use_onnx_root {
            "models-onnx"
        } else {
            "models"
        });
    if let Some(sub) = subdir {
        download_path = download_path.join(sub);
    }
    std::fs::create_dir_all(&download_path).map_err(|e| e.to_string())?;

    // prepare log file under log path
    let log_dir = get_log_path();
    let log_path = log_dir;
    if log_path.exists() {
        std::fs::remove_file(&log_path).map_err(|e| e.to_string())?;
    }
    let f = File::create(&log_path).map_err(|e| e.to_string())?;
    let log_file = Arc::new(Mutex::new(f));

    let conc = concurrency.unwrap_or(8);
    let repo = repo.unwrap_or_else(|| {
        if use_onnx_root {
            "Xenova/chinese-clip-vit-base-patch16"
        } else {
            "OFA-Sys/chinese-clip-vit-base-patch16"
        }
    });
    let files = files.unwrap_or_else(|| {
        if use_onnx_root {
            svec![
                "vocab.txt",
                "config.json",
                "preprocessor_config.json",
                "onnx/model_q4.onnx",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
            ]
        } else {
            svec![
                "vocab.txt",
                "pytorch_model.bin",
                "config.json",
                "preprocessor_config.json",
            ]
        }
    });

    // Validate each file relative path
    for file in &files {
        if !is_safe_relative_path(file) {
            return Err(format!("Invalid file relative path: {}", file));
        }
    }

    match perform_download_hf_repo(
        app.clone(),
        aria2,
        download_path,
        repo,
        files,
        conc,
        log_file,
    )
    .await
    {
        Ok(path) => Ok(path.to_string_lossy().to_string()),
        Err(e) => Err(format!("download_model error: {}", e)),
    }
}

#[tauri::command]
/// Check whether all required model files exist on disk.
/// Returns a JSON object mapping each model key to its status.
pub async fn check_model_files() -> Result<serde_json::Value, String> {
    let appdata_dir = file_in_local_appdata()
        .ok_or_else(|| "Could not determine local appdata directory.".to_string())?;
    let models_dir = appdata_dir.join("models");
    let onnx_models_dir = appdata_dir.join("models-onnx");

    let use_onnx = registry_config::get_bool("use_onnx").unwrap_or(true);
    let mut result = serde_json::Map::new();

    if use_onnx {
        insert_status_with_fallback(
            &mut result,
            "chinese-clip",
            &onnx_models_dir,
            Some(&models_dir),
            true,
            |base| {
                missing_files(
                    base,
                    &[
                        "vocab.txt",
                        "config.json",
                        "preprocessor_config.json",
                        "onnx/model_q4.onnx",
                    ],
                )
            },
        );
        insert_status_with_fallback(
            &mut result,
            "bge-small-zh",
            &onnx_models_dir.join("bge-small-zh-v1.5"),
            Some(&models_dir.join("bge-small-zh-v1.5")),
            true,
            |base| {
                missing_files_with_alternative(
                    base,
                    &[
                        "config.json",
                        "tokenizer.json",
                        "tokenizer_config.json",
                        "special_tokens_map.json",
                    ],
                    &["onnx/model_quantized.onnx", "model_int8.onnx"],
                    "onnx/model_quantized.onnx",
                )
            },
        );
        insert_status_with_fallback(
            &mut result,
            "minilm-l12",
            &onnx_models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"),
            Some(&models_dir.join("paraphrase-multilingual-MiniLM-L12-v2")),
            true,
            |base| {
                missing_files_with_alternative(
                    base,
                    &[
                        "config.json",
                        "tokenizer.json",
                        "tokenizer_config.json",
                        "special_tokens_map.json",
                    ],
                    &["onnx/model_quantized.onnx", "model_int8.onnx"],
                    "onnx/model_quantized.onnx",
                )
            },
        );
    } else {
        insert_status_with_fallback(
            &mut result,
            "chinese-clip",
            &models_dir,
            None,
            true,
            |base| {
                missing_files(
                    base,
                    &[
                        "vocab.txt",
                        "pytorch_model.bin",
                        "config.json",
                        "preprocessor_config.json",
                    ],
                )
            },
        );
        insert_status_with_fallback(
            &mut result,
            "bge-small-zh",
            &models_dir.join("bge-small-zh-v1.5"),
            None,
            true,
            |base| {
                missing_files(
                    base,
                    &[
                        "config.json",
                        "pytorch_model.bin",
                        "tokenizer.json",
                        "tokenizer_config.json",
                        "vocab.txt",
                        "special_tokens_map.json",
                    ],
                )
            },
        );
        insert_status_with_fallback(
            &mut result,
            "minilm-l12",
            &models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"),
            None,
            true,
            |base| {
                missing_files(
                    base,
                    &[
                        "config.json",
                        "pytorch_model.bin",
                        "tokenizer.json",
                        "tokenizer_config.json",
                        "special_tokens_map.json",
                        "sentencepiece.bpe.model",
                    ],
                )
            },
        );
    }

    insert_status_with_fallback(
        &mut result,
        "bge-reranker-v2-m3",
        &models_dir.join("bge-reranker-v2-m3"),
        None,
        false,
        |base| {
            missing_files(
                base,
                &[
                    "config.json",
                    "tokenizer.json",
                    "tokenizer_config.json",
                    "special_tokens_map.json",
                    "onnx/model_uint8.onnx",
                ],
            )
        },
    );

    Ok(serde_json::Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_relative_path() {
        assert!(is_safe_relative_path("onnx/model.onnx"));
        assert!(is_safe_relative_path("config.json"));
        assert!(is_safe_relative_path("a/b/c"));

        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path(" "));
        assert!(!is_safe_relative_path(".."));
        assert!(!is_safe_relative_path("../a"));
        assert!(!is_safe_relative_path("a/../b"));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("C:\\Windows\\temp"));
        assert!(!is_safe_relative_path("\\\\?\\C:\\Windows"));
    }

    #[test]
    fn test_is_valid_repo() {
        assert!(is_valid_repo("Xenova/chinese-clip-vit-base-patch16"));
        assert!(is_valid_repo("OFA-Sys/chinese-clip-vit-base-patch16"));

        assert!(!is_valid_repo(""));
        assert!(!is_valid_repo("  "));
        assert!(!is_valid_repo("repo name"));
        assert!(!is_valid_repo("repo\nname"));
    }
}
