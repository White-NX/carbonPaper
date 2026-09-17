//! Native category classification, learned anchors and screenshot postprocessing.
pub(crate) mod scoring;

use crate::processing_stage::StagedWork;
use crate::storage::{ClassificationFeedback, ClassificationLease, StorageState};
use scoring::{Classification, Classifier, Input, TextEncoder};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct Cache {
    generation: u64,
    revision: i64,
    classifier: Classifier,
}

pub(crate) struct ClassificationState {
    cache: tokio::sync::Mutex<Option<Cache>>,
    capacity: Arc<Semaphore>,
    active: Mutex<HashSet<(u64, i64)>>,
    feedback_running: AtomicBool,
}

impl Default for ClassificationState {
    fn default() -> Self {
        Self {
            cache: tokio::sync::Mutex::new(None),
            capacity: Arc::new(Semaphore::new(8)),
            active: Mutex::new(HashSet::new()),
            feedback_running: AtomicBool::new(false),
        }
    }
}

struct JobPermit {
    state: Arc<ClassificationState>,
    key: (u64, i64),
    _capacity: OwnedSemaphorePermit,
}
impl Drop for JobPermit {
    fn drop(&mut self) {
        self.state
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

impl ClassificationState {
    fn permit(self: &Arc<Self>, generation: u64, id: i64) -> Option<JobPermit> {
        let capacity = self.capacity.clone().try_acquire_owned().ok()?;
        let key = (generation, id);
        if !self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key)
        {
            return None;
        }
        Some(JobPermit {
            state: self.clone(),
            key,
            _capacity: capacity,
        })
    }
}

struct NativeEncoder {
    app: AppHandle,
    generation: u64,
    deadline: tokio::time::Instant,
}
impl TextEncoder for NativeEncoder {
    async fn encode(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        crate::maintenance::guard()?;
        if self.app.state::<Arc<StorageState>>().db_generation() != self.generation {
            return Err("classification database changed".into());
        }
        let result = tokio::time::timeout_at(
            self.deadline,
            crate::classification_runtime::embed_bge_texts(self.app.clone(), texts),
        )
        .await
        .map_err(|_| "classification inference timed out".to_string())??;
        Ok(result.vectors)
    }
}

async fn refresh_cache(
    storage: Arc<StorageState>,
    cache: &mut Option<Cache>,
) -> Result<(), String> {
    let snapshot = tokio::task::spawn_blocking(move || storage.load_classification_anchors())
        .await
        .map_err(|e| e.to_string())??;
    if !cache
        .as_ref()
        .is_some_and(|c| c.generation == snapshot.generation && c.revision == snapshot.revision)
    {
        *cache = Some(Cache {
            generation: snapshot.generation,
            revision: snapshot.revision,
            classifier: Classifier::new(snapshot.anchors)?,
        });
    }
    Ok(())
}

async fn classify(
    app: &AppHandle,
    input: &Input,
    debug: bool,
    generation: u64,
) -> Result<Classification, String> {
    let state = app.state::<Arc<ClassificationState>>();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let mut cache = state.cache.lock().await;
    refresh_cache(storage.clone(), &mut cache).await?;
    let current = cache.as_mut().unwrap();
    if current.generation != generation {
        return Err("classification database changed".into());
    }
    let mut encoder = NativeEncoder {
        app: app.clone(),
        generation,
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(120),
    };
    let result = current
        .classifier
        .classify(&mut encoder, input, debug)
        .await?;
    if storage.db_generation() != generation {
        return Err("classification database changed".into());
    }
    Ok(result)
}

pub(crate) async fn debug(app: &AppHandle, input: Input) -> Result<Value, String> {
    let generation = app.state::<Arc<StorageState>>().db_generation();
    let mut result = classify(app, &input, true, generation).await?.debug;
    result["status"] = json!("success");
    Ok(result)
}

pub(crate) async fn remove_local(
    app: &AppHandle,
    category: &str,
    process: &str,
) -> Result<usize, String> {
    crate::maintenance::guard()?;
    let state = app.state::<Arc<ClassificationState>>();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let mut cache = state.cache.lock().await;
    refresh_cache(storage.clone(), &mut cache).await?;
    let current = cache.as_mut().unwrap();
    let count = current.classifier.remove_local(category, process);
    if count > 0 {
        match storage.save_classification_anchors(
            current.generation,
            current.revision,
            &current.classifier.anchors,
        ) {
            Ok(revision) => current.revision = revision,
            Err(error) => {
                *cache = None;
                return Err(error);
            }
        }
    }
    Ok(count)
}

fn scheduling_deferred(error: &str) -> bool {
    [
        "foreground_busy",
        "background_busy",
        "foreground_request",
        "MAINTENANCE_IN_PROGRESS",
        "maintenance",
        "classification source changed",
        "classification database changed",
        "authentication required",
    ]
    .iter()
    .any(|prefix| error.starts_with(prefix))
}

fn immediate_gate(app: &AppHandle) -> Result<(), String> {
    if let Some(reason) = crate::background_scheduler::environment_gate_reason(
        app,
        crate::background_scheduler::EnvironmentPolicy::Immediate,
    ) {
        return Err(reason.into());
    }
    Ok(())
}

pub(crate) fn enqueue_capture(app: &AppHandle, id: i64, input: Input) -> Result<bool, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let state = app.state::<Arc<ClassificationState>>().inner().clone();
    let generation = storage.db_generation();
    let Some(permit) = state.permit(generation, id) else {
        return Ok(false);
    };
    let Some(lease) = storage.claim_native_classification(generation, id)? else {
        return Ok(false);
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _permit = permit;
        let result = run_capture(&app, &storage, &lease, &input).await;
        if let Err(error) = result {
            let deferred = scheduling_deferred(&error);
            let _ = storage.defer_native_classification(&lease, &error, deferred);
            crate::classification_runtime::record_pipeline_error(&error);
            if deferred {
                tracing::debug!(
                    "[CLASSIFICATION] capture deferred=true screenshot_id={} error={}",
                    id,
                    error
                );
            } else {
                tracing::warn!(
                    "[CLASSIFICATION] capture failed screenshot_id={} error={}",
                    id,
                    error
                );
            }
        }
    });
    Ok(true)
}

async fn run_capture(
    app: &AppHandle,
    storage: &StorageState,
    lease: &ClassificationLease,
    input: &Input,
) -> Result<(), String> {
    if !storage.start_native_classification(lease)? {
        storage.defer_native_classification(lease, "classification source changed", true)?;
        return Ok(());
    }
    immediate_gate(app)?;
    let result = if lease.user_revision == 0
        && crate::registry_config::get_bool("classification_enabled").unwrap_or(true)
    {
        Some(classify(app, input, false, lease.generation).await?)
    } else {
        None
    };
    let result = result
        .filter(|_| crate::registry_config::get_bool("classification_enabled").unwrap_or(true));
    storage.commit_native_classification(
        lease,
        result.as_ref().map(|r| r.category.as_str()),
        result.as_ref().map(|r| scoring::round_score(r.score)),
    )?;
    Ok(())
}

pub(crate) fn enqueue_staged(app: &AppHandle, work: StagedWork) -> Result<bool, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let state = app.state::<Arc<ClassificationState>>().inner().clone();
    storage.processing_stage.check_receipt(&work.receipt)?;
    let Some(permit) = state.permit(work.receipt.db_generation, work.receipt.screenshot_id) else {
        return Ok(false);
    };
    let user_revision = storage.classification_user_revision(work.receipt.screenshot_id)?;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _permit = permit;
        let input = Input {
            title: work.input.window_title.clone(),
            ocr_text: work.input.ocr_text.clone(),
            process_name: work.input.process_name.clone(),
        };
        let result = async {
            immediate_gate(&app)?;
            storage.processing_stage.check_receipt(&work.receipt)?;
            let result = if user_revision == 0
                && crate::registry_config::get_bool("classification_enabled").unwrap_or(true)
            {
                Some(classify(&app, &input, false, work.receipt.db_generation).await?)
            } else {
                None
            };
            let result = result.filter(|_| {
                crate::registry_config::get_bool("classification_enabled").unwrap_or(true)
            });
            let first = storage.commit_staged_category_if_user_unchanged(
                &work.receipt,
                result.as_ref().map(|r| r.category.as_str()),
                result.as_ref().map(|r| scoring::round_score(r.score)),
                Some(user_revision),
            )?;
            let _ = storage.processing_stage.finish(&storage, &work.receipt);
            let pending = storage
                .pending_staged_classification_receipt_count()
                .unwrap_or(u64::from(first));
            crate::background_activity::classification_committed(first, pending);
            Ok::<_, String>(())
        }
        .await;
        if let Err(error) = result {
            let deferred = scheduling_deferred(&error);
            let _ = storage.processing_stage.release(&work.receipt, !deferred);
            crate::background_activity::classification_deferred();
            crate::classification_runtime::record_pipeline_error(&error);
            if deferred {
                tracing::debug!("[CLASSIFICATION] staged processing deferred: {}", error);
            } else {
                tracing::warn!("[CLASSIFICATION] staged processing failed: {}", error);
            }
        }
    });
    Ok(true)
}

struct FeedbackGuard(Arc<ClassificationState>);
impl Drop for FeedbackGuard {
    fn drop(&mut self) {
        self.0.feedback_running.store(false, Ordering::Release);
    }
}

pub(crate) fn start_feedback(app: &AppHandle) {
    let state = app.state::<Arc<ClassificationState>>().inner().clone();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    if !storage.is_silent_read_authorized() || state.feedback_running.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _guard = FeedbackGuard(state.clone());
        for _ in 0..8 {
            if !storage.is_silent_read_authorized() || immediate_gate(&app).is_err() {
                break;
            }
            let generation = storage.db_generation();
            let feedback = match storage.next_classification_feedback() {
                Ok(Some(feedback)) => feedback,
                Ok(None) => break,
                Err(error) => {
                    crate::classification_runtime::record_pipeline_error(&error);
                    break;
                }
            };
            if let Err(error) =
                learn_feedback(&app, storage.clone(), &state, generation, &feedback).await
            {
                let deferred = scheduling_deferred(&error);
                let _ =
                    storage.defer_classification_feedback(generation, &feedback, &error, deferred);
                crate::classification_runtime::record_pipeline_error(&error);
                if deferred {
                    tracing::debug!("[CLASSIFICATION] feedback learning deferred: {}", error);
                } else {
                    tracing::warn!("[CLASSIFICATION] feedback learning failed: {}", error);
                }
                break;
            }
        }
    });
}

async fn learn_feedback(
    app: &AppHandle,
    storage: Arc<StorageState>,
    state: &ClassificationState,
    generation: u64,
    feedback: &ClassificationFeedback,
) -> Result<(), String> {
    let read_storage = storage.clone();
    let id = feedback.screenshot_id;
    let revision = feedback.source_revision;
    let input = tokio::task::spawn_blocking(move || {
        let _activity = read_storage.foreground_db_read();
        if read_storage.db_generation() != generation
            || read_storage.staged_source_revision(id)? != revision
        {
            return Err("feedback source changed".to_string());
        }
        let summary = read_storage
            .get_screenshot_summaries_by_ids_silent(&[id])
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .ok_or("feedback screenshot is no longer active")?;
        let ocr = read_storage
            .get_screenshot_ocr_results_silent(id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.text)
            .collect::<Vec<_>>()
            .join(" ");
        Ok::<_, String>(Input {
            title: summary.window_title.unwrap_or_default(),
            process_name: summary.process_name.unwrap_or_default(),
            ocr_text: ocr,
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut cache = state.cache.lock().await;
    refresh_cache(storage.clone(), &mut cache).await?;
    let current = cache.as_mut().unwrap();
    if current.generation != generation {
        return Err("classification database changed".into());
    }
    let mut encoder = NativeEncoder {
        app: app.clone(),
        generation,
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(120),
    };
    current
        .classifier
        .learn(
            &mut encoder,
            &feedback.category,
            &input,
            feedback.old_category.as_deref(),
        )
        .await?;
    if !storage.is_silent_read_authorized() {
        *cache = None;
        return Err("authentication required".into());
    }
    match storage.commit_classification_feedback(
        generation,
        current.revision,
        &current.classifier.anchors,
        feedback,
    ) {
        Ok(revision) => current.revision = revision,
        Err(error) => {
            *cache = None;
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_admission_is_bounded_and_deduplicated_until_the_job_drops() {
        let state = Arc::new(ClassificationState::default());
        let mut permits = Vec::new();
        for id in 1..=8 {
            permits.push(state.permit(0, id).unwrap());
        }
        assert!(state.permit(0, 9).is_none());
        assert!(state.permit(0, 1).is_none());
        permits.remove(0);
        let replacement = state.permit(0, 1).unwrap();
        drop(replacement);
        drop(permits);
        assert_eq!(state.capacity.available_permits(), 8);
        assert!(state.active.lock().unwrap().is_empty());
    }

    #[test]
    fn scheduling_yields_preserve_retry_budgets_but_inference_failures_do_not() {
        for error in [
            "foreground_busy: search",
            "background_busy: indexing",
            "foreground_request",
            "MAINTENANCE_IN_PROGRESS",
            "authentication required",
        ] {
            assert!(scheduling_deferred(error), "{error}");
        }
        for error in [
            "model missing",
            "invalid weighted classification score",
            "classification inference timed out",
        ] {
            assert!(!scheduling_deferred(error), "{error}");
        }
    }
}
