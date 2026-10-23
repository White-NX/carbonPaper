use super::*;

#[test]
fn ensure_job_is_idempotent_until_source_change() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "41");
    ensure_active_subject(&storage, &spec);
    assert_eq!(
        storage.ensure_derived_index_job(&spec).unwrap(),
        EnsureDerivedIndexJobResult::Queued
    );
    let lease = storage.mark_derived_index_job_processing(&spec).unwrap();
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job: spec.clone(),
            lease_token: lease,
            vector: vec![0.5, 0.25],
        })
        .unwrap();
    assert_eq!(
        storage.ensure_derived_index_job(&spec).unwrap(),
        EnsureDerivedIndexJobResult::AlreadyCurrent
    );
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "41")
            .unwrap()
            .unwrap()
            .status,
        DerivedIndexJobStatus::Completed
    );

    let mut changed = spec.clone();
    changed.source_fingerprint = "new-source".to_string();
    assert_eq!(
        storage.ensure_derived_index_job(&changed).unwrap(),
        EnsureDerivedIndexJobResult::Requeued
    );
}

#[test]
fn cancellation_requeue_preserves_retry_budget() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "43");
    ensure_active_subject(&storage, &spec);
    storage.ensure_derived_index_job(&spec).unwrap();
    let lease = storage.mark_derived_index_job_processing(&spec).unwrap();
    storage
        .requeue_derived_index_job(&spec, &lease, "cancelled", "cancelled by user")
        .unwrap();
    let record = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "43")
        .unwrap()
        .unwrap();
    assert_eq!(record.status, DerivedIndexJobStatus::Pending);
    assert_eq!(record.attempts, 0);
    assert_eq!(record.error_code.as_deref(), Some("cancelled"));
}

#[test]
fn explicit_ensure_resumes_terminal_and_delayed_jobs() {
    let (_temp, storage) = test_storage();
    let discarded = job(DerivedIndexKind::SemanticText, "44");
    let lease = queue_and_claim(&storage, &discarded);
    storage
        .mark_derived_index_job_discarded(&discarded, &lease, "empty_model_input", "empty")
        .unwrap();
    assert_eq!(
        storage.ensure_derived_index_job(&discarded).unwrap(),
        EnsureDerivedIndexJobResult::Requeued
    );

    let failed = job(DerivedIndexKind::SemanticText, "45");
    let lease = queue_and_claim(&storage, &failed);
    storage
        .mark_derived_index_job_failed(
            &failed,
            &lease,
            "temporary",
            "temporary",
            Some("2100-01-01 00:00:00"),
        )
        .unwrap();
    assert_eq!(
        storage.ensure_derived_index_job(&failed).unwrap(),
        EnsureDerivedIndexJobResult::Requeued
    );
    storage
        .mark_derived_index_job_processing(&failed)
        .expect("explicit ensure clears delayed retry timestamp");
}

#[test]
fn completed_vector_and_ledger_commit_atomically() {
    let (_temp, storage) = test_storage();
    let write = claimed_write(
        &storage,
        job(DerivedIndexKind::SemanticText, "42"),
        vec![0.25, -0.5, 0.75],
    );
    storage
        .commit_derived_embedding(&write)
        .expect("commit embedding");

    let visible = storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "42")
        .expect("read embedding")
        .expect("visible embedding");
    assert_eq!(visible.vector, write.vector);
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
            .unwrap()
            .unwrap()
            .status,
        DerivedIndexJobStatus::Completed
    );
}

#[test]
fn ledger_failure_rolls_back_vector_write() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "9");
    let write = claimed_write(&storage, spec, vec![1.0, 0.0]);
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_completed_job BEFORE UPDATE OF status ON derived_index_jobs
                     WHEN NEW.status = 'completed' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
    }
    let result = storage.commit_derived_embedding(&write);
    assert!(result.is_err());
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    let count: i64 = guard
        .as_ref()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM derived_embeddings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pending_rebuild_hides_stale_vector() {
    let (_temp, storage) = test_storage();
    let write = claimed_write(
        &storage,
        job(DerivedIndexKind::SemanticText, "17"),
        vec![0.1, 0.2],
    );
    storage.commit_derived_embedding(&write).unwrap();
    let mut changed = write.job.clone();
    changed.source_fingerprint = "changed-source".to_string();
    storage.upsert_derived_index_job(&changed).unwrap();
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "17")
        .unwrap()
        .is_none());
}

#[test]
fn semantic_subject_keys_must_be_canonical_ids() {
    let (_temp, storage) = test_storage();
    let canonical = job(DerivedIndexKind::SemanticText, "42");
    ensure_active_subject(&storage, &canonical);

    for alias in ["042", "+42"] {
        let aliased = job(DerivedIndexKind::SemanticText, alias);
        assert!(storage.upsert_derived_index_job(&aliased).is_err());
    }
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
        .unwrap()
        .is_none());
}

#[test]
fn interrupted_processing_jobs_are_requeued_at_startup() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "53");
    let old_lease = queue_and_claim(&storage, &spec);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        storage
            .recover_interrupted_derived_index_jobs_at_startup(guard.as_ref().unwrap())
            .unwrap();
    }

    let recovered = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "53")
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status, DerivedIndexJobStatus::Pending);
    assert_eq!(recovered.error_code.as_deref(), Some("worker_interrupted"));
    let new_lease = storage.mark_derived_index_job_processing(&spec).unwrap();
    assert_ne!(new_lease, old_lease);
    assert!(storage
        .mark_derived_index_job_failed(&spec, &old_lease, "late", "stale", None)
        .is_err());
}

#[test]
fn late_worker_cannot_complete_or_fail_requeued_contract() {
    let (_temp, storage) = test_storage();
    let old = job(DerivedIndexKind::SemanticText, "19");
    ensure_active_subject(&storage, &old);
    storage.upsert_derived_index_job(&old).unwrap();
    let old_lease = storage.mark_derived_index_job_processing(&old).unwrap();

    let mut current = old.clone();
    current.source_fingerprint = "new-source".to_string();
    storage.upsert_derived_index_job(&current).unwrap();

    assert!(storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job: old.clone(),
            lease_token: old_lease.clone(),
            vector: vec![1.0, 0.0],
        })
        .is_err());
    assert!(storage
        .mark_derived_index_job_failed(&old, &old_lease, "late", "stale worker", None)
        .is_err());

    let queued = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "19")
        .unwrap()
        .unwrap();
    assert_eq!(queued.spec.source_fingerprint, "new-source");
    assert_eq!(queued.status, DerivedIndexJobStatus::Pending);
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "19")
        .unwrap()
        .is_none());
}

#[test]
fn busy_claims_defer_but_database_errors_do_not() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "22");
    let lease = queue_and_claim(&storage, &spec);
    assert!(storage
        .mark_derived_index_job_processing(&spec)
        .unwrap_err()
        .is_pause());
    storage
        .requeue_derived_index_job(&spec, &lease, "test", "retry")
        .unwrap();
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_claim BEFORE UPDATE ON derived_index_jobs
             WHEN NEW.status='processing' BEGIN
             SELECT RAISE(ABORT, 'background_paused: simulated storage failure'); END;",
        )
        .unwrap();
    let error = storage
        .mark_derived_index_job_processing(&spec)
        .unwrap_err();
    assert!(matches!(error, TaskError::Failed(_)));
    assert!(!error.is_pause());
    let job = storage
        .get_derived_index_job(spec.index_kind, &spec.subject_key)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, DerivedIndexJobStatus::Pending);
    assert_eq!(job.attempts, 0);
}

#[test]
fn worker_lease_prevents_duplicate_claims_and_late_terminal_updates() {
    let (_temp, storage) = test_storage();
    let completed_spec = job(DerivedIndexKind::SemanticText, "20");
    let completed_write = claimed_write(&storage, completed_spec.clone(), vec![1.0, 0.0]);
    assert!(storage
        .mark_derived_index_job_processing(&completed_spec)
        .is_err());
    storage.commit_derived_embedding(&completed_write).unwrap();
    assert!(storage
        .mark_derived_index_job_failed(
            &completed_spec,
            &completed_write.lease_token,
            "late_failure",
            "a slower worker failed after completion",
            None,
        )
        .is_err());
    assert!(storage
        .mark_derived_index_job_discarded(
            &completed_spec,
            &completed_write.lease_token,
            "late_discard",
            "a cancellation arrived after completion",
        )
        .is_err());
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "20")
            .unwrap()
            .unwrap()
            .status,
        DerivedIndexJobStatus::Completed
    );

    let discarded_spec = job(DerivedIndexKind::SemanticText, "21");
    let discarded_write = claimed_write(&storage, discarded_spec.clone(), vec![0.0, 1.0]);
    storage
        .mark_derived_index_job_discarded(
            &discarded_spec,
            &discarded_write.lease_token,
            "cancelled",
            "discarded while inference was running",
        )
        .unwrap();
    assert!(storage.commit_derived_embedding(&discarded_write).is_err());
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "21")
            .unwrap()
            .unwrap()
            .status,
        DerivedIndexJobStatus::Discarded
    );
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "21")
        .unwrap()
        .is_none());
}

#[test]
fn failure_attempts_increment_and_auth_wait_does_not_consume_budget() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "51");
    let auth_lease = queue_and_claim(&storage, &spec);
    storage
        .mark_derived_index_job_waiting_for_auth(&spec, &auth_lease, Some("locked"))
        .unwrap();
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "51")
            .unwrap()
            .unwrap()
            .attempts,
        0
    );
    storage.upsert_derived_index_job(&spec).unwrap();
    let failure_lease = storage.mark_derived_index_job_processing(&spec).unwrap();
    storage
        .mark_derived_index_job_failed(&spec, &failure_lease, "inference", "failed", None)
        .unwrap();
    assert_eq!(
        storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "51")
            .unwrap()
            .unwrap()
            .attempts,
        1
    );

    let failed = storage
        .list_derived_index_jobs(
            DerivedIndexKind::SemanticText,
            Some(DerivedIndexJobStatus::Failed),
            0,
            10,
        )
        .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].spec.subject_key, "51");
}

#[test]
fn retry_backoff_blocks_early_worker_claims() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "52");
    let lease_token = queue_and_claim(&storage, &spec);
    storage
        .mark_derived_index_job_failed(
            &spec,
            &lease_token,
            "inference",
            "temporary failure",
            Some("9999-12-31 23:59:59"),
        )
        .unwrap();

    assert!(storage.mark_derived_index_job_processing(&spec).is_err());
}

#[test]
fn retry_timestamps_are_normalized_to_sqlite_utc() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "54");
    let lease_token = queue_and_claim(&storage, &spec);
    storage
        .mark_derived_index_job_failed(
            &spec,
            &lease_token,
            "inference",
            "temporary failure",
            Some("2000-01-01T08:00:00+08:00"),
        )
        .unwrap();

    let failed = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "54")
        .unwrap()
        .unwrap();
    assert_eq!(failed.next_retry_at.as_deref(), Some("2000-01-01 00:00:00"));
    storage.mark_derived_index_job_processing(&spec).unwrap();
}

#[test]
fn invalid_retry_timestamp_does_not_mutate_processing_job() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "55");
    let lease_token = queue_and_claim(&storage, &spec);
    assert!(storage
        .mark_derived_index_job_failed(
            &spec,
            &lease_token,
            "inference",
            "temporary failure",
            Some("tomorrow"),
        )
        .is_err());

    let processing = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "55")
        .unwrap()
        .unwrap();
    assert_eq!(processing.status, DerivedIndexJobStatus::Processing);
    assert_eq!(processing.attempts, 0);
}

#[test]
fn explicit_retry_reset_clears_failed_derived_job_state() {
    let (_temp, storage) = test_storage();
    let failed = job(DerivedIndexKind::SemanticText, "56");
    ensure_active_subject(&storage, &failed);
    storage.ensure_derived_index_job(&failed).unwrap();
    let lease = storage.mark_derived_index_job_processing(&failed).unwrap();
    storage
        .mark_derived_index_job_failed(
            &failed,
            &lease,
            "embed_failed",
            "temporary worker failure",
            Some("9999-12-31 23:59:59"),
        )
        .unwrap();

    assert_eq!(
        storage
            .reset_derived_index_retries(DerivedIndexKind::SemanticText)
            .unwrap(),
        1
    );
    let reset = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "56")
        .unwrap()
        .unwrap();
    assert_eq!(reset.status, DerivedIndexJobStatus::Pending);
    assert_eq!(reset.attempts, 0);
    assert_eq!(reset.error_code, None);
    assert_eq!(reset.error, None);
    assert_eq!(reset.next_retry_at, None);
    assert_eq!(
        storage
            .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 10)
            .unwrap()
            .len(),
        1
    );
}
