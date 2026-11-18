//! Screenshot inventory and error recovery for authenticated portable backups.

use super::{
    backup_format::check_cancelled, database_snapshot, image_io::regular_image_file_exists,
};
use crate::credential_manager::{
    decrypt_with_master_key, unwrap_storage_domain_key, CredentialManagerState,
};
use database_snapshot::{MissingScreenshot, MissingScreenshotReason};
use rusqlite::{config::DbConfig, params, Connection};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

pub(super) fn relative_image_path(path: &str) -> Result<String, String> {
    let path = path.replace('\\', "/");
    let relative = if path.starts_with("screenshots/") {
        path.as_str()
    } else {
        let start = path
            .to_ascii_lowercase()
            .rfind("/screenshots/")
            .ok_or("Unsupported screenshot path in backup")?;
        &path[start + 1..]
    };
    let name = database_snapshot::normalize_archive_name(relative, false)?;
    if !name.starts_with("screenshots/") || name.starts_with("screenshots/thumbs/") {
        return Err("Unsupported screenshot path in backup".into());
    }
    Ok(name)
}

fn authenticate_published_image(
    directory: &Path,
    relative: &str,
    rowid: i64,
    wrapping: Option<&[u8]>,
    credentials: &CredentialManagerState,
) -> Result<bool, String> {
    let bytes = match std::fs::read(directory.join(relative)) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Failed to read screenshot (id={rowid}, path={relative}): {error}"
            ));
        }
    };
    let wrapping = wrapping.filter(|key| !key.is_empty()).ok_or_else(|| {
        format!("Missing screenshot key while recovering path (id={rowid}, path={relative})")
    })?;
    let key = Zeroizing::new(unwrap_storage_domain_key(credentials, wrapping).map_err(|error| {
        format!("Failed to unwrap screenshot key during path recovery (id={rowid}, path={relative}): {error}")
    })?);
    let _plaintext = Zeroizing::new(decrypt_with_master_key(&key, &bytes).map_err(|error| {
        format!(
            "Screenshot path recovery authentication failed (id={rowid}, path={relative}): {error}"
        )
    })?);
    Ok(true)
}

struct Reference {
    rowid: i64,
    relative: String,
    required: bool,
}

pub(super) struct SourceImages {
    references: BTreeMap<String, Vec<Reference>>,
    missing_ids: HashSet<i64>,
    archived_ids: HashSet<i64>,
    pub(super) files: Vec<(PathBuf, String, u64)>,
    pub(super) missing: Vec<MissingScreenshot>,
    pub(super) repaired_paths: u64,
}

impl SourceImages {
    /// Authenticate exactly the bytes sent to the archive, reusing keys already
    /// unwrapped for the portable bundle. Missing records still retain those
    /// keys because their titles and metadata use them independently of images.
    pub(super) fn validate(
        &self,
        name: &str,
        bytes: &[u8],
        keys: &HashMap<i64, &[u8]>,
    ) -> Result<(), String> {
        if !name.contains(".enc") {
            return Ok(());
        }
        if let Some(references) = self.references.get(&name.to_ascii_lowercase()) {
            for reference in references {
                let key = keys.get(&reference.rowid).ok_or_else(|| {
                    format!(
                        "Missing encrypted screenshot key (id={}, path={})",
                        reference.rowid, reference.relative
                    )
                })?;
                let _plaintext =
                    Zeroizing::new(decrypt_with_master_key(key, bytes).map_err(|error| {
                        format!(
                            "Screenshot authentication failed (id={}, path={}): {error}",
                            reference.rowid, reference.relative
                        )
                    })?);
            }
        }
        Ok(())
    }

    /// A file can also disappear after inventory, for example through external
    /// cleanup. This is recorded before writing the final database and manifest.
    pub(super) fn record_missing(&mut self, name: &str) {
        if let Some(references) = self.references.get(&name.to_ascii_lowercase()) {
            for reference in references.iter().filter(|reference| reference.required) {
                if !self.missing_ids.insert(reference.rowid) {
                    continue;
                }
                tracing::warn!(phase = "images", screenshot_id = reference.rowid, image_path = %reference.relative,
                    reason = "not_found", outcome = "metadata_preserved", "Backup omitted a missing screenshot");
                self.missing.push(MissingScreenshot {
                    screenshot_id: reference.rowid,
                    image_path: reference.relative.clone(),
                    reason: MissingScreenshotReason::NotFound,
                });
            }
        }
    }

    pub(super) fn record_present(&mut self, name: &str) {
        if let Some(references) = self.references.get(&name.to_ascii_lowercase()) {
            for reference in references {
                self.archived_ids.insert(reference.rowid);
            }
        }
    }

    pub(super) fn finish_inventory(&mut self, directory: &Path) -> Result<(), String> {
        // Also cover a referenced file removed between the row inventory and
        // directory traversal, when it never reached the archive's input list.
        let absent = self
            .references
            .iter()
            .filter(|(_, references)| {
                references.iter().any(|reference| {
                    reference.required && !self.archived_ids.contains(&reference.rowid)
                })
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for name in absent {
            let reference = self.references[&name]
                .iter()
                .find(|reference| reference.required)
                .unwrap();
            if !self.missing_ids.contains(&reference.rowid)
                && regular_image_file_exists(&directory.join(&reference.relative))?
            {
                return Err(format!(
                    "Screenshot was not archived (id={}, path={})",
                    reference.rowid, reference.relative
                ));
            }
            self.record_missing(&name);
        }
        self.missing
            .retain(|missing| !self.archived_ids.contains(&missing.screenshot_id));
        self.missing.sort_by_key(|missing| missing.screenshot_id);
        Ok(())
    }
}

/// Normalize references only in the export database. Bulk image authentication
/// is deferred to archive writing, avoiding a second image read and CNG unwrap.
pub(super) fn prepare_source_images(
    conn: &Connection,
    directory: &Path,
    credentials: &CredentialManagerState,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(&str, u64, u64),
) -> Result<SourceImages, String> {
    let mut images = SourceImages {
        references: BTreeMap::new(),
        missing_ids: HashSet::new(),
        archived_ids: HashSet::new(),
        files: Vec::new(),
        missing: Vec::new(),
        repaired_paths: 0,
    };
    let mut query = conn.prepare("SELECT rowid,image_path,content_key_encrypted,is_deleted,status FROM screenshots ORDER BY rowid")
        .map_err(|error| error.to_string())?;
    let count = conn
        .query_row("SELECT count(*) FROM screenshots", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| error.to_string())? as u64;
    let total = count * 2;
    progress("images", 0, total);
    let mut records = Vec::new();
    let mut rows = query.query([]).map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        records.push((
            row.get::<_, i64>(0).map_err(|error| error.to_string())?,
            row.get::<_, Option<String>>(1)
                .map_err(|error| error.to_string())?,
            row.get::<_, Option<Vec<u8>>>(2)
                .map_err(|error| error.to_string())?,
            row.get::<_, i32>(3).map_err(|error| error.to_string())?,
            row.get::<_, Option<String>>(4)
                .map_err(|error| error.to_string())?,
        ));
        if records.len().is_multiple_of(100) || records.len() as u64 == count {
            progress("images", records.len() as u64, total);
        }
    }
    drop(rows);
    drop(query);
    let enabled = conn
        .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)
        .map_err(|error| error.to_string())?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        let tx = conn
            .unchecked_transaction()
            .map_err(|error| error.to_string())?;
        for (index, (rowid, path, wrapping, deleted, status)) in records.into_iter().enumerate() {
            check_cancelled(cancelled).map_err(|error| error.to_string())?;
            let required = deleted == 0 && status.as_deref() != Some("aborted");
            if let Some(path) = path {
                if path.starts_with("memory://") {
                    if required {
                        return Err(format!(
                            "A pending screenshot is not yet available for backup (id={rowid})"
                        ));
                    }
                } else {
                    let mut relative = relative_image_path(&path)?;
                    let mut exists = regular_image_file_exists(&directory.join(&relative))?;
                    if !exists {
                        if let Some(final_path) = relative
                            .strip_suffix(".pending")
                            .filter(|name| name.contains(".enc"))
                        {
                            if regular_image_file_exists(&directory.join(final_path))?
                                && authenticate_published_image(
                                    directory,
                                    final_path,
                                    rowid,
                                    wrapping.as_deref(),
                                    credentials,
                                )?
                            {
                                // Never adopt a similarly named file merely on existence.
                                tracing::info!(phase = "images", screenshot_id = rowid, image_path = %final_path,
                                    outcome = "path_recovered", "Backup recovered a published screenshot path");
                                relative = final_path.to_string();
                                exists = true;
                                images.repaired_paths += 1;
                            }
                        }
                    }
                    if path != relative {
                        tx.execute(
                            "UPDATE screenshots SET image_path=?1 WHERE rowid=?2",
                            params![relative, rowid],
                        )
                        .map_err(|error| error.to_string())?;
                    }
                    images
                        .references
                        .entry(relative.to_ascii_lowercase())
                        .or_default()
                        .push(Reference {
                            rowid,
                            relative: relative.clone(),
                            required,
                        });
                    if !exists {
                        images.record_missing(&relative);
                    }
                }
            }
            if (index + 1).is_multiple_of(100) || index + 1 == count as usize {
                progress("images", count + (index + 1) as u64, total);
            }
        }
        tx.commit().map_err(|error| error.to_string())
    })();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, enabled)
        .map_err(|error| error.to_string())?;
    result?;

    let root = directory.join("screenshots");
    progress("scan", 0, 0);
    if !root.try_exists().map_err(|error| error.to_string())? {
        return Ok(images);
    }
    // Only verified legacy duplicate remnants may be omitted. Unknown plaintext
    // still fails explicitly so an export never silently loses available data.
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.path() != root.join("thumbs"))
    {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error)
                if error
                    .io_error()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(error) => return Err(error.to_string()),
        };
        if entry.file_type().is_symlink() {
            return Err("Symbolic links are not valid backup payloads".into());
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(directory)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        database_snapshot::normalize_archive_name(&relative, false)?;
        if relative.contains(".enc")
            || images
                .references
                .contains_key(&relative.to_ascii_lowercase())
        {
            let size = match entry.metadata() {
                Ok(metadata) => metadata.len(),
                Err(error)
                    if error
                        .io_error()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(error) => return Err(error.to_string()),
            };
            images.files.push((entry.into_path(), relative, size));
            if images.files.len().is_multiple_of(100) {
                progress("scan", images.files.len() as u64, 0);
            }
            continue;
        }
        let duplicate = format!("{relative}.enc");
        let references = images
            .references
            .get(&duplicate.to_ascii_lowercase())
            .ok_or("BACKUP_UNREFERENCED_PLAINTEXT")?;
        let wrapping: Vec<u8> = conn
            .query_row(
                "SELECT content_key_encrypted FROM screenshots WHERE rowid=?1",
                [references[0].rowid],
                |row| row.get(0),
            )
            .map_err(|_| "BACKUP_UNREFERENCED_PLAINTEXT")?;
        let key = Zeroizing::new(
            unwrap_storage_domain_key(credentials, &wrapping).map_err(|error| error.to_string())?,
        );
        let encrypted =
            std::fs::read(directory.join(&duplicate)).map_err(|error| error.to_string())?;
        let expected = Zeroizing::new(
            decrypt_with_master_key(&key, &encrypted).map_err(|error| error.to_string())?,
        );
        let plaintext =
            Zeroizing::new(std::fs::read(entry.path()).map_err(|error| error.to_string())?);
        if *plaintext != *expected {
            return Err("BACKUP_UNREFERENCED_PLAINTEXT".into());
        }
    }
    images.files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(images)
}
