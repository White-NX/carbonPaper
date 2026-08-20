//! M2.5 step 9 — Rust-owned Chinese-CLIP text-to-image search.
//!
//! Provides text encoding and vector similarity queries against the Rust CLIP image index.

use crate::clip_migration::{
    clip_document_id, clip_memory_uri, CLIP_DIMENSIONS, CLIP_VECTOR_SPACE_REVISION,
};
use crate::ml_protocol::MlSemanticModel;
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
/// End-to-end Rust query budget. This covers model encoding, ANN/exact
/// selection, SQL hydration, decryption, filtering, and pagination.
const QUERY_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
/// A persisted generation is already being checksummed and mmap-opened at
/// startup. Let that single-flight finish instead of racing it with the exact
/// scan this feature exists to remove.
const QUERY_ANN_ARM_WAIT: Duration = Duration::from_secs(10);

/// Bound on the number of results returned by one request.
pub const MAX_CLIP_RESULTS: u32 = 200;
/// Preserve the existing IPC/MCP deep-pagination contract. Requests whose
/// exact over-fetch exceeds the ANN envelope explicitly use one full exact
/// scan rather than being silently clamped to a shallower page.
pub const MAX_CLIP_OFFSET: u32 = 10_000;

/// Read-only CLIP retrieval and ANN diagnostic.
#[derive(Debug, Clone, Serialize)]
pub struct ClipBackendStatus {
    /// Most recent Rust retrieval error, cleared by the next success.
    pub last_error: Option<String>,
    pub failure_count: u64,
    /// Query-visible `clip_image` vectors held locally.
    pub indexed_vectors: Option<u64>,
    /// Images captured but not yet encoded, waiting for an idle window.
    pub index_backlog: Option<u64>,
    /// Queued images whose encode retry budget is spent.
    pub index_stalled: Option<u64>,
    pub index_backlog_age_secs: Option<i64>,
    /// A manual CLIP indexing run is executing right now.
    pub index_run_active: bool,
    /// Persistent HNSW readiness. `armed` serves ANN candidates; otherwise
    /// queries remain correct through exact fallback.
    pub ann_state: String,
    pub ann_generation: Option<u64>,
    pub ann_tail_rows: Option<u64>,
    pub ann_last_error: Option<String>,
    /// Build health is independent from readiness: a live generation can keep
    /// serving while its refresh is in backoff.
    pub ann_build_state: String,
    pub ann_build_failure_count: u32,
    pub ann_build_last_failure_at: Option<String>,
    pub ann_build_next_retry_at: Option<String>,
    pub ann_build_error_code: Option<String>,
}

#[derive(Debug, Default)]
struct BackendObservations {
    last_error: Option<String>,
    failure_count: u64,
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

fn observe_served() {
    with_observations(|entry| entry.last_error = None);
}

fn observe_failure(reason: &str) {
    with_observations(|entry| {
        entry.last_error = Some(reason.to_string());
        entry.failure_count = entry.failure_count.saturating_add(1);
    });
}

pub fn backend_status(
    storage: Option<&StorageState>,
    ann: Option<&crate::clip_ann::ClipAnnState>,
) -> ClipBackendStatus {
    backend_status_impl(storage, ann, true)
}

pub(crate) fn backend_status_without_vector_count(
    storage: Option<&StorageState>,
    ann: Option<&crate::clip_ann::ClipAnnState>,
) -> ClipBackendStatus {
    backend_status_impl(storage, ann, false)
}

fn backend_status_impl(
    storage: Option<&StorageState>,
    ann: Option<&crate::clip_ann::ClipAnnState>,
    include_vector_count: bool,
) -> ClipBackendStatus {
    let guard = OBSERVATIONS
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let (last_error, failure_count) = match guard.as_ref() {
        Some(entry) => (entry.last_error.clone(), entry.failure_count),
        None => (None, 0),
    };
    let backlog = storage.and_then(|storage| {
        storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, crate::clip_index::MAX_ATTEMPTS)
            .ok()
    });
    let (ann_state, ann_generation, ann_last_error) = ann
        .map(crate::clip_ann::ClipAnnState::status)
        .unwrap_or(("unavailable", None, None));
    let ann_tail_rows = storage.and_then(|storage| {
        storage
            .get_derived_ann_generation(DerivedIndexKind::ClipImage)
            .ok()
            .flatten()
            .and_then(|generation| {
                storage
                    .derived_ann_tail_count(DerivedIndexKind::ClipImage, generation.covered_epoch)
                    .ok()
            })
    });
    let ann_build = storage.and_then(|storage| {
        storage
            .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
            .ok()
            .flatten()
    });
    let ann_build_state = ann_build
        .as_ref()
        .map(|state| {
            if state.circuit_open {
                "circuit_open"
            } else {
                "backoff"
            }
        })
        .unwrap_or("healthy");
    let persisted_ann_error = ann_build.as_ref().map(|state| state.last_error.clone());
    ClipBackendStatus {
        last_error,
        failure_count,
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
        ann_state: ann_state.to_string(),
        ann_generation,
        ann_tail_rows,
        ann_last_error: ann_last_error.or(persisted_ann_error),
        ann_build_state: ann_build_state.to_string(),
        ann_build_failure_count: ann_build
            .as_ref()
            .map(|state| state.consecutive_failures)
            .unwrap_or(0),
        ann_build_last_failure_at: ann_build
            .as_ref()
            .map(|state| state.last_failure_at.clone()),
        ann_build_next_retry_at: ann_build.as_ref().map(|state| state.next_retry_at.clone()),
        ann_build_error_code: ann_build
            .as_ref()
            .map(|state| state.last_error_code.clone()),
    }
}

/// Outcome of offering one `search_nl` query to the Rust CLIP path.
pub enum ClipQueryOutcome {
    /// Rust served it; the value is the complete `results` array.
    Served(Vec<serde_json::Value>),
    /// Rust could not serve the request.
    Unavailable(String),
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
/// Anything that stops Rust answering becomes `Unavailable` with a specific
/// model, migration, or index reason.
pub async fn try_rust_clip_query(
    app: &AppHandle,
    request: ClipQueryRequest<'_>,
) -> ClipQueryOutcome {
    let trimmed = request.query.trim();
    if trimmed.is_empty() {
        observe_served();
        return ClipQueryOutcome::Served(Vec::new());
    }

    // Announce the query before anything slow, so the background passes stop
    // submitting to the single semantic worker while this one is on its way to
    // it. Dropped by the guard on every path out of this function.
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let _foreground = semantic.foreground_lease();

    // A migration is rewriting the derived store; reading it would race the
    // rewrite, so report the temporary unavailability explicitly.
    if crate::maintenance::is_active() {
        return unavailable("maintenance_in_progress");
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let settled = tokio::task::spawn_blocking(move || migration_settled(&storage))
        .await
        .unwrap_or(false);
    if !settled {
        return unavailable("migration_incomplete");
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
                return unavailable("rust_index_empty");
            }
            observe_served();
            ClipQueryOutcome::Served(Vec::new())
        }
        Ok(results) => {
            observe_served();
            ClipQueryOutcome::Served(results)
        }
        Err(error) => unavailable(&error),
    }
}

fn unavailable(reason: &str) -> ClipQueryOutcome {
    observe_failure(reason);
    ClipQueryOutcome::Unavailable(reason.to_string())
}

async fn run_rust_clip_query(
    app: &AppHandle,
    query: &str,
    request: &ClipQueryRequest<'_>,
) -> Result<Vec<serde_json::Value>, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let ann_state = app
        .state::<Arc<crate::clip_ann::ClipAnnState>>()
        .inner()
        .clone();

    let started = Instant::now();
    let deadline = started + QUERY_TOTAL_TIMEOUT;
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

    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        let _ = ann_state
            .wait_for_startup_arm(QUERY_ANN_ARM_WAIT.min(remaining))
            .await;
    }

    // Python's over-fetch, preserved exactly: enough candidates that the
    // post-scan process and time filters still have a full page to paginate.
    let limit = request.limit.max(1) as usize;
    let offset = bounded_clip_offset(request.offset);
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
    let scan_task = tokio::task::spawn_blocking(move || {
        if Instant::now() >= deadline {
            return Err("query_deadline_exceeded_before_scan".to_string());
        }
        let scan_started = Instant::now();
        let _foreground_db = scan_storage.foreground_db_read();
        let filtered = search_candidates(
            fetch,
            target,
            deadline,
            |requested| app_ann_query(&ann_state, &scan_storage, &query_vec, requested, deadline),
            |scored| {
                filter_scored(
                    &scan_storage,
                    scored,
                    &process_names,
                    start_time,
                    end_time,
                    deadline,
                )
            },
            || app_exact_query(&ann_state, &scan_storage, &query_vec, fetch, deadline),
        )?;
        if Instant::now() >= deadline {
            return Err("query_deadline_exceeded_after_hydrate".to_string());
        }
        Ok::<_, String>((
            paginate(filtered, offset, limit),
            scan_started.elapsed().as_secs_f64() * 1000.0,
        ))
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (results, scan_ms) = tokio::time::timeout(remaining, scan_task)
        .await
        .map_err(|_| "query_deadline_exceeded".to_string())?
        .map_err(|error| format!("scan_task_failed: {error}"))??;

    tracing::debug!(
        "[CLIP] rust nl query embed={embed_ms:.1}ms scan={scan_ms:.1}ms returned={}",
        results.len()
    );
    Ok(results)
}

fn app_ann_query(
    state: &crate::clip_ann::ClipAnnState,
    storage: &StorageState,
    query: &[f32],
    fetch: usize,
    deadline: Instant,
) -> Result<CandidateSource, String> {
    match state.query(storage, query, fetch) {
        Ok(Some(result)) => {
            tracing::debug!(
                "[CLIP:ANN] query mode={} tail={} candidates={}",
                result.mode,
                result.tail_rows,
                result.candidates.len()
            );
            Ok(CandidateSource::Ann(result.candidates))
        }
        Ok(None) => Ok(CandidateSource::Unavailable),
        Err(error) => {
            tracing::warn!("[CLIP:ANN] query failed, using exact fallback: {error}");
            let fallback = app_exact_query(state, storage, query, fetch, deadline);
            state.disarm();
            fallback.map(CandidateSource::ExactFallback)
        }
    }
}

fn app_exact_query(
    state: &crate::clip_ann::ClipAnnState,
    storage: &StorageState,
    query: &[f32],
    fetch: usize,
    deadline: Instant,
) -> Result<Vec<crate::storage::ScoredSubject>, String> {
    match state.exact_from_generation(storage, query, fetch, Some(deadline))? {
        Some(result) => {
            tracing::debug!(
                "[CLIP:ANN] query mode={} tail={} candidates={}",
                result.mode,
                result.tail_rows,
                result.candidates.len()
            );
            Ok(result.candidates)
        }
        None => storage.clip_image_topk_with_deadline(query, fetch, deadline),
    }
}

enum CandidateSource {
    Ann(Vec<crate::storage::ScoredSubject>),
    ExactFallback(Vec<crate::storage::ScoredSubject>),
    Unavailable,
}

fn ann_candidate_attempts(fetch: usize) -> Vec<usize> {
    [1usize, 2, 4]
        .into_iter()
        .map(|multiplier| {
            fetch
                .saturating_mul(multiplier)
                .min(crate::clip_ann::ANN_MAX_CANDIDATES)
        })
        .fold(Vec::new(), |mut attempts, requested| {
            if attempts.last().copied() != Some(requested) {
                attempts.push(requested);
            }
            attempts
        })
}

fn bounded_clip_offset(offset: u32) -> usize {
    offset.min(MAX_CLIP_OFFSET) as usize
}

fn search_candidates(
    fetch: usize,
    target: usize,
    deadline: Instant,
    mut ann_query: impl FnMut(usize) -> Result<CandidateSource, String>,
    mut filter: impl FnMut(Vec<crate::storage::ScoredSubject>) -> Result<Vec<serde_json::Value>, String>,
    exact_scan: impl FnOnce() -> Result<Vec<crate::storage::ScoredSubject>, String>,
) -> Result<Vec<serde_json::Value>, String> {
    if fetch > crate::clip_ann::ANN_MAX_CANDIDATES {
        tracing::debug!(
            "[CLIP:ANN] fetch={fetch} exceeds candidate envelope; using exact for deep pagination"
        );
        return filter(exact_scan()?);
    }

    let mut best = Vec::new();
    for requested in ann_candidate_attempts(fetch) {
        if Instant::now() >= deadline {
            return Err("query_deadline_exceeded_during_ann_deepening".to_string());
        }
        match ann_query(requested)? {
            CandidateSource::Ann(scored) => {
                let current = filter(scored)?;
                if current.len() > best.len() {
                    best = current;
                }
                if best.len() >= target {
                    return Ok(best);
                }
                if requested < crate::clip_ann::ANN_MAX_CANDIDATES {
                    tracing::debug!(
                        "[CLIP:ANN] filtered candidates insufficient; deepening after {requested}"
                    );
                }
            }
            CandidateSource::ExactFallback(scored) => return filter(scored),
            CandidateSource::Unavailable => return filter(exact_scan()?),
        }
    }

    if best.len() >= target || Instant::now() >= deadline {
        return Ok(best);
    }
    filter(exact_scan()?)
}

fn filter_scored(
    storage: &StorageState,
    scored: Vec<crate::storage::ScoredSubject>,
    process_names: &[String],
    start_time: Option<f64>,
    end_time: Option<f64>,
    deadline: Instant,
) -> Result<Vec<serde_json::Value>, String> {
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_after_scan".to_string());
    }
    // The floor is applied before hydration so a rejected hit costs no
    // decryption. Python applies it inside `search_by_text`, at the same point
    // relative to everything else.
    let above_floor: Vec<crate::storage::ScoredSubject> = scored
        .into_iter()
        .filter(|subject| subject.score >= CLIP_MIN_SIMILARITY)
        .collect();
    let rows = hydrate_with_deadline(storage, &above_floor, deadline)?;
    Ok(apply_filters(rows, process_names, start_time, end_time))
}

/// One scored image resolved to the screenshot that represents it.
struct ClipHit {
    row: serde_json::Value,
    process_name: String,
    /// Unix seconds, or `None` when the screenshot has no usable timestamp.
    created_secs: Option<f64>,
}

/// Turn scored image hashes into the Python response rows.
fn hydrate_with_deadline(
    storage: &StorageState,
    scored: &[crate::storage::ScoredSubject],
    deadline: Instant,
) -> Result<Vec<ClipHit>, String> {
    if scored.is_empty() {
        return Ok(Vec::new());
    }
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_before_hydrate".to_string());
    }
    let hashes: Vec<String> = scored
        .iter()
        .map(|subject| subject.subject_key.clone())
        .collect();
    let mapped = storage.map_image_hashes_to_screenshot_ids(&hashes)?;
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_hydrate".to_string());
    }
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
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_hydrate".to_string());
    }
    let texts = storage
        .get_ocr_text_prefixes_by_screenshot_ids_silent(&ids, CLIP_OCR_SNIPPET_CHARS)
        .map_err(|error| format!("hydrate_failed: {error}"))?;
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_hydrate".to_string());
    }

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

/// Render a hit's capture time the way every other backend path renders one:
/// RFC 3339 in UTC, per `storage/wire_time.rs`.
///
/// This used to convert to the machine's local time and drop the zone marker,
/// which happened to display correctly only because JavaScript reads an
/// offset-less date-time as local. The OCR path forwarded UTC through the same
/// field name, so the two search modes disagreed about what the string meant
/// and no reader could tell them apart — issue #166.
fn format_created_at(seconds: f64) -> String {
    crate::storage::wire_time::from_unix_seconds(seconds as i64)
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
    use std::cell::{Cell, RefCell};

    fn hit(process: &str, created_secs: Option<f64>) -> ClipHit {
        ClipHit {
            row: serde_json::json!({ "process_name": process }),
            process_name: process.to_string(),
            created_secs,
        }
    }

    fn scored(count: usize) -> Vec<crate::storage::ScoredSubject> {
        (0..count)
            .map(|index| crate::storage::ScoredSubject {
                subject_key: format!("subject-{index}"),
                score: 1.0,
            })
            .collect()
    }

    fn json_rows(count: usize) -> Vec<serde_json::Value> {
        (0..count).map(|index| serde_json::json!(index)).collect()
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

    #[test]
    fn ann_candidate_search_uses_only_fetch_double_and_quadruple() {
        assert_eq!(ann_candidate_attempts(100), vec![100, 200, 400]);
        assert_eq!(ann_candidate_attempts(2_000), vec![2_000, 4_000, 4_096]);
        assert_eq!(ann_candidate_attempts(4_096), vec![4_096]);

        let calls = RefCell::new(Vec::new());
        let exact_calls = Cell::new(0);
        let result = search_candidates(
            100,
            10,
            Instant::now() + Duration::from_secs(1),
            |requested| {
                calls.borrow_mut().push(requested);
                Ok(CandidateSource::Ann(scored(requested)))
            },
            |_| Ok(Vec::new()),
            || {
                exact_calls.set(exact_calls.get() + 1);
                Ok(scored(100))
            },
        )
        .unwrap();
        assert!(result.is_empty());
        assert_eq!(*calls.borrow(), vec![100, 200, 400]);
        assert_eq!(exact_calls.get(), 1);
    }

    #[test]
    fn exact_fallback_is_not_deepened_or_scanned_again() {
        let ann_calls = Cell::new(0);
        let exact_calls = Cell::new(0);
        let result = search_candidates(
            100,
            10,
            Instant::now() + Duration::from_secs(1),
            |_| {
                ann_calls.set(ann_calls.get() + 1);
                Ok(CandidateSource::ExactFallback(scored(100)))
            },
            |_| Ok(json_rows(3)),
            || {
                exact_calls.set(exact_calls.get() + 1);
                Ok(scored(100))
            },
        )
        .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(ann_calls.get(), 1);
        assert_eq!(exact_calls.get(), 0);
    }

    #[test]
    fn deep_pagination_skips_ann_and_uses_one_exact_scan() {
        let ann_calls = Cell::new(0);
        let exact_calls = Cell::new(0);
        let result = search_candidates(
            crate::clip_ann::ANN_MAX_CANDIDATES + 1,
            2_200,
            Instant::now() + Duration::from_secs(1),
            |_| {
                ann_calls.set(ann_calls.get() + 1);
                Ok(CandidateSource::Ann(Vec::new()))
            },
            |_| Ok(json_rows(5)),
            || {
                exact_calls.set(exact_calls.get() + 1);
                Ok(scored(5))
            },
        )
        .unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(ann_calls.get(), 0);
        assert_eq!(exact_calls.get(), 1);
    }

    #[test]
    fn offset_contract_keeps_ten_thousand_and_clamps_above_it() {
        assert_eq!(MAX_CLIP_OFFSET, 10_000);
        assert_eq!(bounded_clip_offset(9_999), 9_999);
        assert_eq!(bounded_clip_offset(10_000), 10_000);
        assert_eq!(bounded_clip_offset(10_001), 10_000);
    }
}
