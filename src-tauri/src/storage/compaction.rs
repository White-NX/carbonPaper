//! Bounded, restartable storage rewrites. Readers accept both old and compact
//! rows throughout the migration; a batch and its cursor commit together.
use super::StorageState;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
mod tests;

const OCR_CURSOR: &str = "storage_ocr_marker_cursor_v1";
const OCR_DONE: &str = "storage_ocr_marker_done_v1";
const OCR_QUEUE_VERSION: &str = "storage_ocr_queue_version";

/// Keep the legacy column name so old rows remain readable without rebuilding
/// the multi-gigabyte OCR table. Only emptiness is part of the reader contract.
pub(super) const OCR_INDEXED: &str = "1";

/// A representation-only update must not invalidate source revisions or
/// expensive derived caches. Only the named notification triggers are parked;
/// referential, queue and format guards continue running. DDL, data and trigger
/// restoration commit atomically, including after a crash or failed checkpoint.
pub(super) fn representation_transaction<T>(
    conn: &Connection,
    triggers: &[&str],
    operation: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut definitions = Vec::new();
    for name in triggers {
        let definition: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(sql) = definition {
            tx.execute_batch(&format!("DROP TRIGGER {name};"))
                .map_err(|e| e.to_string())?;
            definitions.push(sql);
        }
    }
    let result = operation(&tx)?;
    for definition in definitions {
        tx.execute_batch(&definition).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ocr_index_pending (
            ocr_id INTEGER PRIMARY KEY REFERENCES ocr_results(id) ON DELETE CASCADE
        );
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_insert AFTER INSERT ON ocr_results
        WHEN NEW.text_hash='' AND NEW.is_deleted=0 BEGIN
            INSERT OR IGNORE INTO ocr_index_pending VALUES(NEW.id);
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_update
        AFTER UPDATE OF id,text_hash,is_deleted ON ocr_results
        WHEN OLD.id!=NEW.id OR
            (OLD.text_hash='' AND OLD.is_deleted=0)!=(NEW.text_hash='' AND NEW.is_deleted=0)
        BEGIN
            DELETE FROM ocr_index_pending WHERE ocr_id=OLD.id;
            INSERT OR IGNORE INTO ocr_index_pending
                SELECT NEW.id WHERE NEW.text_hash='' AND NEW.is_deleted=0;
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_delete AFTER DELETE ON ocr_results BEGIN
            DELETE FROM ocr_index_pending WHERE ocr_id=OLD.id;
        END;",
    )
    .map_err(|e| e.to_string())?;
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM app_metadata WHERE key=?1",
            [OCR_QUEUE_VERSION],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match version.as_deref() {
        None => {
            // The legacy hash index locates the usually tiny backlog without
            // scanning every OCR body. Fresh databases have no OCR rows yet.
            tx.execute(
                "INSERT OR IGNORE INTO ocr_index_pending
                    SELECT id FROM ocr_results WHERE text_hash='' AND is_deleted=0",
                [],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO app_metadata(key,value) VALUES(?1,'1')",
                [OCR_QUEUE_VERSION],
            )
            .map_err(|e| e.to_string())?;
        }
        Some("1") => {}
        Some(_) => return Err("Unsupported OCR index queue format".into()),
    }
    tx.execute_batch("DROP INDEX IF EXISTS idx_text_hash;")
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// No OCR decryption or search-index rebuild is needed. Every existing
/// nonempty value already means indexed, including during HMAC migration.
fn compact_ocr_markers(conn: &Connection, limit: usize) -> Result<usize, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let done: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_metadata WHERE key=?1 AND value='1')",
            [OCR_DONE],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if done {
        return Ok(0);
    }
    let cursor: i64 = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?1",
            [OCR_CURSOR],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let ids = {
        let mut stmt = tx
            .prepare("SELECT id FROM ocr_results WHERE id>?1 ORDER BY id LIMIT ?2")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![cursor, limit.clamp(1, 2048) as i64], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    if let Some(last) = ids.last() {
        tx.execute(
            "UPDATE ocr_results SET text_hash=?3
                WHERE id>?1 AND id<=?2 AND length(text_hash)>1",
            params![cursor, last, OCR_INDEXED],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO app_metadata(key,value) VALUES(?1,?2)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![OCR_CURSOR, last.to_string()],
        )
        .map_err(|e| e.to_string())?;
    } else {
        tx.execute(
            "INSERT INTO app_metadata(key,value) VALUES(?1,'1')
                ON CONFLICT(key) DO UPDATE SET value='1'",
            [OCR_DONE],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM app_metadata WHERE key=?1", [OCR_CURSOR])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ids.len())
}

impl StorageState {
    pub(crate) fn start_storage_compaction(self: &Arc<Self>) {
        if self.storage_compaction_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let storage = Arc::clone(self);
        std::thread::spawn(move || {
            while !crate::IS_QUITTING.load(Ordering::Relaxed)
                && !crate::IS_UPDATING.load(Ordering::Relaxed)
            {
                if !storage.is_initialized()
                    || storage.is_database_maintenance_pending()
                    || crate::maintenance::is_active()
                {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                match storage.compact_storage_batch() {
                    Ok(0) => std::thread::sleep(Duration::from_secs(5)),
                    Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                    Err(error) => {
                        tracing::warn!("[DB:COMPACT] Background rewrite deferred: {error}");
                        std::thread::sleep(Duration::from_secs(30));
                    }
                }
            }
        });
    }

    fn compact_storage_batch(&self) -> Result<usize, String> {
        let compacted = {
            let guard = self.get_connection_named("storage_compaction")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            compact_ocr_markers(conn, 1024)? + super::vector_blocks::compact(conn)?
        };
        Ok(compacted + self.compact_process_icons_batch()?)
    }
}
