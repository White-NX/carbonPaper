//! Bounded page-cache policy for the lazy indexer's read/modify/write transaction.

use rusqlite::Connection;

pub(super) const POSTINGS_CACHE_KIB: i64 = 16 * 1024;

// Preserve the existing rowid and token index entry instead of deleting and
// reinserting them for every bitmap merge.
#[cfg(test)]
pub(super) const UPSERT_POSTING_SQL: &str =
    "INSERT INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)
     ON CONFLICT(token_hash) DO UPDATE SET postings_blob = excluded.postings_blob";

pub(super) fn ordered_token_hashes<'a>(hashes: impl Iterator<Item = &'a String>) -> Vec<&'a str> {
    let mut ordered: Vec<&str> = hashes.map(String::as_str).collect();
    ordered.sort_unstable();
    ordered
}

/// A multi-megabyte posting batch otherwise repeatedly evicts and decrypts pages
/// with SQLite's default ~2 MiB cache. Raise the budget only for this transaction,
/// preserving larger configured budgets and restoring the exact setting on exit.
/// This does not change synchronous, checkpoint, or cache-spill policy.
pub(super) struct PostingsCacheBudget<'a> {
    conn: &'a Connection,
    previous: Option<i64>,
}

impl<'a> PostingsCacheBudget<'a> {
    pub(super) fn new(conn: &'a Connection) -> rusqlite::Result<Self> {
        let previous: i64 = conn.pragma_query_value(None, "cache_size", |row| row.get(0))?;
        // A negative setting is KiB; a positive setting counts database pages.
        let previous_bytes = if previous < 0 {
            previous.saturating_neg().saturating_mul(1024)
        } else {
            let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
            previous.saturating_mul(page_size)
        };
        let changed = previous_bytes < POSTINGS_CACHE_KIB * 1024;
        if changed {
            conn.pragma_update(None, "cache_size", -POSTINGS_CACHE_KIB)?;
        }
        Ok(Self {
            conn,
            previous: changed.then_some(previous),
        })
    }
}

impl Drop for PostingsCacheBudget<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            if let Err(error) = self.conn.pragma_update(None, "cache_size", previous) {
                tracing::warn!(%error, "Failed to restore lazy-indexer page cache budget");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_size(conn: &Connection) -> i64 {
        conn.pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn cache_budget_restores_kib_and_page_settings_and_preserves_larger_budgets() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "page_size", 4096).unwrap();
        for original in [-2000, 0, 128, -32768, 8192] {
            conn.pragma_update(None, "cache_size", original).unwrap();
            {
                let _budget = PostingsCacheBudget::new(&conn).unwrap();
                assert_eq!(
                    cache_size(&conn),
                    if original == -32768 || original == 8192 {
                        original
                    } else {
                        -POSTINGS_CACHE_KIB
                    }
                );
            }
            assert_eq!(cache_size(&conn), original);
        }
    }

    #[test]
    fn failed_transaction_restores_cache_and_rolls_back_posting_updates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE blind_bitmap_index(token_hash TEXT PRIMARY KEY, postings_blob BLOB NOT NULL);
            INSERT INTO blind_bitmap_index VALUES ('existing', X'01');").unwrap();
        conn.pragma_update(None, "cache_size", -2000).unwrap();
        let result = (|| -> rusqlite::Result<()> {
            let _budget = PostingsCacheBudget::new(&conn)?;
            let tx = conn.unchecked_transaction()?;
            tx.execute(UPSERT_POSTING_SQL, rusqlite::params!["existing", vec![2u8]])?;
            tx.execute(
                UPSERT_POSTING_SQL,
                rusqlite::params!["invalid", Option::<Vec<u8>>::None],
            )?;
            tx.commit()
        })();
        assert!(result.is_err());
        assert_eq!(cache_size(&conn), -2000);
        assert!(conn.is_autocommit());
        let stored: Vec<u8> = conn
            .query_row(
                "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash='existing'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, vec![1]);
    }

    #[test]
    fn posting_upsert_keeps_rowid_and_inserts_new_tokens() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE blind_bitmap_index(token_hash TEXT PRIMARY KEY, postings_blob BLOB NOT NULL);
            INSERT INTO blind_bitmap_index(rowid, token_hash, postings_blob) VALUES (41, 'existing', X'01');").unwrap();
        {
            let _budget = PostingsCacheBudget::new(&conn).unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute(UPSERT_POSTING_SQL, rusqlite::params!["existing", vec![2u8]])
                .unwrap();
            tx.execute(UPSERT_POSTING_SQL, rusqlite::params!["new", vec![3u8]])
                .unwrap();
            tx.commit().unwrap();
        }
        let existing: (i64, Vec<u8>) = conn
            .query_row(
                "SELECT rowid, postings_blob FROM blind_bitmap_index WHERE token_hash='existing'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(existing, (41, vec![2]));
        let added: Vec<u8> = conn
            .query_row(
                "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash='new'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(added, vec![3]);
    }
}
