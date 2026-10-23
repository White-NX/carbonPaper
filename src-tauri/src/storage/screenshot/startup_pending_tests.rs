use super::*;
use rusqlite::StatementStatus;
use std::sync::Arc;

fn test_storage() -> (tempfile::TempDir, StorageState) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credentials);
    (temp, storage)
}

fn insert_lifecycle_rows(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO screenshots(id,image_path,image_hash,status,is_deleted) VALUES
                (3,'3.enc.pending','h3','pending',0),
                (1,'1.enc.pending','h1','pending',0),
                (2,'2.enc.pending','h2','pending',1),
                (4,'4.enc.pending','h4','aborted',0),
                (5,'5.enc.pending','h5',NULL,0);",
    )
    .unwrap();
}

fn assert_indexed_pending_ids(conn: &Connection, expected: &[i64]) {
    let plan = conn
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {STARTUP_PENDING_SCREENSHOTS_SQL}"
        ))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("; ");
    assert!(
        plan.contains("USING COVERING INDEX"),
        "startup recovery must avoid table reads: {plan}"
    );
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    let mut stmt = conn.prepare(STARTUP_PENDING_SCREENSHOTS_SQL).unwrap();
    let ids = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ids, expected);
    // VM work stays bounded on slow CI hosts as well as fast machines.
    // The old plan inspects all 10,000 historical rows, including misses.
    assert_eq!(stmt.get_status(StatementStatus::FullscanStep), 0);
    assert_eq!(stmt.get_status(StatementStatus::Sort), 0);
    let steps = stmt.get_status(StatementStatus::VmStep);
    assert!(steps < 100, "startup lookup used {steps} VM steps");
}

#[test]
fn lookup_uses_covering_index_without_statistics_on_fresh_and_upgraded_databases() {
    let (_temp, storage) = test_storage();
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute_batch(
        "WITH RECURSIVE ids(id) AS (
                SELECT 100 UNION ALL SELECT id+1 FROM ids WHERE id<10099
             )
             INSERT INTO screenshots(id,image_path,image_hash,status,metadata_enc)
             SELECT id,'history.enc',CAST(id AS TEXT),'committed',zeroblob(128) FROM ids;",
    )
    .unwrap();
    insert_lifecycle_rows(&conn);
    assert_indexed_pending_ids(&conn, &[1, 3]);

    // Reproduce an existing installation with populated tables and all
    // competing indexes, but without the new startup index or statistics.
    conn.execute_batch("DROP INDEX idx_screenshots_startup_pending;")
        .unwrap();
    for _ in 0..2 {
        storage.init_tables(&conn).unwrap();
        assert_indexed_pending_ids(&conn, &[1, 3]);
    }
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM screenshots", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        10005
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='sqlite_stat1'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.execute_batch(
        "UPDATE screenshots SET status='committed' WHERE id=1;
             UPDATE screenshots SET is_deleted=1 WHERE id=3;",
    )
    .unwrap();
    assert_indexed_pending_ids(&conn, &[]);
    conn.execute("UPDATE screenshots SET is_deleted=0 WHERE id=3", [])
        .unwrap();
    assert_indexed_pending_ids(&conn, &[3]);
}

#[test]
fn cancelled_cleanup_does_not_require_a_database_connection() {
    let (_temp, storage) = test_storage();
    assert_eq!(
        storage.abort_startup_pending_screenshots(|| true).unwrap(),
        0
    );
}

#[test]
fn cancellation_after_lock_acquisition_prevents_query_preparation() {
    let (_temp, storage) = test_storage();
    // No screenshots table: any prepared query fails. The cancellation signal
    // follows lock acquisition instead of depending on a callback count.
    *storage.db.lock().unwrap() = Some(Connection::open_in_memory().unwrap());
    let aborted = storage
        .abort_startup_pending_screenshots(|| storage.db.try_lock().is_err())
        .unwrap();
    assert_eq!(aborted, 0);
    assert!(storage.db.try_lock().is_ok());
}

#[test]
fn cleanup_only_aborts_live_pending_rows_and_can_cancel_between_files() {
    let (temp, storage) = test_storage();
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    insert_lifecycle_rows(&conn);
    *storage.db.lock().unwrap() = Some(conn);
    let mut files = Vec::new();
    for id in 1..=5 {
        let image = temp.path().join(format!("{id}.enc.pending"));
        let thumb = StorageState::thumbnail_path_for(&temp.path().join(format!("{id}.enc")));
        std::fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        std::fs::write(&image, b"test image").unwrap();
        std::fs::write(&thumb, b"test thumbnail").unwrap();
        files.push((image, thumb));
    }
    assert_eq!(
        storage
            .abort_startup_pending_screenshots(|| !files[0].0.exists())
            .unwrap(),
        1
    );
    assert!(!files[0].0.exists() && !files[0].1.exists());
    assert!(files[2].0.exists() && files[2].1.exists());
    assert_eq!(
        storage.abort_startup_pending_screenshots(|| false).unwrap(),
        1
    );
    assert!(!files[2].0.exists() && !files[2].1.exists());
    for index in [1, 3, 4] {
        assert!(files[index].0.exists() && files[index].1.exists());
    }
    let guard = storage.db.lock().unwrap();
    let statuses = guard
        .as_ref()
        .unwrap()
        .prepare("SELECT status FROM screenshots ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        statuses,
        vec![
            Some("aborted".into()),
            Some("pending".into()),
            Some("aborted".into()),
            Some("aborted".into()),
            None
        ]
    );
}
