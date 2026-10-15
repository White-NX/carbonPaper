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

use super::{runner::privacy_hash, types::*, with_current_day};
use crate::ai::provider::Cancellation;
use crate::credential_manager::CredentialManagerState;
use crate::minilm_contract::{validate_minilm_vector, MINILM_VECTOR_SPACE_REVISION};
use crate::ml_protocol::MlSemanticModel;
use crate::semantic_runtime::SemanticRuntimeState;
use crate::sensitive_filter::SensitiveFilterState;
use crate::storage::StorageState;

const INDEX_VERSION: u32 = 1;
const ENCODE_BATCH: usize = 4;

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
            day_bounds(date)?;
        }
        if matches!((&self.start_date, &self.end_date), (Some(a), Some(b)) if a > b) {
            return Err("RECAP_INVALID_DATE".into());
        }
        Ok(())
    }

    fn includes(&self, date: &str) -> bool {
        self.start_date.as_deref().is_none_or(|start| date >= start)
            && self.end_date.as_deref().is_none_or(|end| date <= end)
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
    pub indexed_chunks: usize,
    pub total_chunks: usize,
    pub elapsed_ms: u64,
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
    cancel: Option<Arc<Cancellation>>,
    error: Option<String>,
}

#[derive(Default)]
pub struct RecapSearchRuntime {
    serial: AtomicU64,
    corpus: Mutex<Option<Arc<Corpus>>>,
    index: Mutex<IndexRun>,
}

impl RecapSearchRuntime {
    pub fn cancel(&self) -> bool {
        let index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = &index.cancel {
            cancel.cancel();
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

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    generation: u64,
    epoch: i64,
    privacy: String,
    serial: u64,
    session: Option<Instant>,
}

fn identity(app: &tauri::AppHandle) -> Result<Identity, String> {
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

impl Identity {
    fn token(&self) -> String {
        digest(&format!(
            "{}:{}:{}:{}:{:?}",
            self.generation, self.epoch, self.privacy, self.serial, self.session
        ))
    }
    fn check(&self, app: &tauri::AppHandle) -> Result<(), String> {
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
}
struct Corpus {
    identity: Identity,
    documents: Vec<Document>,
    vectors: Mutex<BTreeMap<String, Vec<f32>>>,
}

impl Drop for Corpus {
    fn drop(&mut self) {
        for document in &mut self.documents {
            document.date.zeroize();
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
    let current = identity(app)?;
    let runtime = app.state::<RecapSearchRuntime>();
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
    let mut vectors = BTreeMap::new();
    for (date, revision) in storage.recap_search_days()? {
        current.check(app)?;
        with_current_day(app, &date, |day| {
            for batch in day.batches {
                for activity in batch.activities {
                    if let Some(activity) = filtered_activity(activity, &filter) {
                        documents.push(Document {
                            date: date.clone(),
                            revision,
                            batch_start_ms: batch.start_ms,
                            chunks: chunks(&activity),
                            activity,
                        });
                    }
                }
            }
            Ok(())
        })?;
        match storage.recap_read::<SavedVectors>("search", &date, Some(revision)) {
            Ok(Some(saved))
                if saved.version == INDEX_VERSION
                    && saved.model_revision == MINILM_VECTOR_SPACE_REVISION =>
            {
                vectors.extend(
                    saved
                        .vectors
                        .into_iter()
                        .filter(|(_, v)| validate_minilm_vector(v).is_ok()),
                );
            }
            Ok(_) => (),
            Err(error) if error == "RECAP_INVALID_CACHE" => (),
            Err(error) => return Err(error),
        }
    }
    let keys: HashSet<&str> = documents
        .iter()
        .flat_map(|d| &d.chunks)
        .map(|c| c.key.as_str())
        .collect();
    vectors.retain(|key, _| keys.contains(key.as_str()));
    current.check(app)?;
    let corpus = Arc::new(Corpus {
        identity: current,
        documents,
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
    let keys: HashSet<&str> = corpus
        .documents
        .iter()
        .flat_map(|d| &d.chunks)
        .map(|c| c.key.as_str())
        .collect();
    let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
    (
        keys.iter()
            .filter(|key| vectors.contains_key(**key))
            .count(),
        keys.len(),
    )
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
        dates: corpus
            .documents
            .iter()
            .map(|d| &d.date)
            .collect::<HashSet<_>>()
            .len(),
        error: index.error.clone(),
    };
    drop(index);
    corpus.identity.check(app)?;
    Ok(result)
}

struct IndexGuard {
    app: tauri::AppHandle,
    cancel: Arc<Cancellation>,
}
impl Drop for IndexGuard {
    fn drop(&mut self) {
        let runtime = self.app.state::<RecapSearchRuntime>();
        let mut index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
        if index
            .cancel
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &self.cancel))
        {
            index.cancel = None;
        }
        let _ = self.app.emit("recap-search-progress", ());
    }
}

pub async fn prepare(app: tauri::AppHandle) -> Result<(), String> {
    identity(&app)?;
    let cancel = {
        let runtime = app.state::<RecapSearchRuntime>();
        let mut index = runtime.index.lock().unwrap_or_else(|e| e.into_inner());
        if index.cancel.is_some() {
            return Err("AI_BUSY".into());
        }
        let cancel = Cancellation::new();
        index.cancel = Some(cancel.clone());
        index.error = None;
        cancel
    };
    let _guard = IndexGuard {
        app: app.clone(),
        cancel: cancel.clone(),
    };
    let _ = app.emit("recap-search-progress", ());
    let result = prepare_inner(&app, &cancel).await;
    if let Err(error) = &result {
        app.state::<RecapSearchRuntime>()
            .index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error =
            (!matches!(error.as_str(), "AI_CANCELLED" | "AUTH_REQUIRED")).then(|| error.clone());
    }
    result
}

async fn prepare_inner(app: &tauri::AppHandle, cancel: &Arc<Cancellation>) -> Result<(), String> {
    let load_app = app.clone();
    let corpus = tokio::task::spawn_blocking(move || load_corpus(&load_app))
        .await
        .map_err(|_| "RECAP_WORKER_FAILED")??;
    let days: BTreeMap<&str, Vec<&Document>> =
        corpus
            .documents
            .iter()
            .fold(BTreeMap::new(), |mut days, d| {
                days.entry(&d.date).or_default().push(d);
                days
            });
    for (date, documents) in days {
        let all: BTreeMap<&str, &str> = documents
            .iter()
            .flat_map(|d| &d.chunks)
            .map(|c| (c.key.as_str(), c.text.as_str()))
            .collect();
        let missing: Vec<_> = {
            let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
            all.iter()
                .filter(|(key, _)| !vectors.contains_key(**key))
                .map(|(key, text)| (*key, *text))
                .collect()
        };
        for batch in missing.chunks(ENCODE_BATCH) {
            corpus.identity.check(app)?;
            if cancel.is_cancelled() {
                return Err("AI_CANCELLED".into());
            }
            let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
            let encoding = semantic.embed_text(
                app.clone(),
                MlSemanticModel::MinilmL12,
                batch.iter().map(|(_, text)| text.to_string()).collect(),
                Duration::from_secs(60),
                false,
            );
            let monitor = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    corpus.identity.check(app)?;
                    if cancel.is_cancelled() {
                        return Err::<(), String>("AI_CANCELLED".into());
                    }
                }
            };
            let encoded = tokio::select! {
                result = encoding => result.map_err(|_| "RECAP_SEARCH_MODEL_UNAVAILABLE".to_string())?,
                result = monitor => { result?; unreachable!() }
            };
            corpus.identity.check(app)?;
            if cancel.is_cancelled() {
                return Err("AI_CANCELLED".into());
            }
            if encoded.vectors.len() != batch.len()
                || encoded
                    .vectors
                    .iter()
                    .any(|v| validate_minilm_vector(v).is_err())
            {
                return Err("RECAP_SEARCH_MODEL_UNAVAILABLE".into());
            }
            let saved = {
                let mut vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
                vectors.extend(
                    batch
                        .iter()
                        .zip(encoded.vectors)
                        .map(|((key, _), vector)| (key.to_string(), vector)),
                );
                SavedVectors {
                    version: INDEX_VERSION,
                    model_revision: MINILM_VECTOR_SPACE_REVISION.into(),
                    vectors: all
                        .keys()
                        .filter_map(|key| vectors.get(*key).map(|v| (key.to_string(), v.clone())))
                        .collect(),
                }
            };
            let storage = app.state::<Arc<StorageState>>().inner().clone();
            let (date, revision, generation) = (
                date.to_string(),
                documents[0].revision,
                corpus.identity.generation,
            );
            tokio::task::spawn_blocking(move || {
                storage.recap_write("search", &date, &date, revision, generation, &saved)
            })
            .await
            .map_err(|_| "RECAP_WORKER_FAILED")??;
            let _ = app.emit("recap-search-progress", ());
        }
    }
    corpus.identity.check(app)
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

fn rank(
    corpus: &Corpus,
    query: &RecapSearchQuery,
    embedding: Option<&[f32]>,
) -> (Vec<RecapSearchHit>, usize, usize) {
    let mut keywords = Vec::new();
    let mut semantic = Vec::new();
    let vectors = corpus.vectors.lock().unwrap_or_else(|e| e.into_inner());
    for (i, document) in corpus
        .documents
        .iter()
        .enumerate()
        .filter(|(_, d)| query.includes(&d.date))
    {
        if query.mode != SearchMode::Semantic {
            let score = keyword_score(&document.activity, &query.query, query.include_details);
            if score > 0.0 {
                keywords.push((i, score));
            }
        }
        if let Some(embedding) = embedding {
            let score = document
                .chunks
                .iter()
                .filter(|c| query.include_details || !c.detail)
                .filter_map(|c| vectors.get(&c.key))
                .map(|v| {
                    v.iter()
                        .zip(embedding)
                        .map(|(a, b)| f64::from(a * b))
                        .sum::<f64>()
                })
                .filter(|s| s.is_finite() && *s > 0.0)
                .max_by(f64::total_cmp);
            if let Some(score) = score {
                semantic.push((i, score));
            }
        }
    }
    let indexed = vectors.len();
    drop(vectors);
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
    (hits, total, indexed)
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
            semantic_indexed
        ])
        .to_string(),
    )
}

pub async fn search(
    app: tauri::AppHandle,
    query: RecapSearchQuery,
) -> Result<RecapSearchResult, String> {
    query.validate()?;
    let started = Instant::now();
    let load_app = app.clone();
    let corpus = tokio::task::spawn_blocking(move || load_corpus(&load_app))
        .await
        .map_err(|_| "RECAP_WORKER_FAILED")??;
    let (indexed_chunks, total_chunks) = counts(&corpus);
    let mut semantic_status = if total_chunks == 0 {
        "empty"
    } else if indexed_chunks == 0 {
        "not_prepared"
    } else if indexed_chunks < total_chunks {
        "partial"
    } else {
        "ready"
    };
    let mut embedding = None;
    if query.mode != SearchMode::Keyword && indexed_chunks > 0 {
        let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
        match semantic
            .embed_text(
                app.clone(),
                MlSemanticModel::MinilmL12,
                vec![query.query.trim().into()],
                Duration::from_secs(5),
                false,
            )
            .await
        {
            Ok(result) => {
                embedding = result
                    .vectors
                    .into_iter()
                    .next()
                    .filter(|v| validate_minilm_vector(v).is_ok())
            }
            Err(_) => (),
        }
        if embedding.is_none() {
            semantic_status = "unavailable";
        }
    }
    corpus.identity.check(&app)?;
    if query.mode == SearchMode::Semantic && embedding.is_none() && total_chunks > 0 {
        return Err(if indexed_chunks == 0 {
            "RECAP_SEARCH_NOT_READY"
        } else {
            "RECAP_SEARCH_MODEL_UNAVAILABLE"
        }
        .into());
    }
    let result = tokio::task::spawn_blocking(move || {
        corpus.identity.check(&app)?;
        let (items, total, indexed_chunks) = rank(&corpus, &query, embedding.as_deref());
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
            indexed_chunks,
            total_chunks,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        corpus.identity.check(&app)?;
        Ok(result)
    })
    .await
    .map_err(|_| "RECAP_WORKER_FAILED")?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .map(|(date, activity)| Document {
                date: date.into(),
                revision: 0,
                batch_start_ms: 0,
                chunks: chunks(&activity),
                activity,
            })
            .collect::<Vec<_>>();
        let vectors = BTreeMap::from([
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
