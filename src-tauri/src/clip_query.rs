//! M2.5 step 9 — Rust-owned Chinese-CLIP text-to-image search.
//!
//! Provides text encoding and vector similarity queries against the Rust CLIP image index.

use crate::clip_migration::{
    clip_document_id, clip_memory_uri, CLIP_DIMENSIONS, CLIP_VECTOR_SPACE_REVISION,
};
use crate::ml_protocol::MlSemanticModel;
use crate::registry_config;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{DerivedIndexKind, StorageState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Minimum cosine similarity for a hit, frozen by the M2.1 oracle
/// (`clip_vector_search.min_similarity`). Cross-modal scores are low in
/// absolute terms; this is the floor below which a match is noise.
pub const CLIP_MIN_SIMILARITY: f32 = 0.32;

/// Timeout budget for text encoding, covering model load and worker wait.
const QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(15);

/// Python's own bound on `limit`, preserved so the two backends cannot be told
/// apart by a caller that asks for too much.
pub const MAX_CLIP_RESULTS: u32 = 200;

const CLIP_RUNTIMES: &[&str] = &["python", "rust"];
const CLIP_INDEXES: &[&str] = &["chroma", "dual", "rust"];

const DEFAULT_CLIP_RUNTIME: &str = "rust";
const DEFAULT_CLIP_INDEX: &str = "rust";

/// Selected CLIP inference backend (`python` | `rust`).
pub fn clip_runtime() -> String {
    normalize_enum(
        registry_config::get_string("clip_runtime"),
        CLIP_RUNTIMES,
        DEFAULT_CLIP_RUNTIME,
    )
}

/// Selected CLIP index ownership backend (`chroma` | `dual` | `rust`).
pub fn clip_index_backend() -> String {
    normalize_enum(
        registry_config::get_string("clip_index"),
        CLIP_INDEXES,
        DEFAULT_CLIP_INDEX,
    )
}

/// Whether `value` names a selectable CLIP inference backend.
///
/// Exposed so the command that persists the choice validates against this list
/// rather than a copy of it. Both CLIP keys first shipped readable but not
/// writable: the readers above landed while the settings command kept its own
/// hard-coded list of enum names, and nothing forced that list to grow with
/// them. One owner per enum is what stops that repeating.
pub fn is_selectable_clip_runtime(value: &str) -> bool {
    CLIP_RUNTIMES.contains(&value)
}

/// Whether `value` names a selectable CLIP index owner.
pub fn is_selectable_clip_index(value: &str) -> bool {
    CLIP_INDEXES.contains(&value)
}

fn normalize_enum(value: Option<String>, allowed: &[&str], default: &str) -> String {
    match value {
        Some(value) if allowed.contains(&value.as_str()) => value,
        _ => default.to_string(),
    }
}

/// Read-only backend diagnostic, the whole of what the observable-fallback rule
/// requires for this capability.
#[derive(Debug, Clone, Serialize)]
pub struct ClipBackendStatus {
    pub clip_index: String,
    pub clip_runtime: String,
    /// Backend that served the most recent `search_nl`.
    pub last_query_backend: Option<String>,
    /// Why the last Rust attempt did not serve, when it did not.
    pub last_fallback_reason: Option<String>,
    pub fallback_count: u64,
    /// Query-visible `clip_image` vectors held locally.
    pub indexed_vectors: Option<u64>,
    /// Images captured but not yet encoded, waiting for an idle window.
    pub index_backlog: Option<u64>,
    /// Queued images whose encode retry budget is spent.
    pub index_stalled: Option<u64>,
    pub index_backlog_age_secs: Option<i64>,
    /// A manual CLIP indexing run is executing right now.
    pub index_run_active: bool,
}

#[derive(Debug, Default)]
struct BackendObservations {
    last_query_backend: Option<String>,
    last_fallback_reason: Option<String>,
    fallback_count: u64,
}

static OBSERVATIONS: RwLock<Option<BackendObservations>> = RwLock::new(None);

/// Caches the one-way transition of the step-7 migration sentinel, for the same
/// reason `semantic_query.rs` caches MiniLM's: it is written once per vector
/// space and never cleared, so once observed the answer cannot change for the
/// life of the process.
static MIGRATION_SETTLED: AtomicBool = AtomicBool::new(false);

/// Whether the sentinel-triggered step-7 copy has finished for this vector
/// space.
///
/// The copy commits page by page and a migrated row becomes query-visible as
/// soon as its job row reaches `completed`, so an interrupted run leaves a
/// *prefix* of the collection: not empty, therefore not caught by the
/// empty-index refusal, but missing whatever the cursor never reached. Ranking
/// that returns a plausible page with screenshots silently absent from it,
/// which is the failure this refusal exists to prevent.
///
/// `clip_index.rs::repair_scope` asks the same question for the write path, and
/// for the mirror-image reason: until the copy settles, an image with no vector
/// is one the copy has not reached, so re-encoding it would spend hours
/// reproducing vectors Chroma already holds.
pub(crate) fn migration_settled(storage: &StorageState) -> bool {
    if MIGRATION_SETTLED.load(Ordering::Relaxed) {
        return true;
    }
    let settled = storage
        .is_auto_migration_done(DerivedIndexKind::ClipImage, CLIP_VECTOR_SPACE_REVISION)
        // A database that cannot be read cannot vouch for its own completeness.
        .unwrap_or(false);
    if settled {
        MIGRATION_SETTLED.store(true, Ordering::Relaxed);
    }
    settled
}

fn with_observations<T>(edit: impl FnOnce(&mut BackendObservations) -> T) -> T {
    let mut guard = OBSERVATIONS
        .write()
        .unwrap_or_else(|error| error.into_inner());
    edit(guard.get_or_insert_with(BackendObservations::default))
}

fn observe_served(backend: &str) {
    with_observations(|entry| entry.last_query_backend = Some(backend.to_string()));
}

fn observe_fallback(reason: &str) {
    with_observations(|entry| {
        entry.last_fallback_reason = Some(reason.to_string());
        entry.fallback_count = entry.fallback_count.saturating_add(1);
    });
}

/// Mark Python as the backend that served the current query. Called after
/// Python has actually answered, not when the fallback is decided.
pub fn observe_python_served() {
    observe_served("python");
}

pub fn backend_status(storage: Option<&StorageState>) -> ClipBackendStatus {
    backend_status_impl(storage, true)
}

pub(crate) fn backend_status_without_vector_count(
    storage: Option<&StorageState>,
) -> ClipBackendStatus {
    backend_status_impl(storage, false)
}

fn backend_status_impl(
    storage: Option<&StorageState>,
    include_vector_count: bool,
) -> ClipBackendStatus {
    let guard = OBSERVATIONS
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let (last_query_backend, last_fallback_reason, fallback_count) = match guard.as_ref() {
        Some(entry) => (
            entry.last_query_backend.clone(),
            entry.last_fallback_reason.clone(),
            entry.fallback_count,
        ),
        None => (None, None, 0),
    };
    let backlog = storage.and_then(|storage| {
        storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, crate::clip_index::MAX_ATTEMPTS)
            .ok()
    });
    ClipBackendStatus {
        clip_index: clip_index_backend(),
        clip_runtime: clip_runtime(),
        last_query_backend,
        last_fallback_reason,
        fallback_count,
        indexed_vectors: include_vector_count
            .then(|| {
                storage.and_then(|storage| {
                    storage
                        .count_query_visible_embeddings(DerivedIndexKind::ClipImage)
                        .ok()
                })
            })
            .flatten(),
        index_backlog: backlog.map(|backlog| backlog.claimable),
        index_stalled: backlog.map(|backlog| backlog.exhausted),
        index_backlog_age_secs: backlog.and_then(|backlog| backlog.oldest_claimable_age_secs),
        // Overwritten by the command, which can reach the run state.
        index_run_active: false,
    }
}

/// Outcome of offering one `search_nl` query to the Rust CLIP path.
pub enum ClipQueryOutcome {
    /// Rust served it; the value is the complete `results` array.
    Served(Vec<serde_json::Value>),
    /// The configuration does not select Rust. Not a failure, not counted.
    NotSelected,
    /// Rust was selected but could not serve. The caller must use Python.
    FellBack(String),
}

/// One `search_nl` request, in the shape the Tauri command and the MCP tool
/// both already speak.
pub struct ClipQueryRequest<'a> {
    pub query: &'a str,
    pub limit: u32,
    pub offset: u32,
    pub process_names: &'a [String],
    /// Unix seconds, inclusive.
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
}

/// Offer one natural-language image query to the Rust CLIP index.
///
/// Never returns an error: anything that stops Rust answering becomes
/// `FellBack`, so the caller can serve the user from Python instead.
pub async fn try_rust_clip_query(
    app: &AppHandle,
    request: ClipQueryRequest<'_>,
) -> ClipQueryOutcome {
    if clip_index_backend() != "rust" {
        return ClipQueryOutcome::NotSelected;
    }
    // Serving from the Rust store necessarily encodes the query with the Rust
    // CLIP runtime — scoring Rust-held vectors against a Python-produced query
    // vector would mean an IPC round trip that defeats the point. So an explicit
    // `clip_runtime = python` is honoured as a refusal rather than silently
    // overridden; it is the second, independent rollback lever.
    if clip_runtime() != "rust" {
        return ClipQueryOutcome::NotSelected;
    }
    let trimmed = request.query.trim();
    if trimmed.is_empty() {
        observe_served("rust");
        return ClipQueryOutcome::Served(Vec::new());
    }

    // Announce the query before anything slow, so the background passes stop
    // submitting to the single semantic worker while this one is on its way to
    // it. Dropped by the guard on every path out of this function.
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let _foreground = semantic.foreground_lease();

    // A migration is rewriting the derived store; reading it would race the
    // rewrite. Python keeps serving from Chroma throughout maintenance, so this
    // refusal costs the user nothing.
    if crate::maintenance::is_active() {
        return fell_back("maintenance_in_progress");
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let settled = tokio::task::spawn_blocking(move || migration_settled(&storage))
        .await
        .unwrap_or(false);
    if !settled {
        return fell_back("migration_incomplete");
    }

    match run_rust_clip_query(app, trimmed, &request).await {
        Ok(results) if results.is_empty() => {
            // The 0.32 floor makes an empty result legitimate for a query
            // nothing matches, so this is not conclusive on its own — but an
            // unmigrated machine returns empty for *every* query, and serving
            // that silently would break visual search for that user. Checking
            // the store rather than the result distinguishes the two.
            let storage = app.state::<Arc<StorageState>>().inner().clone();
            let empty = tokio::task::spawn_blocking(move || {
                storage
                    .count_query_visible_embeddings(DerivedIndexKind::ClipImage)
                    .map(|count| count == 0)
                    .unwrap_or(true)
            })
            .await
            .unwrap_or(true);
            if empty {
                return fell_back("rust_index_empty");
            }
            observe_served("rust");
            ClipQueryOutcome::Served(Vec::new())
        }
        Ok(results) => {
            observe_served("rust");
            ClipQueryOutcome::Served(results)
        }
        Err(error) => fell_back(&error),
    }
}

/// Mark a Python-served response with the backend that produced it. Additive:
/// existing consumers read `results` and ignore the rest.
pub fn tag_python_response(mut response: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut response {
        map.insert(
            "backend".to_string(),
            serde_json::Value::String("python".to_string()),
        );
    }
    response
}

fn fell_back(reason: &str) -> ClipQueryOutcome {
    observe_fallback(reason);
    ClipQueryOutcome::FellBack(reason.to_string())
}

async fn run_rust_clip_query(
    app: &AppHandle,
    query: &str,
    request: &ClipQueryRequest<'_>,
) -> Result<Vec<serde_json::Value>, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();

    let started = Instant::now();
    let embedding = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::ChineseClip,
            vec![query.to_string()],
            QUERY_EMBED_TIMEOUT,
            // CPU, matching the image encoder. `clip_index.rs` records why.
            false,
        )
        .await
        .map_err(|error| format!("embed_failed: {error}"))?;
    let embed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let query_vec = embedding
        .vectors
        .into_iter()
        .next()
        .ok_or("embed_failed: semantic worker returned no embedding")?;
    if query_vec.len() != CLIP_DIMENSIONS {
        return Err(format!(
            "embed_failed: query vector has {} dimensions, expected {CLIP_DIMENSIONS}",
            query_vec.len()
        ));
    }

    // Python's over-fetch, preserved exactly: enough candidates that the
    // post-scan process and time filters still have a full page to paginate.
    let limit = request.limit.max(1) as usize;
    let offset = request.offset as usize;
    let target = limit.saturating_add(offset).max(limit);
    let fetch = target.saturating_mul(2).max(target.saturating_add(20));

    let scan_storage = storage.clone();
    let process_names: Vec<String> = request
        .process_names
        .iter()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    let start_time = request.start_time;
    let end_time = request.end_time;

    // Scan, hydrate, and filter share one blocking thread. The scan takes the
    // process-wide, non-reentrant database mutex and hydration additionally does
    // per-row CNG decryption; hopping back onto the async runtime between them
    // would park a tokio worker mid-sequence on the lock the reverse-IPC bridge
    // also needs.
    let (results, scan_ms) = tokio::task::spawn_blocking(move || {
        let scan_started = Instant::now();
        let scored = scan_storage.clip_image_topk(&query_vec, fetch)?;
        // The floor is applied before hydration so a rejected hit costs no
        // decryption. Python applies it inside `search_by_text`, at the same
        // point relative to everything else.
        let above_floor: Vec<crate::storage::ScoredSubject> = scored
            .into_iter()
            .filter(|subject| subject.score >= CLIP_MIN_SIMILARITY)
            .collect();
        let rows = hydrate(&scan_storage, &above_floor)?;
        let filtered = apply_filters(rows, &process_names, start_time, end_time);
        Ok::<_, String>((
            paginate(filtered, offset, limit),
            scan_started.elapsed().as_secs_f64() * 1000.0,
        ))
    })
    .await
    .map_err(|error| format!("scan_task_failed: {error}"))??;

    tracing::debug!(
        "[CLIP] rust nl query embed={embed_ms:.1}ms scan={scan_ms:.1}ms returned={}",
        results.len()
    );
    Ok(results)
}

/// One scored image resolved to the screenshot that represents it.
struct ClipHit {
    row: serde_json::Value,
    process_name: String,
    /// Unix seconds, or `None` when the screenshot has no usable timestamp.
    created_secs: Option<f64>,
}

/// Turn scored image hashes into the Python response rows.
fn hydrate(
    storage: &StorageState,
    scored: &[crate::storage::ScoredSubject],
) -> Result<Vec<ClipHit>, String> {
    if scored.is_empty() {
        return Ok(Vec::new());
    }
    let hashes: Vec<String> = scored
        .iter()
        .map(|subject| subject.subject_key.clone())
        .collect();
    let mapped = storage.map_image_hashes_to_screenshot_ids(&hashes)?;
    // Newest capture of each image, which is what `map_image_hashes_to_screenshot_ids`
    // orders for.
    let representative: HashMap<&String, i64> = mapped
        .iter()
        .filter_map(|(hash, ids)| ids.first().map(|id| (hash, *id)))
        .collect();
    let ids: Vec<i64> = representative.values().copied().collect();

    let summaries: HashMap<i64, crate::storage::BackgroundScreenshotSummary> = storage
        .get_screenshot_summaries_by_ids_silent(&ids)
        .map_err(|error| format!("hydrate_failed: {error}"))?
        .into_iter()
        .map(|summary| (summary.id, summary))
        .collect();
    let texts = storage
        .get_ocr_text_prefixes_by_screenshot_ids_silent(&ids, CLIP_OCR_SNIPPET_CHARS)
        .map_err(|error| format!("hydrate_failed: {error}"))?;

    let mut hits = Vec::with_capacity(scored.len());
    for subject in scored {
        let Some(id) = representative.get(&subject.subject_key).copied() else {
            // No live screenshot maps to this image any more.
            continue;
        };
        let Some(summary) = summaries.get(&id) else {
            continue;
        };
        let process_name = summary.process_name.clone().unwrap_or_default();
        let window_title = summary.window_title.clone().unwrap_or_default();
        let ocr_text = texts.get(&id).cloned().unwrap_or_default();
        let created_secs = created_seconds(summary.timestamp);
        let created_at = created_secs.map(format_created_at);
        let image_path = clip_memory_uri(&subject.subject_key);
        // Stored and query vectors are both L2-normalized, so the scan's dot
        // product is cosine similarity — the same quantity Python derives as
        // `1 - chroma_cosine_distance`.
        let similarity = subject.score;

        hits.push(ClipHit {
            row: serde_json::json!({
                // The Chroma document id, reproduced rather than invented, so a
                // caller keying on `id` cannot tell the backends apart.
                "id": clip_document_id(&subject.subject_key),
                "image_path": image_path.clone(),
                "metadata": {
                    "image_path": image_path,
                    "screenshot_id": id,
                    "process_name": process_name.clone(),
                    "window_title": window_title,
                    "category": summary.category.clone().unwrap_or_default(),
                    "timestamp": created_secs.unwrap_or(0.0),
                    "created_at": created_at.clone(),
                    "screenshot_created_at": created_at.clone(),
                },
                "ocr_text": ocr_text,
                "distance": 1.0_f32 - similarity,
                "similarity": similarity,
                "screenshot_created_at": created_at,
            }),
            process_name,
            created_secs,
        });
    }
    Ok(hits)
}

/// OCR characters returned with a hit. Both result views cut the text to 150
/// characters for display; this leaves room for that without decrypting whole
/// documents for a page nobody scrolls.
const CLIP_OCR_SNIPPET_CHARS: usize = 1_000;

/// Unix seconds for a hit, or `None` when the screenshot has no usable
/// timestamp.
///
/// The house rule is milliseconds at every layer, and this one field is the
/// exception: the summary query selects `strftime('%s', created_at)`, which is
/// Unix *seconds*, and
/// `storage/screenshot.rs::EncryptedScreenshotSummaryRow::from_row` parses that
/// string without converting it. Everything downstream is seconds too — the
/// `start_time`/`end_time` bounds the search box and the MCP tool send,
/// [`format_created_at`], and the `timestamp` metadata Python's `add_image`
/// writes into Chroma — so nothing on this path converts units. The other
/// consumers of the same field agree: `semantic_query.rs` and
/// `minilm_index.rs::mirror_record` both use it as seconds directly.
fn created_seconds(timestamp: Option<i64>) -> Option<f64> {
    timestamp
        .filter(|value| *value > 0)
        .map(|seconds| seconds as f64)
}

fn format_created_at(seconds: f64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// Process and time filtering, in Python's order and with Python's tolerance
/// for an unknown timestamp.
///
/// The "keep a row whose timestamp is unknown" rule is ported rather than
/// tightened: the M2.1 oracle pins it
/// (`search_nl.time_filter_keeps_unknown_timestamp`). What changes is how often
/// it applies — for Python it was every capture-written row, because
/// `created_at` was never in the metadata; here it is only a screenshot with no
/// usable timestamp in SQLite, which is rare. That is the bug fix the module
/// header records.
fn apply_filters(
    hits: Vec<ClipHit>,
    process_names: &[String],
    start_time: Option<f64>,
    end_time: Option<f64>,
) -> Vec<serde_json::Value> {
    hits.into_iter()
        .filter(|hit| {
            if !process_names.is_empty() && !process_names.contains(&hit.process_name) {
                return false;
            }
            match hit.created_secs {
                Some(created) => {
                    if start_time.is_some_and(|start| created < start) {
                        return false;
                    }
                    if end_time.is_some_and(|end| created > end) {
                        return false;
                    }
                    true
                }
                None => true,
            }
        })
        .map(|hit| hit.row)
        .collect()
}

fn paginate(rows: Vec<serde_json::Value>, offset: usize, limit: usize) -> Vec<serde_json::Value> {
    rows.into_iter().skip(offset).take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(process: &str, created_secs: Option<f64>) -> ClipHit {
        ClipHit {
            row: serde_json::json!({ "process_name": process }),
            process_name: process.to_string(),
            created_secs,
        }
    }

    #[test]
    fn unknown_enum_values_fall_back_to_the_shipped_defaults() {
        assert_eq!(
            normalize_enum(Some("python".to_string()), CLIP_RUNTIMES, "rust"),
            "python"
        );
        // A `rust_shadow` written by a future build, or by hand, normalizes
        // rather than selecting a mode that does not exist.
        assert_eq!(
            normalize_enum(Some("rust_shadow".to_string()), CLIP_RUNTIMES, "rust"),
            "rust"
        );
        assert_eq!(
            normalize_enum(None, CLIP_INDEXES, DEFAULT_CLIP_INDEX),
            "rust"
        );
    }

    #[test]
    fn the_cutover_defaults_select_rust_for_both_enums() {
        // A cutover that left the defaults at the Python values would ship a
        // switch nobody flips.
        assert_eq!(
            normalize_enum(None, CLIP_RUNTIMES, DEFAULT_CLIP_RUNTIME),
            "rust"
        );
        assert_eq!(
            normalize_enum(None, CLIP_INDEXES, DEFAULT_CLIP_INDEX),
            "rust"
        );
    }

    #[test]
    fn the_similarity_floor_matches_the_frozen_oracle() {
        // `monitor/oracle/golden-v1.json` pins 0.32 for both
        // `clip_vector_search` and every `search_nl` case.
        assert_eq!(CLIP_MIN_SIMILARITY, 0.32);
    }

    #[test]
    fn the_process_filter_keeps_only_named_processes() {
        let hits = vec![
            hit("chrome.exe", Some(100.0)),
            hit("code.exe", Some(100.0)),
            hit("chrome.exe", Some(100.0)),
        ];
        let filtered = apply_filters(hits, &["chrome.exe".to_string()], None, None);
        assert_eq!(filtered.len(), 2);

        let hits = vec![hit("chrome.exe", Some(100.0)), hit("code.exe", Some(100.0))];
        // An empty process list means "no process filter", not "match nothing".
        assert_eq!(apply_filters(hits, &[], None, None).len(), 2);
    }

    #[test]
    fn the_time_filter_bounds_both_ends_and_keeps_unknown_timestamps() {
        let hits = vec![
            hit("a", Some(50.0)),
            hit("a", Some(150.0)),
            hit("a", Some(250.0)),
            hit("a", None),
        ];
        let filtered = apply_filters(hits, &[], Some(100.0), Some(200.0));
        // 150 survives on its own merit; the undated row survives because the
        // oracle says an unknown timestamp passes.
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn the_summary_timestamp_is_seconds_and_is_not_rescaled() {
        // `strftime('%s', created_at)` is Unix seconds and `from_row` parses it
        // unchanged, so a `millis / 1000` here would land every hit in January
        // 1970: bounded queries would return nothing and the result card would
        // show a 1970 date. 1_785_888_000 is 2026-08-05 00:00:00 UTC.
        assert_eq!(created_seconds(Some(1_785_888_000)), Some(1_785_888_000.0));
        // Rendered in local time, so only the year is safe to pin — but a
        // rescaled value cannot reach 2026 in any timezone.
        assert!(format_created_at(1_785_888_000.0).starts_with("2026-"));
        // An epoch-zero or missing timestamp stays "unknown", which
        // `apply_filters` lets through.
        assert_eq!(created_seconds(Some(0)), None);
        assert_eq!(created_seconds(None), None);
    }

    #[test]
    fn pagination_skips_then_takes_like_python_slicing() {
        let rows: Vec<serde_json::Value> = (0..10).map(|i| serde_json::json!(i)).collect();
        assert_eq!(
            paginate(rows.clone(), 0, 3),
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );
        assert_eq!(paginate(rows.clone(), 8, 5).len(), 2);
        assert_eq!(paginate(rows, 20, 5).len(), 0);
    }
}
