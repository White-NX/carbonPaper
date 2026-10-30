use super::*;
use crate::credential_manager::{encrypt_with_master_key, CredentialManagerState};
use std::cell::Cell;
use std::sync::Arc;

fn row(id: i64) -> RawTimelineRow {
    let encrypt = |text: &str| Some(encrypt_with_master_key(&[7; 32], text.as_bytes()).unwrap());
    RawTimelineRow {
        id,
        image_path: format!("{id}.enc"),
        created_at: "2026-01-01 00:00:00".into(),
        timestamp: Some(1767225600),
        window_title: Some("legacy title".into()),
        process_name: Some("legacy process".into()),
        metadata: None,
        title_enc: encrypt("encrypted title"),
        process_enc: encrypt("browser.exe"),
        metadata_enc: encrypt(r#"{"process_path":"browser.exe","unused":"ignored"}"#),
        key_enc: Some(vec![1]),
        icon_id: Some(10),
        shared_icon_enc: encrypt("shared icon"),
        shared_icon_key: Some(vec![2]),
        category: Some("work".into()),
    }
}

fn encrypted_history() -> (tempfile::TempDir, StorageState, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let credential = Arc::new(CredentialManagerState::new(temp.path().into()));
    let storage = StorageState::new(temp.path().into(), credential);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    for id in 1..=20 {
        let encrypted = row(id);
        conn.execute(
            "INSERT INTO screenshots(id,image_path,image_hash,created_at,window_title,process_name,
                window_title_enc,process_name_enc,metadata_enc,content_key_encrypted,category)
             VALUES (?1, ?1 || '.enc', ?1, datetime(1767225600 + (?1 - 1) * 30, 'unixepoch'),
                'legacy title', 'legacy process', ?2, ?3, ?4, ?5, 'work')",
            params![
                id,
                encrypted.title_enc,
                encrypted.process_enc,
                encrypted.metadata_enc,
                encrypted.key_enc
            ],
        )
        .unwrap();
    }
    conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
        .unwrap();
    (temp, storage, conn)
}

#[test]
fn headers_preserve_full_timeline_positions_without_loading_or_unwrapping_details() {
    let (_temp, _storage, conn) = encrypted_history();
    for limit in [3, 500] {
        let full = read_rows(
            &conn,
            1767225600.0,
            1767226200.0,
            limit,
            &|| Ok(()),
            &mut Timings::default(),
        )
        .unwrap();
        let headers = read_rows_with_details(
            &conn,
            1767225600.0,
            1767226200.0,
            limit,
            false,
            &|| Ok(()),
            &mut Timings::default(),
        )
        .unwrap();
        let positions = |rows: &[RawTimelineRow]| {
            rows.iter()
                .map(|row| {
                    (
                        row.id,
                        row.image_path.clone(),
                        row.created_at.clone(),
                        row.timestamp,
                        row.category.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(positions(&headers), positions(&full));
        assert!(headers.iter().all(|row| row.key_enc.is_none()
            && row.title_enc.is_none()
            && row.process_enc.is_none()
            && row.metadata_enc.is_none()));
        let mut unwraps = 0;
        let records = hydrate(
            headers,
            &|| Ok(()),
            &mut |_, _| {
                unwraps += 1;
                Ok(Some(Zeroizing::new(vec![7; 32])))
            },
            &mut Timings::default(),
        )
        .unwrap();
        assert_eq!(unwraps, 0);
        for record in records {
            assert!(record.window_title.is_none() && record.process_name.is_none());
            assert!(record.process_icon.is_none() && record.process_path.is_none());
            assert!(!serde_json::to_value(record)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("content_key_encrypted"));
        }
    }
    conn.execute_batch("DROP TABLE page_icons").unwrap();
    assert_eq!(
        read_rows_with_details(
            &conn,
            1767225600.0,
            1767226200.0,
            500,
            false,
            &|| Ok(()),
            &mut Timings::default()
        )
        .unwrap()
        .len(),
        19
    );
}

#[test]
fn details_read_only_requested_live_ids_and_retain_decrypted_fields() {
    let (_temp, _storage, conn) = encrypted_history();
    let mut timings = Timings::default();
    let rows = read_rows_by_ids(&conn, &[20, 2, 2, 1, 99, -1], &|| Ok(()), &mut timings).unwrap();
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [2, 20]);
    assert_eq!(
        timings.count,
        Duration::ZERO,
        "ID reads need no range count"
    );
    let mut unwraps = 0;
    let records = hydrate(
        rows,
        &|| Ok(()),
        &mut |_, _| {
            unwraps += 1;
            Ok(Some(Zeroizing::new(vec![7; 32])))
        },
        &mut timings,
    )
    .unwrap();
    assert_eq!(unwraps, 2);
    assert!(records.iter().all(
        |record| record.window_title.as_deref() == Some("encrypted title")
            && record.process_name.as_deref() == Some("browser.exe")
            && record.process_path.as_deref() == Some("browser.exe")
    ));
    assert!(read_rows_by_ids(&conn, &[], &|| Ok(()), &mut timings)
        .unwrap()
        .is_empty());
    assert!(read_rows_by_ids(&conn, &vec![2; 501], &|| Ok(()), &mut timings).is_err());
    assert_eq!(
        read_rows_by_ids(&conn, &[2], &|| Err(CANCELLED.into()), &mut timings)
            .err()
            .unwrap(),
        CANCELLED
    );
}

#[test]
fn headers_and_details_recheck_cancellation_revocation_and_the_admitted_session() {
    let temp = tempfile::tempdir().unwrap();
    let credential = Arc::new(CredentialManagerState::new(temp.path().into()));
    crate::credential_manager::save_public_key_to_file(&credential, b"timeline-header-test-key")
        .unwrap();
    let storage = StorageState::new(temp.path().into(), Arc::clone(&credential));
    storage.initialize().unwrap();
    storage
        .get_connection_named("seed_timeline_header")
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "INSERT INTO screenshots(id,image_path,image_hash,created_at,window_title)
         VALUES (1, 'one.enc', 'one', '2026-01-01 00:00:00', 'legacy title')",
            [],
        )
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    assert_eq!(
        storage
            .get_timeline_headers(1767225600.0, 1767225660.0, 500, &stop)
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    assert_eq!(
        storage.get_timeline_details(&[1], &stop).unwrap_err(),
        "AUTH_REQUIRED"
    );
    assert_eq!(
        storage
            .get_timeline_index(1767225600000.0, 1767225660000.0, &stop)
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    credential.update_auth_time();
    assert_eq!(
        storage
            .get_timeline_index(1767225600000.0, 1767225660000.0, &stop)
            .unwrap(),
        [TimelineIndexEntry(1, 1767225600000)]
    );
    assert!(storage
        .get_timeline_headers(1767225600.0, 1767225660.0, 500, &stop)
        .unwrap()[0]
        .window_title
        .is_none());
    assert_eq!(
        storage.get_timeline_details(&[1], &stop).unwrap()[0]
            .window_title
            .as_deref(),
        Some("legacy title")
    );
    let cancelled = storage.load_timeline_records(&stop, "test_cancel", |conn, check, timings| {
        let rows = read_rows_by_ids(conn, &[1], check, timings)?;
        stop.store(true, Ordering::Release);
        Ok(rows)
    });
    assert_eq!(cancelled.unwrap_err(), CANCELLED);
    assert_eq!(
        storage.get_timeline_details(&[1], &stop).unwrap_err(),
        CANCELLED
    );
    assert_eq!(
        storage
            .get_timeline_index(1767225600000.0, 1767225660000.0, &stop)
            .unwrap_err(),
        CANCELLED
    );
    stop.store(false, Ordering::Release);
    for unlock_again in [false, true] {
        credential.update_auth_time();
        let result = storage.load_timeline_records(&stop, "test_revoke", |conn, check, timings| {
            let rows = read_rows_by_ids(conn, &[1], check, timings)?;
            credential.invalidate_session();
            if unlock_again {
                credential.update_auth_time();
            }
            Ok(rows)
        });
        assert_eq!(result.unwrap_err(), "AUTH_REQUIRED");
        credential.update_auth_time();
        let result = storage.load_timeline_index(&stop, |conn, check| {
            let rows = read_index_rows(conn, 1767225600000.0, 1767225660000.0, check)?;
            credential.invalidate_session();
            if unlock_again {
                credential.update_auth_time();
            }
            Ok(rows)
        });
        assert_eq!(result.unwrap_err(), "AUTH_REQUIRED");
    }
    assert!(storage
        .try_database_maintenance("after_timeline_header")
        .is_some());
}

#[test]
fn repeated_icons_unwrap_once_and_return_only_timeline_fields() {
    let mut timings = Timings::default();
    let mut keys = Vec::new();
    let records = hydrate(
        (1..=338).map(row).collect(),
        &|| Ok(()),
        &mut |key, _| {
            keys.push(key.to_vec());
            Ok(Some(Zeroizing::new(vec![7; 32])))
        },
        &mut timings,
    )
    .unwrap();
    assert_eq!(keys.len(), 339, "338 row keys plus one shared icon key");
    assert_eq!(keys.iter().filter(|key| key.as_slice() == [2]).count(), 1);
    assert_eq!(timings.icon_cache_hits, 337);
    assert!(records
        .iter()
        .all(|record| record.process_icon.as_deref() == Some("shared icon")));
    let record = serde_json::to_value(&records[0]).unwrap();
    assert_eq!(record["window_title"], "encrypted title");
    assert_eq!(record["process_name"], "browser.exe");
    assert_eq!(record["process_path"], "browser.exe");
    assert_eq!(record["timestamp"], 1767225600_i64);
    assert_eq!(record["created_at"], "2026-01-01T00:00:00Z");
    for unused in ["metadata", "visible_links", "page_url", "image_hash"] {
        assert!(record.get(unused).is_none());
    }
}

#[test]
fn unreadable_row_keys_preserve_legacy_labels_without_hiding_auth_errors() {
    let records = hydrate(
        vec![row(1)],
        &|| Ok(()),
        &mut |_, _| Ok(None),
        &mut Timings::default(),
    )
    .unwrap();
    assert_eq!(records[0].window_title.as_deref(), Some("legacy title"));
    assert_eq!(records[0].process_name.as_deref(), Some("legacy process"));
    assert!(records[0].process_icon.is_none());
    let error = hydrate(
        vec![row(1)],
        &|| Ok(()),
        &mut |_, _| Err("AUTH_REQUIRED".into()),
        &mut Timings::default(),
    )
    .unwrap_err();
    assert_eq!(error, "AUTH_REQUIRED");
}

#[test]
fn process_icon_precedence_skips_shared_icon_and_plaintext_labels_still_work() {
    let mut native = row(1);
    native.metadata_enc =
        Some(encrypt_with_master_key(&[7; 32], br#"{"process_icon":"app icon"}"#).unwrap());
    let mut legacy = row(2);
    legacy.key_enc = None;
    legacy.title_enc = None;
    legacy.process_enc = None;
    legacy.metadata_enc = None;
    let mut calls = 0;
    let records = hydrate(
        vec![native, legacy],
        &|| Ok(()),
        &mut |_, _| {
            calls += 1;
            Ok(Some(Zeroizing::new(vec![7; 32])))
        },
        &mut Timings::default(),
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(records[0].process_icon.as_deref(), Some("app icon"));
    assert_eq!(records[1].window_title.as_deref(), Some("legacy title"));
    assert_eq!(records[1].process_icon.as_deref(), Some("shared icon"));
}

#[test]
fn cancellation_and_revocation_stop_before_cached_icon_or_partial_response() {
    for error in [CANCELLED, "AUTH_REQUIRED"] {
        let allowed = Cell::new(true);
        let mut calls = 0;
        let result = hydrate(
            vec![row(1), row(2)],
            &|| {
                if allowed.get() {
                    Ok(())
                } else {
                    Err(error.to_owned())
                }
            },
            &mut |_, _| {
                calls += 1;
                // First row completes. The next row loses authorization while its
                // row key is unwrapped; it must not consume the shared icon cache.
                if calls == 3 {
                    allowed.set(false);
                }
                Ok(Some(Zeroizing::new(vec![7; 32])))
            },
            &mut Timings::default(),
        );
        assert_eq!(result.unwrap_err(), error);
        assert_eq!(calls, 3);
    }
}

#[test]
fn sampling_preserves_buckets_order_limits_and_soft_deletion_without_link_sets() {
    let temp = tempfile::tempdir().unwrap();
    let credential = Arc::new(CredentialManagerState::new(temp.path().into()));
    let storage = StorageState::new(temp.path().into(), credential);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    for id in 1..=20 {
        conn.execute(
            "INSERT INTO screenshots(id,image_path,image_hash,created_at)
            VALUES (?1, 'image.enc', ?1, datetime(1767225600 + (?1 - 1) * 30, 'unixepoch'))",
            [id],
        )
        .unwrap();
    }
    conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
        .unwrap();
    // A timeline has no dependency on hyperlink storage at all.
    conn.execute_batch("DROP TABLE link_sets").unwrap();
    let mut timings = Timings::default();
    let read = |limit, timings: &mut Timings| {
        read_rows(
            &conn,
            1767225600.0,
            1767226200.0,
            limit,
            &|| Ok(()),
            timings,
        )
        .unwrap()
    };
    assert_eq!(
        read(3, &mut timings)
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        [2, 11]
    );
    assert_eq!(read(500, &mut timings).len(), 19);
    assert_eq!(read(0, &mut timings).len(), 1);
    assert_eq!(read(-1, &mut timings).len(), 1);
    assert!(read_rows(&conn, f64::NAN, 0.0, 500, &|| Ok(()), &mut timings).is_err());
}

#[test]
fn index_reads_are_unsampled_covering_and_independent_of_encrypted_payloads() {
    let (_temp, _storage, conn) = encrypted_history();
    conn.execute_batch(
        "WITH RECURSIVE ids(id) AS (SELECT 100 UNION ALL SELECT id + 1 FROM ids WHERE id < 1099)
        INSERT INTO screenshots(id,image_path,image_hash,created_at)
        SELECT id, 'encrypted.enc', id, datetime(1767225600 + id, 'unixepoch') FROM ids;
        DROP TABLE page_icons;",
    )
    .unwrap();
    let rows = read_index_rows(&conn, 1767225600000.0, 1767226800000.0, &|| Ok(())).unwrap();
    assert_eq!(rows.len(), 1019);
    assert!(rows
        .windows(2)
        .all(|pair| (pair[0].1, pair[0].0) < (pair[1].1, pair[1].0)));
    assert!(rows.iter().all(|entry| entry.0 != 1));
    assert_eq!(
        serde_json::to_value(&rows[0]).unwrap(),
        serde_json::json!([2, 1767225630000_i64])
    );
    let plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {INDEX_RANGE_SQL}"))
        .unwrap()
        .query_map(
            params!["2026-01-01 00:00:00", "2026-01-01 00:20:00"],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("COVERING INDEX idx_screenshots_deleted_created_at")),
        "{plan:?}"
    );
}

#[test]
fn index_includes_exact_outside_neighbours_and_duplicate_timestamp_order() {
    let (_temp, _storage, conn) = encrypted_history();
    conn.execute_batch(
        "INSERT INTO screenshots(id,image_path,image_hash,created_at) VALUES
        (21, 'a.enc', 'a', '2026-01-01 00:01:30'),
        (22, 'b.enc', 'b', '2026-01-01 00:02:30');",
    )
    .unwrap();
    let base = 1767225600000.0;
    let rows = read_index_rows(&conn, base + 95000.25, base + 125000.75, &|| Ok(())).unwrap();
    assert_eq!(
        rows,
        [
            TimelineIndexEntry(21, 1767225690000),
            TimelineIndexEntry(5, 1767225720000),
            TimelineIndexEntry(6, 1767225750000)
        ]
    );
    let rows = read_index_rows(&conn, base + 90000.0, base + 150000.0, &|| Ok(())).unwrap();
    assert_eq!(
        rows.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        [3, 4, 21, 5, 6, 22, 7]
    );
    let gap = read_index_rows(&conn, base + 85000.0, base + 86000.0, &|| Ok(())).unwrap();
    assert_eq!(gap.iter().map(|entry| entry.0).collect::<Vec<_>>(), [3, 4]);
    for (offset, expected) in [
        (90000.0, 21),
        (105000.0, 21),
        (105000.1, 5),
        (135000.1, 6),
        (150000.0, 22),
    ] {
        let nearest =
            read_nearest_rows(&conn, base + offset, &|| Ok(()), &mut Timings::default()).unwrap();
        assert_eq!(nearest[0].id, expected);
    }
    assert_eq!(
        read_index_rows(&conn, base, base + 1000.0, &|| Err(CANCELLED.into())).unwrap_err(),
        CANCELLED
    );
    for (start, end) in [
        (f64::NAN, base),
        (base, f64::INFINITY),
        (base, base),
        (base + 1.0, base),
    ] {
        assert!(read_index_rows(&conn, start, end, &|| Ok(())).is_err());
    }
}
