//! Scoring of broker-authorized capture inputs. No archive text reader is used
//! here, including when an old threshold still needs calibration examples.
use super::*;
use crate::processing_stage::StagedWork;
use carbonpaper_app_bound::protocol::Consumer;
use std::future::Future;

pub(crate) struct StagedCommit {
    pub committed: bool,
    pub assignments: u64,
    pub needs_archive: bool,
}

/// A scheduler deferral, rather than a successful zero-work slice which would
/// immediately wake itself while a dependency or retry timer is outstanding.
pub(crate) fn waiting_reason(storage: &StorageState) -> Result<Option<&'static str>, String> {
    if !storage.staged_smart_cluster_pending_ids(1)?.is_empty() {
        return Ok(None);
    }
    if storage.is_silent_read_authorized()
        && !storage.peek_smart_cluster_pending_batch(1)?.is_empty()
    {
        return Ok(None);
    }
    if storage.processing_stage.has_ready(Consumer::SmartCluster) {
        return Ok(Some("waiting_for_index"));
    }
    if !storage
        .processing_stage
        .owned_screenshot_ids(Consumer::SmartCluster)?
        .is_empty()
    {
        return Ok(Some("retry_wait"));
    }
    Ok((!storage.is_silent_read_authorized()).then_some("waiting_for_unlock"))
}

fn current_targets(
    targets: &[SmartClusterScoringTarget],
    scorer: &ScorerIdentity,
) -> (Vec<SmartClusterScoringTarget>, bool, usize) {
    let fingerprint = scorer.fingerprint();
    let mut usable = Vec::new();
    let mut needs_archive = false;
    let mut unverifiable = 0;
    for target in targets {
        if scorer.matches_stored(
            Some(&target.scorer.model_id),
            Some(&target.scorer.model_revision),
            Some(&target.scorer.variant),
            Some(&target.scorer.provider),
        ) {
            usable.push(target.clone());
        } else {
            unverifiable += 1;
            // A previously established permanent failure follows the archive
            // worker's policy. Every other mismatch retains calibration debt.
            needs_archive |= target.rederive_failed_scorer.as_deref() != Some(&fingerprint);
        }
    }
    (usable, needs_archive, unverifiable)
}

/// The production scoring/commit path with an injectable inference operation.
/// Tests use the real storage and broker ledger while keeping model logits
/// deterministic. The callback receives exactly the ordinary rerank document.
pub(crate) async fn score_and_commit<F, Fut, S>(
    storage: &StorageState,
    work: &StagedWork,
    targets: &[SmartClusterScoringTarget],
    anchors: &HashMap<i64, Vec<f32>>,
    mut rerank: F,
    mut stop: S,
) -> Result<StagedCommit, String>
where
    F: FnMut(String, Vec<String>) -> Fut,
    Fut: Future<Output = Result<Vec<f32>, String>>,
    S: FnMut() -> Option<&'static str>,
{
    let check_stop = |reason: Option<&str>| -> Result<(), String> {
        match reason {
            Some(reason) => Err(format!("deferred: {reason}")),
            None => Ok(()),
        }
    };
    check_stop(stop())?;
    storage.processing_stage.check_receipt(&work.receipt)?;
    if work.receipt.consumer != Consumer::SmartCluster
        || !storage.staged_source_is_current(&work.receipt)?
    {
        return Err("deferred: staged source changed".into());
    }
    let id = work.receipt.screenshot_id;
    let input = &work.input;
    let text = crate::minilm_migration::build_minilm_task_text(
        &input.process_name,
        &input.window_title,
        &input.ocr_text,
    );
    let expected = crate::minilm_migration::minilm_job_spec(id, &text);
    let record = storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, &id.to_string())?
        .filter(|record| record.job == expected)
        .ok_or("deferred: waiting_for_index")?;
    crate::minilm_migration::validate_minilm_vector(&record.vector)?;
    let document = build_rerank_document(&input.process_name, &input.window_title, &input.ocr_text);
    let documents = HashMap::from([(id, document)]);
    let vectors = HashMap::from([(id, record.vector)]);
    let (usable, needs_archive, _) = current_targets(targets, &ScorerIdentity::current());
    let mut assignments = Vec::new();
    for target in &usable {
        check_stop(stop())?;
        let anchor = anchors
            .get(&target.id)
            .ok_or("model_mismatch: missing anchor vector")?;
        let candidates = prefilter(anchor, &vectors, &documents);
        if candidates.is_empty() {
            continue;
        }
        let docs = candidates.iter().map(|id| documents[id].clone()).collect();
        let scores = rerank(target.anchor_text.clone(), docs).await?;
        for (_, score) in matching_scores(target, &candidates, &scores)? {
            assignments.push((target.id, score));
        }
    }
    check_stop(stop())?;
    let committed =
        storage.commit_staged_smart_cluster(&work.receipt, targets, &assignments, needs_archive)?;
    Ok(StagedCommit {
        committed,
        assignments: if committed {
            assignments.len() as u64
        } else {
            0
        },
        needs_archive,
    })
}

pub(super) async fn run_batch(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    storage: Arc<StorageState>,
    forced: bool,
) -> Result<Option<BatchProgress>, String> {
    if !forced
        && !crate::background_scheduler::prefer_staged(
            app,
            crate::background_scheduler::BackgroundTaskKind::SmartCluster,
            false,
        )
    {
        return Ok(None);
    }
    let (ids, targets) = tokio::task::spawn_blocking({
        let storage = storage.clone();
        move || -> Result<_, String> {
            let targets = storage.list_smart_cluster_scoring_targets()?;
            let ids = storage.staged_smart_cluster_pending_ids(if forced {
                batch_size_for(true, targets.len()) as usize
            } else {
                1
            })?;
            Ok((ids, targets))
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    if ids.is_empty() {
        return Ok(None);
    }

    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let (usable, _, unverifiable) = current_targets(&targets, &ScorerIdentity::current());
    state
        .unverifiable_thresholds
        .store(unverifiable as u64, Ordering::SeqCst);
    let anchors = embed_anchors(app, &semantic, storage.clone(), &usable).await?;
    let mut progress = BatchProgress::completed(true);
    // One task holds a lease at a time, so interruption/retry never loses an
    // earlier screenshot's completed comparisons or consumes other leases.
    for id in ids {
        if let Some(reason) = stand_down_reason(app, state, forced) {
            progress.stopped_because = Some(reason);
            break;
        }
        let work = tokio::task::spawn_blocking({
            let storage = storage.clone();
            move || {
                storage
                    .processing_stage
                    .claim_screenshot(&storage, Consumer::SmartCluster, id)
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        let Some(work) = work else { continue };
        let result = score_and_commit(
            &storage,
            &work,
            &targets,
            &anchors,
            |query, docs| {
                let app = app.clone();
                let semantic = semantic.clone();
                async move {
                    crate::rerank::rerank_documents(
                        &app,
                        &semantic,
                        &query,
                        &docs,
                        crate::rerank::RerankBudget::Total(RERANK_TIMEOUT),
                        SCORING_PRIORITY,
                        None,
                    )
                    .await
                }
            },
            || stand_down_reason(app, state, forced),
        )
        .await;
        match result {
            Ok(outcome) => {
                if outcome.committed {
                    progress.staged_completed += 1;
                    progress.deleted += u64::from(!outcome.needs_archive);
                    state
                        .assigned_total
                        .fetch_add(outcome.assignments, Ordering::SeqCst);
                    crate::background_activity::index_progress(1);
                }
                let receipt = work.receipt.clone();
                let finish = tokio::task::spawn_blocking({
                    let storage = storage.clone();
                    move || storage.processing_stage.finish(&storage, &receipt)
                })
                .await
                .map_err(|e| e.to_string())?;
                if let Err(error) = finish {
                    tracing::debug!(
                        "[SMART_CLUSTER] staged receipt awaits acknowledgement: {error}"
                    );
                }
                if forced {
                    let remaining = storage.count_smart_cluster_pending()?.max(0) as u64;
                    state.report_processed(
                        u64::from(outcome.committed && !outcome.needs_archive),
                        remaining,
                    );
                    state.emit_progress(app);
                }
            }
            Err(error) => {
                let yielded = crate::rerank::is_yield(&error);
                let failed = !yielded
                    && !error.starts_with("deferred:")
                    && !crate::background_policy::is_pause(&error);
                let receipt = work.receipt.clone();
                let release = tokio::task::spawn_blocking({
                    let storage = storage.clone();
                    move || storage.processing_stage.release(&receipt, failed)
                })
                .await
                .map_err(|e| e.to_string())?;
                if let Err(error) = release {
                    tracing::debug!("[SMART_CLUSTER] staged lease awaits reconciliation: {error}");
                }
                if failed {
                    tracing::warn!(
                        "[SMART_CLUSTER] staged task failed screenshot_id={id}: {error}"
                    );
                }
                if let Some(reason) = stand_down_reason(app, state, forced)
                    .or(yielded.then_some(FOREGROUND_QUERY_STOP))
                {
                    progress.stopped_because = Some(reason);
                    break;
                }
            }
        }
    }
    progress.more = storage.count_smart_cluster_pending()? > 0;
    Ok(Some(progress))
}
