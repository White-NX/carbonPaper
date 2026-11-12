//! Native storage regressions use temporary datasets and unnamed CNG keys.
use super::*;
use crate::storage::smart_cluster::SmartClusterSummaryUpsert;
use crate::storage::{
    DerivedEmbeddingWrite, DerivedIndexJobSpec, DerivedIndexKind, OcrResultInput,
    SaveScreenshotRequest,
};
use base64::Engine;
use carbonpaper_app_bound::payload::is_sealed;

struct OfflineBroker;

struct GatedPublicationBroker {
    release: (Mutex<bool>, std::sync::Condvar),
    entered: std::sync::mpsc::Sender<()>,
    registrations: std::sync::atomic::AtomicUsize,
    status_calls: std::sync::atomic::AtomicUsize,
}

impl crate::processing_stage::Broker for GatedPublicationBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(true)
    }
    fn supported(&self) -> bool {
        true
    }
    fn call(&self, request: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        use carbonpaper_app_bound::protocol::{BrokerStatus, Limits};
        match request {
            Request::Status {} => {
                self.status_calls.fetch_add(1, Ordering::SeqCst);
                Ok(Response::Status(BrokerStatus {
                    enabled: true,
                    limits: Limits::default(),
                    dataset_id: None,
                    active_tasks: 0,
                    active_bytes: 0,
                    runtime_id: "test-runtime".into(),
                    supported_consumers: 0x1f,
                    supported_storage_keys: true,
                }))
            }
            Request::AttachDataset { .. } => Ok(Response::Ok),
            Request::RegisterStorageKey { .. } => {
                self.registrations.fetch_add(1, Ordering::SeqCst);
                let _ = self.entered.send(());
                let guard = self.release.0.lock().unwrap();
                let _ = self
                    .release
                    .1
                    .wait_timeout_while(guard, std::time::Duration::from_secs(5), |released| {
                        !*released
                    })
                    .unwrap();
                Err(BrokerError::Integrity)
            }
            _ => Ok(Response::Ok),
        }
    }
}

#[test]
fn blocked_or_rejected_background_publication_cannot_delay_readiness_or_authenticated_reads() {
    let directory = tempfile::tempdir().unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(directory.path());
    credentials.background_enabled_for_tests(true);
    let mut storage = StorageState::new(directory.path().into(), credentials);
    let (entered, events) = std::sync::mpsc::channel();
    let broker = Arc::new(GatedPublicationBroker {
        release: (Mutex::new(false), std::sync::Condvar::new()),
        entered,
        registrations: std::sync::atomic::AtomicUsize::new(0),
        status_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    storage.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(broker.clone()),
    );
    storage
        .protected
        .automatic_publication_for_tests
        .store(true, Ordering::Release);
    storage.initialize().unwrap();
    assert!(storage.initialized.load(Ordering::Acquire));
    events
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    // Initialization returned while the broker operation remains blocked.
    assert!(!*broker.release.0.lock().unwrap());
    let id = screenshot(&storage, "publication-outage", "Editor.exe");
    let expected = vector(&storage, id);
    assert_eq!(
        storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, &id.to_string())
            .unwrap()
            .unwrap()
            .vector,
        expected
    );
    for _ in 0..10 {
        storage.request_protected_key_publication();
    }
    assert_eq!(broker.registrations.load(Ordering::SeqCst), 1);
    *broker.release.0.lock().unwrap() = true;
    broker.release.1.notify_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while storage
        .protected
        .publication_running
        .load(Ordering::Acquire)
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(storage.initialized.load(Ordering::Acquire));
    assert!(!storage.processing_stage.available());
    assert_eq!(
        storage.processing_stage.refresh().unwrap_err(),
        BrokerError::Integrity.to_string()
    );
    assert_eq!(
        storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, &id.to_string())
            .unwrap()
            .unwrap()
            .vector,
        expected
    );
    assert_eq!(broker.status_calls.load(Ordering::SeqCst), 1);
    assert_eq!(broker.registrations.load(Ordering::SeqCst), 1);
}

#[test]
fn manual_compaction_preserves_primary_memory_projections() {
    let (_directory, storage) = fixture();
    {
        let guard = storage.db.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute_batch(
                "CREATE TEMP TABLE protected_projection(value TEXT);
             INSERT INTO protected_projection VALUES('decrypted-ephemeral');",
            )
            .unwrap();
    }
    storage.run_manual_vacuum().unwrap();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    assert_eq!(
        conn.query_row("SELECT value FROM protected_projection", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "decrypted-ephemeral"
    );
    assert_eq!(
        conn.pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn legacy_conversion_reports_committed_batches_before_compaction() {
    use crate::storage::{InitializationPhase, InitializationProgress};
    let (_directory, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute_batch(
        "DELETE FROM app_metadata WHERE key='protected_content_version';
        INSERT INTO screenshots(id,image_path,image_hash,process_name,created_at,status)
        VALUES(7,'fixture','legacy-hash','Legacy.exe','2026-10-15 12:00:00','committed');",
    )
    .unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        let bytes = 0.1234567f32.to_le_bytes();
        for id in 1..=513 {
            tx.execute("INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,model_id,model_revision,embedding_version,source_fingerprint)
                VALUES('semantic_text',?1,1,?2,'synthetic','1',1,'fixture')", params![id.to_string(), bytes.as_slice()]).unwrap();
        }
        tx.commit().unwrap();
    }
    let progress = std::cell::RefCell::new(Vec::<InitializationProgress>::new());
    storage
        .migrate_protected_content_with_progress(conn, &|next| progress.borrow_mut().push(next))
        .unwrap();
    let progress = progress.into_inner();
    let vectors = progress
        .iter()
        .filter(|next| next.phase == InitializationPhase::Vectors)
        .map(|next| (next.processed, next.total))
        .collect::<Vec<_>>();
    assert_eq!(vectors, vec![(0, 513), (512, 513), (513, 513)]);
    assert!(progress
        .iter()
        .any(|next| next.phase == InitializationPhase::Processes
            && next.processed == 1
            && next.total == 1));
    let compacting = progress
        .iter()
        .position(|next| next.phase == InitializationPhase::Compacting)
        .unwrap();
    assert_eq!(
        progress[compacting].total, 0,
        "compaction must not invent a percentage"
    );
    assert!(progress[..compacting]
        .iter()
        .any(|next| next.phase == InitializationPhase::Vectors && next.processed == 513));
    assert_eq!(
        progress.last().unwrap().phase,
        InitializationPhase::Finalizing
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM app_metadata WHERE key='protected_content_version'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "1"
    );
    assert_eq!(
        conn.pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}
impl crate::processing_stage::Broker for OfflineBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(false)
    }
    fn supported(&self) -> bool {
        false
    }
    fn call(&self, _request: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        Err(BrokerError::Unavailable)
    }
}

fn fixture() -> (tempfile::TempDir, StorageState) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(directory.path());
    let mut storage = StorageState::new(directory.path().into(), credentials);
    storage.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(OfflineBroker)),
    );
    storage.initialize().unwrap();
    (directory, storage)
}

#[test]
fn resetting_background_dataset_preserves_content_identity_and_encrypted_history() {
    let (directory, storage) = fixture();
    let credentials = storage.credential_state.clone();
    let first = screenshot(&storage, "before-reset", "Editor.exe");
    let expected = vector(&storage, first);
    let cluster = storage
        .create_smart_cluster("Stable protected history", 0.5, None, None)
        .unwrap();
    summary(&storage, cluster);
    let content_identity = storage.protected_ann_write_key().unwrap().dataset.clone();
    let old_scope = storage.processing_dataset_id().unwrap();
    credentials.invalidate_session();
    storage.reset_processing_dataset().unwrap();
    let new_scope = storage.processing_dataset_id().unwrap();
    assert_ne!(old_scope, new_scope);
    assert_eq!(
        storage.protected_ann_write_key().unwrap().dataset,
        content_identity
    );
    assert!(!credentials.protected_read_authorized());
    screenshot(&storage, "after-reset", "Editor.exe");
    credentials.update_auth_time();
    assert_eq!(
        storage.list_distinct_processes().unwrap(),
        vec![("Editor.exe".into(), 2)]
    );
    assert_eq!(
        storage.list_smart_clusters().unwrap()[0].anchor_text,
        "Stable protected history"
    );
    storage.shutdown().unwrap();
    let mut reopened = StorageState::new(directory.path().into(), credentials);
    reopened.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(OfflineBroker)),
    );
    reopened.initialize().unwrap();
    assert_eq!(reopened.processing_dataset_id().unwrap(), new_scope);
    assert_eq!(
        reopened.protected_ann_write_key().unwrap().dataset,
        content_identity
    );
    assert_eq!(
        reopened
            .get_smart_cluster_summary(cluster)
            .unwrap()
            .unwrap()
            .summary
            .as_deref(),
        Some("Private project summary")
    );
    let guard = reopened.db.lock().unwrap();
    let bytes: Vec<u8> = guard.as_ref().unwrap().query_row(
        "SELECT cp_unseal(vector_f32,'vectors',index_kind||':'||subject_key) FROM derived_embeddings WHERE subject_key=?1",
        [first.to_string()], |r| r.get(0),
    ).unwrap();
    let decoded = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(decoded, expected);
}

fn screenshot(storage: &StorageState, hash: &str, process: &str) -> i64 {
    storage
        .save_screenshot(&SaveScreenshotRequest {
            image_data: STANDARD.encode(b"synthetic screenshot bytes"),
            image_hash: hash.into(),
            width: 2,
            height: 2,
            window_title: Some("Private synthetic window".into()),
            process_name: Some(process.into()),
            metadata: None,
            ocr_results: Some(vec![OcrResultInput {
                text: format!("Synthetic captured text for {hash}"),
                confidence: 1.0,
                box_coords: vec![
                    vec![0.0, 0.0],
                    vec![2.0, 0.0],
                    vec![2.0, 2.0],
                    vec![0.0, 2.0],
                ],
            }]),
            source: None,
            page_url: None,
            page_icon: None,
            visible_links: None,
        })
        .unwrap()
        .screenshot_id
        .unwrap()
}

fn vector(storage: &StorageState, id: i64) -> Vec<f32> {
    let job = DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::SemanticText,
        subject_key: id.to_string(),
        model_id: "synthetic".into(),
        model_revision: "1".into(),
        embedding_version: 1,
        source_fingerprint: "synthetic-source".into(),
    };
    storage.upsert_derived_index_job(&job).unwrap();
    let lease_token = storage.mark_derived_index_job_processing(&job).unwrap();
    let vector = vec![0.1234567, 0.7654321, 0.2345678];
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job,
            lease_token,
            vector: vector.clone(),
        })
        .unwrap();
    vector
}

fn summary(storage: &StorageState, id: i64) {
    storage
        .upsert_smart_cluster_summary(&SmartClusterSummaryUpsert {
            smart_cluster_id: id,
            title: Some("Private project title".into()),
            summary: Some("Private project summary".into()),
            ocr_summary: Some("Private OCR summary".into()),
            key_points: Some(serde_json::json!(["Private key point"])),
            evidence: Some(serde_json::json!([{"text":"Private cited evidence"}])),
            source_snapshot_count: Some(1),
            source_hash: Some("Private source digest".into()),
            model_provider: Some("synthetic".into()),
            model_name: Some("fixture".into()),
            prompt_version: Some("1".into()),
        })
        .unwrap();
}

#[test]
fn public_database_key_exposes_envelopes_and_opaque_processes_after_native_writes() {
    let (directory, storage) = fixture();
    let screenshot_id = screenshot(&storage, "synthetic-hash", "PrivateApplication.exe");
    let expected = vector(&storage, screenshot_id);
    let cluster = storage
        .create_smart_cluster(
            "Private cluster topic",
            0.5,
            None,
            Some("Private cluster name"),
        )
        .unwrap();
    summary(&storage, cluster);
    storage
        .update_smart_cluster_anchor_vector(
            cluster,
            &expected,
            "Private anchor digest",
            "synthetic",
            "1",
        )
        .unwrap();

    // This connection has the public bootstrap key and no application UDFs,
    // session, retained private handle, or broker transport.
    let public = load_public_key_from_file(&storage.credential_state).unwrap();
    let reader = Connection::open_with_flags(
        directory.path().join("screenshots.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    crate::storage::connection::configure_sqlcipher_connection(
        &reader,
        &crate::credential_manager::derive_db_key_from_public_key(&public),
    )
    .unwrap();
    let process: String = reader
        .query_row(
            "SELECT process_name FROM screenshots WHERE id=?1",
            [screenshot_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(process.starts_with(PROCESS_PREFIX));
    assert_ne!(process, "PrivateApplication.exe");
    let labels: Vec<String> = reader.prepare("SELECT anchor_text FROM smart_clusters UNION ALL SELECT display_name FROM smart_clusters UNION ALL SELECT title FROM smart_cluster_summaries UNION ALL SELECT summary FROM smart_cluster_summaries UNION ALL SELECT ocr_summary FROM smart_cluster_summaries UNION ALL SELECT key_points_json FROM smart_cluster_summaries UNION ALL SELECT evidence_json FROM smart_cluster_summaries UNION ALL SELECT source_hash FROM smart_cluster_summaries").unwrap()
        .query_map([], |r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
    assert!(labels
        .iter()
        .all(|text| text.starts_with(TEXT_PREFIX) && !text.contains("Private")));
    let encrypted: Vec<u8> = reader
        .query_row("SELECT vector_f32 FROM derived_embeddings", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(is_sealed(&encrypted));
    assert_eq!(
        encrypted.len(),
        expected.len() * 4 + carbonpaper_app_bound::payload::OVERHEAD
    );
    let anchor: Vec<u8> = reader
        .query_row(
            "SELECT anchor_vector FROM smart_clusters WHERE id=?1",
            [cluster],
            |r| r.get(0),
        )
        .unwrap();
    assert!(is_sealed(&anchor));
    let process_label: Vec<u8> = reader
        .query_row(
            "SELECT name_enc FROM protected_process_names WHERE token=?1",
            [&process],
            |r| r.get(0),
        )
        .unwrap();
    assert!(is_sealed(&process_label));
    assert!(reader
        .query_row(
            "SELECT cp_text(anchor_text,'cluster_anchor:'||id) FROM smart_clusters",
            [],
            |r| r.get::<_, String>(0)
        )
        .is_err());
    drop(reader);

    storage.credential_state.invalidate_session();
    storage.clear_protected_read_cache();
    assert!(storage
        .list_smart_clusters()
        .unwrap_err()
        .contains("AUTH_REQUIRED"));
    assert!(storage
        .list_distinct_processes()
        .unwrap_err()
        .contains("AUTH_REQUIRED"));
    storage.credential_state.update_auth_time();
    assert_eq!(
        storage
            .get_smart_cluster_summary(cluster)
            .unwrap()
            .unwrap()
            .summary
            .as_deref(),
        Some("Private project summary")
    );
}

#[test]
fn cold_capture_and_process_filters_work_across_key_epochs_without_private_key_reads() {
    let (_directory, storage) = fixture();
    storage.credential_state.invalidate_session();
    let first = screenshot(&storage, "capture-one", "Editor.exe");
    storage.rotate_protected_write_keys().unwrap();
    let second = screenshot(&storage, "capture-two", "Editor.exe");
    let other = screenshot(&storage, "capture-other", "Browser.exe");
    assert!(!storage.credential_state.protected_read_authorized());
    storage.credential_state.update_auth_time();
    assert_eq!(
        storage.list_distinct_processes().unwrap(),
        vec![("Editor.exe".into(), 2), ("Browser.exe".into(), 1)]
    );
    let result = storage
        .search_text(
            "",
            20,
            0,
            false,
            Some(vec!["Editor.exe".into()]),
            None,
            None,
            None,
        )
        .unwrap();
    let ids = result
        .iter()
        .map(|row| row.screenshot_id)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&first) && ids.contains(&second) && !ids.contains(&other));
    let deleted = storage
        .soft_delete_process_month("Editor.exe", None, None)
        .unwrap();
    assert_eq!(deleted.screenshots_marked, 2);
    assert_eq!(
        storage.list_distinct_processes().unwrap(),
        vec![("Browser.exe".into(), 1)]
    );
}

#[test]
fn native_resident_vector_cache_requires_a_current_ui_session_on_every_query() {
    let (_directory, storage) = fixture();
    let id = screenshot(&storage, "resident-fixture", "Editor.exe");
    let query = vector(&storage, id);
    assert_eq!(storage.semantic_text_topk(&query, 1).unwrap().len(), 1);
    storage.credential_state.invalidate_session();
    // No cache-clearing notification has run yet. A warm matrix alone must
    // still be insufficient to return a protected query result.
    assert_eq!(
        storage.semantic_text_topk(&query, 1).unwrap_err(),
        "AUTH_REQUIRED"
    );
    storage.credential_state.update_auth_time();
    assert_eq!(storage.semantic_text_topk(&query, 1).unwrap().len(), 1);
}

#[test]
fn hostile_schema_cannot_use_decrypting_functions_to_persist_plaintext() {
    let (_directory, storage) = fixture();
    let cluster = storage
        .create_smart_cluster("Sensitive schema target", 0.5, None, None)
        .unwrap();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute_batch(&format!("CREATE TABLE plaintext_sink(content TEXT);
        CREATE VIEW hostile_view AS SELECT cp_text(anchor_text,'cluster_anchor:'||id) AS content FROM smart_clusters;
        CREATE TRIGGER hostile_trigger AFTER INSERT ON smart_clusters BEGIN
            INSERT INTO plaintext_sink SELECT cp_text(anchor_text,'cluster_anchor:'||id) FROM smart_clusters WHERE id={cluster}; END;")).unwrap();
    assert!(conn
        .query_row("SELECT content FROM hostile_view", [], |r| r
            .get::<_, String>(0))
        .is_err());
    drop(guard);
    assert!(storage
        .create_smart_cluster("Another topic", 0.5, None, None)
        .is_err());
    let guard = storage.db.lock().unwrap();
    assert_eq!(
        guard
            .as_ref()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM plaintext_sink", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn restart_keeps_history_readable_and_never_selects_injected_wrapped_keys_for_writes() {
    let (directory, storage) = fixture();
    let credentials = storage.credential_state.clone();
    let cluster = storage
        .create_smart_cluster("Preserved cluster topic", 0.5, None, None)
        .unwrap();
    summary(&storage, cluster);
    let known = [0x42; 32];
    let injected_id = "98".repeat(16);
    let public = load_public_key_from_file(&credentials).unwrap();
    let wrapped =
        crate::credential_manager::encrypt_with_exported_public_key(&public, &known).unwrap();
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO protected_storage_keys VALUES(?1,'ann',?2,?3,?4)",
            params![
                injected_id,
                storage.protected.state.lock().unwrap().dataset,
                hex::encode(Sha256::digest(public)),
                wrapped
            ],
        )
        .unwrap();
    }
    storage.shutdown().unwrap();
    let reopened = StorageState::new(directory.path().into(), credentials);
    reopened.initialize().unwrap();
    assert_eq!(
        reopened.list_smart_clusters().unwrap()[0].anchor_text,
        "Preserved cluster topic"
    );
    assert_eq!(
        reopened
            .get_smart_cluster_summary(cluster)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Private project title")
    );
    let write = reopened.protected_ann_write_key().unwrap();
    assert_ne!(write.id, injected_id);
    assert_ne!(write.bytes(), &known);
    assert!(reopened.protected_ann_resume_key(&injected_id).is_err());
}

#[test]
fn interrupted_legacy_migration_preserves_rows_and_revisions_then_retires_plaintext_caches() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let (directory, storage) = fixture();
    let credentials = storage.credential_state.clone();
    let original_dataset = storage.processing_dataset_id().unwrap();
    let vector = vec![0.1234567f32, 0.7654321, 0.2345678];
    let vector_bytes = vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let sidecars = directory.path().join("derived-indexes");
    std::fs::create_dir_all(&sidecars).unwrap();
    let held_path = sidecars.join("clip_image-7.checkpoint-5.partial");
    std::fs::write(&held_path, &vector_bytes).unwrap();
    std::fs::write(sidecars.join(".clip_image-7.cpdvec.partial"), &vector_bytes).unwrap();
    std::fs::write(sidecars.join("semantic_text-7.cpdvec"), &vector_bytes).unwrap();
    std::fs::write(
        directory.path().join("anchors.json"),
        r#"{"custom":[{"text":"Private learned anchor","learned_vector":[0.1234567,0.7654321]}]}"#,
    )
    .unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .open(&held_path)
        .unwrap();
    let anchor_before_retry = {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute_batch("DELETE FROM app_metadata WHERE key='protected_content_version';
            DELETE FROM classification_anchors;
            INSERT INTO screenshots(id,image_path,image_hash,process_name,created_at,status) VALUES(7,'fixture','legacy-hash','LegacyPrivate.exe','2026-10-15 12:00:00','committed');
            INSERT INTO screenshot_processing_revisions(screenshot_id,revision) VALUES(7,37);
            INSERT INTO recap_days(day,start_ms,end_ms,covered_until_ms,revision,privacy_hash) VALUES('2026-10-15',0,9999999999999,9999999999999,23,'fixture');
            INSERT INTO smart_clusters(id,anchor_text,display_name,threshold,enabled) VALUES(7,'Private legacy topic','Private legacy name',0.5,1);
            INSERT INTO smart_cluster_summaries(smart_cluster_id,title,summary,ocr_summary,key_points_json,evidence_json,source_hash) VALUES(7,'Private legacy title','Private legacy summary','Private legacy OCR','[\"Private legacy point\"]','[\"Private legacy evidence\"]','Private legacy hash');
            CREATE TABLE tasks(id INTEGER PRIMARY KEY,label TEXT);
            INSERT INTO tasks VALUES(7,'Private legacy label');").unwrap();
        conn.execute("UPDATE smart_clusters SET anchor_vector=?1,anchor_vector_dimensions=3,anchor_vector_source_hash='Private vector digest' WHERE id=7", [&vector_bytes]).unwrap();
        conn.execute("INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,model_id,model_revision,embedding_version,source_fingerprint) VALUES('semantic_text','7',3,?1,'synthetic','1',1,'fixture')", [&vector_bytes]).unwrap();
        let error = storage.migrate_protected_content(conn).unwrap_err();
        assert!(
            error.contains("Cannot retire legacy vector sidecar"),
            "{error}"
        );
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='protected_content_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(version, None);
        assert_eq!(
            conn.query_row(
                "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            37
        );
        assert_eq!(
            conn.query_row(
                "SELECT revision FROM recap_days WHERE day='2026-10-15'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            23
        );
        let label: String = conn
            .query_row("SELECT label FROM tasks WHERE id=7", [], |r| r.get(0))
            .unwrap();
        assert!(label.starts_with(TEXT_PREFIX));
        let anchor: String = conn
            .query_row(
                "SELECT anchor_text FROM smart_clusters WHERE id=7",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(anchor.starts_with(TEXT_PREFIX));
        anchor
    };
    assert!(!directory.path().join("anchors.json").exists());
    drop(held);
    storage.shutdown().unwrap();
    let reopened = StorageState::new(directory.path().into(), credentials);
    reopened.initialize().unwrap();
    let clusters = reopened.list_smart_clusters().unwrap();
    assert_eq!(clusters[0].anchor_text, "Private legacy topic");
    assert_eq!(
        clusters[0].display_name.as_deref(),
        Some("Private legacy name")
    );
    assert_eq!(
        clusters[0].summary.as_ref().unwrap().summary.as_deref(),
        Some("Private legacy summary")
    );
    let guard = reopened.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT anchor_text FROM smart_clusters WHERE id=7",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        anchor_before_retry
    );
    assert_eq!(conn.query_row("SELECT cp_unseal(vector_f32,'vectors','semantic_text:'||subject_key) FROM derived_embeddings WHERE subject_key='7'", [], |r|r.get::<_,Vec<u8>>(0)).unwrap(), vector_bytes);
    assert_eq!(
        conn.query_row(
            "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        37
    );
    assert_eq!(
        conn.query_row(
            "SELECT revision FROM recap_days WHERE day='2026-10-15'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        23
    );
    assert_eq!(
        conn.query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM app_metadata WHERE key='protected_content_version'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "1"
    );
    drop(guard);
    assert_eq!(reopened.processing_dataset_id().unwrap(), original_dataset);
    assert!(std::fs::read_dir(sidecars).unwrap().next().is_none());
    assert_eq!(
        std::fs::metadata(directory.path().join("screenshots.db-wal"))
            .map(|m| m.len())
            .unwrap_or(0),
        0
    );
    let anchors = reopened.load_classification_anchors().unwrap();
    assert!(serde_json::to_string(&anchors.anchors)
        .unwrap()
        .contains("Private learned anchor"));
}

#[test]
fn legacy_format_prefixes_do_not_hide_plaintext_content_from_migration() {
    let (_directory, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute_batch("DELETE FROM app_metadata WHERE key='protected_content_version';
        INSERT INTO smart_clusters(id,anchor_text,threshold,enabled) VALUES(9,'cpe1:plaintext topic',0.5,1);
        INSERT INTO screenshots(id,image_path,image_hash,process_name,status) VALUES(9,'fixture','fixture-prefix','cpp1:legacy.exe','committed');
        INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,model_id,model_revision,embedding_version,source_fingerprint)
        VALUES('semantic_text','9',2,X'4350454E43303031','synthetic','1',1,'fixture');
        CREATE TABLE tasks(id INTEGER PRIMARY KEY,label TEXT);
        INSERT INTO tasks VALUES(9,'cpe1:plaintext task');").unwrap();
    storage.migrate_protected_content(conn).unwrap();
    let topic: String = conn
        .query_row(
            "SELECT cp_text(anchor_text,'cluster_anchor:'||id) FROM smart_clusters WHERE id=9",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let label: String = conn
        .query_row(
            "SELECT cp_text(label,'task_label:'||id) FROM tasks WHERE id=9",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let process: String = conn
        .query_row(
            "SELECT cp_process(process_name) FROM screenshots WHERE id=9",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let vector: Vec<u8> = conn.query_row("SELECT cp_unseal(vector_f32,'vectors',index_kind||':'||subject_key) FROM derived_embeddings WHERE subject_key='9'", [], |r| r.get(0)).unwrap();
    assert_eq!(topic, "cpe1:plaintext topic");
    assert_eq!(label, "cpe1:plaintext task");
    assert_eq!(process, "cpp1:legacy.exe");
    assert_eq!(vector, b"CPENC001");
}

#[test]
fn native_decrypted_projections_reject_plaintext_and_cross_record_ciphertext() {
    let (_directory, storage) = fixture();
    let first = storage
        .create_smart_cluster("Private first topic", 0.5, None, None)
        .unwrap();
    let second = storage
        .create_smart_cluster("Private second topic", 0.5, None, None)
        .unwrap();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute("UPDATE smart_clusters SET anchor_text=(SELECT anchor_text FROM smart_clusters WHERE id=?1) WHERE id=?2", params![first,second]).unwrap();
    assert!(conn
        .query_row(
            "SELECT cp_text(anchor_text,'cluster_anchor:'||id) FROM smart_clusters WHERE id=?1",
            [second],
            |r| r.get::<_, String>(0)
        )
        .is_err());
    conn.execute(
        "UPDATE smart_clusters SET anchor_text='injected plaintext' WHERE id=?1",
        [second],
    )
    .unwrap();
    assert!(conn
        .query_row(
            "SELECT cp_text(anchor_text,'cluster_anchor:'||id) FROM smart_clusters WHERE id=?1",
            [second],
            |r| r.get::<_, String>(0)
        )
        .is_err());
}
