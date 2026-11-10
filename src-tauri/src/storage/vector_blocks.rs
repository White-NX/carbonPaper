//! Pack existing vector ciphertext into bounded immutable BLOBs. SQLite can
//! fill overflow pages densely instead of spending one leaf page per vector.
//! Inline and packed rows remain readable throughout the background rewrite.
use super::{compaction::representation_transaction, StorageState};
use rusqlite::{params, Connection, OptionalExtension};

const CURSOR: &str = "storage_vector_block_cursor_v1";
const PAGE_ROWS: usize = 64;
const MAX_BLOCK_BYTES: usize = 256 * 1024;
const MAX_VECTOR_BYTES: usize = 64 * 1024;
const NOTIFICATIONS: &[&str] = &["derived_embeddings_epoch_after_update"];

pub(super) fn validate(conn: &Connection) -> Result<(), String> {
    let invalid: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM derived_embeddings e LEFT JOIN vector_blocks b ON b.id=e.vector_block_id
         WHERE e.vector_block_id IS NOT NULL AND (b.id IS NULL OR length(e.vector_f32)!=0
             OR e.vector_offset IS NULL OR e.vector_length IS NULL
             OR e.vector_offset<0 OR e.vector_length<=0
             OR e.vector_offset>length(b.data)-e.vector_length))", [], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if invalid {
        Err("Invalid packed vector reference".into())
    } else {
        Ok(())
    }
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS vector_blocks(id INTEGER PRIMARY KEY,data BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS vector_pack_pending(row_id INTEGER PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS vector_block_gc(id INTEGER PRIMARY KEY);",
    )
    .map_err(|e| e.to_string())?;
    for (name, definition) in [
        ("vector_block_id", "INTEGER REFERENCES vector_blocks(id)"),
        ("vector_offset", "INTEGER"),
        ("vector_length", "INTEGER"),
    ] {
        StorageState::add_column_if_missing(&tx, "derived_embeddings", name, definition)?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_derived_embeddings_block
            ON derived_embeddings(vector_block_id) WHERE vector_block_id IS NOT NULL;
         CREATE VIEW IF NOT EXISTS derived_embeddings_read AS
            SELECT e.rowid AS rowid,e.index_kind,e.subject_key,e.dimensions,
                CASE WHEN e.vector_block_id IS NULL THEN e.vector_f32 ELSE
                    (SELECT substr(b.data,e.vector_offset+1,e.vector_length) FROM vector_blocks b
                     WHERE b.id=e.vector_block_id AND e.vector_offset>=0 AND e.vector_length>0
                       AND e.vector_offset<=length(b.data)-e.vector_length)
                END AS vector_f32,
                e.model_id,e.model_revision,e.embedding_version,e.source_fingerprint,
                e.created_at,e.updated_at
            FROM derived_embeddings e;
         CREATE TRIGGER IF NOT EXISTS vector_pack_insert AFTER INSERT ON derived_embeddings
         WHEN length(NEW.vector_f32)>0 BEGIN
            INSERT OR IGNORE INTO vector_pack_pending VALUES(NEW.rowid);
         END;
         CREATE TRIGGER IF NOT EXISTS vector_pack_update AFTER UPDATE OF vector_f32 ON derived_embeddings BEGIN
            DELETE FROM vector_pack_pending WHERE row_id=OLD.rowid;
            INSERT OR IGNORE INTO vector_pack_pending SELECT NEW.rowid WHERE length(NEW.vector_f32)>0;
            UPDATE derived_embeddings SET vector_block_id=NULL,vector_offset=NULL,vector_length=NULL
                WHERE rowid=NEW.rowid AND length(NEW.vector_f32)>0 AND NEW.vector_block_id IS NOT NULL;
         END;
         CREATE TRIGGER IF NOT EXISTS vector_pack_delete AFTER DELETE ON derived_embeddings BEGIN
            DELETE FROM vector_pack_pending WHERE row_id=OLD.rowid;
            INSERT OR IGNORE INTO vector_block_gc SELECT OLD.vector_block_id WHERE OLD.vector_block_id IS NOT NULL;
         END;
         CREATE TRIGGER IF NOT EXISTS vector_block_detach AFTER UPDATE OF vector_block_id ON derived_embeddings
         WHEN OLD.vector_block_id IS NOT NULL AND OLD.vector_block_id IS NOT NEW.vector_block_id BEGIN
            INSERT OR IGNORE INTO vector_block_gc VALUES(OLD.vector_block_id);
         END;",
    ).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

fn write_block(conn: &Connection, rows: &[(i64, Vec<u8>)]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut data = Vec::new();
    for (row_id, bytes) in rows {
        let unchanged: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM derived_embeddings_read WHERE rowid=?1 AND vector_f32=?2)",
            params![row_id,bytes], |r| r.get(0),
        ).map_err(|e| e.to_string())?;
        if !unchanged {
            return Err("Vector changed before packing; retry the batch".into());
        }
        data.extend_from_slice(bytes);
    }
    if data.len() > MAX_BLOCK_BYTES {
        return Err("Vector block exceeds batch budget".into());
    }
    conn.execute("INSERT INTO vector_blocks(data) VALUES(?1)", [&data])
        .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();
    let mut offset = 0i64;
    for (row_id, bytes) in rows {
        conn.execute(
            "UPDATE derived_embeddings SET vector_f32=X'',vector_block_id=?2,vector_offset=?3,vector_length=?4 WHERE rowid=?1",
            params![row_id,id,offset,bytes.len() as i64],
        ).map_err(|e| e.to_string())?;
        offset += bytes.len() as i64;
    }
    Ok(())
}

/// Reclaim wholly dead blocks, and repack blocks once at least half their bytes
/// are dead. The dirty queue makes cleanup proportional to changed blocks.
fn collect_block(conn: &Connection) -> Result<usize, String> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM vector_block_gc ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(id) = id else { return Ok(0) };
    let data: Option<Vec<u8>> = conn
        .query_row(
            "SELECT data FROM vector_blocks WHERE id=?1 AND length(data)<=?2",
            params![id, MAX_BLOCK_BYTES as i64],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT rowid,vector_offset,vector_length FROM derived_embeddings
         WHERE vector_block_id=?1 ORDER BY rowid LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let entries = stmt
        .query_map(params![id, PAGE_ROWS as i64 + 1], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if entries.len() > PAGE_ROWS {
        return Err("Invalid vector block reference count".into());
    }
    if entries.is_empty() {
        conn.execute("DELETE FROM vector_blocks WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
    } else {
        let data = data.ok_or("Referenced vector block is missing or oversized")?;
        let mut live = Vec::new();
        let mut live_bytes = 0;
        for (row, offset, length) in entries {
            let offset = usize::try_from(offset).map_err(|_| "Invalid vector block offset")?;
            let length = usize::try_from(length).map_err(|_| "Invalid vector block length")?;
            if length == 0 || offset > data.len() || length > data.len() - offset {
                return Err("Invalid vector block range".into());
            }
            live.push((row, data[offset..offset + length].to_vec()));
            live_bytes += length;
        }
        if live_bytes * 2 <= data.len() {
            write_block(conn, &live)?;
            conn.execute("DELETE FROM vector_blocks WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
        }
    }
    conn.execute("DELETE FROM vector_block_gc WHERE id=?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(1)
}

pub(super) fn compact(conn: &Connection) -> Result<usize, String> {
    let cursor: i64 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?1",
            [CURSOR],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    // No bootstrap queue scan: walk old rows once, and use triggers to capture
    // inserts/replacements behind the cursor while that scan is in progress.
    let sql = if cursor >= 0 {
        "SELECT rowid,CASE WHEN vector_block_id IS NULL AND length(vector_f32) BETWEEN 1 AND ?2 THEN vector_f32 END
         FROM derived_embeddings WHERE rowid>?1 ORDER BY rowid LIMIT ?3"
    } else {
        "SELECT e.rowid,CASE WHEN e.vector_block_id IS NULL AND length(e.vector_f32) BETWEEN 1 AND ?2 THEN e.vector_f32 END
         FROM vector_pack_pending p CROSS JOIN derived_embeddings e ON e.rowid=p.row_id
         WHERE p.row_id>?1 ORDER BY p.row_id LIMIT ?3"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut query = stmt
        .query(params![
            cursor.max(0),
            MAX_VECTOR_BYTES as i64,
            PAGE_ROWS as i64
        ])
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let mut scanned = Vec::new();
    let mut bytes = 0;
    while let Some(row) = query.next().map_err(|e| e.to_string())? {
        let row_id: i64 = row.get(0).map_err(|e| e.to_string())?;
        let payload: Option<Vec<u8>> = row.get(1).map_err(|e| e.to_string())?;
        if let Some(payload) = payload {
            if bytes + payload.len() > MAX_BLOCK_BYTES {
                break;
            }
            bytes += payload.len();
            rows.push((row_id, payload));
        }
        scanned.push(row_id);
    }
    drop(query);
    drop(stmt);
    // Keep small fresh tails inline until enough writes arrive to pack well.
    let pack = cursor >= 0
        || scanned.len() >= 16
        || bytes >= MAX_BLOCK_BYTES / 2
        || rows.len() != scanned.len();
    let has_gc: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM vector_block_gc)", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    if !pack && !has_gc {
        return Ok(0);
    }
    representation_transaction(conn, NOTIFICATIONS, |tx| {
        let mut work = collect_block(tx)?;
        if pack {
            write_block(tx, &rows)?;
            for id in &scanned {
                tx.execute("DELETE FROM vector_pack_pending WHERE row_id=?1", [id])
                    .map_err(|e| e.to_string())?;
            }
            work += scanned.len();
            if cursor >= 0 {
                tx.execute(
                    "INSERT INTO app_metadata(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![CURSOR,scanned.last().copied().unwrap_or(-1).to_string()],
                ).map_err(|e| e.to_string())?;
            }
        }
        Ok(work)
    })
}

#[cfg(test)]
mod tests;
