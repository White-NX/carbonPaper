use super::*;
use crate::storage::StorageState;

#[test]
fn new_commits_use_fresh_block_keys_and_can_encrypt_while_locked() {
    let (_temp, storage) = fixture();
    let layout = crate::ml_protocol::MlOcrTextLayout {
        matrix: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
        source_size: [2, 2],
        input_width: 64,
        valid_width: 64,
        time_steps: 16,
        centers: (0..16).map(|i| 2 * i + 1).collect(),
        text_lengths: vec![1; 16],
    };
    let input = vec![
        crate::storage::OcrResultInput {
            text_layout: Some(layout.clone()),
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
        conn.query_row("SELECT ocr_key_id FROM ocr_results WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let rows = storage.get_screenshot_ocr_results_silent(7).unwrap();
    assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);
    for row in rows {
        assert_eq!(row.text, "new capture text");
        assert_eq!(row.text_layout.as_ref(), Some(&layout));
    }
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
    let second = conn
        .query_row("SELECT ocr_key_id FROM ocr_results WHERE id=3", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM ocr_results WHERE text_enc IS NULL AND text_key_encrypted IS NULL AND ocr_block_slot IS NOT NULL",
        [], |r|r.get::<_,i64>(0)).unwrap(), 4);
}

fn fixture() -> (tempfile::TempDir, StorageState) {
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
    *storage.db.lock().unwrap() = Some(conn);
    (temp, storage)
}

fn record(id: i64, text: &str) -> Record {
    let x = id as f64 * 20.0;
    Record {
        id,
        text: text.into(),
        text_layout: None,
        confidence: 0.987654321,
        box_coords: [[x, 10.0], [x + 100.0, 10.0], [x + 100.0, 30.0], [x, 30.0]],
    }
}

fn references(conn: &Connection) -> Vec<(i64, Vec<u8>)> {
    let mut stmt = conn
        .prepare("SELECT id,text_key_ref FROM ocr_results_read ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn short_lines_share_one_authenticated_payload_and_reject_moved_slots_and_revocation() {
    let (_temp, storage) = fixture();
    let records: Vec<_> = (1..=64).map(|id| record(id, "按钮")).collect();
    let key = WriteKey::new(|key| Ok(key.to_vec())).unwrap();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    insert_records(conn, 7, &records, Some(&key), || {
        WriteKey::new(|key| Ok(key.to_vec()))
    })
    .unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT live_rows FROM ocr_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        64
    );
    assert_eq!(conn.query_row("SELECT count(*) FROM ocr_results WHERE text_enc IS NULL AND text_key_encrypted IS NULL
        AND confidence IS NULL AND box_x2 IS NULL AND created_at IS NULL AND ocr_block_slot IS NOT NULL", [], |r| r.get::<_,i64>(0)).unwrap(),64);
    let refs = references(conn);
    let decoder = storage.ocr_decoder();
    decoder
        .load_blocks(conn, refs.iter().map(|(_, r)| r.as_slice()))
        .unwrap();
    let calls = std::cell::Cell::new(0);
    let unwrap = |key: &[u8]| {
        calls.set(calls.get() + 1);
        Ok(key.to_vec())
    };
    for (id, reference) in &refs {
        assert_eq!(
            decoder
                .decrypt_with_unwrap(&[], reference, &unwrap)
                .unwrap(),
            "按钮".as_bytes()
        );
        let metadata = decoder
            .metadata_with_unwrap(reference, &unwrap)
            .unwrap()
            .unwrap();
        assert_eq!(metadata.box_coords, records[(*id - 1) as usize].box_coords);
        assert_eq!(
            metadata.confidence.to_bits(),
            records[(*id - 1) as usize].confidence.to_bits()
        );
    }
    assert_eq!(calls.get(), 1);
    for index in [9, 17, 25, 41, 50] {
        let mut moved = refs[0].1.clone();
        moved[index] ^= 1;
        assert!(decoder.decrypt_with_unwrap(&[], &moved, &unwrap).is_err());
    }
    assert!(conn.execute("DELETE FROM ocr_blocks", []).is_err());
    storage.credential_state.clear_all_cached_keys();
    assert!(matches!(
        decoder.decrypt_with_unwrap(&[], &refs[0].1, &unwrap),
        Err(crate::storage::BackgroundReadError::AuthRequired)
    ));
}

#[test]
fn geometry_float_bits_multi_scalar_tokens_and_size_overrides_round_trip() {
    let mut expected = record(9, "🇨🇳 a");
    expected.box_coords = [[1.25, -0.0], [-9.5, 3.0], [100.25, 30.5], [-0.0, 4.0]];
    expected.text_layout = Some(crate::ml_protocol::MlOcrTextLayout {
        matrix: [[80.0, 3.0, 2.0], [1.0, 18.0, 4.0], [0.05, 0.0, 1.0]],
        source_size: [1920, 1080],
        input_width: 32,
        valid_width: 30,
        time_steps: 8,
        centers: vec![1, 3, 5],
        text_lengths: vec![8, 1, 1],
    });
    let source_size = [3840, 2160];
    let bytes = codec::record_bytes(&expected, source_size).unwrap();
    let encoded = codec::encode(&[(expected.id, bytes)], source_size).unwrap();
    let block = Decoded::open(&encoded).unwrap();
    assert_eq!(block.text(0, expected.id).unwrap(), expected.text);
    let actual = block.record(0, expected.id).unwrap();
    assert_eq!(actual.text_layout, expected.text_layout);
    assert_eq!(
        actual.box_coords.map(|r| r.map(f64::to_bits)),
        expected.box_coords.map(|r| r.map(f64::to_bits))
    );
    assert_eq!(actual.confidence.to_bits(), expected.confidence.to_bits());
    assert!(block.text(0, expected.id + 1).is_err());
}

#[test]
fn offline_python_blocks_preserve_records_and_authenticated_bindings() {
    // Produced by the standalone converter, without running Python in tests.
    // The AES key is synthetic; the unwrap closure stands in for CNG.
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/python_converter_block_v2.json")).unwrap();
    let bytes = |name: &str| hex::decode(golden[name].as_str().unwrap()).unwrap();
    let key = bytes("key_hex");
    let encoded = bytes("envelope_hex");
    let block = Decoded::open(&encoded).unwrap();
    assert_eq!(block.bounds(), (2, 140));
    assert_eq!(block.len(), 3);
    let expected_text = ["UI", "🇨🇳 a", "中a"];
    for (slot, (id, text)) in [2, 9, 140].into_iter().zip(expected_text).enumerate() {
        assert_eq!(block.text(slot, id).unwrap(), text);
    }
    let record = block.record(1, 9).unwrap();
    assert_eq!(record.confidence.to_bits(), 0.99123456789f64.to_bits());
    assert_eq!(
        record.box_coords.map(|r| r.map(f64::to_bits)),
        [[1.25, -0.0], [-9.5, 3.0], [100.25, 30.5], [-0.0, 4.0]].map(|r| r.map(f64::to_bits))
    );
    let layout = record.text_layout.unwrap();
    assert_eq!(layout.source_size, [120, 50]);
    assert_eq!(layout.centers, [1, 3, 5]);
    assert_eq!(layout.text_lengths, [8, 1, 1]);
    assert_eq!(
        layout.matrix.map(|r| r.map(f32::to_bits)),
        [[80.0, 3.0, 2.0], [1.0, 18.0, 4.0], [0.05, 0.02, 2.0]].map(|r| r.map(f32::to_bits))
    );
    let scalar = block.record(2, 140).unwrap().text_layout.unwrap();
    assert_eq!(scalar.source_size, [1920, 1080]);
    assert_eq!(scalar.text_lengths, [3, 1]);

    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    let dataset = golden["dataset"].as_str().unwrap();
    let screenshot = golden["screenshot_id"].as_i64().unwrap();
    let group = golden["key_id"].as_i64().unwrap();
    conn.execute(
        "UPDATE app_metadata SET value=?1 WHERE key='protected_content_dataset_id'",
        [dataset],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ocr_keys(id,screenshot_id,wrapped_key,scope_tag) VALUES(?1,?2,?3,?4)",
        rusqlite::params![group, screenshot, key, bytes("scope_tag_hex")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ocr_blocks(id,screenshot_id,content_enc,created_at,first_row_id,last_row_id)
        VALUES(?1,?2,?3,'2026-11-20 01:02:03',2,140)",
        rusqlite::params![group, screenshot, bytes("content_enc_hex")],
    )
    .unwrap();
    for (slot, id) in [2, 9, 140].into_iter().enumerate() {
        let coords = block.record(slot, id).unwrap().box_coords;
        conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,ocr_key_id,ocr_block_slot,box_x1,box_y1,created_at)
            VALUES(?1,?2,'',?3,?4,?5,?6,NULL)",
            rusqlite::params![id, screenshot, group, slot as i64, coords[0][0], coords[0][1]]).unwrap();
    }
    let refs = references(conn);
    let decoder = storage.ocr_decoder();
    decoder
        .load_blocks(conn, refs.iter().map(|(_, r)| r.as_slice()))
        .unwrap();
    for (slot, (id, reference)) in refs.iter().enumerate() {
        let unwrap = |wrapped: &[u8]| Ok(wrapped.to_vec());
        assert_eq!(
            decoder
                .decrypt_with_unwrap(&[], reference, &unwrap)
                .unwrap(),
            expected_text[slot].as_bytes()
        );
        let metadata = decoder
            .metadata_with_unwrap(reference, &unwrap)
            .unwrap()
            .unwrap();
        let expected = block.record(slot, *id).unwrap();
        assert_eq!(metadata.confidence.to_bits(), expected.confidence.to_bits());
        assert_eq!(
            metadata.box_coords.map(|r| r.map(f64::to_bits)),
            expected.box_coords.map(|r| r.map(f64::to_bits))
        );
    }
}

#[test]
fn deleting_a_member_rekeys_and_rewrites_the_block_without_dead_text_or_ids() {
    let (_temp, storage) = fixture();
    let records = [
        record(1, "deleted secret"),
        record(2, "retained text"),
        record(3, "retained third"),
    ];
    let old_key = storage.new_ocr_write_key().unwrap();
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        insert_records(conn, 7, &records, Some(&old_key), || {
            storage.new_ocr_write_key()
        })
        .unwrap();
        // A malformed block locator can carry an obsolete inline field.
        // Cleanup must compare the raw field and clear it without retrying forever.
        conn.execute("UPDATE ocr_results SET text_enc=X'00' WHERE id=2", [])
            .unwrap();
        conn.execute("DELETE FROM ocr_results WHERE id=1", [])
            .unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM ocr_block_gc", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(storage.compact_ocr_blocks_batch().unwrap(), 2);
    let retained = storage.get_screenshot_ocr_results_silent(7).unwrap();
    assert_eq!(
        retained.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(retained[0].text, "retained text");
    assert_eq!(
        retained[0].box_coords,
        records[1].box_coords.map(Vec::from).to_vec()
    );
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM ocr_blocks WHERE id=?1",
            [old_key.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_block_gc", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let refs = references(conn);
    let decoder = storage.ocr_decoder();
    decoder
        .load_blocks(conn, refs.iter().map(|(_, r)| r.as_slice()))
        .unwrap();
    assert_eq!(decoder.decrypt(&[], &refs[1].1).unwrap(), b"retained third");
    let data: Vec<u8> = conn
        .query_row("SELECT content_enc FROM ocr_blocks", [], |r| r.get(0))
        .unwrap();
    let mut tampered = data;
    tampered[13] ^= 1;
    conn.execute("UPDATE ocr_blocks SET content_enc=?1", [tampered])
        .unwrap();
    let decoder = storage.ocr_decoder();
    decoder
        .load_blocks(conn, refs.iter().map(|(_, r)| r.as_slice()))
        .unwrap();
    assert!(decoder.decrypt(&[], &refs[0].1).is_err());
    conn.execute("DELETE FROM screenshots WHERE id=7", [])
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM ocr_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn block_limits_reject_oversized_decompression_and_incomplete_frames() {
    let rows: Vec<_> = (1..=codec::MAX_ROWS as i64 + 1)
        .map(|id| {
            let row = record(id, "x");
            (id, codec::record_bytes(&row, [0, 0]).unwrap())
        })
        .collect();
    assert!(codec::encode(&rows, [0, 0]).is_err());
    let encoded = codec::encode(&rows[..64], [0, 0]).unwrap();
    let block = Decoded::open(&encoded).unwrap();
    assert_eq!(block.len(), 64);
    for length in 0..encoded.len() {
        assert!(Decoded::open(&encoded[..length]).is_err());
    }
    let mut oversized = vec![1];
    oversized.extend_from_slice(&((codec::MAX_BYTES + 1) as u32).to_le_bytes());
    oversized.extend_from_slice(&encoded[5..]);
    assert!(Decoded::open(&oversized).is_err());
    let chunks = chunks(&(1..=600).map(|id| record(id, "x")).collect::<Vec<_>>()).unwrap();
    assert_eq!(chunks.len(), 2);
}
