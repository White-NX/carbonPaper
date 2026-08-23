//! Consistent database-file snapshots and backup archive validation.
//!
//! SQLite's WAL state is a file group, not a single file.  This module keeps
//! `screenshots.db`, `screenshots.db-wal`, and `screenshots.db-shm` together,
//! fingerprints copied files, and provides the strict extraction boundary used
//! by backup restore and data-directory migration.

use super::connection;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DATABASE_FILE_NAME: &str = "screenshots.db";
pub(crate) const DATABASE_WAL_FILE_NAME: &str = "screenshots.db-wal";
pub(crate) const DATABASE_SHM_FILE_NAME: &str = "screenshots.db-shm";
pub(crate) const DATABASE_JOURNAL_FILE_NAME: &str = "screenshots.db-journal";
pub(crate) const BACKUP_MANIFEST_FILE_NAME: &str = "backup_manifest.json";
pub(crate) const BACKUP_METADATA_FILE_NAME: &str = "metadata.json";
pub(crate) const BACKUP_MASTER_KEY_FILE_NAME: &str = "master_key.enc";
pub(crate) const DATABASE_FILE_NAMES: [&str; 3] = [
    DATABASE_FILE_NAME,
    DATABASE_WAL_FILE_NAME,
    DATABASE_SHM_FILE_NAME,
];
pub(crate) const DATABASE_RUNTIME_FILE_NAMES: [&str; 4] = [
    DATABASE_FILE_NAME,
    DATABASE_WAL_FILE_NAME,
    DATABASE_SHM_FILE_NAME,
    DATABASE_JOURNAL_FILE_NAME,
];

const BACKUP_FORMAT_VERSION: u32 = 2;
const RESTORE_ENTRY_NAMES: [&str; 7] = [
    DATABASE_FILE_NAME,
    DATABASE_WAL_FILE_NAME,
    DATABASE_SHM_FILE_NAME,
    DATABASE_JOURNAL_FILE_NAME,
    "chroma_db",
    "screenshots",
    "derived-indexes",
];

static NEXT_TEMP_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotJournalMode {
    Delete,
    Wal,
}

impl SnapshotJournalMode {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "delete" => Ok(Self::Delete),
            "wal" => Ok(Self::Wal),
            other => Err(format!(
                "Unsupported database journal mode in backup: {other}"
            )),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Wal => "wal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupDatabaseFile {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupManifest {
    pub(crate) format_version: u32,
    pub(crate) journal_mode: String,
    pub(crate) database_files: Vec<BackupDatabaseFile>,
}

impl BackupManifest {
    pub(crate) fn mode(&self) -> Result<SnapshotJournalMode, String> {
        if self.format_version != BACKUP_FORMAT_VERSION {
            return Err(format!(
                "Unsupported backup format version: {}",
                self.format_version
            ));
        }
        SnapshotJournalMode::parse(&self.journal_mode)
    }
}

/// A unique temporary directory that is removed unless ownership is released.
#[derive(Debug)]
pub(crate) struct TemporaryDirectory {
    path: PathBuf,
    cleanup: bool,
}

impl TemporaryDirectory {
    pub(crate) fn create(parent: &Path, prefix: &str) -> Result<Self, String> {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create temporary-directory parent {}: {error}",
                parent.display()
            )
        })?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..128 {
            let sequence = NEXT_TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{prefix}-{}-{timestamp:x}-{sequence:x}",
                std::process::id()
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    return Ok(Self {
                        path: candidate,
                        cleanup: true,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Failed to create temporary directory {}: {error}",
                        candidate.display()
                    ))
                }
            }
        }

        Err(format!(
            "Failed to allocate a unique temporary directory beside {}",
            parent.display()
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn release(mut self) -> PathBuf {
        self.cleanup = false;
        self.path.clone()
    }

    pub(crate) fn remove(mut self) -> Result<(), String> {
        self.cleanup = false;
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).map_err(|error| {
                format!(
                    "Failed to remove temporary directory {}: {error}",
                    self.path.display()
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if self.cleanup && self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
pub(crate) struct DatabaseSnapshot {
    directory: TemporaryDirectory,
    pub(crate) manifest: BackupManifest,
}

impl DatabaseSnapshot {
    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
}

#[derive(Debug)]
pub(crate) struct ExtractedBackup {
    directory: TemporaryDirectory,
    pub(crate) manifest: BackupManifest,
    pub(crate) legacy: bool,
    pub(crate) metadata: Vec<u8>,
    pub(crate) encrypted_master_key: Vec<u8>,
}

impl ExtractedBackup {
    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("Failed to open {} for hashing: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Failed to hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn fingerprint_file(path: &Path) -> Result<BackupDatabaseFile, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Snapshot file must not be a symbolic link: {}",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Database snapshot entry is not a file: {}",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid database file name: {}", path.display()))?;
    Ok(BackupDatabaseFile {
        path: name.to_string(),
        size: metadata.len(),
        sha256: sha256_file(path)?,
    })
}

pub(crate) fn copy_file_verified(source: &Path, target: &Path) -> Result<u64, String> {
    let source_metadata = std::fs::symlink_metadata(source).map_err(|error| {
        format!(
            "Failed to inspect source file {}: {error}",
            source.display()
        )
    })?;
    if source_metadata.file_type().is_symlink() {
        return Err(format!(
            "Cannot copy symbolic link as a data file: {}",
            source.display()
        ));
    }
    if !source_metadata.is_file() {
        return Err(format!(
            "Source is not a regular file: {}",
            source.display()
        ));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create directory {}: {error}", parent.display()))?;
    }
    std::fs::copy(source, target).map_err(|error| {
        format!(
            "Failed to copy {} to {}: {error}",
            source.display(),
            target.display()
        )
    })?;

    let source_size = source_metadata.len();
    let target_metadata = std::fs::symlink_metadata(target)
        .map_err(|error| format!("Failed to inspect {}: {error}", target.display()))?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_file() {
        return Err(format!(
            "Copied target is not a regular file: {}",
            target.display()
        ));
    }
    let target_size = target_metadata.len();
    if source_size != target_size {
        return Err(format!(
            "Copied file size mismatch for {}: expected {source_size}, got {target_size}",
            source.display()
        ));
    }
    let source_hash = sha256_file(source)?;
    let target_hash = sha256_file(target)?;
    if source_hash != target_hash {
        return Err(format!(
            "Copied file hash mismatch for {}",
            source.display()
        ));
    }
    Ok(target_size)
}

fn database_names_for_snapshot(
    directory: &Path,
    mode: SnapshotJournalMode,
) -> Result<Vec<&'static str>, String> {
    let database = directory.join(DATABASE_FILE_NAME);
    let database_metadata = std::fs::symlink_metadata(&database).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("Database file is missing: {}", database.display())
        } else {
            format!(
                "Failed to inspect database file {}: {error}",
                database.display()
            )
        }
    })?;
    if database_metadata.file_type().is_symlink() {
        return Err(format!(
            "Database snapshot entry must not be a symbolic link: {}",
            database.display()
        ));
    }
    if !database_metadata.is_file() {
        return Err(format!("Database file is missing: {}", database.display()));
    }

    let sidecar_exists = |name: &str| -> Result<bool, String> {
        let path = directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
                "Database snapshot entry must not be a symbolic link: {}",
                path.display()
            )),
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(format!(
                "Database snapshot entry is not a file: {}",
                path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!(
                "Failed to inspect database snapshot entry {}: {error}",
                path.display()
            )),
        }
    };
    let wal_exists = sidecar_exists(DATABASE_WAL_FILE_NAME)?;
    let shm_exists = sidecar_exists(DATABASE_SHM_FILE_NAME)?;
    if wal_exists != shm_exists {
        return Err("Database WAL sidecars are incomplete; both -wal and -shm are required".into());
    }

    let mut names = vec![DATABASE_FILE_NAME];
    if mode == SnapshotJournalMode::Wal && wal_exists {
        names.push(DATABASE_WAL_FILE_NAME);
        names.push(DATABASE_SHM_FILE_NAME);
    }
    Ok(names)
}

pub(crate) fn copy_database_group(
    source_directory: &Path,
    target_directory: &Path,
    journal_mode: &str,
) -> Result<BackupManifest, String> {
    let mode = SnapshotJournalMode::parse(journal_mode)?;
    std::fs::create_dir_all(target_directory).map_err(|error| {
        format!(
            "Failed to create database snapshot directory {}: {error}",
            target_directory.display()
        )
    })?;

    let names = database_names_for_snapshot(source_directory, mode)?;
    let mut database_files = Vec::with_capacity(names.len());
    for name in names {
        let source = source_directory.join(name);
        let target = target_directory.join(name);
        copy_file_verified(&source, &target)?;
        database_files.push(fingerprint_file(&target)?);
    }

    Ok(BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        journal_mode: mode.as_str().to_string(),
        database_files,
    })
}

pub(crate) fn create_database_snapshot(
    source_directory: &Path,
    journal_mode: &str,
    temporary_parent: &Path,
) -> Result<DatabaseSnapshot, String> {
    let directory = TemporaryDirectory::create(temporary_parent, "carbonpaper-db-snapshot")?;
    let manifest = copy_database_group(source_directory, directory.path(), journal_mode)?;
    Ok(DatabaseSnapshot {
        directory,
        manifest,
    })
}

/// Copy the user-facing payload directories into a staging tree.  Database
/// files are intentionally excluded because they must come from
/// `copy_database_group`; thumbnail and derived-index caches are rebuildable.
pub(crate) fn copy_payload_tree(
    source_directory: &Path,
    target_directory: &Path,
) -> Result<Vec<PathBuf>, String> {
    let mut copied = Vec::new();
    for root_name in ["chroma_db", "screenshots"] {
        let source_root = source_directory.join(root_name);
        if !source_root.exists() {
            continue;
        }
        let thumbnail_root = source_directory.join("screenshots").join("thumbs");
        for entry in walkdir::WalkDir::new(&source_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.path() != thumbnail_root)
        {
            let entry = entry.map_err(|error| {
                format!(
                    "Failed to scan backup payload {}: {error}",
                    source_root.display()
                )
            })?;
            if entry.file_type().is_symlink() {
                return Err(format!(
                    "Symbolic links are not supported in backup payloads: {}",
                    entry.path().display()
                ));
            }
            let relative = entry
                .path()
                .strip_prefix(source_directory)
                .map_err(|error| format!("Failed to compute backup path: {error}"))?;
            let target = target_directory.join(relative);
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(&target).map_err(|error| {
                    format!(
                        "Failed to create backup directory {}: {error}",
                        target.display()
                    )
                })?;
            } else if entry.file_type().is_file() {
                copy_file_verified(entry.path(), &target)?;
                copied.push(target);
            }
        }
    }
    Ok(copied)
}

pub(crate) fn copy_directory_tree<F, P>(
    source_directory: &Path,
    target_directory: &Path,
    mut skip: F,
    mut progress: P,
) -> Result<usize, String>
where
    F: FnMut(&Path, bool) -> bool,
    P: FnMut(usize, &Path) -> Result<(), String>,
{
    let mut copied = 0usize;
    for entry in walkdir::WalkDir::new(source_directory)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry.map_err(|error| format!("Failed to scan data directory: {error}"))?;
        let relative = entry
            .path()
            .strip_prefix(source_directory)
            .map_err(|error| format!("Failed to compute migration path: {error}"))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if skip(relative, entry.file_type().is_dir()) {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(format!(
                "Symbolic links are not supported in the data directory: {}",
                entry.path().display()
            ));
        }
        let target = target_directory.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| {
                format!(
                    "Failed to create migration directory {}: {error}",
                    target.display()
                )
            })?;
        } else if entry.file_type().is_file() {
            copy_file_verified(entry.path(), &target)?;
            copied += 1;
            progress(copied, relative)?;
        }
    }
    Ok(copied)
}

pub(crate) fn write_manifest(path: &Path, manifest: &BackupManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("Failed to serialize backup manifest: {error}"))?;
    std::fs::write(path, bytes).map_err(|error| {
        format!(
            "Failed to write backup manifest {}: {error}",
            path.display()
        )
    })
}

fn normalize_archive_name(raw_name: &str, is_directory: bool) -> Result<String, String> {
    if raw_name.is_empty() || raw_name.contains('\0') || raw_name.contains('\\') {
        return Err(format!("Unsafe backup entry path: {raw_name:?}"));
    }
    let trimmed = if is_directory {
        raw_name.strip_suffix('/').unwrap_or(raw_name)
    } else {
        raw_name
    };
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return Err(format!("Unsafe backup entry path: {raw_name:?}"));
    }

    let path = Path::new(trimmed);
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value
                    .to_str()
                    .ok_or_else(|| format!("Backup entry path is not UTF-8: {raw_name:?}"))?;
                if value.is_empty()
                    || value == "."
                    || value == ".."
                    || value.contains(':')
                    || value.ends_with(' ')
                    || value.ends_with('.')
                {
                    return Err(format!("Unsafe backup entry path: {raw_name:?}"));
                }
                normalized.push(value);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(format!("Unsafe backup entry path: {raw_name:?}"));
            }
        }
    }
    if normalized.is_empty() {
        return Err(format!("Unsafe backup entry path: {raw_name:?}"));
    }
    Ok(normalized.join("/"))
}

fn validate_archive_payload_path(name: &str) -> Result<(), String> {
    if matches!(
        name,
        BACKUP_METADATA_FILE_NAME | BACKUP_MASTER_KEY_FILE_NAME | BACKUP_MANIFEST_FILE_NAME
    ) || DATABASE_FILE_NAMES.contains(&name)
    {
        return Ok(());
    }

    let file_name = name.rsplit('/').next().unwrap_or(name);
    if file_name.starts_with(DATABASE_FILE_NAME) {
        return Err(format!("Illegal database sidecar in backup: {name}"));
    }

    if name == "chroma_db"
        || name.starts_with("chroma_db/")
        || name == "screenshots"
        || name.starts_with("screenshots/")
    {
        if name == "screenshots/thumbs" || name.starts_with("screenshots/thumbs/") {
            return Err("Derived screenshot thumbnails are not valid backup payloads".into());
        }
        return Ok(());
    }
    Err(format!("Unsupported backup entry: {name}"))
}

fn is_zip_symlink(file: &zip::read::ZipFile<'_>) -> bool {
    file.unix_mode()
        .map(|mode| mode & 0o170000 == 0o120000)
        .unwrap_or(false)
}

fn read_required_file(root: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = root.join(name);
    std::fs::read(&path).map_err(|error| format!("{name} missing or unreadable: {error}"))
}

fn actual_database_file_map(root: &Path) -> Result<BTreeMap<String, BackupDatabaseFile>, String> {
    let mut files = BTreeMap::new();
    for name in DATABASE_FILE_NAMES {
        let path = root.join(name);
        if path.exists() {
            if !path.is_file() {
                return Err(format!("Database backup entry is not a file: {name}"));
            }
            files.insert(name.to_string(), fingerprint_file(&path)?);
        }
    }
    Ok(files)
}

fn validate_database_file_set(
    manifest: &BackupManifest,
    actual: &BTreeMap<String, BackupDatabaseFile>,
) -> Result<(), String> {
    let mode = manifest.mode()?;
    if !actual.contains_key(DATABASE_FILE_NAME) {
        return Err("screenshots.db missing in backup".into());
    }
    let wal_exists = actual.contains_key(DATABASE_WAL_FILE_NAME);
    let shm_exists = actual.contains_key(DATABASE_SHM_FILE_NAME);
    if wal_exists != shm_exists {
        return Err("Database WAL sidecars are incomplete; both -wal and -shm are required".into());
    }
    if mode == SnapshotJournalMode::Delete && (wal_exists || shm_exists) {
        return Err("DELETE-mode backup must not contain WAL sidecars".into());
    }

    let mut declared = BTreeMap::new();
    for file in &manifest.database_files {
        if !DATABASE_FILE_NAMES.contains(&file.path.as_str()) {
            return Err(format!(
                "Manifest contains an illegal database file: {}",
                file.path
            ));
        }
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "Manifest contains an invalid SHA-256 for {}",
                file.path
            ));
        }
        if declared.insert(file.path.clone(), file).is_some() {
            return Err(format!("Manifest contains duplicate file: {}", file.path));
        }
    }

    let declared_names: BTreeSet<_> = declared.keys().cloned().collect();
    let actual_names: BTreeSet<_> = actual.keys().cloned().collect();
    if declared_names != actual_names {
        return Err(format!(
            "Backup manifest database file list does not match archive contents (manifest={declared_names:?}, archive={actual_names:?})"
        ));
    }

    for (name, expected) in declared {
        let observed = actual
            .get(&name)
            .ok_or_else(|| format!("Database file missing after extraction: {name}"))?;
        if expected.size != observed.size {
            return Err(format!(
                "Database file size mismatch for {name}: expected {}, got {}",
                expected.size, observed.size
            ));
        }
        if !expected.sha256.eq_ignore_ascii_case(&observed.sha256) {
            return Err(format!("Database file SHA-256 mismatch for {name}"));
        }
    }
    Ok(())
}

pub(crate) fn extract_backup_archive<F>(
    archive_path: &Path,
    temporary_parent: &Path,
    mut progress: F,
) -> Result<ExtractedBackup, String>
where
    F: FnMut(usize, usize, &str),
{
    let archive_file = File::open(archive_path).map_err(|error| {
        format!(
            "Failed to open backup file {}: {error}",
            archive_path.display()
        )
    })?;
    let mut archive =
        zip::ZipArchive::new(archive_file).map_err(|error| format!("Invalid ZIP: {error}"))?;
    let directory = TemporaryDirectory::create(temporary_parent, "carbonpaper-restore-staging")?;
    let total = archive.len();
    let mut names = HashSet::new();

    for index in 0..total {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Failed to read ZIP entry {index}: {error}"))?;
        if is_zip_symlink(&entry) {
            return Err(format!(
                "Symbolic links are not allowed in backups: {}",
                entry.name()
            ));
        }
        let is_directory = entry.is_dir() || entry.name().ends_with('/');
        let name = normalize_archive_name(entry.name(), is_directory)?;
        validate_archive_payload_path(&name)?;
        let duplicate_key = name.to_ascii_lowercase();
        if !names.insert(duplicate_key) {
            return Err(format!("Duplicate backup entry: {name}"));
        }

        let output = directory.path().join(Path::new(&name));
        if is_directory {
            std::fs::create_dir_all(&output).map_err(|error| {
                format!(
                    "Failed to create extracted directory {}: {error}",
                    output.display()
                )
            })?;
        } else {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "Failed to create extracted directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
            let mut output_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)
                .map_err(|error| {
                    format!(
                        "Failed to create extracted file {}: {error}",
                        output.display()
                    )
                })?;
            std::io::copy(&mut entry, &mut output_file)
                .map_err(|error| format!("Failed to extract {}: {error}", output.display()))?;
            output_file.flush().map_err(|error| {
                format!(
                    "Failed to flush extracted file {}: {error}",
                    output.display()
                )
            })?;
        }
        progress(index + 1, total, &name);
    }

    let metadata = read_required_file(directory.path(), BACKUP_METADATA_FILE_NAME)?;
    let encrypted_master_key = read_required_file(directory.path(), BACKUP_MASTER_KEY_FILE_NAME)?;
    let actual = actual_database_file_map(directory.path())?;
    let manifest_path = directory.path().join(BACKUP_MANIFEST_FILE_NAME);
    let (manifest, legacy) = if manifest_path.exists() {
        let raw = std::fs::read(&manifest_path)
            .map_err(|error| format!("Failed to read backup manifest: {error}"))?;
        let manifest: BackupManifest = serde_json::from_slice(&raw)
            .map_err(|error| format!("Invalid backup manifest: {error}"))?;
        (manifest, false)
    } else {
        if actual.contains_key(DATABASE_WAL_FILE_NAME)
            || actual.contains_key(DATABASE_SHM_FILE_NAME)
        {
            return Err("Legacy backups must contain a DELETE-mode single database file".into());
        }
        let database = actual
            .get(DATABASE_FILE_NAME)
            .ok_or_else(|| "screenshots.db missing in backup".to_string())?
            .clone();
        (
            BackupManifest {
                format_version: BACKUP_FORMAT_VERSION,
                journal_mode: SnapshotJournalMode::Delete.as_str().to_string(),
                database_files: vec![database],
            },
            true,
        )
    };
    validate_database_file_set(&manifest, &actual)?;

    Ok(ExtractedBackup {
        directory,
        manifest,
        legacy,
        metadata,
        encrypted_master_key,
    })
}

pub(crate) fn validate_database_snapshot(
    directory: &Path,
    manifest: &BackupManifest,
    database_key: &[u8],
) -> Result<(), String> {
    let expected_mode = manifest.mode()?;
    let actual = actual_database_file_map(directory)?;
    validate_database_file_set(manifest, &actual)?;

    // SQLite can recover or checkpoint WAL files merely by opening them. Run
    // semantic validation against a disposable clone so the already-hashed
    // staging group remains byte-for-byte identical until it is installed.
    let validation_parent = directory.parent().unwrap_or(directory);
    let validation_directory =
        TemporaryDirectory::create(validation_parent, "carbonpaper-db-validation")?;
    for file in &manifest.database_files {
        copy_file_verified(
            &directory.join(&file.path),
            &validation_directory.path().join(&file.path),
        )?;
    }

    let database_path = validation_directory.path().join(DATABASE_FILE_NAME);
    let connection = Connection::open_with_flags(
        &database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("Failed to open staged database: {error}"))?;
    let status = connection::configure_sqlcipher_connection(&connection, database_key)
        .map_err(|error| format!("Failed database key or schema validation: {error}"))?;

    connection
        .prepare("SELECT name FROM sqlite_master ORDER BY name LIMIT 1")
        .and_then(|mut statement| statement.query([]).map(|_| ()))
        .map_err(|error| format!("Staged database schema is unreadable: {error}"))?;

    let mut statement = connection
        .prepare("PRAGMA integrity_check")
        .map_err(|error| format!("Failed to start database integrity_check: {error}"))?;
    let results = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Failed to run database integrity_check: {error}"))?;
    let mut messages = Vec::new();
    for result in results {
        let message =
            result.map_err(|error| format!("Failed to read database integrity_check: {error}"))?;
        if !message.eq_ignore_ascii_case("ok") {
            messages.push(message);
        }
    }
    if !messages.is_empty() {
        return Err(format!(
            "Staged database failed integrity_check: {}",
            messages.join("; ")
        ));
    }
    if status.journal_mode != expected_mode.as_str() {
        return Err(format!(
            "Backup journal mode mismatch: manifest={}, database={}",
            expected_mode.as_str(),
            status.journal_mode
        ));
    }
    Ok(())
}

/// File-system transaction used by backup restore.
#[derive(Debug)]
pub(crate) struct RestoreFileTransaction {
    data_directory: PathBuf,
    staged_directory: PathBuf,
    rollback_directory: TemporaryDirectory,
    moved_old: Vec<&'static str>,
    installed_new: Vec<&'static str>,
    complete: bool,
}

/// Atomic target-directory swap used by data-directory migration.
#[derive(Debug)]
pub(crate) struct DirectorySwapTransaction {
    target: PathBuf,
    displaced_target: Option<PathBuf>,
    installed: bool,
    complete: bool,
}

impl DirectorySwapTransaction {
    pub(crate) fn install(staging: TemporaryDirectory, target: &Path) -> Result<Self, String> {
        let parent = target
            .parent()
            .ok_or_else(|| format!("Target data directory has no parent: {}", target.display()))?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create target parent {}: {error}",
                parent.display()
            )
        })?;
        let displaced_target = if target.exists() {
            let holder = TemporaryDirectory::create(parent, "carbonpaper-target-backup")?;
            let backup = holder.path().to_path_buf();
            std::fs::remove_dir(&backup).map_err(|error| {
                format!(
                    "Failed to prepare target backup path {}: {error}",
                    backup.display()
                )
            })?;
            std::fs::rename(target, &backup).map_err(|error| {
                format!(
                    "Failed to move existing target {} to {}: {error}",
                    target.display(),
                    backup.display()
                )
            })?;
            Some(holder.release())
        } else {
            None
        };

        let staging_path = staging.release();
        if let Err(error) = std::fs::rename(&staging_path, target) {
            let mut recovery_errors = Vec::new();
            if let Some(backup) = displaced_target.as_ref() {
                if let Err(restore_error) = std::fs::rename(backup, target) {
                    recovery_errors.push(format!(
                        "failed to restore displaced target from {}: {restore_error}",
                        backup.display()
                    ));
                }
            }
            if let Err(cleanup_error) = remove_path(&staging_path) {
                recovery_errors.push(cleanup_error);
            }
            let mut message = format!(
                "Failed to install staged data directory {} at {}: {error}",
                staging_path.display(),
                target.display()
            );
            if !recovery_errors.is_empty() {
                message.push_str(&format!(
                    "; target recovery failures: {}",
                    recovery_errors.join("; ")
                ));
            }
            return Err(message);
        }
        Ok(Self {
            target: target.to_path_buf(),
            displaced_target,
            installed: true,
            complete: false,
        })
    }

    pub(crate) fn rollback(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        if self.installed && self.target.exists() {
            if let Err(error) = remove_path(&self.target) {
                errors.push(error);
            } else {
                self.installed = false;
            }
        }
        if let Some(backup) = self.displaced_target.as_ref() {
            if backup.exists() && !self.target.exists() {
                if let Err(error) = std::fs::rename(backup, &self.target) {
                    errors.push(format!(
                        "Failed to restore previous target {}: {error}",
                        self.target.display()
                    ));
                }
            }
        }
        if errors.is_empty() {
            self.complete = true;
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub(crate) fn commit(mut self) -> Result<(), String> {
        // Installation and runtime validation have already succeeded. Cleanup
        // failure must leave the new target active and report a redundant
        // rollback directory, rather than undoing a valid migration from Drop.
        let backup = self.displaced_target.take();
        self.complete = true;
        if let Some(backup) = backup {
            remove_path(&backup)?;
        }
        Ok(())
    }
}

impl Drop for DirectorySwapTransaction {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.rollback();
        }
    }
}

impl RestoreFileTransaction {
    pub(crate) fn install(data_directory: &Path, staged_directory: &Path) -> Result<Self, String> {
        Self::install_inner(data_directory, staged_directory, None)
    }

    fn install_inner(
        data_directory: &Path,
        staged_directory: &Path,
        fail_after_installs: Option<usize>,
    ) -> Result<Self, String> {
        let parent = data_directory.parent().unwrap_or(data_directory);
        let rollback_directory =
            TemporaryDirectory::create(parent, "carbonpaper-restore-rollback")?;
        let mut transaction = Self {
            data_directory: data_directory.to_path_buf(),
            staged_directory: staged_directory.to_path_buf(),
            rollback_directory,
            moved_old: Vec::new(),
            installed_new: Vec::new(),
            complete: false,
        };

        let install_result = (|| {
            std::fs::create_dir_all(data_directory).map_err(|error| {
                format!(
                    "Failed to create data directory {}: {error}",
                    data_directory.display()
                )
            })?;
            for name in RESTORE_ENTRY_NAMES {
                let current = data_directory.join(name);
                if current.exists() {
                    let rollback = transaction.rollback_directory.path().join(name);
                    std::fs::rename(&current, &rollback).map_err(|error| {
                        format!(
                            "Failed to move current data {} into rollback storage: {error}",
                            current.display()
                        )
                    })?;
                    transaction.moved_old.push(name);
                }
            }
            for name in RESTORE_ENTRY_NAMES {
                let staged = staged_directory.join(name);
                if staged.exists() {
                    let current = data_directory.join(name);
                    std::fs::rename(&staged, &current).map_err(|error| {
                        format!(
                            "Failed to install restored data {}: {error}",
                            staged.display()
                        )
                    })?;
                    transaction.installed_new.push(name);
                    if fail_after_installs == Some(transaction.installed_new.len()) {
                        return Err("Injected restore installation failure".to_string());
                    }
                }
            }
            Ok(())
        })();

        if let Err(error) = install_result {
            let rollback_error = transaction.rollback().err();
            return Err(match rollback_error {
                Some(rollback_error) => {
                    format!("{error}; restore rollback also failed: {rollback_error}")
                }
                None => error,
            });
        }
        Ok(transaction)
    }

    pub(crate) fn rollback(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for name in self.installed_new.iter().rev().copied() {
            let current = self.data_directory.join(name);
            let staged = self.staged_directory.join(name);
            if current.exists() {
                if staged.exists() {
                    if let Err(error) = remove_path(&staged) {
                        errors.push(error);
                        continue;
                    }
                }
                if let Err(error) = std::fs::rename(&current, &staged) {
                    errors.push(format!(
                        "Failed to remove partially restored {}: {error}",
                        current.display()
                    ));
                }
            }
        }
        for name in self.moved_old.iter().rev().copied() {
            let rollback = self.rollback_directory.path().join(name);
            let current = self.data_directory.join(name);
            if rollback.exists() {
                if current.exists() {
                    if let Err(error) = remove_path(&current) {
                        errors.push(error);
                        continue;
                    }
                }
                if let Err(error) = std::fs::rename(&rollback, &current) {
                    errors.push(format!(
                        "Failed to restore previous data {}: {error}",
                        current.display()
                    ));
                }
            }
        }
        self.complete = errors.is_empty();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub(crate) fn commit(mut self) -> Result<(), String> {
        self.complete = true;
        let rollback_directory = std::mem::replace(
            &mut self.rollback_directory,
            TemporaryDirectory {
                path: PathBuf::new(),
                cleanup: false,
            },
        );
        rollback_directory.remove()
    }
}

impl Drop for RestoreFileTransaction {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.rollback();
        }
    }
}

pub(crate) fn remove_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|error| format!("Failed to remove directory {}: {error}", path.display()))
    } else {
        std::fs::remove_file(path)
            .map_err(|error| format!("Failed to remove file {}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

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

        RestoreFileTransaction::install(&data, &staged)
            .unwrap()
            .commit()
            .unwrap();

        assert_eq!(
            std::fs::read(data.join(DATABASE_FILE_NAME)).unwrap(),
            b"new"
        );
        assert!(!data.join(DATABASE_JOURNAL_FILE_NAME).exists());
    }
}
