//! File-backed, independently encrypted source fixtures for reader contracts.
use crate::{
    blind_index::{self, BlindIndex, MutationStats},
    repository::{seal_access_key, AgentRepository},
    search_plan, source,
    types::*,
};
use carbonpaper_app_bound::{payload::CipherKey, protocol::AgentViewBinding};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use sha2::Sha256;

const BOOTSTRAP: &[u8; 32] = &[6; 32];
const HMAC: &[u8; 32] = &[4; 32];
const AGENT: &[u8; 32] = &[7; 32];

pub enum Record {
    Projection(TextRecord),
    Ocr(OcrText),
    Vector(VectorText),
}
impl From<TextRecord> for Record {
    fn from(value: TextRecord) -> Self {
        Self::Projection(value)
    }
}

pub fn connection(binding: &AgentViewBinding) -> Connection {
    let path = std::path::Path::new(&binding.view_path)
        .parent()
        .unwrap()
        .join("screenshots.db");
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "PRAGMA key=\"x'{}'\"; PRAGMA journal_mode=WAL;",
        hex::encode(BOOTSTRAP)
    ))
    .unwrap();
    conn
}
pub fn configure(repo: &mut AgentRepository, binding: &AgentViewBinding) {
    let conn = connection(binding);
    conn.execute_batch("CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE blind_bitmap_index(token_hash TEXT PRIMARY KEY,postings_blob BLOB NOT NULL);
        CREATE TABLE screenshots(id INTEGER PRIMARY KEY,image_hash TEXT UNIQUE,is_deleted INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE ocr_results(id INTEGER PRIMARY KEY,screenshot_id INTEGER NOT NULL,text_enc BLOB,text_hash TEXT NOT NULL,
            confidence REAL,box_x1 REAL,box_y1 REAL,box_x2 REAL,box_y2 REAL,box_x3 REAL,box_y3 REAL,box_x4 REAL,box_y4 REAL,
            created_at TEXT,is_deleted INTEGER NOT NULL DEFAULT 0,ocr_key_id INTEGER,ocr_block_slot INTEGER);
        CREATE TABLE ocr_index_pending(ocr_id INTEGER PRIMARY KEY);
        CREATE TABLE ocr_keys(id INTEGER PRIMARY KEY,screenshot_id INTEGER NOT NULL,scope_tag BLOB NOT NULL);
        CREATE TABLE ocr_blocks(id INTEGER PRIMARY KEY,screenshot_id INTEGER NOT NULL,content_enc BLOB NOT NULL,created_at TEXT NOT NULL);
        CREATE TABLE derived_embeddings(index_kind TEXT,subject_key TEXT,model_id TEXT,model_revision TEXT,vector_f32 BLOB,
            vector_block_id INTEGER,vector_offset INTEGER,vector_length INTEGER,
            PRIMARY KEY(index_kind,subject_key));
        CREATE TABLE vector_blocks(id INTEGER PRIMARY KEY,data BLOB NOT NULL);
        CREATE VIEW derived_embeddings_read AS
            SELECT e.index_kind,e.subject_key,e.model_id,e.model_revision,
              CASE WHEN e.vector_block_id IS NULL THEN e.vector_f32
                ELSE (SELECT substr(b.data,e.vector_offset+1,e.vector_length) FROM vector_blocks b WHERE b.id=e.vector_block_id)
              END AS vector_f32 FROM derived_embeddings e;
        CREATE TABLE recap_days(day TEXT PRIMARY KEY,revision INTEGER);
        CREATE TABLE recap_payloads(kind TEXT,key TEXT,day TEXT,revision INTEGER,data BLOB,PRIMARY KEY(kind,key));
        CREATE TABLE recap_deleted_days(day TEXT PRIMARY KEY);") .unwrap();
    conn.execute(
        "INSERT INTO app_metadata VALUES('protected_content_dataset_id',?1)",
        [&binding.dataset_id],
    )
    .unwrap();
    blind_index::ensure_schema(&conn).unwrap();
    repo.configure_source(BOOTSTRAP, HMAC).unwrap();
}
fn hash(text: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(HMAC).unwrap();
    mac.update(text.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
pub fn ocr(binding: &AgentViewBinding, row: OcrText) -> TextRecord {
    let key: [u8; 32] = rand::random();
    let bytes =
        carbonpaper_app_bound::crypto::encrypt_storage_bytes(&key, row.text.as_bytes()).unwrap();
    let digest = source::digest(&bytes);
    let conn = connection(binding);
    conn.execute(
        "INSERT OR IGNORE INTO screenshots(id,image_hash) VALUES(?1,?2)",
        params![row.screenshot_id, format!("image-{}", row.screenshot_id)],
    )
    .unwrap();
    let point = |i: usize, j: usize| {
        row.box_coords
            .get(i)
            .and_then(|v| v.get(j))
            .copied()
            .unwrap_or(0.0)
    };
    conn.execute("INSERT OR REPLACE INTO ocr_results(id,screenshot_id,text_enc,text_hash,confidence,
        box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'2026-11-09 00:00:00')",
        params![row.ocr_id,row.screenshot_id,bytes,hash(&row.text),row.confidence,point(0,0),point(0,1),point(1,0),point(1,1),point(2,0),point(2,1),point(3,0),point(3,1)]).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let index = BlindIndex::open(&tx).unwrap();
    let characters: Vec<_> = row
        .text
        .chars()
        .filter(|c| search_plan::is_indexable(*c))
        .collect();
    let words: std::collections::HashSet<String> =
        characters.windows(2).map(|v| v.iter().collect()).collect();
    for word in words {
        index
            .add(
                &hash(&word),
                &[row.ocr_id as u32].into_iter().collect(),
                &mut MutationStats::default(),
            )
            .unwrap();
    }
    tx.commit().unwrap();
    TextRecord::OcrAccess(OcrAccess {
        ocr_id: row.ocr_id,
        screenshot_id: row.screenshot_id,
        key_envelope: seal_access_key(
            AGENT,
            binding,
            "ocr",
            &row.ocr_id.to_string(),
            &digest,
            &key,
        )
        .unwrap(),
        payload_digest: digest,
    })
}
pub fn vector(binding: &AgentViewBinding, row: VectorText) -> TextRecord {
    let key = CipherKey::new(
        "01".repeat(16),
        binding.dataset_id.clone(),
        "vectors".into(),
        &[5; 32],
    )
    .unwrap();
    let subject = if row.kind == "clip_image" {
        format!("image-{}", row.screenshot_id)
    } else {
        row.screenshot_id.to_string()
    };
    let bytes: Vec<u8> = row.vector.iter().flat_map(|v| v.to_le_bytes()).collect();
    let conn = connection(binding);
    conn.execute(
        "INSERT OR IGNORE INTO screenshots(id,image_hash) VALUES(?1,?2)",
        params![row.screenshot_id, format!("image-{}", row.screenshot_id)],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO derived_embeddings(index_kind,subject_key,model_id,model_revision,vector_f32) VALUES(?1,?2,?3,?4,?5)",
        params![
            row.kind,
            subject,
            row.model_id,
            row.model_revision,
            key.seal(&format!("{}:{}", row.kind, subject), &bytes)
                .unwrap()
        ],
    )
    .unwrap();
    TextRecord::VectorAccess(VectorAccess {
        key_id: key.id.clone(),
        key_envelope: seal_access_key(AGENT, binding, "vectors", &key.id, &[], key.bytes())
            .unwrap(),
    })
}
pub fn recap_search(binding: &AgentViewBinding, row: RecapText) -> TextRecord {
    let key: [u8; 32] = rand::random();
    let bytes = carbonpaper_app_bound::crypto::encrypt_storage_bytes(
        &key,
        &serde_json::to_vec(&row.content).unwrap(),
    )
    .unwrap();
    let digest = source::digest(&bytes);
    let conn = connection(binding);
    conn.execute(
        "INSERT OR REPLACE INTO recap_days VALUES(?1,?2)",
        params![row.date, row.revision],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO recap_payloads VALUES('search',?1,?2,?3,?4)",
        params![row.key, row.date, row.revision, bytes],
    )
    .unwrap();
    TextRecord::RecapSearchAccess(RecapSearchAccess {
        date: row.date,
        key: row.key.clone(),
        revision: row.revision,
        key_envelope: seal_access_key(AGENT, binding, "recap_search", &row.key, &digest, &key)
            .unwrap(),
        payload_digest: digest,
    })
}
pub fn put(
    repo: &mut AgentRepository,
    binding: &AgentViewBinding,
    revision: i64,
    record: impl Into<Record>,
) {
    let record = match record.into() {
        Record::Ocr(v) => ocr(binding, v),
        Record::Vector(v) => vector(binding, v),
        Record::Projection(TextRecord::Recap(v)) if v.part == "search" => recap_search(binding, v),
        Record::Projection(v) => v,
    };
    repo.apply(&ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change: TextChange::Put { record },
    })
    .unwrap();
}
