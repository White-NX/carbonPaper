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
