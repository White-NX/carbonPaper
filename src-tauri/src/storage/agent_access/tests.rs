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
    }));
    let service = authority.clone();
    *storage.agent_writer.authority.lock().unwrap() = Some(Arc::new(move |request| {
        let mut state = service.lock().unwrap();
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
            Request::AgentFence { generation } => {
                assert_eq!(
                    Some(generation.as_str()),
                    state.binding.as_ref().map(|b| b.generation.as_str())
                );
                state.source += 1;
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
    conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted,confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4) VALUES(43,42,'synthetic-ocr',?1,?2,1,0,0,1,0,1,1,0,1)",params![ocr,ocr_key]).unwrap();
    42
}
fn view(storage: &StorageState) -> AgentRepository {
    let binding = storage.agent_binding().unwrap();
    AgentRepository::open(Path::new(&binding.view_path), &[7; 32], &binding, true).unwrap()
}
#[test]
fn history_backfill_never_opens_image_files_and_future_locked_capture_keeps_full_text() {
    let (_directory, storage, authority) = fixture();
    let historical = seed_history(&storage);
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
    assert_eq!(
        view(&storage).ocr(historical).unwrap()[0].text,
        "historical OCR"
    );
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
