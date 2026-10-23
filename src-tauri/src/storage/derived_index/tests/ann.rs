use super::*;

#[test]
fn ann_generation_zero_is_a_real_fence_and_stale_builds_cannot_write() {
    let (_temp, storage) = test_storage();
    let mut state = ann_checkpoint(&storage, 78);
    assert_eq!(state.runtime_generation, Some(0));
    storage.begin_ann_build(&mut state).unwrap();
    storage.bump_db_generation();
    assert!(storage
        .save_ann_build_checkpoint(&state)
        .unwrap_err()
        .is_pause());
    assert!(storage
        .freeze_ann_page(&mut state, 1)
        .unwrap_err()
        .is_pause());
    assert!(storage.cleanup_ann_inputs(&state).unwrap_err().is_pause());
    // The runtime token is refreshed only after reading a durable state in
    // the new process; it is intentionally absent from the serialized form.
    let restored = storage.ann_build_checkpoint().unwrap().unwrap();
    assert_eq!(restored.runtime_generation, None);
}

#[test]
fn resumed_ann_freezing_keeps_its_bound_and_concurrent_changes_in_the_tail() {
    let (_temp, storage) = test_storage();
    for key in ["a", "c", "z"] {
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, key),
            ann_vector(0),
        )
        .unwrap();
    }
    let mut state = ann_checkpoint(&storage, 77);
    storage.begin_ann_build(&mut state).unwrap();
    storage.freeze_ann_page(&mut state, 1).unwrap();
    assert_eq!(state.scan_cursor, "a");
    let covered = state.covered_epoch;
    let mut changed = job(DerivedIndexKind::ClipImage, "a");
    changed.source_fingerprint = "new a".into();
    commit_vector(&storage, changed, ann_vector(1)).unwrap();
    storage
        .delete_derived_index_subject(DerivedIndexKind::ClipImage, "c")
        .unwrap();
    for key in ["b", "zz"] {
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, key),
            ann_vector(2),
        )
        .unwrap();
    }
    state = storage.ann_build_checkpoint().unwrap().unwrap();
    while state.phase == "freeze" {
        storage.freeze_ann_page(&mut state, 1).unwrap();
    }
    let rows = storage.frozen_ann_page(&state, 0, 10).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.subject_key.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "z"]
    );
    assert_eq!(rows[0].vector_f32[..4], 1.0f32.to_le_bytes());
    let mut manifest = ann_manifest(covered);
    manifest.dimensions = 512;
    manifest.row_count = state.frozen_rows;
    storage.record_derived_ann_generation(&manifest).unwrap();
    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered, 100)
        .unwrap();
    assert!(tail
        .iter()
        .any(|row| row.subject_key == "zz" && row.vector.is_some()));
    assert!(tail
        .iter()
        .any(|row| row.subject_key == "c" && row.vector.is_none()));
    assert_eq!(
        tail.iter()
            .find(|row| row.subject_key == "a")
            .unwrap()
            .vector
            .as_ref()
            .unwrap()[1],
        1.0
    );
}

#[test]
fn ann_frozen_pages_survive_connection_restart_and_reject_a_replaced_dataset() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("ann-checkpoint.db");
    let open = || {
        let credential = Arc::new(CredentialManagerState::new(directory.path().to_path_buf()));
        let storage = StorageState::new(directory.path().to_path_buf(), credential);
        let connection = Connection::open(&database).unwrap();
        storage.init_tables(&connection).unwrap();
        *storage.db.lock().unwrap() = Some(connection);
        storage
    };
    let storage = open();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "a"),
        ann_vector(0),
    )
    .unwrap();
    let mut state = ann_checkpoint(&storage, 42);
    storage.begin_ann_build(&mut state).unwrap();
    storage.freeze_ann_page(&mut state, 1).unwrap();
    drop(storage);
    let storage = open();
    let restored = storage.ann_build_checkpoint().unwrap().unwrap();
    assert_eq!(restored.scan_cursor, "a");
    assert_eq!(restored.frozen_rows, 1);
    assert_eq!(storage.frozen_ann_page(&restored, 0, 1).unwrap().len(), 1);
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE app_metadata SET value='new-dataset' WHERE key='app_bound_dataset_id'",
            [],
        )
        .unwrap();
    assert!(storage
        .save_ann_build_checkpoint(&restored)
        .unwrap_err()
        .is_pause());
    assert!(storage
        .frozen_ann_page(&restored, 0, 1)
        .unwrap_err()
        .is_pause());
}

#[test]
fn ann_build_failure_state_persists_deduplicates_notification_and_clears_on_publish() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-base"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let covered_epoch = current_epoch(&storage);

    let first = storage
        .record_derived_ann_build_failure(
            DerivedIndexKind::ClipImage,
            1,
            "2026-08-14T00:00:00Z",
            "2026-08-14T00:15:00Z",
            "builder_missing",
            "builder missing",
            true,
            true,
        )
        .unwrap();
    assert!(first.should_notify);
    assert!(!first.state.notification_sent);

    let taken = storage
        .take_derived_ann_build_notification(DerivedIndexKind::ClipImage)
        .unwrap()
        .unwrap();
    assert!(taken.notification_sent);
    assert!(storage
        .take_derived_ann_build_notification(DerivedIndexKind::ClipImage)
        .unwrap()
        .is_none());

    let second = storage
        .record_derived_ann_build_failure(
            DerivedIndexKind::ClipImage,
            2,
            "2026-08-14T00:01:00Z",
            "2026-08-15T00:01:00Z",
            "builder_missing",
            "builder still missing",
            true,
            true,
        )
        .unwrap();
    assert!(!second.should_notify);
    assert_eq!(second.state.consecutive_failures, 2);
    assert_eq!(
        storage
            .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
            .unwrap()
            .unwrap(),
        second.state
    );

    storage
        .record_derived_ann_generation(&ann_manifest(covered_epoch))
        .unwrap();
    assert!(storage
        .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
        .unwrap()
        .is_none());
}

#[test]
fn ann_generation_survives_new_capture_and_tail_tracks_latest_value() {
    let (_temp, storage) = test_storage();
    let first = job(DerivedIndexKind::ClipImage, "hash-a");
    commit_vector(&storage, first.clone(), vec![1.0, 0.0]).unwrap();
    let covered_epoch = current_epoch(&storage);
    storage
        .record_derived_ann_generation(&ann_manifest(covered_epoch))
        .unwrap();

    let second = job(DerivedIndexKind::ClipImage, "hash-b");
    commit_vector(&storage, second, vec![0.0, 1.0]).unwrap();
    assert!(storage
        .get_derived_ann_generation(DerivedIndexKind::ClipImage)
        .unwrap()
        .is_some());
    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].subject_key, "hash-b");
    assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));

    let mut changed = first;
    changed.source_fingerprint = "changed-hash-a".to_string();
    let lease = queue_and_claim(&storage, &changed);
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job: changed,
            lease_token: lease,
            vector: vec![0.5, 0.5],
        })
        .unwrap();
    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(
        tail.iter()
            .find(|row| row.subject_key == "hash-a")
            .and_then(|row| row.vector.as_deref()),
        Some(&[0.5, 0.5][..])
    );
}

#[test]
fn ann_tail_returns_tombstone_after_subject_deletion() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-delete"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let covered_epoch = current_epoch(&storage);
    storage
        .delete_derived_index_subject(DerivedIndexKind::ClipImage, "hash-delete")
        .unwrap();
    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].subject_key, "hash-delete");
    assert!(tail[0].vector.is_none());
}

#[test]
fn ann_tail_updates_return_only_new_epochs_and_preserve_tombstones() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "a"),
        ann_vector(0),
    )
    .unwrap();
    let (first_epoch, first) = storage
        .derived_ann_tail_updates(DerivedIndexKind::ClipImage, 0, None, 100, None)
        .unwrap();
    assert_eq!(first.len(), 1);
    let unchanged = storage
        .derived_ann_tail_updates(DerivedIndexKind::ClipImage, 0, Some(first_epoch), 100, None)
        .unwrap();
    assert_eq!(unchanged.0, first_epoch);
    assert!(unchanged.1.is_empty());
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "b"),
        ann_vector(1),
    )
    .unwrap();
    storage
        .delete_derived_index_subject(DerivedIndexKind::ClipImage, "a")
        .unwrap();
    let (next_epoch, next) = storage
        .derived_ann_tail_updates(DerivedIndexKind::ClipImage, 0, Some(first_epoch), 100, None)
        .unwrap();
    assert!(next_epoch > first_epoch);
    assert_eq!(next.len(), 2);
    assert!(next
        .iter()
        .any(|row| row.subject_key == "a" && row.vector.is_none()));
    assert!(next
        .iter()
        .any(|row| row.subject_key == "b" && row.vector.is_some()));
    assert!(storage
        .derived_ann_tail_updates(
            DerivedIndexKind::ClipImage,
            0,
            Some(next_epoch + 1),
            100,
            None
        )
        .is_err());
    assert!(storage
        .derived_ann_tail_updates(DerivedIndexKind::ClipImage, 0, None, 1, None)
        .unwrap_err()
        .starts_with("ann_tail_too_large:"));
    assert!(storage
        .derived_ann_tail_updates(
            DerivedIndexKind::ClipImage,
            0,
            None,
            100,
            Some(Instant::now())
        )
        .unwrap_err()
        .starts_with("query_deadline_exceeded"));
    // A deadline must not leave a SQLite progress callback installed.
    assert_eq!(
        storage
            .derived_ann_tail_updates(DerivedIndexKind::ClipImage, 0, None, 100, None)
            .unwrap()
            .1
            .len(),
        2
    );
}

#[test]
fn incremental_ann_lookup_does_not_walk_older_changes() {
    let (_temp, storage) = test_storage();
    let guard = storage.get_connection_named("seed_ann_delta_plan").unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute_batch(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<50000)
             INSERT INTO derived_ann_changes(index_kind,subject_key,change_epoch)
             SELECT 'clip_image', printf('%08d',x), x FROM n;",
    )
    .unwrap();
    let mut statement = conn.prepare(ANN_TAIL_UPDATES_SQL).unwrap();
    let keys: Vec<String> = statement
        .query_map(params!["clip_image", 49999i64, 20001i64], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(keys, ["00050000"]);
    assert!(
        statement.get_status(rusqlite::StatementStatus::VmStep) < 500,
        "the epoch index should seek directly to the single new row"
    );
}

#[test]
fn ann_tail_returns_tombstone_while_existing_embedding_is_not_query_visible() {
    let (_temp, storage) = test_storage();
    let original = job(DerivedIndexKind::ClipImage, "hash-pending");
    commit_vector(&storage, original.clone(), vec![1.0, 0.0]).unwrap();
    let covered_epoch = current_epoch(&storage);

    let mut replacement = original;
    replacement.source_fingerprint = "replacement-source".to_string();
    storage.upsert_derived_index_job(&replacement).unwrap();

    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].subject_key, "hash-pending");
    assert!(tail[0].vector.is_none());
}

#[test]
fn ann_tail_keeps_only_the_latest_epoch_for_repeated_subject_changes() {
    let (_temp, storage) = test_storage();
    let mut spec = job(DerivedIndexKind::ClipImage, "hash-repeat");
    commit_vector(&storage, spec.clone(), vec![1.0, 0.0]).unwrap();
    let covered_epoch = current_epoch(&storage);

    spec.source_fingerprint = "second".to_string();
    let lease = queue_and_claim(&storage, &spec);
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job: spec.clone(),
            lease_token: lease,
            vector: vec![0.5, 0.5],
        })
        .unwrap();
    let epoch_after_second = current_epoch(&storage);

    spec.source_fingerprint = "third".to_string();
    let lease = queue_and_claim(&storage, &spec);
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job: spec,
            lease_token: lease,
            vector: vec![0.0, 1.0],
        })
        .unwrap();
    let epoch_after_third = current_epoch(&storage);
    assert!(epoch_after_third > epoch_after_second);

    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].subject_key, "hash-repeat");
    assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));

    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    let conn = guard.as_ref().unwrap();
    let stored_epoch: i64 = conn
            .query_row(
                "SELECT change_epoch FROM derived_ann_changes WHERE index_kind = 'clip_image' AND subject_key = 'hash-repeat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(stored_epoch as u64, epoch_after_third);
}

#[test]
fn recording_new_ann_generation_preserves_changes_that_landed_during_build() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-base"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let covered_epoch = current_epoch(&storage);

    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-during-build"),
        vec![0.0, 1.0],
    )
    .unwrap();
    let current = current_epoch(&storage);
    assert!(current > covered_epoch);

    storage
        .record_derived_ann_generation(&ann_manifest(covered_epoch))
        .unwrap();
    let tail = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].subject_key, "hash-during-build");
    assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));
}

#[test]
fn ann_manifest_cannot_claim_an_epoch_newer_than_sqlite() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-base"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let future_epoch = current_epoch(&storage) + 1;
    assert!(storage
        .record_derived_ann_generation(&ann_manifest(future_epoch))
        .is_err());
    assert!(storage
        .get_derived_ann_generation(DerivedIndexKind::ClipImage)
        .unwrap()
        .is_none());
}

#[test]
fn oversized_ann_tail_refuses_partial_overlay() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-base"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let covered_epoch = current_epoch(&storage);
    for key in ["hash-tail-a", "hash-tail-b"] {
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, key),
            vec![0.0, 1.0],
        )
        .unwrap();
    }
    let error = storage
        .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 1)
        .unwrap_err();
    assert_eq!(error, "ann_tail_too_large:2");
}

#[test]
fn ann_page_stream_keeps_pending_jobs_out_without_waiting_for_them() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-a"),
        vec![1.0, 0.0],
    )
    .unwrap();
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "hash-c"),
        vec![0.0, 1.0],
    )
    .unwrap();
    let pending = job(DerivedIndexKind::ClipImage, "hash-b");
    ensure_active_subject(&storage, &pending);
    storage.ensure_derived_index_job(&pending).unwrap();

    let mut pages = Vec::new();
    storage
        .for_each_query_visible_embedding_page_for_ann(DerivedIndexKind::ClipImage, 1, |page| {
            pages.push(
                page.into_iter()
                    .map(|row| row.subject_key)
                    .collect::<Vec<_>>(),
            );
            Ok(())
        })
        .unwrap();

    assert_eq!(
        pages,
        vec![vec!["hash-a".to_string()], vec!["hash-c".to_string()]]
    );
    assert_eq!(
        storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, 3)
            .unwrap()
            .claimable,
        1
    );
}

#[test]
fn non_visible_job_churn_and_unindexed_deletion_preserve_snapshot() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "1"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let pending = job(DerivedIndexKind::SemanticText, "2");
    ensure_active_subject(&storage, &pending);
    storage.upsert_derived_index_job(&pending).unwrap();
    let generation = storage
        .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
        .unwrap();

    let lease_token = storage.mark_derived_index_job_processing(&pending).unwrap();
    storage
        .mark_derived_index_job_failed(
            &pending,
            &lease_token,
            "inference",
            "temporary failure",
            None,
        )
        .unwrap();
    assert_eq!(
        storage
            .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
            .unwrap(),
        generation.clone()
    );

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash) VALUES (3, '3.enc', 'hash-3')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 3", [])
            .unwrap();
    }
    assert_eq!(
        storage
            .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
            .unwrap(),
        generation
    );
}

#[test]
fn rejects_non_finite_and_mixed_dimension_snapshots() {
    let (_temp, storage) = test_storage();
    let invalid = storage.commit_derived_embedding(&claimed_write(
        &storage,
        job(DerivedIndexKind::SemanticText, "90"),
        vec![f32::NAN],
    ));
    assert!(invalid.is_err());

    for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![1.0, 0.0, 0.0])] {
        commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
    }
    assert!(storage
        .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
        .is_err());
}

#[test]
fn rejects_mixed_model_contract_snapshots() {
    let (_temp, storage) = test_storage();
    let first = job(DerivedIndexKind::SemanticText, "1");
    let mut second = job(DerivedIndexKind::SemanticText, "2");
    second.model_revision = "revision-2".to_string();
    for spec in [first, second] {
        commit_vector(&storage, spec, vec![1.0, 0.0]).unwrap();
    }
    assert!(storage
        .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
        .is_err());
}
