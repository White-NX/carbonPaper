use super::super::ann::visible_embedding_aggregate_sql;
use super::*;

#[test]
fn clip_scan_ranks_by_cosine_and_hides_a_deleted_image() {
    // The step-9 read path in miniature: a top-K over image vectors keyed by
    // hash, and a hit whose last live screenshot is gone must not surface.
    let (_temp, storage) = test_storage();
    for (hash, vector) in [
        ("hash-near", vec![1.0_f32, 0.0]),
        ("hash-far", vec![0.0_f32, 1.0]),
    ] {
        commit_vector(&storage, job(DerivedIndexKind::ClipImage, hash), vector).unwrap();
    }

    let ranked = storage.clip_image_topk(&[1.0, 0.0], 2).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].subject_key, "hash-near");
    assert!(ranked[0].score > ranked[1].score);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute(
                "UPDATE screenshots SET is_deleted = 1 WHERE image_hash = 'hash-near'",
                [],
            )
            .unwrap();
    }
    let ranked = storage.clip_image_topk(&[1.0, 0.0], 2).unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].subject_key, "hash-far");
}

#[test]
fn semantic_text_topk_ranks_visible_rows_by_cosine() {
    let (_temp, storage) = test_storage();
    // Three L2-normalized 2-D vectors at increasing angle from the query.
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "1"),
        vec![1.0, 0.0],
    )
    .unwrap();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "2"),
        vec![0.8, 0.6],
    )
    .unwrap();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "3"),
        vec![0.0, 1.0],
    )
    .unwrap();

    // A pending rebuild must hide its row from retrieval.
    let hidden = job(DerivedIndexKind::SemanticText, "4");
    ensure_active_subject(&storage, &hidden);
    storage.upsert_derived_index_job(&hidden).unwrap();

    let query = vec![1.0f32, 0.0];
    let ranked = storage.semantic_text_topk(&query, 2).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].subject_key, "1");
    assert_eq!(ranked[1].subject_key, "2");
    assert!((ranked[0].score - 1.0).abs() < 1e-6);
    assert!(ranked[0].score >= ranked[1].score);

    // The pending subject (id 4) never appears; only completed rows scan.
    assert_eq!(storage.semantic_text_topk(&query, 10).unwrap().len(), 3);
    assert_eq!(
        storage
            .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap(),
        3
    );
    assert!(storage
        .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
        .unwrap());
    assert!(!storage
        .has_query_visible_embeddings(DerivedIndexKind::ClipImage)
        .unwrap());
}

#[test]
fn visibility_metadata_reads_use_covering_indexes_after_schema_upgrade() {
    let (_temp, storage) = test_storage();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    // An existing archive gets the new indexes through the normal,
    // idempotent startup schema path, without rebuilding its vectors.
    conn.execute_batch(
        "DROP INDEX idx_derived_embeddings_visibility;
         DROP INDEX idx_derived_jobs_completed_visibility;",
    )
    .unwrap();
    storage.init_tables(conn).unwrap();
    storage.init_tables(conn).unwrap();
    for sql in [
        visible_embedding_count_sql(),
        visible_embedding_exists_sql(),
        visible_embedding_cache_stats_sql(),
        visible_embedding_aggregate_sql(),
    ] {
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan = statement
            .query_map([DerivedIndexKind::SemanticText.as_str()], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for index in [
            "idx_derived_embeddings_visibility",
            "idx_derived_jobs_completed_visibility",
        ] {
            assert!(
                plan.iter()
                    .any(|step| step.contains(&format!("COVERING INDEX {index}"))),
                "metadata must not read vector or job payload pages: {plan:?}"
            );
        }
    }
}

#[test]
fn visibility_metadata_preserves_every_model_and_source_contract_check() {
    let (_temp, storage) = test_storage();
    for id in 1..=7 {
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, &id.to_string()),
            vec![0.6, 0.8],
        )
        .unwrap();
    }
    commit_vector(
        &storage,
        job(DerivedIndexKind::ClipImage, "other-kind"),
        vec![0.6, 0.8],
    )
    .unwrap();
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute_batch(
            "UPDATE derived_index_jobs SET model_id='other-model'
                WHERE index_kind='semantic_text' AND subject_key='2';
             UPDATE derived_index_jobs SET model_revision='other-revision'
                WHERE index_kind='semantic_text' AND subject_key='3';
             UPDATE derived_index_jobs SET embedding_version=2
                WHERE index_kind='semantic_text' AND subject_key='4';
             UPDATE derived_index_jobs SET source_fingerprint='changed-source'
                WHERE index_kind='semantic_text' AND subject_key='5';
             UPDATE derived_index_jobs SET status='pending'
                WHERE index_kind='semantic_text' AND subject_key='6';
             UPDATE derived_index_jobs SET status='failed'
                WHERE index_kind='semantic_text' AND subject_key='7';",
        )
        .unwrap();
        let shape: (i64, i64, i64, i64) = conn
            .query_row(
                &visible_embedding_cache_stats_sql(),
                [DerivedIndexKind::SemanticText.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(shape, (1, 2, 2, 1));
    }
    assert_eq!(
        storage
            .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap(),
        1
    );
    assert!(storage
        .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
        .unwrap());
    let visible = storage
        .list_query_visible_embeddings(DerivedIndexKind::SemanticText, 0, 10)
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].job.subject_key, "1");
    let (_, row_count, _, dimensions, ..) = storage
        .derived_index_snapshot_for_ann(DerivedIndexKind::SemanticText)
        .unwrap();
    assert_eq!(row_count, 1);
    assert_eq!(dimensions, 2);
}

#[test]
fn query_visible_embedding_existence_tracks_model_contract() {
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "7");
    assert!(!storage
        .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
        .unwrap());

    commit_vector(&storage, spec.clone(), vec![0.6, 0.8]).unwrap();
    assert!(storage
        .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
        .unwrap());

    storage
        .invalidate_derived_index_model(
            DerivedIndexKind::SemanticText,
            &spec.model_id,
            "revision-2",
            spec.embedding_version,
        )
        .unwrap();
    assert!(!storage
        .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
        .unwrap());
}

#[test]
fn semantic_text_topk_rejects_empty_query_and_zero_k() {
    let (_temp, storage) = test_storage();
    assert!(storage.semantic_text_topk(&[], 5).is_err());
    assert!(storage
        .semantic_text_topk(&[1.0, 0.0], 0)
        .unwrap()
        .is_empty());
}

#[test]
fn batch_vector_reads_return_only_query_visible_subjects() {
    // The Smart Cluster prefilter reads a whole peeked batch at once. Two
    // properties matter and neither is obvious from the signature: a
    // subject whose rebuild is pending must not come back (its vector is
    // stale), and a subject that was never indexed must be silently absent
    // rather than an error, because that is the ordinary case for a
    // screenshot still queued in the ledger.
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "1"),
        vec![1.0, 0.0],
    )
    .unwrap();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "2"),
        vec![0.0, 1.0],
    )
    .unwrap();
    let pending = job(DerivedIndexKind::SemanticText, "3");
    ensure_active_subject(&storage, &pending);
    storage.upsert_derived_index_job(&pending).unwrap();

    let subjects: Vec<String> = ["1", "2", "3", "999"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let vectors = storage
        .get_query_visible_embeddings_by_subjects(DerivedIndexKind::SemanticText, &subjects)
        .unwrap();
    assert_eq!(vectors.len(), 2);
    assert_eq!(
        vectors.get("1").map(Vec::as_slice),
        Some([1.0, 0.0].as_slice())
    );
    assert_eq!(
        vectors.get("2").map(Vec::as_slice),
        Some([0.0, 1.0].as_slice())
    );
    assert!(
        !vectors.contains_key("3"),
        "a pending rebuild is not visible"
    );
    assert!(!vectors.contains_key("999"), "an unknown subject is absent");

    // And the empty batch is not a query at all.
    assert!(storage
        .get_query_visible_embeddings_by_subjects(DerivedIndexKind::SemanticText, &[])
        .unwrap()
        .is_empty());
}

#[test]
fn a_batch_vector_read_agrees_with_the_single_subject_read() {
    // The batch path duplicates `visible_embedding_sql`'s predicate, so it
    // could drift from the single-subject reader that defines what
    // "query-visible" means. This pins them together.
    let (_temp, storage) = test_storage();
    let spec = job(DerivedIndexKind::SemanticText, "7");
    commit_vector(&storage, spec.clone(), vec![0.6, 0.8]).unwrap();

    let single = storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "7")
        .unwrap()
        .expect("committed vector is visible");
    let batch = storage
        .get_query_visible_embeddings_by_subjects(
            DerivedIndexKind::SemanticText,
            &["7".to_string()],
        )
        .unwrap();
    assert_eq!(batch.get("7"), Some(&single.vector));

    // Invalidating the model hides the row from both readers together.
    storage
        .invalidate_derived_index_model(
            DerivedIndexKind::SemanticText,
            &spec.model_id,
            "revision-2",
            spec.embedding_version,
        )
        .unwrap();
    assert!(storage
        .get_query_visible_embedding(DerivedIndexKind::SemanticText, "7")
        .unwrap()
        .is_none());
    assert!(storage
        .get_query_visible_embeddings_by_subjects(
            DerivedIndexKind::SemanticText,
            &["7".to_string()]
        )
        .unwrap()
        .is_empty());
}

#[test]
fn resident_cache_absorbs_write_path_updates() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "1"),
        vec![1.0, 0.0],
    )
    .unwrap();
    let query = vec![1.0f32, 0.0];
    // The first query loads the matrix; nothing is resident before it.
    assert_eq!(storage.semantic_vector_cache_bytes(), 0);
    assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 1);
    assert!(
        storage.semantic_vector_cache_matrix_bytes()
            <= crate::storage::semantic_cache::SEMANTIC_TEXT_CACHE_MAX_BYTES
    );
    assert!(storage.semantic_vector_cache_bytes() > storage.semantic_vector_cache_matrix_bytes());

    // A dual-write landing while the cache is resident must be visible to
    // the next query without a reload.
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "2"),
        vec![0.8, 0.6],
    )
    .unwrap();
    let ranked = storage.semantic_text_topk(&query, 5).unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].subject_key, "1");
    assert_eq!(ranked[1].subject_key, "2");
    assert!(
        storage.semantic_vector_cache_matrix_bytes()
            <= crate::storage::semantic_cache::SEMANTIC_TEXT_CACHE_MAX_BYTES
    );

    // Re-encoding an existing subject replaces its row in place.
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "2"),
        vec![0.0, 1.0],
    )
    .unwrap();
    let ranked = storage.semantic_text_topk(&[0.0, 1.0], 1).unwrap();
    assert_eq!(ranked[0].subject_key, "2");
    assert!((ranked[0].score - 1.0).abs() < 1e-6);
    assert!(
        storage.semantic_vector_cache_matrix_bytes()
            <= crate::storage::semantic_cache::SEMANTIC_TEXT_CACHE_MAX_BYTES
    );
}

#[test]
fn resident_cache_does_not_survive_a_trigger_driven_delete() {
    let (_temp, storage) = test_storage();
    for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![0.9, 0.1])] {
        commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
    }
    let query = vec![1.0f32, 0.0];
    assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 2);

    // Soft-deleting the screenshot removes the embedding from inside
    // SQLite, which the write-path hooks cannot observe.
    let before = data_epoch(&storage);
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
            .unwrap();
    }
    // The nested epoch trigger is what makes the resident matrix notice.
    // If this ever stops holding, the visibility re-check below is the only
    // thing standing between the cache and a deleted screenshot.
    assert!(data_epoch(&storage) > before);

    let ranked = storage.semantic_text_topk(&query, 5).unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].subject_key, "2");
    assert!(
        storage.semantic_vector_cache_matrix_bytes()
            <= crate::storage::semantic_cache::SEMANTIC_TEXT_CACHE_MAX_BYTES
    );
}

#[test]
fn resident_cache_drops_a_subject_deleted_through_the_api() {
    let (_temp, storage) = test_storage();
    for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![0.9, 0.1])] {
        commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
    }
    let query = vec![1.0f32, 0.0];
    assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 2);

    assert!(storage
        .delete_derived_index_subject(DerivedIndexKind::SemanticText, "1")
        .unwrap());
    let ranked = storage.semantic_text_topk(&query, 5).unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].subject_key, "2");
    assert!(
        storage.semantic_vector_cache_matrix_bytes()
            <= crate::storage::semantic_cache::SEMANTIC_TEXT_CACHE_MAX_BYTES
    );
}

#[test]
fn tiny_resident_budget_keeps_the_full_clip_history_searchable() {
    let (_temp, storage) = test_storage();
    for (key, vector) in [
        ("image-a", vec![1.0, 0.0]),
        ("image-b", vec![0.8, 0.6]),
        ("image-c", vec![0.0, 1.0]),
    ] {
        commit_vector(&storage, job(DerivedIndexKind::ClipImage, key), vector).unwrap();
    }

    // A budget this small cannot admit even one resident row. The exact
    // fallback must still rank the complete durable index, and it must not
    // leave the temporary scan matrix behind for the next query.
    let ranked = storage
        .semantic_topk_resident(
            DerivedIndexKind::ClipImage,
            &[1.0, 0.0],
            3,
            TopKOptions {
                resident_budget: Some(1),
                ..TopKOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        ranked
            .iter()
            .map(|row| row.subject_key.as_str())
            .collect::<Vec<_>>(),
        vec!["image-a", "image-b", "image-c"]
    );
    assert_eq!(storage.semantic_vector_cache_bytes(), 0);
    assert_eq!(storage.semantic_vector_cache_matrix_bytes(), 0);

    // A second query takes the same bounded-memory path rather than
    // accidentally becoming a resident hit.
    assert_eq!(
        storage
            .semantic_topk_resident(
                DerivedIndexKind::ClipImage,
                &[0.0, 1.0],
                1,
                TopKOptions {
                    resident_budget: Some(1),
                    ..TopKOptions::default()
                }
            )
            .unwrap()[0]
            .subject_key,
        "image-c"
    );
    assert_eq!(storage.semantic_vector_cache_bytes(), 0);
}

#[test]
fn idle_eviction_releases_the_resident_matrix() {
    let (_temp, storage) = test_storage();
    commit_vector(
        &storage,
        job(DerivedIndexKind::SemanticText, "1"),
        vec![1.0, 0.0],
    )
    .unwrap();
    // Nothing resident yet, so there is nothing to evict.
    assert!(!storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));

    assert_eq!(storage.semantic_text_topk(&[1.0, 0.0], 1).unwrap().len(), 1);
    assert!(storage.semantic_vector_cache_bytes() > 0);
    // A live TTL keeps a just-used cache.
    assert!(!storage.evict_semantic_vector_cache_if_idle(crate::storage::SEMANTIC_CACHE_IDLE_TTL));
    assert!(storage.semantic_vector_cache_bytes() > 0);

    assert!(storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));
    assert_eq!(storage.semantic_vector_cache_bytes(), 0);
    assert!(!storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));

    // The next query reloads transparently.
    assert_eq!(storage.semantic_text_topk(&[1.0, 0.0], 1).unwrap().len(), 1);
    assert!(storage.semantic_vector_cache_bytes() > 0);
}

#[test]
fn indexed_rows_remain_searchable_while_pending_work_is_reported() {
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
    let rows = storage.semantic_text_topk(&[1.0, 0.0], 10).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.subject_key.as_str())
            .collect::<Vec<_>>(),
        vec!["1"]
    );
    let status = crate::semantic_query::backend_status(Some(&storage));
    assert_eq!(status.indexed_vectors, Some(1));
    assert_eq!(status.index_backlog, Some(1));
    assert_eq!(status.index_stalled, Some(0));
}
