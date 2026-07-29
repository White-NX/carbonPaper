//! M2.5 step 4 — production Rust semantic-text retrieval.
//!
//! Replaces the Python/Chroma `nl_cluster_query` bi-encoder path with a Rust
//! MiniLM query encode plus an exact cosine scan over the migrated derived
//! store, and rehydrates the response metadata from SQLite. The shadow harness
//! that proved this path is gone (see the M2.5 retirement block); what remains
//! is the read path itself and the observable fallback the enum rule requires.
//!
//! Two scope limits are deliberate and documented in the roadmap:
//!
//! - **Reranked queries stay on Python.** `enable_rerank = true` — which is
//!   every Smart Cluster calibration query — is not routed here. Python's
//!   `query_by_text` fuses retrieval and reranking into one call and the ML
//!   protocol has no standalone rerank operation, so serving the prefilter from
//!   Rust would need a Rust→Python rerank bridge living for exactly one
//!   release. Calibration moves with the reranker at M2.5 step 6.
//! - **Rust reads an index Python still writes.** Nothing on the Rust capture
//!   path encodes a new screenshot yet; `semantic_text` rows come from the M2.4
//!   migration and Python's reverse-IPC dual-write. M2.5 step 5 closes that.
//!
//! Because Python owns the writes, coverage is the failure mode that matters:
//! ranking an incomplete corpus returns plausible results with screenshots
//! silently missing from them, which no user can detect. Three things can leave
//! the local index short of the corpus, and each has its own refusal:
//!
//! - The M2.4 migration has not finished. It commits page by page and its rows
//!   become query-visible immediately, so an interrupted run leaves a partial —
//!   not empty — index. The once-per-revision sentinel gates this.
//! - Python wrote a vector to Chroma that never reached the Rust store. The
//!   dual-write is therefore durable on the Python side (a failed mirror is
//!   queued and retried, exactly as expired-row deletions already were), and
//!   Python reports the size of that outstanding queue both on every write and
//!   once at startup, when it loads the queue back off disk.
//! - The store is empty outright, which is the unmigrated machine.
//!
//! While any of those hold, this module refuses to serve, so an index known to
//! be behind Chroma sends the query to Python instead of ranking a corpus with
//! holes in it.
//!
//! Only mirrors that can still arrive count as debt. A row Rust rejects
//! permanently — its screenshot was deleted, and Chroma keeps documents for
//! deleted screenshots until they age out — is dropped by Python rather than
//! queued, because a row that is never coming would otherwise hold retrieval on
//! Python for as long as the queue survives, which is forever.
//!
//! Every refusal to serve from Rust falls back to Python and records why, so a
//! user whose index is empty or whose worker is broken sees results rather than
//! an empty page. The reason is readable through `get_ml_semantic_status` and
//! shown in Settings → Advanced.

use crate::ml_protocol::MlSemanticModel;
use crate::registry_config;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{BackgroundScreenshotSummary, DerivedIndexKind, StorageState};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// User-initiated search is foreground work: deadline-bound, never idle-gated.
/// Matches the deadline the shadow harness measured this path under.
const QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(5);

/// `rust_shadow` was retired with the step-4 cutover: nothing can enter shadow
/// mode anymore, so leaving the value selectable would offer a switch that does
/// nothing. `python` / `chroma` remain as the one-release observable rollback.
const RUNTIME_BACKENDS: &[&str] = &["python", "rust"];
const INDEX_BACKENDS: &[&str] = &["chroma", "dual", "rust"];

/// M2.5 step 4 flips both defaults. A cutover that left the default at `chroma`
/// would ship a switch nobody flips. The new default is safe on a machine whose
/// M2.4 migration has not run, because an empty Rust index falls back to Python
/// rather than returning nothing.
const DEFAULT_RUNTIME_BACKEND: &str = "rust";
const DEFAULT_INDEX_BACKEND: &str = "rust";

/// Selected semantic inference backend. Invalid/unset values resolve to the
/// current default observably.
pub fn semantic_runtime_backend() -> String {
    normalize_enum(
        registry_config::get_string("semantic_runtime"),
        RUNTIME_BACKENDS,
        DEFAULT_RUNTIME_BACKEND,
    )
}

/// Selected semantic index ownership backend. `rust` routes non-reranked NL
/// retrieval through this module; `chroma` and `dual` keep Python authoritative.
pub fn semantic_index_backend() -> String {
    normalize_enum(
        registry_config::get_string("semantic_index"),
        INDEX_BACKENDS,
        DEFAULT_INDEX_BACKEND,
    )
}

fn normalize_enum(value: Option<String>, allowed: &[&str], default: &str) -> String {
    match value {
        Some(value) if allowed.contains(&value.as_str()) => value,
        _ => default.to_string(),
    }
}

/// Read-only backend diagnostic surfaced on `get_ml_semantic_status`. This is
/// the whole of what the "observable fallback" rule requires — the shadow
/// settings card with its percentiles is gone.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticBackendStatus {
    /// Configured index backend (`chroma` | `dual` | `rust`).
    pub semantic_index: String,
    /// Configured inference backend (`python` | `rust`).
    pub semantic_runtime: String,
    /// Backend that served the most recent non-reranked NL query.
    pub last_query_backend: Option<String>,
    /// Why the last Rust attempt did not serve, when it did not.
    pub last_fallback_reason: Option<String>,
    /// Rust retrieval attempts that fell back since process start.
    pub fallback_count: u64,
    /// Vectors Python has written to Chroma but not yet mirrored into the Rust
    /// store, as last reported by Python's dual-write. Non-zero means the Rust
    /// index is knowably behind and retrieval is served from Python.
    pub pending_imports: u64,
    /// Query-visible `semantic_text` vectors held locally. `None` when the
    /// database could not be read (locked, or not yet initialized).
    pub indexed_vectors: Option<u64>,
}

#[derive(Debug, Default)]
struct BackendObservations {
    last_query_backend: Option<String>,
    last_fallback_reason: Option<String>,
    fallback_count: u64,
}

static OBSERVATIONS: RwLock<Option<BackendObservations>> = RwLock::new(None);

/// Outstanding dual-write debt as last reported by Python. Process-global
/// because the reverse-IPC handler that receives it carries no `AppHandle`.
/// Starting at zero is the honest default: before Python has said anything,
/// nothing is known to be missing.
static PENDING_IMPORTS: AtomicU64 = AtomicU64::new(0);

/// Record how many vectors Python still owes the Rust store. Called from the
/// reverse-IPC dual-write handler on every batch Python delivers.
pub fn note_pending_imports(pending: u64) {
    PENDING_IMPORTS.store(pending, Ordering::Relaxed);
}

fn pending_imports() -> u64 {
    PENDING_IMPORTS.load(Ordering::Relaxed)
}

/// Caches the one-way transition of the M2.4 migration sentinel.
///
/// The sentinel is written once per vector-space revision and never cleared, so
/// once it is observed the answer cannot change for the life of the process and
/// the database does not need to be consulted again. Before that it has to be
/// re-read, which is exactly the state in which the query is being handed to
/// Python anyway.
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
        .is_minilm_auto_migration_done(crate::minilm_migration::MINILM_VECTOR_SPACE_REVISION)
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

/// Record which backend actually produced a response. Deliberately separate
/// from [`observe_fallback`]: a fallback is decided before Python runs, and
/// stamping the serving backend at decision time would claim Python served a
/// query it may still fail.
fn observe_served(backend: &str) {
    with_observations(|entry| entry.last_query_backend = Some(backend.to_string()));
}

/// Record why a Rust attempt refused to serve. Says nothing about who serves
/// the query next.
fn observe_fallback(reason: &str) {
    with_observations(|entry| {
        entry.last_fallback_reason = Some(reason.to_string());
        entry.fallback_count = entry.fallback_count.saturating_add(1);
    });
}

/// Mark Python as the backend that served the current non-reranked query.
/// Called by `monitor_nl_cluster_query` after Python has actually answered.
pub fn observe_python_served() {
    observe_served("python");
}

pub fn backend_status(storage: Option<&StorageState>) -> SemanticBackendStatus {
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
    SemanticBackendStatus {
        semantic_index: semantic_index_backend(),
        semantic_runtime: semantic_runtime_backend(),
        last_query_backend,
        last_fallback_reason,
        fallback_count,
        pending_imports: pending_imports(),
        indexed_vectors: storage.and_then(|storage| {
            storage
                .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
                .ok()
        }),
    }
}

/// Outcome of offering one NL query to the Rust retrieval path.
pub enum RustQueryOutcome {
    /// Rust served the query; the value is the complete command response.
    Served(serde_json::Value),
    /// The configuration does not select Rust. Not a failure, not counted.
    NotSelected,
    /// Rust was selected but could not serve. The caller must use Python.
    FellBack(String),
}

/// Offer one non-reranked NL cluster query to the Rust semantic index.
///
/// Never returns an error: anything that prevents Rust from answering becomes
/// `FellBack` so the caller can serve the user from Python instead. The only
/// thing that would justify surfacing an error here is a broken auth session,
/// and Python rejects that identically one call later.
pub async fn try_rust_nl_query(app: &AppHandle, query: &str, n_results: u32) -> RustQueryOutcome {
    if semantic_index_backend() != "rust" {
        return RustQueryOutcome::NotSelected;
    }
    // Serving from the Rust store necessarily encodes the query with the Rust
    // MiniLM runtime — scoring Rust-held vectors against a Python-produced query
    // vector would mean an IPC round trip that defeats the point. So an explicit
    // `semantic_runtime = python` is honored as a refusal rather than silently
    // overridden; it is the second, independent rollback lever.
    if semantic_runtime_backend() != "rust" {
        return RustQueryOutcome::NotSelected;
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        // Python returns an empty list for a blank query; matching that here
        // avoids paying for an encode that cannot rank anything.
        observe_served("rust");
        return RustQueryOutcome::Served(success_response(Vec::new()));
    }
    // A migration is rewriting the derived store; reading it would race the
    // rewrite. Python is left to answer instead — it keeps serving from Chroma
    // throughout maintenance, because the migration only pauses the monitor's
    // capture loop rather than stopping the process, and command dispatch is
    // not maintenance-gated. So this refusal costs the user nothing.
    if crate::maintenance::is_active() {
        return fell_back("maintenance_in_progress");
    }
    // Python has written vectors to Chroma that never reached the Rust store.
    // Serving here would rank a corpus with known holes and show the user a
    // plausible page that is quietly missing screenshots — the one failure
    // this path cannot make observable after the fact. Python's queue drains
    // on the next capture or clustering pass, so this is self-clearing.
    let pending = pending_imports();
    if pending > 0 {
        return fell_back(&format!(
            "index_incomplete: {pending} vector(s) awaiting mirror"
        ));
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
        return fell_back("migration_incomplete");
    }

    match run_rust_nl_query(app, trimmed, n_results).await {
        Ok(results) if results.is_empty() => {
            // No threshold is applied on this path, so an empty ranking means
            // an empty (or entirely invisible) index — typically a machine that
            // has not run the M2.4 migration. Serving zero results there would
            // silently break NL search for that user.
            fell_back("rust_index_empty")
        }
        Ok(results) => {
            observe_served("rust");
            RustQueryOutcome::Served(success_response(results))
        }
        Err(error) => fell_back(&error),
    }
}

fn fell_back(reason: &str) -> RustQueryOutcome {
    observe_fallback(reason);
    RustQueryOutcome::FellBack(reason.to_string())
}

/// Mark a Python-served response with the backend that produced it. Additive:
/// existing consumers read `results` / `reranked` / `rerank_variant` and ignore
/// the rest.
pub fn tag_python_response(mut response: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut response {
        map.insert(
            "backend".to_string(),
            serde_json::Value::String("python".to_string()),
        );
    }
    response
}

/// Build the `nl_cluster_query` success envelope. Field-for-field the shape
/// Python returns for the non-reranked path (`clustering_commands.py`), so the
/// frontend and the contract tests cannot tell the backends apart.
fn success_response(results: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "status": "success",
        "results": results,
        "reranked": false,
        "rerank_variant": serde_json::Value::Null,
        "backend": "rust",
    })
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
    fn unknown_enum_values_fall_back_to_the_python_defaults() {
        assert_eq!(
            normalize_enum(Some("python".to_string()), RUNTIME_BACKENDS, "rust"),
            "python"
        );
        assert_eq!(
            normalize_enum(Some("nonsense".to_string()), RUNTIME_BACKENDS, "python"),
            "python"
        );
        assert_eq!(normalize_enum(None, INDEX_BACKENDS, "chroma"), "chroma");
    }

    #[test]
    fn step_four_defaults_select_rust_for_both_enums() {
        // An unset registry must land on the cut-over backend, otherwise the
        // switch ships inert. Both defaults must be legal enum values.
        assert_eq!(
            normalize_enum(None, RUNTIME_BACKENDS, DEFAULT_RUNTIME_BACKEND),
            "rust"
        );
        assert_eq!(
            normalize_enum(None, INDEX_BACKENDS, DEFAULT_INDEX_BACKEND),
            "rust"
        );
        assert!(RUNTIME_BACKENDS.contains(&DEFAULT_RUNTIME_BACKEND));
        assert!(INDEX_BACKENDS.contains(&DEFAULT_INDEX_BACKEND));
    }

    #[test]
    fn rollback_values_survive_as_selectable_enum_members() {
        // The one-release rollback the enum rule requires: either lever alone
        // must be able to return the user to the Python path.
        assert!(RUNTIME_BACKENDS.contains(&"python"));
        assert!(INDEX_BACKENDS.contains(&"chroma"));
    }

    #[test]
    fn retired_shadow_value_is_no_longer_selectable() {
        // `rust_shadow` was deleted with the harness. A beta registry left
        // holding the old value must resolve to the shipped default like any
        // other unrecognized string — not to something that silently behaves
        // like a third mode.
        assert!(!RUNTIME_BACKENDS.contains(&"rust_shadow"));
        assert_eq!(
            normalize_enum(
                Some("rust_shadow".to_string()),
                RUNTIME_BACKENDS,
                DEFAULT_RUNTIME_BACKEND
            ),
            DEFAULT_RUNTIME_BACKEND
        );
        assert_eq!(
            normalize_enum(Some("rust_shadow".to_string()), RUNTIME_BACKENDS, "python"),
            "python"
        );
    }

    #[test]
    fn success_envelope_matches_the_python_non_reranked_contract() {
        let response = success_response(vec![serde_json::json!({"screenshot_id": 7})]);
        assert_eq!(response["status"], "success");
        assert_eq!(response["reranked"], false);
        assert!(response["rerank_variant"].is_null());
        assert_eq!(response["backend"], "rust");
        assert_eq!(response["results"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn python_responses_are_tagged_without_disturbing_their_payload() {
        let tagged = tag_python_response(serde_json::json!({
            "status": "success",
            "results": [{"screenshot_id": 3}],
            "reranked": true,
            "rerank_variant": "q4f16",
        }));
        assert_eq!(tagged["backend"], "python");
        assert_eq!(tagged["reranked"], true);
        assert_eq!(tagged["rerank_variant"], "q4f16");
        assert_eq!(tagged["results"][0]["screenshot_id"], 3);
    }

    /// The observation slot and the debt counter are process-global, and cargo
    /// runs these tests on parallel threads. Every test that touches either one
    /// serializes on this lock so the assertions describe their own writes.
    static OBSERVATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn fallbacks_are_counted_and_explained_but_successes_are_not() {
        let _guard = OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let before = backend_status(None).fallback_count;
        observe_served("rust");
        assert_eq!(backend_status(None).fallback_count, before);
        assert_eq!(
            backend_status(None).last_query_backend.as_deref(),
            Some("rust")
        );

        observe_fallback("rust_index_empty");
        let after = backend_status(None);
        assert_eq!(after.fallback_count, before + 1);
        assert_eq!(
            after.last_fallback_reason.as_deref(),
            Some("rust_index_empty")
        );
    }

    #[test]
    fn a_fallback_does_not_claim_python_served_until_python_answers() {
        let _guard = OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // The refusal is decided before Python runs. Stamping the serving
        // backend there would report a successful Python answer even when the
        // monitor call that follows fails outright.
        observe_served("rust");
        observe_fallback("embed_failed: worker_stopped");
        assert_eq!(
            backend_status(None).last_query_backend.as_deref(),
            Some("rust"),
            "the refusal must not retroactively credit python"
        );
        observe_python_served();
        assert_eq!(
            backend_status(None).last_query_backend.as_deref(),
            Some("python")
        );
    }

    #[test]
    fn outstanding_dual_write_debt_is_reported_and_clears() {
        let _guard = OBSERVATION_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        note_pending_imports(0);
        assert_eq!(backend_status(None).pending_imports, 0);
        note_pending_imports(17);
        assert_eq!(backend_status(None).pending_imports, 17);
        // Python reports the remaining queue on every write, so a drained
        // queue restores the Rust path without a restart.
        note_pending_imports(0);
        assert_eq!(backend_status(None).pending_imports, 0);
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
