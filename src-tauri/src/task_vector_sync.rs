//! Repairs the Chroma task-vector consumer from Rust before clustering runs.
//!
//! Acknowledged, bounded pages survive process restarts. Every completed pass
//! starts a fresh reconciliation next time, so missing subsets are repaired too.
//! Historical manual ranges use the same Rust encoder and source contract as
//! capture indexing; the ordinary semantic index's retention remains 30 days.

use crate::background_scheduler::{self, BackgroundTaskKind};
use crate::minilm_index::{minilm_sources, MinilmSource};
use crate::minilm_migration::validate_minilm_vector;
use crate::ml_protocol::MlSemanticModel;
use crate::semantic_runtime::{SemanticRuntimeState, BACKGROUND_PASS_GUARD};
use crate::storage::{DerivedEmbeddingWrite, DerivedIndexKind, StorageState};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static PROGRESS: Mutex<Option<ClusteringProgress>> = Mutex::new(None);
static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);
const PAGE_SIZE: u32 = 32;
const AUTO_PAGES: usize = 8;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusteringPhase {
    Preparing,
    WaitingForWorker,
    Clustering,
    WaitingForIndex,
    Paused,
    AwaitingChoice,
    ResultsReady,
    Completed,
    Interrupted,
}

/// Live progress is separate from the acknowledged, durable page cursor.
/// Preparing a record is visible immediately, but only an acknowledged page
/// advances the checkpoint used when retrying an interrupted run.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ClusteringProgress {
    run_id: u64,
    #[serde(skip)]
    generation: u64,
    manual: bool,
    scope: String,
    start_time: f64,
    end_time: f64,
    pub active: bool,
    phase: ClusteringPhase,
    prepared_count: u64,
}

pub(crate) fn progress_status(storage: &StorageState) -> Option<ClusteringProgress> {
    PROGRESS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|progress| progress.generation == storage.db_generation())
        .cloned()
}

#[derive(Debug)]
pub(crate) struct ClusteringRun {
    // Serialize the whole clustering request, without holding the semantic
    // worker slot during Python clustering. This also keeps one run's progress
    // from being replaced while its caller is still waiting for results.
    _sync: tokio::sync::MutexGuard<'static, ()>,
    run_id: u64,
}

impl ClusteringRun {
    fn new(
        sync: tokio::sync::MutexGuard<'static, ()>,
        generation: u64,
        manual: bool,
        scope: &str,
        start_time: f64,
        end_time: f64,
    ) -> Self {
        let run_id = NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed);
        *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = Some(ClusteringProgress {
            run_id,
            generation,
            manual,
            scope: scope.to_string(),
            start_time,
            end_time,
            active: true,
            phase: ClusteringPhase::Preparing,
            prepared_count: 0,
        });
        Self {
            _sync: sync,
            run_id,
        }
    }

    fn update(&self, update: impl FnOnce(&mut ClusteringProgress)) {
        if let Some(progress) = PROGRESS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .filter(|progress| progress.run_id == self.run_id)
        {
            update(progress);
        }
    }

    fn phase(&self, phase: ClusteringPhase) {
        self.update(|progress| progress.phase = phase);
    }

    pub(crate) fn finish(&self, phase: ClusteringPhase) {
        self.update(|progress| {
            progress.active = false;
            progress.phase = phase;
        });
    }
}

impl Drop for ClusteringRun {
    fn drop(&mut self) {
        // All early returns, errors and cancelled futures must stop looking
        // active, even though an unfinished checkpoint remains in the database.
        self.update(|progress| {
            if progress.active {
                progress.active = false;
                progress.phase = ClusteringPhase::Interrupted;
            }
        });
    }
}

pub(crate) fn schedule_repair(app: &AppHandle) {
    if let Some(scheduler) = app.try_state::<Arc<background_scheduler::BackgroundSchedulerState>>()
    {
        if let Err(error) = scheduler.enqueue(app, BackgroundTaskKind::PythonClustering, false) {
            tracing::warn!("Could not schedule task vector repair: {error}");
        }
    }
}

#[derive(Debug)]
pub(crate) enum SyncOutcome {
    Ready(ClusteringRun),
    Busy,
    More,
    WaitingForIndex,
}

fn check_access(
    app: &AppHandle,
    storage: &StorageState,
    generation: u64,
    manual: bool,
) -> Result<(), String> {
    crate::maintenance::guard()?;
    if !background_scheduler::task_feature_enabled(BackgroundTaskKind::PythonClustering, manual) {
        return Err("disabled".into());
    }
    if storage.db_generation() != generation {
        return Err("database changed during task vector synchronization".into());
    }
    if let Some(reason) = background_scheduler::gate_reason_for_kind(
        app,
        manual,
        BackgroundTaskKind::PythonClustering,
    ) {
        return Err(reason.into());
    }
    Ok(())
}

async fn request(app: &AppHandle, mut payload: Value, manual: bool) -> Result<Value, String> {
    payload["background"] = json!(!manual);
    let monitor = app.state::<crate::monitor::MonitorState>();
    let response = crate::monitor::forward_command_to_python(&monitor, payload).await?;
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        return Err(error.into());
    }
    if response.get("status").and_then(Value::as_str) != Some("success") {
        return Err("invalid task vector synchronization response".into());
    }
    Ok(response)
}

pub(crate) async fn synchronize(
    app: &AppHandle,
    manual: bool,
    start: Option<f64>,
    end: Option<f64>,
) -> Result<SyncOutcome, String> {
    let Ok(sync) = SYNC_LOCK.try_lock() else {
        return Ok(SyncOutcome::Busy);
    };
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let generation = storage.db_generation();
    check_access(app, &storage, generation, manual)?;
    let now = chrono::Utc::now().timestamp() as f64;
    let scope = if start.is_none() && end.is_none() {
        "recent".to_string()
    } else {
        format!("range:{}:{}", start.unwrap_or(0.0), end.unwrap_or(now))
    };
    let begin = start.unwrap_or(if scope == "recent" {
        now - 30.0 * 86400.0
    } else {
        0.0
    });
    let finish = end.unwrap_or(now);
    if !begin.is_finite() || !finish.is_finite() || begin < 0.0 || finish < begin {
        return Err("invalid clustering time range".into());
    }
    let progress = ClusteringRun::new(sync, generation, manual, &scope, begin, finish);
    let target = request(
        app,
        json!({"command":"get_task_vector_sync_target"}),
        manual,
    )
    .await?
    .get("target")
    .and_then(Value::as_str)
    .filter(|s| !s.is_empty())
    .ok_or("missing task vector synchronization target")?
    .to_string();
    let mut state = storage.begin_task_vector_sync(generation, &scope, &target, begin, finish)?;
    progress.update(|progress| {
        progress.start_time = state.start_time;
        progress.end_time = state.end_time;
        progress.prepared_count = state.synced_count;
    });
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut pages = 0;
    loop {
        check_access(app, &storage, generation, manual)?;
        if Instant::now() >= deadline {
            return Err("task vector synchronization timed out; retry to resume".into());
        }
        let mut selection = state.clone();
        if scope == "recent" {
            selection.start_time = selection
                .start_time
                .max(chrono::Utc::now().timestamp() as f64 - 30.0 * 86400.0);
        }
        let ids = storage.task_vector_sync_page(generation, &selection, PAGE_SIZE)?;
        if ids.is_empty() {
            storage.acknowledge_task_vector_sync(generation, &state, state.upper_id, 0, true)?;
            progress.phase(ClusteringPhase::Clustering);
            return Ok(SyncOutcome::Ready(progress));
        }
        let Some(records) =
            prepare_page(app, storage.clone(), generation, &ids, manual, &progress).await?
        else {
            if let Some(scheduler) =
                app.try_state::<Arc<background_scheduler::BackgroundSchedulerState>>()
            {
                scheduler.enqueue(app, BackgroundTaskKind::SemanticIndex, false)?;
            }
            progress.finish(ClusteringPhase::WaitingForIndex);
            return Ok(SyncOutcome::WaitingForIndex);
        };
        check_access(app, &storage, generation, manual)?;
        if !records.is_empty() {
            let response = request(
                app,
                json!({
                    "command":"upsert_task_vectors", "target":target, "records":records,
                }),
                manual,
            )
            .await?;
            if response.get("upserted").and_then(Value::as_u64) != Some(records.len() as u64) {
                return Err("task vector synchronization page was not fully acknowledged".into());
            }
        }
        storage.acknowledge_task_vector_sync(
            generation,
            &state,
            *ids.last().unwrap(),
            records.len(),
            false,
        )?;
        state.cursor = *ids.last().unwrap();
        state.synced_count += records.len() as u64;
        pages += 1;
        if !manual && pages >= AUTO_PAGES {
            progress.finish(ClusteringPhase::Paused);
            return Ok(SyncOutcome::More);
        }
    }
}

async fn prepare_page(
    app: &AppHandle,
    storage: Arc<StorageState>,
    generation: u64,
    ids: &[i64],
    manual: bool,
    progress: &ClusteringRun,
) -> Result<Option<Vec<Value>>, String> {
    let read_storage = storage.clone();
    let ids = ids.to_vec();
    let sources = tokio::task::spawn_blocking(move || {
        let _activity = read_storage.foreground_db_read();
        if read_storage.db_generation() != generation {
            return Err("database changed".to_string());
        }
        let sources = minilm_sources(&read_storage, &ids).map_err(|e| e.to_string())?;
        let mut prepared = Vec::new();
        for id in ids {
            if let Some(source) = sources.indexable.get(&id) {
                let cached = read_storage
                    .get_query_visible_embedding(
                        DerivedIndexKind::SemanticText,
                        &source.spec.subject_key,
                    )?
                    .filter(|row| {
                        row.job == source.spec && validate_minilm_vector(&row.vector).is_ok()
                    })
                    .map(|row| row.vector);
                if cached.is_none() && !manual {
                    read_storage.ensure_derived_index_job(&source.spec)?;
                }
                prepared.push((source.clone(), cached));
            }
        }
        Ok::<_, String>(prepared)
    })
    .await
    .map_err(|e| e.to_string())??;
    if !manual && sources.iter().any(|(_, vector)| vector.is_none()) {
        return Ok(None);
    }
    let mut records = Vec::with_capacity(sources.len());
    for (source, cached) in sources {
        let vector = if let Some(vector) = cached {
            vector
        } else {
            check_access(app, &storage, generation, manual)?;
            encode_source(app, storage.clone(), generation, &source, progress).await?
        };
        records.push(json!({
            "id":source.spec.subject_key, "embedding":vector,
            "timestamp":source.summary.timestamp.unwrap_or(0),
            "process_name":source.summary.process_name.unwrap_or_default(),
            "window_title":source.summary.window_title.unwrap_or_default(),
            "category":source.summary.category.unwrap_or_default(), "document":source.text,
        }));
        progress.update(|progress| progress.prepared_count += 1);
    }
    Ok(Some(records))
}

async fn encode_source(
    app: &AppHandle,
    storage: Arc<StorageState>,
    generation: u64,
    source: &MinilmSource,
    progress: &ClusteringRun,
) -> Result<Vec<f32>, String> {
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let deadline = Instant::now() + Duration::from_secs(120);
    // Manual history rebuilding participates in the existing worker arbitration.
    // Drop the slot after every record so foreground search and classification
    // can proceed between requests, rather than after an entire history range.
    let mut waiting = false;
    let guard = loop {
        check_access(app, &storage, generation, true)?;
        if Instant::now() >= deadline {
            return Err("semantic worker busy; retry clustering to resume".into());
        }
        if !semantic.foreground_waiting() {
            if let Ok(guard) = BACKGROUND_PASS_GUARD.try_lock() {
                break guard;
            }
        }
        if !waiting {
            progress.phase(ClusteringPhase::WaitingForWorker);
            waiting = true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    progress.phase(ClusteringPhase::Preparing);
    let result = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::MinilmL12,
            vec![source.text.clone()],
            Duration::from_secs(120),
            false,
        )
        .await?;
    if result.vectors.len() != 1 {
        return Err("invalid MiniLM batch response".into());
    }
    let vector = result.vectors.into_iter().next().unwrap();
    validate_minilm_vector(&vector)?;
    check_access(app, &storage, generation, true)?;
    let source = source.clone();
    let saved = vector.clone();
    tokio::task::spawn_blocking(move || {
        let _activity = storage.foreground_db_read();
        if storage.db_generation() != generation {
            return Err("database changed".to_string());
        }
        let id = source.summary.id;
        let current = minilm_sources(&storage, &[id]).map_err(|e| e.to_string())?;
        if current.indexable.get(&id).map(|s| &s.spec) != Some(&source.spec) {
            return Err(
                "screenshot changed during task vector synchronization; retry to resume".into(),
            );
        }
        // The existing source/model contract and worker lease keep this write
        // transactional and prevent a deleted or replaced subject from reviving.
        if storage.ensure_derived_index_job(&source.spec)?
            == crate::storage::EnsureDerivedIndexJobResult::AlreadyProcessing
        {
            return Err("semantic index subject is busy; retry to resume".into());
        }
        if let Some(row) = storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, &source.spec.subject_key)?
        {
            if row.job == source.spec && validate_minilm_vector(&row.vector).is_ok() {
                return Ok(());
            }
        }
        storage.upsert_derived_index_job(&source.spec)?;
        let lease_token = storage.mark_derived_index_job_processing(&source.spec)?;
        storage.commit_derived_embedding(&DerivedEmbeddingWrite {
            job: source.spec,
            lease_token,
            vector: saved,
        })?;
        storage.enqueue_smart_cluster_pending(id)?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    drop(guard);
    Ok(vector)
}
