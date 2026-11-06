//! Retire inline browser metadata before publishing the archive connection.
//!
//! v0.6.4 still shipped these columns alongside dedup references, so this
//! schema conversion remains part of the supported direct upgrade path.
//! Moving ciphertext together with its wrapped row key needs no private key.
//! Even an unreadable legacy payload is preserved, without retrying it on every
//! unlock. Existing dedup references remain authoritative. Unconverted values
//! receive an isolated legacy identity; new writes still deduplicate plaintext.

use rusqlite::Connection;

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let columns = conn
        .prepare("PRAGMA table_info(screenshots)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| format!("Failed to inspect browser metadata schema: {error}"))?;
    if !columns
        .iter()
        .any(|name| matches!(name.as_str(), "page_icon_enc" | "visible_links_enc"))
    {
        return Ok(());
    }

    // Copy and column removal commit together. A failed copy or schema change
    // leaves the original archive intact and will be retried at the next open.
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    for (column, reference, table, payload, key) in [
        (
            "page_icon_enc",
            "page_icon_id",
            "page_icons",
            "icon_enc",
            "icon_key_encrypted",
        ),
        (
            "visible_links_enc",
            "link_set_id",
            "link_sets",
            "links_enc",
            "links_key_encrypted",
        ),
    ] {
        if !columns.iter().any(|name| name == column) {
            continue;
        }
        // Missing wrapped keys were already unreadable. Keep their ciphertext
        // and an empty envelope instead of dropping the row or blocking unlock.
        // Legacy link counts stay at the table default; readers decode the JSON.
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE browser_metadata_move AS
             SELECT s.id FROM screenshots s WHERE s.{column} IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM {table} d WHERE d.id = s.{reference});
             INSERT INTO {table} (content_hash, {payload}, {key})
             SELECT 'legacy-inline:' || s.id, s.{column}, COALESCE(s.content_key_encrypted, X'')
             FROM screenshots s
             WHERE s.id IN (SELECT id FROM browser_metadata_move);
             UPDATE screenshots SET {reference} = (
                 SELECT id FROM {table} WHERE content_hash = 'legacy-inline:' || screenshots.id
             ) WHERE id IN (SELECT id FROM browser_metadata_move);
             DROP TABLE browser_metadata_move;
             ALTER TABLE screenshots DROP COLUMN {column};"
        ))
        .map_err(|error| format!("Failed to retire inline {column}: {error}"))?;
    }
    tx.commit()
        .map_err(|error| format!("Failed to commit browser metadata migration: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{credential_manager::CredentialManagerState, storage::StorageState};
    use std::sync::Arc;

    fn v0_6_4_archive() -> (tempfile::TempDir, Connection) {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(temp.path().into()));
        let storage = StorageState::new(temp.path().into(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE screenshots ADD COLUMN page_icon_enc BLOB;
             ALTER TABLE screenshots ADD COLUMN visible_links_enc BLOB;
             WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id<105)
             INSERT INTO screenshots(id,image_path,image_hash,page_icon_enc,visible_links_enc,content_key_encrypted)
             SELECT id, 'test.enc', CAST(id AS TEXT), X'01', X'02', X'03' FROM ids;"
        ).unwrap();
        (temp, conn)
    }

    #[test]
    fn cutover_preserves_unreadable_metadata_and_existing_references_without_unlock() {
        let (_temp, conn) = v0_6_4_archive();
        conn.execute_batch(
            "INSERT INTO page_icons(id,content_hash,icon_enc,icon_key_encrypted) VALUES (1000,'existing',X'04',X'05');
             UPDATE screenshots SET page_icon_id=1000 WHERE id=1;
             UPDATE screenshots SET page_icon_id=1001 WHERE id=3;
             UPDATE screenshots SET content_key_encrypted=NULL WHERE id=2;"
        ).unwrap();
        for _ in 0..2 {
            migrate(&conn).unwrap();
            let preserved: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM screenshots s
                 JOIN page_icons p ON p.id=s.page_icon_id JOIN link_sets l ON l.id=s.link_set_id
                 WHERE l.links_enc=X'02' AND
                    ((s.id=1 AND p.icon_enc=X'04' AND p.icon_key_encrypted=X'05') OR
                     (s.id=2 AND p.icon_enc=X'01' AND p.icon_key_encrypted=X'') OR
                     (s.id>2 AND p.icon_enc=X'01' AND p.icon_key_encrypted=X'03'))",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(preserved, 105);
            assert!(conn
                .prepare("SELECT page_icon_enc, visible_links_enc FROM screenshots")
                .is_err());
        }
    }

    #[test]
    fn failed_cutover_rolls_back_copies_and_column_removal() {
        let (_temp, conn) = v0_6_4_archive();
        conn.execute_batch("CREATE TRIGGER reject_links BEFORE INSERT ON link_sets BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
        assert!(migrate(&conn).is_err());
        let original: i64 = conn.query_row(
            "SELECT COUNT(*) FROM screenshots WHERE page_icon_enc=X'01' AND visible_links_enc=X'02' AND page_icon_id IS NULL",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(original, 105);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM page_icons", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        conn.execute_batch("DROP TRIGGER reject_links").unwrap();
        migrate(&conn).unwrap();
    }
}
