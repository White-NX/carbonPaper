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
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ocr_results r LEFT JOIN ocr_keys k ON k.id=r.ocr_key_id
         WHERE r.ocr_key_id IS NOT NULL AND (k.id IS NULL OR k.screenshot_id!=r.screenshot_id
            OR length(k.wrapped_key)=0 OR length(k.scope_tag)!=32
            OR (r.text_key_encrypted IS NOT NULL AND length(r.text_key_encrypted)>0)
            OR r.text_enc IS NULL OR length(r.text_enc)<28))",
            [],
            |r| r.get(0),
        )
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
    validate_key_fields(&dataset(conn)?, owner, id, &tag, key)
}

pub(in crate::storage) fn validate_key_fields(
    dataset: &str,
    owner: i64,
    id: i64,
    tag: &[u8],
    key: &[u8],
) -> Result<(), String> {
    verify_scope(key, dataset, owner, id, tag).map_err(|e| e.to_string())
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
    let mut scratch = carbonpaper_backup_core::ZeroizingBuffer::new();
    let mut counts = HashMap::<i64, i64>::new();
    let mut stmt = conn.prepare("SELECT id,screenshot_id,ocr_key_id,text_enc FROM ocr_results WHERE ocr_key_id IS NOT NULL ORDER BY id")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        crate::storage::backup_format::check_cancelled(cancelled).map_err(|e| e.to_string())?;
        let id: i64 = row.get(0).map_err(|e| e.to_string())?;
        let screenshot: i64 = row.get(1).map_err(|e| e.to_string())?;
        let group: i64 = row.get(2).map_err(|e| e.to_string())?;
        let data = row
            .get_ref(3)
            .map_err(|e| e.to_string())?
            .as_blob()
            .map_err(|e| e.to_string())?;
        let key = keys.get(&group).ok_or("Missing portable OCR group key")?;
        carbonpaper_app_bound::crypto::authenticate_storage_bytes(
            key,
            data,
            &aad(&dataset, screenshot, group, id).map_err(|e| e.to_string())?,
            &mut scratch,
        )?;
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
