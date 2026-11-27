//! Repack live members of existing blocks after deletion. Historical format
//! conversion is handled by the standalone offline tool.

use super::*;
use crate::storage::{compaction::representation_transaction, StorageState};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;

const NOTIFICATIONS: &[&str] = &[
    "app_bound_ocr_update",
    "recap_o_UPDATE",
    "snapshot_search_ocr_results_UPDATE",
];

struct Member {
    id: i64,
    screenshot: i64,
    encrypted: Option<Vec<u8>>,
    wrapped: Option<Vec<u8>>,
    group: i64,
    reference: Vec<u8>,
    text: Option<String>,
    deleted: i64,
    created_at: String,
    row_created_at: Option<String>,
    confidence: Option<f64>,
    box_coords: [[Option<f64>; 2]; 4],
    slot: i64,
}

impl Member {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            screenshot: row.get(1)?,
            encrypted: row.get(2)?,
            reference: row.get(3)?,
            text: row.get(4)?,
            wrapped: row.get(5)?,
            group: row.get(6)?,
            deleted: row.get(7)?,
            created_at: row.get(8)?,
            confidence: row.get(9)?,
            box_coords: [
                [row.get(10)?, row.get(11)?],
                [row.get(12)?, row.get(13)?],
                [row.get(14)?, row.get(15)?],
                [row.get(16)?, row.get(17)?],
            ],
            slot: row.get(18)?,
            row_created_at: row.get(19)?,
        })
    }

    fn unchanged(&self, conn: &Connection) -> Result<bool, String> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ocr_results WHERE id=?1 AND screenshot_id=?2
            AND text_enc IS ?3 AND text_key_encrypted IS ?4 AND ocr_key_id=?5 AND text IS ?6
            AND is_deleted=?7 AND ocr_block_slot=?8 AND confidence IS ?9
            AND box_x1 IS ?10 AND box_y1 IS ?11 AND box_x2 IS ?12 AND box_y2 IS ?13
            AND box_x3 IS ?14 AND box_y3 IS ?15 AND box_x4 IS ?16 AND box_y4 IS ?17
            AND created_at IS ?18)",
            params![
                self.id,
                self.screenshot,
                self.encrypted,
                self.wrapped,
                self.group,
                self.text,
                self.deleted,
                self.slot,
                self.confidence,
                self.box_coords[0][0],
                self.box_coords[0][1],
                self.box_coords[1][0],
                self.box_coords[1][1],
                self.box_coords[2][0],
                self.box_coords[2][1],
                self.box_coords[3][0],
                self.box_coords[3][1],
                self.row_created_at
            ],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }
}

fn block_members_sql() -> &'static str {
    "SELECT id,screenshot_id,row_text_enc,text_key_ref,text,text_key_encrypted,
        ocr_key_id,is_deleted,created_at,confidence,
        box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4,ocr_block_slot,row_created_at
     FROM ocr_results_read WHERE id BETWEEN
        (SELECT first_row_id FROM ocr_blocks WHERE id=?1) AND
        (SELECT last_row_id FROM ocr_blocks WHERE id=?1)
     AND ocr_key_id=?1 AND ocr_block_slot IS NOT NULL ORDER BY id"
}

struct Work {
    block: i64,
    dataset: String,
    members: Vec<Member>,
}

fn read_work(conn: &Connection, decoder: &ocr_keys::Decoder<'_>) -> Result<Option<Work>, String> {
    let _snapshot = ocr_keys::read_snapshot(conn)?;
    let block = conn
        .query_row(
            "SELECT block_id FROM ocr_block_gc ORDER BY block_id LIMIT 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(block) = block else {
        return Ok(None);
    };
    let mut stmt = conn
        .prepare(block_members_sql())
        .map_err(|e| e.to_string())?;
    let members = stmt
        .query_map([block], Member::from_row)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let live: i64 = conn
        .query_row("SELECT live_rows FROM ocr_keys WHERE id=?1", [block], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if members.len() as i64 != live || members.len() > codec::MAX_ROWS {
        return Err("OCR block bounds do not match its members".into());
    }
    decoder.load_blocks(conn, members.iter().map(|r| r.reference.as_slice()))?;
    Ok(Some(Work {
        block,
        dataset: ocr_keys::dataset(conn)?,
        members,
    }))
}

struct Rewrite {
    key: WriteKey,
    chunk: Chunk,
    created_at: String,
}

fn prepare(
    storage: &StorageState,
    work: &Work,
    decoder: &ocr_keys::Decoder<'_>,
    check: &impl Fn() -> Result<(), String>,
) -> Result<Vec<Rewrite>, String> {
    let mut records = Vec::with_capacity(work.members.len());
    for member in &work.members {
        check()?;
        let decoded = decoder
            .decrypt_record(&[], &member.reference)
            .map_err(|e| e.to_string())?;
        let metadata = decoded.metadata.ok_or("Missing OCR block metadata")?;
        records.push(Record {
            id: member.id,
            text: decoded.text,
            text_layout: decoded.text_layout,
            confidence: metadata.confidence,
            box_coords: metadata.box_coords,
        });
    }
    let created_at = work
        .members
        .first()
        .map(|r| r.created_at.as_str())
        .unwrap_or("");
    chunks(&records)?
        .into_iter()
        .map(|chunk| {
            check()?;
            Ok(Rewrite {
                key: storage.new_ocr_write_key()?,
                chunk,
                created_at: created_at.into(),
            })
        })
        .collect()
}

fn publish(
    conn: &Connection,
    work: &Work,
    rewrites: &[Rewrite],
    check: &impl Fn() -> Result<(), String>,
) -> Result<usize, String> {
    representation_transaction(conn, NOTIFICATIONS, |tx| {
        check()?;
        if work.members.is_empty() {
            return tx
                .execute(
                    "DELETE FROM ocr_keys WHERE id=?1 AND live_rows=0",
                    [work.block],
                )
                .map_err(|e| e.to_string());
        }
        let sources: BTreeMap<_, _> = work.members.iter().map(|r| (r.id, r)).collect();
        let mut changed = 0;
        for rewrite in rewrites {
            let mut rows = Vec::new();
            for (id, bytes) in &rewrite.chunk.rows {
                check()?;
                let source = sources[id];
                if source.unchanged(tx)? {
                    rows.push((source, bytes));
                }
            }
            if rows.is_empty() {
                continue;
            }
            let chunk = Chunk {
                source_size: rewrite.chunk.source_size,
                rows: rows
                    .iter()
                    .map(|(r, bytes)| (r.id, Zeroizing::new(bytes.to_vec())))
                    .collect(),
            };
            publish_block(
                tx,
                &work.dataset,
                rows[0].0.screenshot,
                &rewrite.key,
                &chunk,
                &rewrite.created_at,
            )?;
            for (slot, (row, _)) in rows.iter().enumerate() {
                check()?;
                let created_at = (row.created_at != rewrite.created_at).then_some(&row.created_at);
                tx.execute("UPDATE ocr_results SET text=NULL,text_enc=NULL,text_key_encrypted=NULL,
                    ocr_key_id=?2,ocr_block_slot=?3,confidence=NULL,
                    box_x2=NULL,box_y2=NULL,box_x3=NULL,box_y3=NULL,box_x4=NULL,box_y4=NULL,created_at=?4 WHERE id=?1",
                    params![row.id,rewrite.key.id,slot as i64,created_at]).map_err(|e| e.to_string())?;
                changed += 1;
            }
        }
        check()?;
        Ok(changed)
    })
}

impl StorageState {
    pub(in crate::storage) fn compact_ocr_blocks_batch(&self) -> Result<usize, String> {
        let admitted = self.credential_state.session_marker();
        let check = || {
            if !self.is_silent_read_authorized()
                || self.credential_state.session_marker() != admitted
                || self.is_database_maintenance_pending()
                || crate::IS_QUITTING.load(std::sync::atomic::Ordering::Relaxed)
            {
                Err("AUTH_REQUIRED".into())
            } else {
                Ok(())
            }
        };
        if check().is_err() {
            return Ok(0);
        }
        let decoder = self.ocr_decoder();
        let (generation, work) = {
            let guard = self.get_connection_named("ocr_block_gc_read")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            (self.db_generation(), read_work(conn, &decoder)?)
        };
        let Some(work) = work else {
            return Ok(0);
        };
        let rewrites = match prepare(self, &work, &decoder, &check) {
            Ok(rewrites) => rewrites,
            Err(e) if e == "AUTH_REQUIRED" => return Ok(0),
            Err(e) => return Err(e),
        };
        let guard = self.get_connection_named("ocr_block_gc_publish")?;
        if self.db_generation() != generation || check().is_err() {
            return Ok(0);
        }
        publish(
            guard.as_ref().ok_or("Database not initialized")?,
            &work,
            &rewrites,
            &check,
        )
    }

    pub(crate) fn prepare_ocr_blocks_for_backup(&self) -> Result<(), String> {
        let admitted = self.credential_state.session_marker();
        let check = || {
            self.backup.check()?;
            if !self.is_silent_read_authorized()
                || self.credential_state.session_marker() != admitted
            {
                Err("AUTH_REQUIRED".into())
            } else {
                Ok(())
            }
        };
        let guard = self.get_connection_named("prepare_ocr_blocks_for_backup")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        loop {
            check()?;
            let decoder = self.ocr_decoder();
            let Some(work) = read_work(conn, &decoder)? else {
                return Ok(());
            };
            let rewrites = prepare(self, &work, &decoder, &check)?;
            publish(conn, &work, &rewrites, &check)?;
            let pending: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM ocr_block_gc WHERE block_id=?1)",
                    [work.block],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if pending {
                return Err(
                    "OCR block changed during cleanup; preserved the original members for retry"
                        .into(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
