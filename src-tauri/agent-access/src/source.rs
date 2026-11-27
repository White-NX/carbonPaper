//! Fixed, read-only queries over the public bootstrap database. Only scoped
//! OCR, recap-search and vectors keys supplied by the Agent repository can
//! decrypt content; this module has no archive master key or image API.
use crate::{blind_index::BlindIndex, search_plan::QueryPlan};
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use carbonpaper_app_bound::protocol::AgentViewBinding;
use hmac::{Hmac, Mac};
use roaring::RoaringBitmap;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};
use zeroize::Zeroizing;

fn storage<T>(value: rusqlite::Result<T>) -> Result<T, String> {
    value.map_err(|e| format!("AGENT_SOURCE_FAILED: {e}"))
}

fn key_aad(
    binding: &AgentViewBinding,
    domain: &str,
    identity: &str,
    digest: &[u8],
) -> Result<Vec<u8>, String> {
    if !matches!(
        domain,
        "bootstrap" | "hmac" | "ocr" | "vectors" | "recap_search"
    ) || !matches!(digest.len(), 0 | 32)
    {
        return Err("AGENT_INVALID_KEY_SCOPE".into());
    }
    serde_json::to_vec(&(
        "CarbonPaper-Agent-Key-v1",
        &binding.dataset_id,
        &binding.generation,
        domain,
        identity,
        hex::encode(digest),
    ))
    .map_err(|_| "AGENT_INVALID_KEY_SCOPE".into())
}

pub(crate) fn seal_key(
    content_key: &[u8],
    binding: &AgentViewBinding,
    domain: &str,
    identity: &str,
    digest: &[u8],
    key: &[u8],
) -> Result<Vec<u8>, String> {
    if content_key.len() != 32 || key.len() != 32 {
        return Err("AGENT_INVALID_KEY".into());
    }
    let cipher = Aes256Gcm::new_from_slice(content_key).map_err(|_| "AGENT_INVALID_KEY")?;
    let nonce: [u8; 12] = rand::random();
    let aad = key_aad(binding, domain, identity, digest)?;
    let mut sealed = nonce.to_vec();
    sealed.extend(
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: key,
                    aad: &aad,
                },
            )
            .map_err(|_| "AGENT_KEY_ENCRYPTION_FAILED")?,
    );
    Ok(sealed)
}

pub(crate) fn open_key(
    content_key: &[u8],
    binding: &AgentViewBinding,
    domain: &str,
    identity: &str,
    digest: &[u8],
    sealed: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if content_key.len() != 32 || sealed.len() != 60 {
        return Err("AGENT_INVALID_KEY".into());
    }
    let cipher = Aes256Gcm::new_from_slice(content_key).map_err(|_| "AGENT_INVALID_KEY")?;
    let aad = key_aad(binding, domain, identity, digest)?;
    cipher
        .decrypt(
            Nonce::from_slice(&sealed[..12]),
            Payload {
                msg: &sealed[12..],
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| "AGENT_KEY_INTEGRITY_FAILED".into())
}

pub(crate) fn digest(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Binds a permission to one row in an immutable, authenticated OCR block.
/// A rewrite assigns a new key and block ID, so the ciphertext is read and
/// authenticated once per block instead of hashed once per row.
pub fn ocr_block_digest(block_id: i64, slot: i64, scope_tag: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"carbonpaper/agent/ocr-block/v1\0");
    digest.update(block_id.to_le_bytes());
    digest.update(slot.to_le_bytes());
    digest.update(scope_tag);
    digest.finalize().into()
}

pub(crate) struct RawOcrKey {
    pub id: i64,
    pub scope_tag: Vec<u8>,
}

pub(crate) struct RawOcr {
    pub id: i64,
    pub screenshot_id: i64,
    pub ciphertext: Option<Vec<u8>>,
    pub confidence: f64,
    pub coordinates: Vec<Vec<f64>>,
    pub processed_at_ms: Option<i64>,
    pub key: Option<RawOcrKey>,
    pub slot: Option<i64>,
}
impl RawOcr {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let slot: Option<i64> = row.get(15)?;
        Ok(Self {
            id: row.get(0)?,
            screenshot_id: row.get(1)?,
            ciphertext: row.get(2)?,
            confidence: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            coordinates: if slot.is_some() {
                Vec::new()
            } else {
                (0..4)
                    .map(|i| {
                        Ok(vec![
                            row.get::<_, Option<f64>>(4 + i * 2)?.unwrap_or(0.0),
                            row.get::<_, Option<f64>>(5 + i * 2)?.unwrap_or(0.0),
                        ])
                    })
                    .collect::<rusqlite::Result<_>>()?
            },
            processed_at_ms: row.get(12)?,
            key: row
                .get::<_, Option<i64>>(13)?
                .map(|id| -> rusqlite::Result<_> {
                    Ok(RawOcrKey {
                        id,
                        scope_tag: row.get(14)?,
                    })
                })
                .transpose()?,
            slot,
        })
    }
}
const OCR_COLUMNS: &str = "o.id,o.screenshot_id,o.text_enc,o.confidence,
    o.box_x1,o.box_y1,o.box_x2,o.box_y2,o.box_x3,o.box_y3,o.box_x4,o.box_y4,
    CAST(strftime('%s',o.created_at) AS INTEGER)*1000,
    o.ocr_key_id,k.scope_tag,o.ocr_block_slot";

pub(crate) struct RawVector {
    pub screenshot_id: i64,
    pub kind: String,
    pub subject: String,
    pub model_id: String,
    pub model_revision: String,
    pub ciphertext: Vec<u8>,
}

pub(crate) struct ArchiveReader {
    conn: Connection,
    pub hmac_key: Zeroizing<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_connection_cannot_write_the_archive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent-memory.db");
        let binding = AgentViewBinding {
            dataset_id: "a".repeat(64),
            generation: "b".repeat(64),
            view_path: path.to_string_lossy().into_owned(),
        };
        let mut repo =
            crate::repository::AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
        crate::test_source::configure(&mut repo, &binding);
        let source = repo.source().unwrap();
        assert!(source
            .conn
            .execute("UPDATE app_metadata SET value='modified'", [])
            .is_err());
        let dataset: String = source
            .conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dataset, binding.dataset_id);
    }
}
impl ArchiveReader {
    pub fn open(
        binding: &AgentViewBinding,
        db_key: &[u8],
        hmac_key: &[u8],
    ) -> Result<Self, String> {
        if db_key.len() != 32 || hmac_key.len() != 32 {
            return Err("AGENT_INVALID_SOURCE_KEY".into());
        }
        // The source name is fixed and shares the service-authorized directory.
        // No tool argument or frontend-supplied path selects a file.
        let directory = Path::new(&binding.view_path)
            .parent()
            .ok_or("AGENT_INVALID_SOURCE_PATH")?;
        let conn = storage(Connection::open_with_flags(
            directory.join("screenshots.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ))?;
        storage(conn.execute_batch(&format!("PRAGMA key=\"x'{}'\";", hex::encode(db_key))))?;
        storage(conn.busy_timeout(std::time::Duration::from_secs(3)))?;
        storage(conn.execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY;",
        ))?;
        let dataset: String = storage(conn.query_row(
            "SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'",
            [],
            |r| r.get(0),
        ))?;
        if dataset != binding.dataset_id {
            return Err("AGENT_DATASET_MISMATCH".into());
        }
        Ok(Self {
            conn,
            hmac_key: Zeroizing::new(hmac_key.to_vec()),
        })
    }

    pub fn ocr_for_screenshots(&self, ids: &[i64]) -> Result<Vec<RawOcr>, String> {
        self.ocr_query("o.screenshot_id IN (SELECT value FROM json_each(?1))", ids)
    }

    pub fn ocr_ids(&self, ids: &[i64]) -> Result<Vec<RawOcr>, String> {
        self.ocr_query("o.id IN (SELECT value FROM json_each(?1))", ids)
    }

    fn ocr_query(&self, predicate: &str, ids: &[i64]) -> Result<Vec<RawOcr>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {OCR_COLUMNS} FROM ocr_results o
            JOIN screenshots s ON s.id=o.screenshot_id
            LEFT JOIN ocr_keys k ON k.id=o.ocr_key_id AND k.screenshot_id=o.screenshot_id
            WHERE o.is_deleted=0 AND s.is_deleted=0 AND {predicate} ORDER BY o.id"
        );
        let mut statement = storage(self.conn.prepare_cached(&sql))?;
        let rows = storage(statement.query_map(
            [serde_json::to_string(ids).map_err(|_| "AGENT_INVALID_IDS")?],
            RawOcr::read,
        ))?;
        storage(rows.collect())
    }

    pub fn ocr_block(&self, id: i64, screenshot_id: i64) -> Result<(Vec<u8>, Option<i64>), String> {
        let mut statement = storage(self.conn.prepare_cached(
            "SELECT content_enc, CAST(strftime('%s',created_at) AS INTEGER)*1000
             FROM ocr_blocks WHERE id=?1 AND screenshot_id=?2
             AND length(content_enc) BETWEEN 28 AND ?3",
        ))?;
        storage(statement.query_row(
            params![
                id,
                screenshot_id,
                (crate::ocr_block_codec::MAX_BYTES + 33) as i64
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ))
    }

    pub fn unindexed_ids(&self, cursor: i64, limit: usize) -> Result<Vec<i64>, String> {
        let mut stmt = storage(self.conn.prepare(
            "SELECT p.ocr_id FROM ocr_index_pending p
            CROSS JOIN ocr_results o ON o.id=p.ocr_id
            CROSS JOIN screenshots s ON s.id=o.screenshot_id
            WHERE p.ocr_id>?1 AND o.is_deleted=0 AND s.is_deleted=0
            ORDER BY p.ocr_id LIMIT ?2",
        ))?;
        let rows = storage(stmt.query_map(params![cursor, limit as i64], |r| r.get(0)))?;
        storage(rows.collect())
    }

    pub fn all_ocr_ids(&self, cursor: i64, limit: usize) -> Result<Vec<i64>, String> {
        let mut stmt = storage(self.conn.prepare(
            "SELECT o.id FROM ocr_results o
            JOIN screenshots s ON s.id=o.screenshot_id WHERE o.id>?1
            AND o.is_deleted=0 AND s.is_deleted=0 ORDER BY o.id LIMIT ?2",
        ))?;
        let rows = storage(stmt.query_map(params![cursor, limit as i64], |r| r.get(0)))?;
        storage(rows.collect())
    }

    pub fn candidates(&self, plan: &QueryPlan) -> Result<RoaringBitmap, String> {
        let hashes: HashSet<String> = plan
            .phrase_groups
            .iter()
            .flat_map(|g| &g.variants)
            .map(|term| {
                let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.hmac_key)
                    .expect("validated HMAC key");
                mac.update(term.as_bytes());
                hex::encode(mac.finalize().into_bytes())
            })
            .collect();
        let index = BlindIndex::open(&self.conn)?;
        let probes = index.probe(&hashes.into_iter().collect::<Vec<_>>())?;
        let mut candidates = RoaringBitmap::new();
        for probe in probes.postings {
            if let Some(posting) = index.read(&probe.hash, probe.chunked)? {
                candidates |= posting.bitmap;
            }
        }
        Ok(candidates)
    }

    pub fn vectors(&self, kind: &str, cursor: i64, limit: usize) -> Result<Vec<RawVector>, String> {
        if !matches!(kind, "clip_image" | "semantic_text") {
            return Err("AGENT_INVALID_VECTOR_KIND".into());
        }
        let exists: bool = storage(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM derived_embeddings WHERE index_kind=?1)",
            [kind],
            |r| r.get(0),
        ))?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut stmt = storage(self.conn.prepare("SELECT s.id,e.index_kind,e.subject_key,e.model_id,e.model_revision,e.vector_f32
            FROM screenshots s NOT INDEXED CROSS JOIN derived_embeddings_read e ON e.index_kind=?1
            AND e.subject_key=CASE ?1 WHEN 'clip_image' THEN s.image_hash ELSE CAST(s.id AS TEXT) END
            WHERE s.id>?2 AND s.is_deleted=0 ORDER BY s.id LIMIT ?3"))?;
        let rows = storage(stmt.query_map(params![kind, cursor, limit as i64], |r| {
            Ok(RawVector {
                screenshot_id: r.get(0)?,
                kind: r.get(1)?,
                subject: r.get(2)?,
                model_id: r.get(3)?,
                model_revision: r.get(4)?,
                ciphertext: r.get(5)?,
            })
        }))?;
        storage(rows.collect())
    }

    pub fn recap_search(
        &self,
        key: &str,
        date: &str,
        revision: i64,
    ) -> Result<Option<Vec<u8>>, String> {
        storage(
            self.conn
                .query_row(
                    "SELECT p.data FROM recap_payloads p
            JOIN recap_days d ON d.day=p.day AND d.revision=p.revision
            WHERE p.kind='search' AND p.key=?1 AND p.day=?2 AND p.revision=?3
            AND NOT EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=p.day)",
                    params![key, date, revision],
                    |r| r.get(0),
                )
                .optional(),
        )
    }
}
