//! Durable state for the process-wide automatic-work scheduler.
//!
//! The rows here deliberately contain scheduling metadata only. MiniLM, CLIP,
//! Smart Cluster, and Python retain their actual work queues in their existing
//! tables/collections, so a scheduler restart can never lose a subject.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::StorageState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundTaskState {
    pub task_kind: String,
    pub ready_since_ms: i64,
    pub next_attempt_at_ms: i64,
    pub failure_count: u32,
    pub last_served_seq: u64,
    pub last_error: Option<String>,
    pub last_completed_at_ms: Option<i64>,
    pub status: String,
    pub manual_pending: bool,
}

impl BackgroundTaskState {
    pub fn is_eligible(&self, now_ms: i64) -> bool {
        matches!(self.status.as_str(), "queued" | "retry_wait") && self.next_attempt_at_ms <= now_ms
    }
}

fn row_to_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<BackgroundTaskState> {
    Ok(BackgroundTaskState {
        task_kind: row.get(0)?,
        ready_since_ms: row.get(1)?,
        next_attempt_at_ms: row.get(2)?,
        failure_count: row.get::<_, i64>(3)?.max(0) as u32,
        last_served_seq: row.get::<_, i64>(4)?.max(0) as u64,
        last_error: row.get(5)?,
        last_completed_at_ms: row.get(6)?,
        status: row.get(7)?,
        manual_pending: row.get::<_, i64>(8)? != 0,
    })
}

impl StorageState {
    pub fn recover_background_scheduler_tasks(&self) -> Result<(), String> {
        let guard = self.get_connection_named("recover_background_scheduler_tasks")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'queued', next_attempt_at_ms = 0,
                 manual_pending = CASE WHEN manual_in_flight = 1 THEN 1 ELSE manual_pending END,
                 manual_in_flight = 0,
                 last_error = COALESCE(last_error, 'application restarted during slice')
             WHERE status = 'running'",
            [],
        )
        .map_err(|e| format!("Failed to recover scheduler tasks: {e}"))?;
        // `degraded` is intentionally process-scoped: it means this process
        // exhausted its automatic monitor-restart budget. A fresh process is
        // the documented recovery boundary, so make the durable row runnable
        // again on startup while retaining the last error for diagnostics.
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'queued', next_attempt_at_ms = 0
             WHERE status = 'degraded'",
            [],
        )
        .map_err(|e| format!("Failed to recover degraded scheduler tasks: {e}"))?;
        Ok(())
    }

    pub fn background_scheduler_tasks(&self) -> Result<Vec<BackgroundTaskState>, String> {
        let guard = self.get_connection_named("background_scheduler_tasks")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT task_kind, ready_since_ms, next_attempt_at_ms, failure_count,
                        last_served_seq, last_error, last_completed_at_ms, status,
                        manual_pending
                 FROM background_scheduler_tasks",
            )
            .map_err(|e| format!("Failed to prepare scheduler state query: {e}"))?;
        let rows = stmt
            .query_map([], row_to_state)
            .map_err(|e| format!("Failed to read scheduler state: {e}"))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("Failed to decode scheduler state: {e}"))
    }

    /// Add or wake a task. Duplicate enqueue notifications coalesce into one
    /// row; a manual notification is retained until a successful slice.
    pub fn enqueue_background_task(
        &self,
        task_kind: &str,
        manual: bool,
        now_ms: i64,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("enqueue_background_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            r#"
            INSERT INTO background_scheduler_tasks
                (task_kind, ready_since_ms, next_attempt_at_ms, status, manual_pending)
            VALUES (?1, ?2, 0, 'queued', ?3)
            ON CONFLICT(task_kind) DO UPDATE SET
                manual_pending = CASE
                    WHEN excluded.manual_pending = 1 THEN 1
                    ELSE background_scheduler_tasks.manual_pending
                END,
                 status = CASE
                    WHEN excluded.manual_pending = 1
                         AND background_scheduler_tasks.status IN ('retry_wait', 'degraded')
                        THEN 'queued'
                    WHEN background_scheduler_tasks.status = 'parked'
                        THEN 'queued'
                    WHEN background_scheduler_tasks.status IN ('completed', 'failed')
                        THEN 'queued'
                    ELSE background_scheduler_tasks.status
                END,
                 ready_since_ms = CASE
                    WHEN excluded.manual_pending = 1
                         AND background_scheduler_tasks.status IN ('retry_wait', 'degraded')
                        THEN excluded.ready_since_ms
                    WHEN background_scheduler_tasks.status = 'parked'
                        THEN excluded.ready_since_ms
                    WHEN background_scheduler_tasks.status IN ('completed', 'failed')
                        THEN excluded.ready_since_ms
                    ELSE background_scheduler_tasks.ready_since_ms
                END,
                 next_attempt_at_ms = CASE
                    WHEN excluded.manual_pending = 1
                         AND background_scheduler_tasks.status IN ('retry_wait', 'degraded')
                        THEN 0
                    WHEN background_scheduler_tasks.status = 'parked'
                        THEN 0
                    WHEN background_scheduler_tasks.status IN ('completed', 'failed')
                        THEN 0
                    ELSE background_scheduler_tasks.next_attempt_at_ms
                END,
                last_error = CASE
                    WHEN background_scheduler_tasks.status IN ('completed', 'failed', 'parked')
                        THEN NULL
                    ELSE background_scheduler_tasks.last_error
                END
            "#,
            params![task_kind, now_ms, if manual { 1 } else { 0 }],
        )
        .map_err(|e| format!("Failed to enqueue background task: {e}"))?;
        Ok(())
    }

    /// Put a task back into the runnable queue after an explicit user action.
    /// This is used to release a Python task from the in-process degraded
    /// monitor state after the user manually starts the monitor.
    pub fn resume_background_task(&self, task_kind: &str, now_ms: i64) -> Result<(), String> {
        let guard = self.get_connection_named("resume_background_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'queued', ready_since_ms = ?2, next_attempt_at_ms = 0,
                 failure_count = 0, last_error = NULL
             WHERE task_kind = ?1 AND status = 'degraded'",
            params![task_kind, now_ms],
        )
        .map_err(|e| format!("Failed to resume background task: {e}"))?;
        Ok(())
    }

    /// Persist a non-retryable degraded state. Unlike `retry_wait`, this state
    /// is not made eligible by a timer; an explicit manual recovery action or
    /// a fresh process must release it.
    pub fn mark_background_task_degraded(
        &self,
        task_kind: &str,
        error: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("mark_background_task_degraded")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'degraded', next_attempt_at_ms = 0, last_error = ?2,
                 manual_pending = CASE WHEN manual_in_flight = 1 THEN 1 ELSE manual_pending END,
                 manual_in_flight = 0
             WHERE task_kind = ?1",
            params![task_kind, error],
        )
        .map_err(|e| format!("Failed to mark background task degraded: {e}"))?;
        Ok(())
    }

    pub fn mark_background_task_started(
        &self,
        task_kind: &str,
        served_seq: u64,
        manual: bool,
        now_ms: i64,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("mark_background_task_started")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'running', last_served_seq = ?2,
                 manual_pending = CASE WHEN ?3 = 1 THEN 0 ELSE manual_pending END,
                 manual_in_flight = CASE WHEN ?3 = 1 THEN 1 ELSE manual_in_flight END
             WHERE task_kind = ?1
               AND status IN ('queued', 'retry_wait')
               AND next_attempt_at_ms <= ?4
               AND manual_in_flight = 0
               AND (?3 = 0 OR manual_pending = 1)",
            params![
                task_kind,
                served_seq as i64,
                if manual { 1 } else { 0 },
                now_ms,
            ],
        )
        .map_err(|e| format!("Failed to mark background task started: {e}"))?;
        Ok(conn.changes() > 0)
    }

    /// Park a task until its feature is enabled again. A parked task is not
    /// eligible for timer-based retries and therefore cannot spin while a
    /// feature is disabled; the next enqueue (normally from backlog refresh)
    /// makes it runnable again.
    pub fn park_background_task(&self, task_kind: &str, reason: &str) -> Result<(), String> {
        let guard = self.get_connection_named("park_background_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'parked', next_attempt_at_ms = 0, last_error = ?2,
                 manual_pending = CASE WHEN manual_in_flight = 1 THEN 1 ELSE manual_pending END,
                 manual_in_flight = 0
             WHERE task_kind = ?1",
            params![task_kind, reason],
        )
        .map_err(|e| format!("Failed to park background task: {e}"))?;
        Ok(())
    }

    pub fn mark_background_task_succeeded(
        &self,
        task_kind: &str,
        has_more: bool,
        completed_at_ms: i64,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("mark_background_task_succeeded")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        if has_more {
            conn.execute(
                "UPDATE background_scheduler_tasks
                 SET status = 'queued',
                     ready_since_ms = ?2, next_attempt_at_ms = 0,
                     failure_count = 0, last_error = NULL,
                     last_completed_at_ms = ?2,
                     manual_in_flight = 0
                 WHERE task_kind = ?1",
                params![task_kind, completed_at_ms],
            )
        } else {
            conn.execute(
                "UPDATE background_scheduler_tasks
                 SET status = CASE WHEN manual_pending = 1 THEN 'queued' ELSE 'completed' END,
                 ready_since_ms = CASE WHEN manual_pending = 1 THEN ?2 ELSE ready_since_ms END,
                     next_attempt_at_ms = 0,
                     failure_count = 0, last_error = NULL,
                     last_completed_at_ms = ?2,
                     manual_in_flight = 0
                 WHERE task_kind = ?1",
                params![task_kind, completed_at_ms],
            )
        }
        .map_err(|e| format!("Failed to mark background task succeeded: {e}"))?;
        Ok(())
    }

    pub fn mark_background_task_failed(
        &self,
        task_kind: &str,
        failure_count: u32,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("mark_background_task_failed")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'retry_wait', failure_count = ?2,
                 next_attempt_at_ms = ?3, last_error = ?4,
                 manual_pending = CASE WHEN manual_in_flight = 1 THEN 1 ELSE manual_pending END,
                 manual_in_flight = 0
             WHERE task_kind = ?1",
            params![task_kind, failure_count as i64, next_attempt_at_ms, error],
        )
        .map_err(|e| format!("Failed to record background task failure: {e}"))?;
        Ok(())
    }

    pub fn defer_background_task(
        &self,
        task_kind: &str,
        next_attempt_at_ms: i64,
        reason: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("defer_background_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET status = 'queued', next_attempt_at_ms = ?2, last_error = ?3,
                 manual_pending = CASE WHEN manual_in_flight = 1 THEN 1 ELSE manual_pending END,
                 manual_in_flight = 0
             WHERE task_kind = ?1",
            params![task_kind, next_attempt_at_ms, reason],
        )
        .map_err(|e| format!("Failed to defer background task: {e}"))?;
        Ok(())
    }

    pub fn cancel_manual_background_task(&self, task_kind: &str) -> Result<(), String> {
        let guard = self.get_connection_named("cancel_manual_background_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.execute(
            "UPDATE background_scheduler_tasks
             SET manual_pending = 0,
                 manual_in_flight = 0,
                 status = CASE WHEN status = 'running' THEN status ELSE 'queued' END
             WHERE task_kind = ?1",
            params![task_kind],
        )
        .map_err(|e| format!("Failed to cancel manual background task: {e}"))?;
        Ok(())
    }

    pub fn background_scheduler_task(
        &self,
        task_kind: &str,
    ) -> Result<Option<BackgroundTaskState>, String> {
        let guard = self.get_connection_named("background_scheduler_task")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        conn.query_row(
            "SELECT task_kind, ready_since_ms, next_attempt_at_ms, failure_count,
                    last_served_seq, last_error, last_completed_at_ms, status,
                    manual_pending
             FROM background_scheduler_tasks WHERE task_kind = ?1",
            params![task_kind],
            row_to_state,
        )
        .optional()
        .map_err(|e| format!("Failed to read scheduler task: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temporary storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    fn raw_manual_flags(storage: &StorageState, task_kind: &str) -> (i64, i64) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT manual_pending, manual_in_flight
                 FROM background_scheduler_tasks WHERE task_kind = ?1",
                [task_kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn starting_manual_work_moves_pending_into_flight() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("smart_cluster", true, 10)
            .unwrap();

        assert!(storage
            .mark_background_task_started("smart_cluster", 7, true, 10)
            .unwrap());

        assert_eq!(raw_manual_flags(&storage, "smart_cluster"), (0, 1));
        assert_eq!(
            storage
                .background_scheduler_task("smart_cluster")
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
    }

    #[test]
    fn cancelled_manual_snapshot_cannot_be_claimed_later() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("smart_cluster", true, 10)
            .unwrap();
        storage
            .cancel_manual_background_task("smart_cluster")
            .unwrap();

        assert!(!storage
            .mark_background_task_started("smart_cluster", 8, true, 11)
            .unwrap());
        let task = storage
            .background_scheduler_task("smart_cluster")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "queued");
        assert!(!task.manual_pending);
    }

    #[test]
    fn failure_and_defer_restore_a_manual_request_to_pending() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("python_clustering", true, 10)
            .unwrap();
        storage
            .mark_background_task_started("python_clustering", 1, true, 10)
            .unwrap();

        storage
            .mark_background_task_failed("python_clustering", 1, 500, "temporary")
            .unwrap();
        let failed = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert!(failed.manual_pending);
        assert_eq!(failed.status, "retry_wait");
        assert_eq!(raw_manual_flags(&storage, "python_clustering"), (1, 0));

        storage
            .mark_background_task_started("python_clustering", 2, true, 500)
            .unwrap();
        storage
            .defer_background_task("python_clustering", 900, "waiting_for_unlock")
            .unwrap();
        let deferred = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert!(deferred.manual_pending);
        assert_eq!(deferred.status, "queued");
        assert_eq!(deferred.next_attempt_at_ms, 900);
        assert_eq!(raw_manual_flags(&storage, "python_clustering"), (1, 0));
    }

    #[test]
    fn success_keeps_a_manual_request_arriving_during_the_slice() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("semantic_index", true, 10)
            .unwrap();
        storage
            .mark_background_task_started("semantic_index", 1, true, 10)
            .unwrap();
        // A second click while the first slice is running must survive the
        // first completion and create another queued slice.
        storage
            .enqueue_background_task("semantic_index", true, 20)
            .unwrap();
        storage
            .mark_background_task_succeeded("semantic_index", false, 30)
            .unwrap();

        let task = storage
            .background_scheduler_task("semantic_index")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "queued");
        assert!(task.manual_pending);
        assert_eq!(task.last_completed_at_ms, Some(30));
        assert_eq!(raw_manual_flags(&storage, "semantic_index"), (1, 0));
    }

    #[test]
    fn cancelling_clears_pending_and_in_flight_manual_work() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("clip_index", true, 10)
            .unwrap();
        storage
            .mark_background_task_started("clip_index", 1, true, 10)
            .unwrap();
        storage
            .enqueue_background_task("clip_index", true, 20)
            .unwrap();
        storage.cancel_manual_background_task("clip_index").unwrap();

        assert_eq!(raw_manual_flags(&storage, "clip_index"), (0, 0));
        assert_eq!(
            storage
                .background_scheduler_task("clip_index")
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
    }

    #[test]
    fn recovery_requeues_a_running_manual_slice_after_restart() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("smart_cluster", true, 10)
            .unwrap();
        storage
            .mark_background_task_started("smart_cluster", 1, true, 10)
            .unwrap();
        storage.recover_background_scheduler_tasks().unwrap();

        let task = storage
            .background_scheduler_task("smart_cluster")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "queued");
        assert!(task.manual_pending);
        assert_eq!(task.next_attempt_at_ms, 0);
        assert_eq!(raw_manual_flags(&storage, "smart_cluster"), (1, 0));
    }

    #[test]
    fn manual_enqueue_releases_retry_wait_immediately() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("python_clustering", false, 10)
            .unwrap();
        storage
            .mark_background_task_failed("python_clustering", 2, 9_999, "temporary")
            .unwrap();
        storage
            .enqueue_background_task("python_clustering", true, 20)
            .unwrap();

        let task = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert_eq!(task.status, "queued");
        assert_eq!(task.next_attempt_at_ms, 0);
        assert!(task.manual_pending);
    }

    #[test]
    fn degraded_task_can_only_resume_explicitly() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("python_clustering", false, 10)
            .unwrap();
        storage
            .mark_background_task_degraded("python_clustering", "manual start required")
            .unwrap();
        let degraded = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert_eq!(degraded.status, "degraded");
        assert!(!degraded.is_eligible(100_000));

        storage
            .resume_background_task("python_clustering", 50)
            .unwrap();
        let resumed = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert_eq!(resumed.status, "queued");
        assert_eq!(resumed.ready_since_ms, 50);
    }

    #[test]
    fn parked_task_preserves_completion_and_manual_request_until_reenabled() {
        let (_temp, storage) = test_storage();
        storage
            .enqueue_background_task("python_clustering", false, 10)
            .unwrap();
        storage
            .mark_background_task_started("python_clustering", 1, false, 10)
            .unwrap();
        storage
            .mark_background_task_succeeded("python_clustering", false, 20)
            .unwrap();
        storage
            .enqueue_background_task("python_clustering", true, 30)
            .unwrap();
        storage
            .mark_background_task_started("python_clustering", 2, true, 30)
            .unwrap();

        storage
            .park_background_task("python_clustering", "feature_disabled")
            .unwrap();
        let parked = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert_eq!(parked.status, "parked");
        assert_eq!(parked.last_completed_at_ms, Some(20));
        assert_eq!(parked.last_error.as_deref(), Some("feature_disabled"));
        assert!(parked.manual_pending);
        assert_eq!(raw_manual_flags(&storage, "python_clustering"), (1, 0));

        storage
            .enqueue_background_task("python_clustering", false, 40)
            .unwrap();
        let resumed = storage
            .background_scheduler_task("python_clustering")
            .unwrap()
            .unwrap();
        assert_eq!(resumed.status, "queued");
        assert_eq!(resumed.ready_since_ms, 40);
        assert_eq!(resumed.next_attempt_at_ms, 0);
        assert_eq!(resumed.last_completed_at_ms, Some(20));
        assert_eq!(resumed.last_error, None);
        assert!(resumed.manual_pending);
        assert!(resumed.is_eligible(40));
        assert_eq!(raw_manual_flags(&storage, "python_clustering"), (1, 0));
    }
}
