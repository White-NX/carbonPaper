use crate::{
    source::{self, ArchiveReader, RawOcr},
    types::*,
};
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use carbonpaper_app_bound::{
    payload::{payload_key_id, CipherKey},
    protocol::AgentViewBinding,
};
use once_cell::unsync::OnceCell;
use rand::RngCore;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::{collections::HashMap, path::Path};
use zeroize::Zeroizing;

#[cfg(test)]
mod tests;

const FORMAT: &str = "2";
pub struct AgentRepository {
    pub(crate) conn: Connection,
    binding: AgentViewBinding,
    key: Zeroizing<Vec<u8>>,
    source: OnceCell<ArchiveReader>,
}
fn db<T>(v: rusqlite::Result<T>) -> Result<T, String> {
    v.map_err(|e| format!("AGENT_STORAGE_FAILED: {e}"))
}

pub fn seal_access_key(
    key: &[u8],
    binding: &AgentViewBinding,
    domain: &str,
    identity: &str,
    digest: &[u8],
    plaintext_key: &[u8],
) -> Result<Vec<u8>, String> {
    source::seal_key(key, binding, domain, identity, digest, plaintext_key)
}

pub fn seal_event(key: &[u8], event: &ExportEvent) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "AGENT_INVALID_KEY")?;
    let data = Zeroizing::new(serde_json::to_vec(event).map_err(|_| "AGENT_INVALID_EVENT")?);
    let aad = format!(
        "CarbonPaper agent outbox v2:{}:{}",
        event.dataset_id, event.generation
    );
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let mut output = nonce.to_vec();
    output.extend(
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &data,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| "AGENT_ENCRYPTION_FAILED")?,
    );
    Ok(output)
}
pub fn open_event(
    key: &[u8],
    binding: &AgentViewBinding,
    bytes: &[u8],
) -> Result<ExportEvent, String> {
    if bytes.len() < 28 || bytes.len() > 32 * 1024 * 1024 {
        return Err("AGENT_INVALID_EVENT".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "AGENT_INVALID_KEY")?;
    let aad = format!(
        "CarbonPaper agent outbox v2:{}:{}",
        binding.dataset_id, binding.generation
    );
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&bytes[..12]),
                Payload {
                    msg: &bytes[12..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| "AGENT_EVENT_INTEGRITY_FAILED")?,
    );
    let event: ExportEvent = serde_json::from_slice(&plain).map_err(|_| "AGENT_INVALID_EVENT")?;
    if event.dataset_id != binding.dataset_id
        || event.generation != binding.generation
        || event.revision <= 0
    {
        return Err("AGENT_DATASET_MISMATCH".into());
    }
    Ok(event)
}

impl AgentRepository {
    pub fn open(
        path: &Path,
        key: &[u8],
        binding: &AgentViewBinding,
        read_only: bool,
    ) -> Result<Self, String> {
        if key.len() != 32 {
            return Err("AGENT_INVALID_KEY".into());
        }
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        };
        let conn = db(Connection::open_with_flags(
            path,
            flags | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        db(conn.execute_batch(&format!("PRAGMA key=\"x'{}'\";", hex::encode(key))))?;
        db(conn.busy_timeout(std::time::Duration::from_secs(3)))?;
        db(conn.execute_batch(
            "PRAGMA temp_store=MEMORY; PRAGMA trusted_schema=OFF; PRAGMA secure_delete=ON;",
        ))?;
        let version: String = db(conn.query_row("PRAGMA cipher_version", [], |r| r.get(0)))?;
        if version.is_empty() {
            return Err("AGENT_ENCRYPTION_UNAVAILABLE".into());
        }
        let existing: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='agent_metadata')", [], |r| r.get(0)))?;
        if existing {
            let format: Option<String> = db(conn
                .query_row(
                    "SELECT value FROM agent_metadata WHERE key='format'",
                    [],
                    |r| r.get(0),
                )
                .optional())?;
            if format.as_deref() != Some(FORMAT) {
                return Err(
                    "AGENT_STORAGE_FORMAT_UNSUPPORTED: Prepare unattended access again".into(),
                );
            }
        }
        if !read_only {
            db(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
                CREATE TABLE IF NOT EXISTS agent_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,
                    screenshot_id INTEGER,cluster_id INTEGER,date TEXT,captured_at_ms INTEGER,body TEXT,PRIMARY KEY(kind,key));
                CREATE INDEX IF NOT EXISTS agent_records_time ON records(kind,captured_at_ms,key);
                CREATE INDEX IF NOT EXISTS agent_records_snapshot ON records(screenshot_id,kind);
                CREATE INDEX IF NOT EXISTS agent_records_cluster ON records(cluster_id,kind);
                CREATE INDEX IF NOT EXISTS agent_records_day ON records(date,kind);
                CREATE TABLE IF NOT EXISTS ocr_access(ocr_id INTEGER PRIMARY KEY,screenshot_id INTEGER NOT NULL,
                    revision INTEGER NOT NULL,payload_digest BLOB,key_envelope BLOB);
                CREATE INDEX IF NOT EXISTS agent_ocr_snapshot ON ocr_access(screenshot_id,ocr_id);
                CREATE TABLE IF NOT EXISTS vector_access(key_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,key_envelope BLOB NOT NULL);
                CREATE TABLE IF NOT EXISTS recap_search_access(key TEXT PRIMARY KEY,date TEXT NOT NULL,
                    revision INTEGER NOT NULL,payload_revision INTEGER NOT NULL,payload_digest BLOB,key_envelope BLOB);
                CREATE INDEX IF NOT EXISTS agent_recap_search_day ON recap_search_access(date,key);
                CREATE TABLE IF NOT EXISTS gaps(kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(kind,key));
                CREATE TABLE IF NOT EXISTS retired_days(date TEXT PRIMARY KEY,revision INTEGER NOT NULL);"))?;
            db(conn.execute(
                "INSERT OR IGNORE INTO agent_metadata VALUES('format',?1)",
                [FORMAT],
            ))?;
            db(conn.execute(
                "INSERT OR IGNORE INTO agent_metadata VALUES('binding',?1)",
                [serde_json::to_string(binding).map_err(|_| "AGENT_INVALID_BINDING")?],
            ))?;
        } else {
            db(conn.execute_batch("PRAGMA query_only=ON"))?;
        }
        let saved: String = db(conn.query_row(
            "SELECT value FROM agent_metadata WHERE key='binding'",
            [],
            |r| r.get(0),
        ))?;
        if serde_json::from_str::<AgentViewBinding>(&saved).map_err(|_| "AGENT_INVALID_BINDING")?
            != *binding
        {
            return Err("AGENT_DATASET_MISMATCH".into());
        }
        Ok(Self {
            conn,
            binding: binding.clone(),
            key: Zeroizing::new(key.to_vec()),
            source: OnceCell::new(),
        })
    }

    /// Only bootstrap and search keys are shared here, never the archive master
    /// key or a screenshot content key. Both are bound to this Agent generation.
    pub fn configure_source(
        &mut self,
        bootstrap_key: &[u8],
        hmac_key: &[u8],
    ) -> Result<(), String> {
        let tx = db(self.conn.transaction())?;
        for (domain, key) in [("bootstrap", bootstrap_key), ("hmac", hmac_key)] {
            let sealed = source::seal_key(&self.key, &self.binding, domain, "", &[], key)?;
            db(tx.execute("INSERT INTO agent_metadata VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![domain,hex::encode(sealed)]))?;
        }
        db(tx.commit())?;
        self.source.take();
        Ok(())
    }

    pub(crate) fn source(&self) -> Result<&ArchiveReader, String> {
        self.source.get_or_try_init(|| {
            let load = |domain: &str| -> Result<Zeroizing<Vec<u8>>, String> {
                let sealed: String = db(self.conn.query_row(
                    "SELECT value FROM agent_metadata WHERE key=?1",
                    [domain],
                    |r| r.get(0),
                ))?;
                source::open_key(
                    &self.key,
                    &self.binding,
                    domain,
                    "",
                    &[],
                    &hex::decode(sealed).map_err(|_| "AGENT_INVALID_SOURCE_KEY")?,
                )
            };
            ArchiveReader::open(&self.binding, &load("bootstrap")?, &load("hmac")?)
        })
    }

    pub fn checkpoint(&self) -> Result<i64, String> {
        let value: Option<String> = db(self
            .conn
            .query_row(
                "SELECT value FROM agent_metadata WHERE key='checkpoint'",
                [],
                |r| r.get(0),
            )
            .optional())?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }
    pub fn set_checkpoint(&self, id: i64) -> Result<(), String> {
        db(self.conn.execute("INSERT INTO agent_metadata VALUES('checkpoint',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [id.to_string()]))?;
        Ok(())
    }
    pub fn has_gaps(&self) -> Result<bool, String> {
        db(self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM gaps)", [], |r| r.get(0)))
    }
    pub fn gaps(&self) -> Result<Vec<(String, String, i64)>, String> {
        let mut stmt = db(self
            .conn
            .prepare("SELECT kind,key,revision FROM gaps ORDER BY revision"))?;
        let rows = db(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))))?;
        db(rows.collect())
    }
    pub fn clear_gap(&self, kind: &str, key: &str, revision: i64) -> Result<(), String> {
        db(self.conn.execute(
            "DELETE FROM gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
            params![kind, key, revision],
        ))?;
        Ok(())
    }
    pub fn get<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Result<Option<T>, String> {
        let row: Option<String> = db(self
            .conn
            .query_row(
                "SELECT body FROM records WHERE kind=?1 AND key=?2 AND body IS NOT NULL",
                params![kind, key],
                |r| r.get(0),
            )
            .optional())?;
        row.map(|v| serde_json::from_str(&v).map_err(|_| "AGENT_INVALID_RECORD".into()))
            .transpose()
    }
    pub fn all<T: DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, String> {
        let mut stmt = db(self
            .conn
            .prepare("SELECT body FROM records WHERE kind=?1 AND body IS NOT NULL ORDER BY key"))?;
        let rows = db(stmt.query_map([kind], |r| r.get::<_, String>(0)))?;
        rows.map(|v| serde_json::from_str(&db(v)?).map_err(|_| "AGENT_INVALID_RECORD".into()))
            .collect()
    }
    pub fn snapshot(&self, id: i64) -> Result<Option<SnapshotText>, String> {
        self.get("snapshot", &id.to_string())
    }
    pub fn snapshot_count(&self) -> Result<usize, String> {
        db(self.conn.query_row(
            "SELECT COUNT(*) FROM records WHERE kind='snapshot' AND body IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0),
        ))
        .map(|v| v.max(0) as usize)
    }
    pub fn revision(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let revision: Option<String> = db(self
            .conn
            .query_row(
                "SELECT value FROM agent_metadata WHERE key='revision'",
                [],
                |r| r.get(0),
            )
            .optional())?;
        Ok(hex::encode(Sha256::digest(
            format!(
                "{}:{}:{}",
                self.binding.dataset_id,
                self.binding.generation,
                revision.unwrap_or_else(|| "0".into())
            )
            .as_bytes(),
        )))
    }

    pub fn ocr(&self, id: i64) -> Result<Vec<OcrText>, String> {
        if self.snapshot(id)?.is_none() {
            return Ok(Vec::new());
        }
        self.hydrate_ocr(self.source()?.ocr(id)?)
    }
    pub fn ocr_ids(&self, ids: &[i64]) -> Result<Vec<OcrText>, String> {
        self.hydrate_ocr(self.source()?.ocr_ids(ids)?)
    }
    pub(crate) fn ocr_snapshot(&self, id: i64) -> Result<Option<SnapshotText>, String> {
        let screenshot: Option<i64> = db(self
            .conn
            .query_row(
                "SELECT screenshot_id FROM ocr_access WHERE ocr_id=?1 AND key_envelope IS NOT NULL",
                [id],
                |r| r.get(0),
            )
            .optional())?;
        match screenshot {
            Some(id) => self.snapshot(id),
            None => Ok(None),
        }
    }

    fn hydrate_ocr(&self, rows: Vec<RawOcr>) -> Result<Vec<OcrText>, String> {
        let mut access = db(self.conn.prepare_cached("SELECT screenshot_id,payload_digest,key_envelope FROM ocr_access WHERE ocr_id=?1 AND key_envelope IS NOT NULL"))?;
        let mut output = Vec::with_capacity(rows.len());
        for row in rows {
            let grant: Option<(i64, Vec<u8>, Vec<u8>)> = db(access
                .query_row([row.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional())?;
            let Some((screenshot, digest, sealed)) = grant else {
                return Err("AGENT_SYNC_INCOMPLETE: OCR authorization is unavailable".into());
            };
            if screenshot != row.screenshot_id || source::digest(&row.ciphertext) != digest {
                return Err("AGENT_SOURCE_CHANGED".into());
            }
            let key = source::open_key(
                &self.key,
                &self.binding,
                "ocr",
                &row.id.to_string(),
                &digest,
                &sealed,
            )?;
            let plain = Zeroizing::new(
                carbonpaper_app_bound::crypto::decrypt_storage_bytes(&key, &row.ciphertext)
                    .map_err(|_| "AGENT_SOURCE_INTEGRITY_FAILED")?,
            );
            let text = String::from_utf8(plain.to_vec()).map_err(|_| "AGENT_INVALID_OCR")?;
            output.push(OcrText {
                ocr_id: row.id,
                screenshot_id: row.screenshot_id,
                text,
                confidence: row.confidence,
                box_coords: row.coordinates,
                ocr_processed_at_ms: row.processed_at_ms,
            });
        }
        Ok(output)
    }

    /// Used by the desktop to finish the existing HMAC index during locked
    /// capture. No CNG context is needed once a row has an Agent authorization.
    pub fn unindexed_ocr(&self, cursor: i64, limit: usize) -> Result<Vec<OcrText>, String> {
        self.ocr_ids(&self.source()?.unindexed_ids(cursor, limit)?)
    }
    pub fn source_hmac_key(&self) -> Result<&[u8], String> {
        Ok(&self.source()?.hmac_key)
    }
    pub fn ocr_payload_digest(&self, id: i64) -> Result<Vec<u8>, String> {
        db(self.conn.query_row(
            "SELECT payload_digest FROM ocr_access WHERE ocr_id=?1 AND key_envelope IS NOT NULL",
            [id],
            |r| r.get(0),
        ))
    }

    pub fn vector_page(
        &self,
        kind: &str,
        cursor: i64,
        limit: usize,
    ) -> Result<Vec<VectorText>, String> {
        let mut keys = HashMap::<String, CipherKey>::new();
        let mut lookup = db(self
            .conn
            .prepare_cached("SELECT key_envelope FROM vector_access WHERE key_id=?1"))?;
        let mut output = Vec::new();
        for raw in self.source()?.vectors(kind, cursor, limit)? {
            if self.snapshot(raw.screenshot_id)?.is_none() {
                continue;
            }
            let id = payload_key_id(&raw.ciphertext)?;
            if !keys.contains_key(&id) {
                let sealed: Option<Vec<u8>> = db(lookup.query_row([&id], |r| r.get(0)).optional())?;
                let sealed =
                    sealed.ok_or("AGENT_SYNC_INCOMPLETE: Vector authorization is unavailable")?;
                let key = source::open_key(&self.key, &self.binding, "vectors", &id, &[], &sealed)?;
                keys.insert(
                    id.clone(),
                    CipherKey::new(
                        id.clone(),
                        self.binding.dataset_id.clone(),
                        "vectors".into(),
                        &key,
                    )?,
                );
            }
            let bytes =
                keys[&id].open(&format!("{}:{}", raw.kind, raw.subject), &raw.ciphertext)?;
            if bytes.is_empty() || bytes.len() % 4 != 0 || bytes.len() > 4096 * 4 {
                return Err("AGENT_INVALID_VECTOR".into());
            }
            let vector: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
                .collect();
            if vector.iter().any(|v| !v.is_finite()) {
                return Err("AGENT_INVALID_VECTOR".into());
            }
            output.push(VectorText {
                kind: raw.kind,
                screenshot_id: raw.screenshot_id,
                model_id: raw.model_id,
                model_revision: raw.model_revision,
                vector,
            });
        }
        Ok(output)
    }

    pub(crate) fn recap_vectors(
        &self,
        date: &str,
        revision: i64,
    ) -> Result<Vec<RecapContent>, String> {
        let mut stmt = db(self.conn.prepare("SELECT key,payload_revision,payload_digest,key_envelope FROM recap_search_access WHERE date=?1 AND key_envelope IS NOT NULL ORDER BY key"))?;
        let grants = db(db(stmt.query_map([date], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        }))?
        .collect::<rusqlite::Result<Vec<_>>>())?;
        let mut output = Vec::new();
        for (id, payload_revision, digest, sealed) in grants {
            if payload_revision != revision {
                continue;
            }
            let Some(bytes) = self.source()?.recap_search(&id, date, payload_revision)? else {
                return Err("AGENT_SOURCE_CHANGED".into());
            };
            if source::digest(&bytes) != digest {
                return Err("AGENT_SOURCE_CHANGED".into());
            }
            let key = source::open_key(
                &self.key,
                &self.binding,
                "recap_search",
                &id,
                &digest,
                &sealed,
            )?;
            let plain = Zeroizing::new(
                carbonpaper_app_bound::crypto::decrypt_storage_bytes(&key, &bytes)
                    .map_err(|_| "AGENT_SOURCE_INTEGRITY_FAILED")?,
            );
            output.push(serde_json::from_slice(&plain).map_err(|_| "AGENT_INVALID_RECAP_INDEX")?);
        }
        Ok(output)
    }

    pub fn apply(&mut self, event: &ExportEvent) -> Result<(), String> {
        if event.dataset_id != self.binding.dataset_id
            || event.generation != self.binding.generation
            || event.revision <= 0
        {
            return Err("AGENT_DATASET_MISMATCH".into());
        }
        let tx = db(self.conn.transaction())?;
        let revision = event.revision;
        match &event.change {
            TextChange::Put { record } => {
                let (kind, id) = record.identity();
                match record {
                    TextRecord::OcrAccess(v) => {
                        if v.ocr_id <= 0 || v.screenshot_id <= 0 || v.payload_digest.len() != 32 {
                            return Err("AGENT_INVALID_OCR_ACCESS".into());
                        }
                        source::open_key(
                            &self.key,
                            &self.binding,
                            "ocr",
                            &id,
                            &v.payload_digest,
                            &v.key_envelope,
                        )?;
                        db(tx.execute("INSERT INTO ocr_access SELECT ?1,?2,?3,?4,?5
                            WHERE NOT EXISTS(SELECT 1 FROM records WHERE kind='snapshot' AND key=CAST(?2 AS TEXT) AND body IS NULL)
                            ON CONFLICT(ocr_id) DO UPDATE SET screenshot_id=excluded.screenshot_id,revision=excluded.revision,
                                payload_digest=excluded.payload_digest,key_envelope=excluded.key_envelope WHERE ocr_access.revision<excluded.revision",
                            params![v.ocr_id,v.screenshot_id,revision,v.payload_digest,v.key_envelope]))?;
                    }
                    TextRecord::VectorAccess(v) => {
                        if hex::decode(&v.key_id).map_or(true, |v| v.len() != 16) {
                            return Err("AGENT_INVALID_VECTOR_ACCESS".into());
                        }
                        source::open_key(
                            &self.key,
                            &self.binding,
                            "vectors",
                            &v.key_id,
                            &[],
                            &v.key_envelope,
                        )?;
                        db(tx.execute("INSERT INTO vector_access VALUES(?1,?2,?3) ON CONFLICT(key_id) DO UPDATE SET revision=excluded.revision,key_envelope=excluded.key_envelope WHERE vector_access.revision<excluded.revision",params![v.key_id,revision,v.key_envelope]))?;
                    }
                    TextRecord::RecapSearchAccess(v) => {
                        if v.payload_digest.len() != 32 {
                            return Err("AGENT_INVALID_RECAP_ACCESS".into());
                        }
                        source::open_key(
                            &self.key,
                            &self.binding,
                            "recap_search",
                            &v.key,
                            &v.payload_digest,
                            &v.key_envelope,
                        )?;
                        db(tx.execute("INSERT INTO recap_search_access SELECT ?1,?2,?3,?4,?5,?6 WHERE NOT EXISTS(SELECT 1 FROM retired_days WHERE date=?2)
                            ON CONFLICT(key) DO UPDATE SET date=excluded.date,revision=excluded.revision,payload_revision=excluded.payload_revision,
                            payload_digest=excluded.payload_digest,key_envelope=excluded.key_envelope WHERE recap_search_access.revision<excluded.revision",
                            params![v.key,v.date,revision,v.revision,v.payload_digest,v.key_envelope]))?;
                    }
                    _ => {
                        let (snapshot, cluster, date, captured, body) = match record {
                            TextRecord::Snapshot(v) => (
                                Some(v.screenshot_id),
                                None,
                                None,
                                v.captured_at_ms,
                                json!(v),
                            ),
                            TextRecord::Cluster(v) => {
                                (None, Some(v.cluster_id), None, None, json!(v))
                            }
                            TextRecord::Summary(v) => {
                                (None, Some(v.cluster_id), None, None, json!(v))
                            }
                            TextRecord::Assignment(v) => (
                                Some(v.screenshot_id),
                                Some(v.cluster_id),
                                None,
                                None,
                                json!(v),
                            ),
                            TextRecord::Recap(v) => {
                                if v.part == "search" || !v.content.vectors.is_empty() {
                                    return Err("AGENT_SOURCE_ACCESS_REQUIRED".into());
                                }
                                (None, None, Some(v.date.as_str()), None, json!(v))
                            }
                            TextRecord::Calendar(v) => {
                                (None, None, Some(v.date.as_str()), None, json!(v))
                            }
                            TextRecord::Privacy(v) => (None, None, None, None, json!(v)),
                            _ => unreachable!(),
                        };
                        let retired: bool = db(tx.query_row("SELECT EXISTS(SELECT 1 FROM retired_days WHERE date=?1 AND (?3<>'calendar' OR revision>=?2))
                            OR EXISTS(SELECT 1 FROM records WHERE kind='snapshot' AND key=CAST(?4 AS TEXT) AND body IS NULL AND ?3<>'snapshot')
                            OR EXISTS(SELECT 1 FROM records WHERE kind='cluster' AND key=CAST(?5 AS TEXT) AND body IS NULL AND ?3<>'cluster')",
                            params![date,revision,kind,snapshot,cluster],|r|r.get(0)))?;
                        if !retired {
                            let changed = db(tx.execute("INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,screenshot_id=excluded.screenshot_id,
                                cluster_id=excluded.cluster_id,date=excluded.date,captured_at_ms=excluded.captured_at_ms,body=excluded.body WHERE records.revision<excluded.revision",
                                params![kind,id,revision,snapshot,cluster,date,captured,body.to_string()]))?;
                            if changed > 0 && kind == "calendar" {
                                db(tx.execute(
                                    "DELETE FROM retired_days WHERE date=?1 AND revision<?2",
                                    params![date, revision],
                                ))?;
                            }
                        }
                    }
                }
                db(tx.execute(
                    "DELETE FROM gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
                    params![kind, id, revision],
                ))?;
            }
            TextChange::Delete { kind, key } => {
                if Self::delete(&tx, kind, key, revision)? {
                    if kind == "snapshot" {
                        Self::delete_children(&tx, key, revision)?;
                    }
                    if kind == "cluster" {
                        db(tx.execute("UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE cluster_id=CAST(?1 AS INTEGER)",params![key,revision]))?;
                    }
                }
            }
            TextChange::SnapshotState {
                screenshot_id,
                category,
                status,
                deleted,
            } => {
                let id = screenshot_id.to_string();
                if *deleted {
                    if Self::delete(&tx, "snapshot", &id, revision)? {
                        Self::delete_children(&tx, &id, revision)?;
                    }
                } else {
                    db(tx.execute("UPDATE records SET body=json_set(body,'$.category',?2,'$.status',?3),revision=?4 WHERE kind='snapshot' AND key=?1 AND body IS NOT NULL AND revision<?4",params![id,category,status,revision]))?;
                }
            }
            TextChange::Gap { kind, key } => {
                if Self::record_revision(&tx, kind, key)?.is_none_or(|v| v < revision) {
                    db(tx.execute("INSERT INTO gaps VALUES(?1,?2,?3) ON CONFLICT(kind,key) DO UPDATE SET revision=MAX(gaps.revision,excluded.revision)",params![kind,key,revision]))?;
                }
            }
            TextChange::DeleteDay { date } => {
                let changed = db(tx.execute("INSERT INTO retired_days SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM records WHERE kind='calendar' AND key=?1 AND revision>?2)
                    ON CONFLICT(date) DO UPDATE SET revision=excluded.revision WHERE retired_days.revision<excluded.revision",params![date,revision]))?;
                if changed > 0 {
                    db(tx.execute(
                        "UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE date=?1",
                        params![date, revision],
                    ))?;
                    db(tx.execute("UPDATE recap_search_access SET key_envelope=NULL,payload_digest=NULL,revision=MAX(revision,?2) WHERE date=?1",params![date,revision]))?;
                    db(tx.execute("DELETE FROM gaps WHERE (kind,key) IN (SELECT kind,key FROM records WHERE date=?1) OR (kind='recap' AND key IN (SELECT 'search:'||key FROM recap_search_access WHERE date=?1))",[date]))?;
                }
            }
            TextChange::SourceChanged => {}
        }
        db(tx.execute("INSERT INTO agent_metadata VALUES('revision',?1) ON CONFLICT(key) DO UPDATE SET value=CAST(MAX(CAST(agent_metadata.value AS INTEGER),CAST(excluded.value AS INTEGER)) AS TEXT)",[revision.to_string()]))?;
        db(tx.commit())
    }

    fn record_revision(conn: &Connection, kind: &str, key: &str) -> Result<Option<i64>, String> {
        if kind == "ocr" {
            return db(conn
                .query_row(
                    "SELECT revision FROM ocr_access WHERE ocr_id=CAST(?1 AS INTEGER)",
                    [key],
                    |r| r.get(0),
                )
                .optional());
        }
        if kind == "vector_key" {
            return db(conn
                .query_row(
                    "SELECT revision FROM vector_access WHERE key_id=?1",
                    [key],
                    |r| r.get(0),
                )
                .optional());
        }
        if kind == "recap" {
            if let Some(key) = key.strip_prefix("search:") {
                return db(conn
                    .query_row(
                        "SELECT revision FROM recap_search_access WHERE key=?1",
                        [key],
                        |r| r.get(0),
                    )
                    .optional());
            }
        }
        db(conn
            .query_row(
                "SELECT revision FROM records WHERE kind=?1 AND key=?2",
                params![kind, key],
                |r| r.get(0),
            )
            .optional())
    }
    fn delete(conn: &Connection, kind: &str, key: &str, revision: i64) -> Result<bool, String> {
        let changed = if kind == "ocr" {
            let id = key.parse::<i64>().map_err(|_| "AGENT_INVALID_RECORD")?;
            db(conn.execute("INSERT INTO ocr_access VALUES(?1,0,?2,NULL,NULL) ON CONFLICT(ocr_id) DO UPDATE SET revision=excluded.revision,key_envelope=NULL,payload_digest=NULL WHERE ocr_access.revision<excluded.revision",params![id,revision]))?
        } else if kind == "recap" && key.starts_with("search:") {
            db(conn.execute("INSERT INTO recap_search_access VALUES(?1,'',?2,0,NULL,NULL) ON CONFLICT(key) DO UPDATE SET revision=excluded.revision,key_envelope=NULL,payload_digest=NULL WHERE recap_search_access.revision<excluded.revision",params![&key[7..],revision]))?
        } else {
            db(conn.execute("INSERT INTO records(kind,key,revision,body) VALUES(?1,?2,?3,NULL) ON CONFLICT(kind,key) DO UPDATE SET body=NULL,revision=excluded.revision WHERE records.revision<excluded.revision",params![kind,key,revision]))?
        };
        db(conn.execute(
            "DELETE FROM gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
            params![kind, key, revision],
        ))?;
        Ok(changed > 0)
    }
    fn delete_children(conn: &Connection, id: &str, revision: i64) -> Result<(), String> {
        db(conn.execute("UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE screenshot_id=CAST(?1 AS INTEGER)",params![id,revision]))?;
        db(conn.execute("UPDATE ocr_access SET key_envelope=NULL,payload_digest=NULL,revision=MAX(revision,?2) WHERE screenshot_id=CAST(?1 AS INTEGER)",params![id,revision]))?;
        db(conn.execute("DELETE FROM gaps WHERE (kind,key) IN (SELECT kind,key FROM records WHERE screenshot_id=CAST(?1 AS INTEGER)) OR (kind='ocr' AND CAST(key AS INTEGER) IN (SELECT ocr_id FROM ocr_access WHERE screenshot_id=CAST(?1 AS INTEGER)))",[id]))?;
        Ok(())
    }
}
