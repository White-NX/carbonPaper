use super::*;
use crate::storage::compaction::representation_transaction;
use rusqlite::OptionalExtension;
use std::collections::HashSet;

const CURSOR: &str = "storage_ocr_key_cursor_v1";
const PAGE_ROWS: i64 = 128;
const PAGE_BYTES: usize = 512 * 1024;

struct LegacyRow {
    id: i64,
    screenshot: i64,
    encrypted: Option<Vec<u8>>,
    wrapped: Option<Vec<u8>>,
    grouped: bool,
    deleted: bool,
}

struct Rewrite {
    source: LegacyRow,
    encrypted: Vec<u8>,
}

fn publish(
    conn: &Connection,
    dataset: &str,
    next: i64,
    rows: &[Rewrite],
    keys: &HashMap<i64, WriteKey>,
    check: &impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    representation_transaction(conn, &["app_bound_ocr_update", "recap_o_UPDATE"], |tx| {
        let mut inserted = HashSet::new();
        let mut stale = false;
        for row in rows {
            check()?;
            let source = &row.source;
            let unchanged: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ocr_results WHERE id=?1 AND screenshot_id=?2
                    AND text_enc IS ?3 AND text_key_encrypted IS ?4 AND ocr_key_id IS NULL AND is_deleted=?5)",
                params![source.id,source.screenshot,source.encrypted,source.wrapped,source.deleted], |r| r.get(0),
            ).map_err(|e| e.to_string())?;
            if !unchanged {
                stale = true;
                continue;
            }
            let key = keys
                .get(&source.screenshot)
                .ok_or("Missing prepared OCR migration key")?;
            if inserted.insert(key.id) {
                key.insert(tx, dataset, source.screenshot)?;
            }
            tx.execute("UPDATE ocr_results SET text=NULL,text_enc=?2,text_key_encrypted=NULL,ocr_key_id=?3 WHERE id=?1",
                params![source.id,row.encrypted,key.id]).map_err(|e| e.to_string())?;
        }
        check()?;
        if !stale {
            tx.execute("INSERT INTO app_metadata(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![CURSOR,next.to_string()]).map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

impl StorageState {
    pub(in crate::storage) fn compact_ocr_keys_batch(&self) -> Result<usize, String> {
        if !self.is_silent_read_authorized() {
            return Ok(0);
        }
        let admitted = self.credential_state.session_marker();
        let (generation, dataset, rows) = {
            let guard = self.get_connection_named("compact_ocr_keys_read")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let cursor: i64 = conn
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?1",
                    [CURSOR],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            if cursor == i64::MAX {
                return Ok(0);
            }
            let mut stmt = conn.prepare("SELECT id,screenshot_id,
                CASE WHEN ocr_key_id IS NULL AND length(text_enc)<=?2 THEN text_enc END,
                CASE WHEN ocr_key_id IS NULL THEN text_key_encrypted END,ocr_key_id IS NOT NULL,is_deleted
                FROM ocr_results WHERE id>?1 ORDER BY id LIMIT ?3").map_err(|e| e.to_string())?;
            let mut query = stmt
                .query(params![cursor, PAGE_BYTES as i64, PAGE_ROWS])
                .map_err(|e| e.to_string())?;
            let mut rows = Vec::new();
            let mut bytes = 0;
            while let Some(row) = query.next().map_err(|e| e.to_string())? {
                let source = LegacyRow {
                    id: row.get(0).map_err(|e| e.to_string())?,
                    screenshot: row.get(1).map_err(|e| e.to_string())?,
                    encrypted: row.get(2).map_err(|e| e.to_string())?,
                    wrapped: row.get(3).map_err(|e| e.to_string())?,
                    grouped: row.get(4).map_err(|e| e.to_string())?,
                    deleted: row.get(5).map_err(|e| e.to_string())?,
                };
                let size = source.encrypted.as_ref().map_or(0, Vec::len);
                if bytes + size > PAGE_BYTES {
                    break;
                }
                bytes += size;
                rows.push(source);
            }
            (self.db_generation(), dataset(conn)?, rows)
        };
        let check = || {
            if !self.is_silent_read_authorized()
                || self.credential_state.session_marker() != admitted
                || self.is_database_maintenance_pending()
                || crate::IS_QUITTING.load(std::sync::atomic::Ordering::Relaxed)
            {
                Err("OCR compaction deferred until the admitted session is available".into())
            } else {
                Ok(())
            }
        };
        let count = rows.len();
        let next = rows.last().map_or(i64::MAX, |row| row.id);
        let mut keys = HashMap::<i64, WriteKey>::new();
        let mut rewrites = Vec::new();
        for row in rows {
            if check().is_err() {
                return Ok(0);
            }
            if row.grouped {
                continue;
            }
            let (Some(encrypted), Some(wrapped)) = (&row.encrypted, &row.wrapped) else {
                continue;
            };
            let plain = match self.decrypt_payload_with_row_key_silent(encrypted, wrapped) {
                Ok(plain) => Zeroizing::new(plain),
                Err(BackgroundReadError::AuthRequired) => return Ok(0),
                Err(_) => {
                    tracing::warn!(
                        "[DB:COMPACT] Preserved unreadable legacy OCR row {}",
                        row.id
                    );
                    continue;
                }
            };
            if std::str::from_utf8(&plain).is_err() {
                tracing::warn!(
                    "[DB:COMPACT] Preserved legacy OCR row {} with invalid text encoding",
                    row.id
                );
                continue;
            }
            if !keys.contains_key(&row.screenshot) {
                keys.insert(row.screenshot, self.new_ocr_write_key()?);
            }
            let encrypted =
                keys[&row.screenshot].encrypt(&dataset, row.screenshot, row.id, &plain)?;
            rewrites.push(Rewrite {
                source: row,
                encrypted,
            });
        }
        if check().is_err() {
            return Ok(0);
        }
        let guard = self.get_connection_named("compact_ocr_keys_publish")?;
        if self.db_generation() != generation || check().is_err() {
            return Ok(0);
        }
        publish(
            guard.as_ref().ok_or("Database not initialized")?,
            &dataset,
            next,
            &rewrites,
            &keys,
            &check,
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
