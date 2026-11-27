//! End-to-end storage backups use disposable databases and independent unnamed
//! CNG keys. No test opens the user's persisted key or Windows Hello UI.

use super::*;
use crate::credential_manager::{
    encrypt_with_master_key, get_cached_master_key, wrap_storage_domain_key,
};
use crate::storage::{smart_cluster::SmartClusterSummaryUpsert, StorageState};
use base64::{engine::general_purpose::STANDARD, Engine};
use carbonpaper_app_bound::protocol::{BrokerError, Request, Response};
use rusqlite::params;
use tempfile::TempDir;

#[test]
fn accepted_cancellation_prevents_commit_even_when_racing() {
    for _ in 0..64 {
        let control = Control::default();
        let _operation = control.begin();
        let barrier = std::sync::Barrier::new(2);
        let (accepted, committed) = std::thread::scope(|scope| {
            let cancellation = scope.spawn(|| {
                barrier.wait();
                control.request_cancel()
            });
            barrier.wait();
            let committed = control.begin_commit();
            (cancellation.join().unwrap(), committed)
        });
        if accepted {
            assert!(committed.unwrap_err().contains("BACKUP_CANCELLED"));
        } else {
            committed.unwrap();
            assert!(!control.request_cancel());
        }
    }
}

struct OfflineBroker;
impl crate::processing_stage::Broker for OfflineBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(false)
    }
    fn supported(&self) -> bool {
        false
    }
    fn call(&self, _: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        Err(BrokerError::Unavailable)
    }
}

struct RetirementFailureBroker(AtomicBool);

struct QueuedOfflineBroker(std::sync::Mutex<Vec<carbonpaper_app_bound::revocation::Revocation>>);
impl crate::processing_stage::Broker for QueuedOfflineBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(true)
    }
    fn supported(&self) -> bool {
        true
    }
    fn call(&self, _: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        Err(BrokerError::Unavailable)
    }
    fn persist_revocation(
        &self,
        intent: carbonpaper_app_bound::revocation::Revocation,
    ) -> carbonpaper_app_bound::protocol::Result<bool> {
        self.0.lock().unwrap().push(intent);
        Ok(true)
    }
}
impl crate::processing_stage::Broker for RetirementFailureBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        if self.0.swap(false, Ordering::SeqCst) {
            Err(BrokerError::Unavailable)
        } else {
            Ok(false)
        }
    }
    fn supported(&self) -> bool {
        true
    }
    fn call(&self, _: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        Err(BrokerError::Unavailable)
    }
}

fn storage(directory: &Path) -> StorageState {
    std::fs::create_dir_all(directory).unwrap();
    let credentials = crate::credential_manager::cng_tests::storage_credentials(directory);
    open_storage(directory, credentials)
}

fn open_storage(directory: &Path, credentials: Arc<CredentialManagerState>) -> StorageState {
    let mut storage = StorageState::new(directory.into(), credentials);
    storage.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(OfflineBroker)),
    );
    storage.initialize().unwrap();
    storage
}

fn encrypted(storage: &StorageState, value: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let key = rand::random::<[u8; 32]>();
    (
        encrypt_with_master_key(&key, value).unwrap(),
        wrap_storage_domain_key(&storage.credential_state, &key).unwrap(),
    )
}

fn populate(storage: &StorageState, plaintext_image: bool) -> i64 {
    populate_with_layout(storage, plaintext_image, None)
}

fn populate_with_layout(
    storage: &StorageState,
    plaintext_image: bool,
    text_layout: Option<&crate::ml_protocol::MlOcrTextLayout>,
) -> i64 {
    let credentials = &storage.credential_state;
    let directory = storage.data_dir.lock().unwrap().clone();
    credentials.cache_master_key_for_tests(vec![9; 32]);
    std::fs::write(
        directory.join(MASTER_KEY_FILE_NAME),
        credentials
            .prepare_import_master_key_file(&[9; 32])
            .unwrap(),
    )
    .unwrap();
    credentials.update_auth_time();
    let image = image_bytes();
    let key = rand::random::<[u8; 32]>();
    let wrap = wrap_storage_domain_key(credentials, &key).unwrap();
    let title = encrypt_with_master_key(&key, b"Private window title").unwrap();
    let relative = if plaintext_image {
        "screenshots/旧截图.png"
    } else {
        "screenshots/私密截图.png.enc"
    };
    let data = if plaintext_image {
        image.clone()
    } else {
        encrypt_with_master_key(&key, &image).unwrap()
    };
    std::fs::write(directory.join(relative), data).unwrap();
    let fresh_ocr_key = text_layout.map(|_| storage.new_ocr_write_key().unwrap());
    let legacy_ocr = text_layout
        .is_none()
        .then(|| encrypted(storage, "私密备份搜索内容".as_bytes()));
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        let process = storage
            .protected
            .process_token(conn, "PrivateApplication.exe")
            .unwrap();
        conn.execute("INSERT INTO screenshots(id,image_path,image_hash,window_title_enc,content_key_encrypted,process_name,created_at,status)
            VALUES(7,?1,'image-fixture',?2,?3,?4,'2026-11-05 10:00:00','committed')", params![relative, title, wrap, process]).unwrap();
        let ocr = if let Some(key) = fresh_ocr_key {
            let record = crate::storage::ocr_blocks::Record {
                id: 11,
                text: "私密备份搜索内容".into(),
                text_layout: text_layout.cloned(),
                confidence: 0.99,
                box_coords: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            };
            crate::storage::ocr_blocks::insert_records(conn, 7, &[record], Some(&key), || {
                storage.new_ocr_write_key()
            })
            .unwrap();
            let records = [
                (12, "discarded backup member"),
                (13, "retained backup member"),
            ]
            .map(|(id, text)| crate::storage::ocr_blocks::Record {
                id,
                text: text.into(),
                text_layout: None,
                confidence: 0.95,
                box_coords: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            });
            crate::storage::ocr_blocks::insert_records(conn, 7, &records, None, || {
                storage.new_ocr_write_key()
            })
            .unwrap();
            conn.execute("DELETE FROM ocr_results WHERE id=12", [])
                .unwrap();
            Vec::new()
        } else {
            let (data, key) = legacy_ocr.unwrap();
            conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,text_key_encrypted,confidence,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
                VALUES(11,7,'',?1,?2,0.99,0,0,1,0,1,1,0,1)", params![data,key]).unwrap();
            data
        };
        let ocr_reference = conn
            .query_row(
                "SELECT text_key_ref FROM ocr_results_read WHERE id=11",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .unwrap();
        storage
            .index_batch_internal_on_conn(
                conn,
                vec![(11, ocr, ocr_reference)],
                &credentials.get_hmac_key().unwrap(),
            )
            .unwrap();
        for (sql, value) in [
            ("INSERT INTO screenshot_document_refs(screenshot_id,ref_enc,content_key_encrypted) VALUES(7,?1,?2)", b"private document reference".as_slice()),
            ("INSERT INTO page_icons(id,content_hash,icon_enc,icon_key_encrypted) VALUES(1,'icon',?1,?2)", b"private icon".as_slice()),
            ("INSERT INTO link_sets(id,content_hash,links_enc,links_key_encrypted) VALUES(1,'links',?1,?2)", b"private links".as_slice()),
        ] {
            let (data, key) = encrypted(storage, value);
            conn.execute(sql, params![data, key]).unwrap();
        }
        let vector = storage
            .protected
            .seal("vectors", "clip_image:image-fixture", &[0, 0, 128, 63])
            .unwrap();
        conn.execute("INSERT INTO derived_embeddings(index_kind,subject_key,dimensions,vector_f32,model_id,model_revision,embedding_version,source_fingerprint)
            VALUES('clip_image','image-fixture',1,?1,'fixture','1',1,'source')", [&vector]).unwrap();
        while crate::storage::vector_blocks::compact(conn).unwrap() != 0 {}
        conn.execute("INSERT INTO recap_days(day,start_ms,end_ms,privacy_hash,covered_until_ms) VALUES('2026-11-05',0,2000000000000,'fixture',2000000000000)", []).unwrap();
        let (payload, key) = encrypted(storage, br#"{"private":"settings"}"#);
        conn.execute("INSERT INTO recap_payloads(kind,key,day,revision,data,row_key) VALUES('settings','fixture','2026-11-05',0,?1,?2)", params![payload, key]).unwrap();
        let (payload, key) = encrypted(storage, b"private recap search document");
        conn.execute("INSERT INTO recap_search_documents(id,day,revision,privacy_hash,start_ms,end_ms,data,row_key) VALUES(17,'2026-11-05',0,'fixture',0,1000,?1,?2)", params![payload, key]).unwrap();
        // A restored receipt must never restore background processing authority.
        conn.execute("INSERT INTO app_bound_receipts(task_id,consumer,dataset_id,receipt_json) VALUES('old-task',1,'old-dataset','{}')", []).unwrap();
    }
    let cluster = storage
        .create_smart_cluster("Private old topic", 0.5, None, Some("Private old name"))
        .unwrap();
    storage
        .update_smart_cluster_anchor_vector(
            cluster,
            &[0.1, 0.2, 0.3],
            "private anchor hash",
            "fixture",
            "1",
        )
        .unwrap();
    storage
        .upsert_smart_cluster_summary(&SmartClusterSummaryUpsert {
            smart_cluster_id: cluster,
            title: Some("Private title".into()),
            summary: Some("Private summary".into()),
            ocr_summary: Some("Private OCR summary".into()),
            key_points: Some(serde_json::json!(["private point"])),
            evidence: Some(serde_json::json!([{"text":"private evidence"}])),
            source_snapshot_count: Some(1),
            source_hash: Some("private source hash".into()),
            model_provider: Some("fixture".into()),
            model_name: Some("fixture".into()),
            prompt_version: Some("1".into()),
        })
        .unwrap();
    storage.rotate_protected_write_keys().unwrap();
    storage
        .create_smart_cluster("Private new topic", 0.5, None, None)
        .unwrap();
    cluster
}

fn image_bytes() -> Vec<u8> {
    STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a4bAAAAAASUVORK5CYII=").unwrap()
}

fn export(storage: &StorageState, path: &Path) -> BackupSummary {
    let _maintenance = storage.database_maintenance("test backup export");
    let _operation = storage.backup.begin();
    storage.prepare_ocr_blocks_for_backup().unwrap();
    let directory = storage.data_dir.lock().unwrap().clone();
    let key = storage.database_key().unwrap();
    let master = Zeroizing::new(get_cached_master_key(&storage.credential_state).unwrap());
    let snapshot = storage
        .create_backup_snapshot_under_maintenance(&key, directory.parent().unwrap(), |_, _| {})
        .unwrap();
    write_portable(
        snapshot.path(),
        &directory,
        &storage.credential_state,
        &key,
        &master,
        "backup-password",
        path,
        &storage.backup,
        |_, _, _| {},
    )
    .unwrap()
}

#[test]
fn missing_committed_image_preserves_ocr_and_metadata_across_credentials() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("missing.cpbackup");
    let source_directory = root.path().join("source");
    let source = storage(&source_directory);
    populate(&source, false);
    std::fs::remove_file(source_directory.join("screenshots/私密截图.png.enc")).unwrap();
    let generation = source.db_generation();
    let dataset = source.processing_dataset_id().unwrap();
    let summary = export(&source, &archive);
    assert_eq!(summary.missing_screenshots, 1);
    assert!(source.is_initialized());
    assert_eq!(source.db_generation(), generation);
    assert_eq!(source.processing_dataset_id().unwrap(), dataset);
    drop(source);

    let destination_directory = root.path().join("destination");
    let destination = storage(&destination_directory);
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    assert_eq!(
        prepared.extracted.manifest.format_version,
        database_snapshot::PORTABLE_BACKUP_FORMAT_VERSION
    );
    assert_eq!(
        prepared.extracted.manifest.missing_screenshots[0].screenshot_id,
        7
    );
    {
        let _maintenance = destination.database_maintenance("test missing screenshot import");
        install_prepared(&destination, &prepared).unwrap();
    }
    let credentials = destination.credential_state.clone();
    destination.shutdown().unwrap();
    drop(_operation);
    drop(destination);
    drop(prepared);
    let credentials = crate::credential_manager::cng_tests::reopen_storage_credentials(
        &credentials,
        &destination_directory,
    );
    let reopened = open_storage(&destination_directory, credentials);
    assert_eq!(
        reopened.get_screenshot_ocr_results(7).unwrap()[0].text,
        "私密备份搜索内容"
    );
    let screenshot = reopened.screenshots().by_id(7).unwrap().unwrap();
    assert_eq!(
        screenshot.window_title.as_deref(),
        Some("Private window title")
    );
    let conn = reopened.db.lock().unwrap();
    let record: (i64, String) = conn
        .as_ref()
        .unwrap()
        .query_row(
            "SELECT is_deleted,image_path FROM screenshots WHERE id=7",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(record, (0, "screenshots/私密截图.png.enc".into()));
    assert!(!destination_directory.join(record.1).exists());
}

#[test]
fn export_recovers_a_stale_pending_path_only_in_its_snapshot() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("recovered.cpbackup");
    let source_directory = root.path().join("source");
    let source = storage(&source_directory);
    populate(&source, false);
    let pending = "screenshots/私密截图.png.enc.pending";
    source
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute("UPDATE screenshots SET image_path=?1 WHERE id=7", [pending])
        .unwrap();
    let summary = export(&source, &archive);
    assert_eq!(summary.repaired_paths, 1);
    assert_eq!(summary.missing_screenshots, 0);
    assert_eq!(
        source
            .db
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .query_row("SELECT image_path FROM screenshots WHERE id=7", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        pending
    );
    drop(source);
    let destination = storage(&root.path().join("destination"));
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    {
        let _maintenance = destination.database_maintenance("test pending image import");
        install_prepared(&destination, &prepared).unwrap();
    }
    destination.credential_state.update_auth_time();
    assert_eq!(
        STANDARD
            .decode(destination.read_image_by_id(7).unwrap().0)
            .unwrap(),
        image_bytes()
    );
}

#[test]
fn images_removed_after_inventory_are_declared_in_the_completed_archive() {
    for removal_phase in ["scan", "files"] {
        let root = TempDir::new().unwrap();
        let source_directory = root.path().join("source");
        let archive = root.path().join("late-missing.cpbackup");
        let source = storage(&source_directory);
        populate(&source, false);
        let _operation = source.backup.begin();
        let snapshot = source
            .create_backup_snapshot_under_maintenance(
                &source.database_key().unwrap(),
                root.path(),
                |_, _| {},
            )
            .unwrap();
        let mut removed = false;
        let summary = write_portable(
            snapshot.path(),
            &source_directory,
            &source.credential_state,
            &source.database_key().unwrap(),
            &[9; 32],
            "backup-password",
            &archive,
            &source.backup,
            |phase, _, _| {
                if phase == removal_phase && !removed {
                    std::fs::remove_file(source_directory.join("screenshots/私密截图.png.enc"))
                        .unwrap();
                    removed = true;
                }
            },
        )
        .unwrap();
        assert!(removed);
        assert_eq!(summary.missing_screenshots, 1);
        let destination = storage(&root.path().join("destination"));
        let _operation = destination.backup.begin();
        let prepared = prepare(&destination, &archive, root.path());
        assert_eq!(prepared.extracted.manifest.missing_screenshots.len(), 1);
        assert_eq!(
            prepared.extracted.manifest.missing_screenshots[0].screenshot_id,
            7
        );
    }
}

#[test]
fn corrupt_image_aborts_export_with_context_and_preserves_the_previous_backup() {
    let root = TempDir::new().unwrap();
    let source_directory = root.path().join("source");
    let archive = root.path().join("existing.cpbackup");
    std::fs::write(&archive, b"previous valid backup").unwrap();
    let source = storage(&source_directory);
    populate(&source, false);
    let image = source_directory.join("screenshots/私密截图.png.enc");
    let mut bytes = std::fs::read(&image).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&image, bytes).unwrap();
    let _operation = source.backup.begin();
    let snapshot = source
        .create_backup_snapshot_under_maintenance(
            &source.database_key().unwrap(),
            root.path(),
            |_, _| {},
        )
        .unwrap();
    let error = write_portable(
        snapshot.path(),
        &source_directory,
        &source.credential_state,
        &source.database_key().unwrap(),
        &[9; 32],
        "backup-password",
        &archive,
        &source.backup,
        |_, _, _| {},
    )
    .unwrap_err();
    assert!(error
        .contains("Screenshot authentication failed (id=7, path=screenshots/私密截图.png.enc)"));
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous valid backup");
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".partial")));
}

#[test]
fn streamed_key_validation_rejects_corrupt_metadata_ocr_scope_and_ocr_content() {
    for (corrupt, expected) in [
        (
            "metadata",
            "Content authentication failed for screenshots.window_title_enc",
        ),
        ("ocr_scope", "Invalid OCR key or payload binding"),
        ("ocr_content", "Decryption failed"),
        ("ocr_block_scope", "Invalid OCR key or payload binding"),
        ("ocr_block_content", "Invalid OCR key or payload binding"),
        (
            "legacy_ocr_content",
            "Content authentication failed for ocr_results.text_enc",
        ),
    ] {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("source");
        let archive = root.path().join("previous.cpbackup");
        let source = storage(&directory);
        populate(&source, false);
        // Cover legacy envelopes alongside explicit inline groups and blocks.
        if matches!(
            corrupt,
            "ocr_scope" | "ocr_content" | "ocr_block_scope" | "ocr_block_content"
        ) {
            let key = source.new_ocr_write_key().unwrap();
            let guard = source.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            if corrupt.starts_with("ocr_block_") {
                let record = crate::storage::ocr_blocks::Record {
                    id: 12,
                    text: "grouped backup content".into(),
                    text_layout: None,
                    confidence: 0.99,
                    box_coords: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                };
                crate::storage::ocr_blocks::insert_records(conn, 7, &[record], Some(&key), || {
                    source.new_ocr_write_key()
                })
                .unwrap();
            } else {
                let dataset = crate::storage::ocr_keys::dataset(conn).unwrap();
                key.insert(conn, &dataset, 7).unwrap();
                let data = key
                    .encrypt(&dataset, 7, 12, b"grouped backup content")
                    .unwrap();
                conn.execute(
                    "INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,ocr_key_id) VALUES(12,7,'',?1,?2)",
                    params![data, key.id],
                )
                .unwrap();
            }
        }
        std::fs::write(&archive, b"previous valid backup").unwrap();
        let _operation = source.backup.begin();
        let snapshot = source
            .create_backup_snapshot_under_maintenance(
                &source.database_key().unwrap(),
                root.path(),
                |_, _| {},
            )
            .unwrap();
        let conn = Connection::open(snapshot.path().join(DATABASE_FILE_NAME)).unwrap();
        connection::configure_sqlcipher_connection(&conn, &source.database_key().unwrap()).unwrap();
        conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
            false,
        )
        .unwrap();
        let sql = match corrupt {
            "metadata" => "UPDATE screenshots SET window_title_enc=zeroblob(28) WHERE id=7",
            "ocr_scope" | "ocr_block_scope" => "UPDATE ocr_keys SET scope_tag=zeroblob(32)",
            "ocr_content" => "UPDATE ocr_results SET text_enc=zeroblob(28) WHERE id=12",
            "ocr_block_content" => "UPDATE ocr_blocks SET content_enc=zeroblob(28)",
            _ => "UPDATE ocr_results SET text_enc=zeroblob(28) WHERE id=11",
        };
        assert_eq!(conn.execute(sql, []).unwrap(), 1);
        conn.close().unwrap();
        let error = write_portable(
            snapshot.path(),
            &directory,
            &source.credential_state,
            &source.database_key().unwrap(),
            &[9; 32],
            "backup-password",
            &archive,
            &source.backup,
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(error.contains(expected), "{corrupt}: {error}");
        assert_eq!(std::fs::read(&archive).unwrap(), b"previous valid backup");
    }
}

#[test]
fn undeclared_missing_image_rejects_import_before_installing() {
    let root = TempDir::new().unwrap();
    let original = root.path().join("original.cpbackup");
    let incomplete = root.path().join("incomplete.cpbackup");
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    export(&source, &original);
    drop(source);
    // Build an authenticated archive with a missing payload but no declaration:
    // encryption/ZIP integrity cannot substitute for the completeness contract.
    let flag = Arc::new(AtomicBool::new(false));
    let reader = EncryptedReader::open(
        File::open(&original).unwrap(),
        "backup-password",
        flag.clone(),
    )
    .unwrap();
    let mut input = zip::ZipArchive::new(reader).unwrap();
    let encrypted =
        EncryptedWriter::new(File::create(&incomplete).unwrap(), "backup-password", flag).unwrap();
    let mut output = zip::ZipWriter::new_stream(encrypted);
    for index in 0..input.len() {
        let mut entry = input.by_index(index).unwrap();
        let name = entry.name().unwrap().into_owned();
        if name == "screenshots/私密截图.png.enc" {
            continue;
        }
        output
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        std::io::copy(&mut entry, &mut output).unwrap();
    }
    output.finish().unwrap().into_inner().finish().unwrap();
    let destination = storage(&root.path().join("destination"));
    let generation = destination.db_generation();
    let _operation = destination.backup.begin();
    let result = prepare_import(
        &incomplete,
        root.path(),
        &destination.credential_state,
        &destination.database_key().unwrap(),
        "backup-password",
        &destination.backup,
        |_, _, _| {},
    );
    assert!(result
        .err()
        .unwrap()
        .contains("BACKUP_UNDECLARED_MISSING_SCREENSHOT"));
    assert_eq!(destination.db_generation(), generation);
    assert_eq!(destination.count_active_screenshots().unwrap(), 0);
}

fn prepare(storage: &StorageState, archive: &Path, parent: &Path) -> PreparedBackup {
    prepare_import(
        archive,
        parent,
        &storage.credential_state,
        &storage.database_key().unwrap(),
        "backup-password",
        &storage.backup,
        |_, _, _| {},
    )
    .unwrap()
}

#[test]
fn empty_backup_restores_without_an_archived_payload_directory() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("empty.cpbackup");
    let source = storage(&root.path().join("source"));
    export(&source, &archive);
    drop(source);
    let destination_path = root.path().join("destination");
    let destination = storage(&destination_path);
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    assert!(!prepared.extracted.path().join("screenshots").exists());
    let _maintenance = destination.database_maintenance("test empty backup import");
    install_prepared(&destination, &prepared).unwrap();
    assert!(destination.is_initialized());
    assert!(destination_path.join("screenshots").is_dir());
}

#[test]
fn missing_deleted_and_aborted_images_restore_with_relative_paths() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("missing-images.cpbackup");
    let source_directory = root.path().join("source");
    let source = storage(&source_directory);
    let deleted_relative = "screenshots/deleted.png.enc";
    let aborted_relative = "screenshots/aborted.png.enc";
    let deleted_image = source_directory.join(deleted_relative);
    let deleted_thumbnail = StorageState::thumbnail_path_for(&deleted_image);
    let aborted_image = source_directory.join(aborted_relative);
    {
        let guard = source.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshots(id,image_path,image_hash,status) VALUES(7,?1,'deleted-image','committed')",
            [deleted_image.to_string_lossy().as_ref()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO screenshots(id,image_path,image_hash,status) VALUES(8,?1,'aborted-image','aborted')",
            [aborted_image.to_string_lossy().as_ref()],
        )
        .unwrap();
    }
    assert_eq!(
        source
            .soft_delete_screenshots(&[7])
            .unwrap()
            .queued_screenshots,
        1
    );
    assert!(!deleted_image.exists());
    assert!(!aborted_image.exists());
    export(&source, &archive);
    drop(source);

    // The same absolute location may exist on the importing machine. Cleanup
    // must leave those files intact even though the archive omitted the image.
    std::fs::write(&deleted_image, b"source image").unwrap();
    std::fs::create_dir_all(deleted_thumbnail.parent().unwrap()).unwrap();
    std::fs::write(&deleted_thumbnail, b"source thumbnail").unwrap();
    let destination_directory = root.path().join("destination");
    let destination = storage(&destination_directory);
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    {
        let _maintenance = destination.database_maintenance("test missing images import");
        install_prepared(&destination, &prepared).unwrap();
    }
    {
        let guard = destination.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        let aborted: (String, i32, String) = conn
            .query_row(
                "SELECT image_path,is_deleted,status FROM screenshots WHERE id=8",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(aborted, (aborted_relative.into(), 0, "aborted".into()));
    }
    let candidates = destination.fetch_screenshot_delete_candidates(10).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, 7);
    assert_eq!(candidates[0].image_path, deleted_relative);
    assert_eq!(
        destination.resolve_image_path(&candidates[0].image_path),
        destination_directory.join(deleted_relative)
    );
    destination
        .remove_screenshot_files(&candidates[0].image_path)
        .unwrap();
    assert_eq!(std::fs::read(&deleted_image).unwrap(), b"source image");
    assert_eq!(
        std::fs::read(&deleted_thumbnail).unwrap(),
        b"source thumbnail"
    );
    assert_eq!(
        destination.finalize_screenshot_delete_batch(&[7]).unwrap(),
        1
    );
    assert!(destination
        .fetch_screenshot_delete_candidates(10)
        .unwrap()
        .is_empty());
}

#[test]
fn independent_cng_keys_restore_all_domains_and_survive_a_cold_reopen() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("portable.cpbackup");
    let source = storage(&root.path().join("source"));
    let text_layout = crate::ml_protocol::MlOcrTextLayout {
        matrix: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
        source_size: [2, 2],
        input_width: 32,
        valid_width: 32,
        time_steps: 8,
        centers: (0..8).map(|i| 2 * i + 1).collect(),
        text_lengths: vec![3; 8],
    };
    let cluster = populate_with_layout(&source, false, Some(&text_layout));
    let dirty_key = {
        let guard = source.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT ocr_block_slot FROM ocr_results WHERE id=11",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM ocr_block_gc", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        conn.query_row("SELECT block_id FROM ocr_block_gc", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let source_hmac = source.credential_state.get_hmac_key().unwrap();
    let source_db_key = source.database_key().unwrap();
    let source_dataset = source.processing_dataset_id().unwrap();
    let source_content = source
        .protected
        .write_key("metadata")
        .unwrap()
        .dataset
        .clone();
    let revisions = {
        let guard = source.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        (
            conn.query_row(
                "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT value FROM app_metadata WHERE key='recap_config_revision'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
    };
    export(&source, &archive);
    let ciphertext = std::fs::read(&archive).unwrap();
    for secret in [
        b"portable_keys.bin".as_slice(),
        "私密截图".as_bytes(),
        b"Private summary".as_slice(),
    ] {
        assert!(!ciphertext.windows(secret.len()).any(|w| w == secret));
    }
    assert!(zip::ZipArchive::new(File::open(&archive).unwrap()).is_err());
    drop(source); // Destroy the original unnamed CNG private key before import.
    let destination_path = root.path().join("destination");
    let destination = storage(&destination_path);
    destination
        .create_smart_cluster("Superseded local topic", 0.5, None, None)
        .unwrap();
    assert_ne!(destination.database_key().unwrap(), source_db_key);
    assert_ne!(
        destination.credential_state.get_hmac_key().unwrap(),
        source_hmac
    );
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    assert!(!prepared
        .extracted
        .path()
        .join(backup_keys::FILE_NAME)
        .exists());
    {
        let _maintenance = destination.database_maintenance("test backup import");
        install_prepared(&destination, &prepared).unwrap();
    }
    assert!(!destination.credential_state.is_session_valid());
    assert_ne!(destination.processing_dataset_id().unwrap(), source_dataset);
    let credentials = destination.credential_state.clone();
    destination.shutdown().unwrap();
    drop(_operation);
    drop(destination);
    drop(prepared); // No imported plaintext key bundle or storage key cache remains.
    let reopened_credentials = crate::credential_manager::cng_tests::reopen_storage_credentials(
        &credentials,
        &destination_path,
    );
    assert_eq!(
        get_cached_master_key(&reopened_credentials).unwrap(),
        vec![9; 32]
    );
    assert_eq!(reopened_credentials.get_hmac_key().unwrap(), source_hmac);
    let reopened = open_storage(&destination_path, reopened_credentials);
    assert_eq!(
        STANDARD
            .decode(reopened.read_image_by_id(7).unwrap().0)
            .unwrap(),
        image_bytes()
    );
    assert_eq!(
        reopened.get_screenshot_ocr_results(7).unwrap()[0].text,
        "私密备份搜索内容"
    );
    assert_eq!(
        reopened.get_screenshot_ocr_results(7).unwrap()[0]
            .text_layout
            .as_ref(),
        Some(&text_layout)
    );
    let restored_ocr = reopened.get_screenshot_ocr_results(7).unwrap();
    assert_eq!(
        restored_ocr.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![11, 13]
    );
    assert_eq!(restored_ocr[1].text, "retained backup member");
    assert_eq!(
        reopened
            .get_smart_cluster_summary(cluster)
            .unwrap()
            .unwrap()
            .summary
            .as_deref(),
        Some("Private summary")
    );
    assert_eq!(reopened.list_smart_clusters().unwrap().len(), 2);
    assert_eq!(
        reopened.protected.write_key("metadata").unwrap().dataset,
        source_content
    );
    assert!(!reopened
        .search_text("备份搜索", 20, 0, false, None, None, None, None)
        .unwrap()
        .is_empty());
    let guard = reopened.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ocr_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ocr_block_gc", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM ocr_keys WHERE id=?1",
            [dirty_key],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM app_bound_receipts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(conn.query_row(
        "SELECT cp_unseal(vector_f32,'vectors',index_kind||':'||subject_key) FROM derived_embeddings_read WHERE subject_key='image-fixture'",
        [], |r| r.get::<_, Vec<u8>>(0)).unwrap(), vec![0,0,128,63]);
    assert_eq!(
        conn.query_row(
            "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=7",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        revisions.0
    );
    assert_eq!(
        conn.query_row(
            "SELECT value FROM app_metadata WHERE key='recap_config_revision'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        revisions.1
    );
    for (sql, expected) in [
        (
            "SELECT ref_enc,content_key_encrypted FROM screenshot_document_refs",
            b"private document reference".as_slice(),
        ),
        (
            "SELECT icon_enc,icon_key_encrypted FROM page_icons",
            b"private icon".as_slice(),
        ),
        (
            "SELECT links_enc,links_key_encrypted FROM link_sets",
            b"private links".as_slice(),
        ),
        (
            "SELECT data,row_key FROM recap_payloads WHERE kind='settings'",
            br#"{"private":"settings"}"#.as_slice(),
        ),
        (
            "SELECT data,row_key FROM recap_search_documents",
            b"private recap search document".as_slice(),
        ),
    ] {
        let (data, wrapping): (Vec<u8>, Vec<u8>) = conn
            .query_row(sql, [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(
            reopened
                .decrypt_payload_with_row_key(&data, &wrapping)
                .unwrap(),
            expected
        );
    }
}

#[test]
fn password_failures_and_cancelled_preparation_preserve_the_destination() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("portable.cpbackup");
    let source = storage(&root.path().join("source"));
    populate(&source, true);
    export(&source, &archive);
    let destination_path = root.path().join("destination");
    let destination = storage(&destination_path);
    let _operation = destination.backup.begin();
    let credential_before = std::fs::read(destination_path.join(MASTER_KEY_FILE_NAME)).unwrap();
    let previous_cluster = destination
        .create_smart_cluster("Keep this local topic", 0.5, None, None)
        .unwrap();
    assert!(prepare_import(
        &archive,
        root.path(),
        &destination.credential_state,
        &destination.database_key().unwrap(),
        "wrong password",
        &destination.backup,
        |_, _, _| {}
    )
    .is_err());
    assert!(prepare_import(
        &archive,
        root.path(),
        &destination.credential_state,
        &destination.database_key().unwrap(),
        "backup-password",
        &destination.backup,
        |phase, _, _| {
            if phase == "restore" {
                destination.backup.request_cancel();
            }
        }
    )
    .err()
    .unwrap()
    .contains("BACKUP_CANCELLED"));
    assert_eq!(
        std::fs::read(destination_path.join(MASTER_KEY_FILE_NAME)).unwrap(),
        credential_before
    );
    assert!(destination.is_initialized());
    assert_eq!(
        destination.list_smart_clusters().unwrap()[0].id,
        previous_cluster
    );
    drop(_operation);
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    assert!(!prepared
        .extracted
        .path()
        .join("screenshots/旧截图.png")
        .exists());
    assert!(prepared
        .extracted
        .path()
        .join("screenshots/旧截图.png.enc")
        .exists());
    {
        let _maintenance = destination.database_maintenance("test rollback");
        let result = install_prepared_inner(&destination, &prepared, || {
            Err("injected storage reopen failure".into())
        });
        assert!(result
            .unwrap_err()
            .contains("injected storage reopen failure"));
    }
    assert_eq!(
        get_cached_master_key(&destination.credential_state).unwrap(),
        vec![3; 32]
    );
    assert_eq!(
        std::fs::read(destination_path.join(MASTER_KEY_FILE_NAME)).unwrap(),
        credential_before
    );
    assert!(destination.is_initialized());
    assert_eq!(
        destination.list_smart_clusters().unwrap()[0].anchor_text,
        "Keep this local topic"
    );
    let guard = destination.db.lock().unwrap();
    assert_eq!(
        guard
            .as_ref()
            .unwrap()
            .query_row("SELECT count(*) FROM screenshots", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
#[cfg(windows)]
fn a_locked_rollback_file_keeps_storage_closed_until_recovery_finishes() {
    use std::os::windows::fs::OpenOptionsExt;

    let root = TempDir::new().unwrap();
    let archive = root.path().join("portable.cpbackup");
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    export(&source, &archive);
    let destination_path = root.path().join("destination");
    let destination = storage(&destination_path);
    destination
        .create_smart_cluster("Keep this local topic", 0.5, None, None)
        .unwrap();
    let credential_before = std::fs::read(destination_path.join(MASTER_KEY_FILE_NAME)).unwrap();
    let _operation = destination.backup.begin();
    let prepared = prepare(&destination, &archive, root.path());
    let rollback_lock = std::cell::RefCell::new(None);
    {
        let _maintenance = destination.database_maintenance("test locked rollback");
        let error = install_prepared_inner(&destination, &prepared, || {
            let transaction = std::fs::read_dir(root.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".carbonpaper-restore-transaction-")
                })
                .unwrap();
            *rollback_lock.borrow_mut() = Some(
                OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(transaction.join("old").join(MASTER_KEY_FILE_NAME))
                    .unwrap(),
            );
            Err("injected reopen failure with a locked rollback key".into())
        })
        .unwrap_err();
        assert!(error.contains("File rollback failed"));
        assert!(!destination.is_initialized());
        drop(rollback_lock.borrow_mut().take());
        backup_keys::reset_local_authority(&destination_path, &destination.database_key().unwrap())
            .unwrap();
        destination.initialize_under_maintenance().unwrap();
    }
    assert_eq!(
        std::fs::read(destination_path.join(MASTER_KEY_FILE_NAME)).unwrap(),
        credential_before
    );
    assert_eq!(
        get_cached_master_key(&destination.credential_state).unwrap(),
        vec![3; 32]
    );
    assert_eq!(
        destination.list_smart_clusters().unwrap()[0].anchor_text,
        "Keep this local topic"
    );
}

#[test]
fn retirement_failure_reopens_the_original_content_with_new_processing_authority() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("portable.cpbackup");
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    export(&source, &archive);
    let mut destination = storage(&root.path().join("destination"));
    destination
        .create_smart_cluster("Keep this local topic", 0.5, None, None)
        .unwrap();
    let previous_processing = destination.processing_dataset_id().unwrap();
    let previous_content = destination
        .protected
        .write_key("metadata")
        .unwrap()
        .dataset
        .clone();
    let prepared = prepare(&destination, &archive, root.path());
    destination.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(Arc::new(
            RetirementFailureBroker(AtomicBool::new(true)),
        )),
    );
    let _operation = destination.backup.begin();
    let error = {
        let _maintenance = destination.database_maintenance("test retirement failure");
        install_prepared(&destination, &prepared).unwrap_err()
    };
    assert!(error.contains(&BrokerError::Unavailable.to_string()));
    assert!(destination.is_initialized());
    assert_ne!(
        destination.processing_dataset_id().unwrap(),
        previous_processing
    );
    assert_eq!(
        destination.protected.write_key("metadata").unwrap().dataset,
        previous_content
    );
    assert_eq!(
        get_cached_master_key(&destination.credential_state).unwrap(),
        vec![3; 32]
    );
    assert_eq!(
        destination.list_smart_clusters().unwrap()[0].anchor_text,
        "Keep this local topic"
    );
}

#[test]
fn protected_retirement_inbox_allows_backup_import_while_the_service_is_offline() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("portable.cpbackup");
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    export(&source, &archive);
    let mut destination = storage(&root.path().join("destination"));
    let previous_processing = destination.processing_dataset_id().unwrap();
    let prepared = prepare(&destination, &archive, root.path());
    let broker = Arc::new(QueuedOfflineBroker(std::sync::Mutex::new(Vec::new())));
    destination.replace_processing_stage_for_tests(
        crate::processing_stage::ProcessingStaging::with_test_broker(broker.clone()),
    );
    let _operation = destination.backup.begin();
    {
        let _maintenance = destination.database_maintenance("test offline backup import");
        install_prepared(&destination, &prepared).unwrap();
    }
    assert!(destination.is_initialized());
    assert_ne!(
        destination.processing_dataset_id().unwrap(),
        previous_processing
    );
    assert_eq!(
        *broker.0.lock().unwrap(),
        vec![carbonpaper_app_bound::revocation::Revocation::RetireOwner]
    );
}

#[test]
fn cancelled_export_and_unknown_key_slots_never_replace_an_existing_backup() {
    let root = TempDir::new().unwrap();
    let archive = root.path().join("existing.cpbackup");
    std::fs::write(&archive, b"previous valid backup").unwrap();
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    let directory = source.data_dir.lock().unwrap().clone();
    let _maintenance = source.database_maintenance("test cancellation");
    let snapshot = source
        .create_backup_snapshot_under_maintenance(
            &source.database_key().unwrap(),
            root.path(),
            |_, _| {},
        )
        .unwrap();
    let _operation = source.backup.begin();
    let result = write_portable(
        snapshot.path(),
        &directory,
        &source.credential_state,
        &source.database_key().unwrap(),
        &[9; 32],
        "backup-password",
        &archive,
        &source.backup,
        |phase, _, _| {
            if phase == "keys" {
                source.backup.request_cancel();
            }
        },
    );
    assert!(result.unwrap_err().contains("BACKUP_CANCELLED"));
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous valid backup");
    drop(_operation);
    let _operation = source.backup.begin();
    let conn = Connection::open(snapshot.path().join(DATABASE_FILE_NAME)).unwrap();
    connection::configure_sqlcipher_connection(&conn, &source.database_key().unwrap()).unwrap();
    conn.execute("CREATE TABLE future_secret(row_key BLOB)", [])
        .unwrap();
    drop(conn);
    assert!(write_portable(
        snapshot.path(),
        &directory,
        &source.credential_state,
        &source.database_key().unwrap(),
        &[9; 32],
        "backup-password",
        &archive,
        &source.backup,
        |_, _, _| {}
    )
    .unwrap_err()
    .contains("Unsupported backup key slot"));
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous valid backup");
    assert!(!std::fs::read_dir(root.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".partial")));
}

fn legacy_archive(source: &StorageState, target: &Path, parent: &Path) {
    let directory = source.data_dir.lock().unwrap().clone();
    let mode = source
        .prepare_database_snapshot_under_maintenance()
        .unwrap();
    source.shutdown_under_maintenance().unwrap();
    let snapshot = database_snapshot::create_database_snapshot(&directory, &mode, parent).unwrap();
    database_snapshot::copy_payload_tree(&directory, snapshot.path()).unwrap();
    let salt = "legacy-password-salt";
    let nonce = [7; 12];
    let mut key = [0; 32];
    Argon2::default()
        .hash_password_into(b"backup-password", salt.as_bytes(), &mut key)
        .unwrap();
    let master = Aes256Gcm::new_from_slice(&key)
        .unwrap()
        .encrypt(Nonce::from_slice(&nonce), &[9; 32][..])
        .unwrap();
    std::fs::write(
        snapshot
            .path()
            .join(database_snapshot::BACKUP_MASTER_KEY_FILE_NAME),
        master,
    )
    .unwrap();
    std::fs::write(
        snapshot
            .path()
            .join(database_snapshot::BACKUP_METADATA_FILE_NAME),
        serde_json::json!({"salt":salt,"nonce":hex::encode(nonce)}).to_string(),
    )
    .unwrap();
    database_snapshot::write_manifest(
        &snapshot.path().join(BACKUP_MANIFEST_FILE_NAME),
        &snapshot.manifest,
    )
    .unwrap();
    let mut zip = zip::ZipWriter::new(File::create(target).unwrap());
    for entry in walkdir::WalkDir::new(snapshot.path()).into_iter() {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let name = entry
                .path()
                .strip_prefix(snapshot.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            std::io::copy(&mut File::open(entry.path()).unwrap(), &mut zip).unwrap();
        }
    }
    zip.finish().unwrap();
}

#[test]
fn legacy_zip_requires_the_original_key_even_with_the_correct_password() {
    let root = TempDir::new().unwrap();
    let source = storage(&root.path().join("source"));
    populate(&source, false);
    let archive = root.path().join("legacy.zip");
    legacy_archive(&source, &archive, root.path());
    let other = storage(&root.path().join("other"));
    let _operation = other.backup.begin();
    assert!(prepare_import(
        &archive,
        root.path(),
        &other.credential_state,
        &other.database_key().unwrap(),
        "backup-password",
        &other.backup,
        |_, _, _| {}
    )
    .err()
    .unwrap()
    .contains("BACKUP_LEGACY_DEVICE_REQUIRED"));
    let _original_operation = source.backup.begin();
    let restored = prepare(&source, &archive, root.path());
    assert_eq!(&*restored.master_key, &[9; 32]);
}
