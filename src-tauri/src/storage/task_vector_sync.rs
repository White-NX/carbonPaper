//! Resumable, acknowledged projection of archive vectors into the clustering consumer.

use super::StorageState;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct TaskVectorSync {
    pub scope: String,
    pub target: String,
    pub start_time: f64,
    pub end_time: f64,
    pub upper_id: i64,
    pub cursor: i64,
    pub synced_count: u64,
    pub complete: bool,
}

fn read_state(conn: &rusqlite::Connection) -> Result<Option<TaskVectorSync>, String> {
    conn.query_row(
        "SELECT scope,target,start_time,end_time,upper_id,cursor,synced_count,complete
         FROM task_vector_sync WHERE id=1",
        [],
        |row| {
            Ok(TaskVectorSync {
                scope: row.get(0)?,
                target: row.get(1)?,
                start_time: row.get(2)?,
                end_time: row.get(3)?,
                upper_id: row.get(4)?,
                cursor: row.get(5)?,
                synced_count: row.get::<_, i64>(6)?.max(0) as u64,
                complete: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

impl StorageState {
    pub(crate) fn task_vector_sync_pending(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("task_vector_sync_pending")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM task_vector_sync WHERE complete=0 OR needs_rescan=1)
                OR (NOT EXISTS(SELECT 1 FROM task_vector_sync)
                    AND EXISTS(SELECT 1 FROM derived_embeddings WHERE index_kind='semantic_text'))",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn mark_task_vector_sync_dirty(&self) -> Result<(), String> {
        let guard = self.get_connection_named("mark_task_vector_sync_dirty")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .execute(
                "UPDATE task_vector_sync SET needs_rescan=1 WHERE id=1 AND needs_rescan=0",
                [],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub(crate) fn task_vector_sync_status(&self) -> Result<Option<TaskVectorSync>, String> {
        let guard = self.get_connection_named("task_vector_sync_status")?;
        read_state(guard.as_ref().ok_or("Database not initialized")?)
    }

    pub(crate) fn begin_task_vector_sync(
        &self,
        generation: u64,
        scope: &str,
        target: &str,
        start: f64,
        end: f64,
    ) -> Result<TaskVectorSync, String> {
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end < start
            || target.is_empty()
            || target.len() > 256
            || scope.len() > 128
        {
            return Err("invalid task vector synchronization range".into());
        }
        let guard = self.get_connection_named("begin_task_vector_sync")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        if let Some(state) = read_state(conn)? {
            if !state.complete && state.scope == scope && state.target == target {
                return Ok(state);
            }
        }
        // A new pass scans the entire range, repairing partial gaps as well as
        // a lost collection. A stable high watermark bounds concurrent captures.
        let upper_id: i64 = conn
            .query_row("SELECT COALESCE(MAX(id),0) FROM screenshots", [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO task_vector_sync(id,scope,target,start_time,end_time,upper_id,cursor,synced_count,complete)
             VALUES(1,?1,?2,?3,?4,?5,0,0,0)
             ON CONFLICT(id) DO UPDATE SET scope=excluded.scope,target=excluded.target,
             start_time=excluded.start_time,end_time=excluded.end_time,upper_id=excluded.upper_id,
             cursor=0,synced_count=0,complete=0,needs_rescan=0",
            params![scope,target,start,end,upper_id],
        ).map_err(|e| e.to_string())?;
        read_state(conn)?.ok_or_else(|| "missing task vector synchronization state".into())
    }

    pub(crate) fn task_vector_sync_page(
        &self,
        generation: u64,
        state: &TaskVectorSync,
        limit: u32,
    ) -> Result<Vec<i64>, String> {
        let guard = self.get_connection_named("task_vector_sync_page")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        let mut stmt = conn
            .prepare(
                "SELECT id FROM screenshots WHERE id>?1 AND id<=?2 AND is_deleted=0
             AND CAST(strftime('%s',created_at) AS REAL)>=?3
             AND CAST(strftime('%s',created_at) AS REAL)<=?4 ORDER BY id LIMIT ?5",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    state.cursor,
                    state.upper_id,
                    state.start_time,
                    state.end_time,
                    limit.clamp(1, 128)
                ],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())
    }

    /// Called only after the consumer acknowledges the full page. Replaying a
    /// page after a lost reply is safe because the consumer upserts by screenshot ID.
    pub(crate) fn acknowledge_task_vector_sync(
        &self,
        generation: u64,
        state: &TaskVectorSync,
        cursor: i64,
        count: usize,
        complete: bool,
    ) -> Result<bool, String> {
        if cursor < state.cursor || cursor > state.upper_id {
            return Err("invalid sync cursor".into());
        }
        let guard = self.get_connection_named("acknowledge_task_vector_sync")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let changed = tx
            .execute(
                "UPDATE task_vector_sync SET cursor=?1,synced_count=synced_count+?2,complete=?3
             WHERE id=1 AND scope=?4 AND target=?5 AND cursor=?6 AND upper_id=?7 AND complete=0",
                params![
                    cursor,
                    count as i64,
                    complete,
                    state.scope,
                    state.target,
                    state.cursor,
                    state.upper_id
                ],
            )
            .map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("stale task vector synchronization cursor".into());
        }
        // Observe the dirty marker inside the same transaction as completion.
        // A producer racing the final page cannot disappear between the read
        // and the acknowledgement.
        let rescan: bool = tx
            .query_row(
                "SELECT needs_rescan FROM task_vector_sync WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rescan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use std::sync::Arc;

    fn storage() -> (tempfile::TempDir, StorageState) {
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageState::new(
            dir.path().to_path_buf(),
            Arc::new(CredentialManagerState::new(dir.path().to_path_buf())),
        );
        let conn = rusqlite::Connection::open(dir.path().join("sync.db")).unwrap();
        storage.init_tables(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO screenshots(id,image_path,image_hash,created_at) VALUES
            (1,'one','one','2020-01-01 00:00:00'),(2,'two','two','2020-01-02 00:00:00'),
            (3,'three','three','2020-01-03 00:00:00');",
        )
        .unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        (dir, storage)
    }

    #[test]
    fn updates_during_a_pass_survive_restart_and_require_a_new_pass() {
        let (dir, storage) = storage();
        let first = storage
            .begin_task_vector_sync(0, "recent", "target", 0.0, 2_000_000_000.0)
            .unwrap();
        storage
            .acknowledge_task_vector_sync(0, &first, 2, 2, false)
            .unwrap();
        storage.mark_task_vector_sync_dirty().unwrap();
        *storage.db.lock().unwrap() = None;
        *storage.db.lock().unwrap() =
            Some(rusqlite::Connection::open(dir.path().join("sync.db")).unwrap());
        let resumed = storage
            .begin_task_vector_sync(0, "recent", "target", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(resumed.cursor, 2);
        assert!(storage
            .acknowledge_task_vector_sync(0, &resumed, 3, 1, true)
            .unwrap());
        let next = storage
            .begin_task_vector_sync(0, "recent", "target", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(next.cursor, 0);
        assert!(!storage
            .acknowledge_task_vector_sync(0, &next, 3, 3, true)
            .unwrap());
    }

    #[test]
    fn updates_after_last_ack_cannot_be_lost_at_scheduler_completion() {
        let (_dir, storage) = storage();
        let kind = crate::background_scheduler::TASK_VECTOR_SYNC;
        storage.enqueue_background_task(kind, false, 100).unwrap();
        storage
            .mark_background_task_started(kind, 1, false, 200)
            .unwrap();
        let pass = storage
            .begin_task_vector_sync(0, "recent", "target", 0.0, 2_000_000_000.0)
            .unwrap();
        assert!(!storage
            .acknowledge_task_vector_sync(0, &pass, 3, 3, true)
            .unwrap());
        storage.mark_task_vector_sync_dirty().unwrap();
        storage.enqueue_background_task(kind, false, 300).unwrap();
        storage
            .mark_background_task_succeeded(kind, false, 400)
            .unwrap();
        let task = storage.background_scheduler_task(kind).unwrap().unwrap();
        assert_eq!(task.status, "queued");
        assert_eq!(task.last_completed_at_ms, None);
    }

    #[test]
    fn old_ranges_resume_only_acknowledged_pages_and_restart_after_completion() {
        let (_dir, storage) = storage();
        let state = storage
            .begin_task_vector_sync(0, "range", "collection", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(
            storage.task_vector_sync_page(0, &state, 1).unwrap(),
            vec![1]
        );
        // A failed or lost write has no acknowledgement, so a restart repeats it.
        assert_eq!(
            storage
                .begin_task_vector_sync(0, "range", "collection", 0.0, 2_000_000_000.0)
                .unwrap(),
            state
        );
        storage
            .acknowledge_task_vector_sync(0, &state, 1, 1, false)
            .unwrap();
        assert!(storage
            .acknowledge_task_vector_sync(0, &state, 1, 1, false)
            .is_err());
        *storage.db.lock().unwrap() = None;
        *storage.db.lock().unwrap() =
            Some(rusqlite::Connection::open(_dir.path().join("sync.db")).unwrap());
        let resumed = storage
            .begin_task_vector_sync(0, "range", "collection", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(
            storage.task_vector_sync_page(0, &resumed, 128).unwrap(),
            vec![2, 3]
        );
        storage
            .acknowledge_task_vector_sync(0, &resumed, 3, 2, true)
            .unwrap();
        let next = storage
            .begin_task_vector_sync(0, "range", "collection", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(next.cursor, 0); // Recheck a partial consumer loss on the next run.
    }

    #[test]
    fn replaced_collection_resets_progress_and_stale_database_cannot_acknowledge() {
        let (_dir, storage) = storage();
        let first = storage
            .begin_task_vector_sync(0, "recent", "old", 0.0, 2_000_000_000.0)
            .unwrap();
        storage
            .acknowledge_task_vector_sync(0, &first, 2, 2, false)
            .unwrap();
        let next = storage
            .begin_task_vector_sync(0, "recent", "new", 0.0, 2_000_000_000.0)
            .unwrap();
        assert_eq!(next.cursor, 0);
        assert!(storage
            .acknowledge_task_vector_sync(0, &first, 3, 1, true)
            .is_err());
        storage.bump_db_generation();
        assert!(storage
            .acknowledge_task_vector_sync(0, &next, 3, 3, true)
            .is_err());
    }

    #[test]
    fn pages_exclude_deleted_rows_and_honor_range_and_high_watermark() {
        let (_dir, storage) = storage();
        let state = storage
            .begin_task_vector_sync(0, "range", "target", 1_577_923_200.0, 2_000_000_000.0)
            .unwrap();
        let guard = storage.get_connection_named("test").unwrap();
        guard.as_ref().unwrap().execute_batch("UPDATE screenshots SET is_deleted=1 WHERE id=2;
            INSERT INTO screenshots(id,image_path,image_hash,created_at) VALUES(4,'new','new','2020-01-04 00:00:00');").unwrap();
        drop(guard);
        assert_eq!(
            storage.task_vector_sync_page(0, &state, 128).unwrap(),
            vec![3]
        );
    }
}
