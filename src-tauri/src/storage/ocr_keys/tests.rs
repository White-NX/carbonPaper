use super::*;
use crate::credential_manager::encrypt_with_master_key;

fn fixture() -> (tempfile::TempDir, StorageState) {
    let temp = tempfile::tempdir().unwrap();
    let storage = crate::storage::test_storage_with_schema(temp.path().into());
    {
        let guard = storage.db.lock().unwrap();
        guard.as_ref().unwrap().execute_batch("PRAGMA foreign_keys=ON; INSERT INTO screenshots(id,image_path,image_hash) VALUES(7,'7.enc','7'),(8,'8.enc','8');").unwrap();
    }
    (temp, storage)
}

fn insert(conn: &Connection, key: &WriteKey, id: i64, text: &[u8]) -> Vec<u8> {
    let data = key.encrypt(&dataset(conn).unwrap(), 7, id, text).unwrap();
    conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,ocr_key_id) VALUES(?1,7,'',?2,?3)", params![id,data,key.id]).unwrap();
    data
}

fn reference(conn: &Connection, id: i64) -> Vec<u8> {
    conn.query_row(
        "SELECT text_key_ref FROM ocr_results_read WHERE id=?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn grouped_text_authenticates_rows_groups_dataset_and_ocr_key_scope() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let key = WriteKey::new(|key| Ok(key.to_vec())).unwrap();
    key.insert(conn, &dataset(conn).unwrap(), 7).unwrap();
    let first = insert(conn, &key, 1, "first 文本".as_bytes());
    let second = insert(conn, &key, 2, b"second");
    let first_ref = reference(conn, 1);
    assert_eq!(
        decode(&first, &first_ref, &|key| Ok(key.to_vec())).unwrap(),
        "first 文本".as_bytes()
    );
    assert!(decode(&second, &first_ref, &|key| Ok(key.to_vec())).is_err());
    for index in [9, 17, 25, 42] {
        let mut changed = first_ref.clone();
        changed[index] ^= 1;
        assert!(decode(&first, &changed, &|key| Ok(key.to_vec())).is_err());
    }
    let mut tampered = first.clone();
    tampered[13] ^= 1;
    assert!(decode(&tampered, &first_ref, &|key| Ok(key.to_vec())).is_err());
    // Replacing the group envelope with an image key cannot turn this reader
    // into an image decryption path: the OCR scope MAC is verified first.
    let image_key = [3u8; 32];
    conn.execute(
        "UPDATE ocr_keys SET wrapped_key=?1 WHERE id=?2",
        params![image_key.as_slice(), key.id],
    )
    .unwrap();
    let image = encrypt_with_master_key(&image_key, b"private image").unwrap();
    assert!(decode(&image, &reference(conn, 1), &|key| Ok(key.to_vec())).is_err());
    // A descriptor injected into a legacy key field is wrapped by SQL as data.
    conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted) VALUES(3,7,'',?1,?2)", params![first,first_ref]).unwrap();
    assert!(decode(&first, &reference(conn, 3), &|key| Ok(key.to_vec())).is_err());
}

#[test]
fn shared_keys_keep_independent_nonces_and_transactional_member_counts() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let key = WriteKey::new(|key| Ok(key.to_vec())).unwrap();
    key.insert(conn, &dataset(conn).unwrap(), 7).unwrap();
    let mut nonces = std::collections::HashSet::new();
    for id in 1..=64 {
        nonces.insert(insert(conn, &key, id, b"same text")[..12].to_vec());
    }
    assert_eq!(nonces.len(), 64);
    assert_eq!(
        conn.query_row("SELECT live_rows FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        64
    );
    conn.execute_batch("BEGIN; DELETE FROM ocr_results; ROLLBACK;")
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT live_rows FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        64
    );
    assert!(conn.execute("DELETE FROM ocr_keys", []).is_err());
    assert!(conn
        .execute("UPDATE ocr_results SET screenshot_id=8 WHERE id=1", [])
        .is_err());
    conn.execute("DELETE FROM screenshots WHERE id=7", [])
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn request_cache_reuses_group_unwraps_and_rejects_a_new_or_revoked_session() {
    let (temp, storage) = fixture();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(temp.path());
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let key = WriteKey::new(|key| Ok(key.to_vec())).unwrap();
    key.insert(conn, &dataset(conn).unwrap(), 7).unwrap();
    let data = insert(conn, &key, 1, b"text");
    let reference = reference(conn, 1);
    let decoder = Decoder::new(&credentials);
    let calls = std::cell::Cell::new(0);
    let unwrap = |key: &[u8]| {
        calls.set(calls.get() + 1);
        Ok(key.to_vec())
    };
    for _ in 0..3 {
        assert_eq!(
            decoder
                .decrypt_with_unwrap(&data, &reference, &unwrap)
                .unwrap(),
            b"text"
        );
    }
    assert_eq!(calls.get(), 1);
    credentials.update_auth_time();
    assert!(matches!(
        decoder.decrypt_with_unwrap(&data, &reference, &unwrap),
        Err(BackgroundReadError::AuthRequired)
    ));
    let decoder = Decoder::new(&credentials);
    decoder
        .decrypt_with_unwrap(&data, &reference, &unwrap)
        .unwrap();
    credentials.clear_all_cached_keys();
    assert!(matches!(
        decoder.decrypt_with_unwrap(&data, &reference, &unwrap),
        Err(BackgroundReadError::AuthRequired)
    ));
    assert_eq!(calls.get(), 2);
}
