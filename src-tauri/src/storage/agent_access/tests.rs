use super::*;
use carbonpaper_app_bound::protocol::{AgentAccessStatus, TaskKey};
struct OfflineProcessing;
impl crate::processing_stage::Broker for OfflineProcessing {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(false)
    }
    fn supported(&self) -> bool {
        false
    }
    fn call(&self, _: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        Err(carbonpaper_app_bound::protocol::BrokerError::Unavailable)
    }
}
struct Authority {
    binding: Option<AgentViewBinding>,
    source: i64,
    applied: i64,
    phase: String,
    fence_calls: usize,
}
fn fixture() -> (tempfile::TempDir, StorageState, Arc<Mutex<Authority>>) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(directory.path());
    let mut storage = StorageState::new(directory.path().into(), credentials);
    storage.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(OfflineProcessing)),
    );
    storage.initialize().unwrap();
    let authority = Arc::new(Mutex::new(Authority {
        binding: None,
        source: 0,
        applied: 0,
        phase: "disabled".into(),
        fence_calls: 0,
    }));
    let service = authority.clone();
    *storage.agent_writer.authority.lock().unwrap() = Some(Arc::new(move |request| {
        let mut state = service.lock().unwrap();
        let reserved = match &request {
            Request::AgentFenceBatch { count, .. } => i64::from(*count),
            _ => 1,
        };
        match request {
            Request::AgentPrepare {
                dataset_id,
                view_path,
            } => {
                let binding = AgentViewBinding {
                    dataset_id,
                    generation: "b".repeat(64),
                    view_path,
                };
                state.binding = Some(binding.clone());
                state.source += 1;
                state.phase = "preparing".into();
                Ok(Response::AgentKeys(AgentViewKeys {
                    binding,
                    content_key: TaskKey(vec![7; 32]),
                    outbox_key: Some(TaskKey(vec![8; 32])),
                }))
            }
            Request::AgentStatus {} => Ok(Response::AgentStatus(AgentAccessStatus {
                enabled: state.binding.is_some(),
                binding: state.binding.clone(),
                phase: state.phase.clone(),
                source_revision: state.source,
                applied_revision: state.applied,
                grants: vec![],
            })),
            Request::AgentFence { generation } | Request::AgentFenceBatch { generation, .. } => {
                assert_eq!(
                    Some(generation.as_str()),
                    state.binding.as_ref().map(|b| b.generation.as_str())
                );
                state.source += reserved;
                state.fence_calls += 1;
                state.phase = "syncing".into();
                Ok(Response::AgentRevision(state.source))
            }
            Request::AgentPublish {
                revision, complete, ..
            } => {
                assert_eq!(revision, state.source);
                state.applied = revision;
                state.phase = if complete { "ready" } else { "gap" }.into();
                Ok(Response::Ok)
            }
            Request::AgentWriterKeys { .. } => Ok(Response::AgentKeys(AgentViewKeys {
                binding: state.binding.clone().unwrap(),
                content_key: TaskKey(vec![7; 32]),
                outbox_key: Some(TaskKey(vec![8; 32])),
            })),
            Request::AgentDisable {} => {
                state.binding = None;
                state.phase = "disabled".into();
                Ok(Response::Ok)
            }
            _ => Err("Unexpected Agent test request".into()),
        }
    }));
    (directory, storage, authority)
}
fn seed_history(storage: &StorageState) -> i64 {
    let row_key = [9; 32];
    let wrapping = storage.wrap_row_key_for_storage(&row_key).unwrap();
    let title = encrypt_with_master_key(&row_key, b"historical window").unwrap();
    let (ocr, ocr_key) = storage
        .encrypt_payload_with_row_key(b"historical OCR")
        .unwrap();
    let guard = storage.get_connection_named("agent_test_seed").unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute("INSERT INTO screenshots(id,image_path,image_hash,window_title_enc,content_key_encrypted,created_at,status) VALUES(42,'screenshots/file-must-not-be-opened.png.enc','history',?1,?2,'2026-11-08 00:00:00','committed')",params![title,wrapping]).unwrap();
    conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted,confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4) VALUES(43,42,'',?1,?2,1,0,0,1,0,1,1,0,1)",params![ocr,ocr_key]).unwrap();
    42
}
fn view(storage: &StorageState) -> AgentRepository {
    let binding = storage.agent_binding().unwrap();
    AgentRepository::open(Path::new(&binding.view_path), &[7; 32], &binding, true).unwrap()
}
fn outbox_events(storage: &StorageState) -> Vec<(i64, ExportEvent)> {
    let binding = storage.agent_binding().unwrap();
    let guard = storage.get_connection_named("agent_test_outbox").unwrap();
    let mut statement = guard
        .as_ref()
        .unwrap()
        .prepare("SELECT id,envelope FROM agent_outbox ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            let bytes: Vec<u8> = row.get(1)?;
            Ok((row.get(0)?, open_event(&[8; 32], &binding, &bytes).unwrap()))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn source_transactions_batch_unique_revisions_and_discard_unused_ranges_on_both_outcomes() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let before = authority.lock().unwrap().fence_calls;
    {
        let guard = storage.get_connection_named("agent_test_batch").unwrap();
        let tx = guard.as_ref().unwrap().unchecked_transaction().unwrap();
        for index in 0..600 {
            tx.execute(
                "UPDATE screenshots SET category=?1 WHERE id=42",
                [format!("category-{index}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let events = outbox_events(&storage);
    assert_eq!(events.len(), 600);
    assert!(events.windows(2).all(|v| v[0].1.revision < v[1].1.revision));
    assert_eq!(authority.lock().unwrap().fence_calls - before, 3);
    storage.sync_agent_access().unwrap();
    assert_eq!(
        view(&storage)
            .snapshot(42)
            .unwrap()
            .unwrap()
            .category
            .as_deref(),
        Some("category-599")
    );
    {
        let guard = storage
            .get_connection_named("agent_test_scalar_emission")
            .unwrap();
        let conn = guard.as_ref().unwrap();
        for behavior in [
            None,
            Some(rusqlite::TransactionBehavior::Deferred),
            Some(rusqlite::TransactionBehavior::Immediate),
        ] {
            let tx = behavior.map(|v| rusqlite::Transaction::new_unchecked(conn, v).unwrap());
            let _: String = conn.query_row(
                "SELECT cp_agent_emit('source','revision','put','{}') FROM screenshots WHERE id=42",
                [], |r| r.get(0),
            ).unwrap();
            if let Some(tx) = tx {
                tx.commit().unwrap();
            }
        }
    }
    storage.sync_agent_access().unwrap();
    let published = authority.lock().unwrap().applied;
    {
        let guard = storage
            .get_connection_named("agent_test_new_transaction")
            .unwrap();
        let tx = guard.as_ref().unwrap().unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE screenshots SET category='after commit' WHERE id=42",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    assert!(outbox_events(&storage)[0].1.revision > published);
    storage.sync_agent_access().unwrap();
    {
        let guard = storage
            .get_connection_named("agent_test_rolled_back_range")
            .unwrap();
        let tx = guard.as_ref().unwrap().unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE screenshots SET category='rolled back' WHERE id=42",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();
    }
    assert!(outbox_events(&storage).is_empty());
    storage.sync_agent_access().unwrap();
    let published = authority.lock().unwrap().applied;
    {
        let guard = storage
            .get_connection_named("agent_test_after_rollback")
            .unwrap();
        let tx = guard.as_ref().unwrap().unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE screenshots SET category='after rollback' WHERE id=42",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    assert!(outbox_events(&storage)[0].1.revision > published);
    storage.sync_agent_access().unwrap();
    assert_eq!(
        view(&storage)
            .snapshot(42)
            .unwrap()
            .unwrap()
            .category
            .as_deref(),
        Some("after rollback")
    );
}

#[test]
fn unsupported_batch_fencing_uses_single_events_but_service_denial_aborts_source_writes() {
    for unsupported in [true, false] {
        let (_directory, storage, authority) = fixture();
        seed_history(&storage);
        storage.begin_agent_access().unwrap();
        storage.sync_agent_access().unwrap();
        let original = storage
            .agent_writer
            .authority
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        *storage.agent_writer.authority.lock().unwrap() = Some(Arc::new(move |request| {
            if matches!(request, Request::AgentFenceBatch { .. }) {
                let error = if unsupported {
                    BrokerError::InvalidRequest
                } else {
                    BrokerError::AccessDenied
                };
                return Err(format!("AGENT_SERVICE_UNAVAILABLE: {error}"));
            }
            original(request)
        }));
        let result = {
            let guard = storage
                .get_connection_named("agent_test_fence_compatibility")
                .unwrap();
            let tx = guard.as_ref().unwrap().unchecked_transaction().unwrap();
            let result = tx.execute("UPDATE screenshots SET category='updated' WHERE id=42", []);
            if result.is_ok() {
                tx.commit().unwrap();
            } else {
                tx.rollback().unwrap();
            }
            result
        };
        if unsupported {
            assert!(result.is_ok());
            assert_eq!(outbox_events(&storage).len(), 1);
            storage.sync_agent_access().unwrap();
            assert_eq!(
                view(&storage)
                    .snapshot(42)
                    .unwrap()
                    .unwrap()
                    .category
                    .as_deref(),
                Some("updated")
            );
        } else {
            assert!(result.is_err());
            assert!(outbox_events(&storage).is_empty());
            assert_eq!(authority.lock().unwrap().phase, "ready");
            let guard = storage
                .get_connection_named("agent_test_denied_write")
                .unwrap();
            let category: Option<String> = guard
                .as_ref()
                .unwrap()
                .query_row("SELECT category FROM screenshots WHERE id=42", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(category.is_none());
        }
    }
}

#[test]
fn opportunistic_outbox_sync_skips_busy_writers_and_durably_applied_events() {
    let (_directory, storage, _authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let binding = storage.agent_binding().unwrap();
    let mut repo =
        AgentRepository::open(Path::new(&binding.view_path), &[7; 32], &binding, false).unwrap();
    {
        let guard = storage
            .get_connection_named("agent_test_busy_writer")
            .unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET category='pending' WHERE id=42", [])
            .unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    storage
                        .apply_agent_outbox(&binding, &mut repo, true)
                        .unwrap()
                })
                .join()
                .unwrap();
        });
    }
    let events = outbox_events(&storage);
    assert_eq!(events.len(), 1);
    repo.apply_outbox_batch(&[events[0].1.clone()], events[0].0)
        .unwrap();
    {
        let guard = storage
            .get_connection_named("agent_test_deferred_ack")
            .unwrap();
        // An applied envelope need not be decrypted a second time when its
        // acknowledgement was interrupted after the durable Agent commit.
        guard
            .as_ref()
            .unwrap()
            .execute(
                "UPDATE agent_outbox SET envelope=X'00' WHERE id=?1",
                [events[0].0],
            )
            .unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET category='newer' WHERE id=42", [])
            .unwrap();
    }
    storage
        .apply_agent_outbox(&binding, &mut repo, true)
        .unwrap();
    assert!(outbox_events(&storage).is_empty());
    assert_eq!(
        repo.snapshot(42).unwrap().unwrap().category.as_deref(),
        Some("newer")
    );
}
#[test]
fn database_maintenance_fences_ready_access_and_reconciles_after_locked_reopen() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let revision = authority.lock().unwrap().source;
    storage.credential_state.invalidate_session();
    storage.credential_state.clear_all_cached_keys();

    let maintenance = storage.database_maintenance("agent_test_backup_export");
    assert_eq!(storage.agent_preparation().phase, "syncing");
    {
        let status = authority.lock().unwrap();
        assert_eq!(status.phase, "syncing");
        assert_eq!(status.source, revision + 1);
        assert_eq!(status.applied, revision);
    }
    assert!(storage
        .try_foreground_db_read("agent_test_reader_start")
        .is_none());
    storage.shutdown_under_maintenance().unwrap();
    storage.initialize_under_maintenance().unwrap();
    assert_eq!(storage.agent_preparation().phase, "syncing");
    drop(maintenance);

    storage.sync_agent_access().unwrap();
    assert_eq!(storage.agent_preparation().phase, "ready");
    assert_eq!(authority.lock().unwrap().phase, "ready");
    assert!(!storage.is_session_valid());
    assert_eq!(view(&storage).ocr(42).unwrap()[0].text, "historical OCR");
}

#[test]
fn idle_vacuum_probe_keeps_unattended_access_ready() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let revision = authority.lock().unwrap().source;
    storage.credential_state.invalidate_session();

    storage
        .run_incremental_vacuum_if_idle(i64::MAX, 500)
        .unwrap();
    assert_eq!(storage.agent_preparation().phase, "ready");
    assert_eq!(authority.lock().unwrap().phase, "ready");
    assert_eq!(authority.lock().unwrap().source, revision);
}

#[test]
fn unsuccessful_try_maintenance_keeps_agent_revision_and_readiness() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let revision = authority.lock().unwrap().source;

    for reader in [
        storage.foreground_db_activity.read("agent_test_foreground"),
        storage
            .independent_db_activity
            .read("agent_test_independent"),
    ] {
        assert!(storage
            .try_database_maintenance("agent_test_busy")
            .is_none());
        assert!(!storage.is_database_maintenance_pending());
        assert_eq!(storage.agent_preparation().phase, "ready");
        assert_eq!(authority.lock().unwrap().source, revision);
        drop(reader);
    }
    let maintenance = storage.try_database_maintenance("agent_test_idle").unwrap();
    assert_eq!(storage.agent_preparation().phase, "syncing");
    assert_eq!(authority.lock().unwrap().source, revision + 1);
    drop(maintenance);
    storage.sync_agent_access().unwrap();
    assert_eq!(authority.lock().unwrap().phase, "ready");
}

#[test]
fn failed_history_page_does_not_publish_a_prefix_or_advance_past_unprepared_ocr() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    {
        let guard = storage
            .get_connection_named("agent_test_bad_history_key")
            .unwrap();
        guard.as_ref().unwrap().execute(
            "INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted,confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
             SELECT 44,screenshot_id,'',text_enc,X'00',confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4 FROM ocr_results WHERE id=43",
            [],
        ).unwrap();
    }
    storage.begin_agent_access().unwrap();
    assert!(storage.sync_agent_access().is_err());
    let projected = view(&storage);
    assert!(projected.ocr_payload_digest(43).is_err());
    assert!(projected.ocr(42).is_err());
    drop(projected);
    assert_ne!(authority.lock().unwrap().phase, "ready");
    let checkpoint: Checkpoint =
        serde_json::from_str(&view(&storage).preparation_checkpoint().unwrap().unwrap()).unwrap();
    assert_eq!(checkpoint.phase, 1);
    assert!(checkpoint.cursor.is_empty());

    {
        let guard = storage
            .get_connection_named("agent_test_repair_history_key")
            .unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute(
                "UPDATE agent_access_state SET checkpoint='superseded legacy cursor'",
                [],
            )
            .unwrap();
        guard.as_ref().unwrap().execute(
            "UPDATE ocr_results SET text_key_encrypted=(SELECT text_key_encrypted FROM ocr_results WHERE id=43) WHERE id=44",
            [],
        ).unwrap();
    }
    storage.sync_agent_access().unwrap();
    let ids = view(&storage)
        .ocr(42)
        .unwrap()
        .into_iter()
        .map(|row| row.ocr_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![43, 44]);
    assert_eq!(authority.lock().unwrap().phase, "ready");
}

#[test]
fn older_preparations_migrate_the_resume_cursor_without_rewriting_source_state() {
    let (_directory, storage, _authority) = fixture();
    seed_history(&storage);
    let binding = storage.begin_agent_access().unwrap();
    {
        let conn = Connection::open(&binding.view_path).unwrap();
        conn.execute_batch(&format!("PRAGMA key=\"x'{}'\"", hex::encode([7; 32])))
            .unwrap();
        conn.execute(
            "DELETE FROM agent_metadata WHERE key='preparation_checkpoint'",
            [],
        )
        .unwrap();
    }
    storage.sync_agent_access().unwrap();
    let checkpoint: Checkpoint =
        serde_json::from_str(&view(&storage).preparation_checkpoint().unwrap().unwrap()).unwrap();
    assert!(checkpoint.complete);
    let guard = storage
        .get_connection_named("agent_test_legacy_state")
        .unwrap();
    let saved: String = guard
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT checkpoint FROM agent_access_state WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let legacy: Checkpoint = serde_json::from_str(&saved).unwrap();
    assert!(!legacy.complete);
    assert_eq!(view(&storage).ocr(42).unwrap()[0].text, "historical OCR");
}

#[test]
fn history_backfill_never_opens_image_files_and_future_locked_capture_keeps_full_text() {
    let (_directory, storage, authority) = fixture();
    let historical = seed_history(&storage);
    let bounds = vec![
        vec![10.0, 20.0],
        vec![50.0, 20.0],
        vec![50.0, 40.0],
        vec![10.0, 40.0],
    ];
    storage
        .commit_screenshot(
            historical,
            Some(&vec![super::super::OcrResultInput {
                text: "historical block".into(),
                text_layout: None,
                confidence: 0.75,
                box_coords: bounds.clone(),
            }]),
            None,
            None,
        )
        .unwrap();
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    assert_eq!(authority.lock().unwrap().phase, "ready");
    assert_eq!(
        view(&storage)
            .snapshot(historical)
            .unwrap()
            .unwrap()
            .window_title
            .as_deref(),
        Some("historical window")
    );
    let history = view(&storage).ocr(historical).unwrap();
    assert_eq!(history[0].text, "historical OCR");
    assert_eq!(history[1].text, "historical block");
    assert_eq!(history[1].box_coords, bounds);
    assert_eq!(history[1].confidence, 0.75);
    storage.credential_state.invalidate_session();
    let request = super::super::SaveScreenshotRequest {
        image_data: base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"synthetic captured pixels",
        ),
        image_hash: "future".into(),
        width: 1,
        height: 1,
        window_title: Some("future window".into()),
        process_name: Some("editor.exe".into()),
        metadata: Some(
            json!({"image_path":"must not be exported","private_key":"must not be exported"}),
        ),
        ocr_results: Some(vec![super::super::OcrResultInput {
            text: "future OCR".into(),
            text_layout: None,
            confidence: 1.0,
            box_coords: vec![
                vec![0.0, 0.0],
                vec![1.0, 0.0],
                vec![1.0, 1.0],
                vec![0.0, 1.0],
            ],
        }]),
        source: None,
        page_url: None,
        page_icon: None,
        visible_links: None,
    };
    let future = storage
        .save_screenshot(&request)
        .unwrap()
        .screenshot_id
        .unwrap();
    assert_eq!(authority.lock().unwrap().phase, "syncing");
    storage.sync_agent_access().unwrap();
    assert_eq!(authority.lock().unwrap().phase, "ready");
    assert!(!storage.is_session_valid());
    let text = view(&storage);
    assert_eq!(
        text.snapshot(future)
            .unwrap()
            .unwrap()
            .window_title
            .as_deref(),
        Some("future window")
    );
    assert_eq!(text.ocr(future).unwrap()[0].text, "future OCR");
    {
        let guard = storage
            .get_connection_named("agent_test_compact_ocr")
            .unwrap();
        let source = guard.as_ref().unwrap();
        let (slot, indexed): (i64, String) = source
            .query_row(
                "SELECT ocr_block_slot, text_hash FROM ocr_results WHERE screenshot_id=?1",
                [future],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(slot, 0);
        assert_eq!(indexed, super::super::compaction::OCR_INDEXED);
    }
    let result = carbonpaper_agent_access::dispatcher::call(
        &text,
        &carbonpaper_agent_access::sensitive_filter::SensitiveFilterState::default(),
        "search_ocr_text",
        json!({"query":"future OCR","fuzzy":false}),
        None,
    )
    .unwrap();
    assert_eq!(result["items"][0]["screenshot_id"], future);
    let data = serde_json::to_value(text.snapshot(future).unwrap().unwrap()).unwrap();
    assert!(data.get("metadata").is_none());
    assert!(data.get("image_path").is_none());
}
#[test]
fn missing_write_material_records_a_transactional_gap_and_deletion_fences_cached_text() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    storage.credential_state.invalidate_session();
    *storage.agent_writer.keys.lock().unwrap() = None;
    {
        let guard = storage.get_connection_named("agent_test_gap").unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute("UPDATE screenshots SET category='work' WHERE id=42", [])
            .unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_projection_gaps", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    storage.sync_agent_access().unwrap();
    assert_eq!(authority.lock().unwrap().phase, "gap");
    assert!(view(&storage).has_gaps().unwrap());
    storage.credential_state.update_auth_time();
    storage.sync_agent_access().unwrap();
    assert!(!view(&storage).has_gaps().unwrap());
    assert_eq!(
        view(&storage)
            .snapshot(42)
            .unwrap()
            .unwrap()
            .category
            .as_deref(),
        Some("work")
    );
    storage.credential_state.invalidate_session();
    {
        let guard = storage.get_connection_named("agent_test_delete").unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute("UPDATE screenshots SET is_deleted=1 WHERE id=42", [])
            .unwrap();
    }
    storage.sync_agent_access().unwrap();
    assert!(view(&storage).snapshot(42).unwrap().is_none());
    assert!(view(&storage).ocr(42).unwrap().is_empty());
    assert_eq!(authority.lock().unwrap().phase, "ready");
}

#[test]
fn locked_restart_restores_scoped_reads_without_an_archive_master_key() {
    let (directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    storage.credential_state.invalidate_session();
    storage.credential_state.clear_all_cached_keys();
    assert!(storage.credential_state.get_hmac_key().is_err());
    let mut restarted =
        StorageState::new(directory.path().into(), storage.credential_state.clone());
    restarted.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(OfflineProcessing)),
    );
    *restarted.agent_writer.authority.lock().unwrap() =
        storage.agent_writer.authority.lock().unwrap().clone();
    {
        let mut state = authority.lock().unwrap();
        state.source += 1;
        state.phase = "syncing".into();
    }
    restarted.initialize().unwrap();
    restarted.sync_agent_access().unwrap();
    assert!(!restarted.is_session_valid());
    assert!(restarted.credential_state.get_hmac_key().is_err());
    assert_eq!(view(&restarted).ocr(42).unwrap()[0].text, "historical OCR");
    assert_eq!(authority.lock().unwrap().phase, "ready");
}
#[test]
fn rollback_never_commits_an_outbox_event_and_preparation_resumes_from_its_checkpoint() {
    let (_directory, storage, authority) = fixture();
    seed_history(&storage);
    storage.begin_agent_access().unwrap();
    {
        let guard = storage.get_connection_named("agent_test_rollback").unwrap();
        let conn = guard.as_ref().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            "UPDATE screenshots SET category='rolled back' WHERE id=42",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM agent_outbox", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    storage.credential_state.invalidate_session();
    assert_eq!(storage.sync_agent_access().unwrap_err(), "AUTH_REQUIRED");
    assert_ne!(authority.lock().unwrap().phase, "ready");
    storage.credential_state.update_auth_time();
    storage.sync_agent_access().unwrap();
    assert_eq!(authority.lock().unwrap().phase, "ready");
    assert!(view(&storage)
        .snapshot(42)
        .unwrap()
        .unwrap()
        .category
        .is_none());
}
#[test]
fn backfill_unwraps_summary_text_and_keeps_independent_corrections_without_diagnostics() {
    let (_directory, storage, _authority) = fixture();
    seed_history(&storage);
    let summary = json!({"summary":{"overview":"saved historical overview"},"error":"private provider diagnostics","attempts":[{"reasoning":"private reasoning"}]});
    let corrections = json!({"current":{"names":{"corrected":"reviewed task"},"assignments":{"42":"corrected"}},"previous":null});
    let mut payloads = Vec::new();
    for (kind, revision, value) in [("summary", 7, summary), ("corrections", -1, corrections)] {
        let (data, key) = storage
            .encrypt_payload_with_row_key(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        payloads.push((kind, revision, data, key));
    }
    {
        let guard = storage.get_connection_named("agent_test_recaps").unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute("INSERT INTO recap_days(day,start_ms,end_ms,revision,privacy_hash) VALUES('2026-11-08',0,100000,7,'fixture')",[]).unwrap();
        for (kind, revision, data, key) in payloads {
            conn.execute("INSERT INTO recap_payloads(kind,key,day,revision,data,row_key) VALUES(?1,'2026-11-08:0','2026-11-08',?2,?3,?4)",params![kind,revision,data,key]).unwrap();
        }
    }
    storage.begin_agent_access().unwrap();
    storage.sync_agent_access().unwrap();
    let repo = view(&storage);
    let summary: RecapText = repo.get("recap", "summary:2026-11-08:0").unwrap().unwrap();
    assert_eq!(summary.content.overview, "saved historical overview");
    let corrections: RecapText = repo
        .get("recap", "corrections:2026-11-08:0")
        .unwrap()
        .unwrap();
    assert_eq!(
        corrections.content.assignments.get(&42).map(String::as_str),
        Some("corrected")
    );
    let serialized = serde_json::to_string(&summary).unwrap();
    assert!(!serialized.contains("diagnostics"));
    assert!(!serialized.contains("reasoning"));
}

fn seed_pagination_history(conn: &Connection, count: i64) {
    conn.execute(
        "WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id<?1)
         INSERT INTO screenshots(id,image_path,image_hash,created_at,status)
         SELECT id,'synthetic.enc','image-'||id,'2026-11-08 00:00:00','committed' FROM ids",
        [count],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO ocr_results(id,screenshot_id,text_hash,text,confidence,
            box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
         SELECT id,id,'text-'||id,'synthetic text',1,0,0,1,0,1,1,0,1 FROM screenshots",
    )
    .unwrap();
}

fn source_query_keys(
    conn: &Connection,
    kind: &str,
    cursor: &str,
    key: Option<&str>,
) -> (Vec<String>, i32) {
    let source = SOURCES.iter().find(|source| source.kind == kind).unwrap();
    let (sql, parameters) = source.query(cursor, key).unwrap();
    let mut query = conn.prepare(&sql).unwrap();
    let rows: Vec<(String, String)> = query
        .query_map(rusqlite::params_from_iter(parameters), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let steps = query.get_status(rusqlite::StatementStatus::VmStep);
    let keys = rows
        .into_iter()
        .map(|(key, payload)| {
            serde_json::from_str::<Value>(&payload).unwrap();
            key
        })
        .collect();
    (keys, steps)
}

#[test]
fn source_pages_seek_native_ids_and_project_only_the_selected_history() {
    use std::sync::atomic::AtomicUsize;
    let (_directory, storage, _authority) = fixture();
    let guard = storage
        .get_connection_named("agent_test_pagination")
        .unwrap();
    let conn = guard.as_ref().unwrap();
    seed_pagination_history(conn, 30_000);
    let projected = Arc::new(AtomicUsize::new(0));
    let calls = projected.clone();
    conn.create_scalar_function(
        "cp_process",
        1,
        FunctionFlags::SQLITE_DIRECTONLY,
        move |args| {
            calls.fetch_add(1, Ordering::Relaxed);
            args.get::<Option<String>>(0)
        },
    )
    .unwrap();
    for kind in ["snapshot", "ocr"] {
        let (keys, steps) = source_query_keys(conn, kind, "29870", None);
        assert_eq!(
            keys,
            (29_871..=29_998)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
        );
        // A late 128-row page must not scan/sort the 30,000-row history.
        assert!(steps < 50_000, "{kind} page executed {steps} VM steps");
        let (keys, steps) = source_query_keys(conn, kind, "30000", Some("9"));
        assert_eq!(keys, ["9"]);
        assert!(steps < 1_000, "{kind} lookup executed {steps} VM steps");
        let (keys, _) = source_query_keys(conn, kind, "8", None);
        assert_eq!(&keys[..3], ["9", "10", "11"]);
    }
    assert_eq!(projected.load(Ordering::Relaxed), 128 + 1 + 128);

    conn.execute_batch(
        "INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,
            model_id,model_revision,embedding_version,source_fingerprint)
         SELECT 'clip_image',image_hash,1,x'0000803f','synthetic','fixture',1,'fixture' FROM screenshots;",
    ).unwrap();
    conn.create_scalar_function("cp_unseal", 3, FunctionFlags::SQLITE_DIRECTONLY, |args| {
        args.get::<Vec<u8>>(0)
    })
    .unwrap();
    let expected: Vec<String> = conn
        .prepare(
            "SELECT key_id FROM protected_storage_keys WHERE purpose='vectors' ORDER BY key_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let (keys, steps) = source_query_keys(conn, "vector_key", "", None);
    assert_eq!(keys, expected);
    assert!(
        steps < 2_000,
        "vector key preparation executed {steps} VM steps for 30,000 vectors"
    );
}

#[test]
fn source_pages_finish_while_the_primary_database_mutex_is_held() {
    use std::{sync::mpsc, thread, time::Duration};
    let (_directory, storage, _authority) = fixture();
    seed_history(&storage);
    let storage = Arc::new(storage);
    let primary = storage
        .get_connection_named("agent_test_held_writer")
        .unwrap();
    let reader_storage = storage.clone();
    let (finished_tx, finished_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let result = reader_storage.agent_source_page(&SOURCES[0], "", None);
        finished_tx.send(result).unwrap();
    });
    let finished = finished_rx.recv_timeout(Duration::from_secs(5));
    // Release the mutex before reporting a regression, so the reader can exit.
    drop(primary);
    reader.join().unwrap();
    let rows = finished
        .expect("Agent history reads must not wait for the primary mutex")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "42");
}

#[test]
fn composite_source_pages_keep_relationships_vector_keys_and_recap_visibility() {
    use std::sync::atomic::AtomicUsize;
    let (_directory, storage, _authority) = fixture();
    let guard = storage
        .get_connection_named("agent_test_composite_pages")
        .unwrap();
    let conn = guard.as_ref().unwrap();
    seed_pagination_history(conn, 350);
    conn.execute_batch(
        "INSERT INTO smart_clusters(id,anchor_text) VALUES(2,'two'),(10,'ten');
         INSERT INTO smart_cluster_summaries(smart_cluster_id) VALUES(2),(10);
         INSERT INTO smart_cluster_assignments(smart_cluster_id,screenshot_id)
            SELECT c.id,s.id FROM smart_clusters c CROSS JOIN screenshots s;
         INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,
            model_id,model_revision,embedding_version,source_fingerprint)
            SELECT 'clip_image',image_hash,1,x'0000803f','synthetic','fixture',1,'fixture' FROM screenshots;
         INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,
            model_id,model_revision,embedding_version,source_fingerprint)
            SELECT 'semantic_text',CAST(id AS TEXT),1,x'0000803f','synthetic','fixture',1,'fixture' FROM screenshots;
         UPDATE screenshots SET is_deleted=1 WHERE id=9;
         UPDATE ocr_results SET is_deleted=1 WHERE id=8;
         INSERT INTO recap_days(day,start_ms,end_ms,revision,privacy_hash)
            VALUES('2026-11-07',0,1,7,'fixture'),('2026-11-08',1,2,7,'fixture');
         INSERT INTO recap_deleted_days(day) VALUES('2026-11-07');
         INSERT INTO recap_payloads(kind,key,day,revision,data,row_key) VALUES
            ('batch','2026-11-08:0','2026-11-08',7,x'01',x'02'),
            ('batch','2026-11-08:old','2026-11-08',6,x'01',x'02'),
            ('batch','2026-11-07:0','2026-11-07',7,x'01',x'02'),
            ('corrections','2026-11-08:0','2026-11-08',-1,x'01',x'02'),
            ('summary','2026-11-08:0','2026-11-08',7,x'01',x'02'),
            ('prompt','2026-11-08:0','2026-11-08',7,x'01',x'02');",
    ).unwrap();
    // Test only key selection/projection here; the encrypted plaintext contract
    // is covered by the authenticated backfill tests above.
    conn.create_scalar_function("cp_text", 2, FunctionFlags::SQLITE_DIRECTONLY, |args| {
        args.get::<Option<String>>(0)
    })
    .unwrap();
    let projected = Arc::new(AtomicUsize::new(0));
    let calls = projected.clone();
    conn.create_scalar_function(
        "cp_unseal",
        3,
        FunctionFlags::SQLITE_DIRECTONLY,
        move |args| {
            calls.fetch_add(1, Ordering::Relaxed);
            args.get::<Vec<u8>>(0)
        },
    )
    .unwrap();
    let live_ids: Vec<i64> = (1..=350).filter(|id| *id != 9).collect();
    let expectations = [
        (
            "snapshot",
            live_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ),
        (
            "ocr",
            live_ids
                .iter()
                .filter(|id| **id != 8)
                .map(ToString::to_string)
                .collect(),
        ),
        ("cluster", vec!["2".into(), "10".into()]),
        ("summary", vec!["2".into(), "10".into()]),
        (
            "assignment",
            [2, 10]
                .into_iter()
                .flat_map(|cluster| live_ids.iter().map(move |id| format!("{cluster}:{id}")))
                .collect(),
        ),
        ("calendar", vec!["2026-11-08".into()]),
        (
            "recap",
            vec![
                "batch:2026-11-08:0".into(),
                "corrections:2026-11-08:0".into(),
                "summary:2026-11-08:0".into(),
            ],
        ),
        (
            "vector_key",
            conn.prepare(
                "SELECT key_id FROM protected_storage_keys WHERE purpose='vectors' ORDER BY key_id",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap(),
        ),
    ];
    for (kind, expected) in expectations {
        let mut cursor = String::new();
        let mut actual = Vec::new();
        loop {
            let (keys, _) = source_query_keys(conn, kind, &cursor, None);
            if keys.is_empty() {
                break;
            }
            cursor = keys.last().unwrap().clone();
            actual.extend(keys);
            assert!(
                actual.len() <= expected.len(),
                "{kind} must advance without repeating keys"
            );
        }
        assert_eq!(actual, expected, "{kind}");
        let key = expected.last().unwrap();
        assert_eq!(
            source_query_keys(conn, kind, "", Some(key)).0,
            [key.clone()]
        );
    }
    assert_eq!(projected.load(Ordering::Relaxed), 0);
}
