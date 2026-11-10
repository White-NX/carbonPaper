use super::*;

fn fixture() -> (tempfile::TempDir, StorageState) {
    let temp = tempfile::tempdir().unwrap();
    let storage = crate::storage::test_storage_with_schema(temp.path().into());
    (temp, storage)
}

fn seed(conn: &Connection, count: i64) -> Vec<(String, Vec<u8>)> {
    let tx = conn.unchecked_transaction().unwrap();
    let mut expected = Vec::new();
    for n in 1..=count {
        let bytes = vec![(n % 251) as u8; 2148];
        let subject = format!("{n:08}");
        tx.execute("INSERT INTO derived_index_jobs(index_kind,subject_key,status,model_id,model_revision,embedding_version,source_fingerprint)
            VALUES('clip_image',?1,'completed','model','revision',1,'source')", [&subject]).unwrap();
        tx.execute("INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,model_id,model_revision,embedding_version,source_fingerprint)
            VALUES('clip_image',?1,512,?2,'model','revision',1,'source')", params![subject,bytes]).unwrap();
        expected.push((subject, bytes));
    }
    tx.commit().unwrap();
    expected
}

fn resolved(conn: &Connection) -> Vec<(String, Vec<u8>)> {
    conn.prepare("SELECT subject_key,vector_f32 FROM derived_embeddings_read ORDER BY subject_key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn epoch(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT data_epoch FROM derived_index_state WHERE index_kind='clip_image'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_fresh_tail_of_large_vectors_uses_the_byte_budget_without_waiting_for_more_rows() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    compact(conn).unwrap(); // Finish the empty legacy scan before new writes.
    seed(conn, 4);
    let bytes = vec![3u8; MAX_VECTOR_BYTES];
    conn.execute("UPDATE derived_embeddings SET vector_f32=?1", [&bytes])
        .unwrap();
    assert_eq!(compact(conn).unwrap(), 4);
    assert!(resolved(conn).iter().all(|(_, value)| value == &bytes));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM derived_embeddings WHERE vector_block_id IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        4
    );
}

#[test]
fn packing_preserves_every_ciphertext_and_content_epoch_across_resume() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let expected = seed(conn, 512);
    let original_epoch = epoch(conn);
    let pages = || {
        conn.query_row(
        "SELECT sum(pgsize) FROM dbstat WHERE name IN ('derived_embeddings','vector_blocks','vector_pack_pending','idx_derived_embeddings_block')",
        [], |r| r.get::<_, i64>(0)).unwrap()
    };
    let before = pages();
    assert_eq!(compact(conn).unwrap(), PAGE_ROWS);
    assert_eq!(resolved(conn), expected);
    initialize(conn).unwrap();
    while compact(conn).unwrap() != 0 {}
    validate(conn).unwrap();
    assert_eq!(resolved(conn), expected);
    assert_eq!(epoch(conn), original_epoch);
    assert!(
        conn.query_row("SELECT max(length(data)) FROM vector_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            <= MAX_BLOCK_BYTES as i64
    );
    // This checks the attainable physical footprint after the existing manual
    // vacuum. The online migration itself never runs a blocking full VACUUM.
    conn.execute_batch("VACUUM;").unwrap();
    let after = pages();
    eprintln!("Synthetic vector table + block allocation: {before} -> {after} bytes");
    assert!(after * 10 < before * 7, "before={before}, after={after}");
}

#[test]
fn checkpoint_failure_rolls_back_blocks_rows_and_notification_triggers() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let expected = seed(conn, 20);
    let original_epoch = epoch(conn);
    conn.execute_batch(
        "CREATE TRIGGER fail_block_checkpoint BEFORE INSERT ON app_metadata
        WHEN NEW.key='storage_vector_block_cursor_v1' BEGIN SELECT RAISE(ABORT,'injected'); END;",
    )
    .unwrap();
    assert!(compact(conn).is_err());
    assert_eq!(resolved(conn), expected);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM vector_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(epoch(conn), original_epoch);
    conn.execute_batch("DROP TRIGGER fail_block_checkpoint;")
        .unwrap();
    compact(conn).unwrap();
    assert_eq!(epoch(conn), original_epoch);
    // A real replacement detaches the reference and still invalidates caches.
    conn.execute(
        "UPDATE derived_embeddings SET vector_f32=X'01020304' WHERE subject_key='00000001'",
        [],
    )
    .unwrap();
    assert!(epoch(conn) > original_epoch);
    assert_eq!(resolved(conn)[0].1, vec![1, 2, 3, 4]);
    validate(conn).unwrap();
}

#[test]
fn deleted_and_replaced_vectors_release_blocks_without_stale_reads() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let expected = seed(conn, 64);
    while compact(conn).unwrap() != 0 {}
    conn.execute(
        "DELETE FROM derived_embeddings WHERE subject_key<'00000049'",
        [],
    )
    .unwrap();
    while compact(conn).unwrap() != 0 {}
    assert_eq!(resolved(conn), expected[48..]);
    let size: i64 = conn
        .query_row("SELECT sum(length(data)) FROM vector_blocks", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(size, 16 * 2148);
    conn.execute(
        "UPDATE derived_embeddings SET vector_offset=-1 WHERE subject_key='00000049'",
        [],
    )
    .unwrap();
    assert!(validate(conn).is_err());
    conn.execute("DELETE FROM derived_embeddings", []).unwrap();
    while compact(conn).unwrap() != 0 {}
    assert_eq!(
        conn.query_row("SELECT count(*) FROM vector_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
