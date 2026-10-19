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
use crate::storage::StorageState;
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

#[derive(Clone, Debug, Serialize)]
pub struct RecapSearchHit {
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
    pub keyword_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
}

#[derive(Serialize)]
pub struct RecapSearchResult {
    pub revision: String,
    pub ranking_revision: String,
    pub items: Vec<RecapSearchHit>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    pub mode: SearchMode,
    pub semantic_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_error: Option<String>,
    pub indexed_chunks: usize,
    pub total_chunks: usize,
    pub coverage: SearchCoverage,
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
    corpus: Mutex<Option<Arc<Corpus>>>,
    index: Mutex<IndexRun>,
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
        self.index.lock().unwrap_or_else(|e| e.into_inner()).error = None;
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
    keywords: KeywordText,
}

impl Document {
    fn new(date: String, revision: i64, batch_start_ms: i64, activity: RecapActivity) -> Self {
        Self {
            date,
            revision,
            batch_start_ms,
            chunks: chunks(&activity),
            keywords: KeywordText::new(&activity),
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
            document.keywords.title.zeroize();
            document.keywords.text.zeroize();
            let activity = &mut document.activity;
            activity.task_title.zeroize();
            activity.text.zeroize();
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
    for (date, revision) in storage.recap_search_days()? {
        current.check(app)?;
        with_current_day(app, &date, |day| {
            for batch in day.batches {
                for activity in batch.activities {
                    if let Some(activity) = filtered_activity(activity, &filter) {
                        documents.push(Document::new(
                            date.clone(),
                            revision,
                            batch.start_ms,
                            activity,
                        ));
                    }
                }
            }
            Ok(())
        })?;
        for key in storage.recap_payload_keys("search", &date, revision)? {
            current.check(app)?;
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
    current.check(app)?;
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
    *runtime.corpus.lock().unwrap_or_else(|e| e.into_inner()) = Some(corpus.clone());
    // A lock event between publication and this fence must not resurrect a cache.
    if let Err(error) = corpus.identity.check(app) {
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

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

struct KeywordText {
    title: String,
    text: String,
    body_bytes: usize,
}

impl KeywordText {
    fn new(activity: &RecapActivity) -> Self {
        let mut text = normalize(&activity.text);
        let body_bytes = text.len();
        for detail in &activity.details {
            let detail = normalize(&detail.text);
            if !detail.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&detail);
            }
        }
        Self {
            title: normalize(&activity.task_title),
            text,
            body_bytes,
        }
    }

    fn score(&self, query: &str, terms: &[&str], details: bool) -> f64 {
        if query.is_empty() {
            return 0.0;
        }
        let text = if details {
            &self.text
        } else {
            &self.text[..self.body_bytes]
        };
        if !terms
            .iter()
            .all(|term| self.title.contains(term) || text.contains(term))
        {
            return 0.0;
        }
        let title_hits = terms
            .iter()
            .filter(|term| self.title.contains(**term))
            .count();
        1.0 + title_hits as f64 * 3.0
            + if self.title.contains(query) { 8.0 } else { 0.0 }
            + if text.contains(query) { 4.0 } else { 0.0 }
    }
}

#[cfg(test)]
fn keyword_score(activity: &RecapActivity, query: &str, details: bool) -> f64 {
    let query = normalize(query);
    if query.is_empty() {
        return 0.0;
    }
    let title = normalize(&activity.task_title);
    let text = normalize(&format!(
        "{} {}",
        activity.text,
        if details {
            activity
                .details
                .iter()
                .map(|d| d.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        }
    ));
    let terms: HashSet<_> = query.split_whitespace().collect();
    let matched = terms
        .iter()
        .filter(|term| title.contains(**term) || text.contains(**term))
        .count();
    if matched != terms.len() {
        return 0.0;
    }
    let title_hits = terms.iter().filter(|term| title.contains(**term)).count();
    1.0 + title_hits as f64 * 3.0
        + if title.contains(&query) { 8.0 } else { 0.0 }
        + if text.contains(&query) { 4.0 } else { 0.0 }
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
    let normalized = normalize(&query.query);
    let mut terms: Vec<_> = normalized.split_whitespace().collect();
    terms.sort_unstable();
    terms.dedup();
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
            let score = document
                .keywords
                .score(&normalized, &terms, query.include_details);
            if score > 0.0 {
                keywords.push((i, score));
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
                .filter(|s| s.is_finite() && *s > 0.0)
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
                keyword_rank,
                semantic_rank,
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

fn ranking_revision(
    identity: &Identity,
    query: &RecapSearchQuery,
    semantic_indexed: Option<usize>,
) -> String {
    digest(
        &serde_json::json!([
            identity.token(),
            query.query.trim(),
            query.mode,
            query.start_date,
            query.end_date,
            query.include_details,
            query.process_names,
            query.start_ms,
            query.end_ms,
            semantic_indexed
        ])
        .to_string(),
    )
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
    mut query: RecapSearchQuery,
) -> Result<RecapSearchResult, String> {
    let requested_mode = query.mode;
    let enabled = semantic_enabled(&app)?;
    if !enabled {
        query.mode = SearchMode::Keyword;
    }
    let started = Instant::now();
    let load_app = app.clone();
    let corpus = tokio::task::spawn_blocking(move || load_corpus(&load_app))
        .await
        .map_err(|error| worker_failure("query_load", error))??;
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
    corpus.identity.check(&app)?;
    if query.mode == SearchMode::Semantic && embedding.is_none() && total_chunks > 0 {
        return Err(semantic_error.unwrap_or_else(|| "RECAP_SEARCH_NOT_READY".into()));
    }
    let result = tokio::task::spawn_blocking(move || {
        corpus.identity.check(&app)?;
        let RankedSearch {
            items,
            total,
            indexed_chunks,
            coverage,
            mut execution,
            ..
        } = rank_with_report(&corpus, &query, embedding.as_deref());
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
        let ranking_revision = ranking_revision(
            &corpus.identity,
            &query,
            embedding.as_ref().map(|_| indexed_chunks),
        );
        if query
            .ranking_revision
            .as_ref()
            .is_some_and(|previous| previous != &ranking_revision)
        {
            return Err("RECAP_SEARCH_RESULTS_CHANGED".into());
        }
        let end = query.offset.saturating_add(query.limit).min(total);
        let result = RecapSearchResult {
            revision: corpus.identity.token(),
            ranking_revision,
            items,
            total,
            offset: query.offset,
            limit: query.limit,
            next_offset: (end < total).then_some(end),
            mode: query.mode,
            semantic_status: semantic_status.into(),
            semantic_error,
            indexed_chunks,
            total_chunks,
            coverage,
            execution,
            index_progress,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        corpus.identity.check(&app)?;
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
    fn keyword_requires_all_terms_and_details_are_an_independent_ablation() {
        let a = activity("回顾搜索", "比较关键词与语义检索");
        assert!(keyword_score(&a, "回顾 检索", false) > 0.0);
        assert_eq!(keyword_score(&a, "回顾 火星", true), 0.0);
        assert_eq!(keyword_score(&a, "内存缓存", false), 0.0);
        assert!(keyword_score(&a, "内存缓存", true) > 0.0);
        assert_eq!(keyword_score(&a, "!!!", true), 0.0);
    }

    #[test]
    fn cached_keywords_match_the_original_scores_in_both_detail_modes() {
        for a in [
            activity("回顾 SEARCH", "比较 project"),
            activity("", ""),
            activity("Unicode：界", "多余  空格、与标点，mixed-CASE"),
        ] {
            let cached = KeywordText::new(&a);
            for query in [
                "回顾 检索",
                "SEARCH SEARCH",
                "project 采用内存缓存降低查询延迟",
                "mixed CASE",
                "火星",
                "!!!",
                "界",
            ] {
                let normalized = normalize(query);
                let terms = normalized
                    .split_whitespace()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                for details in [true, false] {
                    assert_eq!(
                        cached.score(&normalized, &terms, details),
                        keyword_score(&a, query, details),
                        "{query}, details={details}"
                    );
                }
            }
        }
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
    fn page_identity_changes_when_index_coverage_or_search_options_change() {
        let identity = Identity {
            generation: 1,
            epoch: 2,
            privacy: "policy".into(),
            serial: 3,
            session: None,
        };
        let mut query = RecapSearchQuery {
            query: "cache".into(),
            ..Default::default()
        };
        let original = ranking_revision(&identity, &query, Some(10));
        query.offset = 20;
        assert_eq!(original, ranking_revision(&identity, &query, Some(10)));
        assert_ne!(original, ranking_revision(&identity, &query, Some(11)));
        assert_ne!(original, ranking_revision(&identity, &query, None));
        query.include_details = false;
        assert_ne!(original, ranking_revision(&identity, &query, Some(10)));
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
