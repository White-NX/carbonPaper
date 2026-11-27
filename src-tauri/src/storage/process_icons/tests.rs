use super::*;

fn legacy(id: i64, metadata: &str) -> LegacyMetadata {
    LegacyMetadata {
        id,
        plain: None,
        encrypted: Some(encrypt_with_master_key(&[7; 32], metadata.as_bytes()).unwrap()),
        wrapped_key: Some(vec![1]),
        icon_id: None,
        deleted: false,
    }
}

fn fixture() -> (tempfile::TempDir, StorageState, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let credential = std::sync::Arc::new(crate::credential_manager::CredentialManagerState::new(
        temp.path().into(),
    ));
    let storage = StorageState::new(temp.path().into(), credential);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    (temp, storage, conn)
}

fn insert(conn: &Connection, row: &LegacyMetadata) {
    conn.execute(
        "INSERT INTO screenshots(id,image_path,image_hash,metadata_enc,content_key_encrypted)
         VALUES(?1,'test.enc',?1,?2,?3)",
        params![row.id, row.encrypted, row.wrapped_key],
    )
    .unwrap();
}

fn icon(conn: &Connection, plain: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT OR IGNORE INTO page_icons(content_hash,icon_enc,icon_key_encrypted) VALUES(?1,?2,X'02')",
        params![StorageState::compute_static_hash(plain), encrypt_with_master_key(&[9; 32], plain.as_bytes()).unwrap()],
    ).unwrap();
    Ok(conn
        .query_row(
            "SELECT id FROM page_icons WHERE content_hash=?1",
            [StorageState::compute_static_hash(plain)],
            |r| r.get(0),
        )
        .unwrap())
}

#[test]
fn shared_icons_round_trip_metadata_and_survive_orphan_cleanup() {
    let (_temp, _storage, conn) = fixture();
    let original = r#"{"process_icon":"same icon","process_path":"app.exe","nested":{"value":7}}"#;
    for id in [1, 2] {
        let row = legacy(id, original);
        insert(&conn, &row);
        let rewrite = prepare(row, &|_| Ok(vec![7; 32])).unwrap().unwrap();
        publish(&conn, vec![rewrite], id, &mut icon).unwrap();
    }
    StorageState::cleanup_orphaned_dedup_entries(&conn).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM page_icons", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let (metadata, icon_enc): (Vec<u8>, Vec<u8>) = conn.query_row(
        "SELECT s.metadata_enc,p.icon_enc FROM screenshots s JOIN page_icons p ON p.id=s.process_icon_id WHERE s.id=1",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    let compact = String::from_utf8(decrypt_with_master_key(&[7; 32], &metadata).unwrap()).unwrap();
    assert!(!compact.contains("process_icon"));
    let shared = String::from_utf8(decrypt_with_master_key(&[9; 32], &icon_enc).unwrap()).unwrap();
    let restored = restore_metadata_icon(Some(compact), Some(shared)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&restored).unwrap(),
        serde_json::from_str::<Value>(original).unwrap()
    );
    let raw = conn
        .query_row(
            &format!("{} WHERE s.id=1", super::super::RawScreenshotRow::SELECT),
            [],
            super::super::RawScreenshotRow::from_row,
        )
        .unwrap();
    assert_eq!(raw.process_icon_ref_enc, Some(icon_enc));
    conn.execute("DELETE FROM screenshots WHERE id=1", [])
        .unwrap();
    StorageState::cleanup_orphaned_dedup_entries(&conn).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM page_icons", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    conn.execute("DELETE FROM screenshots", []).unwrap();
    StorageState::cleanup_orphaned_dedup_entries(&conn).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM page_icons", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn failed_checkpoint_and_racing_metadata_edits_keep_the_original() {
    let (_temp, _storage, conn) = fixture();
    let row = legacy(1, r#"{"process_icon":"icon"}"#);
    let original = row.encrypted.clone();
    insert(&conn, &row);
    conn.execute_batch(
        "CREATE TRIGGER fail_icon_checkpoint BEFORE INSERT ON app_metadata
        WHEN NEW.key='storage_process_icon_cursor_v1' BEGIN SELECT RAISE(ABORT,'injected'); END;",
    )
    .unwrap();
    let rewrite = prepare(row, &|_| Ok(vec![7; 32])).unwrap().unwrap();
    assert!(publish(&conn, vec![rewrite], 1, &mut icon).is_err());
    assert_eq!(
        conn.query_row("SELECT metadata_enc FROM screenshots WHERE id=1", [], |r| r
            .get::<_, Option<Vec<u8>>>(0))
            .unwrap(),
        original
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM page_icons", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_icon_checkpoint;")
        .unwrap();
    let row = LegacyMetadata {
        encrypted: original,
        ..legacy(1, "")
    };
    let rewrite = prepare(row, &|_| Ok(vec![7; 32])).unwrap().unwrap();
    conn.execute("UPDATE screenshots SET metadata_enc=X'01' WHERE id=1", [])
        .unwrap();
    publish(&conn, vec![rewrite], 1, &mut icon).unwrap();
    assert_eq!(
        conn.query_row("SELECT metadata_enc FROM screenshots WHERE id=1", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        vec![1]
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

#[test]
fn legacy_shapes_and_locked_keys_are_preserved() {
    for plain in [
        "[]",
        "null",
        "not JSON",
        r#"{"process_icon":3}"#,
        r#"{"process_icon":""}"#,
    ] {
        assert!(prepare(legacy(1, plain), &|_| Ok(vec![7; 32]))
            .unwrap()
            .is_none());
        assert_eq!(
            restore_metadata_icon(Some(plain.into()), None),
            Some(plain.into())
        );
    }
    assert!(matches!(
        prepare(legacy(1, r#"{"process_icon":"icon"}"#), &|_| Err(
            CredentialError::AuthRequired
        )),
        Err(CredentialError::AuthRequired)
    ));
    let mut row = legacy(1, r#"{"process_icon":"icon"}"#);
    row.encrypted.as_mut().unwrap()[13] ^= 1;
    assert!(prepare(row, &|_| Ok(vec![7; 32])).is_err());
}
