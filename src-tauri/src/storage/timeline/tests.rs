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
        icon_enc: None,
        icon_id: Some(10),
        shared_icon_enc: encrypt("shared icon"),
        shared_icon_key: Some(vec![2]),
        category: Some("work".into()),
    }
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
