use super::*;
use crate::credential_manager::CredentialManagerState;
use std::sync::Arc;

#[test]
fn postprocess_retry_uses_bounded_exponential_backoff() {
    assert_eq!(ocr_postprocess_retry_decision(0), ("pending", Some(30), 1));
    assert_eq!(ocr_postprocess_retry_decision(1), ("pending", Some(60), 2));
    assert_eq!(ocr_postprocess_retry_decision(2), ("pending", Some(120), 3));
    assert_eq!(ocr_postprocess_retry_decision(3), ("pending", Some(300), 4));
    assert_eq!(ocr_postprocess_retry_decision(4), ("failed", None, 5));
    assert_eq!(ocr_postprocess_retry_decision(99), ("failed", None, 100));
}

#[test]
fn expected_clip_images_are_not_counted_per_ocr_row() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    connection
        .execute_batch(
            "CREATE TABLE screenshots (
                    id INTEGER PRIMARY KEY,
                    image_hash TEXT NOT NULL,
                    is_deleted INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE ocr_results (
                    screenshot_id INTEGER NOT NULL,
                    is_deleted INTEGER NOT NULL DEFAULT 0
                 );
                 INSERT INTO screenshots (id, image_hash, is_deleted) VALUES
                    (1, 'h1', 0),
                    (2, 'h1', 0),
                    (3, 'h2', 0),
                    (4, 'h3', 0),
                    (5, 'h4', 1),
                    (6, 'h5', 0);
                 INSERT INTO ocr_results (screenshot_id, is_deleted) VALUES
                    (1, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0);
                 ",
        )
        .expect("CLIP eligibility fixture");
    *storage.db.lock().unwrap_or_else(|e| e.into_inner()) = Some(connection);

    // h1 is one image vector despite three OCR rows; h2/h3 each contribute
    // one; h4 is deleted; h5 has no OCR row.
    assert_eq!(storage.count_expected_clip_image_rows().unwrap(), 3);
}

#[test]
fn screenshot_queue_waits_until_all_child_ocr_rows_are_gone() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage.init_tables(&connection).expect("initialize schema");
    connection
        .execute_batch(
            "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                    (1, '1.enc', 'h1', 1),
                    (2, '2.enc', 'h2', 1);
                 INSERT INTO ocr_results (id, screenshot_id, text_hash, is_deleted)
                    VALUES (10, 1, 'x', 1);
                 INSERT INTO delete_queue_screenshots (id) VALUES (1), (2);",
        )
        .expect("insert delete queue fixture");
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);

    let ready = storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read screenshot queue");
    assert_eq!(ready.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2]);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute("DELETE FROM ocr_results WHERE id = 10", [])
            .expect("remove final OCR row");
    }
    let ready = storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read unblocked screenshot queue");
    assert_eq!(
        ready.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn ocr_delete_queue_isolates_unreadable_rows_and_rearms_repair() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    // Cleanup of indexed rows only runs while protected reads are
    // authorized; model an unlocked foreground session.
    credential_state.set_foreground_state(true);
    credential_state.update_auth_time();
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage.init_tables(&connection).expect("initialize schema");
    connection
        .execute_batch(
            "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                    (1, '1.enc', 'h1', 1),
                    (2, '2.enc', 'h2', 1);
                 INSERT INTO ocr_results
                    (id, screenshot_id, text, text_hash, text_enc, text_key_encrypted, is_deleted)
                    VALUES
                    (10, 1, NULL, 'indexed', NULL, NULL, 1),
                    (20, 2, NULL, '', NULL, NULL, 1);
                 INSERT INTO delete_queue_ocr (id) VALUES (10), (20);
                 INSERT INTO delete_queue_screenshots (id) VALUES (1), (2);",
        )
        .expect("insert delete queue fixture");
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);

    storage
        .run_blind_index_repair(|_| {})
        .expect("mark initial repair complete");
    assert!(!storage.is_blind_index_repair_needed().unwrap());

    let first = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process first OCR delete batch");
    assert_eq!(first.queue_rows, 1);
    assert_eq!(first.deleted_rows, 1);
    assert_eq!(first.retry_rows, 1);
    assert_eq!(first.fallback_deleted_rows, 0);
    assert!(!first.blind_index_repair_requested);
    assert!(!first.queue_empty);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        let failure_count: i64 = conn
            .query_row(
                "SELECT failure_count FROM delete_queue_ocr WHERE id = 10",
                [],
                |row| row.get(0),
            )
            .expect("persist first failure");
        assert_eq!(failure_count, 1);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE id = 20",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    let ready = storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read partially unblocked screenshot queue");
    assert_eq!(ready.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2]);

    let second = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process second OCR delete batch");
    assert_eq!(second.queue_rows, 0);
    assert_eq!(second.retry_rows, 1);
    assert!(!second.queue_empty);

    let third = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process fallback OCR delete batch");
    assert_eq!(third.queue_rows, 1);
    assert_eq!(third.deleted_rows, 1);
    assert_eq!(third.retry_rows, 0);
    assert_eq!(third.fallback_deleted_rows, 1);
    assert!(third.blind_index_repair_requested);
    assert!(third.queue_empty);
    assert!(storage.is_blind_index_repair_needed().unwrap());

    let ready = storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read fully unblocked screenshot queue");
    assert_eq!(
        ready.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn ocr_delete_queue_waits_while_locked_without_counting_failures() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    // No cached master key, no session, no background lease: the state a
    // hidden launch is in before the user unlocks.
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state.clone());
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage.init_tables(&connection).expect("initialize schema");
    connection
        .execute_batch(
            "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                    (1, '1.enc', 'h1', 1),
                    (2, '2.enc', 'h2', 1),
                    (3, '3.enc', 'h3', 1);
                 INSERT INTO ocr_results
                    (id, screenshot_id, text, text_hash, text_enc, text_key_encrypted, is_deleted)
                    VALUES
                    (10, 1, NULL, 'indexed', X'00', X'00', 1),
                    (20, 2, NULL, '', NULL, NULL, 1);
                 INSERT INTO delete_queue_ocr (id) VALUES (10), (20), (30);
                 INSERT INTO delete_queue_screenshots (id) VALUES (1), (2), (3);",
        )
        .expect("insert delete queue fixture");
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
    storage
        .run_blind_index_repair(|_| {})
        .expect("mark initial repair complete");
    assert!(!storage.is_blind_index_repair_needed().unwrap());
    assert!(!storage.is_silent_read_authorized());

    // The whole batch stays queued while locked, including rows that would
    // otherwise be removable without decrypting their text.
    let locked = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process OCR delete batch while locked");
    assert!(locked.waiting_for_unlock);
    assert_eq!(locked.queue_rows, 0);
    assert_eq!(locked.deleted_rows, 0);
    assert_eq!(locked.stale_queue_rows, 0);
    assert_eq!(locked.retry_rows, 0);
    assert_eq!(locked.fallback_deleted_rows, 0);
    assert!(!locked.blind_index_repair_requested);
    assert!(!locked.queue_empty);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| {
                row.get(0)
            })
            .expect("inspect queue");
        assert_eq!(remaining, 3, "the locked batch stays queued");
        assert_eq!(
            conn.query_row(
                "SELECT failure_count FROM delete_queue_ocr WHERE id = 10",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0,
            "waiting for unlock is not a failure"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM ocr_results", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2,
            "no OCR row is deleted while locked"
        );
    }

    // Repeated locked passes make no progress and never consume the retry
    // budget or re-arm a blind-index repair.
    for _ in 0..MAX_OCR_DELETE_CLEANUP_FAILURES {
        let again = storage
            .process_ocr_delete_queue_batch(10)
            .expect("process locked batch again");
        assert!(again.waiting_for_unlock);
        assert_eq!(again.queue_rows, 0);
        assert_eq!(again.retry_rows, 0);
        assert_eq!(again.fallback_deleted_rows, 0);
    }
    assert!(!storage.is_blind_index_repair_needed().unwrap());

    // Make the encrypted fixture readable only after the locked pass has
    // proved that it never attempted a non-silent unwrap.
    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        conn.execute(
            "UPDATE ocr_results
                 SET text = 'indexed text', text_enc = NULL, text_key_encrypted = NULL
                 WHERE id = 10",
            [],
        )
        .expect("prepare readable fixture");
    }
    credential_state.cache_master_key_for_tests(vec![9u8; 32]);
    credential_state.set_foreground_state(true);
    credential_state.update_auth_time();
    let unlocked = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process OCR delete batch after unlock");
    assert!(!unlocked.waiting_for_unlock);
    assert_eq!(unlocked.queue_rows, 3);
    assert_eq!(unlocked.deleted_rows, 2);
    assert_eq!(unlocked.stale_queue_rows, 1);
    assert_eq!(unlocked.retry_rows, 0);
    assert_eq!(unlocked.fallback_deleted_rows, 0);
    assert!(unlocked.queue_empty);
    assert!(!storage.is_blind_index_repair_needed().unwrap());
}

#[test]
fn ocr_delete_queue_drains_readable_rows_and_unblocks_screenshots() {
    assert_readable_ocr_delete_queue_drains(false);
}

#[test]
fn ocr_delete_queue_preserves_other_ids_in_chunked_postings() {
    assert_readable_ocr_delete_queue_drains(true);
}

fn assert_readable_ocr_delete_queue_drains(chunked: bool) {
    use super::super::blind_index::{BlindIndex, MutationStats};
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    credential_state.cache_master_key_for_tests(vec![9u8; 32]);
    credential_state.set_foreground_state(true);
    credential_state.update_auth_time();
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state.clone());
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage.init_tables(&connection).expect("initialize schema");

    // A readable, indexed row: plaintext lives in the legacy `text` column
    // and `text_hash` is non-empty, so the batch must unlink its postings
    // and then physically delete it.
    connection
        .execute_batch(
            "INSERT INTO screenshots (id, image_path, image_hash, is_deleted) VALUES
                    (1, '1.enc', 'h1', 1);
                 INSERT INTO ocr_results
                    (id, screenshot_id, text, text_hash, text_enc, text_key_encrypted, is_deleted)
                    VALUES (10, 1, 'hello world', 'indexed', NULL, NULL, 1);
                 INSERT INTO delete_queue_ocr (id) VALUES (10);
                 INSERT INTO delete_queue_screenshots (id) VALUES (1);",
        )
        .expect("insert delete queue fixture");

    let hmac_key = credential_state.get_hmac_key().expect("seeded HMAC key");
    let tokens: Vec<String> = StorageState::bigram_tokenize("hello world")
        .into_iter()
        .map(|token| StorageState::compute_hmac_hash(&token, &hmac_key))
        .collect();
    assert!(!tokens.is_empty(), "fixture text must produce bigrams");
    let mut expected: RoaringBitmap = if chunked {
        (0..70_000).map(|n| n * 37).collect()
    } else {
        RoaringBitmap::new()
    };
    expected.extend([10, 11]);
    let tx = connection.unchecked_transaction().unwrap();
    for token_hash in &tokens {
        BlindIndex::open(&tx)
            .unwrap()
            .replace(token_hash, &expected, &mut MutationStats::default())
            .unwrap();
    }
    tx.commit().unwrap();
    expected.remove(10);
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);

    // The screenshot stays blocked while its OCR row is still present.
    assert!(storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read blocked screenshot queue")
        .is_empty());

    let batch = storage
        .process_ocr_delete_queue_batch(10)
        .expect("process OCR delete batch");
    assert_eq!(batch.queue_rows, 1);
    assert_eq!(batch.deleted_rows, 1);
    assert_eq!(batch.stale_queue_rows, 0);
    assert_eq!(batch.retry_rows, 0);
    assert_eq!(batch.fallback_deleted_rows, 0);
    assert!(!batch.blind_index_repair_requested);
    assert!(batch.queue_empty);

    {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().expect("database");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM ocr_results", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "the readable OCR row must be hard-deleted"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "the queue row must be cleared so the batch cannot repeat forever"
        );
        let index = BlindIndex::open(conn).unwrap();
        for token_hash in &tokens {
            let posting = index.read(token_hash, chunked).unwrap().unwrap();
            assert_eq!(posting.bitmap, expected, "only the deleted row is unlinked");
            let probe = index
                .probe(&[token_hash.clone()])
                .unwrap()
                .postings
                .remove(0);
            assert_eq!(probe.chunked, chunked);
            assert_eq!(probe.bytes, expected.serialized_size());
            if chunked {
                assert_eq!(probe.cardinality, Some(expected.len()));
            }
        }
    }

    let ready = storage
        .fetch_screenshot_delete_candidates(10)
        .expect("read unblocked screenshot queue");
    assert_eq!(ready.iter().map(|row| row.id).collect::<Vec<_>>(), vec![1]);
}

#[test]
fn schema_upgrade_adds_ocr_delete_failure_count() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    connection
        .execute("CREATE TABLE delete_queue_ocr (id INTEGER PRIMARY KEY)", [])
        .expect("create legacy OCR queue");

    storage.init_tables(&connection).expect("upgrade schema");

    let failure_count_exists = connection
        .prepare("PRAGMA table_info(delete_queue_ocr)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .iter()
        .any(|column| column == "failure_count");
    assert!(failure_count_exists);
}

#[test]
fn incremental_vacuum_exhausts_more_than_one_pragma_step() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let db_path = temp.path().join("vacuum.db");
    let connection = Connection::open(&db_path).expect("open vacuum fixture");
    connection
        .execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
                 VACUUM;
                 CREATE TABLE delete_queue_screenshots (id INTEGER PRIMARY KEY);
                 CREATE TABLE delete_queue_ocr (id INTEGER PRIMARY KEY);
                 CREATE TABLE payloads (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
                 WITH RECURSIVE rows(id) AS (
                    SELECT 1 UNION ALL SELECT id + 1 FROM rows WHERE id < 800
                 )
                 INSERT INTO payloads (id, payload) SELECT id, zeroblob(3500) FROM rows;
                 DELETE FROM payloads;",
        )
        .expect("create vacuum fixture");
    let freelist_before: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("read initial freelist");
    assert!(freelist_before > 50);
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);

    let result = storage
        .run_incremental_vacuum_if_idle(0, 50)
        .expect("run incremental vacuum");
    assert!(result.attempted);
    assert!(result.stepped_pages > 1);
    assert!(result.reclaimed_pages > 1);
    assert!(result.freelist_after < result.freelist_before);
}

#[test]
fn clip_candidates_preserve_live_ids_and_optional_utc_timestamps_without_decryption() {
    let temp = tempfile::tempdir().unwrap();
    let storage = super::super::test_storage_with_schema(temp.path().to_path_buf());
    {
        let guard = storage
            .get_connection_named("clip_metadata_fixture")
            .unwrap();
        guard.as_ref().unwrap().execute_batch(
                "INSERT INTO screenshots(id,image_path,image_hash,created_at,process_name,is_deleted,window_title_enc)
                 VALUES(1,'1.enc','live','1970-01-01 00:00:01','editor.exe',0,X'ff'),
                       (2,'2.enc','deleted','2026-10-01 00:00:00','editor.exe',1,X'ff'),
                       (3,'3.enc','invalid','invalid timestamp',NULL,0,X'ff');"
            ).unwrap();
    }
    let keys = ["live", "deleted", "invalid", "missing"].map(str::to_string);
    let metadata = storage
        .clip_candidate_metadata(&keys, Instant::now() + std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(metadata.len(), 2);
    assert_eq!(metadata["live"].id, 1);
    assert_eq!(metadata["live"].timestamp, Some(1));
    assert_eq!(metadata["live"].process_name.as_deref(), Some("editor.exe"));
    assert!(metadata["live"].window_title.is_none());
    assert_eq!(metadata["invalid"].id, 3);
    assert!(metadata["invalid"].timestamp.is_none());
    assert!(storage
        .clip_candidate_metadata(&keys, Instant::now())
        .unwrap_err()
        .starts_with("query_deadline_exceeded"));
}

#[test]
fn silent_metadata_reads_fail_fast_while_session_is_locked() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);

    assert!(matches!(
        storage.get_ocr_text_prefixes_by_screenshot_ids_silent(&[1], 200),
        Err(BackgroundReadError::AuthRequired)
    ));
    assert!(matches!(
        storage.get_screenshot_summaries_by_ids_silent(&[1]),
        Err(BackgroundReadError::AuthRequired)
    ));
    assert!(matches!(
        storage.get_ocr_text_prefixes_by_screenshot_ids_silent_with_deadline(
            &[1],
            200,
            Some(Instant::now())
        ),
        Err(BackgroundReadError::AuthRequired)
    ));
    assert!(matches!(
        storage.get_screenshot_summaries_by_ids_silent_with_deadline(&[1], Some(Instant::now())),
        Err(BackgroundReadError::AuthRequired)
    ));
}

#[test]
fn expired_private_reads_stop_before_opening_the_database() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    credentials.cache_master_key_for_tests(vec![7; 32]);
    credentials.update_auth_time();
    let storage = StorageState::new(temp.path().to_path_buf(), credentials);
    assert!(storage.is_silent_read_authorized());
    let deadline = Some(Instant::now());
    for error in [
        storage
            .get_screenshot_summaries_by_ids_silent_with_deadline(&[1], deadline)
            .unwrap_err(),
        storage
            .get_ocr_text_prefixes_by_screenshot_ids_silent_with_deadline(&[1], 1000, deadline)
            .unwrap_err(),
    ] {
        assert_eq!(error.to_string(), "query_deadline_exceeded_during_hydrate");
    }
}

#[test]
fn ocr_deadline_stops_the_remaining_boxes_after_a_slow_unwrap() {
    let deadline = Instant::now() + std::time::Duration::from_millis(100);
    let rows = (0..100)
        .map(|_| (1, Some(vec![1]), Some(vec![2])))
        .collect();
    let mut calls = 0;
    let error = assemble_ocr_text_prefixes(rows, 1000, Some(deadline), |_, _| {
        calls += 1;
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        Ok("one box".to_string())
    })
    .unwrap_err();
    // The first CNG operation cannot be preempted; the other 99 must not
    // keep the detached blocking task busy after its caller times out.
    assert_eq!(calls, 1);
    assert_eq!(error.to_string(), "query_deadline_exceeded_during_hydrate");
}

#[test]
fn ocr_text_prefix_query_replays_insertion_order() {
    let connection = Connection::open_in_memory().expect("in-memory database");
    connection
        .execute_batch(
            "CREATE TABLE ocr_results (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    screenshot_id INTEGER NOT NULL,
                    text_enc BLOB,
                    text_key_encrypted BLOB,
                    box_x1 REAL, box_y1 REAL,
                    is_deleted INTEGER NOT NULL DEFAULT 0
                 );
                 -- Rows are listed in OCR engine order, which is the order the
                 -- boxes were embedded. Geometry disagrees on purpose: the last
                 -- box of screenshot 7 sits highest on screen, and the first two
                 -- share a visual line whose box_y1 jitters by one pixel.
                 INSERT INTO ocr_results
                    (screenshot_id, text_enc, text_key_encrypted, box_x1, box_y1, is_deleted)
                 VALUES
                    (7, X'01', X'00', 900.0, 41.0, 0),
                    (7, X'02', X'00', 120.0, 40.0, 0),
                    (7, X'03', X'00', 300.0, 10.0, 0),
                    (7, X'04', X'00',  10.0,  9.0, 1),
                    (8, X'05', X'00',  50.0, 99.0, 0);",
        )
        .expect("OCR ordering fixture");

    let sql = ocr_text_prefix_query(2);
    let mut stmt = connection.prepare(&sql).expect("prefix query");
    let rows: Vec<(i64, u8)> = stmt
        .query_map(params![7i64, 8i64], |row| {
            let screenshot_id: i64 = row.get(0)?;
            let text_enc: Vec<u8> = row.get(1)?;
            Ok((screenshot_id, text_enc[0]))
        })
        .expect("execute prefix query")
        .collect::<Result<Vec<_>, _>>()
        .expect("read prefix rows");

    // Insertion order — ordering by `box_y1, box_x1` would hand back 3, 2, 1
    // for screenshot 7 and rebuild text the vector was never encoded from.
    // The soft-deleted box 4 stays out regardless of order.
    assert_eq!(rows, vec![(7, 1u8), (7, 2u8), (7, 3u8), (8, 5u8)]);
}

#[test]
fn ocr_prefix_assembly_matches_full_join_and_skips_tail_boxes() {
    let box_row = |id: i64, text: &str| (id, Some(text.as_bytes().to_vec()), Some(vec![0u8]));
    let rows = vec![
        box_row(1, "0123456789"),
        (1, None, None),          // legacy row without payload is skipped for free
        box_row(1, ""),           // empty box costs one decrypt but adds no text
        box_row(1, "abcdefghij"), // joined length reaches the 20-char budget here
        box_row(1, "tail-never-needed"),
        box_row(1, "tail-never-needed-2"),
        box_row(2, "短文本"),
        box_row(3, "这是一段刚好超过二十个字符预算的中文文本内容"),
        box_row(3, "尾部"),
        box_row(4, ""), // only-empty screenshot yields no map entry
    ];
    let full_join = |wanted: i64| -> String {
        rows.iter()
            .filter(|(id, data, _)| *id == wanted && data.is_some())
            .filter_map(|(_, data, _)| String::from_utf8(data.clone().unwrap()).ok())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut decrypts = 0usize;
    let result = assemble_ocr_text_prefixes(rows.clone(), 20, None, |data, _key| {
        decrypts += 1;
        Ok::<String, BackgroundReadError>(String::from_utf8(data.to_vec()).unwrap())
    })
    .unwrap();

    // The assembled text must agree with the full join on the consumed
    // prefix — that is the exact fingerprint-equivalence contract.
    for id in [1i64, 2, 3] {
        let expected: String = full_join(id).chars().take(20).collect();
        let got: String = result
            .get(&id)
            .cloned()
            .unwrap_or_default()
            .chars()
            .take(20)
            .collect();
        assert_eq!(got, expected, "prefix mismatch for screenshot {id}");
    }
    // Screenshot 4 produced no text; the caller pre-fills empty strings.
    assert!(!result.contains_key(&4));
    // 9 decryptable boxes exist, but the two tails of screenshot 1 and
    // the tail of screenshot 3 must be skipped once the budget is met.
    assert_eq!(decrypts, 6);
}

#[test]
fn incomplete_postprocess_waits_for_unlock_on_restart_without_consuming_attempts() {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage
        .init_tables(&connection)
        .expect("current OCR lifecycle schema");
    connection.execute_batch(
            "INSERT INTO screenshots(id,image_path,image_hash) VALUES
                (41,'41','hash41'),(42,'42','hash42'),(43,'43','hash43'),
                (44,'44','hash44'),(45,'45','hash45'),(46,'46','hash46');
             INSERT INTO screenshot_ocr_status(screenshot_id,postprocess_status,postprocess_attempts,postprocess_lease)
             VALUES (41,'pending',1,NULL),(42,'queued',2,'old-queued'),(43,'processing',3,'old-processing'),
                    (44,'waiting_for_auth',4,NULL),(45,'completed',0,NULL),(46,'failed',5,NULL);"
        ).expect("OCR lifecycle fixture");
    *storage.db.lock().unwrap_or_else(|e| e.into_inner()) = Some(connection);

    assert_eq!(
        storage
            .recover_incomplete_ocr_postprocess()
            .expect("recover incomplete rows"),
        3
    );
    {
        let guard = storage.db.lock().unwrap_or_else(|e| e.into_inner());
        let connection = guard.as_ref().expect("database");
        for (id, attempts) in [(41, 1), (42, 2), (43, 3), (44, 4)] {
            let (status, stored_attempts, error): (String, i64, Option<String>) = connection
                .query_row(
                    "SELECT postprocess_status, postprocess_attempts, postprocess_error
                         FROM screenshot_ocr_status WHERE screenshot_id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("recovered row");
            assert_eq!(status, "waiting_for_auth");
            assert_eq!(stored_attempts, attempts);
            assert_eq!(error, None);
        }

        let completed: String = connection
            .query_row(
                "SELECT postprocess_status FROM screenshot_ocr_status WHERE screenshot_id = 45",
                [],
                |row| row.get(0),
            )
            .expect("completed row");
        let failed: String = connection
            .query_row(
                "SELECT postprocess_status FROM screenshot_ocr_status WHERE screenshot_id = 46",
                [],
                |row| row.get(0),
            )
            .expect("failed row");
        assert_eq!(completed, "completed");
        assert_eq!(failed, "failed");
    }
    assert_eq!(
        storage
            .list_pending_ocr_postprocess_ids(10)
            .expect("pending rows after startup recovery"),
        vec![41, 42, 43, 44]
    );
}
