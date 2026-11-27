use super::*;
use sha2::{Digest, Sha256};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS snapshot_links(id INTEGER PRIMARY KEY,hash BLOB NOT NULL UNIQUE,data BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS snapshot_contexts(id INTEGER PRIMARY KEY,hash BLOB NOT NULL UNIQUE,source TEXT,window_title TEXT,process_name TEXT,page_url TEXT);
CREATE TABLE IF NOT EXISTS snapshots(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL,deleted INTEGER NOT NULL DEFAULT 0,
    captured_at_ms INTEGER,width INTEGER,height INTEGER,status TEXT,category TEXT,context_id INTEGER,link_id INTEGER);
CREATE TABLE IF NOT EXISTS retired_snapshot_ranges(first INTEGER PRIMARY KEY,last INTEGER NOT NULL CHECK(last>=first));
CREATE INDEX IF NOT EXISTS deleted_snapshot_revision ON snapshots(revision) WHERE deleted=1;
CREATE INDEX IF NOT EXISTS snapshots_time ON snapshots(captured_at_ms) WHERE deleted=0;
CREATE INDEX IF NOT EXISTS snapshots_context ON snapshots(context_id) WHERE context_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS snapshots_links ON snapshots(link_id) WHERE link_id IS NOT NULL;
CREATE TRIGGER IF NOT EXISTS agent_drop_links AFTER UPDATE OF link_id ON snapshots WHEN OLD.link_id IS NOT NULL AND OLD.link_id IS NOT NEW.link_id BEGIN
    DELETE FROM snapshot_links WHERE id=OLD.link_id AND NOT EXISTS(SELECT 1 FROM snapshots WHERE link_id=OLD.link_id);
END;
CREATE TRIGGER IF NOT EXISTS agent_drop_context AFTER UPDATE OF context_id ON snapshots WHEN OLD.context_id IS NOT NULL AND OLD.context_id IS NOT NEW.context_id BEGIN
    DELETE FROM snapshot_contexts WHERE id=OLD.context_id AND NOT EXISTS(SELECT 1 FROM snapshots WHERE context_id=OLD.context_id);
END;";

fn context_id(conn: &Connection, v: &SnapshotText) -> Result<Option<i64>, String> {
    let context = (&v.source, &v.window_title, &v.process_name, &v.page_url);
    if v.source.is_none()
        && v.window_title.is_none()
        && v.process_name.is_none()
        && v.page_url.is_none()
    {
        return Ok(None);
    }
    let digest = Sha256::digest(serde_json::to_vec(&context).map_err(|_| "AGENT_INVALID_RECORD")?);
    if let Some(id) = db(conn
        .query_row(
            "SELECT id FROM snapshot_contexts WHERE hash=?1",
            [digest.as_slice()],
            |r| r.get(0),
        )
        .optional())?
    {
        return Ok(Some(id));
    }
    db(conn.execute("INSERT INTO snapshot_contexts(hash,source,window_title,process_name,page_url) VALUES(?1,?2,?3,?4,?5)",
        params![digest.as_slice(),v.source,v.window_title,v.process_name,v.page_url]))?;
    Ok(Some(conn.last_insert_rowid()))
}

pub(super) fn put(conn: &Connection, v: &SnapshotText, revision: i64) -> Result<(), String> {
    if db(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE id=?1 AND revision>=?2)",
        params![v.screenshot_id, revision],
        |r| r.get::<_, bool>(0),
    ))? {
        return Ok(());
    }
    let links = if v.visible_links.is_empty() {
        None
    } else {
        let raw = serde_json::to_string(&v.visible_links).map_err(|_| "AGENT_INVALID_LINKS")?;
        let digest = Sha256::digest(&raw);
        let existing = db(conn
            .query_row(
                "SELECT id FROM snapshot_links WHERE hash=?1",
                [digest.as_slice()],
                |r| r.get(0),
            )
            .optional())?;
        match existing {
            Some(id) => Some(id),
            None => {
                let data = payload::encode(raw)?;
                db(conn.execute(
                    "INSERT INTO snapshot_links(hash,data) VALUES(?1,?2)",
                    params![digest.as_slice(), data],
                ))?;
                Some(conn.last_insert_rowid())
            }
        }
    };
    let context = context_id(conn, v)?;
    db(conn.execute("INSERT INTO snapshots VALUES(?1,?2,0,?3,?4,?5,?6,?7,?8,?9)
        ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,deleted=0,captured_at_ms=excluded.captured_at_ms,width=excluded.width,height=excluded.height,
        status=excluded.status,category=excluded.category,context_id=excluded.context_id,link_id=excluded.link_id",
        params![v.screenshot_id,revision,v.captured_at_ms,v.width,v.height,v.status,v.category,context,links]))?;
    Ok(())
}

pub(super) fn delete(conn: &Connection, key: &str, revision: i64) -> Result<usize, String> {
    db(conn.execute("INSERT INTO snapshots(id,revision,deleted) VALUES(?1,?2,1) ON CONFLICT(id) DO UPDATE SET
        revision=excluded.revision,deleted=1,captured_at_ms=NULL,width=NULL,height=NULL,status=NULL,category=NULL,context_id=NULL,link_id=NULL WHERE snapshots.revision<excluded.revision",
        params![key.parse::<i64>().map_err(|_| "AGENT_INVALID_RECORD")?,revision]))
}

/// A live/restored snapshot overrides its historical interval. A missing
/// parent which has never been retired may still be arriving in the outbox.
pub(super) fn retired(conn: &Connection, id: i64) -> Result<bool, String> {
    db(conn.query_row("SELECT COALESCE((SELECT deleted FROM snapshots WHERE id=?1),
        (SELECT last>=?1 FROM retired_snapshot_ranges WHERE first<=?1 ORDER BY first DESC LIMIT 1),0)",[id],|r|r.get(0)))
}

pub(super) fn reclaim(conn: &Connection, floor: i64) -> Result<bool, String> {
    let mut stmt = db(conn.prepare(
        "SELECT id FROM snapshots WHERE deleted=1 AND revision<=?1 ORDER BY revision LIMIT 256",
    ))?;
    let ids =
        db(db(stmt.query_map([floor], |r| r.get::<_, i64>(0)))?
            .collect::<rusqlite::Result<Vec<_>>>())?;
    drop(stmt);
    for &id in &ids {
        let prev:Option<(i64,i64)>=db(conn.query_row("SELECT first,last FROM retired_snapshot_ranges WHERE first<=?1 ORDER BY first DESC LIMIT 1",
            [id],|r|Ok((r.get(0)?,r.get(1)?))).optional())?;
        if !prev.is_some_and(|(_, last)| last >= id) {
            let first = prev
                .filter(|(_, last)| last.checked_add(1) == Some(id))
                .map_or(id, |(first, _)| first);
            let next = id.checked_add(1);
            let upper: Option<i64> = db(conn
                .query_row(
                    "SELECT last FROM retired_snapshot_ranges WHERE first=?1",
                    [next],
                    |r| r.get(0),
                )
                .optional())?;
            if upper.is_some() {
                db(conn.execute("DELETE FROM retired_snapshot_ranges WHERE first=?1", [next]))?;
            }
            db(conn.execute("INSERT INTO retired_snapshot_ranges VALUES(?1,?2) ON CONFLICT(first) DO UPDATE SET last=excluded.last",
                params![first,upper.unwrap_or(id)]))?;
        }
        db(conn.execute("DELETE FROM snapshots WHERE id=?1", [id]))?;
    }
    Ok(ids.len() == 256)
}

pub(crate) struct SearchHeader {
    pub captured_at_ms: Option<i64>,
    pub process_name: Option<String>,
    pub category: Option<String>,
}

impl AgentRepository {
    pub(crate) fn search_headers(&self, ids: &[i64]) -> Result<HashMap<i64, SearchHeader>, String> {
        let mut stmt = db(self.conn.prepare_cached(
            "SELECT s.id,s.captured_at_ms,c.process_name,s.category FROM json_each(?1) j
            CROSS JOIN snapshots s ON s.id=j.value LEFT JOIN snapshot_contexts c ON c.id=s.context_id WHERE s.deleted=0",
        ))?;
        let rows = db(stmt.query_map(
            [serde_json::to_string(ids).map_err(|_| "AGENT_INVALID_IDS")?],
            |r| {
                Ok((
                    r.get(0)?,
                    SearchHeader {
                        captured_at_ms: r.get(1)?,
                        process_name: r.get(2)?,
                        category: r.get(3)?,
                    },
                ))
            },
        ))?;
        db(rows.collect())
    }

    pub(crate) fn snapshots_where(
        &self,
        predicate: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<SnapshotText>, String> {
        let sql=format!("SELECT s.id,s.captured_at_ms,s.width,s.height,c.source,s.status,s.category,c.window_title,c.process_name,c.page_url,s.link_id,l.data
            FROM snapshots s LEFT JOIN snapshot_contexts c ON c.id=s.context_id LEFT JOIN snapshot_links l ON l.id=s.link_id WHERE s.deleted=0 {predicate}");
        let mut stmt = db(self.conn.prepare_cached(&sql))?;
        let mut rows = db(stmt.query(args))?;
        let mut snapshots = Vec::new();
        while let Some(r) = db(rows.next())? {
            let links = if db(r.get::<_, Option<i64>>(10))?.is_some() {
                payload::decode(db(r.get_ref(11))?)?
            } else {
                Vec::new()
            };
            snapshots.push(SnapshotText {
                screenshot_id: db(r.get(0))?,
                captured_at_ms: db(r.get(1))?,
                width: db(r.get(2))?,
                height: db(r.get(3))?,
                source: db(r.get(4))?,
                status: db(r.get(5))?,
                category: db(r.get(6))?,
                window_title: db(r.get(7))?,
                process_name: db(r.get(8))?,
                page_url: db(r.get(9))?,
                visible_links: links,
            });
        }
        Ok(snapshots)
    }
}
