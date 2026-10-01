//! Exercise the real SQLCipher and row-encryption paths using an unnamed key.
use super::*;
use crate::ai::recap::{day_bounds, PrivacyFingerprint, RecapBatch, RecapSettings};
use crate::sensitive_filter::SensitiveFilterConfig;
use crate::storage::StorageState;
use serde_json::{json, Value};
use std::sync::Arc;
use windows::Win32::Security::Cryptography::NCryptExportKey;

fn storage_credentials(data_dir: PathBuf) -> Arc<CredentialManagerState> {
    let state = authorized_state();
    state.set_data_dir(data_dir);
    let key = ephemeral_key();
    let blob_type = HSTRING::from("RSAPUBLICBLOB");
    let mut length = 0;
    // SAFETY: the unnamed test key, blob name and output length are live.
    unsafe {
        NCryptExportKey(
            key.key,
            NCRYPT_KEY_HANDLE::default(),
            &blob_type,
            None,
            None,
            &mut length,
            NCRYPT_FLAGS(0),
        )
    }
    .unwrap();
    let mut public_key = vec![0; length as usize];
    // SAFETY: the output buffer has the queried size and is exclusively owned.
    unsafe {
        NCryptExportKey(
            key.key,
            NCRYPT_KEY_HANDLE::default(),
            &blob_type,
            None,
            Some(&mut public_key),
            &mut length,
            NCRYPT_FLAGS(0),
        )
    }
    .unwrap();
    public_key.truncate(length as usize);
    *state.cached_public_key.lock().unwrap() = Some(public_key);
    *state.cached_private_key.lock().unwrap() = Some(key);
    Arc::new(state)
}

#[test]
fn recap_icons_read_encrypted_metadata_skip_favicons_and_require_unlock() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = storage_credentials(temp.path().to_path_buf());
    let storage = StorageState::new(temp.path().to_path_buf(), credentials.clone());
    storage.initialize().unwrap();
    let save = |hash: &str, source: &str, metadata: Value| {
        let request = serde_json::from_value(json!({
            "image_data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a1WQAAAAASUVORK5CYII=",
            "image_hash": hash, "width": 1, "height": 1,
            "process_name": "Editor", "source": source, "metadata": metadata,
        })).unwrap();
        storage
            .save_screenshot(&request)
            .unwrap()
            .screenshot_id
            .unwrap()
    };
    let native = save("native", "capture", json!({"process_icon": "native-icon"}));
    let missing = save("missing", "capture", json!({"process_icon": null}));
    let extension = save(
        "extension",
        "extension",
        json!({"process_icon": "website-icon"}),
    );
    assert_eq!(
        storage
            .recap_process_icon(&[extension, missing, native])
            .unwrap()
            .as_deref(),
        Some("native-icon")
    );
    assert_eq!(
        storage.recap_process_icon(&[extension, missing]).unwrap(),
        None
    );
    assert_eq!(storage.recap_process_icon(&[native + 1000]).unwrap(), None);
    credentials.invalidate_session();
    assert_eq!(
        storage.recap_process_icon(&[native]).unwrap_err(),
        "AUTH_REQUIRED"
    );
    storage.shutdown().unwrap();
}

#[test]
fn correction_jobs_survive_restart_and_only_acknowledge_the_processed_version() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = storage_credentials(temp.path().to_path_buf());
    let storage = StorageState::new(temp.path().to_path_buf(), credentials.clone());
    storage.initialize().unwrap();
    let date = "2026-01-01";
    let history = json!({"current": {"names": {"task": "Renamed"},
        "assignments": {}, "merges": {}}, "undo": []});
    storage
        .recap_write(
            "corrections",
            date,
            date,
            -1,
            storage.db_generation(),
            &history,
        )
        .unwrap();
    assert_eq!(storage.recap_pending_summaries().unwrap()[date], 1);
    storage.shutdown().unwrap();
    drop(storage);

    let reopened = StorageState::new(temp.path().to_path_buf(), credentials.clone());
    reopened.initialize().unwrap();
    let generation = reopened.db_generation();
    let version = reopened.recap_pending_summaries().unwrap()[date];
    assert_eq!(version, 1);
    assert_eq!(
        reopened
            .recap_read::<Value>("corrections", date, None)
            .unwrap(),
        Some(history.clone())
    );
    // A correction arriving during summarization must survive the old run's ack.
    reopened
        .recap_write("corrections", date, date, -1, generation, &history)
        .unwrap();
    reopened
        .recap_finish_summary_update(date, version, generation)
        .unwrap();
    assert_eq!(reopened.recap_pending_summaries().unwrap()[date], 2);
    assert_eq!(
        reopened
            .recap_finish_summary_update(date, 2, generation + 1)
            .unwrap_err(),
        "RECAP_SOURCE_CHANGED"
    );
    reopened
        .recap_finish_summary_update(date, 2, generation)
        .unwrap();
    assert!(reopened.recap_pending_summaries().unwrap().is_empty());
    reopened.shutdown().unwrap();
    reopened.initialize().unwrap();
    assert!(reopened.recap_pending_summaries().unwrap().is_empty());
    let generation = reopened.db_generation();
    let undo = json!({"current": {"names": {}, "assignments": {}, "merges": {}}, "undo": []});
    reopened
        .recap_write("corrections", date, date, -1, generation, &undo)
        .unwrap();
    reopened
        .recap_finish_summary_update(date, 1, generation)
        .unwrap();
    assert_eq!(reopened.recap_pending_summaries().unwrap()[date], 3);
    credentials.invalidate_session();
    assert_eq!(
        reopened.recap_pending_summaries().unwrap_err(),
        "AUTH_REQUIRED"
    );
    assert_eq!(
        reopened
            .recap_finish_summary_update(date, 3, generation)
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    reopened.shutdown().unwrap();
}

#[test]
fn recap_payloads_survive_database_reopen_and_equivalent_policy_reload() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = storage_credentials(temp.path().to_path_buf());
    let storage = StorageState::new(temp.path().to_path_buf(), credentials.clone());
    storage.initialize().unwrap();
    let config = SensitiveFilterConfig::default();
    let saved_policy = serde_json::to_value(&config).unwrap();
    let privacy = PrivacyFingerprint::new(config).unwrap();
    let day = "2026-01-01";
    let (start, end) = day_bounds(day).unwrap();
    let revision = storage
        .recap_day_revision(day, start, end, &privacy)
        .unwrap()
        .0;
    let generation = storage.db_generation();
    let batch_key = format!("{day}:{start}");
    let source = json!({"id": 42, "timestamp_ms": start + 1000,
        "process_name": "Synthetic app", "window_title": "Synthetic source"});
    let batch = json!({
        "start_ms": start, "end_ms": start + 14_400_000, "updated_at_ms": start,
        "status": "ready", "activities": [{
            "id": "a1", "task_id": "task", "task_title": "Synthetic task",
            "text": "Synthetic activity", "start_ms": start + 1000,
            "end_ms": start + 1000, "segments": ["s1"], "sources": [source.clone()],
            "member_ids": [42]
        }], "records": [source.clone()], "coverage": 1, "error": null, "attempts": []
    });
    let payloads = vec![
        ("batch", batch_key.as_str(), revision, batch),
        ("index", batch_key.as_str(), revision, json!([source])),
        (
            "summary",
            batch_key.as_str(),
            revision,
            json!({
                "input_hash": "synthetic", "updated_at_ms": start, "error": null,
                "summary": {"overview": "Synthetic overview", "overview_activity_ids": ["a1"],
                    "topics": [{"task_id": "task", "title": "Synthetic task",
                        "text": "Synthetic summary", "activity_ids": ["a1"]}]}
            }),
        ),
        (
            "screen",
            "synthetic-screen",
            revision,
            json!({"category": "code", "confidence": 1.0,
            "result": false, "related_previous": null}),
        ),
        (
            "settings",
            "settings",
            -1,
            serde_json::to_value(RecapSettings::default()).unwrap(),
        ),
        (
            "corrections",
            day,
            -1,
            json!({"current": {"names": {"task": "Renamed"},
            "assignments": {"42": "task"}, "merges": {}}, "undo": []}),
        ),
    ];
    for (kind, key, revision, value) in &payloads {
        storage
            .recap_write(kind, key, day, *revision, generation, value)
            .unwrap();
    }
    storage.shutdown().unwrap();
    drop(storage);

    let reopened = StorageState::new(temp.path().to_path_buf(), credentials.clone());
    reopened.initialize().unwrap();
    for _ in 0..8 {
        let loaded = serde_json::from_value(saved_policy.clone()).unwrap();
        let privacy = PrivacyFingerprint::new(loaded).unwrap();
        assert_eq!(
            reopened
                .recap_day_revision(day, start, end, &privacy)
                .unwrap()
                .0,
            revision
        );
        for (kind, key, revision, value) in &payloads {
            let actual = reopened
                .recap_read::<Value>(kind, key, Some(*revision))
                .unwrap();
            assert_eq!(
                actual.as_ref(),
                Some(value),
                "encrypted disk roundtrip for {kind}"
            );
        }
    }
    let typed = reopened
        .recap_read::<RecapBatch>("batch", &batch_key, Some(revision))
        .unwrap()
        .unwrap();
    assert_eq!(typed.activities[0].text, "Synthetic activity");
    // Old activity payloads without the new derived-summary fields still decode.
    assert!(typed.summary.is_none());

    let changed = PrivacyFingerprint::new(SensitiveFilterConfig {
        enabled: false,
        ..Default::default()
    })
    .unwrap();
    let next_revision = reopened
        .recap_day_revision(day, start, end, &changed)
        .unwrap()
        .0;
    assert_eq!(next_revision, revision + 1);
    for kind in ["batch", "summary", "index"] {
        assert!(reopened
            .recap_read::<Value>(kind, &batch_key, Some(next_revision))
            .unwrap()
            .is_none());
    }
    assert!(reopened
        .recap_read::<Value>("corrections", day, None)
        .unwrap()
        .is_some());
    credentials.invalidate_session();
    assert_eq!(
        reopened
            .recap_read::<Value>("batch", &batch_key, None)
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    reopened.shutdown().unwrap();
}
