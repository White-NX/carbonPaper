use super::*;

fn fixture() -> (tempfile::TempDir, StorageState, i64) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(temp.path());
    let storage = StorageState::new(temp.path().into(), credentials);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
        INSERT INTO screenshots(id,image_path,image_hash) VALUES(7,'7.enc','7'),(8,'8.enc','8');",
    )
    .unwrap();
    let records = (5..=7)
        .map(|id| Record {
            id,
            text: "bounded member".into(),
            text_layout: None,
            confidence: 1.0,
            box_coords: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        })
        .collect::<Vec<_>>();
    let key = storage.new_ocr_write_key().unwrap();
    insert_records(&conn, 7, &records, Some(&key), || {
        storage.new_ocr_write_key()
    })
    .unwrap();
    *storage.db.lock().unwrap() = Some(conn);
    (temp, storage, key.id)
}

#[test]
fn garbage_collection_uses_row_id_bounds_when_the_corpus_grows() {
    let (_temp, storage, key) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    for extra in [0, 10000] {
        if extra > 0 {
            conn.execute_batch("WITH RECURSIVE ids(id) AS (SELECT 100 UNION ALL SELECT id+1 FROM ids WHERE id<10099)
                INSERT INTO ocr_results(id,screenshot_id,text,text_hash) SELECT id,8,'synthetic','' FROM ids;").unwrap();
        }
        let sql = block_members_sql();
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([key], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|p| p.contains("SEARCH r USING INTEGER PRIMARY KEY")
                    && p.contains("rowid>?")
                    && p.contains("rowid<?")),
            "{plan:?}"
        );
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt
            .query_map([key], Member::from_row)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 6, 7]);
        assert!(stmt.get_status(rusqlite::StatementStatus::VmStep) < 500);
    }
}

#[test]
fn authorization_failure_rolls_back_repacking_and_changed_members_are_preserved() {
    let (_temp, storage, key) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute("DELETE FROM ocr_results WHERE id=5", [])
        .unwrap();
    let decoder = storage.ocr_decoder();
    let work = read_work(conn, &decoder).unwrap().unwrap();
    let rewrites = prepare(&storage, &work, &decoder, &|| Ok(())).unwrap();
    let calls = std::cell::Cell::new(0);
    assert!(publish(conn, &work, &rewrites, &|| {
        calls.set(calls.get() + 1);
        if calls.get() > 1 {
            Err("AUTH_REQUIRED".into())
        } else {
            Ok(())
        }
    })
    .is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ocr_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    conn.execute(
        "UPDATE ocr_results SET created_at='2026-11-19 10:00:00' WHERE id=6",
        [],
    )
    .unwrap();
    assert_eq!(publish(conn, &work, &rewrites, &|| Ok(())).unwrap(), 1);
    assert_eq!(
        conn.query_row("SELECT ocr_key_id FROM ocr_results WHERE id=6", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        key
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM ocr_block_gc WHERE block_id=?1",
            [key],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(guard);
    assert_eq!(storage.compact_ocr_blocks_batch().unwrap(), 1);
    let rows = storage.get_screenshot_ocr_results_silent(7).unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![6, 7]);
    assert_eq!(rows[0].created_at, "2026-11-19T10:00:00Z");
}

#[test]
fn ordinary_maintenance_leaves_legacy_rows_for_the_offline_converter() {
    let (_temp, storage, _) = fixture();
    let (data, wrapped) = storage
        .encrypt_payload_with_row_key(b"legacy fixture")
        .unwrap();
    {
        let guard = storage.db.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute(
                "INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted)
            VALUES(1,8,'already indexed',?1,?2)",
                params![data, wrapped],
            )
            .unwrap();
    }
    assert_eq!(storage.compact_ocr_blocks_batch().unwrap(), 0);
    storage.run_manual_vacuum().unwrap();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let preserved = conn.query_row("SELECT text_enc,text_key_encrypted,ocr_key_id,ocr_block_slot,text_hash FROM ocr_results WHERE id=1",
        [], |r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,String>(4)?))).unwrap();
    assert_eq!(
        preserved,
        (data, wrapped, None, None, "already indexed".into())
    );
}
