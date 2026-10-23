use super::*;
use crate::ai::{
    config::{AiSettings, ProviderKind, ResolvedProvider, ToolCalling},
    recap::{
        self,
        protected::{mirror, ProtectedRun},
        RecapSettings,
    },
};
use crate::processing_stage::recap::{RecapConfig, SourceVersion};
use serde_json::{json, Value};
use std::collections::HashSet;

fn config(storage: &StorageState) -> RecapConfig {
    RecapConfig {
        settings: RecapSettings {
            enabled: true,
            ..Default::default()
        },
        provider: ResolvedProvider {
            kind: ProviderKind::OpenaiCompatible,
            base_url: "http://localhost:1234/v1".into(),
            model: "test".into(),
            api_key: Some("private-api-key".into()),
            tool_calling: ToolCalling::Unknown,
            context_tokens: 32768,
        },
        remote_consent: false,
        revision: storage.recap_config_revision().unwrap(),
        ai_hash: crate::processing_stage::recap::ai_hash(&AiSettings::load(storage).unwrap())
            .unwrap(),
    }
}

#[test]
fn recap_consumer_completion_preserves_all_other_consumers_and_its_own_context() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    insert_input(storage, 1, Consumer::ALL_MASK);
    let work = storage
        .processing_stage
        .claim(storage, Consumer::Recap)
        .unwrap()
        .unwrap();
    storage
        .processing_stage
        .put_recap(
            storage,
            "day:2026-01-01:evidence:1",
            &json!({"text":work.input.ocr_text}),
            &[1],
        )
        .unwrap();
    storage.recap_record_input_receipt(&work.receipt).unwrap();
    storage
        .processing_stage
        .finish(storage, &work.receipt)
        .unwrap();
    restart_stage(dir, storage, broker);
    let Response::TaskState(state) = broker
        .call(Request::InspectTask {
            task_id: work.receipt.task_id.clone(),
        })
        .unwrap()
    else {
        panic!()
    };
    assert!(state.active);
    assert_eq!(state.finished_consumers, Consumer::Recap.bit());
    for consumer in [
        Consumer::Classification,
        Consumer::MiniLm,
        Consumer::Clip,
        Consumer::SmartCluster,
    ] {
        let work = storage
            .processing_stage
            .claim(storage, consumer)
            .unwrap()
            .unwrap();
        assert!(work.input.ocr_text.contains("private captured input"));
        storage
            .processing_stage
            .finish(storage, &work.receipt)
            .unwrap();
    }
    assert!(storage
        .processing_stage
        .get_recap::<Value>(storage, "day:2026-01-01:evidence:1")
        .unwrap()
        .is_some());
    assert!(storage
        .get_screenshot_summaries_by_ids_silent(&[1])
        .is_err());
}

#[test]
fn recap_config_and_cross_period_corrections_survive_locked_restart_and_delete_revokes_copies() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    insert_input(storage, 1, Consumer::Recap.bit());
    let config = config(storage);
    storage
        .processing_stage
        .put_recap(storage, "config", &config, &[])
        .unwrap();
    let source = SourceVersion {
        id: 1,
        revision: storage.staged_source_revision(1).unwrap(),
    };
    mirror(storage,"2026-01-01","corrections","2026-01-01",-1,"privacy",std::slice::from_ref(&source),&json!({"current":{"names":{"task-1":"Corrected task"},"merges":{},"assignments":{}},"undo":[]})).unwrap();
    let slot = storage
        .processing_stage
        .recap_slots("day:2026-01-01:payload:")
        .unwrap()
        .pop()
        .unwrap();
    let stage = Connection::open(dir.join(STAGING_FILE)).unwrap();
    let copied:(String,String,Vec<u8>,String,String,i64)=stage.query_row("SELECT object_id,binding,payload,digest,content_hash,created FROM recap_objects WHERE slot=?1",[&slot],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    restart_stage(dir, storage, broker);
    assert!(get_cached_master_key(&storage.credential_state).is_none());
    assert_eq!(
        storage
            .processing_stage
            .get_recap::<RecapConfig>(storage, "config")
            .unwrap()
            .unwrap()
            .provider
            .api_key
            .as_deref(),
        Some("private-api-key")
    );
    let saved = storage
        .processing_stage
        .get_recap::<Value>(storage, &slot)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved["value"]["current"]["names"]["task-1"],
        "Corrected task"
    );
    sql(storage, "UPDATE screenshots SET is_deleted=1 WHERE id=1");
    assert!(storage
        .processing_stage
        .get_recap::<Value>(storage, &slot)
        .unwrap()
        .is_none());
    stage
        .execute(
            "INSERT OR REPLACE INTO recap_objects VALUES(?1,?2,?3,?4,?5,?6,?7,'active',?8)",
            params![
                copied.0,
                storage.processing_dataset_id().unwrap(),
                slot,
                copied.1,
                copied.2,
                copied.3,
                copied.4,
                copied.5
            ],
        )
        .unwrap();
    assert!(storage
        .processing_stage
        .get_recap::<Value>(storage, &slot)
        .unwrap()
        .is_none());
    assert!(storage
        .processing_stage
        .get_recap::<RecapConfig>(storage, "config")
        .unwrap()
        .is_some());
}

#[test]
fn recap_disable_retires_only_its_consumer_and_reenable_cannot_restore_old_context() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    insert_input(storage, 1, Consumer::ALL_MASK);
    storage
        .processing_stage
        .put_recap(storage, "config", &config(storage), &[])
        .unwrap();
    storage
        .processing_stage
        .put_recap(
            storage,
            "day:2026-01-01:evidence:1",
            &json!({"text":"secret"}),
            &[1],
        )
        .unwrap();
    storage.processing_stage.revoke_recap().unwrap();
    assert!(storage
        .processing_stage
        .claim(storage, Consumer::Recap)
        .unwrap()
        .is_none());
    for consumer in [
        Consumer::Classification,
        Consumer::MiniLm,
        Consumer::Clip,
        Consumer::SmartCluster,
    ] {
        assert!(storage
            .processing_stage
            .claim(storage, consumer)
            .unwrap()
            .is_some());
    }
    assert!(storage
        .processing_stage
        .get_recap::<Value>(storage, "day:2026-01-01:evidence:1")
        .unwrap()
        .is_none());
}

#[test]
fn recap_metadata_from_a_previous_database_generation_cannot_attach_to_reused_ids() {
    let mut fixture = fixture();
    let (_dir, storage, _broker) = fixture.parts();
    insert_input(storage, 1, Consumer::Recap.bit());
    storage
        .processing_stage
        .put_recap(storage, "config", &config(storage), &[])
        .unwrap();
    let stale_generation = storage.db_generation().wrapping_add(1);
    assert_eq!(
        storage
            .processing_stage
            .put_recap_metadata(
                storage,
                1,
                stale_generation,
                Some("https://private.example/old"),
                None
            )
            .unwrap_err(),
        "RECAP_SOURCE_CHANGED"
    );
    assert!(storage
        .processing_stage
        .recap_slots("metadata:")
        .unwrap()
        .is_empty());
    storage
        .processing_stage
        .put_recap_metadata(
            storage,
            1,
            storage.db_generation(),
            Some("https://private.example/current"),
            None,
        )
        .unwrap();
    let metadata = storage
        .processing_stage
        .get_recap::<crate::processing_stage::recap::CaptureMetadata>(storage, "metadata:1")
        .unwrap()
        .unwrap();
    assert_eq!(metadata.page_url, "https://private.example/current");
}

#[cfg(windows)]
#[test]
fn locked_recap_writes_use_public_key_encryption_and_leave_public_reads_locked() {
    let mut fixture = fixture();
    let (dir, storage, _broker) = fixture.parts();
    storage.credential_state =
        crate::credential_manager::cng_tests::recap::storage_credentials(dir.to_path_buf());
    let settings = RecapSettings {
        enabled: true,
        ..Default::default()
    };
    storage
        .recap_write(
            "settings",
            "settings",
            "",
            -1,
            storage.db_generation(),
            &settings,
        )
        .unwrap();
    storage.credential_state.invalidate_session();
    assert!(!storage.is_session_valid());
    assert!(storage.is_background_authorized());
    assert_eq!(
        storage
            .recap_read::<Value>("settings", "settings", None)
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    assert!(
        storage
            .recap_read_background::<RecapSettings>("settings", "settings", None)
            .unwrap()
            .unwrap()
            .enabled
    );
    storage
        .recap_write_background(
            "batch",
            "test",
            "",
            -1,
            storage.db_generation(),
            &json!({"text":"private result"}),
        )
        .unwrap();
    storage.credential_state.clear_all_cached_keys();
    assert!(storage
        .recap_read_background::<Value>("batch", "test", None)
        .is_err());
    storage.credential_state =
        crate::credential_manager::cng_tests::recap::storage_credentials(dir.to_path_buf());
    storage.credential_state.invalidate_session();
    storage.credential_state.revoke_background_lease();
    storage
        .processing_stage
        .put_recap(storage, "config", &config(storage), &[])
        .unwrap();
    let date = "2026-01-01";
    let (start, end) = recap::day_bounds(date).unwrap();
    let privacy = recap::PrivacyFingerprint::new(Default::default()).unwrap();
    let (revision, _, _) = storage
        .recap_day_revision(date, start, end, &privacy)
        .unwrap();
    let run = ProtectedRun {
        date: date.into(),
        config: config(storage),
        config_hash: storage
            .processing_stage
            .recap_content_hash("config")
            .unwrap()
            .unwrap(),
        generation: storage.db_generation(),
        correction_version: storage.recap_correction_version(date).unwrap(),
        evidence: vec![],
        sources: vec![],
        input_ids: HashSet::new(),
    };
    storage
        .recap_commit_protected(
            &run,
            "batch",
            "protected-result",
            date,
            revision,
            run.generation,
            &json!({"text":"private protected result"}),
        )
        .unwrap();
    let encrypted: Vec<u8> = storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT data FROM recap_payloads WHERE key='protected-result'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("private protected result"));
    assert!(storage
        .recap_read::<Value>("batch", "protected-result", Some(revision))
        .is_err());
    storage.processing_stage.revoke_recap().unwrap();
    assert!(storage
        .recap_commit_protected(
            &run,
            "batch",
            "protected-result",
            date,
            revision,
            run.generation,
            &json!({"text":"late"})
        )
        .is_err());
}

#[test]
fn protected_period_prompt_reuses_corrected_task_after_restart_without_archive_keys() {
    use crate::processing_stage::recap::{CaptureEvidence, SavedEvidence};
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    let date = "2026-01-01";
    let calendar = crate::calendar::CalendarDay::local(date).unwrap();
    let first = calendar.period_boundaries_ms[0] + 60_000;
    let second = calendar.period_boundaries_ms[1] + 60_000;
    insert_input(storage, 1, Consumer::Recap.bit());
    sql(
        storage,
        &format!(
            "UPDATE screenshots SET created_at='{}' WHERE id=1",
            chrono::DateTime::from_timestamp_millis(first)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
        ),
    );
    let config = config(storage);
    storage
        .processing_stage
        .put_recap(storage, "config", &config, &[])
        .unwrap();
    let filter = crate::sensitive_filter::SensitiveFilterState::default();
    let privacy = recap::PrivacyFingerprint::new(filter.get_config()).unwrap();
    let (revision, _, _) = storage
        .recap_day_revision(date, calendar.start_ms, calendar.end_ms, &privacy)
        .unwrap();
    let source = SourceVersion {
        id: 1,
        revision: storage.staged_source_revision(1).unwrap(),
    };
    let evidence = |id, timestamp_ms| recap::Evidence {
        id,
        timestamp_ms,
        process_name: "code.exe".into(),
        window_title: "Project notes".into(),
        page_url: String::new(),
        text: "Viewed project notes about the same concrete project".into(),
        segment: String::new(),
        context: String::new(),
        novelty: 0.,
        span_ms: 0,
        screening: Default::default(),
    };
    let batch = recap::RecapBatch {
        start_ms: calendar.period_boundaries_ms[0],
        end_ms: calendar.period_boundaries_ms[1],
        updated_at_ms: 0,
        status: "ready".into(),
        records: vec![recap::SourceRef::from(&evidence(1, first))],
        coverage: 1,
        error: None,
        attempts: vec![],
        summary: None,
        summary_error: None,
        activities: vec![recap::RecapActivity {
            id: "activity-1".into(),
            task_id: "task-1".into(),
            task_title: "Project work".into(),
            text: "Viewed project notes".into(),
            details: vec![],
            start_ms: first,
            end_ms: first,
            segments: vec![],
            sources: vec![recap::SourceRef::from(&evidence(1, first))],
            member_ids: vec![1],
        }],
    };
    let first_key = format!("{date}:{}", calendar.start_ms);
    mirror(
        storage,
        date,
        "batch",
        &first_key,
        revision,
        privacy.as_str(),
        std::slice::from_ref(&source),
        &batch,
    )
    .unwrap();
    let correction = recap::CorrectionHistory {
        current: recap::Corrections {
            names: std::collections::BTreeMap::from([(
                "task-1".into(),
                "User corrected project".into(),
            )]),
            ..Default::default()
        },
        undo: vec![],
    };
    mirror(
        storage,
        date,
        "corrections",
        date,
        -1,
        privacy.as_str(),
        std::slice::from_ref(&source),
        &correction,
    )
    .unwrap();
    restart_stage(dir, storage, broker);
    insert_input(storage, 2, Consumer::Recap.bit());
    sql(
        storage,
        &format!(
            "UPDATE screenshots SET created_at='{}' WHERE id=2",
            chrono::DateTime::from_timestamp_millis(second)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
        ),
    );
    let saved = SavedEvidence {
        evidence: evidence(2, second),
        source: SourceVersion {
            id: 2,
            revision: storage.staged_source_revision(2).unwrap(),
        },
        capture: CaptureEvidence::default(),
    };
    storage
        .processing_stage
        .put_recap(storage, &format!("day:{date}:evidence:2"), &saved, &[2])
        .unwrap();
    assert!(get_cached_master_key(&storage.credential_state).is_none());
    assert!(!storage.is_silent_read_authorized());
    let run = recap::protected::prepare_with_filter(storage, date, &filter)
        .unwrap()
        .unwrap();
    assert!(run
        .covers_period(
            storage,
            calendar.period_boundaries_ms[1],
            calendar.period_boundaries_ms[2]
        )
        .unwrap());
    let mut batches = vec![run
        .read::<recap::RecapBatch>(
            storage,
            "batch",
            &first_key,
            Some(revision),
            privacy.as_str(),
        )
        .unwrap()
        .unwrap()];
    let corrections = run
        .read::<recap::CorrectionHistory>(storage, "corrections", date, None, privacy.as_str())
        .unwrap()
        .unwrap();
    let threads = recap::apply_corrections(&mut batches, &corrections.current);
    let prior = recap::RecapDay {
        date: date.into(),
        calendar,
        batches,
        threads,
        usage: Default::default(),
        can_undo: false,
        running: false,
        error: None,
    };
    let mut records = run
        .evidence
        .iter()
        .map(|s| s.evidence.clone())
        .collect::<Vec<_>>();
    let (request, selected) = recap::initial_request(
        &records,
        &prior,
        &run.config.settings,
        &run.config.provider,
        second,
    )
    .unwrap();
    let crate::ai::provider::Message::User(payload) = &request.messages[1] else {
        panic!()
    };
    let payload: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(payload["existing_tasks"][0]["id"], "task-1");
    assert_eq!(
        payload["existing_tasks"][0]["title"],
        "User corrected project"
    );
    assert_eq!(selected[0].id, 2);
    records.clear();
    assert!(storage
        .recap_read::<Value>("batch", &first_key, None)
        .is_err());
}

#[test]
fn protected_recap_rejects_unsynchronized_settings_and_missing_or_stale_corrections() {
    let mut fixture = fixture();
    let (dir, storage, broker) = fixture.parts();
    let date = "2026-01-01";
    let filter = crate::sensitive_filter::SensitiveFilterState::default();
    let privacy = recap::PrivacyFingerprint::new(filter.get_config()).unwrap();
    storage
        .processing_stage
        .put_recap(storage, "config", &config(storage), &[])
        .unwrap();
    let initial = recap::protected::prepare_with_filter(storage, date, &filter)
        .unwrap()
        .unwrap();
    initial.check(storage).unwrap();
    assert!(initial
        .read::<Value>(storage, "corrections", date, None, privacy.as_str())
        .unwrap()
        .is_none());
    sql(
        storage,
        "INSERT INTO recap_payloads VALUES('settings','settings','',-1,X'01',X'02')",
    );
    assert_eq!(initial.check(storage).unwrap_err(), "RECAP_SOURCE_CHANGED");
    assert!(
        recap::protected::prepare_with_filter(storage, date, &filter)
            .unwrap()
            .is_none()
    );
    {
        let guard = storage.db.lock().unwrap();
        assert_eq!(
            initial
                .check_sources_on_connection(storage, guard.as_ref().unwrap())
                .unwrap_err(),
            "RECAP_SOURCE_CHANGED"
        );
    }
    storage
        .processing_stage
        .put_recap(storage, "config", &config(storage), &[])
        .unwrap();
    mirror(
        storage,
        date,
        "corrections",
        date,
        -1,
        privacy.as_str(),
        &[],
        &json!({"current":{}}),
    )
    .unwrap();
    let run = recap::protected::prepare_with_filter(storage, date, &filter)
        .unwrap()
        .unwrap();
    sql(
        storage,
        "INSERT INTO recap_payloads VALUES('corrections','2026-01-01','2026-01-01',-1,X'01',X'02')",
    );
    assert_eq!(run.check(storage).unwrap_err(), "RECAP_SOURCE_CHANGED");
    assert_eq!(
        run.read::<Value>(storage, "corrections", date, None, privacy.as_str())
            .unwrap_err(),
        "RECAP_SOURCE_CHANGED"
    );
    mirror(
        storage,
        date,
        "corrections",
        date,
        -1,
        privacy.as_str(),
        &[],
        &json!({"current":{"names":{"task-1":"Latest name"}}}),
    )
    .unwrap();
    restart_stage(dir, storage, broker);
    let run = recap::protected::prepare_with_filter(storage, date, &filter)
        .unwrap()
        .unwrap();
    run.check(storage).unwrap();
    assert_eq!(
        run.read::<Value>(storage, "corrections", date, None, privacy.as_str())
            .unwrap()
            .unwrap()["current"]["names"]["task-1"],
        "Latest name"
    );
    Connection::open(dir.join(STAGING_FILE))
        .unwrap()
        .execute(
            "DELETE FROM recap_objects WHERE slot LIKE 'day:2026-01-01:payload:corrections:%'",
            [],
        )
        .unwrap();
    assert_eq!(
        run.read::<Value>(storage, "corrections", date, None, privacy.as_str())
            .unwrap_err(),
        "RECAP_SOURCE_CHANGED"
    );
    let mut stale_config = run.clone();
    stale_config.config.ai_hash = "obsolete-model-settings".into();
    assert_eq!(
        stale_config.check(storage).unwrap_err(),
        "RECAP_SOURCE_CHANGED"
    );
}
