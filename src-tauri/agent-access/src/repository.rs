use crate::types::*;
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use carbonpaper_app_bound::protocol::AgentViewBinding;
use rand::RngCore;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::Path;
use zeroize::Zeroizing;

pub struct AgentRepository {
    pub(crate) conn: Connection,
    binding: AgentViewBinding,
}
fn db<T>(v: rusqlite::Result<T>) -> Result<T, String> {
    v.map_err(|e| format!("AGENT_STORAGE_FAILED: {e}"))
}

pub fn seal_event(key: &[u8], event: &ExportEvent) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "AGENT_INVALID_KEY")?;
    let data = Zeroizing::new(serde_json::to_vec(event).map_err(|_| "AGENT_INVALID_EVENT")?);
    let aad = format!(
        "CarbonPaper agent outbox v1:{}:{}",
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
        "CarbonPaper agent outbox v1:{}:{}",
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
        db(conn.execute_batch(&format!(
            "PRAGMA key=\"x'{}'\"; PRAGMA temp_store=MEMORY; PRAGMA secure_delete=ON;",
            hex::encode(key)
        )))?;
        let version: String = db(conn.query_row("PRAGMA cipher_version", [], |r| r.get(0)))?;
        if version.is_empty() {
            return Err("AGENT_ENCRYPTION_UNAVAILABLE".into());
        }
        db(conn.busy_timeout(std::time::Duration::from_secs(3)))?;
        if !read_only {
            db(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
                CREATE TABLE IF NOT EXISTS agent_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS records(kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,
                    screenshot_id INTEGER,cluster_id INTEGER,date TEXT,captured_at_ms INTEGER,body TEXT,
                    PRIMARY KEY(kind,key));
                CREATE INDEX IF NOT EXISTS agent_records_time ON records(kind,captured_at_ms,key);
                CREATE INDEX IF NOT EXISTS agent_records_snapshot ON records(screenshot_id,kind);
                CREATE INDEX IF NOT EXISTS agent_records_cluster ON records(cluster_id,kind);
                CREATE INDEX IF NOT EXISTS agent_records_day ON records(date,kind);
                CREATE TABLE IF NOT EXISTS gaps(kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(kind,key));
                CREATE TABLE IF NOT EXISTS retired_days(date TEXT PRIMARY KEY,revision INTEGER NOT NULL);
                CREATE VIRTUAL TABLE IF NOT EXISTS text_fts USING fts5(kind UNINDEXED,key UNINDEXED,text,tokenize='trigram');
                CREATE TABLE IF NOT EXISTS text_terms(kind TEXT NOT NULL,key TEXT NOT NULL,term TEXT NOT NULL,PRIMARY KEY(kind,key,term));
                CREATE INDEX IF NOT EXISTS agent_text_terms ON text_terms(term,kind,key);
                CREATE TRIGGER IF NOT EXISTS agent_text_tombstone AFTER UPDATE OF body ON records WHEN NEW.body IS NULL BEGIN
                    DELETE FROM text_fts WHERE kind=NEW.kind AND key=NEW.key;
                    DELETE FROM text_terms WHERE kind=NEW.kind AND key=NEW.key;
                    DELETE FROM gaps WHERE kind=NEW.kind AND key=NEW.key AND revision<=NEW.revision;
                END;"))?;
            let json = serde_json::to_string(binding).map_err(|_| "AGENT_INVALID_BINDING")?;
            db(conn.execute(
                "INSERT OR IGNORE INTO agent_metadata VALUES('binding',?1)",
                [json],
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
        })
    }

    pub fn checkpoint(&self) -> Result<i64, String> {
        Ok(db(self
            .conn
            .query_row(
                "SELECT value FROM agent_metadata WHERE key='checkpoint'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional())?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
    }
    pub fn set_checkpoint(&self, id: i64) -> Result<(), String> {
        db(self.conn.execute("INSERT INTO agent_metadata VALUES('checkpoint',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[id.to_string()]))?;
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
            .prepare("SELECT kind,key,revision FROM gaps ORDER BY revision LIMIT 256"))?;
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
        rows.map(|r| serde_json::from_str(&db(r)?).map_err(|_| "AGENT_INVALID_RECORD".into()))
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
        .map(|n| n.max(0) as usize)
    }
    pub fn revision(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let revision =
            db(self
                .conn
                .query_row("SELECT COALESCE(MAX(revision),0) FROM records", [], |r| {
                    r.get::<_, i64>(0)
                }))?;
        Ok(hex::encode(Sha256::digest(
            format!(
                "{}:{}:{revision}",
                self.binding.dataset_id, self.binding.generation
            )
            .as_bytes(),
        )))
    }
    pub fn ocr(&self, id: i64) -> Result<Vec<OcrText>, String> {
        let mut stmt=db(self.conn.prepare("SELECT body FROM records WHERE kind='ocr' AND screenshot_id=?1 AND body IS NOT NULL ORDER BY CAST(key AS INTEGER)"))?;
        let rows = db(stmt.query_map([id], |r| r.get::<_, String>(0)))?;
        rows.map(|r| serde_json::from_str(&db(r)?).map_err(|_| "AGENT_INVALID_RECORD".into()))
            .collect()
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
                let (kind, key) = record.identity();
                let (snapshot, cluster, date, captured, body, text) = match record {
                    TextRecord::Snapshot(v) => (
                        Some(v.screenshot_id),
                        None,
                        None,
                        v.captured_at_ms,
                        json!(v),
                        v.window_title.clone().unwrap_or_default(),
                    ),
                    TextRecord::Ocr(v) => (
                        Some(v.screenshot_id),
                        None,
                        None,
                        None,
                        json!(v),
                        v.text.clone(),
                    ),
                    TextRecord::Cluster(v) => (
                        None,
                        Some(v.cluster_id),
                        None,
                        None,
                        json!(v),
                        format!(
                            "{} {}",
                            v.display_name.as_deref().unwrap_or_default(),
                            v.anchor_text
                        ),
                    ),
                    TextRecord::Summary(v) => (
                        None,
                        Some(v.cluster_id),
                        None,
                        None,
                        json!(v),
                        v.summary.clone().unwrap_or_default(),
                    ),
                    TextRecord::Assignment(v) => (
                        Some(v.screenshot_id),
                        Some(v.cluster_id),
                        None,
                        None,
                        json!(v),
                        String::new(),
                    ),
                    TextRecord::Recap(v) => (
                        None,
                        None,
                        Some(v.date.as_str()),
                        None,
                        json!(v),
                        v.content
                            .activities
                            .iter()
                            .map(|a| format!("{} {}", a.task_title, a.text))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    TextRecord::Calendar(v) => (
                        None,
                        None,
                        Some(v.date.as_str()),
                        None,
                        json!(v),
                        String::new(),
                    ),
                    TextRecord::Vector(v) => (
                        Some(v.screenshot_id),
                        None,
                        None,
                        None,
                        json!(v),
                        String::new(),
                    ),
                    TextRecord::Privacy(v) => (None, None, None, None, json!(v), String::new()),
                };
                let retired: bool = db(tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM retired_days WHERE date=?1 AND
                        (?3<>'calendar' OR revision>=?2)) OR
                        EXISTS(SELECT 1 FROM records WHERE kind='snapshot' AND key=CAST(?4 AS TEXT) AND body IS NULL AND ?3<>'snapshot') OR
                        EXISTS(SELECT 1 FROM records WHERE kind='cluster' AND key=CAST(?5 AS TEXT) AND body IS NULL AND ?3<>'cluster')",
                    params![date, revision, kind, snapshot, cluster],
                    |r| r.get(0),
                ))?;
                if !retired {
                    let changed=db(tx.execute("INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                        ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,screenshot_id=excluded.screenshot_id,
                        cluster_id=excluded.cluster_id,date=excluded.date,captured_at_ms=excluded.captured_at_ms,body=excluded.body
                        WHERE records.revision<excluded.revision",params![kind,key,revision,snapshot,cluster,date,captured,body.to_string()]))?;
                    if changed > 0 {
                        if kind == "calendar" {
                            db(tx.execute(
                                "DELETE FROM retired_days WHERE date=?1 AND revision<?2",
                                params![date, revision],
                            ))?;
                        }
                        db(tx.execute(
                            "DELETE FROM text_fts WHERE kind=?1 AND key=?2",
                            params![kind, key],
                        ))?;
                        db(tx.execute(
                            "DELETE FROM text_terms WHERE kind=?1 AND key=?2",
                            params![kind, key],
                        ))?;
                        if !text.is_empty() {
                            db(tx.execute(
                                "INSERT INTO text_fts VALUES(?1,?2,?3)",
                                params![kind, key, text],
                            ))?;
                            if kind == "ocr" {
                                let folded = crate::search_plan::fold(&text);
                                let mut insert = db(
                                    tx.prepare("INSERT OR IGNORE INTO text_terms VALUES(?1,?2,?3)")
                                )?;
                                for term in folded.windows(2) {
                                    db(insert.execute(params![
                                        kind,
                                        key,
                                        term.iter().collect::<String>()
                                    ]))?;
                                }
                            }
                        }
                    }
                }
                db(tx.execute(
                    "DELETE FROM gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
                    params![kind, key, revision],
                ))?;
            }
            TextChange::Delete { kind, key } => {
                let changed = Self::delete(&tx, kind, key, revision)?;
                if changed && kind == "snapshot" {
                    let id = key.parse::<i64>().map_err(|_| "AGENT_INVALID_RECORD")?;
                    db(tx.execute("DELETE FROM text_fts WHERE (kind,key) IN (SELECT kind,key FROM records WHERE screenshot_id=?1)",[id]))?;
                    db(tx.execute("UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE screenshot_id=?1",params![id,revision]))?;
                }
                if changed && kind == "cluster" {
                    let id = key.parse::<i64>().map_err(|_| "AGENT_INVALID_RECORD")?;
                    db(tx.execute("DELETE FROM text_fts WHERE (kind,key) IN (SELECT kind,key FROM records WHERE cluster_id=?1)",[id]))?;
                    db(tx.execute("UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE cluster_id=?1",params![id,revision]))?;
                }
            }
            TextChange::SnapshotState {
                screenshot_id,
                category,
                status,
                deleted,
            } => {
                let key = screenshot_id.to_string();
                if *deleted {
                    if Self::delete(&tx, "snapshot", &key, revision)? {
                        db(tx.execute("DELETE FROM text_fts WHERE (kind,key) IN (SELECT kind,key FROM records WHERE screenshot_id=?1)",[screenshot_id]))?;
                        db(tx.execute("UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE screenshot_id=?1",params![screenshot_id,revision]))?;
                    }
                } else {
                    db(tx.execute("UPDATE records SET body=json_set(body,'$.category',?2,'$.status',?3),revision=?4
                        WHERE kind='snapshot' AND key=?1 AND body IS NOT NULL AND revision<?4",params![key,category,status,revision]))?;
                }
            }
            TextChange::Gap { kind, key } => {
                db(tx.execute("INSERT INTO gaps SELECT ?1,?2,?3 WHERE NOT EXISTS(SELECT 1 FROM records WHERE kind=?1 AND key=?2 AND revision>=?3)
                    ON CONFLICT(kind,key) DO UPDATE SET revision=MAX(gaps.revision,excluded.revision)",params![kind,key,revision]))?;
            }
            TextChange::DeleteDay { date } => {
                let changed = db(tx.execute("INSERT INTO retired_days SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM records WHERE kind='calendar' AND key=?1 AND revision>?2)
                    ON CONFLICT(date) DO UPDATE SET revision=excluded.revision WHERE retired_days.revision<excluded.revision",params![date,revision]))?;
                if changed > 0 {
                    db(tx.execute("DELETE FROM text_fts WHERE (kind,key) IN (SELECT kind,key FROM records WHERE date=?1)",[date]))?;
                    db(tx.execute(
                        "UPDATE records SET body=NULL,revision=MAX(revision,?2) WHERE date=?1",
                        params![date, revision],
                    ))?;
                    db(tx.execute("DELETE FROM gaps WHERE (kind,key) IN (SELECT kind,key FROM records WHERE date=?1)",[date]))?;
                }
            }
        }
        db(tx.commit())
    }
    fn delete(conn: &Connection, kind: &str, key: &str, revision: i64) -> Result<bool, String> {
        let changed = db(conn.execute("INSERT INTO records(kind,key,revision,body) VALUES(?1,?2,?3,NULL)
            ON CONFLICT(kind,key) DO UPDATE SET body=NULL,revision=excluded.revision WHERE records.revision<excluded.revision",params![kind,key,revision]))?;
        db(conn.execute("DELETE FROM text_fts WHERE kind=?1 AND key=?2 AND EXISTS(SELECT 1 FROM records WHERE kind=?1 AND key=?2 AND body IS NULL)",params![kind,key]))?;
        db(conn.execute(
            "DELETE FROM gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
            params![kind, key, revision],
        ))?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(path: &Path) -> AgentViewBinding {
        AgentViewBinding {
            dataset_id: "a".repeat(64),
            generation: "b".repeat(64),
            view_path: path.to_string_lossy().into_owned(),
        }
    }
    #[test]
    fn projection_is_independently_encrypted_and_tombstones_fence_late_callbacks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-memory.db");
        let binding = binding(&path);
        let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
        let put = |revision| ExportEvent {
            dataset_id: binding.dataset_id.clone(),
            generation: binding.generation.clone(),
            revision,
            change: TextChange::Put {
                record: TextRecord::Snapshot(SnapshotText {
                    screenshot_id: 42,
                    window_title: Some("private history fixture".into()),
                    ..Default::default()
                }),
            },
        };
        repo.apply(&put(1)).unwrap();
        repo.apply(&ExportEvent {
            change: TextChange::Delete {
                kind: "snapshot".into(),
                key: "42".into(),
            },
            ..put(3)
        })
        .unwrap();
        repo.apply(&put(2)).unwrap();
        assert!(repo.snapshot(42).unwrap().is_none());
        drop(repo);
        assert!(AgentRepository::open(&path, &[8; 32], &binding, true).is_err());
        let image = aes_gcm::Aes256Gcm::new_from_slice(&[9; 32])
            .unwrap()
            .encrypt(Nonce::from_slice(&[0; 12]), b"image fixture".as_ref())
            .unwrap();
        assert!(aes_gcm::Aes256Gcm::new_from_slice(&[7; 32])
            .unwrap()
            .decrypt(Nonce::from_slice(&[0; 12]), image.as_ref())
            .is_err());
        assert!(!std::fs::read(&path)
            .unwrap()
            .windows(23)
            .any(|v| v == b"private history fixture"));
    }
    #[test]
    fn outbox_cannot_cross_dataset_or_generation_and_never_contains_plaintext() {
        let binding = binding(Path::new("fixture"));
        let event = ExportEvent {
            dataset_id: binding.dataset_id.clone(),
            generation: binding.generation.clone(),
            revision: 1,
            change: TextChange::Put {
                record: TextRecord::Ocr(OcrText {
                    text: "private OCR fixture".into(),
                    ..Default::default()
                }),
            },
        };
        let bytes = seal_event(&[7; 32], &event).unwrap();
        assert!(!bytes.windows(19).any(|v| v == b"private OCR fixture"));
        assert!(matches!(
            open_event(&[7; 32], &binding, &bytes).unwrap().change,
            TextChange::Put { .. }
        ));
        let mut other = binding;
        other.generation = "c".repeat(64);
        assert!(open_event(&[7; 32], &other, &bytes).is_err());
    }
    #[test]
    fn stale_deletes_and_gaps_cannot_damage_newer_rows_or_revive_deleted_children() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-memory.db");
        let binding = binding(&path);
        let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
        let event = |revision, change| ExportEvent {
            dataset_id: binding.dataset_id.clone(),
            generation: binding.generation.clone(),
            revision,
            change,
        };
        let snapshot = TextRecord::Snapshot(SnapshotText {
            screenshot_id: 42,
            ..Default::default()
        });
        let ocr = TextRecord::Ocr(OcrText {
            ocr_id: 43,
            screenshot_id: 42,
            text: "current searchable text".into(),
            ..Default::default()
        });
        repo.apply(&event(10, TextChange::Put { record: snapshot }))
            .unwrap();
        repo.apply(&event(
            11,
            TextChange::Put {
                record: ocr.clone(),
            },
        ))
        .unwrap();
        repo.apply(&event(
            5,
            TextChange::Delete {
                kind: "snapshot".into(),
                key: "42".into(),
            },
        ))
        .unwrap();
        repo.apply(&event(
            6,
            TextChange::Gap {
                kind: "ocr".into(),
                key: "43".into(),
            },
        ))
        .unwrap();
        assert!(repo.snapshot(42).unwrap().is_some());
        assert_eq!(repo.ocr(42).unwrap().len(), 1);
        assert!(!repo.has_gaps().unwrap());
        repo.apply(&event(
            12,
            TextChange::Delete {
                kind: "snapshot".into(),
                key: "42".into(),
            },
        ))
        .unwrap();
        repo.apply(&event(13, TextChange::Put { record: ocr }))
            .unwrap();
        repo.apply(&event(
            14,
            TextChange::Put {
                record: TextRecord::Ocr(OcrText {
                    ocr_id: 44,
                    screenshot_id: 42,
                    text: "late new OCR row".into(),
                    ..Default::default()
                }),
            },
        ))
        .unwrap();
        repo.apply(&event(
            15,
            TextChange::Put {
                record: TextRecord::Vector(VectorText {
                    kind: "clip_image".into(),
                    screenshot_id: 42,
                    model_id: "fixture".into(),
                    model_revision: "fixture".into(),
                    vector: vec![1.0],
                }),
            },
        ))
        .unwrap();
        assert!(repo.ocr(42).unwrap().is_empty());
        assert!(repo.all::<VectorText>("vector").unwrap().is_empty());
        assert_eq!(
            repo.conn
                .query_row("SELECT COUNT(*) FROM text_fts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            repo.conn
                .query_row("SELECT COUNT(*) FROM text_terms", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
