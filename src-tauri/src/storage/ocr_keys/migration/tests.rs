use super::*;

fn fixture() -> (
    tempfile::TempDir,
    StorageState,
    Vec<(i64, Vec<u8>, Vec<u8>)>,
) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(temp.path());
    let storage = StorageState::new(temp.path().into(), credentials);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO screenshots(id,image_path,image_hash) VALUES(7,'7.enc','7'),(8,'8.enc','8');
        INSERT INTO recap_days(day,start_ms,end_ms,covered_until_ms,privacy_hash) VALUES('fixture',0,4000000000000,4000000000000,'fixture');").unwrap();
    let mut originals = Vec::new();
    for id in 1..=4 {
        let (data, key) = storage
            .encrypt_payload_with_row_key(format!("migration text {id}").as_bytes())
            .unwrap();
        conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted,confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
            VALUES(?1,7,'',?2,?3,1,0,0,1,0,1,1,0,1)", params![id,data,key]).unwrap();
        originals.push((id, data, key));
    }
    *storage.db.lock().unwrap() = Some(conn);
    (temp, storage, originals)
}

#[test]
fn encrypted_migration_resumes_without_invalidating_processing_or_search() {
    let (_temp, storage, originals) = fixture();
    let before = {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.query_row("SELECT (SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7),revision FROM recap_days", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap()
    };
    assert_eq!(storage.compact_ocr_keys_batch().unwrap(), 4);
    assert_eq!(storage.compact_ocr_keys_batch().unwrap(), 0);
    let rows = storage.get_screenshot_ocr_results_silent(7).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>(),
        (1..=4)
            .map(|id| format!("migration text {id}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(storage.process_lazy_indexing_batch().unwrap(), 4);
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(text_key_encrypted) FROM ocr_results",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let after = conn.query_row("SELECT (SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7),revision FROM recap_days", [], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).unwrap();
    assert_eq!(before, after);
    assert!(
        conn.query_row("SELECT count(*) FROM ocr_index_pending", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            == 0
    );
    // Mixed history stays readable even after the scan cursor reached the end.
    conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted) VALUES(10,7,'',?1,?2)", params![originals[0].1,originals[0].2]).unwrap();
    let (data, reference): (Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT text_enc,text_key_ref FROM ocr_results_read WHERE id=10",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        storage.ocr_decoder().decrypt(&data, &reference).unwrap(),
        b"migration text 1"
    );
}

#[test]
fn new_commits_share_one_fresh_key_per_batch_and_can_encrypt_while_locked() {
    let (_temp, storage, _) = fixture();
    let input = vec![
        crate::storage::OcrResultInput {
            text: "new capture text".into(),
            confidence: 0.99,
            box_coords: vec![
                vec![0.0, 0.0],
                vec![1.0, 0.0],
                vec![1.0, 1.0],
                vec![0.0, 1.0]
            ],
        };
        2
    ];
    assert_eq!(
        storage
            .commit_screenshot(7, Some(&input), None, None)
            .unwrap()
            .added,
        2
    );
    let first = {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        assert_eq!(
            conn.query_row("SELECT live_rows FROM ocr_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        let (data, reference): (Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT text_enc,text_key_ref FROM ocr_results_read WHERE id=5",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            storage.ocr_decoder().decrypt(&data, &reference).unwrap(),
            b"new capture text"
        );
        conn.query_row("SELECT ocr_key_id FROM ocr_results WHERE id=5", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    storage.credential_state.clear_all_cached_keys();
    assert_eq!(
        storage
            .commit_screenshot(7, Some(&input), None, None)
            .unwrap()
            .added,
        2
    );
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let next: i64 = conn
        .query_row("SELECT ocr_key_id FROM ocr_results WHERE id=7", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(first, next);
    assert_eq!(conn.query_row("SELECT count(*) FROM ocr_results WHERE id>=5 AND text IS NULL AND text_key_encrypted IS NULL AND ocr_key_id IS NOT NULL", [], |r| r.get::<_,i64>(0)).unwrap(),4);
}

#[test]
fn failed_checkpoint_leaves_every_legacy_envelope_and_revision_intact() {
    let (_temp, storage, originals) = fixture();
    {
        let guard = storage.db.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_ocr_key_checkpoint BEFORE INSERT ON app_metadata
            WHEN NEW.key='storage_ocr_key_cursor_v1' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .unwrap();
    }
    assert!(storage.compact_ocr_keys_batch().is_err());
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        for (id, data, key) in originals {
            assert_eq!(
                conn.query_row(
                    "SELECT text_enc,text_key_encrypted FROM ocr_results WHERE id=?1",
                    [id],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
                )
                .unwrap(),
                (data, key)
            );
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM ocr_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN ('recap_o_UPDATE','app_bound_ocr_update')", [], |r| r.get::<_,i64>(0)).unwrap(),2);
        conn.execute_batch("DROP TRIGGER fail_ocr_key_checkpoint;")
            .unwrap();
    }
    assert_eq!(storage.compact_ocr_keys_batch().unwrap(), 4);
}

#[test]
fn revocation_and_concurrent_deletion_cannot_publish_partial_or_stale_rewrites() {
    let (_temp, storage, originals) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let dataset = dataset(conn).unwrap();
    let key = storage.new_ocr_write_key().unwrap();
    let mut rows = Vec::new();
    for (id, data, wrapped) in originals {
        let encrypted = key
            .encrypt(&dataset, 7, id, format!("migration text {id}").as_bytes())
            .unwrap();
        rows.push(Rewrite {
            source: LegacyRow {
                id,
                screenshot: 7,
                encrypted: Some(data),
                wrapped: Some(wrapped),
                grouped: false,
                deleted: false,
            },
            encrypted,
        });
    }
    let keys = HashMap::from([(7, key)]);
    let calls = std::cell::Cell::new(0);
    assert!(publish(conn, &dataset, 4, &rows, &keys, &|| {
        calls.set(calls.get() + 1);
        if calls.get() > 1 {
            Err("AUTH_REQUIRED".into())
        } else {
            Ok(())
        }
    })
    .is_err());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    conn.execute("DELETE FROM ocr_results WHERE id=1", [])
        .unwrap();
    publish(conn, &dataset, 4, &rows, &keys, &|| Ok(())).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_results WHERE id=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT live_rows FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM app_metadata WHERE key=?1",
            [CURSOR],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
