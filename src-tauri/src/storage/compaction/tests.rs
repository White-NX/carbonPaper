use super::*;

fn legacy() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
        CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT);
        CREATE TABLE ocr_results(id INTEGER PRIMARY KEY,text_hash TEXT NOT NULL,
            is_deleted INTEGER NOT NULL DEFAULT 0,text_enc BLOB);
        CREATE INDEX idx_text_hash ON ocr_results(text_hash);
        INSERT INTO ocr_results VALUES
            (1,'legacy-full-hash',0,X'010203'),
            (2,'',0,X'040506'),(3,'old-hmac',1,X'070809');",
    )
    .unwrap();
    conn
}

fn pending(conn: &Connection) -> Vec<i64> {
    conn.prepare("SELECT ocr_id FROM ocr_index_pending ORDER BY ocr_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn pending_queue_tracks_indexing_deletion_restore_and_rollback() {
    let conn = legacy();
    initialize(&conn).unwrap();
    assert_eq!(pending(&conn), vec![2]);
    conn.execute_batch(
        "BEGIN;
        UPDATE ocr_results SET text_hash='1' WHERE id=2;
        INSERT INTO ocr_results VALUES(4,'',0,X'10');
        ROLLBACK;",
    )
    .unwrap();
    assert_eq!(pending(&conn), vec![2]);
    conn.execute("UPDATE ocr_results SET is_deleted=1 WHERE id=2", [])
        .unwrap();
    assert!(pending(&conn).is_empty());
    conn.execute("UPDATE ocr_results SET is_deleted=0 WHERE id=2", [])
        .unwrap();
    assert_eq!(pending(&conn), vec![2]);
    conn.execute("UPDATE ocr_results SET text_hash='1' WHERE id=2", [])
        .unwrap();
    assert!(pending(&conn).is_empty());
    conn.execute("UPDATE ocr_results SET text_hash='' WHERE id=1", [])
        .unwrap();
    conn.execute("DELETE FROM ocr_results WHERE id=1", [])
        .unwrap();
    assert!(pending(&conn).is_empty());
    initialize(&conn).unwrap();
    assert!(pending(&conn).is_empty());
}

#[test]
fn marker_rewrite_resumes_and_preserves_ciphertext_and_pending_rows() {
    let conn = legacy();
    initialize(&conn).unwrap();
    assert_eq!(compact_ocr_markers(&conn, 1).unwrap(), 1);
    initialize(&conn).unwrap();
    conn.execute("INSERT INTO ocr_results VALUES(4,'',0,X'ABCD')", [])
        .unwrap();
    while compact_ocr_markers(&conn, 1).unwrap() != 0 {}
    let rows: Vec<(i64, String, String)> = conn
        .prepare("SELECT id,text_hash,hex(text_enc) FROM ocr_results ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "1".into(), "010203".into()),
            (2, "".into(), "040506".into()),
            (3, "1".into(), "070809".into()),
            (4, "".into(), "ABCD".into()),
        ]
    );
    assert_eq!(pending(&conn), vec![2, 4]);
    assert_eq!(compact_ocr_markers(&conn, 1).unwrap(), 0);
    assert!(!conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='idx_text_hash')",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn failed_checkpoint_rolls_back_the_marker_rewrite() {
    let conn = legacy();
    initialize(&conn).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_compaction_checkpoint BEFORE INSERT ON app_metadata
         WHEN NEW.key='storage_ocr_marker_cursor_v1' BEGIN
             SELECT RAISE(ABORT,'injected checkpoint failure');
         END;",
    )
    .unwrap();
    assert!(compact_ocr_markers(&conn, 1).is_err());
    let marker: String = conn
        .query_row("SELECT text_hash FROM ocr_results WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(marker, "legacy-full-hash");
    assert_eq!(pending(&conn), vec![2]);
    conn.execute_batch("DROP TRIGGER reject_compaction_checkpoint;")
        .unwrap();
    assert_eq!(compact_ocr_markers(&conn, 1).unwrap(), 1);
}

fn progress_database() -> Connection {
    let conn = legacy();
    conn.execute_batch(
        "CREATE TABLE screenshots(id INTEGER PRIMARY KEY);
         INSERT INTO screenshots VALUES(5);
         CREATE TABLE derived_embeddings(subject_key TEXT);
         INSERT INTO derived_embeddings(rowid,subject_key) VALUES(64,'test');
         INSERT INTO app_metadata VALUES
            ('storage_ocr_marker_done_v1','1'),
            ('storage_vector_block_cursor_v1','-1'),
            ('storage_process_icon_cursor_v1','3'),
            ('storage_ocr_key_cursor_v1','1');",
    )
    .unwrap();
    conn
}

#[test]
fn resumed_scan_retains_estimate_and_waits_for_protected_terminal_markers() {
    let conn = progress_database();
    let snapshot = Snapshot::read(&conn).unwrap();
    assert!(snapshot.pending());
    assert!(!snapshot.unprotected_pending());
    assert_eq!(
        snapshot.phase(Some("waiting_for_unlock")),
        "waiting_for_unlock"
    );
    assert_eq!(
        snapshot.phase(Some("waiting_for_verification")),
        "waiting_for_verification"
    );
    let percent = snapshot.estimated_percent().unwrap();
    assert!(percent > 0.0 && percent < 100.0);

    // Visiting the last IDs cannot report completion while CNG is unavailable.
    conn.execute_batch(
        "UPDATE app_metadata SET value='5' WHERE key='storage_process_icon_cursor_v1';
         UPDATE app_metadata SET value='3' WHERE key='storage_ocr_key_cursor_v1';",
    )
    .unwrap();
    let snapshot = Snapshot::read(&conn).unwrap();
    assert!(snapshot.pending());
    assert_eq!(snapshot.estimated_percent(), Some(99.0));

    for key in [
        super::super::process_icons::CURSOR,
        super::super::ocr_keys::COMPACTION_CURSOR,
    ] {
        conn.execute(
            "UPDATE app_metadata SET value=?2 WHERE key=?1",
            params![key, i64::MAX.to_string()],
        )
        .unwrap();
    }
    let snapshot = Snapshot::read(&conn).unwrap();
    assert!(!snapshot.pending());
    assert_eq!(snapshot.phase(Some("waiting_for_unlock")), "idle");
    assert_eq!(snapshot.estimated_percent(), Some(100.0));
}

#[test]
fn empty_database_has_no_optimization_task_while_locked() {
    let conn = progress_database();
    conn.execute_batch(
        "DELETE FROM screenshots;
         DELETE FROM ocr_results;
         DELETE FROM derived_embeddings;
         DELETE FROM app_metadata;",
    )
    .unwrap();
    let snapshot = Snapshot::read(&conn).unwrap();
    assert!(!snapshot.pending());
    assert_eq!(snapshot.phase(Some("waiting_for_unlock")), "idle");
    assert_eq!(snapshot.estimated_percent(), None);
}

#[test]
fn progress_reads_are_cached_and_a_database_swap_rejects_old_updates() {
    let directory = tempfile::tempdir().unwrap();
    let storage = crate::storage::test_storage_with_schema(directory.path().to_path_buf());
    let generation = storage.db_generation();
    storage
        .storage_compaction_progress
        .lock()
        .unwrap()
        .update(generation, "running", Some(42.0))
        .unwrap();
    let database = storage.db.lock().unwrap();
    // This read must complete even while the database is held by a writer.
    let before = storage.storage_compaction_progress();
    assert!(before.running);
    assert_eq!(before.percent, Some(42.0));
    storage.bump_db_generation();
    let after = storage.storage_compaction_progress();
    assert_eq!(after.phase, "idle");
    assert_eq!(after.percent, None);
    assert!(after.revision > before.revision);
    assert!(storage
        .storage_compaction_progress
        .lock()
        .unwrap()
        .update(generation, "running", Some(80.0))
        .is_none());
    drop(database);
}

#[test]
fn later_stage_failure_keeps_committed_work_in_diagnostic_counts() {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
        directory.path().to_path_buf(),
    ));
    let storage = StorageState::new(directory.path().to_path_buf(), credentials);
    let conn = legacy();
    initialize(&conn).unwrap();
    // The absent vector table injects a failure after the marker transaction.
    *storage.db.lock().unwrap() = Some(conn);
    let mut work = BatchWork::default();
    let error = storage.compact_storage_batch(&mut work).unwrap_err();
    assert!(error.starts_with("vector blocks:"));
    assert_eq!(work.marker_rows_scanned, 3);
    assert_eq!(work.vector_work_units, 0);
    assert_eq!(
        storage
            .db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .query_row("SELECT text_hash FROM ocr_results WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        OCR_INDEXED
    );
}
