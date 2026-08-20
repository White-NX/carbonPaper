//! M2.5 steps 4 and 6 — production Rust semantic-text retrieval.
//!
//! Replaces the Python/Chroma `nl_cluster_query` path with a Rust MiniLM query
//! encode plus an exact cosine scan over the migrated derived store, and
//! rehydrates the response metadata from SQLite. The shadow harness that proved
//! this path is gone (see the M2.5 retirement block); what remains is the read
//! path itself and its read-only diagnostics.
//!
//! **Both halves of the query now live here.** The non-reranked path retrieves
//! from the Rust MiniLM index, while `run_rust_reranked_nl_query` adds the Rust
//! cross-encoder used by Smart Cluster calibration. The scoring worker that
//! compares against the thresholds produced by that path is Rust-owned too.
//!
//! Coverage is the failure mode this path has to reason about: ranking an
//! incomplete corpus returns plausible results with screenshots silently
//! missing from them, which no user can detect. Two things still mean the local
//! store is a *prefix* of a corpus somebody else holds in full, and each keeps
//! its refusal:
//!
//! - The M2.4 migration has not finished. It commits page by page and its rows
//!   become query-visible immediately, so an interrupted run leaves a partial
//!   index. The once-per-revision sentinel gates this.
//! - The store is empty outright, which is the unmigrated machine.
//!
//! A capture-indexing backlog is reported rather than used to select another
//! query implementation. Rust is the only MiniLM encoder, so a screenshot
//! waiting in the ledger is missing from the derived store until indexing runs;
//! the diagnostic exposes that freshness cost and the count of exhausted jobs.
//!
//! A request that cannot be served from Rust records and returns the reason.
//! The reason is readable through `get_ml_semantic_status` and shown in
//! Settings → Advanced.

use crate::ml_protocol::MlSemanticModel;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{BackgroundScreenshotSummary, DerivedIndexKind, StorageState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// User-initiated search is foreground work: deadline-bound, never idle-gated.
/// Matches the deadline the shadow harness measured this path under.
///
/// The budget covers the wait for the semantic worker as well as the encode.
/// Idle-gated capture indexing shares that worker on a more generous budget.
/// Refusing after five seconds bounds a user-facing search when a background
/// request cannot yield promptly.
const QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(5);

/// Read-only semantic retrieval diagnostic surfaced on
/// `get_ml_semantic_status`.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticBackendStatus {
    /// Most recent Rust retrieval error, cleared by the next success.
    pub last_error: Option<String>,
    /// Rust retrieval failures since process start.
    pub failure_count: u64,
    /// Query-visible `semantic_text` vectors held locally. `None` when the
    /// database could not be read (locked, or not yet initialized).
    pub indexed_vectors: Option<u64>,
    /// Screenshots captured but not yet encoded, waiting for an idle window.
    /// Ordinary operation rather than a fault — reported so the freshness cost
    /// of idle gating is visible instead of merely felt.
    pub index_backlog: Option<u64>,
    /// Queued screenshots whose encode retry budget is spent. Unlike the
    /// backlog, nothing clears these on its own.
    pub index_stalled: Option<u64>,
    /// Age in seconds of the oldest screenshot still waiting to be encoded.
    /// `None` when nothing is waiting.
    pub index_backlog_age_secs: Option<i64>,
    /// A manual indexing run is executing right now.
    ///
    /// Filled by `get_ml_semantic_status`, which can reach the run state, rather
    /// than by [`backend_status`], which cannot and is also called from paths
    /// that must not depend on it. The settings dialog unmounts when the user
    /// switches tabs, and a run drains the whole queue rather than 128
    /// screenshots, so "I started this, walked away, and came back" is now an
    /// ordinary thing to do. This is how the reopened dialog finds out.
    pub index_run_active: bool,
}

#[derive(Debug, Default)]
struct BackendObservations {
    last_error: Option<String>,
    failure_count: u64,
}

static OBSERVATIONS: RwLock<Option<BackendObservations>> = RwLock::new(None);

/// Caches the one-way transition of the M2.4 migration sentinel.
///
/// The sentinel is written once per vector-space revision and never cleared, so
/// once it is observed the answer cannot change for the life of the process and
/// the database does not need to be consulted again. Before that it is re-read
/// so a partial migration is reported instead of queried as a complete corpus.
static MIGRATION_SETTLED: AtomicBool = AtomicBool::new(false);

/// Whether the sentinel-triggered M2.4 copy has finished for this vector space.
///
/// The migration commits page by page against a persisted cursor, and a
/// migrated row becomes query-visible as soon as its job row reaches
/// `completed` — there is no generation gate in front of the read path. So a
/// run that failed or was interrupted leaves a *partial* index: not empty, and
/// therefore not caught by the empty-index fallback, but missing whatever the
/// cursor never reached. Ranking that is the same silent-omission failure the
/// dual-write debt check exists to prevent, so it gets the same treatment.
fn migration_settled(storage: &StorageState) -> bool {
    if MIGRATION_SETTLED.load(Ordering::Relaxed) {
        return true;
    }
    let settled = storage
        .is_auto_migration_done(
            DerivedIndexKind::SemanticText,
            crate::minilm_migration::MINILM_VECTOR_SPACE_REVISION,
        )
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

pub fn backend_status(storage: Option<&StorageState>) -> SemanticBackendStatus {
    backend_status_impl(storage, true)
}

pub(crate) fn backend_status_without_vector_count(
    storage: Option<&StorageState>,
) -> SemanticBackendStatus {
    backend_status_impl(storage, false)
}

fn backend_status_impl(
    storage: Option<&StorageState>,
    include_vector_count: bool,
) -> SemanticBackendStatus {
    let guard = OBSERVATIONS
        .read()
        .unwrap_or_else(|error| error.into_inner());
    let (last_error, failure_count) = match guard.as_ref() {
        Some(entry) => (entry.last_error.clone(), entry.failure_count),
        None => (None, 0),
    };
    // One ledger read for all three depth figures. Absent when the database is
    // locked or uninitialized, which is honestly "not known" rather than zero.
    let backlog = storage.and_then(|storage| {
        storage
            .derived_index_backlog(
                DerivedIndexKind::SemanticText,
                crate::minilm_index::MAX_ATTEMPTS,
            )
            .ok()
    });
    SemanticBackendStatus {
        last_error,
        failure_count,
        indexed_vectors: include_vector_count
            .then(|| {
                storage.and_then(|storage| {
                    storage
                        .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
                        .ok()
                })
            })
            .flatten(),
        index_backlog: backlog.map(|backlog| backlog.claimable),
        index_stalled: backlog.map(|backlog| backlog.exhausted),
        index_backlog_age_secs: backlog.and_then(|backlog| backlog.oldest_claimable_age_secs),
        // Overwritten by the command; false is the honest answer for every
        // caller that cannot see the run state.
        index_run_active: false,
    }
}

/// Outcome of offering one NL query to the Rust retrieval path.
pub enum RustQueryOutcome {
    /// Rust served the query; the value is the complete command response.
    Served(serde_json::Value),
    /// Rust could not serve the request.
    Unavailable(String),
    /// The user stopped the query. Distinct from `Unavailable` because the caller
    /// must not be reported as a model or index failure.
    Cancelled,
}

/// Offer one non-reranked NL cluster query to the Rust semantic index.
///
/// Anything that prevents Rust from answering becomes `Unavailable` so the
/// caller can surface the specific model, migration, or index error.
pub async fn try_rust_nl_query(app: &AppHandle, query: &str, n_results: u32) -> RustQueryOutcome {
    try_rust_nl_query_inner(app, query, n_results, false).await
}

/// Offer one **reranked** NL cluster query to the Rust path — M2.5 step 6.
///
/// Every Smart Cluster calibration query is one of these; retrieval and
/// cross-encoder scoring are both executed by Rust.
///
/// It shares the same maintenance, migration-completeness, and empty-index
/// checks as the bi-encoder path.
pub async fn try_rust_reranked_nl_query(
    app: &AppHandle,
    query: &str,
    n_results: u32,
) -> RustQueryOutcome {
    try_rust_nl_query_inner(app, query, n_results, true).await
}

async fn try_rust_nl_query_inner(
    app: &AppHandle,
    query: &str,
    n_results: u32,
    rerank: bool,
) -> RustQueryOutcome {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        // Preserve the established empty-query contract without paying for an
        // encode that cannot rank anything.
        observe_served();
        return RustQueryOutcome::Served(success_response(Vec::new(), rerank));
    }
    // Announce the query before anything slow, so the background loops stop
    // submitting work to the single semantic worker while this one is on its
    // way to it. Taken here rather than immediately before the encode because
    // the migration check below is a blocking database read that can itself sit
    // behind the process-wide mutex, and every millisecond spent holding the
    // lease early is a millisecond the background pass is not starting a new
    // chunk. Dropped by the guard on every path out of this function.
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let _foreground = semantic.foreground_lease();

    // A migration is rewriting the derived store; reading it would race the
    // rewrite. Refuse explicitly and let the caller show the maintenance state.
    if crate::maintenance::is_active() {
        return unavailable("maintenance_in_progress");
    }
    // The M2.4 copy has not finished, so whatever is in the store is a prefix of
    // the corpus rather than the corpus. Read on a blocking thread: it takes the
    // process-wide database mutex, and it costs nothing once the sentinel has
    // been seen.
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let settled = tokio::task::spawn_blocking(move || migration_settled(&storage))
        .await
        .unwrap_or(false);
    if !settled {
        return unavailable("migration_incomplete");
    }

    let outcome = if rerank {
        run_rust_reranked_nl_query(app, trimmed, n_results).await
    } else {
        run_rust_nl_query(app, trimmed, n_results).await
    };
    match outcome {
        Ok(results) if results.is_empty() => {
            // No threshold is applied on this path, so an empty ranking means
            // an empty (or entirely invisible) index — typically a machine that
            // has not run the M2.4 migration. Serving zero results there would
            // silently break NL search for that user.
            unavailable("rust_index_empty")
        }
        Ok(results) => {
            observe_served();
            RustQueryOutcome::Served(success_response(results, rerank))
        }
        // A stop is not a model failure: the user simply stopped waiting.
        Err(error) if crate::rerank::is_cancelled(&error) => {
            tracing::info!("[SEMANTIC] reranked query stopped by the user: {error}");
            RustQueryOutcome::Cancelled
        }
        Err(error) => unavailable(&error),
    }
}

fn unavailable(reason: &str) -> RustQueryOutcome {
    observe_failure(reason);
    RustQueryOutcome::Unavailable(reason.to_string())
}

/// Build the `nl_cluster_query` success envelope. The field names remain
/// compatible with persisted callers; `backend` is always `rust` in the
/// current production path. `rerank_variant` is non-null exactly when
/// reranking ran, which is what `NlClusterView` reads to label the result.
fn success_response(results: Vec<serde_json::Value>, reranked: bool) -> serde_json::Value {
    serde_json::json!({
        "status": "success",
        "results": results,
        "reranked": reranked,
        "rerank_variant": if reranked {
            serde_json::Value::String(crate::rerank::RERANK_VARIANT.to_string())
        } else {
            serde_json::Value::Null
        },
        "backend": "rust",
    })
}

/// Retrieve with the bi-encoder, then re-score with the cross-encoder.
///
/// The same two stages Python's `query_by_text` performs in one call, with the
/// same over-fetch factor, the same 600-character document contract, and the
/// same final sort by descending rerank score. The difference is that the
/// candidates come from the Rust derived store rather than Chroma, which means
/// a candidate whose screenshot has since been deleted is dropped during
/// hydration instead of being scored and rendered from stale metadata.
///
/// This is the only path in the codebase a user waits on for minutes, so it is
/// also the only one that reports itself as it goes. The claim is taken before
/// retrieval rather than before the rerank, so the stop button is live for the
/// whole wait rather than only for the part that happens to be slow.
async fn run_rust_reranked_nl_query(
    app: &AppHandle,
    query: &str,
    n_results: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let watcher = app
        .state::<Arc<crate::rerank::RerankQueryState>>()
        .inner()
        .clone();
    let watcher = watcher.begin();

    let k = n_results.max(1) as usize;
    let fetch = k.saturating_mul(crate::rerank::RERANK_OVERFETCH as usize);
    watcher.report_phase(app, crate::rerank::RerankPhase::Retrieving);
    let mut candidates = run_rust_nl_query(app, query, fetch as u32).await?;
    if candidates.is_empty() {
        return Ok(candidates);
    }

    // The reranker sees more OCR than the index vector did, so the documents
    // are built from a second, longer read rather than from anything cached.
    let ids: Vec<i64> = candidates
        .iter()
        .filter_map(|row| row.get("screenshot_id").and_then(serde_json::Value::as_i64))
        .collect();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let ocr = tokio::task::spawn_blocking(move || {
        storage.get_ocr_text_prefixes_by_screenshot_ids_silent(
            &ids,
            crate::rerank::RERANK_OCR_SNIPPET_CHARS,
        )
    })
    .await
    .map_err(|error| format!("rerank_read_failed: {error}"))?
    .map_err(|error| format!("rerank_read_failed: {error}"))?;

    let documents: Vec<String> = candidates
        .iter()
        .map(|row| {
            let id = row
                .get("screenshot_id")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            crate::rerank::build_rerank_document(
                row.get("process_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                row.get("window_title")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                ocr.get(&id).map(String::as_str).unwrap_or(""),
            )
        })
        .collect();

    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    // Announced before the first chunk rather than inferred from a chunk that
    // takes unusually long, because the alternative is a progress bar that sits
    // at zero for half a minute and looks stuck.
    //
    // Unconditional, and it has to be. The retrieval above encoded the query
    // with MiniLM, which evicts the cross-encoder
    // (`semantic_runtime.rs::BACKGROUND_PASS_GUARD` states what that costs), so by
    // the time this line runs the model has been unloaded — by this query, not
    // by whatever else the machine was doing. Every reranked query pays the
    // 570 MB read, and a resident cross-encoder is not a state this path can
    // reach, so there is no fast path here worth testing for.
    watcher.report_phase(app, crate::rerank::RerankPhase::LoadingModel);
    let started = Instant::now();
    let scores = crate::rerank::rerank_documents(
        app,
        &semantic,
        query,
        &documents,
        crate::rerank::RerankBudget::interactive(),
        crate::rerank::RerankPriority::Foreground,
        Some(&watcher),
    )
    .await
    // A stop keeps its own marker: the caller distinguishes it from a failure
    // and must not answer it by re-running the query on Python.
    .map_err(|error| {
        if crate::rerank::is_cancelled(&error) {
            error
        } else {
            format!("rerank_failed: {error}")
        }
    })?;
    tracing::debug!(
        "[SEMANTIC] rust rerank scored {} document(s) in {:.1}ms",
        documents.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );

    for (row, score) in candidates.iter_mut().zip(scores) {
        if let serde_json::Value::Object(map) = row {
            map.insert(
                "rerank_score".to_string(),
                serde_json::json!(f64::from(score)),
            );
        }
    }
    // Raw logits, descending, exactly as Python sorts them. A row that somehow
    // has no score sorts last rather than being dropped, which mirrors
    // Python's `float("-inf")` default.
    candidates.sort_by(|left, right| {
        let score = |row: &serde_json::Value| {
            row.get("rerank_score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(f64::NEG_INFINITY)
        };
        score(right)
            .partial_cmp(&score(left))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(k);
    Ok(candidates)
}

async fn run_rust_nl_query(
    app: &AppHandle,
    query: &str,
    n_results: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();

    let started = Instant::now();
    let embedding = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::MinilmL12,
            vec![query.to_string()],
            QUERY_EMBED_TIMEOUT,
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

    // Scan and hydrate share one blocking thread. The scan takes the
    // process-wide, non-reentrant database mutex and hydration additionally
    // does per-row CNG decryption on its own read connection; hopping back onto
    // the async runtime between them would park a tokio worker mid-sequence on
    // the lock the reverse-IPC bridge also needs.
    let k = n_results.max(1) as usize;
    let scan_storage = storage.clone();
    let (results, scan_ms) = tokio::task::spawn_blocking(move || {
        let scan_started = Instant::now();
        let scored = scan_storage.semantic_text_topk(&query_vec, k)?;
        let ranked = hydrate(&scan_storage, &scored)?;
        Ok::<_, String>((ranked, scan_started.elapsed().as_secs_f64() * 1000.0))
    })
    .await
    .map_err(|error| format!("scan_task_failed: {error}"))??;

    tracing::debug!(
        "[SEMANTIC] rust nl query embed={embed_ms:.1}ms scan={scan_ms:.1}ms returned={}",
        results.len()
    );
    Ok(results)
}

/// Turn scored subject keys into the Python response rows.
///
/// One deliberate behavior difference, recorded as a bug fix rather than a
/// parity gap: Python reads process/title/timestamp/category out of Chroma
/// metadata, so a screenshot deleted after indexing is still rendered from a
/// stale copy. Rust reads them from SQLite and drops rows that no longer map to
/// an active screenshot. `semantic_text_topk` already re-checks visibility, so
/// in practice this only trims a row deleted mid-query.
fn hydrate(
    storage: &StorageState,
    scored: &[crate::storage::ScoredSubject],
) -> Result<Vec<serde_json::Value>, String> {
    if scored.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::with_capacity(scored.len());
    for subject in scored {
        let id = subject.subject_key.parse::<i64>().map_err(|error| {
            format!(
                "invalid_subject_key: '{}' is not a screenshot id ({error})",
                subject.subject_key
            )
        })?;
        ids.push(id);
    }

    let summaries: HashMap<i64, BackgroundScreenshotSummary> = storage
        .get_screenshot_summaries_by_ids_silent(&ids)
        .map_err(|error| format!("hydrate_failed: {error}"))?
        .into_iter()
        .map(|summary| (summary.id, summary))
        .collect();

    Ok(build_rows(scored, &ids, &summaries))
}

/// The response-shaping half of hydration, split out so the contract that must
/// match Python field-for-field is testable without a database.
fn build_rows(
    scored: &[crate::storage::ScoredSubject],
    ids: &[i64],
    summaries: &HashMap<i64, BackgroundScreenshotSummary>,
) -> Vec<serde_json::Value> {
    let mut rows = Vec::with_capacity(scored.len());
    for (subject, id) in scored.iter().zip(ids.iter().copied()) {
        let Some(summary) = summaries.get(&id) else {
            continue;
        };
        // Stored and query vectors are both L2-normalized, so the scan's dot
        // product is cosine similarity — the same quantity Python derives as
        // `1 - chroma_cosine_distance`. Reporting `distance` as its complement
        // keeps the pair internally consistent for either backend.
        let similarity = subject.score;
        rows.push(serde_json::json!({
            "screenshot_id": id,
            "similarity": similarity,
            "distance": 1.0_f32 - similarity,
            "timestamp": summary.timestamp.unwrap_or(0) as f64,
            "process_name": summary.process_name.clone().unwrap_or_default(),
            "window_title": summary.window_title.clone().unwrap_or_default(),
            "category": summary.category.clone().unwrap_or_default(),
            // The derived store mirrors the Chroma hot layer that M2.4 copied;
            // cold centroids stay in Python until Milestone 4.
            "layer": "hot",
        }));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_envelope_matches_the_python_non_reranked_contract() {
        let response = success_response(vec![serde_json::json!({"screenshot_id": 7})], false);
        assert_eq!(response["status"], "success");
        assert_eq!(response["reranked"], false);
        assert!(response["rerank_variant"].is_null());
        assert_eq!(response["backend"], "rust");
        assert_eq!(response["results"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_reranked_envelope_names_the_variant_that_produced_the_scores() {
        // `NlClusterView` reads `reranked` to decide whether to show scores at
        // all, and `rerank_variant` to label them. A reranked response that
        // left the variant null would render as an un-reranked one.
        let response = success_response(vec![serde_json::json!({"screenshot_id": 7})], true);
        assert_eq!(response["reranked"], true);
        assert_eq!(response["rerank_variant"], crate::rerank::RERANK_VARIANT);
        assert_eq!(response["backend"], "rust");
    }

    /// The observation slot and failure counter are process-global, and cargo
    /// runs these tests on parallel threads. Every test that touches either one
    /// serializes on this lock so the assertions describe their own writes.
    static OBSERVATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn failures_are_counted_and_success_clears_the_last_error() {
        let _guard = OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let before = backend_status(None).failure_count;
        observe_failure("rust_index_empty");
        let after = backend_status(None);
        assert_eq!(after.failure_count, before + 1);
        assert_eq!(after.last_error.as_deref(), Some("rust_index_empty"));
        observe_served();
        let success = backend_status(None);
        assert_eq!(success.failure_count, before + 1);
        assert_eq!(success.last_error, None);
    }

    #[test]
    fn an_indexing_backlog_is_reported_but_is_not_a_refusal() {
        let _guard = OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Step 5 removed the only refusal that a backlog could trigger. Rust is
        // now the sole MiniLM encoder and Chroma is fed from this store, so a
        // screenshot waiting in the ledger is missing from Python too; standing
        // down would cost the faster path and recover nothing. The depth stays
        // visible in the diagnostic, which is where the freshness cost of idle
        // gating shows up.
        let reasons = [
            "maintenance_in_progress",
            "migration_incomplete",
            "rust_index_empty",
        ];
        for reason in reasons {
            observe_failure(reason);
        }
        let status = backend_status(None);
        assert!(
            !status
                .last_error
                .as_deref()
                .unwrap_or("")
                .contains("index_incomplete"),
            "a backlog must not be spelled as a retrieval error"
        );
        // Nothing was read from a database, so the depths are unknown rather
        // than zero — reporting zero would claim a complete index.
        assert_eq!(status.index_backlog, None);
        assert_eq!(status.index_stalled, None);
        assert_eq!(status.index_backlog_age_secs, None);
    }

    fn scored(subject_key: &str, score: f32) -> crate::storage::ScoredSubject {
        crate::storage::ScoredSubject {
            subject_key: subject_key.to_string(),
            score,
        }
    }

    fn summary(id: i64) -> BackgroundScreenshotSummary {
        BackgroundScreenshotSummary {
            id,
            window_title: Some(format!("title-{id}")),
            process_name: Some(format!("proc-{id}.exe")),
            timestamp: Some(1_700_000_000 + id),
            category: Some("work".to_string()),
        }
    }

    #[test]
    fn rows_carry_exactly_the_python_result_fields_in_score_order() {
        let scanned = [scored("11", 0.9), scored("22", 0.5)];
        let ids = [11, 22];
        let summaries = HashMap::from([(11, summary(11)), (22, summary(22))]);
        let rows = build_rows(&scanned, &ids, &summaries);

        assert_eq!(rows.len(), 2);
        // Ranking order survives the map lookup, which is unordered.
        assert_eq!(rows[0]["screenshot_id"], 11);
        assert_eq!(rows[1]["screenshot_id"], 22);

        // The exact key set `query_by_text` returns for a non-reranked query.
        // `rerank_score` is deliberately absent: this path never reranks.
        let keys: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "category",
                "distance",
                "layer",
                "process_name",
                "screenshot_id",
                "similarity",
                "timestamp",
                "window_title",
            ]
        );
        assert_eq!(rows[0]["process_name"], "proc-11.exe");
        assert_eq!(rows[0]["window_title"], "title-11");
        assert_eq!(rows[0]["category"], "work");
        assert_eq!(rows[0]["layer"], "hot");
        assert_eq!(rows[0]["timestamp"], 1_700_000_011.0);
    }

    #[test]
    fn similarity_and_distance_stay_complements_like_the_chroma_pair() {
        let scanned = [scored("5", 0.75)];
        let rows = build_rows(&scanned, &[5], &HashMap::from([(5, summary(5))]));
        let similarity = rows[0]["similarity"].as_f64().unwrap();
        let distance = rows[0]["distance"].as_f64().unwrap();
        assert!((similarity + distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_subject_with_no_active_screenshot_is_dropped_not_rendered_stale() {
        // Python reads process/title out of Chroma metadata and so renders a
        // deleted screenshot from a stale copy. Rust reads SQLite and drops it.
        let scanned = [scored("1", 0.9), scored("2", 0.8), scored("3", 0.7)];
        let summaries = HashMap::from([(1, summary(1)), (3, summary(3))]);
        let rows = build_rows(&scanned, &[1, 2, 3], &summaries);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["screenshot_id"], 1);
        assert_eq!(rows[1]["screenshot_id"], 3);
    }

    #[test]
    fn missing_metadata_renders_as_empty_strings_like_the_python_defaults() {
        let scanned = [scored("9", 0.6)];
        let bare = BackgroundScreenshotSummary {
            id: 9,
            window_title: None,
            process_name: None,
            timestamp: None,
            category: None,
        };
        let rows = build_rows(&scanned, &[9], &HashMap::from([(9, bare)]));
        assert_eq!(rows[0]["process_name"], "");
        assert_eq!(rows[0]["window_title"], "");
        assert_eq!(rows[0]["category"], "");
        assert_eq!(rows[0]["timestamp"], 0.0);
    }
}
