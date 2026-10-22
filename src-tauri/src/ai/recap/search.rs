//! Cross-date retrieval over corrected activities. Embeddings are encrypted
//! recap payloads; the resident projection is fenced by source/privacy/session
//! revisions and discarded on lock. No screenshot hot-layer retention applies.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use zeroize::Zeroize;

use super::activity_contract::{member_directory, ActivityEvidence};
use super::search_pages::SearchPages;
use super::{runner::privacy_hash, types::*, with_current_day, RecapIndexProgress};
use crate::ai::provider::Cancellation;
use crate::credential_manager::CredentialManagerState;
use crate::minilm_contract::{
    validate_minilm_vector, MINILM_DIMENSIONS, MINILM_MODEL_ID, MINILM_VECTOR_SPACE_REVISION,
};
use crate::ml_protocol::{MlErrorKind, MlSemanticModel};
use crate::semantic_runtime::{
    SemanticEmbeddingResult, SemanticRuntimeState, SemanticRuntimeStatus, SemanticTimeouts,
};
use crate::sensitive_filter::SensitiveFilterState;
use crate::storage::{search_plan::QueryPlan, search_rank, SearchMatchEvidence, StorageState};
use crate::task_error::TaskError;

const INDEX_VERSION: u32 = 2;
const ENCODE_BATCH: usize = 4;
const SAVE_SHARD: usize = 64;
const QUERY_EMBED_TIMEOUT: SemanticTimeouts = SemanticTimeouts {
    warm: Duration::from_secs(5),
    // Debug worker measurements took 5.3-5.7 seconds for model verification,
    // tokenizer setup and ONNX session construction. Allow that one-time cost
    // to complete while keeping warm requests on their original deadline.
    cold: Duration::from_secs(20),
};
const INDEX_EMBED_TIMEOUT: Duration = Duration::from_secs(60);
// Gate the semantic channel before ranking and candidate caps. Verified keyword
// hits remain eligible independently and receive no boost from weak vectors.
const MIN_SEMANTIC_SCORE: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Keyword,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecapSearchQuery {
    pub query: String,
    pub mode: SearchMode,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub include_details: bool,
    pub offset: usize,
    pub limit: usize,
    pub ranking_revision: Option<String>,
    pub process_names: Vec<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl Default for RecapSearchQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Hybrid,
            start_date: None,
            end_date: None,
            include_details: true,
            offset: 0,
            limit: 20,
            ranking_revision: None,
            process_names: Vec::new(),
            start_ms: None,
            end_ms: None,
        }
    }
}

impl RecapSearchQuery {
    fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty()
            || self.query.chars().count() > 256
            || !(1..=50).contains(&self.limit)
            || self.offset > 100_000
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        if self
            .ranking_revision
            .as_ref()
            .is_some_and(|r| r.len() != 64 || !r.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        for date in [&self.start_date, &self.end_date].into_iter().flatten() {
            crate::calendar::parse_date(date)?;
        }
        if matches!((&self.start_date, &self.end_date), (Some(a), Some(b)) if a > b) {
            return Err("RECAP_INVALID_DATE".into());
        }
        if matches!((self.start_ms, self.end_ms), (Some(a), Some(b)) if a > b) {
            return Err("RECAP_INVALID_DATE".into());
        }
        if self.process_names.len() > 100
            || self
                .process_names
                .iter()
                .any(|name| name.trim().is_empty() || name.len() > 1024)
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        Ok(())
    }

    fn includes(&self, date: &str) -> bool {
        self.start_date.as_deref().is_none_or(|start| date >= start)
            && self.end_date.as_deref().is_none_or(|end| date <= end)
    }

    fn includes_activity(&self, activity: &RecapActivity) -> bool {
        self.start_ms.is_none_or(|start| activity.end_ms >= start)
            && self.end_ms.is_none_or(|end| activity.start_ms <= end)
            && (self.process_names.is_empty()
                || activity.sources.iter().any(|source| {
                    self.process_names
                        .iter()
                        .any(|name| source.process_name.eq_ignore_ascii_case(name))
                }))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecapSearchHit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_offset: Option<usize>,
    pub date: String,
    pub batch_start_ms: i64,
    pub activity_id: String,
    pub task_id: String,
    pub task_title: String,
    pub text: String,
    pub details: Vec<RecapDetail>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub sources: Vec<SourceRef>,
    #[serde(flatten)]
    pub evidence: ActivityEvidence,
    pub keyword_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub keyword_score: Option<f64>,
    pub semantic_score: Option<f64>,
    pub match_evidence: Option<SearchMatchEvidence>,
}

#[derive(Clone, Serialize)]
pub struct RecapSearchResult {
    pub revision: String,
    pub ranking_revision: String,
    pub items: Vec<RecapSearchHit>,
    pub total: usize,
    pub complete: bool,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    pub mode: SearchMode,
    pub semantic_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_error: Option<String>,
    pub indexed_chunks: Option<usize>,
    pub total_chunks: Option<usize>,
    pub coverage: Option<SearchCoverage>,
    pub execution: SearchExecution,
    pub index_progress: RecapIndexProgress,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CoverageCounts {
    pub status: &'static str,
    pub total_chunks: usize,
    pub indexed_chunks: usize,
    pub missing_chunks: usize,
    pub activities: usize,
    pub fully_indexed_activities: usize,
    pub partially_indexed_activities: usize,
    pub unindexed_activities: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DateCoverage {
    pub date: String,
    #[serde(flatten)]
    pub counts: CoverageCounts,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SearchCoverage {
    pub global: CoverageCounts,
    pub query_scope: CoverageCounts,
    pub dates: Vec<DateCoverage>,
    pub missing_dates: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SearchExecution {
    pub requested_mode: SearchMode,
    pub keyword_executed: bool,
    pub keyword_matches: usize,
    pub keyword_complete: bool,
    pub semantic_executed: bool,
    pub semantic_matches: usize,
    pub semantic_candidates: usize,
    pub semantic_candidate_limit: usize,
    pub semantic_candidates_truncated: bool,
    pub fallback_reason: Option<&'static str>,
}

fn coverage_counts(
    documents: &[&Document],
    vectors: &HashMap<String, Vec<f32>>,
    details: bool,
) -> CoverageCounts {
    let mut counts = CoverageCounts {
        activities: documents.len(),
        ..Default::default()
    };
    let mut keys = HashSet::new();
    for document in documents {
        let chunks: Vec<_> = document
            .chunks
            .iter()
            .filter(|chunk| details || !chunk.detail)
            .collect();
        if chunks.is_empty() {
            continue;
        }
        let indexed = chunks
            .iter()
            .filter(|chunk| vectors.contains_key(&chunk.key))
            .count();
        if indexed == 0 {
            counts.unindexed_activities += 1;
        } else if indexed == chunks.len() {
            counts.fully_indexed_activities += 1;
        } else {
            counts.partially_indexed_activities += 1;
        }
        keys.extend(chunks.into_iter().map(|chunk| chunk.key.as_str()));
    }
    counts.total_chunks = keys.len();
    counts.indexed_chunks = keys
        .iter()
        .filter(|key| vectors.contains_key(**key))
        .count();
    counts.missing_chunks = counts.total_chunks - counts.indexed_chunks;
    counts.status = if counts.total_chunks == 0 {
        "empty"
    } else if counts.indexed_chunks == 0 {
        "not_prepared"
    } else if counts.missing_chunks > 0 {
        "partial"
    } else {
        "ready"
    };
    counts
}

fn search_coverage(
    corpus: &Corpus,
    query: &RecapSearchQuery,
    vectors: &HashMap<String, Vec<f32>>,
) -> SearchCoverage {
    let global: Vec<_> = corpus.documents.iter().collect();
    let scoped: Vec<_> = global
        .iter()
        .copied()
        .filter(|document| {
            query.includes(&document.date) && query.includes_activity(&document.activity)
        })
        .collect();
    let mut dates = BTreeMap::<&str, Vec<&Document>>::new();
    for document in &scoped {
        dates.entry(&document.date).or_default().push(document);
    }
    let dates: Vec<_> = dates
        .into_iter()
        .map(|(date, documents)| DateCoverage {
            date: date.into(),
            counts: coverage_counts(&documents, vectors, query.include_details),
        })
        .collect();
    let missing_dates = dates
        .iter()
        .filter(|coverage| coverage.counts.missing_chunks > 0)
        .map(|coverage| coverage.date.clone())
        .collect();
    SearchCoverage {
        global: coverage_counts(&global, vectors, true),
        query_scope: coverage_counts(&scoped, vectors, query.include_details),
        dates,
        missing_dates,
    }
}

#[derive(Clone, Default, Serialize)]
pub struct RecapSearchStatus {
    pub revision: String,
    pub running: bool,
    pub indexed_chunks: usize,
    pub total_chunks: usize,
    pub activities: usize,
    pub dates: usize,
    pub error: Option<String>,
}

#[derive(Default)]
struct IndexRun {
    progress: RecapIndexProgress,
    counts_identity: Option<Identity>,
    cancel: Option<Arc<Cancellation>>,
    error: Option<String>,
    stop_reason: Option<&'static str>,
}

#[derive(Default)]
pub struct RecapSearchRuntime {
    serial: AtomicU64,
    loading: Mutex<()>,
    keyword_loading: Mutex<()>,
    keyword_identity: Mutex<Option<Identity>>,
    corpus: Mutex<Option<Arc<Corpus>>>,
    index: Mutex<IndexRun>,
    pages: Mutex<SearchPages<RecapSearchResult>>,
    pub(super) unified_pages: Mutex<SearchPages<super::unified::UnifiedSearchResult>>,
}

impl RecapSearchRuntime {
    pub fn progress(&self) -> RecapIndexProgress {
        self.index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .progress
    }

    fn update_progress(&self, update: impl FnOnce(&mut IndexRun)) -> Option<RecapIndexProgress> {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        let previous = index.progress;
        update(&mut index);
        if index.progress == previous {
            return None;
        }
        index.progress.run.revision = previous.run.revision + 1;
        Some(index.progress)
    }

    pub(crate) fn sync_scheduler_progress(
        &self,
        app: &tauri::AppHandle,
        task: Option<&crate::storage::BackgroundTaskState>,
        expected_revision: u64,
    ) -> RecapIndexProgress {
        let wait_reason = task
            .filter(|task| matches!(task.status.as_str(), "queued" | "running" | "retry_wait"))
            .and_then(|_| {
                crate::background_scheduler::task_wait_reason(
                    app,
                    crate::background_scheduler::BackgroundTaskKind::RecapIndex,
                )
            });
        let progress = self.update_progress(|index| {
            // A ledger read queued behind storage must not overwrite a newer
            // count, checkpoint or run transition with its old task state.
            if index.progress.run.revision != expected_revision || index.cancel.is_some() {
                return;
            }
            index
                .progress
                .sync_task(task, wait_reason, chrono::Utc::now().timestamp_millis());
        });
        emit_index_progress(app, progress);
        self.progress()
    }

    fn publish_counts(&self, app: &tauri::AppHandle, corpus: &Arc<Corpus>, committed: bool) {
        let progress = {
            let current = self.corpus.lock().unwrap_or_else(|e| e.into_inner());
            if !current.as_ref().is_some_and(|c| Arc::ptr_eq(c, corpus)) {
                return;
            }
            // Read and publish under the same locks so a concurrent status
            // scan cannot restore the count from before a committed batch.
            let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
            self.update_progress(|index| {
                if index.counts_identity.as_ref() != Some(&corpus.identity) {
                    index.counts_identity = Some(corpus.identity.clone());
                    index.progress.run.run_id += 1;
                }
                index.progress.total_known = true;
                index.progress.run.total = corpus.total_chunks as u64;
                index.progress.run.processed = vectors.len() as u64;
                index.progress.run.indexed = vectors.len() as u64;
                if committed {
                    index.progress.last_progress_at_ms =
                        Some(chrono::Utc::now().timestamp_millis());
                }
                if index.cancel.is_none() && index.progress.complete() {
                    index.progress.set_phase("idle");
                }
            })
        };
        emit_index_progress(app, progress);
    }

    pub fn cancel(&self) -> bool {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = &index.cancel {
            cancel.cancel();
            if !index.progress.run.stopping {
                index.progress.run.stopping = true;
                index.progress.set_phase("stopping");
                index.progress.run.revision += 1;
            }
            true
        } else {
            false
        }
    }

    pub fn clear(&self) {
        self.serial.fetch_add(1, Ordering::SeqCst);
        self.cancel();
        *self.corpus.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .keyword_identity
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.index.lock().unwrap_or_else(|e| e.into_inner()).error = None;
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.unified_pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

fn emit_index_progress(app: &tauri::AppHandle, progress: Option<RecapIndexProgress>) {
    if let Some(progress) = progress {
        let _ = app.emit("recap-search-progress", progress);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Identity {
    generation: u64,
    epoch: i64,
    privacy: String,
    serial: u64,
    session: Option<Instant>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SearchContext {
    pub generation: u64,
    pub privacy: String,
    pub serial: u64,
    pub session: Option<Instant>,
}

pub(super) fn search_context(app: &tauri::AppHandle) -> Result<SearchContext, String> {
    let credentials = app.state::<Arc<CredentialManagerState>>();
    if !credentials.is_session_valid() {
        app.state::<RecapSearchRuntime>().clear();
        return Err("AUTH_REQUIRED".into());
    }
    if crate::maintenance::is_active() {
        return Err("MAINTENANCE_IN_PROGRESS".into());
    }
    Ok(SearchContext {
        generation: app.state::<Arc<StorageState>>().db_generation(),
        privacy: privacy_hash(app)?,
        serial: app
            .state::<RecapSearchRuntime>()
            .serial
            .load(Ordering::SeqCst),
        session: credentials.session_marker(),
    })
}

impl SearchContext {
    pub fn token(&self) -> String {
        digest(&format!(
            "{}:{}:{}:{:?}",
            self.generation, self.privacy, self.serial, self.session
        ))
    }

    pub fn check(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let current = search_context(app)?;
        if self.session != current.session || self.serial != current.serial {
            return Err("AUTH_REQUIRED".into());
        }
        if self != &current {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(())
    }
}

pub(super) fn identity(app: &tauri::AppHandle) -> Result<Identity, String> {
    if !app
        .state::<Arc<CredentialManagerState>>()
        .is_session_valid()
    {
        app.state::<RecapSearchRuntime>().clear();
        return Err("AUTH_REQUIRED".into());
    }
    if crate::maintenance::is_active() {
        return Err("MAINTENANCE_IN_PROGRESS".into());
    }
    let storage = app.state::<Arc<StorageState>>();
    Ok(Identity {
        generation: storage.db_generation(),
        epoch: storage.recap_search_epoch()?,
        privacy: privacy_hash(app)?,
        serial: app
            .state::<RecapSearchRuntime>()
            .serial
            .load(Ordering::SeqCst),
        session: app.state::<Arc<CredentialManagerState>>().session_marker(),
    })
}

pub(crate) fn semantic_enabled(app: &tauri::AppHandle) -> Result<bool, String> {
    let storage = app.state::<Arc<StorageState>>();
    Ok(storage
        .recap_read::<RecapSettings>("settings", "settings", None)?
        .unwrap_or_default()
        .semantic_search_enabled)
}

/// Changes enqueue automatic work; scheduler reconciliation also catches source
/// and privacy changes that arrive without a recap UI event.
pub(crate) fn schedule_index(app: &tauri::AppHandle) {
    let keyword_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = prepare_keywords(&keyword_app) {
            tracing::debug!("[RECAP:SEARCH] Keyword index update deferred: {error}");
        }
    });
    if semantic_enabled(app).is_ok_and(|enabled| enabled) {
        if let Some(scheduler) =
            app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        {
            if let Err(error) = scheduler.enqueue(
                app,
                crate::background_scheduler::BackgroundTaskKind::RecapIndex,
                false,
            ) {
                tracing::debug!("[RECAP:SEARCH] Failed to queue index: {error}");
            }
        }
    }
}

impl Identity {
    fn context(&self) -> SearchContext {
        SearchContext {
            generation: self.generation,
            privacy: self.privacy.clone(),
            serial: self.serial,
            session: self.session,
        }
    }
    pub(super) fn token(&self) -> String {
        digest(&format!(
            "{}:{}:{}:{}:{:?}",
            self.generation, self.epoch, self.privacy, self.serial, self.session
        ))
    }
    pub(super) fn check(&self, app: &tauri::AppHandle) -> Result<(), String> {
        if self != &identity(app)? {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(())
    }
}

struct Chunk {
    key: String,
    text: String,
    detail: bool,
}
struct Document {
    date: String,
    revision: i64,
    batch_start_ms: i64,
    activity: RecapActivity,
    chunks: Vec<Chunk>,
    evidence: ActivityEvidence,
}

impl Document {
    fn new(date: String, revision: i64, batch_start_ms: i64, activity: RecapActivity) -> Self {
        Self {
            date,
            revision,
            batch_start_ms,
            chunks: chunks(&activity),
            evidence: ActivityEvidence::project(
                &activity,
                &super::activity_contract::MemberDirectory::default(),
            ),
            activity,
        }
    }
}
struct Corpus {
    identity: Identity,
    documents: Vec<Document>,
    total_chunks: usize,
    dates: usize,
    vectors: Mutex<HashMap<String, Vec<f32>>>,
}

impl Drop for Corpus {
    fn drop(&mut self) {
        for document in &mut self.documents {
            document.date.zeroize();
            let activity = &mut document.activity;
            activity.task_title.zeroize();
            activity.text.zeroize();
            activity.member_ids.zeroize();
            document.evidence.member_screenshot_ids.zeroize();
            for detail in &mut activity.details {
                detail.text.zeroize();
            }
            for source in &mut activity.sources {
                source.process_name.zeroize();
                source.window_title.zeroize();
            }
            for chunk in &mut document.chunks {
                chunk.text.zeroize();
            }
        }
        for vector in self
            .vectors
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values_mut()
        {
            vector.zeroize();
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedVectors {
    version: u32,
    model_revision: String,
    vectors: BTreeMap<String, Vec<f32>>,
}

impl Drop for SavedVectors {
    fn drop(&mut self) {
        for vector in self.vectors.values_mut() {
            vector.zeroize();
        }
    }
}

impl SavedVectors {
    fn checkpoint(
        shard: &[(&str, &str)],
        vectors: &HashMap<String, Vec<f32>>,
        additions: &BTreeMap<String, Vec<f32>>,
    ) -> Self {
        Self {
            version: INDEX_VERSION,
            model_revision: MINILM_VECTOR_SPACE_REVISION.into(),
            vectors: shard
                .iter()
                .filter_map(|(key, _)| {
                    additions
                        .get(*key)
                        .or_else(|| vectors.get(*key))
                        .map(|v| (key.to_string(), v.clone()))
                })
                .collect(),
        }
    }
}

struct EmbeddingAttempt {
    request_id: String,
    stage: &'static str,
    timeout: SemanticTimeouts,
    batch_size: usize,
    input_bytes: usize,
    indexed_chunks: usize,
    total_chunks: usize,
    started: Instant,
}

impl EmbeddingAttempt {
    fn failure(&self, error: &TaskError, status: &SemanticRuntimeStatus) -> String {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let warm_timeout_ms = self.timeout.warm.as_millis() as u64;
        let cold_timeout_ms = self.timeout.cold.as_millis() as u64;
        // Record runtime metadata without serializing unrelated backend state,
        // search text, recap content or the returned vectors.
        let runtime = serde_json::json!({
            "state": status.state,
            "provider": status.provider,
            "worker_version": status.worker_version,
            "ort_version": status.ort_version,
            "runtime_path": status.runtime_path,
            "loaded_model": status.loaded_model,
            "model_revision": status.model_revision,
            "restart_count": status.restart_count,
            "last_error_kind": status.last_error_kind,
            "last_error": status.last_error,
            "directml_disabled_for_session": status.directml_disabled_for_session,
        });
        tracing::warn!(target: "recap", request_id = %self.request_id, stage = self.stage,
            model = MINILM_MODEL_ID, batch_size = self.batch_size, input_bytes = self.input_bytes,
            indexed_chunks = self.indexed_chunks, total_chunks = self.total_chunks,
            warm_timeout_ms, cold_timeout_ms, elapsed_ms, error_kind = error.code(), detail = error.message(),
            runtime = %runtime, "[RECAP:SEARCH] Embedding failed");
        format!(
            "RECAP_SEARCH_MODEL_UNAVAILABLE: {error}\n\
             stage={} request_id={} model={MINILM_MODEL_ID} batch_size={} input_bytes={} \
             indexed_chunks={} total_chunks={} warm_timeout_ms={warm_timeout_ms} \
             cold_timeout_ms={cold_timeout_ms} elapsed_ms={elapsed_ms}\n\
             runtime={runtime}",
            self.stage,
            self.request_id,
            self.batch_size,
            self.input_bytes,
            self.indexed_chunks,
            self.total_chunks,
        )
    }
}

fn validate_embeddings(result: &SemanticEmbeddingResult, expected: usize) -> Result<(), TaskError> {
    if result.model != MlSemanticModel::MinilmL12 {
        return Err(TaskError::worker(
            MlErrorKind::Protocol,
            format!("Expected MiniLM embeddings, got {:?}", result.model),
        ));
    }
    if result.vectors.len() != expected {
        return Err(TaskError::worker(
            MlErrorKind::Protocol,
            format!(
                "Expected {expected} MiniLM embeddings, got {} (reported dimensions: {})",
                result.vectors.len(),
                result.dimensions,
            ),
        ));
    }
    if result.dimensions != MINILM_DIMENSIONS {
        return Err(TaskError::worker(
            MlErrorKind::Protocol,
            format!(
                "Expected {MINILM_DIMENSIONS} MiniLM dimensions, worker reported {}",
                result.dimensions,
            ),
        ));
    }
    for (index, vector) in result.vectors.iter().enumerate() {
        validate_minilm_vector(vector).map_err(|detail| {
            TaskError::worker(
                MlErrorKind::Inference,
                format!("Invalid MiniLM embedding at batch index {index}: {detail}"),
            )
        })?;
    }
    Ok(())
}

async fn embed_minilm(
    app: &tauri::AppHandle,
    texts: Vec<String>,
    stage: &'static str,
    timeout: SemanticTimeouts,
    indexed_chunks: usize,
    total_chunks: usize,
) -> Result<Vec<Vec<f32>>, String> {
    let attempt = EmbeddingAttempt {
        request_id: crate::maintenance_support::new_run_id("recap-search"),
        stage,
        timeout,
        batch_size: texts.len(),
        input_bytes: texts.iter().map(String::len).sum(),
        indexed_chunks,
        total_chunks,
        started: Instant::now(),
    };
    tracing::info!(target: "recap", request_id = %attempt.request_id, stage,
        model = MINILM_MODEL_ID, batch_size = attempt.batch_size, input_bytes = attempt.input_bytes,
        indexed_chunks, total_chunks,
        warm_timeout_ms = timeout.warm.as_millis() as u64,
        cold_timeout_ms = timeout.cold.as_millis() as u64,
        "[RECAP:SEARCH] Embedding started");
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let result = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::MinilmL12,
            texts,
            timeout,
            false,
        )
        .await
        .and_then(|result| {
            validate_embeddings(&result, attempt.batch_size)?;
            Ok(result)
        });
    match result {
        Ok(result) => {
            tracing::info!(target: "recap", request_id = %attempt.request_id, stage,
                elapsed_ms = attempt.started.elapsed().as_millis() as u64,
                model_load_ms = result.timings.model_load_ms,
                preprocess_ms = result.timings.preprocess_ms,
                inference_ms = result.timings.inference_ms,
                request_total_ms = result.timings.request_total_ms,
                "[RECAP:SEARCH] Embedding finished");
            Ok(result.vectors)
        }
        Err(error) => Err(attempt.failure(&error, &semantic.status())),
    }
}

fn worker_failure(stage: &str, error: tokio::task::JoinError) -> String {
    tracing::error!(target: "recap", stage, detail = %error, "[RECAP:SEARCH] Task failed");
    format!("RECAP_WORKER_FAILED: stage={stage}: {error}")
}

/// Byte-bounded windows are conservative for this multilingual tokenizer's
/// 256-token limit, including title context and special tokens. Overlap keeps
/// phrases at boundaries retrievable; every source character is retained.
fn windows(text: &str, bytes: usize) -> Vec<String> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        let mut size = 0;
        while end < chars.len() && size + chars[end].len_utf8() <= bytes {
            size += chars[end].len_utf8();
            end += 1;
        }
        if end == start {
            end += 1;
        }
        out.push(chars[start..end].iter().collect());
        if end == chars.len() {
            break;
        }
        start = end.saturating_sub(12).max(start + 1);
    }
    out
}

fn chunks(activity: &RecapActivity) -> Vec<Chunk> {
    let title = windows(&activity.task_title, 72)
        .into_iter()
        .next()
        .unwrap_or_default();
    [
        (activity.task_title.as_str(), false),
        (activity.text.as_str(), false),
    ]
    .into_iter()
    .chain(activity.details.iter().map(|d| (d.text.as_str(), true)))
    .flat_map(|(text, detail)| {
        windows(text, 160)
            .into_iter()
            .map(move |text| (text, detail))
    })
    .map(|(text, detail)| {
        let text = format!("{title}\n{text}");
        let key = digest(&format!(
            "recap-search-{INDEX_VERSION}:{MINILM_VECTOR_SPACE_REVISION}:{text}"
        ));
        Chunk { key, text, detail }
    })
    .collect()
}

/// Refilter generated text, user corrections and identities before either UI
/// or AI retrieval. Fact citations must survive as a complete set.
fn filtered_activity(
    mut activity: RecapActivity,
    filter: &SensitiveFilterState,
) -> Option<RecapActivity> {
    let mode = filter.mode();
    activity.task_title =
        crate::mcp_server::filter_identity(filter, mode, &activity.task_title).ok()?;
    activity.text = crate::mcp_server::filter_joined_text(filter, mode, &activity.text).ok()?;
    for source in &mut activity.sources {
        source.process_name =
            crate::mcp_server::filter_identity(filter, mode, &source.process_name).ok()?;
        source.window_title =
            crate::mcp_server::filter_identity(filter, mode, &source.window_title).ok()?;
    }
    if activity.sources.is_empty() {
        return None;
    }
    activity.details = activity
        .details
        .into_iter()
        .filter_map(|mut detail| {
            if detail.evidence_ids.is_empty()
                || !detail
                    .evidence_ids
                    .iter()
                    .all(|id| activity.sources.iter().any(|s| s.id == *id))
            {
                return None;
            }
            detail.text = crate::mcp_server::filter_joined_text(filter, mode, &detail.text).ok()?;
            Some(detail)
        })
        .collect();
    Some(activity)
}

fn load_corpus(app: &tauri::AppHandle) -> Result<Arc<Corpus>, String> {
    load_corpus_inner(app, true)
}

fn load_corpus_inner(app: &tauri::AppHandle, strict: bool) -> Result<Arc<Corpus>, String> {
    let runtime = app.state::<RecapSearchRuntime>();
    let _loading = runtime.loading.lock().unwrap_or_else(|e| e.into_inner());
    let current = identity(app)?;
    if let Some(corpus) = runtime
        .corpus
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|c| c.identity == current)
    {
        return Ok(corpus.clone());
    }
    let storage = app.state::<Arc<StorageState>>();
    let filter = app.state::<Arc<SensitiveFilterState>>();
    let mut documents = Vec::new();
    let mut vectors = HashMap::new();
    for (date, revision) in storage.recap_search_days(&current.privacy)? {
        current.context().check(app)?;
        let first_document = documents.len();
        let day_result = with_current_day(app, &date, |day| {
            for batch in day.batches {
                let directory = member_directory(&batch.records, &filter);
                for activity in batch.activities {
                    if let Some(activity) = filtered_activity(activity, &filter) {
                        let evidence = ActivityEvidence::project(&activity, &directory);
                        let mut document =
                            Document::new(date.clone(), revision, batch.start_ms, activity);
                        document.evidence = evidence;
                        documents.push(document);
                    }
                }
            }
            Ok(())
        });
        if let Err(error) = day_result {
            documents.truncate(first_document);
            if !strict && error == "RECAP_SOURCE_CHANGED" {
                continue;
            }
            return Err(error);
        }
        for key in storage.recap_payload_keys("search", &date, revision)? {
            current.context().check(app)?;
            match storage.recap_read::<SavedVectors>("search", &key, Some(revision)) {
                Ok(Some(mut saved))
                    if saved.version == INDEX_VERSION
                        && saved.model_revision == MINILM_VECTOR_SPACE_REVISION =>
                {
                    vectors.extend(
                        std::mem::take(&mut saved.vectors)
                            .into_iter()
                            .filter(|(_, v)| validate_minilm_vector(v).is_ok()),
                    );
                }
                Ok(_) => (),
                Err(error) if error == "RECAP_INVALID_CACHE" => (),
                Err(error) => return Err(error),
            }
        }
    }
    let keys: HashSet<&str> = documents
        .iter()
        .flat_map(|d| &d.chunks)
        .map(|c| c.key.as_str())
        .collect();
    vectors.retain(|key, _| keys.contains(key.as_str()));
    current.context().check(app)?;
    let total_chunks = keys.len();
    let dates = documents
        .iter()
        .map(|d| &d.date)
        .collect::<HashSet<_>>()
        .len();
    let corpus = Arc::new(Corpus {
        identity: current,
        documents,
        total_chunks,
        dates,
        vectors: Mutex::new(vectors),
    });
    let unchanged = corpus.identity == identity(app)?;
    if strict && !unchanged {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    if unchanged {
        *runtime.corpus.lock().unwrap_or_else(|e| e.into_inner()) = Some(corpus.clone());
    }
    // A lock event between publication and this fence must not resurrect a cache.
    if let Err(error) = corpus.identity.context().check(app) {
        runtime.clear();
        return Err(error);
    }
    Ok(corpus)
}

fn counts(corpus: &Corpus) -> (usize, usize) {
    let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
    (vectors.len(), corpus.total_chunks)
}

pub fn status(app: &tauri::AppHandle) -> Result<RecapSearchStatus, String> {
    let corpus = load_corpus(app)?;
    let (indexed_chunks, total_chunks) = counts(&corpus);
    let runtime = app.state::<RecapSearchRuntime>();
    let index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
    let result = RecapSearchStatus {
        revision: corpus.identity.token(),
        running: index.cancel.is_some(),
        indexed_chunks,
        total_chunks,
        activities: corpus.documents.len(),
        dates: corpus.dates,
        error: index.error.clone(),
    };
    drop(index);
    corpus.identity.check(app)?;
    runtime.publish_counts(app, &corpus, false);
    Ok(result)
}

struct IndexGuard {
    app: tauri::AppHandle,
    cancel: Arc<Cancellation>,
}
impl Drop for IndexGuard {
    fn drop(&mut self) {
        let runtime = self.app.state::<RecapSearchRuntime>();
        let progress = {
            let mut index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
            if index
                .cancel
                .as_ref()
                .is_some_and(|c| Arc::ptr_eq(c, &self.cancel))
            {
                index.cancel = None;
                index.progress.run.running = false;
                index.progress.run.stopping = false;
                if index.progress.complete() {
                    index.progress.set_phase("idle");
                } else {
                    let reason = index.stop_reason.take();
                    index.progress.wait(reason);
                }
                index.progress.run.revision += 1;
            }
            index.progress
        };
        emit_index_progress(&self.app, Some(progress));
    }
}

pub async fn prepare(app: tauri::AppHandle) -> Result<(), String> {
    prepare_run(app, false)
        .await
        .map(|_| ())
        .map_err(String::from)
}

pub(crate) async fn run_scheduled_slice(
    app: &tauri::AppHandle,
) -> Result<crate::background_scheduler::ScheduledSliceResult, TaskError> {
    if !semantic_enabled(app)? {
        return Ok(crate::background_scheduler::ScheduledSliceResult::skipped(
            "disabled",
        ));
    }
    let Ok(_worker) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
        return Ok(crate::background_scheduler::ScheduledSliceResult::skipped(
            "semantic_worker_busy",
        ));
    };
    prepare_run(app.clone(), true)
        .await
        .map(crate::background_scheduler::ScheduledSliceResult::complete)
        .map_err(|error| match error {
            TaskError::Failed(message) if message == "AUTH_REQUIRED" => {
                TaskError::AuthenticationRequired
            }
            TaskError::Failed(message) if message == "RECAP_SOURCE_CHANGED" => {
                TaskError::Paused("source_changed")
            }
            other => other,
        })
}

fn index_check(
    app: &tauri::AppHandle,
    cancel: &Cancellation,
    automatic: bool,
) -> Result<(), TaskError> {
    if cancel.is_cancelled() {
        return Err(TaskError::Paused("cancelled"));
    }
    if !semantic_enabled(app)? {
        return Err(TaskError::Paused("disabled"));
    }
    if automatic {
        if let Some(lease) = crate::background_policy::current_execution() {
            lease.check()?;
        }
        if app
            .state::<Arc<SemanticRuntimeState>>()
            .foreground_waiting()
        {
            return Err(TaskError::Paused("foreground_request"));
        }
    }
    Ok(())
}

async fn prepare_run(app: tauri::AppHandle, automatic: bool) -> Result<bool, TaskError> {
    identity(&app)?;
    let (cancel, progress) = {
        let runtime = app.state::<RecapSearchRuntime>();
        let mut index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
        if index.cancel.is_some() {
            return Err(TaskError::Paused("recap_index_busy"));
        }
        let cancel = Cancellation::new();
        index.cancel = Some(cancel.clone());
        index.error = None;
        index.stop_reason = None;
        index.progress.run.running = true;
        index.progress.run.stopping = false;
        index.progress.set_phase("preparing");
        index.progress.run.revision += 1;
        (cancel, index.progress)
    };
    let _guard = IndexGuard {
        app: app.clone(),
        cancel: cancel.clone(),
    };
    emit_index_progress(&app, Some(progress));
    let result = prepare_inner(&app, &cancel, automatic).await;
    if let Err(error) = &result {
        let runtime = app.state::<RecapSearchRuntime>();
        let mut index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
        index.error = (!error.is_pause() && !matches!(error, TaskError::AuthenticationRequired))
            .then(|| error.to_string());
        index.stop_reason = crate::background_policy::current_execution()
            .and_then(|lease| lease.reason())
            .or(Some(match error {
                TaskError::Paused(reason) => *reason,
                TaskError::AuthenticationRequired => "waiting_for_unlock",
                TaskError::Failed(message) if message == "AUTH_REQUIRED" => "waiting_for_unlock",
                TaskError::Failed(message) if message == "RECAP_SOURCE_CHANGED" => "source_changed",
                _ if error.is_pause() => "cancelled",
                _ if crate::semantic_runtime::is_deterministic_worker_failure(error) => "failed",
                _ => "retry_wait",
            }));
    }
    result
}

async fn prepare_inner(
    app: &tauri::AppHandle,
    cancel: &Arc<Cancellation>,
    automatic: bool,
) -> Result<bool, TaskError> {
    let load_app = app.clone();
    let corpus = tokio::task::spawn_blocking(move || load_corpus(&load_app))
        .await
        .map_err(|error| worker_failure("index_load", error))??;
    let (mut indexed_chunks, total_chunks) = counts(&corpus);
    let runtime = app.state::<RecapSearchRuntime>();
    runtime.publish_counts(app, &corpus, false);
    if indexed_chunks == total_chunks {
        corpus.identity.check(app)?;
        return Ok(false);
    }
    emit_index_progress(
        app,
        runtime.update_progress(|index| {
            if !index.progress.run.stopping {
                index.progress.set_phase("running");
            }
        }),
    );
    // Explicit preparation shares the worker fairly with interactive queries
    // and asks automatic model passes to yield until preparation ends.
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let _foreground = (!automatic).then(|| semantic.foreground_lease());
    let days =
        corpus
            .documents
            .iter()
            .fold(BTreeMap::<&str, Vec<&Document>>::new(), |mut days, d| {
                days.entry(&d.date).or_default().push(d);
                days
            });
    for (date, documents) in days {
        let all: Vec<_> = documents
            .iter()
            .flat_map(|d| &d.chunks)
            .map(|c| (c.key.as_str(), c.text.as_str()))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect();
        // Stable, bounded shards cap rewriting at 64 vectors per commit. Saving
        // every inference batch preserves cancellation/restart progress without
        // serializing the whole day's accumulated vectors each time.
        for shard in all.chunks(SAVE_SHARD) {
            let shard_key = format!(
                "{date}:{}",
                digest(
                    &shard
                        .iter()
                        .map(|(key, _)| *key)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            );
            let missing: Vec<_> = {
                let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
                shard
                    .iter()
                    .filter(|(key, _)| !vectors.contains_key(*key))
                    .copied()
                    .collect()
            };
            for batch in missing.chunks(ENCODE_BATCH) {
                corpus.identity.check(app)?;
                index_check(app, cancel, automatic)?;
                let encoding = semantic.embed_text(
                    app.clone(),
                    MlSemanticModel::MinilmL12,
                    batch.iter().map(|(_, text)| text.to_string()).collect(),
                    INDEX_EMBED_TIMEOUT,
                    false,
                );
                let monitor = async {
                    loop {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        corpus.identity.check(app)?;
                        index_check(app, cancel, automatic)?;
                        if cancel.is_cancelled() {
                            return Err::<(), TaskError>(TaskError::Paused("cancelled"));
                        }
                    }
                };
                let encoded = tokio::select! {
                    result = encoding => { let result = result?; validate_embeddings(&result, batch.len())?; result.vectors },
                    result = monitor => { result?; unreachable!() }
                };
                corpus.identity.check(app)?;
                index_check(app, cancel, automatic)?;
                let mut additions = SavedVectors {
                    version: INDEX_VERSION,
                    model_revision: MINILM_VECTOR_SPACE_REVISION.into(),
                    vectors: batch
                        .iter()
                        .zip(encoded)
                        .map(|((key, _), v)| (key.to_string(), v))
                        .collect(),
                };
                let saved = {
                    let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
                    SavedVectors::checkpoint(shard, &vectors, &additions.vectors)
                };
                let storage = app.state::<Arc<StorageState>>().inner().clone();
                let (key, day, revision, generation) = (
                    shard_key.clone(),
                    date.to_string(),
                    documents[0].revision,
                    corpus.identity.generation,
                );
                tokio::task::spawn_blocking(move || {
                    storage.recap_write("search", &key, &day, revision, generation, &saved)
                })
                .await
                .map_err(|error| worker_failure("index_save", error))??;
                corpus.identity.check(app)?;
                corpus
                    .vectors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(std::mem::take(&mut additions.vectors));
                indexed_chunks += batch.len();
                runtime.publish_counts(app, &corpus, true);
                crate::background_activity::index_progress(batch.len() as u64);
                if automatic {
                    return Ok(indexed_chunks < total_chunks);
                }
            }
        }
    }
    corpus.identity.check(app)?;
    Ok(false)
}

fn activity_text(activity: &RecapActivity, details: bool) -> String {
    let mut text = format!("{}\n{}", activity.task_title, activity.text);
    if details {
        for detail in &activity.details {
            text.push('\n');
            text.push_str(&detail.text);
        }
    }
    text
}

fn lexical_evidence(
    activity: &RecapActivity,
    plan: &QueryPlan,
    details: bool,
) -> Option<SearchMatchEvidence> {
    let mut text = activity_text(activity, details);
    let ranked = search_rank::score_text(plan, &text);
    text.zeroize();
    ranked.matched.then(|| SearchMatchEvidence {
        scope: "recap_activity".into(),
        relevance_score: ranked.score,
        matched_terms: ranked.matched_terms,
        literal_terms: ranked.literal_terms,
        match_type: if ranked.literal {
            "literal"
        } else {
            "approximate"
        }
        .into(),
        phrase_matched: ranked.phrase_matched,
        matched_bigram_groups: None,
        total_bigram_groups: None,
    })
}

fn activity_hit(
    date: String,
    batch_start_ms: i64,
    activity: RecapActivity,
    evidence: ActivityEvidence,
) -> RecapSearchHit {
    RecapSearchHit {
        result_offset: None,
        date,
        batch_start_ms,
        activity_id: activity.id,
        task_id: activity.task_id,
        task_title: activity.task_title,
        text: activity.text,
        details: activity.details,
        start_ms: activity.start_ms,
        end_ms: activity.end_ms,
        sources: activity.sources,
        evidence,
        keyword_rank: None,
        semantic_rank: None,
        keyword_score: None,
        semantic_score: None,
        match_evidence: None,
    }
}

fn hit_activity(hit: &RecapSearchHit) -> RecapActivity {
    RecapActivity {
        id: hit.activity_id.clone(),
        task_id: hit.task_id.clone(),
        task_title: hit.task_title.clone(),
        text: hit.text.clone(),
        details: hit.details.clone(),
        start_ms: hit.start_ms,
        end_ms: hit.end_ms,
        sources: hit.sources.clone(),
        member_ids: hit.evidence.member_screenshot_ids.clone(),
        segments: vec![],
    }
}

/// Build the blind index one changed day at a time. Ordinary searches never
/// materialize the semantic corpus or read unrelated saved activity payloads.
fn prepare_keywords(app: &tauri::AppHandle) -> Result<Identity, String> {
    let runtime = app.state::<RecapSearchRuntime>();
    let _loading = runtime
        .keyword_loading
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let storage = app.state::<Arc<StorageState>>();
    let filter = app.state::<Arc<SensitiveFilterState>>();
    let current = identity(app)?;
    let prepared = runtime
        .keyword_identity
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        == Some(&current);
    if !prepared {
        storage.recap_keyword_prune(&current.privacy)?;
    }
    let pending = if prepared {
        vec![]
    } else {
        storage.recap_keyword_pending_dates(&current.privacy)?
    };
    for date in pending {
        current.check(app)?;
        let revision = storage.recap_keyword_day_revision(&date)?;
        let mut documents = Vec::new();
        with_current_day(app, &date, |day| {
            for batch in day.batches {
                let directory = member_directory(&batch.records, &filter);
                for activity in batch.activities {
                    if let Some(activity) = filtered_activity(activity, &filter) {
                        let evidence = ActivityEvidence::project(&activity, &directory);
                        let text = activity_text(&activity, true);
                        let processes = activity
                            .sources
                            .iter()
                            .map(|s| s.process_name.clone())
                            .collect();
                        let (start, end) = (activity.start_ms, activity.end_ms);
                        documents.push((
                            activity_hit(date.clone(), batch.start_ms, activity, evidence),
                            text,
                            processes,
                            start,
                            end,
                        ));
                    }
                }
            }
            Ok(())
        })?;
        current.check(app)?;
        storage.recap_keyword_replace_day(
            &date,
            revision,
            &current.privacy,
            current.generation,
            current.epoch,
            documents,
        )?;
    }
    current.check(app)?;
    *runtime
        .keyword_identity
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(current.clone());
    current.check(app)?;
    Ok(current)
}

fn keyword_search(
    app: &tauri::AppHandle,
    query: &RecapSearchQuery,
) -> Result<(Vec<RecapSearchHit>, bool), String> {
    let (current, prepared) = match prepare_keywords(app) {
        Ok(current) => (current, true),
        Err(error) if error == "RECAP_SOURCE_CHANGED" => (identity(app)?, false),
        Err(error) => return Err(error),
    };
    let storage = app.state::<Arc<StorageState>>();
    let filter = app.state::<Arc<SensitiveFilterState>>();
    let (candidates, complete) = storage.recap_keyword_candidates::<RecapSearchHit>(
        &query.query,
        &current.privacy,
        query.start_date.as_deref(),
        query.end_date.as_deref(),
        query.start_ms,
        query.end_ms,
        &query.process_names,
    )?;
    let plan = QueryPlan::build(&query.query);
    let mut hits = Vec::new();
    for candidate in candidates {
        let mut hit = candidate.value;
        if query.query.trim().is_empty() {
            hit.keyword_score = Some(0.0);
            hits.push(hit);
            continue;
        }
        if let Some(activity) = filtered_activity(hit_activity(&hit), &filter) {
            if let Some(mut evidence) = lexical_evidence(&activity, &plan, query.include_details) {
                evidence.matched_bigram_groups = Some(candidate.hits);
                evidence.total_bigram_groups = Some(candidate.groups);
                hit.keyword_score = Some(evidence.relevance_score);
                hit.match_evidence = Some(evidence);
                hits.push(hit);
            }
        }
    }
    hits.sort_by(|a, b| {
        b.keyword_score
            .unwrap_or_default()
            .total_cmp(&a.keyword_score.unwrap_or_default())
            .then_with(|| b.start_ms.cmp(&a.start_ms))
            .then_with(|| a.activity_id.cmp(&b.activity_id))
    });
    for (i, hit) in hits.iter_mut().enumerate() {
        hit.keyword_rank = Some(i + 1);
    }
    current.context().check(app)?;
    Ok((hits, complete && prepared))
}

struct RankedSearch {
    items: Vec<RecapSearchHit>,
    total: usize,
    indexed_chunks: usize,
    coverage: SearchCoverage,
    execution: SearchExecution,
}

#[cfg(test)]
fn rank(
    corpus: &Corpus,
    query: &RecapSearchQuery,
    embedding: Option<&[f32]>,
) -> (Vec<RecapSearchHit>, usize, usize) {
    let ranked = rank_with_report(corpus, query, embedding);
    (ranked.items, ranked.total, ranked.indexed_chunks)
}

fn rank_with_report(
    corpus: &Corpus,
    query: &RecapSearchQuery,
    embedding: Option<&[f32]>,
) -> RankedSearch {
    let mut keywords = Vec::new();
    let mut semantic = Vec::new();
    let plan = QueryPlan::build(&query.query);
    let mut evidence_by_document = HashMap::new();
    // Shared titles and repeated excerpts are content-addressed: score each
    // vector once, after date/detail filtering, for the lifetime of this query.
    let mut scores = HashMap::<&str, f64>::new();
    let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
    for (i, document) in corpus
        .documents
        .iter()
        .enumerate()
        .filter(|(_, d)| query.includes(&d.date) && query.includes_activity(&d.activity))
    {
        if query.mode != SearchMode::Semantic {
            if let Some(evidence) =
                lexical_evidence(&document.activity, &plan, query.include_details)
            {
                keywords.push((i, evidence.relevance_score));
                evidence_by_document.insert(i, evidence);
            }
        }
        if let Some(embedding) = embedding {
            let score = document
                .chunks
                .iter()
                .filter(|c| query.include_details || !c.detail)
                .filter_map(|c| {
                    if let Some(score) = scores.get(c.key.as_str()) {
                        return Some(*score);
                    }
                    let v = vectors.get(&c.key)?;
                    let score = v
                        .iter()
                        .zip(embedding)
                        .map(|(a, b)| f64::from(a * b))
                        .sum::<f64>();
                    scores.insert(&c.key, score);
                    Some(score)
                })
                .filter(|s| s.is_finite() && *s >= MIN_SEMANTIC_SCORE)
                .max_by(f64::total_cmp);
            if let Some(score) = score {
                semantic.push((i, score));
            }
        }
    }
    let indexed = vectors.len();
    let coverage = search_coverage(corpus, query, &vectors);
    drop(vectors);
    let execution = SearchExecution {
        requested_mode: query.mode,
        keyword_executed: query.mode != SearchMode::Semantic,
        keyword_matches: keywords.len(),
        keyword_complete: true,
        semantic_executed: embedding.is_some(),
        semantic_matches: 0,
        semantic_candidates: semantic.len().min(200),
        semantic_candidate_limit: 200,
        semantic_candidates_truncated: false,
        fallback_reason: None,
    };
    let sort = |rows: &mut Vec<(usize, f64)>| {
        rows.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| {
                    corpus.documents[b.0]
                        .activity
                        .start_ms
                        .cmp(&corpus.documents[a.0].activity.start_ms)
                })
                .then_with(|| a.0.cmp(&b.0))
        })
    };
    sort(&mut keywords);
    // Exact activity top-200 needs no full semantic sort. Keep deterministic
    // ties when selecting the prefix so later pages retain the same ranking.
    let semantic_matches = semantic.len();
    let semantic_candidates_truncated = semantic_matches > 200;
    if semantic_candidates_truncated {
        semantic.select_nth_unstable_by(200, |a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| {
                    corpus.documents[b.0]
                        .activity
                        .start_ms
                        .cmp(&corpus.documents[a.0].activity.start_ms)
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        semantic.truncate(200);
    }
    sort(&mut semantic);
    let mut ranks: HashMap<usize, (Option<usize>, Option<usize>)> = HashMap::new();
    for (position, (i, _)) in keywords.iter().enumerate() {
        ranks.entry(*i).or_default().0 = Some(position + 1);
    }
    // Bound broad semantic tails while preserving all exact keyword hits.
    for (position, (i, _)) in semantic.iter().take(200).enumerate() {
        ranks.entry(*i).or_default().1 = Some(position + 1);
    }
    let keyword_scores: HashMap<_, _> = keywords.iter().copied().collect();
    let semantic_scores: HashMap<_, _> = semantic.iter().copied().collect();
    let mut ranked: Vec<_> = ranks.into_iter().collect();
    let rrf = |ranks: &(Option<usize>, Option<usize>)| {
        [ranks.0, ranks.1]
            .into_iter()
            .flatten()
            .map(|rank| 1.0 / (60 + rank) as f64)
            .sum::<f64>()
    };
    ranked.sort_by(|(a, ar), (b, br)| {
        rrf(br)
            .total_cmp(&rrf(ar))
            .then_with(|| {
                corpus.documents[*b]
                    .activity
                    .start_ms
                    .cmp(&corpus.documents[*a].activity.start_ms)
            })
            .then(a.cmp(b))
    });
    let total = ranked.len();
    let hits = ranked
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .map(|(i, (keyword_rank, semantic_rank))| {
            let d = &corpus.documents[i];
            let a = &d.activity;
            RecapSearchHit {
                result_offset: None,
                date: d.date.clone(),
                batch_start_ms: d.batch_start_ms,
                activity_id: a.id.clone(),
                task_id: a.task_id.clone(),
                task_title: a.task_title.clone(),
                text: a.text.clone(),
                details: a.details.clone(),
                start_ms: a.start_ms,
                end_ms: a.end_ms,
                sources: a.sources.clone(),
                evidence: d.evidence.clone(),
                keyword_rank,
                semantic_rank,
                keyword_score: keyword_scores.get(&i).copied(),
                semantic_score: semantic_scores.get(&i).copied(),
                match_evidence: evidence_by_document.remove(&i),
            }
        })
        .collect();
    let mut execution = execution;
    execution.semantic_matches = semantic_matches;
    execution.semantic_candidates_truncated = semantic_candidates_truncated;
    RankedSearch {
        items: hits,
        total,
        indexed_chunks: indexed,
        coverage,
        execution,
    }
}

fn query_key(query: &RecapSearchQuery) -> String {
    digest(
        &serde_json::json!([
            query.query.trim(),
            query.mode,
            query.start_date,
            query.end_date,
            query.include_details,
            query.process_names,
            query.start_ms,
            query.end_ms
        ])
        .to_string(),
    )
}

fn rank_semantic_with_keywords(
    corpus: &Corpus,
    query: &RecapSearchQuery,
    embedding: Option<&[f32]>,
    keywords: Vec<RecapSearchHit>,
) -> RankedSearch {
    let mut semantic_query = query.clone();
    semantic_query.mode = SearchMode::Semantic;
    semantic_query.offset = 0;
    semantic_query.limit = 200;
    let mut semantic = rank_with_report(corpus, &semantic_query, embedding);
    let keyword_count = keywords.len();
    let mut hits: HashMap<_, _> = keywords
        .into_iter()
        .map(|hit| ((hit.date.clone(), hit.activity_id.clone()), hit))
        .collect();
    for hit in semantic.items {
        let key = (hit.date.clone(), hit.activity_id.clone());
        if let Some(existing) = hits.get_mut(&key) {
            existing.semantic_rank = hit.semantic_rank;
            existing.semantic_score = hit.semantic_score;
        } else {
            hits.insert(key, hit);
        }
    }
    let score = |hit: &RecapSearchHit| {
        [hit.keyword_rank, hit.semantic_rank]
            .into_iter()
            .flatten()
            .map(|rank| 1.0 / (60 + rank) as f64)
            .sum::<f64>()
    };
    let mut items: Vec<_> = hits.into_values().collect();
    items.sort_by(|a, b| {
        score(b)
            .total_cmp(&score(a))
            .then_with(|| b.start_ms.cmp(&a.start_ms))
            .then_with(|| a.date.cmp(&b.date))
            .then_with(|| a.activity_id.cmp(&b.activity_id))
    });
    semantic.total = items.len();
    semantic.items = items
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .collect();
    semantic.execution.requested_mode = query.mode;
    semantic.execution.keyword_executed = query.mode != SearchMode::Semantic;
    semantic.execution.keyword_matches = keyword_count;
    semantic
}

pub async fn search(
    app: tauri::AppHandle,
    mut query: RecapSearchQuery,
) -> Result<RecapSearchResult, String> {
    query.validate()?;
    query.include_details = true;
    search_inner(app, query).await
}

pub(super) async fn search_inner(
    app: tauri::AppHandle,
    query: RecapSearchQuery,
) -> Result<RecapSearchResult, String> {
    let started = Instant::now();
    for _ in 0..3 {
        let result = search_page(app.clone(), query.clone(), started).await;
        if !matches!(&result, Err(error) if error == "RECAP_SOURCE_CHANGED") {
            return result;
        }
        tokio::task::yield_now().await;
    }
    let context = search_context(&app)?;
    let (enabled, progress) = (
        semantic_enabled(&app)?,
        app.state::<RecapSearchRuntime>().progress(),
    );
    let fallback = RecapSearchResult {
        revision: context.token(),
        ranking_revision: String::new(),
        items: vec![],
        total: 0,
        complete: false,
        offset: 0,
        limit: query.limit,
        next_offset: None,
        mode: SearchMode::Keyword,
        semantic_status: if enabled { "not_prepared" } else { "disabled" }.into(),
        semantic_error: None,
        indexed_chunks: None,
        total_chunks: None,
        coverage: None,
        execution: SearchExecution {
            requested_mode: query.mode,
            keyword_executed: true,
            keyword_complete: false,
            fallback_reason: Some("not_prepared"),
            ..Default::default()
        },
        index_progress: progress,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    store_search_page(&app, &query, &context, fallback, started)
}

async fn search_page(
    app: tauri::AppHandle,
    query: RecapSearchQuery,
    started: Instant,
) -> Result<RecapSearchResult, String> {
    let context = search_context(&app)?;
    let cached = app
        .state::<RecapSearchRuntime>()
        .pages
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .find(
            &context,
            &query_key(&query),
            query.ranking_revision.as_deref(),
            query.offset,
        );
    if let Some(cached) = cached {
        return read_search_page(&app, &query, &context, cached, query.offset, started);
    }
    let mut full_query = query.clone();
    full_query.offset = 0;
    full_query.limit = 1000;
    full_query.ranking_revision = None;
    let result = collect_search(app.clone(), full_query).await?;
    store_search_page(&app, &query, &context, result, started)
}

fn store_search_page(
    app: &tauri::AppHandle,
    query: &RecapSearchQuery,
    context: &SearchContext,
    mut result: RecapSearchResult,
    started: Instant,
) -> Result<RecapSearchResult, String> {
    context.check(app)?;
    let items = std::mem::take(&mut result.items);
    let snapshot = app
        .state::<RecapSearchRuntime>()
        .pages
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(context.clone(), query_key(query), result, &items)?;
    // An expired or evicted token starts a new set at zero. Clients can replace
    // their old prefix instead of appending a page from another ranking.
    let offset = if query.ranking_revision.is_some() {
        0
    } else {
        query.offset
    };
    read_search_page(app, query, context, snapshot, offset, started)
}

fn read_search_page(
    app: &tauri::AppHandle,
    query: &RecapSearchQuery,
    context: &SearchContext,
    snapshot: Arc<super::search_pages::SearchSnapshot<RecapSearchResult>>,
    offset: usize,
    started: Instant,
) -> Result<RecapSearchResult, String> {
    context.check(app)?;
    let mut result = snapshot.header.clone();
    result.items = snapshot.items()?;
    let storage = app.state::<Arc<StorageState>>();
    let days: Vec<_> = result
        .items
        .iter()
        .map(|item| item.date.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let visible = storage.search_visible_recap_days(&days)?;
    let total = result.items.len();
    let end = offset.saturating_add(query.limit).min(total);
    result.items = result
        .items
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(query.limit)
        .filter_map(|(position, mut item)| {
            item.result_offset = Some(position);
            visible.contains(&item.date).then_some(item)
        })
        .collect();
    result.offset = offset;
    result.limit = query.limit;
    result.next_offset = (end < total).then_some(end);
    result.ranking_revision = snapshot.token.clone();
    result.index_progress = app.state::<RecapSearchRuntime>().progress();
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    context.check(app)?;
    Ok(result)
}

async fn collect_search(
    app: tauri::AppHandle,
    mut query: RecapSearchQuery,
) -> Result<RecapSearchResult, String> {
    let request_identity = identity(&app)?;
    let requested_mode = query.mode;
    let enabled = semantic_enabled(&app)?;
    if !enabled || query.query.trim().is_empty() {
        query.mode = SearchMode::Keyword;
    }
    let started = Instant::now();
    if query.mode == SearchMode::Keyword {
        return tokio::task::spawn_blocking(move || {
            let (mut items, complete) = keyword_search(&app, &query)?;
            request_identity.context().check(&app)?;
            let current = request_identity;
            let total = items.len();
            let end = query.offset.saturating_add(query.limit).min(total);
            items = items
                .into_iter()
                .skip(query.offset)
                .take(query.limit)
                .collect();
            let result = RecapSearchResult {
                revision: current.token(),
                ranking_revision: String::new(),
                items,
                total,
                complete,
                offset: query.offset,
                limit: query.limit,
                next_offset: (end < total).then_some(end),
                mode: SearchMode::Keyword,
                semantic_status: if !enabled {
                    "disabled"
                } else {
                    "not_requested"
                }
                .into(),
                semantic_error: None,
                indexed_chunks: None,
                total_chunks: None,
                coverage: None,
                execution: SearchExecution {
                    requested_mode,
                    keyword_executed: true,
                    keyword_matches: total,
                    keyword_complete: complete,
                    fallback_reason: (requested_mode != SearchMode::Keyword)
                        .then_some(if enabled { "empty" } else { "disabled" }),
                    ..Default::default()
                },
                index_progress: app.state::<RecapSearchRuntime>().progress(),
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
            current.context().check(&app)?;
            Ok(result)
        })
        .await
        .map_err(|error| worker_failure("query_keywords", error))?;
    }
    let load_app = app.clone();
    let keyword_query = query.clone();
    let (keyword_hits, keyword_complete) = if query.mode == SearchMode::Hybrid {
        tokio::task::spawn_blocking(move || keyword_search(&load_app, &keyword_query))
            .await
            .map_err(|error| worker_failure("query_keywords", error))??
    } else {
        (Vec::new(), true)
    };
    let load_app = app.clone();
    let corpus = tokio::task::spawn_blocking(move || load_corpus_inner(&load_app, false))
        .await
        .map_err(|error| worker_failure("query_load", error))??;
    request_identity.context().check(&app)?;
    let (indexed_chunks, total_chunks) = counts(&corpus);
    let mut semantic_status = if !enabled {
        "disabled"
    } else if total_chunks == 0 {
        "empty"
    } else if indexed_chunks == 0 {
        "not_prepared"
    } else if indexed_chunks < total_chunks {
        "partial"
    } else {
        "ready"
    };
    let mut embedding = None;
    let mut semantic_error = None;
    let semantic = (query.mode != SearchMode::Keyword && indexed_chunks > 0)
        .then(|| app.state::<Arc<SemanticRuntimeState>>().inner().clone());
    let _foreground = semantic
        .as_ref()
        .map(|semantic| semantic.foreground_lease());
    if query.mode != SearchMode::Keyword && indexed_chunks > 0 {
        match embed_minilm(
            &app,
            vec![query.query.trim().into()],
            "query_embedding",
            QUERY_EMBED_TIMEOUT,
            indexed_chunks,
            total_chunks,
        )
        .await
        {
            Ok(vectors) => embedding = vectors.into_iter().next(),
            Err(error) => {
                semantic_status = "unavailable";
                semantic_error = Some(error);
            }
        }
    }
    drop(_foreground);
    corpus.identity.context().check(&app)?;
    if query.mode == SearchMode::Semantic && embedding.is_none() && total_chunks > 0 {
        return Err(semantic_error.unwrap_or_else(|| "RECAP_SEARCH_NOT_READY".into()));
    }
    let result = tokio::task::spawn_blocking(move || {
        corpus.identity.context().check(&app)?;
        let RankedSearch {
            items,
            total,
            indexed_chunks,
            coverage,
            mut execution,
            ..
        } = rank_semantic_with_keywords(&corpus, &query, embedding.as_deref(), keyword_hits);
        execution.keyword_complete = keyword_complete;
        execution.requested_mode = requested_mode;
        if requested_mode != SearchMode::Keyword && !execution.semantic_executed {
            execution.fallback_reason = Some(if !enabled {
                "disabled"
            } else if semantic_error.is_some() {
                "unavailable"
            } else if coverage.query_scope.total_chunks == 0 {
                "empty"
            } else {
                "not_prepared"
            });
        }
        let semantic_status = if matches!(semantic_status, "disabled" | "unavailable") {
            semantic_status
        } else {
            coverage.global.status
        };
        let index_progress = app.state::<RecapSearchRuntime>().progress();
        let end = query.offset.saturating_add(query.limit).min(total);
        let result = RecapSearchResult {
            revision: corpus.identity.token(),
            ranking_revision: String::new(),
            items,
            total,
            complete: keyword_complete && !execution.semantic_candidates_truncated,
            offset: query.offset,
            limit: query.limit,
            next_offset: (end < total).then_some(end),
            mode: query.mode,
            semantic_status: semantic_status.into(),
            semantic_error,
            indexed_chunks: Some(indexed_chunks),
            total_chunks: Some(total_chunks),
            coverage: Some(coverage),
            execution,
            index_progress,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        corpus.identity.context().check(&app)?;
        Ok(result)
    })
    .await
    .map_err(|error| worker_failure("query_rank", error))?;
    result
}

#[cfg(test)]
#[path = "search_benchmark.rs"]
mod benchmark;

#[cfg(test)]
mod tests {
    use super::*;

    fn embeddings(vectors: Vec<Vec<f32>>) -> SemanticEmbeddingResult {
        SemanticEmbeddingResult {
            model: MlSemanticModel::MinilmL12,
            dimensions: MINILM_DIMENSIONS,
            vectors,
            timings: crate::ml_protocol::MlSemanticTimings {
                model_load_ms: 0.0,
                preprocess_ms: 0.0,
                inference_ms: 0.0,
                request_total_ms: 0.0,
                cpu_ms: 0.0,
                model_load_cpu_ms: 0.0,
            },
        }
    }

    #[test]
    fn embedding_diagnostics_distinguish_empty_extra_and_malformed_responses() {
        let vector = vec![0.25; MINILM_DIMENSIONS];
        assert!(validate_embeddings(&embeddings(vec![vector.clone()]), 1).is_ok());
        for (vectors, actual) in [(vec![], 0), (vec![vector.clone(), vector.clone()], 2)] {
            let error = validate_embeddings(&embeddings(vectors), 1).unwrap_err();
            assert_eq!(error.worker_kind(), Some(MlErrorKind::Protocol));
            assert!(error
                .message()
                .contains(&format!("Expected 1 MiniLM embeddings, got {actual}")));
        }
        let error = validate_embeddings(&embeddings(vec![vector.clone()]), 4).unwrap_err();
        assert!(error
            .message()
            .contains("Expected 4 MiniLM embeddings, got 1"));
        let mut wrong_dimensions = embeddings(vec![vector.clone()]);
        wrong_dimensions.dimensions = 512;
        assert!(validate_embeddings(&wrong_dimensions, 1)
            .unwrap_err()
            .message()
            .contains("worker reported 512"));
        let mut wrong_model = embeddings(vec![vector.clone()]);
        wrong_model.model = MlSemanticModel::BgeSmallZh;
        assert!(validate_embeddings(&wrong_model, 1)
            .unwrap_err()
            .message()
            .contains("Expected MiniLM embeddings"));

        for (invalid, reason) in [
            (vec![0.25; 512], "Expected 384 dimensions, got 512"),
            (vec![0.0; MINILM_DIMENSIONS], "zero vector"),
            (vec![f32::NAN; MINILM_DIMENSIONS], "non-finite"),
        ] {
            let error =
                validate_embeddings(&embeddings(vec![vector.clone(), invalid]), 2).unwrap_err();
            assert_eq!(error.worker_kind(), Some(MlErrorKind::Inference));
            assert!(error.message().contains("batch index 1"));
            assert!(error.message().contains(reason));
        }
    }

    #[test]
    fn embedding_failure_logs_and_reports_the_cause_and_request_context() {
        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(bytes.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let attempt = EmbeddingAttempt {
            request_id: "recap-search-test".into(),
            stage: "query_embedding",
            timeout: QUERY_EMBED_TIMEOUT,
            batch_size: 1,
            input_bytes: 12,
            indexed_chunks: 4,
            total_chunks: 8,
            started: Instant::now(),
        };
        let mut status = SemanticRuntimeState::new().status();
        status.state = "degraded".into();
        status.provider = "cpu".into();
        status.loaded_model = Some("bge-small-zh-v1.5".into());
        status.worker_version = Some("0.9.0".into());
        // Unrelated index diagnostics must not be serialized into this report.
        status.backend.last_error = Some("private unrelated index detail".into());
        let error = TaskError::worker(
            MlErrorKind::Timeout,
            "semantic request 7 gave up waiting for the semantic worker",
        );
        let report = attempt.failure(&error, &status);
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(report.starts_with("RECAP_SEARCH_MODEL_UNAVAILABLE: timeout:"));
        for value in [
            "recap-search-test",
            "query_embedding",
            "warm_timeout_ms=5000",
            "cold_timeout_ms=20000",
            "indexed_chunks=4",
            "total_chunks=8",
            error.message(),
            "bge-small-zh-v1.5",
            "cpu",
            "degraded",
        ] {
            assert!(report.contains(value), "missing report context: {value}");
            assert!(output.contains(value), "missing log context: {value}");
        }
        assert!(output.contains("[RECAP:SEARCH] Embedding failed"));
        assert!(output.contains("error_kind=\"timeout\""));
        assert!(!report.contains("private unrelated"));
        assert!(!output.contains("private unrelated"));
    }

    #[tokio::test]
    async fn worker_panics_keep_the_stage_and_original_failure() {
        let error = tokio::task::spawn_blocking(|| panic!("test search task panic"))
            .await
            .unwrap_err();
        let report = worker_failure("query_load", error);
        assert!(report.starts_with("RECAP_WORKER_FAILED: stage=query_load:"));
        assert!(report.contains("test search task panic"));
    }

    fn activity(title: &str, text: &str) -> RecapActivity {
        RecapActivity {
            id: "activity".into(),
            task_id: "task".into(),
            task_title: title.into(),
            text: text.into(),
            start_ms: 1,
            end_ms: 2,
            segments: vec![],
            member_ids: vec![],
            sources: vec![SourceRef {
                id: 1,
                timestamp_ms: 1,
                process_name: "editor.exe".into(),
                window_title: "Notes".into(),
            }],
            details: vec![RecapDetail {
                text: "采用内存缓存降低查询延迟".into(),
                evidence_ids: vec![1],
            }],
        }
    }

    #[test]
    fn recap_keywords_use_snapshot_scores_and_tolerate_typos() {
        let a = activity("回顾搜索", "Six Degrees of Separation");
        let exact = lexical_evidence(&a, &QueryPlan::build("separation"), true).unwrap();
        let near = lexical_evidence(&a, &QueryPlan::build("Separtion"), true).unwrap();
        assert!(exact.relevance_score > near.relevance_score);
        assert_eq!(near.match_type, "approximate");
        assert!(lexical_evidence(&a, &QueryPlan::build("内存缓存"), false).is_none());
        assert!(lexical_evidence(&a, &QueryPlan::build("内存缓存"), true).is_some());
        assert!(lexical_evidence(&a, &QueryPlan::build("火星"), true).is_none());
    }

    #[test]
    fn checkpoints_bound_rewrites_and_resume_a_partly_saved_shard() {
        let keys = (0..SAVE_SHARD * 2 + 1)
            .map(|i| format!("key-{i:04}"))
            .collect::<Vec<_>>();
        let all = keys
            .iter()
            .map(|key| (key.as_str(), "synthetic"))
            .collect::<Vec<_>>();
        let mut vectors = HashMap::new();
        let mut rewritten = 0;
        for shard in all.chunks(SAVE_SHARD) {
            for batch in shard.chunks(ENCODE_BATCH) {
                let additions = batch
                    .iter()
                    .map(|(key, _)| (key.to_string(), vec![0.25; MINILM_DIMENSIONS]))
                    .collect();
                let saved = SavedVectors::checkpoint(shard, &vectors, &additions);
                assert!(saved.vectors.len() <= SAVE_SHARD);
                // A restart materializes exactly the committed prefix, not the
                // entire day's vectors and not the next uncommitted batch.
                let mut restored: SavedVectors =
                    serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
                rewritten += restored.vectors.len();
                vectors.extend(std::mem::take(&mut restored.vectors));
            }
        }
        assert_eq!(vectors.len(), all.len());
        assert!(rewritten <= all.len() * (SAVE_SHARD / ENCODE_BATCH + 1) / 2);
    }

    #[test]
    fn chunks_keep_late_content_and_separate_fact_windows() {
        let a = activity("标题", &format!("{}独特结尾", "内容".repeat(300)));
        let parts = chunks(&a);
        assert!(parts.iter().any(|c| c.text.contains("独特结尾")));
        assert!(parts
            .iter()
            .any(|c| c.detail && c.text.contains("内存缓存")));
        assert!(parts.iter().all(|c| c.text.len() <= 233));
        let mut renamed = a.clone();
        renamed.task_title = "用户修正的标题".into();
        assert_ne!(parts[0].key, chunks(&renamed)[0].key);
    }

    #[test]
    fn retrieval_fuses_routes_and_applies_date_filters_before_ranking() {
        let a = activity("回顾搜索", "比较两种检索方法");
        let b = activity("数据库", "分析查询耗时");
        let c = activity("回顾搜索", "很早以前的记录");
        let documents = vec![("2026-10-10", a), ("2026-10-11", b), ("2025-01-01", c)]
            .into_iter()
            .map(|(date, activity)| Document::new(date.into(), 0, 0, activity))
            .collect::<Vec<_>>();
        let vectors = HashMap::from([
            // Distinct descriptions have distinct vectors; repeated title-only
            // windows deliberately share the same content-addressed key.
            (documents[0].chunks[1].key.clone(), vec![0.7, 0.3]),
            (documents[1].chunks[1].key.clone(), vec![1.0, 0.0]),
            (documents[2].chunks[1].key.clone(), vec![1.0, 0.0]),
        ]);
        let corpus = Corpus {
            identity: Identity {
                generation: 0,
                epoch: 0,
                privacy: String::new(),
                serial: 0,
                session: None,
            },
            documents,
            total_chunks: 3,
            dates: 3,
            vectors: Mutex::new(vectors),
        };
        let query = RecapSearchQuery {
            query: "回顾搜索".into(),
            start_date: Some("2026-10-01".into()),
            ..Default::default()
        };
        let (hits, total, _) = rank(&corpus, &query, Some(&[1.0, 0.0]));
        assert_eq!(total, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].task_title, "回顾搜索");
        assert_eq!(hits[0].keyword_rank, Some(1));
        assert_eq!(hits[0].semantic_rank, Some(2));
        let second_page = RecapSearchQuery {
            offset: 1,
            limit: 1,
            ..query
        };
        let (hits, total, _) = rank(&corpus, &second_page, Some(&[1.0, 0.0]));
        assert_eq!(total, 2);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].task_title, "数据库");
    }

    #[test]
    fn search_validates_dates_query_and_page_bounds() {
        assert!(RecapSearchQuery::default().validate().is_err());
        let mut q = RecapSearchQuery {
            query: "缓存".into(),
            ..Default::default()
        };
        assert!(q.validate().is_ok());
        q.limit = 51;
        assert!(q.validate().is_err());
        q.limit = 20;
        q.start_date = Some("2026-10-12".into());
        q.end_date = Some("2026-10-01".into());
        assert!(q.validate().is_err());
    }

    #[test]
    fn hybrid_search_keeps_verified_keywords_without_weak_semantic_boosts() {
        let fixtures = [
            (
                "literal",
                "CarbonPaper",
                "Review capture settings",
                Some(0.3),
            ),
            (
                "approximate",
                "CarbonPapr",
                "Review app settings",
                Some(0.49),
            ),
            ("detail", "Project notes", "Review saved notes", Some(0.2)),
            ("unindexed", "CarbonPaper", "Read the release notes", None),
            (
                "boundary",
                "Daily routine",
                "Review today's activities",
                Some(0.5),
            ),
            (
                "strong",
                "Screen history",
                "Find earlier desktop work",
                Some(0.8),
            ),
            (
                "weak",
                "Recipe notes",
                "Plan tomorrow's dinner",
                Some(0.499),
            ),
            ("invalid", "Travel plans", "Compare hotels", Some(f32::NAN)),
        ];
        let mut documents = Vec::new();
        let mut vectors = HashMap::new();
        for (id, title, text, score) in fixtures {
            let mut a = activity(title, text);
            a.id = id.into();
            if id == "detail" {
                a.details[0].text = "CarbonPaper keeps a searchable screenshot history".into();
            }
            let document = Document::new("2026-10-10".into(), 0, 0, a);
            if let Some(score) = score {
                vectors.insert(document.chunks[1].key.clone(), vec![score, 0.0]);
            }
            documents.push(document);
        }
        let corpus = Corpus {
            identity: Identity {
                generation: 0,
                epoch: 0,
                privacy: String::new(),
                serial: 0,
                session: None,
            },
            total_chunks: vectors.len(),
            dates: 1,
            documents,
            vectors: Mutex::new(vectors),
        };
        let query = RecapSearchQuery {
            query: "carbonpaper".into(),
            ..Default::default()
        };
        let keyword_query = RecapSearchQuery {
            mode: SearchMode::Keyword,
            ..query.clone()
        };
        let keywords = rank_with_report(&corpus, &keyword_query, None).items;
        assert_eq!(keywords.len(), 4);
        let report = rank_semantic_with_keywords(&corpus, &query, Some(&[1.0, 0.0]), keywords);
        assert_eq!(report.total, 6);
        assert_eq!(report.execution.keyword_matches, 4);
        assert_eq!(report.execution.semantic_matches, 2);
        assert_eq!(report.execution.semantic_candidates, 2);
        assert!(!report.execution.semantic_candidates_truncated);
        for id in ["literal", "approximate", "detail", "unindexed"] {
            let hit = report
                .items
                .iter()
                .find(|hit| hit.activity_id == id)
                .unwrap();
            assert!(hit.keyword_rank.is_some(), "missing keyword match: {id}");
            assert!(
                hit.semantic_rank.is_none(),
                "weak vector boosted keyword: {id}"
            );
            assert!(hit.semantic_score.is_none());
            assert_eq!(
                hit.match_evidence.as_ref().unwrap().match_type,
                if id == "approximate" {
                    "approximate"
                } else {
                    "literal"
                }
            );
        }
        let boundary = report
            .items
            .iter()
            .find(|hit| hit.activity_id == "boundary")
            .unwrap();
        assert_eq!(boundary.semantic_score, Some(0.5));
        assert!(boundary.keyword_rank.is_none());
        assert!(!report
            .items
            .iter()
            .any(|hit| ["weak", "invalid"].contains(&hit.activity_id.as_str())));

        let semantic_query = RecapSearchQuery {
            mode: SearchMode::Semantic,
            ..query
        };
        let semantic =
            rank_semantic_with_keywords(&corpus, &semantic_query, Some(&[1.0, 0.0]), vec![]);
        assert_eq!(semantic.total, 2);
        assert!(semantic.items.iter().all(|hit| hit.keyword_rank.is_none()));
    }

    #[test]
    fn semantic_threshold_excludes_noise_before_candidate_caps_and_pagination() {
        let mut documents: Vec<_> = (0..205)
            .map(|i| {
                let mut a = activity("Recipe notes", "Plan tomorrow's dinner");
                a.id = format!("noise-{i}");
                Document::new("2026-10-10".into(), 0, 0, a)
            })
            .collect();
        let mut vectors = HashMap::from([(documents[0].chunks[0].key.clone(), vec![0.49, 0.0])]);
        for (id, score) in [("strong", 0.9), ("boundary", 0.5)] {
            let mut a = activity(id, "Find earlier desktop work");
            a.id = id.into();
            let document = Document::new("2026-10-10".into(), 0, 0, a);
            vectors.insert(document.chunks[0].key.clone(), vec![score, 0.0]);
            documents.push(document);
        }
        let corpus = Corpus {
            identity: Identity {
                generation: 0,
                epoch: 0,
                privacy: String::new(),
                serial: 0,
                session: None,
            },
            total_chunks: vectors.len(),
            dates: 1,
            documents,
            vectors: Mutex::new(vectors),
        };
        let query = RecapSearchQuery {
            query: "carbonpaper".into(),
            mode: SearchMode::Semantic,
            offset: 1,
            limit: 1,
            ..Default::default()
        };
        let report = rank_semantic_with_keywords(&corpus, &query, Some(&[1.0, 0.0]), vec![]);
        assert_eq!(report.total, 2);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].activity_id, "boundary");
        assert_eq!(report.items[0].semantic_rank, Some(2));
        assert_eq!(report.items[0].semantic_score, Some(0.5));
        assert_eq!(report.execution.semantic_matches, 2);
        assert_eq!(report.execution.semantic_candidates, 2);
        assert!(!report.execution.semantic_candidates_truncated);
        assert_eq!(report.coverage.query_scope.activities, 207);
    }

    #[test]
    fn coverage_tracks_shared_chunks_and_scoped_missing_dates_independently_of_hits() {
        let mut first = activity("shared", "first body");
        first.sources[0].process_name = "editor.exe".into();
        let mut second = activity("shared", "second body");
        second.sources[0].process_name = "browser.exe".into();
        let documents = vec![
            Document::new("2026-10-01".into(), 0, 0, first),
            Document::new("2026-10-02".into(), 0, 0, second),
        ];
        let vectors = HashMap::from([(documents[0].chunks[0].key.clone(), vec![1.0, 0.0])]);
        let corpus = Corpus {
            identity: Identity {
                generation: 0,
                epoch: 0,
                privacy: String::new(),
                serial: 0,
                session: None,
            },
            documents,
            total_chunks: 0,
            dates: 2,
            vectors: Mutex::new(vectors),
        };
        let query = RecapSearchQuery {
            query: "absent".into(),
            process_names: vec!["EDITOR.EXE".into()],
            ..Default::default()
        };
        let report = rank_with_report(&corpus, &query, Some(&[1.0, 0.0]));
        assert_eq!(report.coverage.global.activities, 2);
        assert_eq!(report.coverage.global.indexed_chunks, 1);
        assert_eq!(report.coverage.global.partially_indexed_activities, 2);
        assert_eq!(report.coverage.query_scope.activities, 1);
        assert_eq!(report.coverage.dates.len(), 1);
        assert_eq!(report.coverage.missing_dates, vec!["2026-10-01"]);
        assert_eq!(report.coverage.query_scope.status, "partial");
        assert!(report.execution.keyword_executed);
        assert_eq!(report.execution.keyword_matches, 0);
        assert!(report.execution.semantic_executed);
        assert_eq!(report.execution.semantic_matches, 1);
        assert!(report.items[0].keyword_rank.is_none());
        let keyword = rank_with_report(&corpus, &query, None);
        assert!(keyword.execution.keyword_executed);
        assert!(!keyword.execution.semantic_executed);
        assert_eq!(keyword.total, 0);
        assert_eq!(keyword.coverage.query_scope.indexed_chunks, 1);
    }

    #[test]
    fn semantic_report_counts_matches_before_candidate_cap_and_pagination() {
        let documents: Vec<_> = (0..205)
            .map(|i| {
                let mut a = activity("shared", "body");
                a.id = i.to_string();
                Document::new("2026-10-01".into(), 0, 0, a)
            })
            .collect();
        let vectors = HashMap::from([(documents[0].chunks[0].key.clone(), vec![1.0, 0.0])]);
        let corpus = Corpus {
            identity: Identity {
                generation: 0,
                epoch: 0,
                privacy: String::new(),
                serial: 0,
                session: None,
            },
            documents,
            total_chunks: 0,
            dates: 1,
            vectors: Mutex::new(vectors),
        };
        let query = RecapSearchQuery {
            query: "absent".into(),
            limit: 1,
            offset: 3,
            ..Default::default()
        };
        let report = rank_with_report(&corpus, &query, Some(&[1.0, 0.0]));
        assert_eq!(report.execution.semantic_matches, 205);
        assert_eq!(report.execution.semantic_candidates, 200);
        assert!(report.execution.semantic_candidates_truncated);
        assert_eq!(report.total, 200);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.coverage.query_scope.activities, 205);
    }

    #[test]
    fn activity_filters_match_source_processes_and_inclusive_time_overlap() {
        let mut activity = activity("搜索", "回顾活动");
        activity.start_ms = 1000;
        activity.end_ms = 2000;
        activity.sources = vec![SourceRef {
            id: 1,
            timestamp_ms: 1500,
            process_name: "Code.exe".into(),
            window_title: "Design".into(),
        }];
        let mut query = RecapSearchQuery {
            query: "搜索".into(),
            process_names: vec!["code.EXE".into()],
            start_ms: Some(2000),
            end_ms: Some(3000),
            ..Default::default()
        };
        assert!(query.includes_activity(&activity));
        query.start_ms = Some(2001);
        assert!(!query.includes_activity(&activity));
        query.start_ms = Some(0);
        query.end_ms = Some(1000);
        assert!(query.includes_activity(&activity));
        query.end_ms = Some(999);
        assert!(!query.includes_activity(&activity));
        query.end_ms = Some(3000);
        query.process_names = vec!["msedge.exe".into()];
        assert!(!query.includes_activity(&activity));
        query.process_names.clear();
        assert!(query.includes_activity(&activity));
    }

    #[test]
    fn page_query_key_keeps_pagination_separate_from_search_options() {
        let mut query = RecapSearchQuery {
            query: "cache".into(),
            ..Default::default()
        };
        let original = query_key(&query);
        query.offset = 20;
        query.limit = 5;
        query.ranking_revision = Some("a".repeat(64));
        assert_eq!(original, query_key(&query));
        query.include_details = false;
        assert_ne!(original, query_key(&query));
    }

    #[test]
    fn retrieval_filters_generated_facts_and_drops_dangling_citations() {
        let filter = SensitiveFilterState::with_test_words(&["private"]);
        let mut a = activity("Search", "Read project notes");
        a.details = vec![
            RecapDetail {
                text: "private fact".into(),
                evidence_ids: vec![1],
            },
            RecapDetail {
                text: "missing evidence".into(),
                evidence_ids: vec![99],
            },
        ];
        for mode in ["mask", "remove_paragraph", "reject"] {
            let mut config = filter.get_config();
            config.mode = mode.into();
            filter.update_config(config);
            let clean = filtered_activity(a.clone(), &filter).unwrap();
            let json = serde_json::to_string(&clean).unwrap();
            assert!(!json.contains("private"));
            assert!(!json.contains("missing evidence"));
        }
    }
}
