//! Compact content flags shared by the recap calendar and MCP date discovery.
use super::StorageState;
use rusqlite::{params, Connection};
use serde::{de::IgnoredAny, Deserialize};

pub(super) const CONTENT_DAYS_SQL: &str = "SELECT d.day FROM recap_days d
     WHERE d.privacy_hash=?1
     AND d.day NOT IN (SELECT day FROM recap_deleted_days)
     AND EXISTS(SELECT 1 FROM recap_payloads p
         JOIN recap_batch_content c ON c.key=p.key AND c.has_content=1
         WHERE p.day=d.day AND p.kind='batch' AND p.revision=d.revision)
     ORDER BY d.day DESC LIMIT 730";

const PENDING_CONTENT_SQL: &str = "SELECT p.key,p.data,p.row_key FROM recap_days d
     JOIN recap_payloads p ON p.day=d.day AND p.kind='batch' AND p.revision=d.revision
     WHERE d.privacy_hash=?1
     AND p.key NOT IN (SELECT key FROM recap_batch_content)
     AND d.day NOT IN (SELECT day FROM recap_deleted_days)
     AND NOT EXISTS(SELECT 1 FROM recap_payloads known
         JOIN recap_batch_content content ON content.key=known.key AND content.has_content=1
         WHERE known.day=d.day AND known.kind='batch' AND known.revision=d.revision)
     ORDER BY d.day DESC,p.key LIMIT 64";

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recap_batch_content (
            key TEXT PRIMARY KEY, has_content INTEGER NOT NULL CHECK(has_content IN (0,1)))
            WITHOUT ROWID;
         CREATE TRIGGER IF NOT EXISTS recap_content_batch_insert AFTER INSERT ON recap_payloads
            WHEN NEW.kind='batch' BEGIN
            DELETE FROM recap_batch_content WHERE key=NEW.key; END;
         CREATE TRIGGER IF NOT EXISTS recap_content_batch_update AFTER UPDATE ON recap_payloads
            WHEN OLD.kind='batch' OR NEW.kind='batch' BEGIN
            DELETE FROM recap_batch_content WHERE key IN (OLD.key,NEW.key); END;
         CREATE TRIGGER IF NOT EXISTS recap_content_batch_delete AFTER DELETE ON recap_payloads
            WHEN OLD.kind='batch' BEGIN
            DELETE FROM recap_batch_content WHERE key=OLD.key; END;",
    )
    .map_err(|e| e.to_string())
}

/// Inspect only the activity array without retaining any private text. Empty
/// periods and failures without retained activities are not recap content.
pub(super) fn has_content(bytes: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct BatchContent {
        #[serde(default)]
        activities: Vec<IgnoredAny>,
    }
    serde_json::from_slice::<BatchContent>(bytes).is_ok_and(|batch| !batch.activities.is_empty())
}

pub(super) fn record_content(
    conn: &Connection,
    key: &str,
    has_content: bool,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO recap_batch_content(key,has_content) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET has_content=excluded.has_content",
        params![key, has_content],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

struct PendingContent {
    key: String,
    data: Vec<u8>,
    row_key: Vec<u8>,
}

impl StorageState {
    pub(crate) fn recap_list_days(&self, privacy: &str) -> Result<Vec<String>, String> {
        self.recap_backfill_content(privacy)?;
        self.with_read_connection_named("recap_list_days", |conn| {
            let mut statement = conn.prepare(CONTENT_DAYS_SQL).map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([privacy], |row| row.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string());
            rows
        })
    }

    /// Older databases acquire flags lazily. Each read lease ends before CNG
    /// decryption; later polls use only the compact flags. Once a day's content
    /// is confirmed, its remaining legacy batches need no discovery work.
    fn recap_backfill_content(&self, privacy: &str) -> Result<(), String> {
        let generation = self.db_generation();
        loop {
            let pending = self.with_read_connection_named("recap_content_pending", |conn| {
                let mut statement = conn
                    .prepare(PENDING_CONTENT_SQL)
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([privacy], |row| {
                        Ok(PendingContent {
                            key: row.get(0)?,
                            data: row.get(1)?,
                            row_key: row.get(2)?,
                        })
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string());
                rows
            })?;
            if pending.is_empty() {
                return Ok(());
            }
            let content = pending
                .iter()
                .map(|batch| {
                    // Match recap reads: inability to unwrap a key must not
                    // become a durable assertion that the batch is empty.
                    let mut bytes = self
                        .decrypt_payload_with_row_key_silent(&batch.data, &batch.row_key)
                        .map_err(|_| "AUTH_REQUIRED".to_owned())?;
                    let content = has_content(&bytes);
                    Self::zeroize_bytes(&mut bytes);
                    Ok(content)
                })
                .collect::<Result<Vec<_>, String>>()?;
            let guard = self.get_connection_named("recap_content_backfill")?;
            if generation != self.db_generation() {
                return Err("RECAP_SOURCE_CHANGED".into());
            }
            let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            for (batch, content) in pending.iter().zip(content) {
                // A regeneration may replace a batch at the same revision
                // while it is being decrypted. Never classify the replacement
                // from the old ciphertext, or resurrect a deleted flag.
                tx.execute(
                    "INSERT OR IGNORE INTO recap_batch_content(key,has_content)
                     SELECT key,?2 FROM recap_payloads
                     WHERE kind='batch' AND key=?1 AND data=?3 AND row_key=?4",
                    params![batch.key, content, batch.data, batch.row_key],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
    }
}
