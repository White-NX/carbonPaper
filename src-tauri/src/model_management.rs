//! Download, verification, installation, and discovery of local ML model artifacts.

use crate::registry_config;
use crate::resource_utils::{file_in_local_appdata, find_existing_file_in_resources};
use anyhow::{anyhow, Context, Result};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::AppHandle;
use tauri::Emitter; // 或者你当前代码中实际使用的类型

use tokio::sync::Semaphore;
use tokio::task;

static MODEL_DOWNLOAD_LOCKS: once_cell::sync::Lazy<
    Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
> = once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn model_download_lock(model_id: &str, use_onnx: bool) -> Arc<tokio::sync::Mutex<()>> {
    let key = format!("{}:{}", model_id, if use_onnx { "onnx" } else { "pytorch" });
    let mut locks = MODEL_DOWNLOAD_LOCKS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

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

fn build_file_url(repo: &str, revision: &str, relpath: &str) -> String {
    let encoded = relpath
        .split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SEGMENT_ENCODE_SET).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "https://hf-mirror.com/{}/resolve/{}/{}",
        repo, revision, encoded
    )
}

struct ModelDownloadSpec {
    repo: &'static str,
    revision: &'static str,
    root: &'static str,
    subdir: Option<&'static str>,
    files: &'static [&'static str],
}

fn model_download_spec(model_id: &str, use_onnx: bool) -> Option<ModelDownloadSpec> {
    match (model_id, use_onnx) {
        ("chinese-clip", true) => Some(ModelDownloadSpec {
            repo: "Xenova/chinese-clip-vit-base-patch16",
            revision: "f26904860903e70e050b8f48255e5f48401816e9",
            root: "models-onnx",
            subdir: None,
            files: &[
                "vocab.txt",
                "config.json",
                "preprocessor_config.json",
                "onnx/model_q4.onnx",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
            ],
        }),
        ("chinese-clip", false) => Some(ModelDownloadSpec {
            repo: "OFA-Sys/chinese-clip-vit-base-patch16",
            revision: "36e679e65c2a2fead755ae21162091293ad37834",
            root: "models",
            subdir: None,
            files: &[
                "vocab.txt",
                "pytorch_model.bin",
                "config.json",
                "preprocessor_config.json",
            ],
        }),
        ("bge-small-zh", true) => Some(ModelDownloadSpec {
            repo: "Xenova/bge-small-zh-v1.5",
            revision: "75c43b069aac4d136ba6bc1122f995fedcfd2781",
            root: "models-onnx",
            subdir: Some("bge-small-zh-v1.5"),
            files: &[
                "config.json",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
                "onnx/model_quantized.onnx",
            ],
        }),
        ("bge-small-zh", false) => Some(ModelDownloadSpec {
            repo: "BAAI/bge-small-zh-v1.5",
            revision: "7999e1d3359715c523056ef9478215996d62a620",
            root: "models",
            subdir: Some("bge-small-zh-v1.5"),
            files: &[
                "config.json",
                "pytorch_model.bin",
                "tokenizer.json",
                "tokenizer_config.json",
                "vocab.txt",
                "special_tokens_map.json",
            ],
        }),
        ("minilm-l12", true) => Some(ModelDownloadSpec {
            repo: "Xenova/paraphrase-multilingual-MiniLM-L12-v2",
            revision: "2c4055b12046f11709e9df2c122e59ffbdc2f900",
            root: "models-onnx",
            subdir: Some("paraphrase-multilingual-MiniLM-L12-v2"),
            files: &[
                "config.json",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
                "onnx/model_quantized.onnx",
            ],
        }),
        ("minilm-l12", false) => Some(ModelDownloadSpec {
            repo: "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2",
            revision: "e8f8c211226b894fcb81acc59f3b34ba3efd5f42",
            root: "models",
            subdir: Some("paraphrase-multilingual-MiniLM-L12-v2"),
            files: &[
                "config.json",
                "pytorch_model.bin",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
                "sentencepiece.bpe.model",
            ],
        }),
        ("bge-reranker-v2-m3", _) => Some(ModelDownloadSpec {
            repo: "onnx-community/bge-reranker-v2-m3-ONNX",
            revision: "6f5ff65298512715a1e669753bc754d2bc8f367b",
            root: "models",
            subdir: Some("bge-reranker-v2-m3"),
            files: &[
                "config.json",
                "tokenizer.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
                "onnx/model_uint8.onnx",
            ],
        }),
        _ => None,
    }
}

fn file_exists_nonempty(path: &std::path::Path) -> bool {
    path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
}

fn model_files_complete(base: &Path, files: &[&str]) -> bool {
    files
        .iter()
        .all(|file| file_exists_nonempty(&base.join(file)))
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

fn chinese_clip_onnx_missing(base: &Path) -> Vec<String> {
    missing_files(
        base,
        &[
            "vocab.txt",
            "config.json",
            "preprocessor_config.json",
            "onnx/model_q4.onnx",
            "tokenizer.json",
        ],
    )
}

fn bge_onnx_missing(base: &Path) -> Vec<String> {
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
}

fn minilm_onnx_missing(base: &Path) -> Vec<String> {
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
}

fn chinese_clip_pytorch_missing(base: &Path) -> Vec<String> {
    missing_files(
        base,
        &[
            "vocab.txt",
            "pytorch_model.bin",
            "config.json",
            "preprocessor_config.json",
        ],
    )
}

fn bge_pytorch_missing(base: &Path) -> Vec<String> {
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
}

fn minilm_pytorch_missing(base: &Path) -> Vec<String> {
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
}

fn required_onnx_complete_with_fallback(onnx_models_dir: &Path, models_dir: &Path) -> bool {
    let clip_complete = chinese_clip_onnx_missing(onnx_models_dir).is_empty()
        || chinese_clip_onnx_missing(models_dir).is_empty();
    let bge_complete = bge_onnx_missing(&onnx_models_dir.join("bge-small-zh-v1.5")).is_empty()
        || bge_onnx_missing(&models_dir.join("bge-small-zh-v1.5")).is_empty();
    let minilm_complete =
        minilm_onnx_missing(&onnx_models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"))
            .is_empty()
            || minilm_onnx_missing(&models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"))
                .is_empty();

    clip_complete && bge_complete && minilm_complete
}

fn required_pytorch_complete(models_dir: &Path) -> bool {
    chinese_clip_pytorch_missing(models_dir).is_empty()
        && bge_pytorch_missing(&models_dir.join("bge-small-zh-v1.5")).is_empty()
        && minilm_pytorch_missing(&models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"))
            .is_empty()
}

#[derive(Debug, Clone)]
pub struct ResolvedRequiredModelPaths {
    pub clip_path: PathBuf,
    pub bge_path: PathBuf,
    pub minilm_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredModelRuntime {
    Onnx,
    Pytorch,
}

impl RequiredModelRuntime {
    pub fn use_onnx(self) -> bool {
        matches!(self, Self::Onnx)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Onnx => "onnx",
            Self::Pytorch => "pytorch",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRequiredModels {
    pub runtime: RequiredModelRuntime,
    pub used_pytorch_fallback: bool,
    pub paths: ResolvedRequiredModelPaths,
}

fn resolve_required_model_paths(
    runtime: RequiredModelRuntime,
    models_dir: &Path,
    onnx_models_dir: &Path,
) -> ResolvedRequiredModelPaths {
    match runtime {
        RequiredModelRuntime::Onnx => {
            let primary_bge = onnx_models_dir.join("bge-small-zh-v1.5");
            let legacy_bge = models_dir.join("bge-small-zh-v1.5");
            let primary_minilm = onnx_models_dir.join("paraphrase-multilingual-MiniLM-L12-v2");
            let legacy_minilm = models_dir.join("paraphrase-multilingual-MiniLM-L12-v2");

            ResolvedRequiredModelPaths {
                clip_path: if chinese_clip_onnx_missing(onnx_models_dir).is_empty() {
                    onnx_models_dir.to_path_buf()
                } else {
                    models_dir.to_path_buf()
                },
                bge_path: if bge_onnx_missing(&primary_bge).is_empty() {
                    primary_bge
                } else {
                    legacy_bge
                },
                minilm_path: if minilm_onnx_missing(&primary_minilm).is_empty() {
                    primary_minilm
                } else {
                    legacy_minilm
                },
            }
        }
        RequiredModelRuntime::Pytorch => ResolvedRequiredModelPaths {
            clip_path: models_dir.to_path_buf(),
            bge_path: models_dir.join("bge-small-zh-v1.5"),
            minilm_path: models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"),
        },
    }
}

pub fn resolve_required_model_runtime() -> Result<ResolvedRequiredModels, String> {
    let appdata_dir = file_in_local_appdata()
        .ok_or_else(|| "Could not determine local appdata directory.".to_string())?;
    let models_dir = appdata_dir.join("models");
    let onnx_models_dir = appdata_dir.join("models-onnx");
    let prefer_onnx = registry_config::get_bool("use_onnx").unwrap_or(true);

    if prefer_onnx {
        if required_onnx_complete_with_fallback(&onnx_models_dir, &models_dir) {
            let runtime = RequiredModelRuntime::Onnx;
            return Ok(ResolvedRequiredModels {
                runtime,
                used_pytorch_fallback: false,
                paths: resolve_required_model_paths(runtime, &models_dir, &onnx_models_dir),
            });
        }
        if required_pytorch_complete(&models_dir) {
            let runtime = RequiredModelRuntime::Pytorch;
            return Ok(ResolvedRequiredModels {
                runtime,
                used_pytorch_fallback: true,
                paths: resolve_required_model_paths(runtime, &models_dir, &onnx_models_dir),
            });
        }
    } else if required_pytorch_complete(&models_dir) {
        let runtime = RequiredModelRuntime::Pytorch;
        return Ok(ResolvedRequiredModels {
            runtime,
            used_pytorch_fallback: false,
            paths: resolve_required_model_paths(runtime, &models_dir, &onnx_models_dir),
        });
    }

    Err("Required model files are incomplete".to_string())
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
    revision: &str,
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

        let url = build_file_url(repo, revision, &relpath);

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

#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    window: tauri::Window,
    model_id: String,
) -> Result<String, String> {
    crate::commands::check_main_window(&window)?;
    let use_onnx = registry_config::get_bool("use_onnx").unwrap_or(true);
    let spec = model_download_spec(&model_id, use_onnx)
        .ok_or_else(|| format!("Unsupported model id: {}", model_id))?;
    let download_lock = model_download_lock(&model_id, use_onnx);
    let _download_guard = download_lock.lock().await;

    let mut download_path = file_in_local_appdata()
        .ok_or_else(|| "Could not determine resource directory.".to_string())?
        .join(spec.root);
    if let Some(sub) = spec.subdir {
        download_path = download_path.join(sub);
    }
    std::fs::create_dir_all(&download_path).map_err(|e| e.to_string())?;

    // Bootstrap downloads are idempotent. Existing complete models require no
    // network or authentication and should not break a partially restored setup.
    if model_files_complete(&download_path, spec.files) {
        return Ok(download_path.to_string_lossy().to_string());
    }

    if !registry_config::get_bool("network_enabled").unwrap_or(true) {
        return Err("Network features are disabled".to_string());
    }

    let aria2 = find_existing_file_in_resources(&app, "aria2c.exe").ok_or_else(|| {
        "aria2c executable not found in resources; The program installation may be incomplete."
            .to_string()
    })?;

    // prepare log file under log path
    let runtime = if use_onnx { "onnx" } else { "pytorch" };
    let log_path = env::temp_dir().join(format!(
        "carbonpaper_model_{}_{}.log",
        model_id.replace(|c: char| !c.is_ascii_alphanumeric(), "_"),
        runtime
    ));
    if log_path.exists() {
        std::fs::remove_file(&log_path).map_err(|e| e.to_string())?;
    }
    let f = File::create(&log_path).map_err(|e| e.to_string())?;
    let log_file = Arc::new(Mutex::new(f));

    let files = spec.files.iter().map(|file| (*file).to_string()).collect();

    match perform_download_hf_repo(
        app.clone(),
        aria2,
        download_path,
        spec.repo,
        spec.revision,
        files,
        8,
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
    let use_pytorch_fallback = use_onnx
        && !required_onnx_complete_with_fallback(&onnx_models_dir, &models_dir)
        && required_pytorch_complete(&models_dir);
    let active_runtime = if use_onnx && !use_pytorch_fallback {
        RequiredModelRuntime::Onnx
    } else {
        RequiredModelRuntime::Pytorch
    };
    let mut result = serde_json::Map::new();

    if active_runtime.use_onnx() {
        insert_status_with_fallback(
            &mut result,
            "chinese-clip",
            &onnx_models_dir,
            Some(&models_dir),
            true,
            chinese_clip_onnx_missing,
        );
        insert_status_with_fallback(
            &mut result,
            "bge-small-zh",
            &onnx_models_dir.join("bge-small-zh-v1.5"),
            Some(&models_dir.join("bge-small-zh-v1.5")),
            true,
            bge_onnx_missing,
        );
        insert_status_with_fallback(
            &mut result,
            "minilm-l12",
            &onnx_models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"),
            Some(&models_dir.join("paraphrase-multilingual-MiniLM-L12-v2")),
            true,
            minilm_onnx_missing,
        );
    } else {
        insert_status_with_fallback(
            &mut result,
            "chinese-clip",
            &models_dir,
            None,
            true,
            chinese_clip_pytorch_missing,
        );
        insert_status_with_fallback(
            &mut result,
            "bge-small-zh",
            &models_dir.join("bge-small-zh-v1.5"),
            None,
            true,
            bge_pytorch_missing,
        );
        insert_status_with_fallback(
            &mut result,
            "minilm-l12",
            &models_dir.join("paraphrase-multilingual-MiniLM-L12-v2"),
            None,
            true,
            minilm_pytorch_missing,
        );
    }

    result.insert(
        "_runtime".to_string(),
        json!({
            "complete": true,
            "required": false,
            "runtime": active_runtime.as_str(),
            "pytorch_fallback": use_pytorch_fallback,
        }),
    );

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
    use std::fs;

    fn touch(base: &Path, rel: &str) {
        let path = base.join(rel);
        fs::create_dir_all(path.parent().unwrap()).expect("create parent dir");
        fs::write(path, b"x").expect("write model marker");
    }

    fn write_complete_pytorch_models(models_dir: &Path) {
        for rel in [
            "vocab.txt",
            "pytorch_model.bin",
            "config.json",
            "preprocessor_config.json",
            "bge-small-zh-v1.5/config.json",
            "bge-small-zh-v1.5/pytorch_model.bin",
            "bge-small-zh-v1.5/tokenizer.json",
            "bge-small-zh-v1.5/tokenizer_config.json",
            "bge-small-zh-v1.5/vocab.txt",
            "bge-small-zh-v1.5/special_tokens_map.json",
            "paraphrase-multilingual-MiniLM-L12-v2/config.json",
            "paraphrase-multilingual-MiniLM-L12-v2/pytorch_model.bin",
            "paraphrase-multilingual-MiniLM-L12-v2/tokenizer.json",
            "paraphrase-multilingual-MiniLM-L12-v2/tokenizer_config.json",
            "paraphrase-multilingual-MiniLM-L12-v2/special_tokens_map.json",
            "paraphrase-multilingual-MiniLM-L12-v2/sentencepiece.bpe.model",
        ] {
            touch(models_dir, rel);
        }
    }

    fn write_complete_clip_onnx(base: &Path) {
        for rel in [
            "vocab.txt",
            "config.json",
            "preprocessor_config.json",
            "onnx/model_q4.onnx",
            "tokenizer.json",
        ] {
            touch(base, rel);
        }
    }

    fn write_complete_text_onnx(base: &Path) {
        for rel in [
            "config.json",
            "tokenizer.json",
            "tokenizer_config.json",
            "special_tokens_map.json",
            "onnx/model_quantized.onnx",
        ] {
            touch(base, rel);
        }
    }

    #[test]
    fn test_required_models_detect_complete_pytorch_without_onnx() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let models_dir = tmp.path().join("models");
        let onnx_models_dir = tmp.path().join("models-onnx");
        write_complete_pytorch_models(&models_dir);

        assert!(!required_onnx_complete_with_fallback(
            &onnx_models_dir,
            &models_dir
        ));
        assert!(required_pytorch_complete(&models_dir));
    }

    #[test]
    fn chinese_clip_onnx_requires_fast_tokenizer_contract() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        for rel in [
            "vocab.txt",
            "config.json",
            "preprocessor_config.json",
            "onnx/model_q4.onnx",
        ] {
            touch(tmp.path(), rel);
        }
        assert_eq!(
            chinese_clip_onnx_missing(tmp.path()),
            vec!["tokenizer.json".to_string()]
        );
    }

    #[test]
    fn test_onnx_runtime_paths_skip_incomplete_primary_model_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let models_dir = tmp.path().join("models");
        let onnx_models_dir = tmp.path().join("models-onnx");

        write_complete_clip_onnx(&onnx_models_dir);

        let primary_bge = onnx_models_dir.join("bge-small-zh-v1.5");
        let legacy_bge = models_dir.join("bge-small-zh-v1.5");
        touch(&primary_bge, "onnx/model_quantized.onnx");
        write_complete_text_onnx(&legacy_bge);

        let primary_minilm = onnx_models_dir.join("paraphrase-multilingual-MiniLM-L12-v2");
        write_complete_text_onnx(&primary_minilm);

        assert!(required_onnx_complete_with_fallback(
            &onnx_models_dir,
            &models_dir
        ));

        let paths =
            resolve_required_model_paths(RequiredModelRuntime::Onnx, &models_dir, &onnx_models_dir);
        assert_eq!(paths.clip_path, onnx_models_dir);
        assert_eq!(paths.bge_path, legacy_bge);
        assert_eq!(paths.minilm_path, primary_minilm);
    }

    #[test]
    fn test_model_catalog_rejects_unknown_ids_and_pins_revisions() {
        assert!(model_download_spec("unknown", true).is_none());
        for model_id in [
            "chinese-clip",
            "bge-small-zh",
            "minilm-l12",
            "bge-reranker-v2-m3",
        ] {
            let spec = model_download_spec(model_id, true).expect("known model");
            assert_eq!(spec.revision.len(), 40);
            assert!(!spec.files.is_empty());
            assert!(spec.files.iter().all(|file| !file.contains("..")));
        }
    }

    #[test]
    fn model_completeness_requires_every_file_to_be_nonempty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path();
        std::fs::write(base.join("a.bin"), b"ready").expect("write a");
        std::fs::write(base.join("b.bin"), b"").expect("write b");

        assert!(!model_files_complete(base, &["a.bin", "b.bin"]));

        std::fs::write(base.join("b.bin"), b"ready").expect("rewrite b");
        assert!(model_files_complete(base, &["a.bin", "b.bin"]));
    }
}
