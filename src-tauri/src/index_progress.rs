//! Manual index progress belongs to the user's request, across scheduler retries.

use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct IndexRunProgress {
    pub running: bool,
    pub stopping: bool,
    pub processed: u64,
    pub indexed: u64,
    /// Zero until the initial queue has been counted.
    pub total: u64,
    pub run_id: u64,
    /// Orders chunk events against concurrent status replies.
    pub revision: u64,
}

#[derive(Default)]
struct RunState {
    requested_id: u64,
    cancelled_id: Option<u64>,
    progress: IndexRunProgress,
}

impl RunState {
    fn reset_progress(&mut self) {
        self.progress = IndexRunProgress {
            run_id: self.requested_id,
            revision: self.progress.revision + 1,
            ..IndexRunProgress::default()
        };
    }
}

/// The const parameter keeps text and image state distinct in Tauri's state map.
#[derive(Default)]
pub struct IndexRunState<const IMAGE: bool> {
    state: Mutex<RunState>,
}

impl<const IMAGE: bool> IndexRunState<IMAGE> {
    pub fn request_run(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.requested_id += 1;
        if !state.progress.running {
            state.reset_progress();
        }
    }

    pub fn request_stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.cancelled_id = Some(state.requested_id);
        state.progress.stopping = state.progress.running;
        state.progress.revision += 1;
    }

    pub fn is_running(&self) -> bool {
        self.progress().running
    }

    pub(crate) fn stopped_by_user(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .cancelled_id
            .is_some_and(|id| id >= state.progress.run_id)
    }

    pub fn progress(&self) -> IndexRunProgress {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .progress
    }

    pub(crate) fn begin(self: &Arc<Self>) -> ActiveRun<IMAGE> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.progress.run_id != state.requested_id {
            state.reset_progress();
        }
        state.progress.running = true;
        state.progress.stopping = state
            .cancelled_id
            .is_some_and(|id| id >= state.progress.run_id);
        state.progress.revision += 1;
        ActiveRun(self.clone())
    }

    pub(crate) fn set_remaining(&self, remaining: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.progress.total = state
            .progress
            .total
            .max(state.progress.processed + remaining);
        state.progress.revision += 1;
    }

    fn record_chunk(&self, processed: u64, indexed: u64) -> IndexRunProgress {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.progress.processed += processed;
        state.progress.indexed += indexed;
        state.progress.revision += 1;
        state.progress
    }

    pub(crate) fn report_chunk(&self, app: &AppHandle, processed: u64, indexed: u64) {
        let progress = self.record_chunk(processed, indexed);
        let event = if IMAGE {
            crate::clip_index::CLIP_INDEX_PROGRESS_EVENT
        } else {
            crate::minilm_index::SEMANTIC_INDEX_PROGRESS_EVENT
        };
        let _ = app.emit(event, progress);
    }
}

pub(crate) struct ActiveRun<const IMAGE: bool>(Arc<IndexRunState<IMAGE>>);

impl<const IMAGE: bool> Drop for ActiveRun<IMAGE> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.progress.running = false;
        state.progress.stopping = false;
        state.progress.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_retries_keep_the_request_and_completed_progress() {
        let state = Arc::new(IndexRunState::<true>::default());
        state.request_run();
        let first = state.begin();
        state.set_remaining(10);
        state.record_chunk(4, 3);
        let before = state.progress();
        drop(first);
        let _retry = state.begin();
        state.set_remaining(6);
        let after = state.progress();
        assert_eq!(after.run_id, before.run_id);
        assert_eq!((after.processed, after.indexed, after.total), (4, 3, 10));
        assert!(after.revision > before.revision);
    }

    #[test]
    fn cancellation_between_claim_and_begin_is_not_lost() {
        let state = Arc::new(IndexRunState::<false>::default());
        state.request_run();
        state.request_stop();
        let active = state.begin();
        assert!(state.stopped_by_user());
        assert!(state.progress().stopping);
        drop(active);
        state.request_run();
        let _next = state.begin();
        assert!(!state.stopped_by_user());
    }

    #[test]
    fn a_new_request_does_not_clear_cancellation_of_the_current_pass() {
        let state = Arc::new(IndexRunState::<true>::default());
        state.request_run();
        let first = state.begin();
        state.set_remaining(10);
        state.record_chunk(4, 4);
        state.request_stop();
        state.request_run();
        assert!(state.stopped_by_user());
        assert_eq!(state.progress().processed, 4);
        drop(first);
        let _next = state.begin();
        assert!(!state.stopped_by_user());
        assert_eq!(state.progress().processed, 0);
    }
}
