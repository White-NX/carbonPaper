use super::*;
use crate::{
    background_scheduler::BackgroundTaskKind,
    minilm_migration::{build_minilm_task_text, minilm_job_spec, MINILM_DIMENSIONS},
    rerank::{build_rerank_document, ScorerIdentity},
    smart_cluster_scoring::staged::score_and_commit,
    storage::{
        smart_cluster::SmartClusterScorer, DerivedEmbeddingWrite, DerivedIndexJobSpec,
        DerivedIndexKind,
    },
};
use std::{collections::HashMap, future::ready};

fn vector() -> Vec<f32> {
    let mut v = vec![0.0; MINILM_DIMENSIONS];
    v[0] = 1.0;
    v
}

fn spec(id: i64, input: &ProcessingInput) -> DerivedIndexJobSpec {
    minilm_job_spec(
        id,
        &build_minilm_task_text(&input.process_name, &input.window_title, &input.ocr_text),
    )
}

fn write(storage: &StorageState, spec: DerivedIndexJobSpec) -> DerivedEmbeddingWrite {
    storage.ensure_derived_index_job(&spec).unwrap();
    let lease_token = storage.mark_derived_index_job_processing(&spec).unwrap();
    DerivedEmbeddingWrite {
        job: spec,
        lease_token,
        vector: vector(),
    }
}

fn indexed_input(storage: &StorageState, id: i64, consumers: u8) -> ProcessingInput {
    let input = insert_input(storage, id, consumers | Consumer::MiniLm.bit());
    let work = storage
        .processing_stage
        .claim_screenshot(storage, Consumer::MiniLm, id)
        .unwrap()
        .unwrap();
    storage
        .commit_staged_embedding(&write(storage, spec(id, &input)), &work.receipt)
        .unwrap();
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    input
}

fn current_scorer() -> SmartClusterScorer {
    let scorer = ScorerIdentity::current();
    SmartClusterScorer {
        model_id: scorer.model_id,
        model_revision: scorer.model_revision,
        variant: scorer.variant,
        provider: scorer.provider,
    }
}

fn cluster(storage: &StorageState, text: &str, current: bool) -> i64 {
    let id = storage.create_smart_cluster(text, 2.0, None, None).unwrap();
    if current {
        storage
            .update_smart_cluster_threshold_with_scorer(id, 2.0, &current_scorer())
            .unwrap();
    }
    id
}

fn claim(storage: &StorageState, id: i64) -> crate::processing_stage::StagedWork {
    storage
        .processing_stage
        .claim_screenshot(storage, Consumer::SmartCluster, id)
        .unwrap()
        .unwrap()
}

fn assignment_count(storage: &StorageState) -> i64 {
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM smart_cluster_assignments", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn smart_cluster_completes_after_other_consumers_on_a_locked_restart() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    let input = indexed_input(storage, 1, Consumer::ALL_MASK);
    let category = storage
        .processing_stage
        .claim(storage, Consumer::Classification)
        .unwrap()
        .unwrap();
    storage
        .commit_staged_category(&category.receipt, Some("Development"), Some(1.2))
        .unwrap();
    storage
        .processing_stage
        .finish(storage, &category.receipt)
        .unwrap();
    let clip = storage
        .processing_stage
        .claim(storage, Consumer::Clip)
        .unwrap()
        .unwrap();
    storage
        .commit_staged_embedding(
            &write(
                storage,
                crate::clip_migration::clip_job_spec(&input.image_hash),
            ),
            &clip.receipt,
        )
        .unwrap();
    storage
        .processing_stage
        .finish(storage, &clip.receipt)
        .unwrap();
    let Response::TaskState(state) = broker
        .call(Request::InspectTask {
            task_id: clip.receipt.task_id.clone(),
        })
        .unwrap()
    else {
        panic!("expected task state")
    };
    assert!(state.active);
    assert_eq!(state.finished_consumers, Consumer::LEGACY_MASK);
    restart_stage(dir, storage, broker);

    let id = cluster(storage, "development", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let anchors = HashMap::from([(id, vector())]);
    assert_eq!(storage.staged_smart_cluster_pending_ids(32).unwrap(), [1]);
    assert!(crate::processing_stage::ready_for_kind(
        storage,
        BackgroundTaskKind::SmartCluster
    ));
    assert!(storage
        .get_screenshot_summaries_by_ids_silent(&[1])
        .is_err());
    assert!(storage
        .get_ocr_text_prefixes_by_screenshot_ids_silent(&[1], 100)
        .is_err());
    let work = claim(storage, 1);
    let expected_doc =
        build_rerank_document(&input.process_name, &input.window_title, &input.ocr_text);
    let result = score_and_commit(
        storage,
        &work,
        &targets,
        &anchors,
        |query, docs| {
            assert_eq!(query, "development");
            assert_eq!(docs, [expected_doc.clone()]);
            ready(Ok(vec![4.0]))
        },
        || None,
    )
    .await
    .unwrap();
    assert!(result.committed);
    assert!(!result.needs_archive);
    assert_eq!(result.assignments, 1);
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 0);
    assert_eq!(assignment_count(storage), 1);
    assert!(!storage.is_session_valid());
    assert!(!storage.is_background_authorized());
    assert!(get_cached_master_key(&storage.credential_state).is_none());
    let Response::TaskState(state) = broker
        .call(Request::InspectTask {
            task_id: work.receipt.task_id,
        })
        .unwrap()
    else {
        panic!("expected task state")
    };
    assert!(state.retired);
    assert_eq!(state.finished_consumers, Consumer::ALL_MASK);
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
}

#[tokio::test]
async fn no_matches_prefilter_rejection_and_no_enabled_clusters_complete_normally() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    let id = cluster(storage, "development", true);
    for screenshot in 1..=3 {
        indexed_input(storage, screenshot, Consumer::SmartCluster.bit());
        if screenshot == 3 {
            storage.update_smart_cluster_enabled(id, false).unwrap();
        }
        let targets = storage.list_smart_cluster_scoring_targets().unwrap();
        let mut anchor = vector();
        if screenshot == 2 {
            anchor[0] = -1.0;
        }
        let anchors = HashMap::from([(id, anchor)]);
        let work = claim(storage, screenshot);
        let calls = std::cell::Cell::new(0);
        let result = score_and_commit(
            storage,
            &work,
            &targets,
            &anchors,
            |_, _| {
                calls.set(calls.get() + 1);
                ready(Ok(vec![1.0]))
            },
            || None,
        )
        .await
        .unwrap();
        assert_eq!(calls.get(), usize::from(screenshot == 1));
        assert!(result.committed);
        assert!(!result.needs_archive);
        assert_eq!(result.assignments, 0);
        storage
            .processing_stage
            .finish(storage, &work.receipt)
            .unwrap();
    }
    assert_eq!(assignment_count(storage), 0);
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 0);
}

#[tokio::test]
async fn mixed_thresholds_retain_archive_debt_without_blocking_new_staged_inputs() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::MiniLm.bit()); // Old capture without a SmartCluster grant.
    indexed_input(storage, 2, Consumer::SmartCluster.bit());
    indexed_input(storage, 3, Consumer::SmartCluster.bit());
    let valid = cluster(storage, "current", true);
    let legacy = cluster(storage, "needs calibration", false);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let anchors = HashMap::from([(valid, vector())]);
    assert_eq!(storage.peek_smart_cluster_pending_batch(32).unwrap(), [1]);
    assert_eq!(
        storage.staged_smart_cluster_pending_ids(32).unwrap(),
        [2, 3]
    );
    let work = claim(storage, 2);
    let result = score_and_commit(
        storage,
        &work,
        &targets,
        &anchors,
        |query, _| {
            assert_eq!(query, "current");
            ready(Ok(vec![4.0]))
        },
        || None,
    )
    .await
    .unwrap();
    assert!(result.needs_archive);
    assert_eq!(result.assignments, 1);
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 3);
    assert_eq!(storage.staged_smart_cluster_pending_ids(32).unwrap(), [3]);
    assert_eq!(
        storage.peek_smart_cluster_pending_batch(32).unwrap(),
        [1, 2]
    );
    assert!(storage
        .list_smart_cluster_scoring_targets()
        .unwrap()
        .iter()
        .find(|t| t.id == legacy)
        .unwrap()
        .rederive_failed_scorer
        .is_none());

    // When every threshold is awaiting calibration the stage still hands its
    // result to the archive queue and frees its grant, without loading a model.
    storage.update_smart_cluster_enabled(valid, false).unwrap();
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 3);
    let result = score_and_commit(
        storage,
        &work,
        &targets,
        &HashMap::new(),
        |_, _| ready(Err("unexpected inference".into())),
        || None,
    )
    .await
    .unwrap();
    assert!(result.needs_archive);
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    assert_eq!(
        storage.peek_smart_cluster_pending_batch(32).unwrap(),
        [1, 2, 3]
    );
}

#[test]
fn ready_selection_scans_past_unindexed_inputs_without_acquiring_keys() {
    let mut fixture = fixture();
    let (_dir, storage, broker) = fixture.parts();
    for id in 1..=257 {
        insert_input(
            storage,
            id,
            Consumer::SmartCluster.bit() | Consumer::MiniLm.bit(),
        );
        storage.enqueue_smart_cluster_pending(id).unwrap();
    }
    indexed_input(storage, 258, Consumer::SmartCluster.bit());
    let before = broker.acquisitions.load(Ordering::SeqCst);
    assert_eq!(storage.staged_smart_cluster_pending_ids(1).unwrap(), [258]);
    assert_eq!(broker.acquisitions.load(Ordering::SeqCst), before);
    assert!(storage
        .peek_smart_cluster_pending_batch(32)
        .unwrap()
        .is_empty());
}

#[test]
fn cached_minilm_completion_enqueues_atomically_and_does_not_requeue_on_replay() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    let input = insert_input(storage, 1, Consumer::MiniLm.bit());
    let spec = spec(1, &input);
    storage
        .commit_derived_embedding(&write(storage, spec.clone()))
        .unwrap();
    let work = storage
        .processing_stage
        .claim(storage, Consumer::MiniLm)
        .unwrap()
        .unwrap();
    sql(storage, "CREATE TRIGGER reject_queue BEFORE INSERT ON smart_cluster_pending BEGIN SELECT RAISE(ABORT,'queue disk failure'); END");
    assert!(storage
        .record_staged_embedding_receipt(&work.receipt, &spec)
        .is_err());
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    sql(storage, "DROP TRIGGER reject_queue");
    storage
        .record_staged_embedding_receipt(&work.receipt, &spec)
        .unwrap();
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 1);
    storage.delete_smart_cluster_pending_ids(&[1]).unwrap();
    storage
        .record_staged_embedding_receipt(&work.receipt, &spec)
        .unwrap();
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 0);
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
}

#[test]
fn minilm_vector_receipt_and_smart_queue_roll_back_together() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    let input = insert_input(
        storage,
        1,
        Consumer::MiniLm.bit() | Consumer::SmartCluster.bit(),
    );
    let write = write(storage, spec(1, &input));
    let work = storage
        .processing_stage
        .claim(storage, Consumer::MiniLm)
        .unwrap()
        .unwrap();
    sql(storage, "CREATE TRIGGER reject_queue BEFORE INSERT ON smart_cluster_pending BEGIN SELECT RAISE(ABORT,'queue disk failure'); END");
    assert!(storage
        .commit_staged_embedding(&write, &work.receipt)
        .is_err());
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "1")
        .unwrap()
        .is_none());
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    sql(storage, "DROP TRIGGER reject_queue");
    storage
        .commit_staged_embedding(&write, &work.receipt)
        .unwrap();
    // Queue eligibility does not wait for a Python mirror or service acknowledgement.
    assert_eq!(storage.staged_smart_cluster_pending_ids(32).unwrap(), [1]);
}

#[test]
fn smart_result_rolls_back_and_completed_receipts_cannot_rewrite_or_retarget_it() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::SmartCluster.bit());
    let id = cluster(storage, "current", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 1);
    sql(storage, "CREATE TRIGGER reject_smart_receipt BEFORE INSERT ON app_bound_receipts WHEN NEW.consumer=8 BEGIN SELECT RAISE(ABORT,'receipt disk failure'); END");
    assert!(storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 4.0)], false)
        .is_err());
    assert_eq!(assignment_count(storage), 0);
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 1);
    sql(storage, "DROP TRIGGER reject_smart_receipt");
    for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(storage
            .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, score)], false)
            .is_err());
    }
    assert!(storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 4.0)], false)
        .unwrap());
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    assert!(!storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 8.0)], false)
        .unwrap());
    assert_eq!(
        storage.list_smart_cluster_assignments(id, 0, 10).unwrap()[0].rerank_score,
        Some(4.0)
    );
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 0);
    let mut forged = work.receipt.clone();
    forged.screenshot_id = 2;
    assert!(storage
        .commit_staged_smart_cluster(&forged, &targets, &[(id, 9.0)], false)
        .is_err());
}

#[tokio::test]
async fn interruption_and_configuration_changes_leave_the_task_retryable() {
    let mut fixture = fixture();
    let (dir, storage, _broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::SmartCluster.bit());
    let id = cluster(storage, "current", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 1);
    let inferred = std::cell::Cell::new(false);
    let anchors = HashMap::from([(id, vector())]);
    let result = score_and_commit(
        storage,
        &work,
        &targets,
        &anchors,
        |_, _| {
            inferred.set(true);
            ready(Ok(vec![4.0]))
        },
        || inferred.get().then_some("foreground_request"),
    )
    .await;
    assert!(result.err().unwrap().starts_with("deferred:"));
    assert_eq!(assignment_count(storage), 0);
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 1);
    storage
        .update_smart_cluster_threshold_with_scorer(id, 3.0, &current_scorer())
        .unwrap();
    assert!(storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 4.0)], false)
        .unwrap_err()
        .starts_with("deferred:"));
    storage
        .processing_stage
        .release(&work.receipt, false)
        .unwrap();
    let stage = Connection::open(dir.join(STAGING_FILE)).unwrap();
    let attempts: i64 = stage
        .query_row(
            "SELECT attempts FROM staged_work WHERE consumer=8",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 0);
}

#[tokio::test]
async fn changed_vectors_or_sources_cannot_complete_a_staged_smart_task() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::SmartCluster.bit());
    let id = cluster(storage, "current", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 1);
    sql(
        storage,
        "UPDATE derived_index_jobs SET status='pending' WHERE index_kind='semantic_text'",
    );
    assert!(storage
        .staged_smart_cluster_pending_ids(1)
        .unwrap()
        .is_empty());
    let error = score_and_commit(
        storage,
        &work,
        &targets,
        &HashMap::from([(id, vector())]),
        |_, _| ready(Err("unexpected inference".into())),
        || None,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error, "deferred: waiting_for_index");
    sql(storage, "DELETE FROM ocr_results WHERE screenshot_id=1");
    assert!(storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[], false)
        .is_err());
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 1);
}

#[test]
fn exhausted_smart_task_falls_back_without_revoking_other_consumers() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::ALL_MASK);
    let stage = Connection::open(dir.join(STAGING_FILE)).unwrap();
    let mut task_id = String::new();
    for _ in 0..5 {
        let work = claim(storage, 1);
        task_id = work.receipt.task_id.clone();
        storage
            .processing_stage
            .release(&work.receipt, true)
            .unwrap();
        stage
            .execute("UPDATE staged_work SET next_attempt=0 WHERE consumer=8", [])
            .unwrap();
    }
    assert!(storage
        .staged_smart_cluster_pending_ids(32)
        .unwrap()
        .is_empty());
    assert_eq!(storage.peek_smart_cluster_pending_batch(32).unwrap(), [1]);
    let Response::TaskState(state) = broker.call(Request::InspectTask { task_id }).unwrap() else {
        panic!("expected task state")
    };
    assert_eq!(state.abandoned_consumers, Consumer::SmartCluster.bit());
    assert!(!state.retired);
    assert!(storage
        .processing_stage
        .claim(storage, Consumer::Classification)
        .unwrap()
        .is_some());
    assert!(storage
        .processing_stage
        .claim(storage, Consumer::Clip)
        .unwrap()
        .is_some());
}

#[test]
fn legacy_service_preserves_other_consumers_and_cannot_retrofit_old_bindings() {
    let mut fixture = fixture();
    let (_dir, storage, broker) = fixture.parts();
    broker
        .supported_consumers
        .store(Consumer::LEGACY_MASK, Ordering::SeqCst);
    storage.processing_stage.refresh().unwrap();
    indexed_input(storage, 1, Consumer::ALL_MASK);
    assert!(!storage.processing_stage.supports(Consumer::SmartCluster));
    assert!(storage.processing_stage.has_ready(Consumer::Classification));
    assert_eq!(storage.peek_smart_cluster_pending_batch(32).unwrap(), [1]);
    broker
        .supported_consumers
        .store(Consumer::ALL_MASK, Ordering::SeqCst);
    storage.processing_stage.refresh().unwrap();
    assert!(storage.processing_stage.supports(Consumer::SmartCluster));
    assert!(!storage
        .processing_stage
        .owns_screenshot(1, Consumer::SmartCluster));
    assert!(storage
        .staged_smart_cluster_pending_ids(32)
        .unwrap()
        .is_empty());
}

#[test]
fn waiting_dependencies_and_retry_timers_defer_even_with_an_unlocked_session() {
    use crate::smart_cluster_scoring::staged::waiting_reason;
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    let input = insert_input(
        storage,
        1,
        Consumer::MiniLm.bit() | Consumer::SmartCluster.bit(),
    );
    storage.enqueue_smart_cluster_pending(1).unwrap();
    storage.credential_state.update_auth_time();
    assert!(storage.is_session_valid());
    assert_eq!(waiting_reason(storage).unwrap(), Some("waiting_for_index"));
    let mini = storage
        .processing_stage
        .claim(storage, Consumer::MiniLm)
        .unwrap()
        .unwrap();
    storage
        .commit_staged_embedding(&write(storage, spec(1, &input)), &mini.receipt)
        .unwrap();
    storage
        .processing_stage
        .finish(storage, &mini.receipt)
        .unwrap();
    assert_eq!(waiting_reason(storage).unwrap(), None);
    let smart = claim(storage, 1);
    storage
        .processing_stage
        .release(&smart.receipt, true)
        .unwrap();
    assert_eq!(waiting_reason(storage).unwrap(), Some("retry_wait"));
    indexed_input(storage, 2, Consumer::MiniLm.bit());
    assert_eq!(waiting_reason(storage).unwrap(), None); // Archive work remains runnable.
    storage.credential_state.invalidate_session();
    assert_eq!(waiting_reason(storage).unwrap(), Some("retry_wait"));
}

#[test]
fn committed_smart_result_recovers_after_crash_without_rescoring_or_revoking_other_grants() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::ALL_MASK);
    let id = cluster(storage, "current", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 1);
    storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 4.0)], false)
        .unwrap();
    broker.online.store(false, Ordering::SeqCst);
    assert!(storage
        .processing_stage
        .finish(storage, &work.receipt)
        .is_err());
    broker.online.store(true, Ordering::SeqCst);
    broker.principal.lock().unwrap().process_identity = "10:200".into();
    broker.clock.fetch_add(LEASE_TTL_SECS + 1, Ordering::SeqCst);
    restart_stage(dir, storage, broker);
    storage.processing_stage.reconcile(storage).unwrap();
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    assert_eq!(assignment_count(storage), 1);
    assert_eq!(storage.count_smart_cluster_pending().unwrap(), 0);
    assert!(!storage.is_silent_read_authorized());
    let Response::TaskState(state) = broker
        .call(Request::InspectTask {
            task_id: work.receipt.task_id,
        })
        .unwrap()
    else {
        panic!("expected task state")
    };
    assert_eq!(
        state.finished_consumers,
        Consumer::MiniLm.bit() | Consumer::SmartCluster.bit()
    );
    assert_eq!(state.abandoned_consumers, 0);
    assert!(storage
        .processing_stage
        .claim(storage, Consumer::Classification)
        .unwrap()
        .is_some());
    assert!(storage
        .processing_stage
        .claim(storage, Consumer::Clip)
        .unwrap()
        .is_some());
}

#[test]
fn smart_receipt_cannot_cross_dataset_generation_or_deleted_screenshot_boundaries() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    indexed_input(storage, 1, Consumer::SmartCluster.bit());
    let id = cluster(storage, "current", true);
    let targets = storage.list_smart_cluster_scoring_targets().unwrap();
    let work = claim(storage, 1);
    for field in 0..5 {
        let mut forged = work.receipt.clone();
        match field {
            0 => forged.dataset_id = "b".repeat(64),
            1 => forged.db_generation += 1,
            2 => forged.source_revision += 1,
            3 => forged.consumer = Consumer::MiniLm,
            _ => forged.lease_id = "c".repeat(64),
        }
        assert!(storage
            .commit_staged_smart_cluster(&forged, &targets, &[(id, 4.0)], false)
            .is_err());
    }
    sql(storage, "UPDATE screenshots SET is_deleted=1 WHERE id=1");
    assert!(storage
        .commit_staged_smart_cluster(&work.receipt, &targets, &[(id, 4.0)], false)
        .is_err());
    assert_eq!(assignment_count(storage), 0);
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
}
