use super::*;

/// Checkpoints are sealed only after the source write lock excludes in-flight
/// transactions, backfill has finished, and every gap and outbox row is drained.
impl AgentRepository {
    pub fn seal_revision(&self, revision: i64) -> Result<(), String> {
        db(self.conn.execute("INSERT INTO agent_metadata(key,value) VALUES('floor',?1)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE CAST(value AS INTEGER)<CAST(excluded.value AS INTEGER)",[revision.to_string()]))?;
        Ok(())
    }

    pub fn reclaim_batch(&mut self) -> Result<bool, String> {
        let tx = db(self.conn.transaction())?;
        let floor:i64=db(tx.query_row("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM agent_metadata WHERE key='floor'),0)",[],|r|r.get(0)))?;
        let mut more = snapshots::reclaim(&tx, floor)?;
        for (table, dead) in [
            ("ocr_access", "key_envelope IS NULL"),
            ("vector_access", "key_envelope IS NULL"),
            ("block_access", "envelope IS NULL"),
            ("recap_search_access", "key_envelope IS NULL"),
            (
                "records",
                "body IS NULL AND kind NOT IN ('cluster','calendar')",
            ),
        ] {
            let sql=format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE {dead} AND revision<=?1 LIMIT 256)");
            more |= db(tx.execute(&sql, [floor]))? == 256;
        }
        db(tx.commit())?;
        let pages: i64 = db(self
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0)))?;
        if pages >= 64 {
            // SQLite yields after each reclaimed page. Finalizing the statement
            // at its first row would reclaim only one page per maintenance pass.
            let mut stmt = db(self.conn.prepare("PRAGMA incremental_vacuum(64)"))?;
            let mut rows = db(stmt.query([]))?;
            while db(rows.next())?.is_some() {}
        }
        Ok(more || pages >= 128)
    }
}

pub(super) const INDEXES:&str="
CREATE INDEX IF NOT EXISTS retired_vector_revision ON vector_access(revision) WHERE key_envelope IS NULL;
CREATE INDEX IF NOT EXISTS retired_ocr_revision ON ocr_access(revision) WHERE key_envelope IS NULL;
CREATE INDEX IF NOT EXISTS retired_block_revision ON block_access(revision) WHERE envelope IS NULL;
CREATE INDEX IF NOT EXISTS retired_recap_revision ON recap_search_access(revision) WHERE key_envelope IS NULL;
CREATE INDEX IF NOT EXISTS retired_record_revision ON records(revision) WHERE body IS NULL AND kind NOT IN ('cluster','calendar');";
