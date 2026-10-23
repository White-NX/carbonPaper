use super::*;

#[test]
fn archive_embedding_commit_cancels_without_spending_attempts_or_replacing_new_sources() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "901");
    let lease_token = queue_and_claim(&storage, &spec);
    let generation = storage.db_generation();
    let revisions = storage
        .archive_scoring_source_revisions(generation, &[901])
        .unwrap();
    let write = DerivedEmbeddingWrite {
        job: spec.clone(),
        lease_token: lease_token.clone(),
        vector: vec![1.0, 0.0],
    };
    let execution = crate::background_policy::ExecutionLease::new(
        "semantic_index",
        crate::background_policy::ExecutionProfile::Background,
        20,
    );
    execution.revoke("input_resumed");
    assert!(storage
        .commit_archive_embedding(&write, generation, &revisions, Some(&execution))
        .unwrap_err()
        .is_pause());
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "INSERT INTO screenshot_processing_revisions(screenshot_id,revision) VALUES(901,99)
             ON CONFLICT(screenshot_id) DO UPDATE SET revision=99",
            [],
        )
        .unwrap();
    assert!(storage
        .commit_archive_embedding(&write, generation, &revisions, None)
        .unwrap_err()
        .is_pause());
    storage.bump_db_generation();
    assert!(storage
        .commit_archive_embedding(&write, generation, &[], None)
        .unwrap_err()
        .is_pause());
    storage
        .requeue_derived_index_job(&spec, &lease_token, "paused", "try next pass")
        .unwrap();
    let row = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "901")
        .unwrap()
        .unwrap();
    assert_eq!(row.attempts, 0);
    assert_eq!(row.status, DerivedIndexJobStatus::Pending);
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "901")
        .unwrap()
        .is_none());
}

#[test]
fn inactive_subject_cannot_be_requeued_after_screenshot_deletion() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "18");
    ensure_active_subject(&storage, &spec);
    storage.upsert_derived_index_job(&spec).unwrap();

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 18", [])
            .unwrap();
    }

    assert!(storage.upsert_derived_index_job(&spec).is_err());
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "18")
        .unwrap()
        .is_none());
}

#[test]
fn duplicate_clip_subject_replaces_one_image_hash_row() {
    let (_temp, storage) = test_storage();
    let first = claimed_write(
        &storage,
        job(DerivedIndexKind::ClipImage, "same-image-hash"),
        vec![1.0, 0.0],
    );
    storage.commit_derived_embedding(&first).unwrap();
    let second = claimed_write(&storage, first.job.clone(), vec![0.0, 1.0]);
    storage.commit_derived_embedding(&second).unwrap();
    let rows = storage
        .list_query_visible_embeddings(DerivedIndexKind::ClipImage, 0, 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].vector, second.vector);
}

#[test]
fn deletion_removes_vector_and_ledger_together() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "81"),
        vec![0.3, 0.4],
    )
    .unwrap();
    assert!(storage
        .delete_derived_index_subject(DerivedIndexKind::SemanticText, "81")
        .unwrap());
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "81")
        .unwrap()
        .is_none());
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "81")
        .unwrap()
        .is_none());
}

#[test]
fn screenshot_lifecycle_removes_text_and_image_derived_rows() {
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (42, '42.enc', 'hash-42')",
                    [],
                )
                .unwrap();
    }
    for (spec, vector) in [
        (job(DerivedIndexKind::SemanticText, "42"), vec![1.0, 0.0]),
        (job(DerivedIndexKind::ClipImage, "hash-42"), vec![0.0, 1.0]),
    ] {
        commit_vector(&storage, spec, vector).unwrap();
    }

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 42", [])
            .unwrap();
    }
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "42")
        .unwrap()
        .is_none());
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::ClipImage, "hash-42")
        .unwrap()
        .is_none());
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
        .unwrap()
        .is_none());
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::ClipImage, "hash-42")
        .unwrap()
        .is_none());
}

#[test]
fn late_workers_cannot_resurrect_soft_deleted_screenshot_subjects() {
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (91, '91.enc', 'hash-91')",
                    [],
                )
                .unwrap();
    }
    let writes = [
        claimed_write(
            &storage,
            job(DerivedIndexKind::SemanticText, "91"),
            vec![1.0, 0.0],
        ),
        claimed_write(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-91"),
            vec![0.0, 1.0],
        ),
    ];

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 91", [])
            .unwrap();
    }

    for write in writes {
        assert!(storage.commit_derived_embedding(&write).is_err());
        assert!(storage
            .get_query_visible_embedding(write.job.index_kind, &write.job.subject_key)
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_job(write.job.index_kind, &write.job.subject_key)
            .unwrap()
            .is_none());
    }
}

#[test]
fn model_invalidation_deletes_stale_vector_and_requeues_job() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "23"),
        vec![0.3, 0.4],
    )
    .unwrap();
    assert_eq!(
        storage
            .invalidate_derived_index_model(
                DerivedIndexKind::SemanticText,
                "model-a",
                "revision-2",
                2,
            )
            .unwrap(),
        1
    );
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "23")
        .unwrap()
        .is_none());
    let queued = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "23")
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, DerivedIndexJobStatus::Pending);
    assert_eq!(queued.spec.model_revision, "revision-2");
    assert_eq!(queued.spec.embedding_version, 2);
}

#[test]
fn model_invalidation_does_not_resurrect_discarded_jobs() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "24");
    let lease_token = queue_and_claim(&storage, &spec);
    storage
        .mark_derived_index_job_discarded(&spec, &lease_token, "cancelled", "explicitly discarded")
        .unwrap();

    assert_eq!(
        storage
            .invalidate_derived_index_model(
                DerivedIndexKind::SemanticText,
                "model-a",
                "revision-2",
                2,
            )
            .unwrap(),
        0
    );
    let discarded = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "24")
        .unwrap()
        .unwrap();
    assert_eq!(discarded.status, DerivedIndexJobStatus::Discarded);
    assert_eq!(discarded.spec.model_revision, "revision-1");
    assert_eq!(discarded.spec.embedding_version, 1);
}

/// The candidate scan can only over-approximate the corpus, so the ledger has
/// to remember what the decrypting builder ruled out. Without this the same
/// screenshot is handed back, re-decrypted, and re-dropped on every pass, and
/// once enough of them accumulate they fill the scan's `LIMIT` and the repair
/// path stops reaching screenshots that really are missing a vector.
#[test]
fn an_excluded_subject_leaves_the_repair_scan_for_good() {
    let (_temp, storage) = test_storage();
    insert_screenshot_with_ocr(&storage, 30);
    insert_screenshot_with_ocr(&storage, 31);
    assert_eq!(
        storage
            .list_semantic_text_index_candidates("-30 days", 100)
            .unwrap(),
        vec![31, 30]
    );

    let empty = job(DerivedIndexKind::SemanticText, "30");
    assert!(storage
        .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
        .unwrap());

    // Out of the scan, and out of both worker-facing projections: nothing
    // will lease it, and it is neither queue depth nor a stalled job.
    assert_eq!(
        storage
            .list_semantic_text_index_candidates("-30 days", 100)
            .unwrap(),
        vec![31]
    );
    assert!(storage
        .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
        .unwrap()
        .is_empty());
    let backlog = storage
        .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
        .unwrap();
    assert_eq!(backlog.claimable, 0);
    assert_eq!(backlog.exhausted, 0);

    // Repeating the decision is a no-op rather than a second row.
    assert!(storage
        .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
        .unwrap());
    let record = storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "30")
        .unwrap()
        .unwrap();
    assert_eq!(record.status, DerivedIndexJobStatus::Discarded);
    assert_eq!(record.attempts, 0);
    assert_eq!(record.error_code.as_deref(), Some("empty_source"));
}

#[test]
fn regaining_text_reclaims_an_excluded_subject() {
    let (_temp, storage) = test_storage();
    let empty = job(DerivedIndexKind::SemanticText, "32");
    ensure_active_subject(&storage, &empty);
    assert!(storage
        .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
        .unwrap());

    // The exclusion is fingerprinted against the empty source, so a
    // re-OCR that produces text is an ordinary source change and not
    // something the ledger has already settled.
    let mut with_text = empty.clone();
    with_text.source_fingerprint = "source-32-with-text".to_string();
    assert_eq!(
        storage.ensure_derived_index_job(&with_text).unwrap(),
        EnsureDerivedIndexJobResult::Requeued
    );
    let claimable: Vec<String> = storage
        .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
        .unwrap()
        .into_iter()
        .map(|record| record.spec.subject_key)
        .collect();
    assert_eq!(claimable, vec!["32".to_string()]);
}

#[test]
fn an_exclusion_drops_a_vector_whose_source_is_gone_but_spares_a_matching_one() {
    let (_temp, storage) = test_storage();
    // A vector encoded from text that has since disappeared cannot stay
    // query-visible: it would keep ranking against a source nothing holds.
    let stale = job(DerivedIndexKind::SemanticText, "33");
    commit_vector(&storage, stale.clone(), vec![1.0, 0.0]).unwrap();
    let mut now_empty = stale.clone();
    now_empty.source_fingerprint = "source-33-empty".to_string();
    assert!(storage
        .exclude_derived_index_subject(&now_empty, "empty_source", "nothing to encode")
        .unwrap());
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "33")
        .unwrap()
        .is_none());

    // The M2.4 migration deliberately copies a legacy Chroma vector for a
    // screenshot whose current text is empty, stamped with exactly this
    // fingerprint. It cannot be recomputed, so excluding must not touch it.
    let legacy = job(DerivedIndexKind::SemanticText, "34");
    commit_vector(&storage, legacy.clone(), vec![0.0, 1.0]).unwrap();
    assert!(storage
        .exclude_derived_index_subject(&legacy, "empty_source", "nothing to encode")
        .unwrap());
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "34")
        .unwrap()
        .is_some());
}

#[test]
fn a_deleted_screenshot_is_reported_rather_than_recorded_as_excluded() {
    let (_temp, storage) = test_storage();
    // Nothing to record: the lifecycle triggers already removed the rows,
    // and inserting one would resurrect a subject the schema retired.
    let gone = job(DerivedIndexKind::SemanticText, "35");
    assert!(!storage
        .exclude_derived_index_subject(&gone, "empty_source", "nothing to encode")
        .unwrap());
    assert!(storage
        .get_derived_index_job(DerivedIndexKind::SemanticText, "35")
        .unwrap()
        .is_none());
}

#[test]
fn expiry_reaps_aged_subjects_and_leaves_the_rest_alone() {
    let (_temp, storage) = test_storage();
    for id in [20, 21] {
        let spec = job(DerivedIndexKind::SemanticText, &id.to_string());
        commit_vector(&storage, spec, vec![1.0, 0.0]).unwrap();
    }
    backdate_screenshot(&storage, 20, "-40 days");

    let expired = storage
        .list_expired_semantic_text_subjects("-30 days", 100)
        .unwrap();
    assert_eq!(expired, vec!["20".to_string()]);

    assert!(storage
        .delete_derived_index_subject(DerivedIndexKind::SemanticText, "20")
        .unwrap());
    assert!(storage
        .list_expired_semantic_text_subjects("-30 days", 100)
        .unwrap()
        .is_empty());
    // The row still inside the window survives the sweep.
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "21")
        .unwrap()
        .is_some());
}

#[test]
fn expiry_also_sweeps_a_ledger_row_whose_screenshot_is_gone() {
    // The schema triggers already delete derived rows with their screenshot,
    // and `ensure_derived_index_job` refuses to queue work for a subject that
    // is not live, so this branch cannot be reached through the normal APIs
    // at all. It is a safety net for a row that arrived some other way — a
    // database written before those triggers existed — which is otherwise
    // invisible and never ages out, because ageing is decided against a
    // screenshot row that no longer exists.
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute(
                "INSERT INTO derived_index_jobs
                       (index_kind, subject_key, model_id, model_revision,
                        embedding_version, source_fingerprint, status)
                     VALUES ('semantic_text', '999', 'model-a', 'revision-1', 1,
                             'source-999', 'pending')",
                [],
            )
            .unwrap();
    }

    let expired = storage
        .list_expired_semantic_text_subjects("-30 days", 100)
        .unwrap();
    assert_eq!(expired, vec!["999".to_string()]);
}

#[test]
fn startup_cleanup_retains_referenced_legacy_sidecars() {
    let (temp, storage) = test_storage();
    let directory = temp.path().join("derived-indexes");
    std::fs::create_dir_all(&directory).unwrap();
    let first_path = directory.join("semantic_text-1.cpdvec");
    let second_path = directory.join("semantic_text-2.cpdvec");
    std::fs::write(&first_path, b"replaced legacy generation").unwrap();
    std::fs::write(&second_path, b"referenced legacy generation").unwrap();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO derived_index_generations
                    (index_kind, generation, data_epoch, file_name, checksum_sha256, row_count)
                 VALUES ('semantic_text', 2, 0, 'semantic_text-2.cpdvec', ?1, 0)",
            ["00".repeat(32)],
        )
        .unwrap();
        storage
            .cleanup_derived_index_sidecars_at_startup(conn, temp.path())
            .unwrap();
    }
    assert!(!first_path.exists());
    assert!(second_path.exists());
}

#[test]
fn startup_cleanup_does_not_fail_when_sidecar_cache_path_is_not_a_directory() {
    let (temp, storage) = test_storage();
    let sidecar_path = temp.path().join("derived-indexes");
    std::fs::write(&sidecar_path, b"not a directory").unwrap();

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        storage
            .cleanup_derived_index_sidecars_at_startup(guard.as_ref().unwrap(), temp.path())
            .unwrap();
    }

    assert!(sidecar_path.is_file());
}
