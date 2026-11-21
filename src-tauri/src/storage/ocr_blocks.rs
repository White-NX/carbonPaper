//! OCR payloads are compressed and encrypted once per bounded screenshot block.
//! The existing row table keeps stable IDs, indexing/deletion state and the
//! top-left ordering coordinates; full geometry and confidence live in the block.

pub(super) mod codec;
mod gc;
pub(super) use codec::{Decoded, Metadata, Record};

use super::ocr_keys::{self, WriteKey};
use rusqlite::{params, Connection};
use zeroize::Zeroizing;

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    super::StorageState::add_column_if_missing(conn, "ocr_results", "ocr_block_slot", "INTEGER")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ocr_blocks(
            id INTEGER PRIMARY KEY REFERENCES ocr_keys(id) ON DELETE CASCADE,
            screenshot_id INTEGER NOT NULL,
            content_enc BLOB NOT NULL,
            created_at TEXT NOT NULL,
            first_row_id INTEGER NOT NULL CHECK(first_row_id>0),
            last_row_id INTEGER NOT NULL CHECK(last_row_id>=first_row_id));
         CREATE TABLE IF NOT EXISTS ocr_block_gc(
            block_id INTEGER PRIMARY KEY REFERENCES ocr_blocks(id) ON DELETE CASCADE);
         CREATE TRIGGER IF NOT EXISTS ocr_block_owner BEFORE INSERT ON ocr_blocks BEGIN
            SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM ocr_keys WHERE id=NEW.id AND screenshot_id=NEW.screenshot_id)
                THEN RAISE(ABORT,'Missing or mismatched OCR block key') END;
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_in_use BEFORE DELETE ON ocr_blocks
         WHEN (SELECT live_rows FROM ocr_keys WHERE id=OLD.id)>0 BEGIN
            SELECT RAISE(ABORT,'OCR block is still referenced');
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_key_delete BEFORE DELETE ON ocr_keys
         WHEN OLD.live_rows=0 BEGIN
            DELETE FROM ocr_block_gc WHERE block_id=OLD.id;
            DELETE FROM ocr_blocks WHERE id=OLD.id;
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_member_insert BEFORE INSERT ON ocr_results
         WHEN NEW.ocr_block_slot IS NOT NULL AND NEW.ocr_key_id IS NULL BEGIN
            SELECT RAISE(ABORT,'Missing OCR block reference');
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_member_update BEFORE UPDATE OF ocr_block_slot,ocr_key_id ON ocr_results
         WHEN NEW.ocr_block_slot IS NOT NULL AND NEW.ocr_key_id IS NULL BEGIN
            SELECT RAISE(ABORT,'Missing OCR block reference');
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_delete BEFORE DELETE ON ocr_results
         WHEN OLD.ocr_block_slot IS NOT NULL BEGIN
            INSERT OR IGNORE INTO ocr_block_gc SELECT OLD.ocr_key_id
                WHERE EXISTS(SELECT 1 FROM ocr_blocks WHERE id=OLD.ocr_key_id);
         END;
         CREATE TRIGGER IF NOT EXISTS ocr_block_detach BEFORE UPDATE OF ocr_key_id,ocr_block_slot ON ocr_results
         WHEN OLD.ocr_block_slot IS NOT NULL AND
              (OLD.ocr_key_id IS NOT NEW.ocr_key_id OR OLD.ocr_block_slot IS NOT NEW.ocr_block_slot) BEGIN
            INSERT OR IGNORE INTO ocr_block_gc SELECT OLD.ocr_key_id
                WHERE EXISTS(SELECT 1 FROM ocr_blocks WHERE id=OLD.ocr_key_id);
         END;",
    ).map_err(|e| e.to_string())
}

pub(super) struct Chunk {
    pub source_size: [u32; 2],
    pub rows: Vec<(i64, Zeroizing<Vec<u8>>)>,
}

pub(super) fn chunks(records: &[Record]) -> Result<Vec<Chunk>, String> {
    let source_size = records
        .iter()
        .find_map(|r| r.text_layout.as_ref().map(|l| l.source_size))
        .unwrap_or([0, 0]);
    let mut result = Vec::new();
    let mut rows = Vec::new();
    let mut bytes = 32usize;
    let mut previous = 0i64;
    for record in records {
        if record.id <= previous {
            return Err("OCR block rows must have increasing IDs".into());
        }
        previous = record.id;
        let encoded = codec::record_bytes(record, source_size)?;
        if !rows.is_empty()
            && (rows.len() >= codec::MAX_ROWS || bytes + encoded.len() + 20 > codec::TARGET_BYTES)
        {
            result.push(Chunk {
                source_size,
                rows: std::mem::take(&mut rows),
            });
            bytes = 32;
        }
        bytes += encoded.len() + 20;
        rows.push((record.id, encoded));
    }
    if !rows.is_empty() {
        result.push(Chunk { source_size, rows });
    }
    Ok(result)
}

pub(super) fn publish_block(
    conn: &Connection,
    dataset: &str,
    screenshot: i64,
    key: &WriteKey,
    chunk: &Chunk,
    created_at: &str,
) -> Result<(), String> {
    let encoded = codec::encode(&chunk.rows, chunk.source_size)?;
    let encrypted = key.encrypt_block(dataset, screenshot, &encoded)?;
    key.insert(conn, dataset, screenshot)?;
    conn.execute("INSERT INTO ocr_blocks(id,screenshot_id,content_enc,created_at,first_row_id,last_row_id) VALUES(?1,?2,?3,?4,?5,?6)",
        params![key.id, screenshot, encrypted, created_at,chunk.rows.first().unwrap().0,chunk.rows.last().unwrap().0]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Called inside the screenshot transaction. The first key is generated before
/// taking the database lock; additional keys are needed only for oversized OCR.
pub(super) fn insert_records(
    conn: &Connection,
    screenshot: i64,
    records: &[Record],
    first_key: Option<&WriteKey>,
    new_key: impl Fn() -> Result<WriteKey, String>,
) -> Result<usize, String> {
    if records.is_empty() {
        return Ok(0);
    }
    let dataset = ocr_keys::dataset(conn)?;
    let created_at: String = conn
        .query_row("SELECT CURRENT_TIMESTAMP", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let chunks = chunks(records)?;
    let positions: std::collections::HashMap<i64, &Record> =
        records.iter().map(|r| (r.id, r)).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let owned;
        let key = if index == 0 {
            if let Some(key) = first_key {
                key
            } else {
                owned = new_key()?;
                &owned
            }
        } else {
            owned = new_key()?;
            &owned
        };
        publish_block(conn, &dataset, screenshot, key, chunk, &created_at)?;
        for (slot, (id, _)) in chunk.rows.iter().enumerate() {
            let record = positions[id];
            conn.execute("INSERT INTO ocr_results(id,screenshot_id,text_hash,ocr_key_id,ocr_block_slot,box_x1,box_y1,created_at)
                VALUES(?1,?2,'',?3,?4,?5,?6,NULL)",
                params![id, screenshot, key.id, slot as i64, record.box_coords[0][0], record.box_coords[0][1]])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(records.len())
}

pub(super) fn input_record(id: i64, result: &super::OcrResultInput) -> Result<Record, String> {
    super::screenshot::validate_ocr_result(result)?;
    Ok(Record {
        id,
        text: result.text.clone(),
        text_layout: result.text_layout.clone(),
        confidence: result.confidence,
        box_coords: std::array::from_fn(|i| [result.box_coords[i][0], result.box_coords[i][1]]),
    })
}

#[cfg(test)]
mod tests;
