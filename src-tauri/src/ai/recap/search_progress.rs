//! One ordered snapshot for recap index events and status polling.

use crate::index_progress::IndexRunProgress;
use crate::storage::BackgroundTaskState;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecapIndexProgress {
    #[serde(flatten)]
    pub run: IndexRunProgress,
    /// Distinguishes an uncounted corpus from a counted, empty one.
    pub total_known: bool,
    pub phase: &'static str,
    pub wait_reason: Option<&'static str>,
    pub retry_at_ms: Option<i64>,
    /// Last vector checkpoint committed during this application session.
    pub last_progress_at_ms: Option<i64>,
}

impl Default for RecapIndexProgress {
    fn default() -> Self {
        Self {
            run: IndexRunProgress::default(),
            total_known: false,
            phase: "idle",
            wait_reason: None,
            retry_at_ms: None,
            last_progress_at_ms: None,
        }
    }
}

impl RecapIndexProgress {
    pub(super) fn complete(&self) -> bool {
        self.total_known && self.run.processed >= self.run.total
    }

    pub(super) fn set_phase(&mut self, phase: &'static str) {
        self.phase = phase;
        self.wait_reason = None;
        self.retry_at_ms = None;
    }

    pub(super) fn wait(&mut self, reason: Option<&'static str>) {
        self.set_phase(match reason {
            Some("waiting_for_unlock") => "waiting_for_unlock",
            Some("waiting_for_verification") => "waiting_for_verification",
            Some("retry_wait") => "retry_wait",
            Some("failed") => "failed",
            _ => "queued",
        });
        self.wait_reason = reason;
    }

    pub(super) fn sync_task(
        &mut self,
        task: Option<&BackgroundTaskState>,
        wait_reason: Option<&'static str>,
        now_ms: i64,
    ) {
        if self.complete() {
            self.set_phase("idle");
            return;
        }
        match task.map(|task| task.status.as_str()) {
            Some("failed") => self.wait(Some("failed")),
            Some("retry_wait") if task.is_some_and(|task| task.next_attempt_at_ms > now_ms) => {
                self.wait(Some("retry_wait"));
                self.retry_at_ms = task.map(|task| task.next_attempt_at_ms);
            }
            Some("queued" | "running" | "retry_wait") => self.wait(wait_reason),
            _ => self.set_phase("idle"),
        }
    }
}
