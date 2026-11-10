//! Share independently encrypted application icons without changing the wire metadata.
use super::StorageState;
use crate::credential_manager::{
    decrypt_row_key_with_cng_silent, decrypt_with_master_key, encrypt_with_master_key,
    CredentialError,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use zeroize::Zeroizing;

const CURSOR: &str = "storage_process_icon_cursor_v1";

fn take_icon(metadata: &mut Value) -> Option<String> {
    let object = metadata.as_object_mut()?;
    let icon = object.get("process_icon")?.as_str()?.to_owned();
    if icon.is_empty() {
        return None;
    }
    object.remove("process_icon");
    Some(icon)
}

pub(super) fn restore_metadata_icon(
    metadata: Option<String>,
    icon: Option<String>,
) -> Option<String> {
    let Some(icon) = icon else { return metadata };
    let Some(mut value) = metadata
        .as_ref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    else {
        return metadata;
    };
    let Some(object) = value.as_object_mut() else {
        return metadata;
    };
    object.insert("process_icon".into(), Value::String(icon));
    serde_json::to_string(&value).ok()
}

struct LegacyMetadata {
    id: i64,
    plain: Option<String>,
    encrypted: Option<Vec<u8>>,
    wrapped_key: Option<Vec<u8>>,
    icon_id: Option<i64>,
    deleted: bool,
}

struct Rewrite {
    source: LegacyMetadata,
    metadata_enc: Vec<u8>,
    icon: Zeroizing<String>,
}

fn prepare(
    source: LegacyMetadata,
    unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
) -> Result<Option<Rewrite>, CredentialError> {
    if source.icon_id.is_some() {
        return Ok(None);
    }
    let (Some(encrypted), Some(wrapped_key)) = (&source.encrypted, &source.wrapped_key) else {
        return Ok(None);
    };
    let key = Zeroizing::new(unwrap(wrapped_key)?);
    let plain = Zeroizing::new(decrypt_with_master_key(&key, encrypted)?);
    let Ok(mut value) = serde_json::from_slice::<Value>(&plain) else {
        // Preserve non-JSON legacy metadata exactly as it is.
        return Ok(None);
    };
    let Some(icon) = take_icon(&mut value) else {
        return Ok(None);
    };
    let compact = Zeroizing::new(serde_json::to_vec(&value).expect("JSON value serializes"));
    let metadata_enc = encrypt_with_master_key(&key, &compact)?;
    Ok(Some(Rewrite {
        source,
        metadata_enc,
        icon: Zeroizing::new(icon),
    }))
}

fn publish(
    conn: &Connection,
    rewrites: Vec<Rewrite>,
    next: i64,
    get_icon: &mut impl FnMut(&Connection, &str) -> Result<i64, String>,
) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut stale = false;
    for rewrite in rewrites {
        let source = rewrite.source;
        let unchanged: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND metadata IS ?2
                AND metadata_enc IS ?3 AND content_key_encrypted IS ?4
                AND process_icon_id IS NULL AND is_deleted=?5)",
                params![
                    source.id,
                    source.plain,
                    source.encrypted,
                    source.wrapped_key,
                    source.deleted
                ],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !unchanged {
            stale = true;
            continue;
        }
        let icon_id = get_icon(&tx, &rewrite.icon)?;
        tx.execute(
            "UPDATE screenshots SET metadata=NULL,metadata_enc=?2,process_icon_id=?3 WHERE id=?1",
            params![source.id, rewrite.metadata_enc, icon_id],
        )
        .map_err(|e| e.to_string())?;
    }
    // A racing metadata edit is reread on the next page, not overwritten or
    // silently lost behind the cursor. Deleted IDs simply disappear on reread.
    if !stale {
        tx.execute(
            "INSERT INTO app_metadata(key,value) VALUES(?1,?2)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![CURSOR, next.to_string()],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

impl StorageState {
    pub(super) fn serialize_metadata_with_shared_icon(
        &self,
        conn: &Connection,
        metadata: Option<&Value>,
    ) -> Result<(Option<String>, Option<i64>), String> {
        let Some(mut metadata) = metadata.cloned() else {
            return Ok((None, None));
        };
        let id = take_icon(&mut metadata)
            .map(|icon| self.get_or_create_page_icon_id(conn, &icon))
            .transpose()?;
        Ok((
            Some(serde_json::to_string(&metadata).map_err(|e| e.to_string())?),
            id,
        ))
    }

    pub(super) fn compact_process_icons_batch(&self) -> Result<usize, String> {
        if !self.is_silent_read_authorized() {
            return Ok(0);
        }
        let (generation, rows) = {
            let guard = self.get_connection_named("compact_process_icons_read")?;
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
            let mut stmt = conn.prepare(
                "SELECT id,metadata,metadata_enc,content_key_encrypted,process_icon_id,is_deleted
                 FROM screenshots WHERE id>?1 ORDER BY id LIMIT 16",
            ).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([cursor], |r| {
                    Ok(LegacyMetadata {
                        id: r.get(0)?,
                        plain: r.get(1)?,
                        encrypted: r.get(2)?,
                        wrapped_key: r.get(3)?,
                        icon_id: r.get(4)?,
                        deleted: r.get(5)?,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            (self.db_generation(), rows)
        };
        let count = rows.len();
        let next = rows.last().map_or(i64::MAX, |row| row.id);
        let mut rewrites = Vec::new();
        for row in rows {
            if !self.is_silent_read_authorized() || self.is_database_maintenance_pending() {
                return Ok(0);
            }
            let id = row.id;
            match prepare(row, &|wrapped| {
                decrypt_row_key_with_cng_silent(&self.credential_state, wrapped)
            }) {
                Ok(Some(rewrite)) => rewrites.push(rewrite),
                Ok(None) => {}
                Err(CredentialError::AuthRequired) => return Ok(0),
                Err(_) => tracing::warn!(
                    "[DB:COMPACT] Preserved unreadable legacy metadata for screenshot {id}"
                ),
            }
        }
        let guard = self.get_connection_named("compact_process_icons_publish")?;
        if self.db_generation() != generation
            || !self.is_silent_read_authorized()
            || self.is_database_maintenance_pending()
        {
            return Ok(0);
        }
        publish(
            guard.as_ref().ok_or("Database not initialized")?,
            rewrites,
            next,
            &mut |conn, icon| self.get_or_create_page_icon_id(conn, icon),
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
