use super::*;

#[test]
fn clip_candidates_need_ocr_and_stop_once_a_ledger_row_exists() {
    // The scan is the SQL half of Python's `ocr_text.strip()` gate: an image
    // with no OCR row at all was never in this corpus, and an image that
    // already has a ledger row is not offered again.
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash, width, height, created_at)
                 VALUES (1, 'a', 'with-ocr', 1920, 1080, datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash, created_at)
                 VALUES (2, 'b', 'no-ocr', datetime('now'))",
            [],
        )
        .unwrap();
        // Older than the repair window: only a full backfill reaches it.
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash, width, height, created_at)
                 VALUES (3, 'c', 'old-with-ocr', 1920, 1080, datetime('now', '-30 days'))",
            [],
        )
        .unwrap();
        for id in [1, 3] {
            conn.execute(
                "INSERT INTO ocr_results (screenshot_id, text, text_hash)
                     VALUES (?1, 'hello', 'hash-of-hello-' || ?1)",
                params![id],
            )
            .unwrap();
        }
    }
    let all = storage.list_clip_image_index_candidates(None, 10).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.contains(&"with-ocr".to_string()));
    assert!(all.contains(&"old-with-ocr".to_string()));

    // The bounded scan is the one the automatic repair pass uses. Its whole
    // purpose is to leave the old screenshot alone: before the step-7 copy
    // settles, an image with no ledger row is one the copy has not reached,
    // and re-encoding it costs hours to reproduce a vector Chroma holds.
    let recent = storage
        .list_clip_image_index_candidates(Some("-7 days"), 10)
        .unwrap();
    assert_eq!(recent, vec!["with-ocr".to_string()]);

    storage
        .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "with-ocr"))
        .unwrap();
    assert!(storage
        .list_clip_image_index_candidates(Some("-7 days"), 10)
        .unwrap()
        .is_empty());
    // A ledger row removes a subject from the unbounded scan too, which is
    // what stops a settled migration's own rows from being offered back.
    assert_eq!(
        storage.list_clip_image_index_candidates(None, 10).unwrap(),
        vec!["old-with-ocr".to_string()]
    );
}

#[test]
fn backfill_work_counts_megapixels_and_assumes_a_size_for_rows_without_one() {
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash, width, height)
                 VALUES (1, 'a', 'uhd', 3840, 2160)",
            [],
        )
        .unwrap();
        // Written before the dimension columns existed.
        conn.execute(
            "INSERT INTO screenshots (id, image_path, image_hash) VALUES (2, 'b', 'unknown')",
            [],
        )
        .unwrap();
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO ocr_results (screenshot_id, text, text_hash)
                     VALUES (?1, 'hello', 'hash-' || ?1)",
                params![id],
            )
            .unwrap();
        }
    }
    let work = storage.clip_image_backfill_work(2.0736).unwrap();
    assert_eq!(work.images, 2);
    // 8.2944 for the 4K row plus the assumed 1080p for the unsized one. A
    // count alone would have called these two images equal work; they are
    // not, by a factor of four.
    assert!((work.megapixels - (8.2944 + 2.0736)).abs() < 1e-6);

    storage
        .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "uhd"))
        .unwrap();
    let work = storage.clip_image_backfill_work(2.0736).unwrap();
    assert_eq!(work.images, 1);
    assert!((work.megapixels - 2.0736).abs() < 1e-6);
}

#[test]
fn clip_orphan_scan_reclaims_a_row_the_delete_triggers_never_saw() {
    // `screenshots.image_hash` is UNIQUE and the capture path skips a hash
    // it already holds, so an image maps to exactly one screenshot. When
    // that screenshot is soft-deleted through the ordinary path the trigger
    // removes the derived rows inside the deleting transaction and this
    // scan finds nothing — which is what the middle assertion pins.
    //
    // The scan exists for rows that arrived some other way: a database
    // written before those triggers, or a step-7 import that committed for a
    // screenshot deleted after the snapshot was taken. Both are modelled
    // here by writing the ledger row directly, after the delete, which is
    // the one thing `upsert_derived_index_job` refuses to do.
    let (_temp, storage) = test_storage();
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute(
                "INSERT INTO screenshots (id, image_path, image_hash) VALUES (1, 'a', 'only')",
                [],
            )
            .unwrap();
    }
    storage
        .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "only"))
        .unwrap();
    assert!(storage
        .list_orphaned_clip_image_subjects(10)
        .unwrap()
        .is_empty());

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
            .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_index_jobs WHERE index_kind = 'clip_image'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "the delete trigger owns the ordinary path");

        conn.execute(
                "INSERT INTO derived_index_jobs (index_kind, subject_key, status, model_id,                  model_revision, embedding_version, source_fingerprint)                  VALUES ('clip_image', 'only', 'completed', 'm', 'r', 1, 'f')",
                [],
            )
            .unwrap();
    }
    assert_eq!(
        storage.list_orphaned_clip_image_subjects(10).unwrap(),
        vec!["only".to_string()]
    );
}

#[test]
fn only_jobs_a_worker_could_actually_pick_up_are_claimable() {
    let (_temp, storage) = test_storage();

    // Never started.
    let pending = job(DerivedIndexKind::SemanticText, "1");
    ensure_active_subject(&storage, &pending);
    storage.upsert_derived_index_job(&pending).unwrap();

    // Already leased by someone else.
    let processing = job(DerivedIndexKind::SemanticText, "2");
    queue_and_claim(&storage, &processing);

    // Failed with the budget intact and the backoff elapsed.
    let retryable = job(DerivedIndexKind::SemanticText, "3");
    let lease = queue_and_claim(&storage, &retryable);
    storage
        .mark_derived_index_job_failed(&retryable, &lease, "embed_failed", "boom", None)
        .unwrap();

    // Failed with the backoff still running.
    let backing_off = job(DerivedIndexKind::SemanticText, "4");
    let lease = queue_and_claim(&storage, &backing_off);
    storage
        .mark_derived_index_job_failed(
            &backing_off,
            &lease,
            "embed_failed",
            "boom",
            Some("9999-12-31 23:59:59"),
        )
        .unwrap();

    let claimable = storage
        .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
        .unwrap();
    let subjects: Vec<&str> = claimable
        .iter()
        .map(|record| record.spec.subject_key.as_str())
        .collect();
    assert_eq!(subjects, vec!["1", "3"]);

    // A spent retry budget stops the job being claimable: nothing picks it
    // up again on its own, which is what the diagnostic calls stalled.
    let starved: Vec<String> = storage
        .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 1, 100)
        .unwrap()
        .into_iter()
        .map(|record| record.spec.subject_key)
        .collect();
    assert_eq!(starved, vec!["1".to_string()]);

    let backlog = storage
        .derived_index_backlog(DerivedIndexKind::SemanticText, 1)
        .unwrap();
    // Only the never-started job is still claimable at a budget of one; both
    // failures have spent it, and a spent budget is what `exhausted` counts.
    // The leased job is neither: someone is holding it right now.
    assert_eq!(backlog.claimable, 1);
    assert_eq!(backlog.ready, 1);
    assert_eq!(backlog.exhausted, 2);
    assert!(backlog.oldest_claimable_age_secs.is_some());

    // With the real budget the retryable failure is claimable again, and so
    // is the one still inside its backoff: this figure is queue depth, not
    // what a worker could lease this instant. Nothing is stalled.
    let generous = storage
        .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
        .unwrap();
    assert_eq!(generous.claimable, 3);
    assert_eq!(generous.ready, 2);
    assert_eq!(generous.exhausted, 0);

    // A different index kind must not leak into either number.
    let clip = storage
        .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
        .unwrap();
    assert_eq!(clip, DerivedIndexBacklog::default());
}

#[test]
fn index_candidates_are_screenshots_with_text_no_ledger_row_and_inside_retention() {
    let (_temp, storage) = test_storage();
    for id in [10, 11, 12, 13] {
        insert_screenshot_with_ocr(&storage, id);
    }
    // 11 already has a ledger row, 12 aged out, 13 was deleted.
    let queued = job(DerivedIndexKind::SemanticText, "11");
    storage.upsert_derived_index_job(&queued).unwrap();
    backdate_screenshot(&storage, 12, "-40 days");
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 13", [])
            .unwrap();
    }
    // A screenshot with no OCR row at all is not part of the corpus.
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (14, '14.enc', 'hash-14')",
                    [],
                )
                .unwrap();
    }

    let candidates = storage
        .list_semantic_text_index_candidates("-30 days", 100)
        .unwrap();
    assert_eq!(candidates, vec![10]);
}
