//! Portable backup orchestration, independent of Tauri and the live database.

use super::{
    backup_format::{self, EncryptedReader, EncryptedWriter},
    backup_keys, connection,
    database_snapshot::{
        self, BackupManifest, DatabaseSnapshot, ExtractedBackup, TemporaryDirectory,
        BACKUP_MANIFEST_FILE_NAME, DATABASE_FILE_NAME,
    },
};
use crate::credential_manager::{CredentialManagerState, MASTER_KEY_FILE_NAME};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;

#[derive(Default)]
pub(crate) struct Control {
    cancellable: Mutex<bool>,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct Operation<'a>(&'a Control);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        *self.0.cancellable.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

impl Control {
    pub(crate) fn begin(&self) -> Operation<'_> {
        let mut cancellable = self.cancellable.lock().unwrap_or_else(|e| e.into_inner());
        self.cancelled.store(false, Ordering::Release);
        *cancellable = true;
        Operation(self)
    }
    pub(crate) fn request_cancel(&self) -> bool {
        let cancellable = self.cancellable.lock().unwrap_or_else(|e| e.into_inner());
        if !*cancellable {
            return false;
        }
        self.cancelled.store(true, Ordering::Release);
        true
    }
    fn flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        backup_format::check_cancelled(&self.cancelled).map_err(|e| e.to_string())
    }
    pub(crate) fn begin_commit(&self) -> Result<(), String> {
        // Serialize accepted cancellation with the irreversible commit boundary.
        let mut cancellable = self.cancellable.lock().unwrap_or_else(|e| e.into_inner());
        self.check()?;
        *cancellable = false;
        Ok(())
    }
}

struct PendingExport {
    path: PathBuf,
}
impl PendingExport {
    fn create(target: &Path) -> Result<(Self, File), String> {
        let parent = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let path = parent.join(format!(
            ".carbonpaper-backup-{}.partial",
            carbonpaper_app_bound::protocol::random_id()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        Ok((Self { path }, file))
    }
    fn publish(&self, target: &Path) -> Result<(), String> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::{
                core::PCWSTR,
                Win32::Storage::FileSystem::{
                    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
                },
            };
            let source: Vec<_> = self.path.as_os_str().encode_wide().chain(Some(0)).collect();
            let destination: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: both owned buffers are valid, NUL-terminated paths. The
            // completed temporary file is beside the destination on one volume.
            unsafe {
                MoveFileExW(
                    PCWSTR(source.as_ptr()),
                    PCWSTR(destination.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(|e| format!("Failed to publish backup: {e}"))
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&self.path, target).map_err(|e| e.to_string())
        }
    }
}
impl Drop for PendingExport {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn snapshot_connection(
    snapshot: &DatabaseSnapshot,
) -> Result<(TemporaryDirectory, Connection), String> {
    // Opening SQLite can change SHM read marks or checkpoint WALs. Enumerate
    // keys on a disposable clone, keeping the archive's manifest exact.
    let directory = TemporaryDirectory::create(
        snapshot.path().parent().unwrap_or(snapshot.path()),
        "carbonpaper-backup-keys",
    )?;
    database_snapshot::copy_database_group(
        snapshot.path(),
        directory.path(),
        &snapshot.manifest.journal_mode,
    )?;
    let conn = Connection::open_with_flags(
        directory.path().join(DATABASE_FILE_NAME),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    Ok((directory, conn))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Keeps archive inputs, wrapping keys, cancellation and progress explicit."
)]
pub(crate) fn write_portable<F: FnMut(&str, u64, u64)>(
    snapshot: &DatabaseSnapshot,
    data_directory: &Path,
    credentials: &CredentialManagerState,
    database_key: &[u8],
    master_key: &[u8],
    password: &str,
    target: &Path,
    control: &Control,
    mut progress: F,
) -> Result<(), String> {
    control.check()?;
    let (_key_directory, conn) = snapshot_connection(snapshot)?;
    connection::configure_sqlcipher_connection(&conn, database_key).map_err(|e| e.to_string())?;
    backup_keys::validate_source_images(&conn, data_directory, credentials, &control.cancelled)?;
    let mut files = snapshot
        .manifest
        .database_files
        .iter()
        .map(|file| (snapshot.path().join(&file.path), file.path.clone()))
        .collect::<Vec<_>>();
    let root = data_directory.join("screenshots");
    if root.exists() {
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.path() != root.join("thumbs"))
        {
            control.check()?;
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().is_symlink() {
                return Err("Symbolic links are not valid backup payloads".into());
            }
            if entry.file_type().is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(data_directory)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                database_snapshot::normalize_archive_name(&relative, false)?;
                files.push((entry.into_path(), relative));
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // An export must never replace its source database, credentials or images.
    let parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let source = std::fs::canonicalize(data_directory).map_err(|e| e.to_string())?;
    let parent_name = parent.to_string_lossy().replace('\\', "/").to_lowercase();
    let source_name = source.to_string_lossy().replace('\\', "/").to_lowercase();
    if parent_name.starts_with(&(source_name.clone() + "/")) || parent_name == source_name {
        return Err("BACKUP_EXPORT_TARGET_CONFLICT".into());
    }
    let (pending, output) = PendingExport::create(target)?;
    let encrypted =
        EncryptedWriter::new(output, password, control.flag()).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipWriter::new_stream(encrypted);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    archive
        .start_file(backup_keys::FILE_NAME, options)
        .map_err(|e| e.to_string())?;
    backup_keys::write_bundle(
        &conn,
        credentials,
        database_key,
        master_key,
        &mut archive,
        &control.cancelled,
        |done, total| progress("keys", done, total),
    )?;
    archive
        .start_file(BACKUP_MANIFEST_FILE_NAME, options)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut archive, &snapshot.manifest).map_err(|e| e.to_string())?;
    progress("files", 0, files.len() as u64);
    for (index, (path, name)) in files.iter().enumerate() {
        control.check()?;
        archive
            .start_file(name, options)
            .map_err(|e| e.to_string())?;
        let mut input = File::open(path).map_err(|e| e.to_string())?;
        std::io::copy(&mut input, &mut archive).map_err(|e| e.to_string())?;
        progress("files", (index + 1) as u64, files.len() as u64);
    }
    let output = archive
        .finish()
        .map_err(|e| e.to_string())?
        .into_inner()
        .finish()
        .map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    control.begin_commit()?;
    pending.publish(target)
}

struct CancellableFile {
    file: File,
    cancelled: Arc<AtomicBool>,
}
impl Read for CancellableFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        backup_format::check_cancelled(&self.cancelled)?;
        self.file.read(buffer)
    }
}
impl Seek for CancellableFile {
    fn seek(&mut self, offset: SeekFrom) -> std::io::Result<u64> {
        backup_format::check_cancelled(&self.cancelled)?;
        self.file.seek(offset)
    }
}

pub(crate) struct PreparedBackup {
    pub(crate) extracted: ExtractedBackup,
    pub(crate) master_key: Zeroizing<Vec<u8>>,
}

fn legacy_master_key(
    extracted: &ExtractedBackup,
    password: &str,
) -> Result<Zeroizing<Vec<u8>>, String> {
    if password.is_empty() {
        return Err("BACKUP_PASSWORD_EMPTY".into());
    }
    let metadata: serde_json::Value =
        serde_json::from_slice(&extracted.metadata).map_err(|e| e.to_string())?;
    let salt = metadata["salt"]
        .as_str()
        .ok_or("Invalid legacy backup salt")?;
    let nonce = hex::decode(
        metadata["nonce"]
            .as_str()
            .ok_or("Invalid legacy backup nonce")?,
    )
    .map_err(|e| e.to_string())?;
    if nonce.len() != 12 || salt.len() > 128 {
        return Err("Invalid legacy backup metadata".into());
    }
    let mut key = Zeroizing::new([0; 32]);
    // Keep the former base64-text salt interpretation for existing archives.
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt.as_bytes(), &mut *key)
        .map_err(|e| e.to_string())?;
    let master = Zeroizing::new(
        Aes256Gcm::new_from_slice(&*key)
            .unwrap()
            .decrypt(Nonce::from_slice(&nonce), &*extracted.encrypted_master_key)
            .map_err(|_| "BACKUP_INCORRECT_PASSWORD_OR_CORRUPT")?,
    );
    if master.len() != 32 {
        return Err("Invalid legacy backup master key length".into());
    }
    Ok(master)
}

pub(crate) fn prepare_import<F: FnMut(&str, u64, u64)>(
    path: &Path,
    temporary_parent: &Path,
    credentials: &CredentialManagerState,
    local_database_key: &[u8],
    password: &str,
    control: &Control,
    mut progress: F,
) -> Result<PreparedBackup, String> {
    control.check()?;
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut magic = [0; 8];
    file.read_exact(&mut magic).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let portable = &magic == backup_format::MAGIC;
    progress("verify", 0, 0);
    let mut extracted = if portable {
        let reader =
            EncryptedReader::open(file, password, control.flag()).map_err(|e| e.to_string())?;
        database_snapshot::extract_backup_reader(
            reader,
            temporary_parent,
            true,
            |done, total, _| progress("files", done as u64, total as u64),
        )?
    } else if magic.starts_with(b"PK") {
        database_snapshot::extract_backup_reader(
            CancellableFile {
                file,
                cancelled: control.flag(),
            },
            temporary_parent,
            false,
            |done, total, _| progress("files", done as u64, total as u64),
        )?
    } else {
        return Err("BACKUP_UNSUPPORTED_FORMAT".into());
    };
    let legacy_master = if portable {
        None
    } else {
        Some(legacy_master_key(&extracted, password)?)
    };
    let portable_keys = if portable {
        extracted
            .portable_keys
            .take()
            .ok_or("Portable backup keys are missing")?
    } else {
        database_snapshot::validate_database_snapshot(
            extracted.path(),
            &extracted.manifest,
            local_database_key,
        )
        .map_err(|_| "BACKUP_LEGACY_DEVICE_REQUIRED")?;
        let conn = Connection::open(extracted.path().join(DATABASE_FILE_NAME))
            .map_err(|e| e.to_string())?;
        connection::configure_sqlcipher_connection(&conn, local_database_key)
            .map_err(|_| "BACKUP_LEGACY_DEVICE_REQUIRED")?;
        let mut bundle = Zeroizing::new(Vec::new());
        backup_keys::write_bundle(
            &conn,
            credentials,
            local_database_key,
            legacy_master.as_ref().unwrap(),
            &mut *bundle,
            &control.cancelled,
            |done, total| progress("keys", done, total),
        )
        .map_err(|error| {
            if error.contains("AUTH_REQUIRED") || error.contains("BACKUP_CANCELLED") {
                error
            } else {
                "BACKUP_LEGACY_DEVICE_REQUIRED".into()
            }
        })?;
        // Opening the extracted legacy DB may checkpoint it. The old manifest
        // was validated already; its new fingerprint is produced below.
        bundle
    };
    let bundle = backup_keys::Bundle::parse(&portable_keys)?;
    if portable {
        database_snapshot::validate_database_snapshot(
            extracted.path(),
            &extracted.manifest,
            bundle.database_key(),
        )?;
    }
    let master_key = Zeroizing::new(bundle.master_key().to_vec());
    let conn =
        Connection::open(extracted.path().join(DATABASE_FILE_NAME)).map_err(|e| e.to_string())?;
    connection::configure_sqlcipher_connection(&conn, bundle.database_key())
        .map_err(|e| e.to_string())?;
    backup_keys::rewrap(
        &conn,
        &bundle,
        credentials,
        extracted.path(),
        &control.cancelled,
        |done, total| progress("restore", done, total),
    )?;
    control.check()?;
    progress("finalize", 0, 0);
    let checkpoint: (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|e| e.to_string())?;
    if checkpoint.0 != 0 {
        return Err("Restored database checkpoint is busy".into());
    }
    let mode: String = conn
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err("Restored database could not leave WAL mode".into());
    }
    conn.execute_batch(&format!(
        "PRAGMA rekey = \"x'{}'\";",
        hex::encode(local_database_key)
    ))
    .map_err(|e| e.to_string())?;
    drop(conn);
    extracted.manifest = BackupManifest {
        format_version: database_snapshot::BACKUP_FORMAT_VERSION,
        journal_mode: "delete".into(),
        database_files: vec![database_snapshot::fingerprint_file(
            &extracted.path().join(DATABASE_FILE_NAME),
        )?],
    };
    database_snapshot::validate_database_snapshot(
        extracted.path(),
        &extracted.manifest,
        local_database_key,
    )?;
    let credential_file = credentials
        .prepare_import_master_key_file(&master_key)
        .map_err(|e| e.to_string())?;
    database_snapshot::write_staged_file(
        &extracted.path().join(MASTER_KEY_FILE_NAME),
        &credential_file,
    )?;
    control.check()?;
    Ok(PreparedBackup {
        extracted,
        master_key,
    })
}

/// Caller has quiesced producers and owns database/derived maintenance guards.
pub(crate) fn install_prepared(
    state: &super::StorageState,
    prepared: &PreparedBackup,
) -> Result<(), String> {
    install_prepared_inner(state, prepared, || state.initialize_under_maintenance())
}

fn install_prepared_inner(
    state: &super::StorageState,
    prepared: &PreparedBackup,
    initialize: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let directory = state
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let database_key = state.database_key()?;
    // Revalidate the staged bytes immediately before replacing live files.
    database_snapshot::validate_database_snapshot(
        prepared.extracted.path(),
        &prepared.extracted.manifest,
        &database_key,
    )?;
    state.backup.begin_commit()?;
    let credential_snapshot = state.credential_state.snapshot_import_state();
    let result = (|| {
        state.processing_stage.retire_dataset()?;
        state.shutdown_under_maintenance()?;
        let mut transaction = database_snapshot::RestoreFileTransaction::install(
            &directory,
            prepared.extracted.path(),
        )?;
        let apply = (|| {
            state
                .credential_state
                .activate_imported_master_key(&prepared.master_key)
                .map_err(|e| e.to_string())?;
            initialize()?;
            transaction.mark_committed()?;
            Ok::<(), String>(())
        })();
        if let Err(error) = apply {
            let retire_import = state.processing_stage.retire_dataset();
            let _ = state.shutdown_under_maintenance();
            let rollback = transaction.rollback();
            let mut failures = vec![error];
            if let Err(error) = rollback {
                failures.push(format!("File rollback failed: {error}"));
            }
            if let Err(error) = retire_import {
                failures.push(format!("Imported dataset retirement failed: {error}"));
            }
            return Err(failures.join("; "));
        }
        // Once the durable commit marker exists, credentials and files remain
        // together. Obsolete transaction cleanup can be retried at startup.
        transaction.commit()
    })();
    if let Err(error) = result {
        state
            .credential_state
            .restore_import_state(credential_snapshot);
        let mut failures = vec![error];
        // Service retirement cannot be undone. Renew only processing authority;
        // the previous archive's content, master key and content identities stay.
        // Retirement itself can partially fail; reopen even if the primary
        // connection was still live so its cached processing identity is reset.
        if let Err(error) = state.shutdown_under_maintenance() {
            failures.push(format!("Previous database close failed: {error}"));
            return Err(failures.join("; "));
        }
        if let Err(error) = backup_keys::reset_local_authority(&directory, &database_key) {
            failures.push(format!(
                "Previous processing identity recovery failed: {error}"
            ));
            // Keep ambiguous files closed until the durable rollback can
            // finish; opening them now could combine old credentials with
            // partially installed content.
            return Err(failures.join("; "));
        }
        if let Err(error) = state.initialize_under_maintenance() {
            failures.push(format!("Previous database reopen failed: {error}"));
        }
        Err(failures.join("; "))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
