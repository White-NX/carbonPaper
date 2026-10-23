use super::*;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use base64::Engine;
use std::io::Write;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

const LEGACY_SQLCIPHER_3453_FIXTURE_BASE64: &str =
    include_str!("../fixtures/sqlcipher-3.45.3.db.b64");

fn write_legacy_sqlcipher_fixture(directory: &Path) -> PathBuf {
    let path = directory.join(DATABASE_FILE_NAME);
    let encoded = LEGACY_SQLCIPHER_3453_FIXTURE_BASE64
        .split_whitespace()
        .collect::<String>();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("decode legacy SQLCipher fixture");
    std::fs::write(&path, bytes).expect("write legacy SQLCipher fixture");
    path
}

fn create_encrypted_database(directory: &Path, key: &[u8]) -> Connection {
    std::fs::create_dir_all(directory).unwrap();
    let connection = Connection::open(directory.join(DATABASE_FILE_NAME)).unwrap();
    connection::configure_sqlcipher_connection(&connection, key).unwrap();
    connection
        .execute_batch("CREATE TABLE records(value TEXT); INSERT INTO records VALUES ('base');")
        .unwrap();
    connection
}

fn write_backup(path: &Path, entries: &[(&str, &[u8])], manifest: Option<&BackupManifest>) {
    let output = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(output);
    let options = SimpleFileOptions::default();
    zip.start_file(BACKUP_METADATA_FILE_NAME, options).unwrap();
    zip.write_all(br#"{"salt":"salt","nonce":"000000000000000000000000"}"#)
        .unwrap();
    zip.start_file(BACKUP_MASTER_KEY_FILE_NAME, options)
        .unwrap();
    zip.write_all(b"encrypted-key").unwrap();
    if let Some(manifest) = manifest {
        zip.start_file(BACKUP_MANIFEST_FILE_NAME, options).unwrap();
        zip.write_all(&serde_json::to_vec(manifest).unwrap())
            .unwrap();
    }
    for (name, contents) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

fn write_restore_credential(directory: &Path, contents: &[u8]) {
    std::fs::write(directory.join(MASTER_KEY_FILE_NAME), contents).unwrap();
}

#[test]
fn delete_snapshot_contains_only_database_file() {
    let source = tempdir().unwrap();
    std::fs::write(source.path().join(DATABASE_FILE_NAME), b"database").unwrap();
    std::fs::write(source.path().join(DATABASE_WAL_FILE_NAME), b"stale wal").unwrap();
    std::fs::write(source.path().join(DATABASE_SHM_FILE_NAME), b"stale shm").unwrap();
    let parent = tempdir().unwrap();

    let snapshot = create_database_snapshot(source.path(), "delete", parent.path()).unwrap();

    assert_eq!(snapshot.manifest.database_files.len(), 1);
    assert!(snapshot.path().join(DATABASE_FILE_NAME).exists());
    assert!(!snapshot.path().join(DATABASE_WAL_FILE_NAME).exists());
    assert!(!snapshot.path().join(DATABASE_SHM_FILE_NAME).exists());
}

#[test]
fn delete_snapshot_validates_and_reopens_encrypted_database() {
    let source = tempdir().unwrap();
    let key = [13u8; 32];
    let connection = create_encrypted_database(source.path(), &key);
    connection
        .execute("INSERT INTO records VALUES ('from delete')", [])
        .unwrap();
    drop(connection);
    let parent = tempdir().unwrap();

    let snapshot = create_database_snapshot(source.path(), "delete", parent.path()).unwrap();
    validate_database_snapshot(snapshot.path(), &snapshot.manifest, &key).unwrap();

    let copied = Connection::open(snapshot.path().join(DATABASE_FILE_NAME)).unwrap();
    let status = connection::configure_sqlcipher_connection(&copied, &key).unwrap();
    let values: Vec<String> = copied
        .prepare("SELECT value FROM records ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();

    assert_eq!(values, ["base", "from delete"]);
    assert_eq!(status.engine.sqlite_version, connection::MIN_SQLITE_VERSION);
    assert_eq!(status.journal_mode, "delete");
    assert_eq!(snapshot.manifest.database_files.len(), 1);
}

#[test]
fn legacy_sqlcipher_3453_database_survives_new_runtime_snapshot_and_reopen() {
    let source = tempdir().unwrap();
    let key = [0x3a; 32];
    let database = write_legacy_sqlcipher_fixture(source.path());

    let connection = Connection::open(&database).unwrap();
    let status = connection::configure_sqlcipher_connection(&connection, &key).unwrap();
    assert_eq!(status.engine.sqlite_version, connection::MIN_SQLITE_VERSION);
    assert_eq!(status.engine.cipher_version, "4.14.0 community");
    let legacy_value: String = connection
        .query_row(
            "SELECT value FROM legacy_records ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_value, "old-baseline");
    connection
        .execute(
            "INSERT INTO legacy_records(value) VALUES (?1)",
            ["new-baseline"],
        )
        .unwrap();
    drop(connection);

    let parent = tempdir().unwrap();
    let snapshot = create_database_snapshot(source.path(), "delete", parent.path()).unwrap();
    validate_database_snapshot(snapshot.path(), &snapshot.manifest, &key).unwrap();

    let reopened = Connection::open(snapshot.path().join(DATABASE_FILE_NAME)).unwrap();
    connection::configure_sqlcipher_connection(&reopened, &key).unwrap();
    let values: Vec<String> = reopened
        .prepare("SELECT value FROM legacy_records ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(values, ["old-baseline", "new-baseline"]);
}

#[test]
fn encrypted_delete_backup_archive_round_trips_through_restore_transaction() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let archive_path = root.path().join("backup.zip");
    let snapshot_parent = tempdir().unwrap();
    let extraction_parent = tempdir().unwrap();
    let database_key = [0x2au8; 32];
    let master_key = [0x71u8; 32];
    let password = b"archive-password";
    let salt = "carbonpaper-archive-test-salt";
    let nonce_bytes = [0x43u8; 12];

    let connection = create_encrypted_database(&data, &database_key);
    connection
        .execute("INSERT INTO records VALUES ('archive payload')", [])
        .unwrap();
    drop(connection);
    std::fs::create_dir_all(data.join("screenshots")).unwrap();
    std::fs::create_dir_all(data.join("chroma_db")).unwrap();
    std::fs::write(data.join("screenshots/record.enc"), b"encrypted screenshot").unwrap();
    std::fs::write(data.join("chroma_db/record.bin"), b"retired vector store").unwrap();
    write_restore_credential(&data, b"old wrapped credential");

    // Build the same four-part encrypted archive produced by the export
    // command: database snapshot, payload files, metadata, and wrapped
    // master key.  Keeping this in a unit test exercises the actual ZIP
    // extraction boundary rather than only testing synthetic entries.
    let snapshot = create_database_snapshot(&data, "delete", snapshot_parent.path()).unwrap();
    copy_payload_tree(&data, snapshot.path()).unwrap();
    // The retired vector store is not part of a backup anymore.
    assert!(!snapshot.path().join("chroma_db").exists());
    assert!(snapshot.path().join("screenshots/record.enc").exists());
    let mut derived_key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password, salt.as_bytes(), &mut derived_key)
        .unwrap();
    let cipher = Aes256Gcm::new_from_slice(&derived_key).unwrap();
    let encrypted_master_key = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), master_key.as_slice())
        .unwrap();
    std::fs::write(
        snapshot.path().join(BACKUP_METADATA_FILE_NAME),
        serde_json::json!({
            "salt": salt,
            "nonce": hex::encode(nonce_bytes),
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        snapshot.path().join(BACKUP_MASTER_KEY_FILE_NAME),
        encrypted_master_key,
    )
    .unwrap();
    write_manifest(
        &snapshot.path().join(BACKUP_MANIFEST_FILE_NAME),
        &snapshot.manifest,
    )
    .unwrap();

    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(snapshot.path())
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(snapshot.path()).unwrap();
            files.push((
                entry.path().to_path_buf(),
                relative.to_string_lossy().replace('\\', "/"),
            ));
        }
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    let output = File::create(&archive_path).unwrap();
    let mut zip = zip::ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (path, name) in files {
        zip.start_file(name, options).unwrap();
        let mut input = File::open(path).unwrap();
        std::io::copy(&mut input, &mut zip).unwrap();
    }
    zip.finish().unwrap();

    let extracted =
        extract_backup_archive(&archive_path, extraction_parent.path(), |_, _, _| {}).unwrap();
    assert!(!extracted.legacy);
    let metadata: serde_json::Value = serde_json::from_slice(&extracted.metadata).unwrap();
    let extracted_salt = metadata["salt"].as_str().unwrap();
    let extracted_nonce = hex::decode(metadata["nonce"].as_str().unwrap()).unwrap();
    let mut extracted_derived_key = [0u8; 32];
    Argon2::default()
        .hash_password_into(
            password,
            extracted_salt.as_bytes(),
            &mut extracted_derived_key,
        )
        .unwrap();
    let extracted_master_key = Aes256Gcm::new_from_slice(&extracted_derived_key)
        .unwrap()
        .decrypt(
            Nonce::from_slice(&extracted_nonce),
            extracted.encrypted_master_key.as_slice(),
        )
        .unwrap();
    assert_eq!(extracted_master_key, master_key);

    validate_database_snapshot(extracted.path(), &extracted.manifest, &database_key).unwrap();
    write_staged_file(
        &extracted.path().join(MASTER_KEY_FILE_NAME),
        b"new wrapped credential",
    )
    .unwrap();
    RestoreFileTransaction::install(&data, extracted.path())
        .unwrap()
        .commit()
        .unwrap();

    let reopened = Connection::open(data.join(DATABASE_FILE_NAME)).unwrap();
    connection::configure_sqlcipher_connection(&reopened, &database_key).unwrap();
    let values: Vec<String> = reopened
        .prepare("SELECT value FROM records ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(values, ["base", "archive payload"]);
    assert_eq!(
        std::fs::read(data.join("screenshots/record.enc")).unwrap(),
        b"encrypted screenshot"
    );
    assert_eq!(
        std::fs::read(data.join(MASTER_KEY_FILE_NAME)).unwrap(),
        b"new wrapped credential"
    );
}

#[test]
fn wal_snapshot_reopens_after_last_connection_closes() {
    let source = tempdir().unwrap();
    let key = [7u8; 32];
    let connection = create_encrypted_database(source.path(), &key);
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection::set_journal_mode(&connection, connection::JournalMode::Wal).unwrap();
    connection
        .execute("INSERT INTO records VALUES ('from wal')", [])
        .unwrap();
    assert!(source.path().join(DATABASE_WAL_FILE_NAME).exists());
    assert!(source.path().join(DATABASE_SHM_FILE_NAME).exists());
    connection::preserve_wal_sidecars_on_close(&connection).unwrap();
    drop(connection);
    assert!(source.path().join(DATABASE_WAL_FILE_NAME).exists());
    assert!(source.path().join(DATABASE_SHM_FILE_NAME).exists());
    let parent = tempdir().unwrap();

    let snapshot = create_database_snapshot(source.path(), "wal", parent.path()).unwrap();
    let before_validation = DATABASE_FILE_NAMES
        .iter()
        .map(|name| {
            (
                *name,
                std::fs::read(snapshot.path().join(name)).expect("snapshot member"),
            )
        })
        .collect::<Vec<_>>();
    validate_database_snapshot(snapshot.path(), &snapshot.manifest, &key).unwrap();
    for (name, expected) in before_validation {
        assert_eq!(std::fs::read(snapshot.path().join(name)).unwrap(), expected);
    }
    let copied = Connection::open(snapshot.path().join(DATABASE_FILE_NAME)).unwrap();
    connection::configure_sqlcipher_connection(&copied, &key).unwrap();
    let count: i64 = copied
        .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(snapshot.manifest.database_files.len(), 3);
}

#[test]
fn legacy_backup_without_manifest_is_delete_mode() {
    let root = tempdir().unwrap();
    let zip_path = root.path().join("legacy.zip");
    write_backup(&zip_path, &[(DATABASE_FILE_NAME, b"legacy")], None);

    let extracted = extract_backup_archive(&zip_path, root.path(), |_, _, _| {}).unwrap();

    assert!(extracted.legacy);
    assert_eq!(extracted.manifest.journal_mode, "delete");
    assert_eq!(extracted.manifest.database_files.len(), 1);
}

#[test]
fn manifest_hash_and_size_mismatches_are_rejected() {
    for (size, hash) in [
        (99, hex::encode(Sha256::digest(b"database"))),
        (8, "0".repeat(64)),
    ] {
        let root = tempdir().unwrap();
        let zip_path = root.path().join("bad.zip");
        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            journal_mode: "delete".into(),
            database_files: vec![BackupDatabaseFile {
                path: DATABASE_FILE_NAME.into(),
                size,
                sha256: hash,
            }],
        };
        write_backup(
            &zip_path,
            &[(DATABASE_FILE_NAME, b"database")],
            Some(&manifest),
        );
        assert!(extract_backup_archive(&zip_path, root.path(), |_, _, _| {}).is_err());
    }
}

#[test]
fn incomplete_wal_sidecars_are_rejected() {
    let root = tempdir().unwrap();
    let zip_path = root.path().join("incomplete.zip");
    let db_hash = hex::encode(Sha256::digest(b"database"));
    let wal_hash = hex::encode(Sha256::digest(b"wal"));
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        journal_mode: "wal".into(),
        database_files: vec![
            BackupDatabaseFile {
                path: DATABASE_FILE_NAME.into(),
                size: 8,
                sha256: db_hash,
            },
            BackupDatabaseFile {
                path: DATABASE_WAL_FILE_NAME.into(),
                size: 3,
                sha256: wal_hash,
            },
        ],
    };
    write_backup(
        &zip_path,
        &[
            (DATABASE_FILE_NAME, b"database"),
            (DATABASE_WAL_FILE_NAME, b"wal"),
        ],
        Some(&manifest),
    );

    let error = extract_backup_archive(&zip_path, root.path(), |_, _, _| {}).unwrap_err();
    assert!(error.contains("incomplete"), "{error}");
}

#[test]
fn unsafe_duplicate_and_illegal_database_entries_are_rejected() {
    for entries in [
        vec![("../screenshots.db", b"db".as_slice())],
        vec![
            ("screenshots/item.enc", b"one".as_slice()),
            ("screenshots/ITEM.enc", b"two".as_slice()),
        ],
        vec![("screenshots.db-journal", b"journal".as_slice())],
        vec![("screenshots/screenshots.db-journal", b"journal".as_slice())],
    ] {
        let root = tempdir().unwrap();
        let zip_path = root.path().join("unsafe.zip");
        write_backup(&zip_path, &entries, None);
        assert!(extract_backup_archive(&zip_path, root.path(), |_, _, _| {}).is_err());
    }
}

#[test]
fn corrupted_database_fails_validation() {
    let root = tempdir().unwrap();
    let key = [11u8; 32];
    let connection = create_encrypted_database(root.path(), &key);
    drop(connection);
    let database = root.path().join(DATABASE_FILE_NAME);
    let length = std::fs::metadata(&database).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&database)
        .unwrap()
        .set_len(length.saturating_sub(100))
        .unwrap();
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        journal_mode: "delete".into(),
        database_files: vec![fingerprint_file(&database).unwrap()],
    };

    assert!(validate_database_snapshot(root.path(), &manifest, &key).is_err());
}

#[test]
fn injected_restore_failure_restores_old_database_group_and_directories() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let staged = root.path().join("staged");
    std::fs::create_dir_all(data.join("screenshots")).unwrap();
    std::fs::create_dir_all(data.join("chroma_db")).unwrap();
    std::fs::create_dir_all(data.join("derived-indexes")).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    for name in DATABASE_FILE_NAMES {
        std::fs::write(data.join(name), format!("old-{name}")).unwrap();
        std::fs::write(staged.join(name), format!("new-{name}")).unwrap();
    }
    std::fs::write(
        data.join(DATABASE_JOURNAL_FILE_NAME),
        b"old rollback journal",
    )
    .unwrap();
    std::fs::write(data.join("screenshots/old.enc"), b"old image").unwrap();
    std::fs::write(data.join("chroma_db/old.bin"), b"old index").unwrap();
    std::fs::write(data.join("derived-indexes/old.cpdvec"), b"old ann").unwrap();
    std::fs::create_dir_all(staged.join("screenshots")).unwrap();
    std::fs::create_dir_all(staged.join("chroma_db")).unwrap();
    std::fs::write(staged.join("screenshots/new.enc"), b"new image").unwrap();
    std::fs::write(staged.join("chroma_db/new.bin"), b"new index").unwrap();
    write_restore_credential(&data, b"old credential");
    write_restore_credential(&staged, b"new credential");

    let error = RestoreFileTransaction::install_inner(&data, &staged, Some(2)).unwrap_err();
    assert!(error.contains("Injected"), "{error}");
    for name in DATABASE_FILE_NAMES {
        assert_eq!(
            std::fs::read_to_string(data.join(name)).unwrap(),
            format!("old-{name}")
        );
    }
    assert!(data.join("screenshots/old.enc").exists());
    assert!(data.join("chroma_db/old.bin").exists());
    assert!(data.join("derived-indexes/old.cpdvec").exists());
    assert!(!data.join("screenshots/new.enc").exists());
    assert_eq!(
        std::fs::read(data.join(DATABASE_JOURNAL_FILE_NAME)).unwrap(),
        b"old rollback journal"
    );
    assert_eq!(
        std::fs::read(data.join(MASTER_KEY_FILE_NAME)).unwrap(),
        b"old credential"
    );
}

#[test]
fn successful_restore_removes_stale_rollback_journal() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let staged = root.path().join("staged");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(data.join(DATABASE_FILE_NAME), b"old").unwrap();
    std::fs::write(data.join(DATABASE_JOURNAL_FILE_NAME), b"stale").unwrap();
    std::fs::write(staged.join(DATABASE_FILE_NAME), b"new").unwrap();
    write_restore_credential(&data, b"old credential");
    write_restore_credential(&staged, b"new credential");

    RestoreFileTransaction::install(&data, &staged)
        .unwrap()
        .commit()
        .unwrap();

    assert_eq!(
        std::fs::read(data.join(DATABASE_FILE_NAME)).unwrap(),
        b"new"
    );
    assert!(!data.join(DATABASE_JOURNAL_FILE_NAME).exists());
    assert_eq!(
        std::fs::read(data.join(MASTER_KEY_FILE_NAME)).unwrap(),
        b"new credential"
    );
}

#[test]
fn successful_restore_materializes_omitted_empty_screenshots_directory() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let staged = root.path().join("staged");
    std::fs::create_dir_all(data.join("screenshots")).unwrap();
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(data.join(DATABASE_FILE_NAME), b"old").unwrap();
    std::fs::write(data.join("screenshots/old.enc"), b"old image").unwrap();
    std::fs::write(staged.join(DATABASE_FILE_NAME), b"new").unwrap();
    write_restore_credential(&data, b"old credential");
    write_restore_credential(&staged, b"new credential");

    RestoreFileTransaction::install(&data, &staged)
        .unwrap()
        .commit()
        .unwrap();

    assert_eq!(
        std::fs::read(data.join(DATABASE_FILE_NAME)).unwrap(),
        b"new"
    );
    assert!(data.join("screenshots").is_dir());
    assert!(std::fs::read_dir(data.join("screenshots"))
        .unwrap()
        .next()
        .is_none());
    assert!(!data.join("screenshots/old.enc").exists());
}

#[test]
fn failed_commit_is_rolled_back_by_transaction_drop() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::install(&data, &staged).unwrap();
    std::fs::remove_file(data.join(DATABASE_FILE_NAME)).unwrap();
    assert!(transaction.commit().is_err());
    drop(transaction);

    assert_old_restore_fixture(&data);
    assert_no_restore_transaction(root.path());
}

fn create_interruption_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let data = root.join("data");
    let staged = root.join("staged");
    std::fs::create_dir_all(data.join("screenshots")).unwrap();
    std::fs::create_dir_all(data.join("chroma_db")).unwrap();
    std::fs::create_dir_all(staged.join("screenshots")).unwrap();
    std::fs::create_dir_all(staged.join("chroma_db")).unwrap();
    for name in DATABASE_FILE_NAMES {
        std::fs::write(data.join(name), format!("old-{name}")).unwrap();
        std::fs::write(staged.join(name), format!("new-{name}")).unwrap();
    }
    std::fs::write(data.join("screenshots/old.enc"), b"old image").unwrap();
    std::fs::write(data.join("chroma_db/old.bin"), b"old index").unwrap();
    std::fs::write(staged.join("screenshots/new.enc"), b"new image").unwrap();
    std::fs::write(staged.join("chroma_db/new.bin"), b"new index").unwrap();
    write_restore_credential(&data, b"old credential");
    write_restore_credential(&staged, b"new credential");
    (data, staged)
}

fn assert_old_restore_fixture(data: &Path) {
    for name in DATABASE_FILE_NAMES {
        assert_eq!(
            std::fs::read_to_string(data.join(name)).unwrap(),
            format!("old-{name}")
        );
    }
    assert!(data.join("screenshots/old.enc").exists());
    assert!(!data.join("screenshots/new.enc").exists());
    assert!(data.join("chroma_db/old.bin").exists());
    assert!(!data.join("chroma_db/new.bin").exists());
    assert_eq!(
        std::fs::read(data.join(MASTER_KEY_FILE_NAME)).unwrap(),
        b"old credential"
    );
}

fn assert_new_restore_fixture(data: &Path) {
    for name in DATABASE_FILE_NAMES {
        assert_eq!(
            std::fs::read_to_string(data.join(name)).unwrap(),
            format!("new-{name}")
        );
    }
    assert!(!data.join("screenshots/old.enc").exists());
    assert!(data.join("screenshots/new.enc").exists());
    assert!(!data.join("chroma_db/old.bin").exists());
    assert!(data.join("chroma_db/new.bin").exists());
    assert_eq!(
        std::fs::read(data.join(MASTER_KEY_FILE_NAME)).unwrap(),
        b"new credential"
    );
}

fn assert_no_restore_transaction(parent: &Path) {
    let scan = scan_restore_directories(parent).unwrap();
    assert!(scan.preparing.is_empty());
    assert!(scan.active.is_empty());
    assert!(scan.cleanup.is_empty());
    assert!(scan.legacy_rollback.is_empty());
    assert!(scan.legacy_recovery.is_empty());
}

#[test]
fn startup_recovery_rolls_back_interruption_while_moving_old_entries() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::prepare(&data, &staged).unwrap();
    assert!(transaction.move_old_entries(Some(2)).is_err());
    std::mem::forget(transaction);

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_old_restore_fixture(&data);
    assert_no_restore_transaction(root.path());
}

#[test]
fn startup_recovery_rolls_back_interruption_while_installing_new_entries() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::prepare(&data, &staged).unwrap();
    transaction.move_old_entries(None).unwrap();
    assert!(transaction.install_new_entries(Some(2)).is_err());
    std::mem::forget(transaction);

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_old_restore_fixture(&data);
    assert_no_restore_transaction(root.path());
}

#[test]
fn startup_recovery_rolls_back_fully_installed_uncommitted_restore() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::prepare(&data, &staged).unwrap();
    transaction.move_old_entries(None).unwrap();
    transaction.install_new_entries(None).unwrap();
    std::mem::forget(transaction);

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_old_restore_fixture(&data);
    assert_no_restore_transaction(root.path());
}

#[test]
fn startup_recovery_keeps_committed_restore_and_only_cleans_rollback() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::prepare(&data, &staged).unwrap();
    transaction.move_old_entries(None).unwrap();
    transaction.install_new_entries(None).unwrap();
    transaction.mark_committed().unwrap();
    std::mem::forget(transaction);

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_new_restore_fixture(&data);
    assert_no_restore_transaction(root.path());
}

#[test]
fn startup_recovery_never_rolls_back_a_committed_restore_after_runtime_changes() {
    let root = tempdir().unwrap();
    let (data, staged) = create_interruption_fixture(root.path());
    let mut transaction = RestoreFileTransaction::prepare(&data, &staged).unwrap();
    transaction.move_old_entries(None).unwrap();
    transaction.install_new_entries(None).unwrap();
    transaction.mark_committed().unwrap();
    std::fs::create_dir(data.join("derived-indexes")).unwrap();
    std::fs::write(
        data.join("derived-indexes/runtime.cpdvec"),
        b"runtime index",
    )
    .unwrap();
    std::mem::forget(transaction);

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_new_restore_fixture(&data);
    assert_eq!(
        std::fs::read(data.join("derived-indexes/runtime.cpdvec")).unwrap(),
        b"runtime index"
    );
    assert_no_restore_transaction(root.path());
}

#[test]
fn legacy_rollback_is_recovered_when_the_live_database_is_missing() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let rollback = root
        .path()
        .join(".carbonpaper-restore-rollback-legacy-test");
    std::fs::create_dir_all(data.join("screenshots")).unwrap();
    std::fs::create_dir_all(data.join("chroma_db")).unwrap();
    std::fs::create_dir_all(rollback.join("screenshots")).unwrap();
    std::fs::write(data.join("screenshots/new.enc"), b"partial new").unwrap();
    std::fs::write(data.join("chroma_db/new.bin"), b"partial new index").unwrap();
    std::fs::write(rollback.join(DATABASE_FILE_NAME), b"old database").unwrap();
    std::fs::write(rollback.join("screenshots/old.enc"), b"old image").unwrap();

    assert!(recover_interrupted_restore(&data).unwrap());

    assert_eq!(
        std::fs::read(data.join(DATABASE_FILE_NAME)).unwrap(),
        b"old database"
    );
    assert!(data.join("screenshots/old.enc").exists());
    assert!(!data.join("screenshots/new.enc").exists());
    assert!(!data.join("chroma_db").exists());
    assert_no_restore_transaction(root.path());
}

#[test]
fn initialized_storage_marker_prevents_silent_empty_database_creation() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    mark_storage_initialized(&data).unwrap();

    let error = ensure_database_creation_is_safe(&data).unwrap_err();

    assert!(error.contains("Refusing to create an empty database"));
}
