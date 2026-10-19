//! M2.5 step 9 — Rust-owned Chinese-CLIP text-to-image search.
//!
//! Provides text encoding and vector similarity queries against the Rust CLIP image index.

use crate::clip_contract::{
    clip_document_id, clip_memory_uri, CLIP_DIMENSIONS, CLIP_VECTOR_SPACE_REVISION,
};
use crate::ml_protocol::MlSemanticModel;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{BackgroundScreenshotSummary, DerivedIndexKind, ScoredSubject, StorageState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
pub const MAX_CLIP_OFFSET: u32 = 100_000;

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

/// Caches the one-way transition of the index sentinel, for the same reason
/// `semantic_query.rs` caches MiniLM's: it is written once per vector space and
/// never cleared, so once observed the answer cannot change for the life of
/// the process.
static MIGRATION_SETTLED: AtomicBool = AtomicBool::new(false);

/// Whether the index sentinel has settled for this vector space.
///
/// Settled at startup by `legacy_vector_discard.rs`; until then the store's
/// contents are unvouched-for and a query is refused rather than ranked.
/// `clip_index.rs::repair_scope` asks the same question for the write path.
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
    query: &'a str,
    limit: u32,
    offset: u32,
    process_names: &'a [String],
    /// Unix seconds, inclusive.
    start_time: Option<f64>,
    end_time: Option<f64>,
    include_ocr: bool,
}

impl<'a> ClipQueryRequest<'a> {
    pub fn new(
        query: &'a str,
        limit: Option<u64>,
        offset: Option<u64>,
        process_names: &'a [String],
        start_time: Option<f64>,
        end_time: Option<f64>,
    ) -> Self {
        Self {
            query,
            limit: limit.unwrap_or(20).clamp(1, u64::from(MAX_CLIP_RESULTS)) as u32,
            offset: offset.unwrap_or(0).min(u64::from(MAX_CLIP_OFFSET)) as u32,
            process_names,
            start_time,
            end_time,
            include_ocr: true,
        }
    }

    /// Image grids render metadata and thumbnails; text consumers retain the
    /// default OCR prefix and can use the same ranking and pagination path.
    pub(crate) fn with_ocr(mut self, include_ocr: bool) -> Self {
        self.include_ocr = include_ocr;
        self
    }
}

/// Shared search behavior; adapters own transport formats and privacy filtering.
pub async fn search(
    app: &AppHandle,
    request: ClipQueryRequest<'_>,
) -> Result<Vec<serde_json::Value>, String> {
    match try_rust_clip_query(app, request).await {
        ClipQueryOutcome::Served(results) => Ok(results),
        ClipQueryOutcome::Unavailable(reason) => Err(format!("CLIP search unavailable: {reason}")),
    }
}

/// Offer one natural-language image query to the Rust CLIP index.
///
/// Anything that stops Rust answering becomes `Unavailable` with a specific
/// model, migration, or index reason.
async fn try_rust_clip_query(app: &AppHandle, request: ClipQueryRequest<'_>) -> ClipQueryOutcome {
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

    let arm_started = Instant::now();
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        let _ = ann_state
            .wait_for_startup_arm(QUERY_ANN_ARM_WAIT.min(remaining))
            .await;
    }
    let arm_ms = arm_started.elapsed().as_secs_f64() * 1000.0;

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
    let include_ocr = request.include_ocr;

    // Keep synchronous database reads and CNG work off the async runtime. OCR
    // is presentation data: decrypt it only after filtering and pagination.
    let stage = Arc::new(Mutex::new("queued"));
    let scan_stage = Arc::clone(&stage);
    let scan_task = tokio::task::spawn_blocking(move || {
        if Instant::now() >= deadline {
            return Err("query_deadline_exceeded_before_scan".to_string());
        }
        let scan_started = Instant::now();
        *scan_stage.lock().unwrap_or_else(|error| error.into_inner()) = "database_wait";
        let _foreground_db = scan_storage.foreground_db_read();
        let ann_snapshot = match query_stage(&scan_stage, "ann_snapshot", || {
            ann_state.prepare_query(&scan_storage, &query_vec, Some(deadline))
        }) {
            Ok(snapshot) => snapshot,
            Err(error) if error.starts_with("query_deadline_exceeded") => return Err(error),
            Err(error) => {
                tracing::warn!("[CLIP:ANN] snapshot unavailable, using exact fallback: {error}");
                ann_state.disarm();
                None
            }
        };
        let mut candidate_filter =
            ClipCandidateFilter::new(target, &process_names, start_time, end_time, deadline);
        let filtered = search_candidates(
            fetch,
            target,
            deadline,
            |requested| {
                query_stage(&scan_stage, "ann", || {
                    app_ann_query(
                        &ann_state,
                        ann_snapshot.as_ref(),
                        &scan_storage,
                        &query_vec,
                        requested,
                        deadline,
                    )
                })
            },
            |scored| {
                candidate_filter.filter(scored, |hashes| {
                    query_stage(&scan_stage, "candidate_metadata", || {
                        if process_names.is_empty() {
                            scan_storage.clip_candidate_metadata(hashes, deadline)
                        } else {
                            load_candidate_summaries(&scan_storage, hashes, deadline)
                        }
                    })
                })
            },
            || {
                query_stage(&scan_stage, "exact", || {
                    app_exact_query(
                        ann_snapshot.as_ref(),
                        &scan_storage,
                        &query_vec,
                        fetch,
                        deadline,
                    )
                })
            },
        )?;
        let page = paginate(filtered, offset, limit);
        let page = resolve_page_metadata(page, |ids| {
            query_stage(&scan_stage, "metadata", || {
                scan_storage
                    .get_screenshot_summaries_by_ids_silent_with_deadline(ids, Some(deadline))
                    .map_err(|error| format!("hydrate_failed: {error}"))
            })
        })?;
        let results = hydrate_page(page, 0, limit, deadline, |ids| {
            query_stage(&scan_stage, "ocr", || {
                load_page_texts(&scan_storage, ids, deadline, include_ocr)
            })
        })?;
        if Instant::now() >= deadline {
            return Err("query_deadline_exceeded_after_hydrate".to_string());
        }
        Ok::<_, String>((results, scan_started.elapsed().as_secs_f64() * 1000.0))
    });
    let remaining = deadline.saturating_duration_since(Instant::now());
    let (results, scan_ms) = tokio::time::timeout(remaining, scan_task)
        .await
        .map_err(|_| {
            let stage = *stage.lock().unwrap_or_else(|error| error.into_inner());
            tracing::warn!(
                "[CLIP] query deadline exceeded stage={stage} embed={embed_ms:.1}ms arm={arm_ms:.1}ms total={:.1}ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            "query_deadline_exceeded".to_string()
        })?
        .map_err(|error| format!("scan_task_failed: {error}"))??;

    tracing::debug!(
        "[CLIP] rust nl query embed={embed_ms:.1}ms arm={arm_ms:.1}ms scan={scan_ms:.1}ms returned={}",
        results.len()
    );
    Ok(results)
}

fn query_stage<T>(
    progress: &Mutex<&'static str>,
    stage: &'static str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    *progress.lock().unwrap_or_else(|error| error.into_inner()) = stage;
    let started = Instant::now();
    let result = operation();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    if result.is_err() {
        // Also record cooperative deadline failures that finish just before
        // the async timeout fires. Do not copy private read errors into logs.
        tracing::warn!("[CLIP] query stage failed stage={stage} elapsed={elapsed_ms:.1}ms");
    } else {
        tracing::debug!("[CLIP] stage={stage} elapsed={elapsed_ms:.1}ms");
    }
    result
}

fn app_ann_query(
    state: &crate::clip_ann::ClipAnnState,
    snapshot: Option<&crate::clip_ann::ClipAnnSnapshot>,
    storage: &StorageState,
    query: &[f32],
    fetch: usize,
    deadline: Instant,
) -> Result<CandidateSource, String> {
    let Some(snapshot) = snapshot else {
        return Ok(CandidateSource::Unavailable);
    };
    match snapshot.query(fetch, Some(deadline)) {
        Ok(result) => {
            tracing::debug!(
                "[CLIP:ANN] query mode={} tail={} candidates={}",
                result.mode,
                result.tail_rows,
                result.candidates.len()
            );
            Ok(CandidateSource::Ann(result.candidates))
        }
        Err(error) if error.starts_with("query_deadline_exceeded") => Err(error),
        Err(error) => {
            tracing::warn!("[CLIP:ANN] query failed, using exact fallback: {error}");
            let fallback = app_exact_query(Some(snapshot), storage, query, fetch, deadline);
            state.disarm();
            fallback.map(CandidateSource::ExactFallback)
        }
    }
}

fn app_exact_query(
    snapshot: Option<&crate::clip_ann::ClipAnnSnapshot>,
    storage: &StorageState,
    query: &[f32],
    fetch: usize,
    deadline: Instant,
) -> Result<Vec<crate::storage::ScoredSubject>, String> {
    match snapshot
        .map(|snapshot| snapshot.exact(fetch, Some(deadline)))
        .transpose()?
    {
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

fn search_candidates<T>(
    fetch: usize,
    target: usize,
    deadline: Instant,
    mut ann_query: impl FnMut(usize) -> Result<CandidateSource, String>,
    mut filter: impl FnMut(Vec<crate::storage::ScoredSubject>) -> Result<Vec<T>, String>,
    exact_scan: impl FnOnce() -> Result<Vec<crate::storage::ScoredSubject>, String>,
) -> Result<Vec<T>, String> {
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

/// One scored image resolved to the screenshot that represents it.
#[derive(Debug)]
struct ClipHit {
    subject: ScoredSubject,
    summary: BackgroundScreenshotSummary,
    summary_complete: bool,
}

/// Only live for this query: later queries must observe current deletion,
/// metadata, and authentication state. Cache misses as well as hits so ANN
/// deepening and the exact fallback do not repeat private reads.
struct ClipCandidateFilter<'a> {
    target: usize,
    process_names: &'a [String],
    start_time: Option<f64>,
    end_time: Option<f64>,
    deadline: Instant,
    summaries: HashMap<String, Option<BackgroundScreenshotSummary>>,
}

impl<'a> ClipCandidateFilter<'a> {
    fn new(
        target: usize,
        process_names: &'a [String],
        start_time: Option<f64>,
        end_time: Option<f64>,
        deadline: Instant,
    ) -> Self {
        Self {
            target,
            process_names,
            start_time,
            end_time,
            deadline,
            summaries: HashMap::new(),
        }
    }

    fn filter(
        &mut self,
        scored: Vec<ScoredSubject>,
        mut load: impl FnMut(&[String]) -> Result<HashMap<String, BackgroundScreenshotSummary>, String>,
    ) -> Result<Vec<ClipHit>, String> {
        if Instant::now() >= self.deadline {
            return Err("query_deadline_exceeded_after_scan".to_string());
        }
        let above_floor: Vec<ScoredSubject> = scored
            .into_iter()
            .filter(|subject| subject.score >= CLIP_MIN_SIMILARITY)
            .collect();
        let mut hits = Vec::new();
        let mut cursor: usize = 0;
        while cursor < above_floor.len() && hits.len() < self.target {
            if Instant::now() >= self.deadline {
                return Err("query_deadline_exceeded_during_metadata".to_string());
            }
            // Stop once the filtered page is complete. With no process or
            // time filter this reads only offset + limit summaries instead
            // of every over-fetched candidate.
            let batch_len = (self.target - hits.len()).min(MAX_CLIP_RESULTS as usize);
            let end = cursor.saturating_add(batch_len).min(above_floor.len());
            let batch = &above_floor[cursor..end];
            let missing: Vec<String> = batch
                .iter()
                .filter(|subject| !self.summaries.contains_key(&subject.subject_key))
                .map(|subject| subject.subject_key.clone())
                .collect();
            if !missing.is_empty() {
                let mut loaded = load(&missing)?;
                for key in missing {
                    let summary = loaded.remove(&key);
                    self.summaries.insert(key, summary);
                }
            }
            let rows = batch
                .iter()
                .filter_map(|subject| {
                    self.summaries
                        .get(&subject.subject_key)
                        .and_then(Option::as_ref)
                        .map(|summary| ClipHit {
                            subject: subject.clone(),
                            summary: summary.clone(),
                            summary_complete: !self.process_names.is_empty(),
                        })
                })
                .collect();
            hits.extend(apply_filters(
                rows,
                self.process_names,
                self.start_time,
                self.end_time,
            ));
            cursor = end;
        }
        Ok(hits)
    }
}

fn load_candidate_summaries(
    storage: &StorageState,
    hashes: &[String],
    deadline: Instant,
) -> Result<HashMap<String, BackgroundScreenshotSummary>, String> {
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_before_metadata".to_string());
    }
    let mapped = storage.map_image_hashes_to_screenshot_ids(hashes)?;
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_metadata".to_string());
    }
    // Newest capture of each image, which is what `map_image_hashes_to_screenshot_ids`
    // orders for.
    let representative: HashMap<String, i64> = mapped
        .iter()
        .filter_map(|(hash, ids)| ids.first().map(|id| (hash.clone(), *id)))
        .collect();
    let ids: Vec<i64> = representative.values().copied().collect();

    let summaries: HashMap<i64, BackgroundScreenshotSummary> = storage
        .get_screenshot_summaries_by_ids_silent_with_deadline(&ids, Some(deadline))
        .map_err(|error| format!("hydrate_failed: {error}"))?
        .into_iter()
        .map(|summary| (summary.id, summary))
        .collect();
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_metadata".to_string());
    }
    Ok(representative
        .into_iter()
        .filter_map(|(hash, id)| summaries.get(&id).cloned().map(|summary| (hash, summary)))
        .collect())
}

fn resolve_page_metadata(
    page: Vec<ClipHit>,
    load: impl FnOnce(&[i64]) -> Result<Vec<BackgroundScreenshotSummary>, String>,
) -> Result<Vec<ClipHit>, String> {
    let missing: Vec<i64> = page
        .iter()
        .filter(|hit| !hit.summary_complete)
        .map(|hit| hit.summary.id)
        .collect();
    if missing.is_empty() {
        return Ok(page);
    }
    let summaries: HashMap<_, _> = load(&missing)?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    Ok(page
        .into_iter()
        .filter_map(|mut hit| {
            if !hit.summary_complete {
                hit.summary = summaries.get(&hit.summary.id)?.clone();
                hit.summary_complete = true;
            }
            Some(hit)
        })
        .collect())
}

fn hydrate_page(
    hits: Vec<ClipHit>,
    offset: usize,
    limit: usize,
    deadline: Instant,
    load_texts: impl FnOnce(&[i64]) -> Result<HashMap<i64, String>, String>,
) -> Result<Vec<serde_json::Value>, String> {
    let page = paginate(hits, offset, limit);
    if page.is_empty() {
        return Ok(Vec::new());
    }
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_before_ocr".to_string());
    }
    let ids: Vec<i64> = page.iter().map(|hit| hit.summary.id).collect();
    let texts = load_texts(&ids)?;
    if Instant::now() >= deadline {
        return Err("query_deadline_exceeded_during_ocr".to_string());
    }
    let mut rows = Vec::with_capacity(page.len());
    for ClipHit {
        subject, summary, ..
    } in page
    {
        let id = summary.id;
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

        rows.push(serde_json::json!({
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
        }));
    }
    Ok(rows)
}

fn load_page_texts(
    storage: &StorageState,
    ids: &[i64],
    deadline: Instant,
    include_ocr: bool,
) -> Result<HashMap<i64, String>, String> {
    if !include_ocr {
        return Ok(HashMap::new());
    }
    storage
        .get_ocr_text_prefixes_by_screenshot_ids_silent_with_deadline(
            ids,
            CLIP_OCR_SNIPPET_CHARS,
            Some(deadline),
        )
        .map_err(|error| format!("hydrate_failed: {error}"))
}

/// OCR characters returned to text consumers. The quick-search dropdown cuts
/// the text to 150 characters; image grids explicitly omit this prefix.
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
) -> Vec<ClipHit> {
    hits.into_iter()
        .filter(|hit| {
            if !process_names.is_empty()
                && !process_names
                    .iter()
                    .any(|name| name == hit.summary.process_name.as_deref().unwrap_or_default())
            {
                return false;
            }
            match created_seconds(hit.summary.timestamp) {
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
        .collect()
}

fn paginate<T>(rows: Vec<T>, offset: usize, limit: usize) -> Vec<T> {
    rows.into_iter().skip(offset).take(limit).collect()
}

/// The fixture uses the production SQL/CNG readers. `legacy` reproduces the
/// pre-scalability metadata reads for an unfiltered request, after the #233 fix.
#[cfg(test)]
pub(crate) fn benchmark_hydration(
    storage: &StorageState,
    scored: Vec<ScoredSubject>,
    offset: usize,
    limit: usize,
    include_ocr: bool,
    legacy: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut filter = ClipCandidateFilter::new(offset + limit, &[], None, None, deadline);
    let mut hits = filter.filter(scored, |keys| {
        if legacy {
            load_candidate_summaries(storage, keys, deadline)
        } else {
            storage.clip_candidate_metadata(keys, deadline)
        }
    })?;
    if legacy {
        for hit in &mut hits {
            hit.summary_complete = true;
        }
    }
    let page = resolve_page_metadata(paginate(hits, offset, limit), |ids| {
        storage
            .get_screenshot_summaries_by_ids_silent_with_deadline(ids, Some(deadline))
            .map_err(|error| error.to_string())
    })?;
    hydrate_page(page, 0, limit, deadline, |ids| {
        load_page_texts(storage, ids, deadline, include_ocr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn hit(process: &str, created_secs: Option<f64>) -> ClipHit {
        ClipHit {
            subject: ScoredSubject {
                subject_key: "image".to_string(),
                score: 0.75,
            },
            summary: BackgroundScreenshotSummary {
                id: 1,
                process_name: Some(process.to_string()),
                window_title: Some("window".to_string()),
                timestamp: created_secs.map(|value| value as i64),
                category: Some("category".to_string()),
            },
            summary_complete: true,
        }
    }

    fn summary_for_key(key: &str) -> BackgroundScreenshotSummary {
        let index: i64 = key.strip_prefix("subject-").unwrap().parse().unwrap();
        BackgroundScreenshotSummary {
            id: index + 1,
            process_name: Some("chrome.exe".to_string()),
            window_title: Some(format!("window-{index}")),
            timestamp: Some(1_785_888_000 + index),
            category: None,
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
    fn similarity_filter_excludes_low_scores_before_attempting_private_metadata_reads() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
            temp.path().to_path_buf(),
        ));
        let storage = StorageState::new(temp.path().to_path_buf(), credentials);
        for (score, requires_read) in [(0.31, false), (0.32, true), (0.33, true)] {
            let rows = vec![crate::storage::ScoredSubject {
                subject_key: "unreadable".into(),
                score,
            }];
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = ClipCandidateFilter::new(1, &[], None, None, deadline)
                .filter(rows, |hashes| {
                    load_candidate_summaries(&storage, hashes, deadline)
                });
            if requires_read {
                assert!(result.is_err(), "score {score} reaches the protected read");
            } else {
                assert!(result.unwrap().is_empty());
            }
        }
    }

    #[test]
    fn overfetch_and_offset_do_not_decrypt_ocr_outside_the_returned_page() {
        for (offset, limit) in [(0, 100), (100, 20)] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut filter = ClipCandidateFilter::new(offset + limit, &[], None, None, deadline);
            let mut summaries_read = Vec::new();
            let hits = filter
                .filter(scored(400), |keys| {
                    summaries_read.extend_from_slice(keys);
                    Ok(keys
                        .iter()
                        .map(|key| (key.clone(), summary_for_key(key)))
                        .collect())
                })
                .unwrap();
            assert_eq!(summaries_read.len(), offset + limit);
            let rows = hydrate_page(hits, offset, limit, deadline, |ids| {
                assert_eq!(
                    ids,
                    &((offset + 1) as i64..=(offset + limit) as i64).collect::<Vec<_>>()
                );
                Ok(ids.iter().map(|&id| (id, format!("ocr-{id}"))).collect())
            })
            .unwrap();
            assert_eq!(rows.len(), limit);
            assert_eq!(rows[0]["ocr_text"], format!("ocr-{}", offset + 1));
            assert_eq!(rows[0]["metadata"]["screenshot_id"], (offset + 1) as i64);
        }
    }

    #[test]
    fn ann_deepening_and_exact_fallback_read_each_summary_once() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let process_names = vec!["missing.exe".to_string()];
        let mut filter = ClipCandidateFilter::new(10, &process_names, None, None, deadline);
        let mut reads = HashMap::<String, usize>::new();
        let exact_calls = Cell::new(0);
        let hits = search_candidates(
            100,
            10,
            deadline,
            |requested| Ok(CandidateSource::Ann(scored(requested))),
            |rows| {
                filter.filter(rows, |keys| {
                    for key in keys {
                        *reads.entry(key.clone()).or_default() += 1;
                    }
                    // Missing/deleted screenshots are cached as well.
                    Ok(keys
                        .iter()
                        .filter(|key| *key != "subject-0")
                        .map(|key| (key.clone(), summary_for_key(key)))
                        .collect())
                })
            },
            || {
                exact_calls.set(exact_calls.get() + 1);
                Ok(scored(100))
            },
        )
        .unwrap();
        assert_eq!(exact_calls.get(), 1);
        assert_eq!(reads.len(), 400);
        assert!(reads.values().all(|count| *count == 1));
        assert!(hydrate_page(hits, 0, 10, deadline, |_| panic!(
            "no matching page needs OCR"
        ))
        .unwrap()
        .is_empty());
    }

    #[test]
    fn cached_metadata_preserves_new_scores_and_order_after_deepening() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut filter = ClipCandidateFilter::new(10, &[], None, None, deadline);
        filter
            .filter(scored(2), |keys| {
                Ok(keys
                    .iter()
                    .map(|key| (key.clone(), summary_for_key(key)))
                    .collect())
            })
            .unwrap();
        let revised = vec![
            ScoredSubject {
                subject_key: "subject-1".to_string(),
                score: 0.9,
            },
            ScoredSubject {
                subject_key: "subject-0".to_string(),
                score: 0.4,
            },
        ];
        let hits = filter
            .filter(revised.clone(), |_| panic!("metadata was already read"))
            .unwrap();
        assert_eq!(
            hits.into_iter().map(|hit| hit.subject).collect::<Vec<_>>(),
            revised
        );
    }

    #[test]
    fn missing_and_filtered_candidates_do_not_underfill_a_page() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let process_names = vec!["chrome.exe".to_string()];
        let mut filter =
            ClipCandidateFilter::new(3, &process_names, Some(100.0), Some(200.0), deadline);
        let hits = filter
            .filter(scored(10), |keys| {
                Ok(keys
                    .iter()
                    .filter_map(|key| {
                        let mut summary = summary_for_key(key);
                        match summary.id {
                            1 => return None,
                            2 => summary.timestamp = Some(50),
                            3 => {
                                summary.timestamp = Some(150);
                                summary.process_name = Some("code.exe".to_string());
                            }
                            4 => summary.timestamp = None,
                            5 => summary.timestamp = Some(100),
                            _ => summary.timestamp = Some(200),
                        }
                        Some((key.clone(), summary))
                    })
                    .collect())
            })
            .unwrap();
        assert_eq!(
            hits.iter().map(|hit| hit.summary.id).collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
    }

    #[test]
    fn paged_hydration_preserves_the_search_response_contract() {
        let rows = hydrate_page(
            vec![hit("chrome.exe", Some(1_785_888_000.0))],
            0,
            1,
            Instant::now() + Duration::from_secs(5),
            |ids| {
                assert_eq!(ids, &[1]);
                Ok(HashMap::from([(1, "recognized text".to_string())]))
            },
        )
        .unwrap();
        let created_at = format_created_at(1_785_888_000.0);
        assert_eq!(
            rows,
            vec![serde_json::json!({
                "id": clip_document_id("image"),
                "image_path": clip_memory_uri("image"),
                "metadata": {
                    "image_path": clip_memory_uri("image"),
                    "screenshot_id": 1,
                    "process_name": "chrome.exe",
                    "window_title": "window",
                    "category": "category",
                    "timestamp": 1_785_888_000.0,
                    "created_at": created_at,
                    "screenshot_created_at": created_at,
                },
                "ocr_text": "recognized text",
                "distance": 0.25,
                "similarity": 0.75,
                "screenshot_created_at": created_at,
            })]
        );
    }

    #[test]
    fn expired_queries_do_not_start_metadata_or_ocr_reads() {
        let deadline = Instant::now();
        let mut filter = ClipCandidateFilter::new(1, &[], None, None, deadline);
        assert_eq!(
            filter
                .filter(scored(1), |_| panic!("expired metadata read"))
                .unwrap_err(),
            "query_deadline_exceeded_after_scan"
        );
        assert_eq!(
            hydrate_page(vec![hit("chrome.exe", None)], 0, 1, deadline, |_| panic!(
                "expired OCR read"
            ))
            .unwrap_err(),
            "query_deadline_exceeded_before_ocr"
        );
    }

    #[test]
    fn metadata_decryption_is_limited_to_the_page_after_skipping_old_results() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut filter = ClipCandidateFilter::new(1_100, &[], None, None, deadline);
        let hits = filter
            .filter(scored(2_200), |keys| {
                Ok(keys
                    .iter()
                    .map(|key| {
                        let mut summary = summary_for_key(key);
                        summary.window_title = None;
                        (key.clone(), summary)
                    })
                    .collect())
            })
            .unwrap();
        let mut private_reads = 0;
        let page = resolve_page_metadata(paginate(hits, 1_000, 100), |ids| {
            private_reads += ids.len();
            Ok(ids
                .iter()
                .map(|id| summary_for_key(&format!("subject-{}", id - 1)))
                .collect())
        })
        .unwrap();
        assert_eq!(private_reads, 100);
        assert_eq!(page[0].summary.id, 1_001);
        assert!(page
            .iter()
            .all(|hit| hit.summary_complete && hit.summary.window_title.is_some()));
    }

    #[test]
    fn decrypted_process_filters_do_not_repeat_their_metadata_reads() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let processes = vec!["chrome.exe".to_string()];
        let mut filter = ClipCandidateFilter::new(2, &processes, None, None, deadline);
        let hits = filter
            .filter(scored(3), |keys| {
                Ok(keys
                    .iter()
                    .map(|key| (key.clone(), summary_for_key(key)))
                    .collect())
            })
            .unwrap();
        assert_eq!(
            resolve_page_metadata(hits, |_| panic!("already decrypted for filtering"))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn image_grids_skip_ocr_reads_and_other_consumers_keep_the_default() {
        let request = ClipQueryRequest::new("game", None, None, &[], None, None);
        assert!(request.include_ocr);
        assert!(!request.with_ocr(false).include_ocr);
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
            temp.path().to_path_buf(),
        ));
        let storage = StorageState::new(temp.path().to_path_buf(), credentials);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(load_page_texts(&storage, &[1], deadline, false)
            .unwrap()
            .is_empty());
        assert!(load_page_texts(&storage, &[1], deadline, true).is_err());
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
    fn ann_expansion_is_bounded_and_exhaustion_runs_exactly_one_fallback() {
        for fetch in [100, 2_000, crate::clip_ann::ANN_MAX_CANDIDATES] {
            let calls = RefCell::new(Vec::new());
            let exact_calls = Cell::new(0);
            let result = search_candidates(
                fetch,
                10,
                Instant::now() + Duration::from_secs(5),
                |requested| {
                    calls.borrow_mut().push(requested);
                    Ok(CandidateSource::Ann(scored(requested)))
                },
                |_| Ok(Vec::<serde_json::Value>::new()),
                || {
                    exact_calls.set(exact_calls.get() + 1);
                    Ok(scored(fetch))
                },
            )
            .unwrap();
            assert!(result.is_empty());
            let calls = calls.into_inner();
            assert_eq!(calls.first(), Some(&fetch));
            assert!(calls.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(calls
                .iter()
                .all(|n| *n <= crate::clip_ann::ANN_MAX_CANDIDATES));
            assert_eq!(exact_calls.get(), 1);
        }
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
    fn offset_contract_matches_the_tool_catalog_without_silently_repeating_pages() {
        assert_eq!(MAX_CLIP_OFFSET, 100_000);
        assert_eq!(bounded_clip_offset(9_999), 9_999);
        assert_eq!(bounded_clip_offset(10_000), 10_000);
        assert_eq!(bounded_clip_offset(10_001), 10_001);
        assert_eq!(bounded_clip_offset(100_001), 100_000);
        let defaults = ClipQueryRequest::new("query", None, None, &[], None, None);
        assert_eq!((defaults.limit, defaults.offset), (20, 0));
        let bounded =
            ClipQueryRequest::new("query", Some(u64::MAX), Some(u64::MAX), &[], None, None);
        assert_eq!(
            (bounded.limit, bounded.offset),
            (MAX_CLIP_RESULTS, MAX_CLIP_OFFSET)
        );
    }
}
