use super::*;
use rusqlite::types::{Value, ValueRef};

const MAX_COMPRESSED_INPUT: usize = 32 * 1024 * 1024;

/// Ordinary JSON remains TEXT. A BLOB carries a bounded length prefix and
/// one zstd frame. SQL predicates only inspect JSON for uncompressed kinds.
pub(super) fn encode(raw: String) -> Result<Value, String> {
    let n = raw.len();
    if (256..=MAX_COMPRESSED_INPUT).contains(&n) {
        let frame =
            zstd::bulk::compress(raw.as_bytes(), 1).map_err(|_| "AGENT_COMPRESSION_FAILED")?;
        if frame.len() + 4 < n {
            let mut stored = Vec::with_capacity(4 + frame.len());
            stored.extend_from_slice(&(n as u32).to_le_bytes());
            stored.extend_from_slice(&frame);
            return Ok(Value::Blob(stored));
        }
    }
    Ok(Value::Text(raw))
}

pub(super) fn decode<T: DeserializeOwned>(value: ValueRef<'_>) -> Result<T, String> {
    let plain;
    let bytes = match value {
        ValueRef::Text(raw) => raw,
        ValueRef::Blob(blob) => {
            let (n, frame) = blob.split_at_checked(4).ok_or("AGENT_INVALID_RECORD")?;
            let n = u32::from_le_bytes(n.try_into().unwrap()) as usize;
            if n > MAX_COMPRESSED_INPUT {
                return Err("AGENT_INVALID_RECORD".into());
            }
            plain = Zeroizing::new(
                zstd::bulk::decompress(frame, n).map_err(|_| "AGENT_INVALID_RECORD")?,
            );
            if plain.len() != n {
                return Err("AGENT_INVALID_RECORD".into());
            }
            &plain
        }
        _ => return Err("AGENT_INVALID_RECORD".into()),
    };
    serde_json::from_slice(bytes).map_err(|_| "AGENT_INVALID_RECORD".into())
}

impl AgentRepository {
    pub(crate) fn json_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<T>, String> {
        let mut stmt = db(self.conn.prepare_cached(sql))?;
        let mut cursor = db(stmt.query(params))?;
        let mut out = Vec::new();
        while let Some(r) = db(cursor.next())? {
            out.push(decode(db(r.get_ref(0))?)?);
        }
        Ok(out)
    }
}
