use super::*;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    destination: PathBuf,
    key: String,
    release: Release,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("package");
    let destination = root.path().join("application");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&destination).unwrap();
    let mut files = BTreeMap::new();
    for name in [
        "carbonpaper.exe",
        "carbonpaper-key-service.exe",
        HELPER_NAME,
        "carbonpaper-semantic-worker.exe",
        "carbonpaper-ml.exe",
        "carbonpaper-office.exe",
        "carbonpaper-nmh.exe",
    ] {
        fs::write(source.join(name), format!("new {name}")).unwrap();
        fs::write(destination.join(name), format!("old {name}")).unwrap();
        files.insert(
            name.into(),
            hex::encode(Sha256::digest(format!("new {name}"))),
        );
    }
    fs::write(destination.join("user-data.txt"), b"preserve this").unwrap();
    let manifest = ReleaseManifest {
        format: 1,
        product: manifest::PRODUCT.into(),
        version: "1.2.3".into(),
        architecture: "x86_64".into(),
        service_protocol: 1,
        files,
    };
    let signing_key = SigningKey::from_bytes(&[9; 32]);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let mut payload = manifest::SIGNING_CONTEXT.to_vec();
    payload.extend_from_slice(&bytes);
    let signature =
        base64::engine::general_purpose::STANDARD.encode(signing_key.sign(&payload).to_bytes());
    let key =
        base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key().to_bytes());
    fs::write(source.join(MANIFEST_NAME), &bytes).unwrap();
    fs::write(source.join(SIGNATURE_NAME), signature).unwrap();
    let release = read_release(&source, &key).unwrap();
    Fixture {
        _root: root,
        source,
        destination,
        key,
        release,
    }
}

#[test]
fn changed_source_is_rejected_before_any_installed_file_changes() {
    let fixture = fixture();
    fs::write(
        fixture.source.join("carbonpaper.exe"),
        b"changed after download",
    )
    .unwrap();
    assert!(Transaction::prepare(&fixture.source, &fixture.destination, &fixture.release).is_err());
    assert_eq!(
        fs::read(fixture.destination.join("carbonpaper.exe")).unwrap(),
        b"old carbonpaper.exe"
    );
    assert_eq!(
        fs::read(fixture.destination.join("user-data.txt")).unwrap(),
        b"preserve this"
    );
}

#[test]
fn prepared_bytes_and_parent_paths_stay_locked_through_commit() {
    let fixture = fixture();
    let write_destination_directory = || {
        OpenOptions::new()
            .access_mode(windows::Win32::Foundation::GENERIC_WRITE.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&fixture.destination)
    };
    drop(write_destination_directory().unwrap());
    fs::write(
        fixture.source.join("unsigned-extra.exe"),
        b"not in the manifest",
    )
    .unwrap();
    let mut transaction =
        Transaction::prepare(&fixture.source, &fixture.destination, &fixture.release).unwrap();
    let staged = transaction.staging.join("prepared/carbonpaper.exe");
    assert!(fs::write(&staged, b"replace prepared bytes").is_err());
    assert!(fs::rename(&staged, transaction.staging.join("renamed.exe")).is_err());
    assert!(fs::rename(&fixture.source, fixture._root.path().join("moved-package")).is_err());
    // Changing a source after preparation cannot change the retained objects.
    fs::write(fixture.source.join("carbonpaper.exe"), b"replacement").unwrap();
    transaction.commit().unwrap();
    let _execution_paths = transaction.execution_locks().unwrap();
    assert!(write_destination_directory().is_err());
    assert_eq!(
        fs::read(fixture.destination.join("carbonpaper.exe")).unwrap(),
        b"new carbonpaper.exe"
    );
    assert!(fs::write(
        fixture.destination.join("carbonpaper.exe"),
        b"replace installed bytes"
    )
    .is_err());
    assert!(!fixture.destination.join("unsigned-extra.exe").exists());
    assert_eq!(
        fs::read(fixture.destination.join("user-data.txt")).unwrap(),
        b"preserve this"
    );
    transaction.finish();
    drop(transaction);
    read_release(&fixture.destination, &fixture.key).unwrap();
}

#[test]
fn launch_validation_rejects_a_different_file_at_the_installed_path() {
    let fixture = fixture();
    let mut transaction =
        Transaction::prepare(&fixture.source, &fixture.destination, &fixture.release).unwrap();
    transaction.commit().unwrap();
    let replacement = transaction
        .replacements
        .iter()
        .find(|replacement| replacement.relative == Path::new("carbonpaper.exe"))
        .unwrap();
    // Simulate a namespace that no longer resolves to the retained object,
    // despite that object's verified bytes and handle remaining intact.
    rename_locked(
        &replacement.file,
        &transaction.staging.join("displaced.exe"),
        transaction.locks.directory(&transaction.staging).unwrap(),
    )
    .unwrap();
    fs::write(fixture.destination.join("carbonpaper.exe"), b"substituted").unwrap();
    assert!(transaction.execution_locks().is_err());
    transaction.rollback().unwrap();
    assert_eq!(
        fs::read(fixture.destination.join("carbonpaper.exe")).unwrap(),
        b"old carbonpaper.exe"
    );
}

#[test]
fn failed_replacement_rolls_back_files_already_installed() {
    let fixture = fixture();
    let blocked = fixture.destination.join("carbonpaper-ml.exe");
    fs::remove_file(&blocked).unwrap();
    fs::create_dir(&blocked).unwrap();
    let mut transaction =
        Transaction::prepare(&fixture.source, &fixture.destination, &fixture.release).unwrap();
    assert!(transaction.commit().is_err());
    transaction.rollback().unwrap();
    drop(transaction);
    assert_eq!(
        fs::read(fixture.destination.join("carbonpaper-key-service.exe")).unwrap(),
        b"old carbonpaper-key-service.exe"
    );
    assert_eq!(
        fs::read(fixture.destination.join("user-data.txt")).unwrap(),
        b"preserve this"
    );
    assert!(blocked.is_dir());
}

#[test]
fn an_unfinished_commit_restores_the_previous_application() {
    let fixture = fixture();
    let mut transaction =
        Transaction::prepare(&fixture.source, &fixture.destination, &fixture.release).unwrap();
    transaction.commit().unwrap();
    drop(transaction);
    assert_eq!(
        fs::read(fixture.destination.join("carbonpaper.exe")).unwrap(),
        b"old carbonpaper.exe"
    );
    assert_eq!(
        fs::read(fixture.destination.join("user-data.txt")).unwrap(),
        b"preserve this"
    );
}

#[test]
fn locked_verified_executable_can_be_renamed_and_started() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("prepared.exe");
    let target = root.path().join("installed.exe");
    fs::copy(std::env::current_exe().unwrap(), &source).unwrap();
    let mut locks = DirectoryLocks::default();
    locks.pin(root.path()).unwrap();
    let file = movable_file(&source).unwrap();
    rename_locked(&file, &target, locks.directory(root.path()).unwrap()).unwrap();
    let _execution_paths = DirectoryLocks::for_execution(root.path()).unwrap();
    let status = std::process::Command::new(&target)
        .arg("--list")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(fs::write(&target, b"changed").is_err());
    drop(file);
    drop(locks);
}
