//! M2.5 step 3 — MiniLM Rust semantic shadow queries.
//!
//! Two ways to generate samples, both comparing the authoritative Chroma/Python
//! `nl_cluster_query` (MiniLM over `task_vectors`) against the equivalent Rust
//! exact-scan retrieval over the migrated derived store:
//!
//! 1. Passive: when `semantic_runtime = rust_shadow`, every live NL cluster
//!    query is shadowed in the background (`spawn_shadow_sample`).
//! 2. Active: `run_semantic_shadow_probe` drives a built-in (or caller-supplied)
//!    query corpus on demand, so parity can be measured without depending on the
//!    user manually searching the demo NL surface.
//!
//! Neither path ever changes user-visible search behavior; Python/Chroma stays
//! authoritative throughout the shadow phase. Samples feed the telemetry-free
//! `get_semantic_shadow_report` diagnostic used to prove the release gate before
//! any cutover.

use crate::credential_manager::CredentialManagerState;
use crate::ml_protocol::MlSemanticModel;
use crate::monitor::{authenticated_monitor_command, MonitorState};
use crate::registry_config;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{
    DerivedEmbeddingRecord, DerivedIndexKind, ScoredSubject, SemanticDocEncoderRun,
    SemanticShadowSample, StorageState,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// User-initiated search is foreground work: deadline-bound, not idle-gated.
const SHADOW_EMBED_TIMEOUT: Duration = Duration::from_secs(5);
/// Overlap is reported over the top-N (Rust-vs-Python) for a stable headline
/// metric that matches the release gate's "top-10 overlap".
const OVERLAP_WINDOW: usize = 10;
/// Candidates fetched per probe query. Matches the frontend NL default so the
/// probe exercises the same over-fetch/threshold path as a real query.
const PROBE_N_RESULTS: u32 = 30;
const MAX_PROBE_QUERIES: usize = 256;
const MAX_QUERY_CHARS: usize = 512;

/// Default document-encoder sample size when the caller does not specify one.
const DEFAULT_DOC_SAMPLE_SIZE: u32 = 256;
/// Hard cap on a single document-encoder probe run.
const MAX_DOC_SAMPLE_SIZE: u32 = 4_096;
/// Worker round-trip batch size for document re-encoding. The semantic protocol
/// bounds a single embed request to 32 texts.
const DOC_ENCODE_BATCH: usize = 32;
/// Document re-encoding is an explicit batch diagnostic, not a foreground search,
/// so it gets a larger per-batch deadline than a single interactive query.
const DOC_EMBED_TIMEOUT: Duration = Duration::from_secs(30);

const RUNTIME_BACKENDS: &[&str] = &["python", "rust_shadow", "rust"];
const INDEX_BACKENDS: &[&str] = &["chroma", "dual", "rust"];

/// Built-in, non-sensitive probe corpus. Generic screenshot-content queries in
/// Chinese and English so the harness works on any user's data without needing
/// them to type anything sensitive. Deliberately broad (dozens of everyday
/// domains) so p05/p95 over the corpus are not dominated by a handful of
/// queries; `sample_count` in the report makes the effective N explicit.
const DEFAULT_PROBE_QUERIES: &[&str] = &[
    "代码 编辑器 开发",
    "会议 日程 安排",
    "浏览器 网页 搜索",
    "聊天 消息 对话",
    "文档 报告 写作",
    "视频 播放 观看",
    "购物 订单 支付",
    "设置 配置 选项",
    "邮件 收件箱",
    "终端 命令行",
    "代码审查 拉取请求",
    "版本控制 提交记录",
    "错误 报错 堆栈跟踪",
    "数据库 查询 表",
    "接口 文档 调试",
    "日志 监控 告警",
    "表格 数据 统计",
    "幻灯片 演示 汇报",
    "笔记 待办 清单",
    "日历 提醒 事项",
    "地图 导航 路线",
    "天气 预报 温度",
    "音乐 歌单 播放器",
    "照片 相册 图片编辑",
    "翻译 词典 语言",
    "新闻 资讯 头条",
    "社交 动态 评论",
    "论坛 帖子 回复",
    "云盘 文件 同步",
    "下载 上传 进度",
    "安装 更新 卸载",
    "登录 注册 密码",
    "个人资料 账户 头像",
    "通知 消息中心",
    "搜索结果 筛选 排序",
    "支付 账单 发票",
    "银行 转账 余额",
    "股票 基金 行情",
    "机票 酒店 预订",
    "外卖 餐厅 菜单",
    "快递 物流 追踪",
    "视频会议 屏幕共享",
    "白板 思维导图",
    "设计 原型 界面",
    "表单 提交 校验",
    "游戏 关卡 成就",
    "直播 弹幕 打赏",
    "健身 步数 运动记录",
    "医疗 挂号 报告单",
    "课程 学习 作业",
    "招聘 简历 面试",
    "合同 协议 条款",
    "报销 审批 流程",
    "客服 工单 反馈",
    "invoice and billing",
    "meeting notes",
    "pull request review",
    "error stack trace",
    "database query editor",
    "api documentation",
    "spreadsheet data table",
    "presentation slides",
    "todo list and tasks",
    "calendar reminder",
    "map directions route",
    "weather forecast",
    "music playlist player",
    "photo gallery editor",
    "email inbox thread",
    "terminal command output",
    "settings and preferences",
    "login and password reset",
    "shopping cart checkout",
    "flight and hotel booking",
    "file upload progress",
    "video call screen share",
    "chat conversation history",
    "dashboard charts metrics",
    "search filters and sorting",
    "notification center",
    "source control commit log",
    "unit test results",
    "cloud storage sync",
    "job application resume",
    "bank transfer balance",
    "stock market quotes",
    "food delivery order",
    "package tracking status",
    "online course lecture",
    "design prototype mockup",
];

/// Single-flight guard so a double-click cannot launch two overlapping probes.
static PROBE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Selected semantic inference backend. Invalid/unset values fall back to
/// `python` observably (the shadow simply does not run).
pub fn semantic_runtime_backend() -> String {
    normalize_enum(
        registry_config::get_string("semantic_runtime"),
        RUNTIME_BACKENDS,
        "python",
    )
}

/// Selected semantic index ownership backend. Reserved for the step-4 cutover;
/// read here so the setting round-trips through the diagnostics UI today.
pub fn semantic_index_backend() -> String {
    normalize_enum(
        registry_config::get_string("semantic_index"),
        INDEX_BACKENDS,
        "chroma",
    )
}

fn normalize_enum(value: Option<String>, allowed: &[&str], default: &str) -> String {
    match value {
        Some(value) if allowed.contains(&value.as_str()) => value,
        _ => default.to_string(),
    }
}

/// Run one synchronous storage closure off the async runtime.
///
/// Every storage call in this module takes the process-wide, non-reentrant
/// database mutex, and several are O(N) scans or per-row CNG decryptions.
/// Awaiting one on a tokio worker parks that worker for the whole lock wait —
/// and the probe issues hundreds of them back-to-back while live capture holds
/// the same lock, which starves the runtime the reverse-IPC bridge shares.
async fn blocking_storage<T, F>(storage: &Arc<StorageState>, work: F) -> Result<T, String>
where
    F: FnOnce(&StorageState) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let storage = storage.clone();
    tokio::task::spawn_blocking(move || work(&storage))
        .await
        .map_err(|error| format!("storage task failed: {error}"))?
}

/// Persist one sample off the async runtime and hand it back to the caller.
async fn record_sample(
    storage: &Arc<StorageState>,
    sample: SemanticShadowSample,
) -> Result<SemanticShadowSample, String> {
    let recorded = sample.clone();
    blocking_storage(storage, move |storage| {
        storage.record_semantic_shadow_sample(&sample)
    })
    .await?;
    Ok(recorded)
}

/// Query-visible Rust vector count, or 0 when the store cannot answer.
async fn rust_visible_count(storage: &Arc<StorageState>) -> u64 {
    blocking_storage(storage, |storage| {
        Ok(storage
            .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap_or(0))
    })
    .await
    .unwrap_or(0)
}

/// Spawn a best-effort passive shadow comparison for one already-served NL
/// query. Returns immediately; the comparison runs on a background task and
/// never affects the caller's response.
pub fn spawn_shadow_sample(
    app: AppHandle,
    query: String,
    enable_rerank: bool,
    python_response: serde_json::Value,
    python_ms: f64,
) {
    // The Rust path is bi-encoder retrieval; reranker parity is M2.5 step 5.
    // Comparing against a reranked Python response would measure the reranker,
    // not retrieval, so skip those queries entirely.
    if enable_rerank {
        return;
    }
    if semantic_runtime_backend() != "rust_shadow" {
        return;
    }
    // A migration is rewriting the derived store; a shadow read would race it.
    if crate::maintenance::is_active() {
        return;
    }
    if query.trim().is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let storage = app.state::<Arc<StorageState>>().inner().clone();
        let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
        let chroma_scope = resolve_chroma_scope(&app, &storage).await;
        if let Err(error) = evaluate_and_record(
            &app,
            &storage,
            &semantic,
            query.trim(),
            &python_response,
            python_ms,
            chroma_scope,
            false,
        )
        .await
        {
            tracing::debug!("[SEMANTIC:SHADOW] passive sample skipped: {error}");
        }
    });
}

/// Compare one query's Python ranking against the Rust exact-scan retrieval and
/// record a sample. Always records: when the comparison cannot be made it
/// records a `note` sample instead, which the aggregate excludes rather than
/// scoring as a zero-overlap divergence. Returns the recorded sample.
async fn evaluate_and_record(
    app: &AppHandle,
    storage: &Arc<StorageState>,
    semantic: &Arc<SemanticRuntimeState>,
    query: &str,
    python_response: &serde_json::Value,
    python_ms: f64,
    chroma_scope: Option<u64>,
    cold_start: bool,
) -> Result<SemanticShadowSample, String> {
    let query_hash = hash_query(query);
    let python_results = parse_python_results(python_response);
    let rust_visible = rust_visible_count(storage).await;

    // Python answered `success` with no ranking at all. `query_by_text` returns
    // an empty list for an absent collection, a zero-count collection, and any
    // internal Chroma exception it swallows — none of which is a measurement.
    // Comparing against it would yield overlap=0 / top1=false, identical to a
    // real divergence, so record it as a note sample and stop. Otherwise one
    // click on the probe before the MiniLM index exists writes ~90 phantom
    // zero-overlap rows that hold the gate numbers down for several runs.
    if python_results.is_empty() {
        let sample = note_sample(
            &query_hash,
            0,
            python_ms,
            0.0,
            rust_visible,
            chroma_scope,
            cold_start,
            "python_empty_results: Chroma returned no ranking to compare against",
        );
        return record_sample(storage, sample).await;
    }

    // Embed the query with the Rust MiniLM runtime (CPU-only, foreground).
    let embed_started = Instant::now();
    let embedding = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::MinilmL12,
            vec![query.to_string()],
            SHADOW_EMBED_TIMEOUT,
            false,
        )
        .await;
    let embed_ms = elapsed_ms(embed_started);
    let embedding = match embedding {
        Ok(embedding) => embedding,
        Err(error) => {
            let sample = note_sample(
                &query_hash,
                python_results.len(),
                python_ms,
                embed_ms,
                rust_visible,
                chroma_scope,
                cold_start,
                &format!("embed_failed: {error}"),
            );
            return record_sample(storage, sample).await;
        }
    };
    let query_vec = embedding
        .vectors
        .into_iter()
        .next()
        .ok_or("semantic worker returned no embedding")?;

    // Exact-scan retrieval over the query-visible derived store, followed by the
    // per-id classification lookups. Both are synchronous SQLite work under the
    // global storage mutex, so they share one blocking thread instead of hopping
    // back onto the async runtime between them.
    let k = python_results.len().max(1);
    let python_count = python_results.len();
    let scan_storage = storage.clone();
    let scan_vec = query_vec;
    let scan_results = python_results;
    let (rust_count, scan_ms, metrics) = tokio::task::spawn_blocking(move || {
        let scan_started = Instant::now();
        let rust_top = scan_storage.semantic_text_topk(&scan_vec, k)?;
        let scan_ms = elapsed_ms(scan_started);
        let metrics = compute_metrics(&scan_results, &rust_top, &scan_vec, &scan_storage);
        Ok::<_, String>((rust_top.len() as u32, scan_ms, metrics))
    })
    .await
    .map_err(|error| format!("shadow scan task failed: {error}"))??;

    // A classification lookup that errored leaves this row's divergence
    // breakdown incomplete, so mark it and let the aggregate skip it.
    let note = (metrics.lookup_failed > 0).then(|| {
        format!(
            "classification_lookup_failed: {} of {} top-K lookups errored",
            metrics.lookup_failed,
            OVERLAP_WINDOW.min(python_count)
        )
    });

    let sample = SemanticShadowSample {
        query_hash,
        k: k as u32,
        python_count: python_count as u32,
        rust_count,
        shared: metrics.shared,
        top1_agreement: metrics.top1_agreement,
        overlap_k: metrics.overlap,
        max_abs_err: metrics.max_abs_err,
        mean_abs_err: metrics.mean_abs_err,
        embed_ms,
        scan_ms,
        python_ms,
        rust_visible,
        chroma_scope,
        only_in_chroma: metrics.only_in_chroma,
        in_both_diff_rank: metrics.in_both_diff_rank,
        only_in_rust: metrics.only_in_rust,
        cold_start,
        note,
    };
    record_sample(storage, sample).await
}

struct ShadowMetrics {
    shared: u32,
    top1_agreement: bool,
    overlap: f32,
    max_abs_err: Option<f32>,
    mean_abs_err: Option<f32>,
    only_in_chroma: u32,
    in_both_diff_rank: u32,
    only_in_rust: u32,
    /// Top-K ids whose local lookup returned an error instead of a definite
    /// present/absent answer. Deliberately never folded into `only_in_chroma`.
    lookup_failed: u32,
}

fn compute_metrics(
    python_results: &[(i64, Option<f32>)],
    rust_top: &[ScoredSubject],
    query_vec: &[f32],
    storage: &StorageState,
) -> ShadowMetrics {
    let window = OVERLAP_WINDOW.min(python_results.len()).max(1);
    let python_ids: Vec<i64> = python_results.iter().map(|(id, _)| *id).collect();
    let rust_ids: Vec<i64> = rust_top
        .iter()
        .filter_map(|scored| scored.subject_key.parse::<i64>().ok())
        .collect();

    let python_window: HashSet<i64> = python_ids.iter().take(window).copied().collect();
    let rust_window: HashSet<i64> = rust_ids.iter().take(window).copied().collect();
    let shared = python_window.intersection(&rust_window).count();
    let overlap = shared as f32 / window as f32;
    let top1_agreement = matches!(
        (python_ids.first(), rust_ids.first()),
        (Some(a), Some(b)) if a == b
    );

    // One local lookup per Python-top id does double duty: it classifies the
    // disagreement AND supplies the stored vector for the cosine-error check.
    // Cosine error isolates the query-encoder difference (stored doc vectors
    // are the migrated Python copies). The classification is the decisive
    // signal: a Python-top id absent from the Rust store means Rust is missing
    // a document Chroma serves (real divergence); a Python-top id present in
    // Rust but ranked out is ranking/approximation, not missing data.
    let mut abs_errors: Vec<f32> = Vec::new();
    let mut only_in_chroma = 0u32;
    let mut in_both_diff_rank = 0u32;
    let mut lookup_failed = 0u32;
    for (id, python_sim) in python_results.iter().take(window) {
        let stored = match storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, &id.to_string())
        {
            Ok(stored) => stored,
            Err(error) => {
                // A read error means "unknown", not "absent". Folding it into
                // `only_in_chroma` would report a transient storage fault as the
                // one metric that means "Rust is missing data Chroma serves" —
                // the most alarming number on the cutover gate.
                tracing::debug!(
                    "[SEMANTIC:SHADOW] top-K classification lookup failed for {id}: {error}"
                );
                lookup_failed += 1;
                continue;
            }
        };
        if !rust_window.contains(id) {
            match &stored {
                Some(_) => in_both_diff_rank += 1,
                None => only_in_chroma += 1,
            }
        }
        if let (Some(python_sim), Some(record)) = (python_sim.as_ref(), stored.as_ref()) {
            if record.vector.len() == query_vec.len() {
                let rust_sim: f32 = query_vec
                    .iter()
                    .zip(&record.vector)
                    .map(|(x, y)| x * y)
                    .sum();
                abs_errors.push((rust_sim - *python_sim).abs());
            }
        }
    }
    let only_in_rust = rust_ids
        .iter()
        .take(window)
        .filter(|id| !python_window.contains(id))
        .count() as u32;

    let max_abs_err = abs_errors
        .iter()
        .copied()
        .fold(None, |acc: Option<f32>, value| {
            Some(acc.map_or(value, |current| current.max(value)))
        });
    let mean_abs_err = if abs_errors.is_empty() {
        None
    } else {
        Some(abs_errors.iter().sum::<f32>() / abs_errors.len() as f32)
    };

    ShadowMetrics {
        shared: shared as u32,
        top1_agreement,
        overlap,
        max_abs_err,
        mean_abs_err,
        only_in_chroma,
        in_both_diff_rank,
        only_in_rust,
        lookup_failed,
    }
}

/// The comparison denominator per the roadmap gate is the set of valid,
/// mappable vectors in the migrated Chroma snapshot. This is only a fallback
/// for when the live count is unavailable — it is a historical migration-time
/// value, not the current hot-layer size.
async fn mappable_chroma_scope(storage: &Arc<StorageState>) -> Option<u64> {
    blocking_storage(storage, |storage| {
        Ok(storage.get_latest_minilm_migration_run().ok().flatten())
    })
    .await
    .ok()
    .flatten()
    .map(|run| run.migrated + run.legacy_unverified)
}

/// Live count of the Chroma `task_vectors` hot layer, so coverage compares
/// live-Rust against live-Chroma. Best-effort: `None` if the monitor is down.
async fn live_chroma_task_vectors_count(app: &AppHandle) -> Option<u64> {
    let credential = app.state::<Arc<CredentialManagerState>>();
    let monitor = app.state::<MonitorState>();
    let response = authenticated_monitor_command(
        &credential,
        &monitor,
        serde_json::json!({ "command": "get_task_vectors_count" }),
    )
    .await
    .ok()?;
    if response.get("status").and_then(|value| value.as_str()) != Some("success") {
        return None;
    }
    response.get("count").and_then(serde_json::Value::as_u64)
}

/// Resolve the coverage denominator: the live Chroma hot-layer count when the
/// monitor answers, otherwise the historical migration-snapshot fallback.
async fn resolve_chroma_scope(app: &AppHandle, storage: &Arc<StorageState>) -> Option<u64> {
    match live_chroma_task_vectors_count(app).await {
        Some(count) => Some(count),
        None => mappable_chroma_scope(storage).await,
    }
}

fn parse_python_results(response: &serde_json::Value) -> Vec<(i64, Option<f32>)> {
    let Some(results) = response.get("results").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let mut parsed = Vec::with_capacity(results.len());
    let mut seen = HashMap::new();
    for entry in results {
        let Some(id) = entry
            .get("screenshot_id")
            .and_then(serde_json::Value::as_i64)
            .filter(|id| *id > 0)
        else {
            continue;
        };
        // Chroma can only return each id once, but guard against duplicates so
        // the overlap denominator stays honest.
        if seen.insert(id, ()).is_some() {
            continue;
        }
        let similarity = entry
            .get("similarity")
            .and_then(serde_json::Value::as_f64)
            .map(|value| value as f32);
        parsed.push((id, similarity));
    }
    parsed
}

#[allow(clippy::too_many_arguments)]
fn note_sample(
    query_hash: &str,
    python_count: usize,
    python_ms: f64,
    embed_ms: f64,
    rust_visible: u64,
    chroma_scope: Option<u64>,
    cold_start: bool,
    note: &str,
) -> SemanticShadowSample {
    SemanticShadowSample {
        query_hash: query_hash.to_string(),
        k: 0,
        python_count: python_count as u32,
        rust_count: 0,
        shared: 0,
        top1_agreement: false,
        overlap_k: 0.0,
        max_abs_err: None,
        mean_abs_err: None,
        embed_ms,
        scan_ms: 0.0,
        python_ms,
        rust_visible,
        chroma_scope,
        only_in_chroma: 0,
        in_both_diff_rank: 0,
        only_in_rust: 0,
        cold_start,
        note: Some(note.to_string()),
    }
}

fn hash_query(query: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(query.as_bytes());
    format!("{:x}", digest.finalize())
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Authoritatively query the Python MiniLM NL surface for the probe. Reranking
/// is off so the comparison stays at the bi-encoder retrieval layer.
async fn probe_python_query(
    app: &AppHandle,
    query: &str,
) -> Result<(serde_json::Value, f64), String> {
    let credential = app.state::<Arc<CredentialManagerState>>();
    let monitor = app.state::<MonitorState>();
    let started = Instant::now();
    let response = authenticated_monitor_command(
        &credential,
        &monitor,
        serde_json::json!({
            "command": "nl_cluster_query",
            "query": query,
            "n_results": PROBE_N_RESULTS,
            "enable_rerank": false,
            "rerank_variant": "q4f16",
        }),
    )
    .await?;
    let python_ms = elapsed_ms(started);
    match response.get("status").and_then(|value| value.as_str()) {
        Some("success") => Ok((response, python_ms)),
        _ => Err(response
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("nl_cluster_query did not return success")
            .to_string()),
    }
}

fn resolve_probe_queries(queries: Option<Vec<String>>) -> Vec<String> {
    let raw = match queries {
        Some(queries) if !queries.is_empty() => queries,
        _ => DEFAULT_PROBE_QUERIES
            .iter()
            .map(|query| query.to_string())
            .collect(),
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for query in raw {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            continue;
        }
        let capped: String = trimmed.chars().take(MAX_QUERY_CHARS).collect();
        if seen.insert(capped.clone()) {
            out.push(capped);
            if out.len() >= MAX_PROBE_QUERIES {
                break;
            }
        }
    }
    out
}

async fn run_probe_inner(
    app: AppHandle,
    queries: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    let queries = resolve_probe_queries(queries);
    if queries.is_empty() {
        return Err("No valid probe queries were provided".to_string());
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    // The live hot-layer size is stable across one probe run; fetch it once so
    // coverage is live-Rust vs live-Chroma without an IPC per query.
    let chroma_scope = resolve_chroma_scope(&app, &storage).await;

    let mut recorded = 0u32;
    let mut full_samples = 0u32;
    let mut note_samples = 0u32;
    let mut python_failures = 0u32;

    for (index, query) in queries.iter().enumerate() {
        // The first probe sample bears the ONNX session-init cost; flag it so
        // the report can exclude it from steady-state latency percentiles.
        let cold_start = index == 0;
        match probe_python_query(&app, query).await {
            Ok((response, python_ms)) => {
                match evaluate_and_record(
                    &app,
                    &storage,
                    &semantic,
                    query,
                    &response,
                    python_ms,
                    chroma_scope,
                    cold_start,
                )
                .await
                {
                    Ok(sample) => {
                        recorded += 1;
                        if sample.note.is_some() {
                            note_samples += 1;
                        } else {
                            full_samples += 1;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            "[SEMANTIC:SHADOW] probe evaluate failed for one query: {error}"
                        );
                    }
                }
            }
            Err(error) => {
                python_failures += 1;
                let sample = note_sample(
                    &hash_query(query),
                    0,
                    0.0,
                    0.0,
                    rust_visible_count(&storage).await,
                    chroma_scope,
                    cold_start,
                    &format!("python_query_failed: {error}"),
                );
                if record_sample(&storage, sample).await.is_ok() {
                    recorded += 1;
                    note_samples += 1;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "queries": queries.len(),
        "recorded": recorded,
        "full_samples": full_samples,
        "note_samples": note_samples,
        "python_failures": python_failures,
        "report": report_value(&storage).await?,
    }))
}

async fn report_value(storage: &Arc<StorageState>) -> Result<serde_json::Value, String> {
    let report = blocking_storage(storage, |storage| {
        storage.get_semantic_shadow_report(500)
    })
    .await?;
    let mut value = serde_json::to_value(&report).map_err(|error| error.to_string())?;
    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "semantic_runtime".to_string(),
            serde_json::Value::String(semantic_runtime_backend()),
        );
        map.insert(
            "semantic_index".to_string(),
            serde_json::Value::String(semantic_index_backend()),
        );
    }
    Ok(value)
}

/// Async on purpose: a non-async Tauri command runs on the main thread, and this
/// one scans up to 500 sample rows plus a doc-run lookup under the global
/// storage mutex. The settings page calls it on every mount, so a sync version
/// froze the UI thread whenever a capture write held that lock.
#[tauri::command]
pub async fn get_semantic_shadow_report(
    storage: tauri::State<'_, Arc<StorageState>>,
    _window: Option<u32>,
) -> Result<serde_json::Value, String> {
    let storage = storage.inner().clone();
    report_value(&storage).await
}

/// Run the active shadow probe over a built-in (or caller-supplied) query
/// corpus. This is an explicit, foreground diagnostic: it runs regardless of the
/// `semantic_runtime` setting so parity can be measured before enabling the
/// passive shadow, but it is still rejected during maintenance.
#[tauri::command]
pub async fn run_semantic_shadow_probe(
    app: AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    queries: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    if crate::maintenance::is_active() {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    }
    if PROBE_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A semantic shadow probe is already running".to_string());
    }
    let result = run_probe_inner(app, queries).await;
    PROBE_RUNNING.store(false, Ordering::SeqCst);
    result
}

/// Single-flight guard for the document-encoder probe.
static DOC_PROBE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Run the active document-encoder parity probe. This fills the half of the
/// chain the per-query shadow cannot: it re-encodes a sample of the migrated
/// documents with the Rust MiniLM runtime (`build_minilm_task_text` + Rust
/// encoder) and compares each against the stored Python document vector. The
/// per-query samples, by contrast, reuse the stored Python doc vectors and so
/// only exercise the query encoder. Foreground, auth-gated, rejected during
/// maintenance.
#[tauri::command]
pub async fn run_semantic_doc_encoder_probe(
    app: AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    sample_size: Option<u32>,
) -> Result<serde_json::Value, String> {
    crate::commands::check_auth_required(&credential_state)?;
    if crate::maintenance::is_active() {
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    }
    if DOC_PROBE_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A document-encoder probe is already running".to_string());
    }
    let result = run_doc_encoder_probe_inner(app, sample_size).await;
    DOC_PROBE_RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_doc_encoder_probe_inner(
    app: AppHandle,
    sample_size: Option<u32>,
) -> Result<serde_json::Value, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let requested = sample_size
        .unwrap_or(DEFAULT_DOC_SAMPLE_SIZE)
        .clamp(1, MAX_DOC_SAMPLE_SIZE) as usize;

    // Draw an unbiased sample of the migrated (query-visible) MiniLM vectors off
    // the async runtime — the reservoir scan is synchronous SQLite work.
    let sample_storage = storage.clone();
    let sampled =
        tokio::task::spawn_blocking(move || sample_visible_embeddings(&sample_storage, requested))
            .await
            .map_err(|error| format!("doc-encoder sampling task failed: {error}"))??;

    if sampled.is_empty() {
        let run = SemanticDocEncoderRun {
            requested: requested as u64,
            note: Some("no query-visible MiniLM vectors to compare".to_string()),
            ..Default::default()
        };
        blocking_storage(&storage, move |storage| storage.record_doc_encoder_run(&run)).await?;
        return Ok(serde_json::json!({
            "doc_encoder": blocking_storage(&storage, |storage| storage
                .get_latest_doc_encoder_run())
            .await?,
            "report": report_value(&storage).await?,
        }));
    }

    let drawn = sampled.len() as u64;
    let mut cosines: Vec<f32> = Vec::with_capacity(sampled.len());
    let mut source_changed = 0u64;
    let mut missing_text = 0u64;
    let mut cold_start_ms: Option<f64> = None;
    let mut steady_ms_total = 0f64;
    let mut steady_docs = 0u64;
    let mut first_batch = true;

    for chunk in sampled.chunks(DOC_ENCODE_BATCH) {
        let ids: Vec<i64> = chunk
            .iter()
            .filter_map(|record| record.job.subject_key.parse::<i64>().ok())
            .collect();
        // Reconstruct the exact migration-contract task text for this batch.
        // Per batch this reads SQLite, unwraps a CNG-protected key and decrypts
        // every row, so it belongs off the async runtime like the scan does.
        let sources_ids = ids.clone();
        let sources =
            blocking_storage(&storage, move |storage| {
                crate::minilm_migration::minilm_sources_for_ids(storage, &sources_ids)
            })
            .await?;

        let mut batch_texts: Vec<String> = Vec::with_capacity(chunk.len());
        let mut batch_python: Vec<Vec<f32>> = Vec::with_capacity(chunk.len());
        for record in chunk {
            let Ok(id) = record.job.subject_key.parse::<i64>() else {
                missing_text += 1;
                continue;
            };
            match sources.get(&id) {
                Some((text, fingerprint)) if !text.is_empty() => {
                    // Only compare when the current SQLite text still hashes to
                    // the stored fingerprint; otherwise the stored Python vector
                    // and a fresh Rust encode would describe different text.
                    if *fingerprint == record.job.source_fingerprint {
                        batch_texts.push(text.clone());
                        batch_python.push(record.vector.clone());
                    } else {
                        source_changed += 1;
                    }
                }
                _ => missing_text += 1,
            }
        }
        if batch_texts.is_empty() {
            continue;
        }

        let batch_len = batch_texts.len();
        let started = Instant::now();
        let embedded = semantic
            .embed_text(
                app.clone(),
                MlSemanticModel::MinilmL12,
                batch_texts,
                DOC_EMBED_TIMEOUT,
                false,
            )
            .await?;
        let batch_ms = elapsed_ms(started);

        for (rust_vec, python_vec) in embedded.vectors.iter().zip(batch_python.iter()) {
            if let Some(similarity) = cosine(rust_vec, python_vec) {
                cosines.push(similarity);
            }
        }
        // The first batch bears the ONNX session-init cost; report it as the
        // cold figure and amortize only the remaining batches into steady state.
        if first_batch {
            cold_start_ms = Some(batch_ms);
            first_batch = false;
        } else {
            steady_ms_total += batch_ms;
            steady_docs += batch_len as u64;
        }
    }

    cosines.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let doc_sample_count = cosines.len() as u64;
    let cos_min = cosines.first().copied();
    let cos_max = cosines.last().copied();
    let cos_mean = if cosines.is_empty() {
        None
    } else {
        Some(cosines.iter().sum::<f32>() / cosines.len() as f32)
    };
    let steady_ms_per_doc = if steady_docs > 0 {
        Some(steady_ms_total / steady_docs as f64)
    } else {
        None
    };
    let note = (doc_sample_count == 0)
        .then(|| "all sampled documents were skipped (source changed or empty)".to_string());

    let run = SemanticDocEncoderRun {
        created_at: String::new(),
        doc_sample_count,
        requested: drawn,
        source_changed,
        missing_text,
        cos_min,
        cos_p05: sorted_percentile_f32(&cosines, 0.05),
        cos_p50: sorted_percentile_f32(&cosines, 0.50),
        cos_mean,
        cos_max,
        max_abs_err: cos_min.map(|value| 1.0 - value),
        cold_start_ms,
        steady_ms_per_doc,
        note,
    };
    blocking_storage(&storage, move |storage| storage.record_doc_encoder_run(&run)).await?;

    Ok(serde_json::json!({
        "doc_encoder": blocking_storage(&storage, |storage| storage.get_latest_doc_encoder_run())
            .await?,
        "report": report_value(&storage).await?,
    }))
}

/// Reservoir-sample (Algorithm R) up to `sample_size` query-visible semantic
/// embeddings so a probe draws an unbiased spread across the whole migrated hot
/// layer instead of just the lowest screenshot ids.
fn sample_visible_embeddings(
    storage: &StorageState,
    sample_size: usize,
) -> Result<Vec<DerivedEmbeddingRecord>, String> {
    use rand::Rng;
    const PAGE: u32 = 2_000;
    let mut reservoir: Vec<DerivedEmbeddingRecord> = Vec::with_capacity(sample_size.min(4_096));
    let mut rng = rand::thread_rng();
    let mut seen = 0usize;
    let mut offset = 0u32;
    loop {
        let page =
            storage.list_query_visible_embeddings(DerivedIndexKind::SemanticText, offset, PAGE)?;
        let page_len = page.len();
        for record in page {
            if reservoir.len() < sample_size {
                reservoir.push(record);
            } else {
                let index = rng.gen_range(0..=seen);
                if index < sample_size {
                    reservoir[index] = record;
                }
            }
            seen += 1;
        }
        if (page_len as u32) < PAGE {
            break;
        }
        offset = offset
            .checked_add(PAGE)
            .ok_or("doc-encoder sampling offset overflow")?;
    }
    Ok(reservoir)
}

/// True cosine similarity with f64 accumulation. Both MiniLM outputs are
/// L2-normalized, so this equals their dot product, but normalizing here keeps
/// the metric unambiguous regardless of stored-vector scale.
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let mut dot = 0f64;
    let mut norm_a = 0f64;
    let mut norm_b = 0f64;
    for (x, y) in a.iter().zip(b) {
        dot += f64::from(*x) * f64::from(*y);
        norm_a += f64::from(*x) * f64::from(*x);
        norm_b += f64::from(*y) * f64::from(*y);
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= f64::EPSILON {
        return None;
    }
    Some((dot / denom).clamp(-1.0, 1.0) as f32)
}

/// Nearest-rank percentile over an already-ascending-sorted slice. `q` in [0,1].
fn sorted_percentile_f32(sorted: &[f32], q: f64) -> Option<f32> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(id: &str, score: f32) -> ScoredSubject {
        ScoredSubject {
            subject_key: id.to_string(),
            score,
        }
    }

    #[test]
    fn enum_normalization_rejects_unknown_values() {
        assert_eq!(normalize_enum(None, RUNTIME_BACKENDS, "python"), "python");
        assert_eq!(
            normalize_enum(Some("rust_shadow".to_string()), RUNTIME_BACKENDS, "python"),
            "rust_shadow"
        );
        assert_eq!(
            normalize_enum(Some("nonsense".to_string()), RUNTIME_BACKENDS, "python"),
            "python"
        );
    }

    #[test]
    fn parses_ranked_ids_and_similarities() {
        let response = serde_json::json!({
            "status": "success",
            "results": [
                {"screenshot_id": 7, "similarity": 0.9},
                {"screenshot_id": 3, "similarity": null},
                {"screenshot_id": 0, "similarity": 0.5},
                {"screenshot_id": 7, "similarity": 0.9}
            ]
        });
        let parsed = parse_python_results(&response);
        assert_eq!(parsed, vec![(7, Some(0.9)), (3, None)]);
    }

    #[test]
    fn overlap_and_top1_are_computed_over_the_window() {
        let python: Vec<(i64, Option<f32>)> = vec![(1, None), (2, None), (3, None)];
        let rust = vec![scored("1", 0.99), scored("2", 0.98), scored("9", 0.10)];
        let window = OVERLAP_WINDOW.min(python.len());
        let py: HashSet<i64> = python.iter().map(|(id, _)| *id).take(window).collect();
        let ru: HashSet<i64> = rust
            .iter()
            .filter_map(|s| s.subject_key.parse::<i64>().ok())
            .take(window)
            .collect();
        assert_eq!(py.intersection(&ru).count(), 2);
    }

    #[test]
    fn resolve_probe_queries_uses_defaults_and_dedupes() {
        let defaulted = resolve_probe_queries(None);
        assert_eq!(defaulted.len(), DEFAULT_PROBE_QUERIES.len());

        let custom = resolve_probe_queries(Some(vec![
            "  hello  ".to_string(),
            "hello".to_string(),
            "   ".to_string(),
            "world".to_string(),
        ]));
        assert_eq!(custom, vec!["hello".to_string(), "world".to_string()]);

        // Empty vec falls back to defaults, not an empty run.
        assert_eq!(resolve_probe_queries(Some(vec![])).len(), DEFAULT_PROBE_QUERIES.len());
    }

    #[test]
    fn default_corpus_is_deduped_and_within_cap() {
        // The expanded corpus must have no accidental duplicates and stay under
        // the per-run cap so `resolve_probe_queries` runs all of it.
        let resolved = resolve_probe_queries(None);
        assert_eq!(resolved.len(), DEFAULT_PROBE_QUERIES.len());
        assert!(DEFAULT_PROBE_QUERIES.len() <= MAX_PROBE_QUERIES);
        // A materially larger corpus than the original 12 so p05/p95 are less
        // noise-dominated; the exact count can grow, but not shrink below this.
        assert!(DEFAULT_PROBE_QUERIES.len() >= 64);
    }

    #[test]
    fn cosine_matches_dot_product_for_normalized_vectors() {
        // Identical direction → 1.0; orthogonal → 0.0; opposite → -1.0.
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap().abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]).unwrap() + 1.0).abs() < 1e-6);
        // Scale-invariant: cosine ignores magnitude.
        assert!((cosine(&[3.0, 0.0], &[0.5, 0.0]).unwrap() - 1.0).abs() < 1e-6);
        // Degenerate inputs are rejected rather than producing NaN.
        assert!(cosine(&[0.0, 0.0], &[1.0, 0.0]).is_none());
        assert!(cosine(&[1.0], &[1.0, 0.0]).is_none());
    }

    #[test]
    fn sorted_percentile_uses_nearest_rank() {
        let sorted = [0.90_f32, 0.95, 0.98, 0.99, 1.00];
        assert_eq!(sorted_percentile_f32(&sorted, 0.0), Some(0.90));
        assert_eq!(sorted_percentile_f32(&sorted, 0.5), Some(0.98));
        assert_eq!(sorted_percentile_f32(&sorted, 0.05), Some(0.90));
        assert_eq!(sorted_percentile_f32(&[], 0.5), None);
    }
}
