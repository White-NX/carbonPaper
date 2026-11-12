//! Explicit inventory of portable key slots in a database snapshot.
//!
//! The bundle is streamed directly into the encrypted archive. On import it is
//! held in zeroizing memory, never staged as a plaintext file. Slot digests bind
//! each recovered key to the exact original wrapping and snapshot row.

use super::{backup_format::check_cancelled, connection, database_snapshot};
use crate::credential_manager::{
    decrypt_with_master_key, encrypt_with_master_key, load_public_key_from_file,
    unwrap_storage_domain_key, wrap_storage_domain_key, CredentialManagerState,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use carbonpaper_app_bound::payload::{payload_key_id, CipherKey};
use rand::RngCore;
use rusqlite::{config::DbConfig, params, Connection};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::Path,
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

pub(super) const FILE_NAME: &str = "portable_keys.bin";
pub(super) const MAX_BUNDLE_BYTES: usize = 256 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"CPKEY001";
const HEADER_BYTES: usize = 8 + 32 + 32 + 32 + 8;
const SLOT_BYTES: usize = 1 + 8 + 32 + 32;

struct Domain {
    table: &'static str,
    wrapping: &'static str,
    payloads: &'static [&'static str],
}

// These ids are part of CPKEY001. Append new domains; never renumber them.
const DOMAINS: &[Domain] = &[
    Domain {
        table: "screenshots",
        wrapping: "content_key_encrypted",
        payloads: &[
            "window_title_enc",
            "process_name_enc",
            "metadata_enc",
            "page_url_enc",
            "page_icon_enc",
            "visible_links_enc",
        ],
    },
    Domain {
        table: "ocr_results",
        wrapping: "text_key_encrypted",
        payloads: &["text_enc"],
    },
    Domain {
        table: "screenshot_document_refs",
        wrapping: "content_key_encrypted",
        payloads: &["ref_enc"],
    },
    Domain {
        table: "page_icons",
        wrapping: "icon_key_encrypted",
        payloads: &["icon_enc"],
    },
    Domain {
        table: "link_sets",
        wrapping: "links_key_encrypted",
        payloads: &["links_enc"],
    },
    Domain {
        table: "recap_payloads",
        wrapping: "row_key",
        payloads: &["data"],
    },
    Domain {
        table: "recap_search_documents",
        wrapping: "row_key",
        payloads: &["data"],
    },
    Domain {
        table: "protected_storage_keys",
        wrapping: "wrapped_key",
        payloads: &[],
    },
    Domain {
        table: "ocr_keys",
        wrapping: "wrapped_key",
        payloads: &[],
    },
];

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut query = conn
        .prepare("SELECT name FROM pragma_table_info(?1)")
        .map_err(|e| e.to_string())?;
    let result = query
        .query_map([table], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    result
}

fn inventory(conn: &Connection) -> Result<Vec<(usize, HashSet<String>)>, String> {
    super::ocr_keys::backup::validate_references(conn)?;
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(|e| e.to_string())?;
    let names = tables
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut present = Vec::new();
    for table in names {
        let available = columns(conn, &table)?;
        for column in &available {
            if (column.ends_with("_key_encrypted")
                || column == "row_key"
                || column == "wrapped_key")
                && !DOMAINS
                    .iter()
                    .any(|d| d.table == table && d.wrapping == column)
            {
                return Err(format!("Unsupported backup key slot: {table}.{column}"));
            }
        }
        if let Some(index) = DOMAINS.iter().position(|d| d.table == table) {
            let domain = &DOMAINS[index];
            if !available.contains(domain.wrapping) {
                return Err(format!(
                    "Missing backup key column: {table}.{}",
                    domain.wrapping
                ));
            }
            for payload in domain.payloads.iter().filter(|p| available.contains(**p)) {
                let legacy_only = if table == "ocr_results" && available.contains("ocr_key_id") {
                    " AND ocr_key_id IS NULL"
                } else {
                    ""
                };
                let missing: bool = conn.query_row(&format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} WHERE {payload} IS NOT NULL AND length({payload})>0 AND ({} IS NULL OR length({})=0){legacy_only})",
                    domain.wrapping, domain.wrapping), [], |r| r.get(0)).map_err(|e| e.to_string())?;
                if missing {
                    return Err(format!("Missing content key for {table}.{payload}"));
                }
            }
            present.push((index, available));
        }
    }
    present.sort_by_key(|(index, _)| *index);
    Ok(present)
}

fn slot_count(conn: &Connection, present: &[(usize, HashSet<String>)]) -> Result<u64, String> {
    let mut count = 0u64;
    for (index, _) in present {
        let d = &DOMAINS[*index];
        let rows: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(*) FROM {} WHERE {} IS NOT NULL AND length({})>0",
                    d.table, d.wrapping, d.wrapping
                ),
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        count = count
            .checked_add(rows as u64)
            .ok_or("Backup key inventory is too large")?;
    }
    if count > ((MAX_BUNDLE_BYTES - HEADER_BYTES) / SLOT_BYTES) as u64 {
        return Err("Backup key inventory is too large".into());
    }
    Ok(count)
}

pub(super) struct Bundle<'a> {
    bytes: &'a [u8],
    pub(super) count: u64,
}

impl<'a> Bundle<'a> {
    pub(super) fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC || bytes.len() > MAX_BUNDLE_BYTES {
            return Err("Invalid portable backup key bundle".into());
        }
        let count = u64::from_le_bytes(bytes[104..112].try_into().unwrap());
        let expected = count
            .checked_mul(SLOT_BYTES as u64)
            .and_then(|v| v.checked_add(HEADER_BYTES as u64));
        if expected != Some(bytes.len() as u64) {
            return Err("Incomplete portable backup key bundle".into());
        }
        Ok(Self { bytes, count })
    }
    pub(super) fn database_key(&self) -> &[u8] {
        &self.bytes[8..40]
    }
    pub(super) fn master_key(&self) -> &[u8] {
        &self.bytes[40..72]
    }
    fn fingerprint(&self) -> String {
        hex::encode(&self.bytes[72..104])
    }
    fn slot(&self, number: u64) -> &[u8] {
        let start = HEADER_BYTES + number as usize * SLOT_BYTES;
        &self.bytes[start..start + SLOT_BYTES]
    }
}

fn validate_row(
    conn: &Connection,
    index: usize,
    rowid: i64,
    available: &HashSet<String>,
    key: &[u8],
) -> Result<(), String> {
    let d = &DOMAINS[index];
    if d.table == "ocr_keys" {
        return super::ocr_keys::backup::validate_key(conn, rowid, key);
    }
    for column in d.payloads.iter().filter(|p| available.contains(**p)) {
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                &format!("SELECT {column} FROM {} WHERE rowid=?1", d.table),
                [rowid],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if let Some(bytes) = bytes.filter(|b| !b.is_empty()) {
            let _plaintext =
                Zeroizing::new(decrypt_with_master_key(key, &bytes).map_err(|_| {
                    format!("Content authentication failed for {}.{column}", d.table)
                })?);
        }
    }
    Ok(())
}

fn protected_key(
    conn: &Connection,
    rowid: i64,
    key: &[u8],
    fingerprint: &str,
) -> Result<CipherKey, String> {
    let (id, purpose, dataset, saved_fingerprint): (String, String, String, String) = conn.query_row(
        "SELECT key_id,purpose,dataset_id,public_key_fingerprint FROM protected_storage_keys WHERE rowid=?1", [rowid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| e.to_string())?;
    if saved_fingerprint != fingerprint
        || !matches!(purpose.as_str(), "metadata" | "vectors" | "ann")
        || !carbonpaper_app_bound::protocol::valid_id(&dataset)
    {
        return Err("Invalid protected backup key identity".into());
    }
    CipherKey::new(id, dataset, purpose, key)
}

// identities must match protected_migration::text_identity and the writers.
fn validate_protected(conn: &Connection, keys: &HashMap<String, CipherKey>) -> Result<(), String> {
    if columns(conn, "derived_embeddings")?.contains("vector_block_id") {
        super::vector_blocks::validate(conn)?;
    }
    for (table, identity, column, text) in [
        (
            "smart_clusters",
            "'cluster_anchor:'||id",
            "anchor_text",
            true,
        ),
        (
            "smart_clusters",
            "'cluster_name:'||id",
            "display_name",
            true,
        ),
        (
            "smart_clusters",
            "'cluster_anchor_hash:'||id",
            "anchor_vector_source_hash",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':title'",
            "title",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':summary'",
            "summary",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':ocr_summary'",
            "ocr_summary",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':key_points_json'",
            "key_points_json",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':evidence_json'",
            "evidence_json",
            true,
        ),
        (
            "smart_cluster_summaries",
            "'cluster_summary:'||smart_cluster_id||':source_hash'",
            "source_hash",
            true,
        ),
        (
            "classification_anchors",
            "'classification_anchors:1'",
            "anchors_json",
            true,
        ),
        ("tasks", "'task_label:'||id", "label", true),
        (
            "protected_process_names",
            "'process:'||token",
            "name_enc",
            false,
        ),
        (
            "smart_clusters",
            "'cluster_anchor_vector:'||id",
            "anchor_vector",
            false,
        ),
        (
            "derived_embeddings",
            "index_kind||':'||subject_key",
            "vector_f32",
            false,
        ),
    ] {
        if !columns(conn, table)?.contains(column) {
            continue;
        }
        let source =
            if table == "derived_embeddings" && columns(conn, table)?.contains("vector_block_id") {
                "derived_embeddings_read"
            } else {
                table
            };
        let mut query = conn
            .prepare(&format!(
                "SELECT {identity},{column} FROM {source} WHERE {column} IS NOT NULL"
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = query.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let identity: String = row.get(0).map_err(|e| e.to_string())?;
            let data = if text {
                let value: String = row.get(1).map_err(|e| e.to_string())?;
                // Old ZIPs may predate protected-content migration.
                let Some(encoded) = value.strip_prefix("cpe1:") else {
                    continue;
                };
                STANDARD
                    .decode(encoded)
                    .map_err(|_| "Invalid encrypted backup text")?
            } else {
                let value: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
                if !carbonpaper_app_bound::payload::is_sealed(&value) {
                    continue;
                }
                value
            };
            let id = payload_key_id(&data)?;
            let key = keys
                .get(&id)
                .ok_or("Missing protected backup content key")?;
            let _plaintext = key.open(&identity, &data)?;
        }
    }
    Ok(())
}

pub(super) fn write_bundle<W: Write, F: FnMut(u64, u64)>(
    conn: &Connection,
    credentials: &CredentialManagerState,
    database_key: &[u8],
    master_key: &[u8],
    writer: &mut W,
    cancelled: &AtomicBool,
    mut progress: F,
) -> Result<(), String> {
    if database_key.len() != 32 || master_key.len() != 32 {
        return Err("Invalid backup root key length".into());
    }
    let present = inventory(conn)?;
    let count = slot_count(conn, &present)?;
    let public = load_public_key_from_file(credentials).map_err(|e| e.to_string())?;
    let fingerprint = Sha256::digest(public);
    writer
        .write_all(MAGIC)
        .and_then(|_| writer.write_all(database_key))
        .and_then(|_| writer.write_all(master_key))
        .and_then(|_| writer.write_all(&fingerprint))
        .and_then(|_| writer.write_all(&count.to_le_bytes()))
        .map_err(|e| e.to_string())?;
    let mut completed = 0u64;
    let mut protected = HashMap::new();
    let mut ocr_groups = HashMap::new();
    progress(0, count);
    for (index, available) in present {
        let d = &DOMAINS[index];
        let mut query = conn
            .prepare(&format!(
                "SELECT rowid,{} FROM {} WHERE {} IS NOT NULL AND length({})>0 ORDER BY rowid",
                d.wrapping, d.table, d.wrapping, d.wrapping
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = query.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            check_cancelled(cancelled).map_err(|e| e.to_string())?;
            let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
            let wrapping: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
            if wrapping.len() > 4096 {
                return Err("Invalid backup key wrapping length".into());
            }
            let key = Zeroizing::new(
                unwrap_storage_domain_key(credentials, &wrapping).map_err(|e| e.to_string())?,
            );
            if key.len() != 32 {
                return Err("Invalid backup content key length".into());
            }
            validate_row(conn, index, rowid, &available, &key)?;
            if index == 7 {
                let cipher = protected_key(conn, rowid, &key, &hex::encode(fingerprint))?;
                protected.insert(cipher.id.clone(), cipher);
            }
            if index == 8 {
                ocr_groups.insert(rowid, Zeroizing::new(key.to_vec()));
            }
            writer
                .write_all(&[index as u8])
                .and_then(|_| writer.write_all(&rowid.to_le_bytes()))
                .and_then(|_| writer.write_all(&Sha256::digest(&wrapping)))
                .and_then(|_| writer.write_all(&key))
                .map_err(|e| e.to_string())?;
            completed += 1;
            if completed % 20 == 0 || completed == count {
                progress(completed, count);
            }
        }
    }
    super::ocr_keys::backup::validate_members(conn, &ocr_groups, cancelled)?;
    validate_protected(conn, &protected)
}

fn without_triggers<T>(
    conn: &Connection,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let enabled = conn
        .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)
        .map_err(|e| e.to_string())?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .map_err(|e| e.to_string())?;
    let result = operation();
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, enabled)
        .map_err(|e| e.to_string())?;
    result
}

pub(super) fn reset_processing_authority(conn: &Connection) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO app_metadata(key,value) VALUES('app_bound_dataset_id',?1)",
        [carbonpaper_app_bound::protocol::random_id()],
    )
    .map_err(|e| e.to_string())?;
    for table in [
        "app_bound_receipts",
        "app_bound_revocations",
        "ann_build_inputs",
        "ann_build_checkpoints",
        "derived_ann_generations",
        "derived_index_generations",
    ] {
        if !columns(conn, table)?.is_empty() {
            conn.execute(&format!("DELETE FROM {table}"), [])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(super) fn rewrap<F: FnMut(u64, u64)>(
    conn: &Connection,
    bundle: &Bundle<'_>,
    credentials: &CredentialManagerState,
    directory: &Path,
    cancelled: &AtomicBool,
    mut progress: F,
) -> Result<(), String> {
    let present = inventory(conn)?;
    if slot_count(conn, &present)? != bundle.count {
        return Err("Backup key inventory does not cover the database".into());
    }
    let fingerprint = hex::encode(Sha256::digest(
        load_public_key_from_file(credentials).map_err(|e| e.to_string())?,
    ));
    without_triggers(conn, || {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut completed = 0u64;
        let mut protected = HashMap::new();
        let mut ocr_groups = HashMap::new();
        progress(0, bundle.count);
        for (index, available) in &present {
            let d = &DOMAINS[*index];
            let mut query = tx
                .prepare(&format!(
                    "SELECT rowid,{} FROM {} WHERE {} IS NOT NULL AND length({})>0 ORDER BY rowid",
                    d.wrapping, d.table, d.wrapping, d.wrapping
                ))
                .map_err(|e| e.to_string())?;
            let mut rows = query.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                check_cancelled(cancelled).map_err(|e| e.to_string())?;
                let rowid: i64 = row.get(0).map_err(|e| e.to_string())?;
                let original: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
                let slot = bundle.slot(completed);
                if slot[0] as usize != *index
                    || i64::from_le_bytes(slot[1..9].try_into().unwrap()) != rowid
                    || slot[9..41] != Sha256::digest(&original)[..]
                {
                    return Err("Backup key slot does not match the database".into());
                }
                let key = &slot[41..73];
                validate_row(&tx, *index, rowid, available, key)?;
                if *index == 7 {
                    let cipher = protected_key(&tx, rowid, key, &bundle.fingerprint())?;
                    protected.insert(cipher.id.clone(), cipher);
                }
                if *index == 8 {
                    ocr_groups.insert(rowid, Zeroizing::new(key.to_vec()));
                }
                let wrapping =
                    wrap_storage_domain_key(credentials, key).map_err(|e| e.to_string())?;
                let checked = Zeroizing::new(
                    unwrap_storage_domain_key(credentials, &wrapping).map_err(|e| e.to_string())?,
                );
                if &*checked != key {
                    return Err("Restored content key verification failed".into());
                }
                tx.execute(
                    &format!("UPDATE {} SET {}=?1 WHERE rowid=?2", d.table, d.wrapping),
                    params![wrapping, rowid],
                )
                .map_err(|e| e.to_string())?;
                completed += 1;
                if completed % 20 == 0 || completed == bundle.count {
                    progress(completed, bundle.count);
                }
            }
        }
        super::ocr_keys::backup::validate_members(&tx, &ocr_groups, cancelled)?;
        validate_protected(&tx, &protected)?;
        normalize_images(&tx, directory, credentials, cancelled, true)?;
        if !columns(&tx, "protected_storage_keys")?.is_empty() {
            tx.execute(
                "UPDATE protected_storage_keys SET public_key_fingerprint=?1",
                [&fingerprint],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE app_metadata SET value=?1 WHERE key='protected_public_key_fingerprint'",
                [&fingerprint],
            )
            .map_err(|e| e.to_string())?;
        }
        reset_processing_authority(&tx)?;
        tx.commit().map_err(|e| e.to_string())
    })
}

fn relative_image_path(path: &str) -> Result<String, String> {
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

// Convert referenced legacy images in staging. Verified duplicate remnants are
// removed; unrelated plaintext fails closed rather than being lost or exposed.
pub(super) fn validate_source_images(
    conn: &Connection,
    directory: &Path,
    credentials: &CredentialManagerState,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    normalize_images(conn, directory, credentials, cancelled, false)
}

fn normalize_images(
    conn: &Connection,
    directory: &Path,
    credentials: &CredentialManagerState,
    cancelled: &AtomicBool,
    restore: bool,
) -> Result<(), String> {
    if columns(conn, "screenshots")?.is_empty() {
        return Ok(());
    }
    let mut query = conn.prepare("SELECT rowid,image_path,content_key_encrypted,is_deleted,status FROM screenshots ORDER BY rowid").map_err(|e| e.to_string())?;
    let records = query
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, i32>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(query);
    let mut referenced = BTreeMap::<String, (i64, Option<Vec<u8>>)>::new();
    for (rowid, path, wrapping, deleted, status) in records {
        check_cancelled(cancelled).map_err(|e| e.to_string())?;
        let Some(path) = path else {
            continue;
        };
        if path.starts_with("memory://") {
            if deleted == 0 && status.as_deref() != Some("aborted") {
                return Err("A pending screenshot is not yet available for backup".into());
            }
            continue;
        }
        let relative = relative_image_path(&path)?;
        let file = directory.join(&relative);
        if !file.exists() {
            if deleted == 0 && status.as_deref() != Some("aborted") {
                return Err("A screenshot file is missing from the backup".into());
            }
            if restore {
                // Tombstones still feed deferred cleanup after import. Rebase
                // their paths even when there is no file left to re-encrypt.
                conn.execute(
                    "UPDATE screenshots SET image_path=?1 WHERE rowid=?2",
                    params![relative, rowid],
                )
                .map_err(|e| e.to_string())?;
            }
            continue;
        }
        let data = Zeroizing::new(std::fs::read(&file).map_err(|e| e.to_string())?);
        let mut key = match wrapping.as_deref().filter(|w| !w.is_empty()) {
            Some(w) => Zeroizing::new(
                unwrap_storage_domain_key(credentials, w).map_err(|e| e.to_string())?,
            ),
            None => {
                if relative.contains(".enc") {
                    return Err("Missing encrypted screenshot key".into());
                }
                let mut bytes = Zeroizing::new(vec![0; 32]);
                rand::thread_rng().fill_bytes(&mut bytes);
                bytes
            }
        };
        if key.len() != 32 {
            return Err("Invalid screenshot content key".into());
        }
        let destination = if relative.contains(".enc") {
            let _plaintext = Zeroizing::new(
                decrypt_with_master_key(&key, &data)
                    .map_err(|_| "Screenshot authentication failed")?,
            );
            relative.clone()
        } else if restore {
            let encrypted = encrypt_with_master_key(&key, &data).map_err(|e| e.to_string())?;
            let destination = format!("{relative}.enc");
            let target = directory.join(&destination);
            if target.exists() {
                return Err("Conflicting legacy screenshot files in backup".into());
            }
            database_snapshot::write_staged_file(&target, &encrypted)?;
            std::fs::remove_file(&file).map_err(|e| e.to_string())?;
            destination
        } else {
            relative.clone()
        };
        let wrapping = if restore {
            Some(wrap_storage_domain_key(credentials, &key).map_err(|e| e.to_string())?)
        } else {
            wrapping
        };
        key.fill(0);
        if restore {
            conn.execute(
                "UPDATE screenshots SET image_path=?1,content_key_encrypted=?2 WHERE rowid=?3",
                params![destination, wrapping, rowid],
            )
            .map_err(|e| e.to_string())?;
        }
        referenced.insert(destination.to_ascii_lowercase(), (rowid, wrapping));
    }
    let root = directory.join("screenshots");
    // ZIP omits an empty payload directory; referenced missing files were
    // rejected above, and the restore transaction recreates an empty root.
    if !root.exists() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.path() != root.join("thumbs"))
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(directory)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if relative.contains(".enc") {
            continue;
        }
        if referenced.contains_key(&relative.to_ascii_lowercase()) {
            continue;
        }
        let duplicate = format!("{relative}.enc").to_ascii_lowercase();
        let Some((_, Some(wrapping))) = referenced.get(&duplicate) else {
            return Err("BACKUP_UNREFERENCED_PLAINTEXT".into());
        };
        let key = Zeroizing::new(
            unwrap_storage_domain_key(credentials, wrapping).map_err(|e| e.to_string())?,
        );
        let encrypted =
            std::fs::read(directory.join(format!("{relative}.enc"))).map_err(|e| e.to_string())?;
        let expected =
            Zeroizing::new(decrypt_with_master_key(&key, &encrypted).map_err(|e| e.to_string())?);
        let plaintext = Zeroizing::new(std::fs::read(entry.path()).map_err(|e| e.to_string())?);
        if *plaintext != *expected {
            return Err("BACKUP_UNREFERENCED_PLAINTEXT".into());
        }
        if restore {
            std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(super) fn reset_local_authority(directory: &Path, database_key: &[u8]) -> Result<(), String> {
    database_snapshot::recover_interrupted_restore(directory)?;
    let conn = Connection::open(directory.join(database_snapshot::DATABASE_FILE_NAME))
        .map_err(|e| e.to_string())?;
    connection::configure_sqlcipher_connection(&conn, database_key).map_err(|e| e.to_string())?;
    without_triggers(&conn, || {
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        reset_processing_authority(&tx)?;
        tx.commit().map_err(|e| e.to_string())
    })
}
