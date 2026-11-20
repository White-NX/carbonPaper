//! Portable backups wrap group keys once, then authenticate all children in one
//! streaming pass. Group membership needs no full-size secondary OCR index.
use super::*;
use std::sync::atomic::AtomicBool;

fn enabled(conn: &Connection) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('ocr_results') WHERE name='ocr_key_id')",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(in crate::storage) fn validate_references(conn: &Connection) -> Result<(), String> {
    if !enabled(conn)? {
        return Ok(());
    }
    let blocks: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('ocr_results') WHERE name='ocr_block_slot')", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if blocks {
        let invalid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM ocr_results WHERE ocr_block_slot IS NOT NULL AND ocr_key_id IS NULL)
            OR EXISTS(SELECT 1 FROM ocr_block_gc)", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if invalid {
            return Err(
                "Complete OCR block cleanup before exporting or importing this archive".into(),
            );
        }
    }
    let join = if blocks {
        " LEFT JOIN ocr_blocks b ON b.id=r.ocr_key_id"
    } else {
        ""
    };
    let content = if blocks {
        "(r.ocr_block_slot IS NULL AND (r.text_enc IS NULL OR length(r.text_enc)<28)) OR
         (r.ocr_block_slot IS NOT NULL AND (r.text_enc IS NOT NULL OR r.ocr_block_slot<0 OR r.ocr_block_slot>=512
          OR b.id IS NULL OR b.screenshot_id!=r.screenshot_id OR length(b.content_enc)<28
          OR r.id<b.first_row_id OR r.id>b.last_row_id))"
    } else {
        "r.text_enc IS NULL OR length(r.text_enc)<28"
    };
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM ocr_results r LEFT JOIN ocr_keys k ON k.id=r.ocr_key_id {join}
         WHERE r.ocr_key_id IS NOT NULL AND (k.id IS NULL OR k.screenshot_id!=r.screenshot_id
            OR length(k.wrapped_key)=0 OR length(k.scope_tag)!=32
            OR (r.text_key_encrypted IS NOT NULL AND length(r.text_key_encrypted)>0)
            OR {content}))"
    );
    let invalid: bool = conn
        .query_row(&sql, [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if invalid {
        Err("Invalid grouped OCR key reference".into())
    } else {
        Ok(())
    }
}

pub(in crate::storage) fn validate_key(
    conn: &Connection,
    id: i64,
    key: &[u8],
) -> Result<(), String> {
    let (owner, tag): (i64, Vec<u8>) = conn
        .query_row(
            "SELECT screenshot_id,scope_tag FROM ocr_keys WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    verify_scope(key, &dataset(conn)?, owner, id, &tag).map_err(|e| e.to_string())
}

pub(in crate::storage) fn validate_members(
    conn: &Connection,
    keys: &HashMap<i64, Zeroizing<Vec<u8>>>,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if !enabled(conn)? {
        return Ok(());
    }
    let dataset = dataset(conn)?;
    let mut counts = HashMap::<i64, i64>::new();
    let has_blocks: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('ocr_results') WHERE name='ocr_block_slot')", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let slot_column = if has_blocks { "ocr_block_slot" } else { "NULL" };
    let sql = format!("SELECT id,screenshot_id,ocr_key_id,text_enc,{slot_column} FROM ocr_results WHERE ocr_key_id IS NOT NULL ORDER BY id");
    let mut blocks = HashMap::<i64, crate::storage::ocr_blocks::Decoded>::new();
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        crate::storage::backup_format::check_cancelled(cancelled).map_err(|e| e.to_string())?;
        let id: i64 = row.get(0).map_err(|e| e.to_string())?;
        let screenshot: i64 = row.get(1).map_err(|e| e.to_string())?;
        let group: i64 = row.get(2).map_err(|e| e.to_string())?;
        let data: Option<Vec<u8>> = row.get(3).map_err(|e| e.to_string())?;
        let slot: Option<i64> = row.get(4).map_err(|e| e.to_string())?;
        let key = keys.get(&group).ok_or("Missing portable OCR group key")?;
        if let Some(slot) = slot {
            if !blocks.contains_key(&group) {
                let (data, first, last): (Vec<u8>, i64, i64) = conn
                    .query_row(
                        "SELECT content_enc,first_row_id,last_row_id FROM ocr_blocks WHERE id=?1",
                        [group],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| e.to_string())?;
                let plain = Zeroizing::new(
                    open(
                        key,
                        &block_aad(&dataset, screenshot, group).map_err(|e| e.to_string())?,
                        &data,
                    )
                    .map_err(|e| e.to_string())?,
                );
                let decoded = crate::storage::ocr_blocks::Decoded::open(&plain)?;
                if decoded.bounds() != (first, last) {
                    return Err("Portable OCR block bounds do not match the payload".into());
                }
                let live: i64 = conn
                    .query_row("SELECT live_rows FROM ocr_keys WHERE id=?1", [group], |r| {
                        r.get(0)
                    })
                    .map_err(|e| e.to_string())?;
                if decoded.len() as i64 != live {
                    return Err("Portable OCR block contains unreferenced records".into());
                }
                if blocks.len() >= MAX_CACHED_KEYS
                    || blocks.values().map(|b| b.size()).sum::<usize>() + decoded.size()
                        > 4 * 1024 * 1024
                {
                    blocks.clear();
                }
                blocks.insert(group, decoded);
            }
            let slot = usize::try_from(slot).map_err(|_| "Invalid portable OCR block slot")?;
            let _record = blocks
                .get(&group)
                .ok_or("Missing portable OCR block")?
                .record(slot, id)?;
        } else {
            let data = data.ok_or("Missing portable inline OCR payload")?;
            let _plain = Zeroizing::new(
                open(
                    key,
                    &aad(&dataset, screenshot, group, id).map_err(|e| e.to_string())?,
                    &data,
                )
                .map_err(|e| e.to_string())?,
            );
        }
        *counts.entry(group).or_default() += 1;
    }
    let mut groups = conn
        .prepare("SELECT id,live_rows FROM ocr_keys")
        .map_err(|e| e.to_string())?;
    let mut rows = groups.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let id: i64 = row.get(0).map_err(|e| e.to_string())?;
        let count: i64 = row.get(1).map_err(|e| e.to_string())?;
        if !keys.contains_key(&id) || counts.get(&id).copied().unwrap_or(0) != count {
            return Err("Portable OCR group membership does not match the archive".into());
        }
    }
    Ok(())
}
