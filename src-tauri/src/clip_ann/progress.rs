//! Content-free ANN construction progress, restored from durable checkpoints.

use super::ClipAnnState;
use crate::storage::{AnnBuildCheckpoint, BackgroundTaskState, DerivedIndexKind, StorageState};
use crate::task_error::TaskError;
use serde::Serialize;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AnnBuildProgress {
    pub recovery_needed: bool,
    pub generation: Option<u64>,
    pub phase: String,
    pub stage: String,
    pub running: bool,
    pub processed: u64,
    pub total: u64,
    pub frozen_rows: u64,
    pub materialized_rows: u64,
    pub built_rows: u64,
    pub percent: Option<f64>,
    pub wait_reason: Option<String>,
    pub revision: u64,
}

impl Default for AnnBuildProgress {
    fn default() -> Self {
        Self {
            recovery_needed: false,
            generation: None,
            phase: "idle".into(),
            stage: "idle".into(),
            running: false,
            processed: 0,
            total: 0,
            frozen_rows: 0,
            materialized_rows: 0,
            built_rows: 0,
            percent: None,
            wait_reason: None,
            revision: 0,
        }
    }
}

impl AnnBuildProgress {
    fn resolve(mut self, task: Option<&BackgroundTaskState>, read_wait: Option<&str>) -> Self {
        if !self.recovery_needed || self.running {
            return self;
        }
        self.phase = match task.map(|task| task.status.as_str()) {
            Some("failed") => "failed",
            Some("retry_wait") if task.is_some_and(|task| task.failure_count > 0) => "retry_wait",
            _ => read_wait.unwrap_or("queued"),
        }
        .into();
        // Scheduling errors may include implementation details. Only these
        // ordinary waiting states are part of the public progress contract.
        self.wait_reason = task.and_then(|task| {
            task.last_error
                .as_deref()
                .filter(|reason| {
                    matches!(
                        *reason,
                        "waiting_for_ac_power" | "waiting_for_idle" | "input_resumed"
                    )
                })
                .map(str::to_owned)
        });
        self
    }
}

#[derive(Default)]
pub(super) struct AnnProgressState {
    snapshot: AnnBuildProgress,
    emitted_at: Option<Instant>,
    logged_at: Option<Instant>,
}

struct ProgressUpdate {
    snapshot: AnnBuildProgress,
    emit: bool,
    log: bool,
}

impl AnnProgressState {
    pub(super) fn clear(&mut self) {
        self.snapshot = AnnBuildProgress {
            revision: self.snapshot.revision.wrapping_add(1),
            ..AnnBuildProgress::default()
        };
        self.emitted_at = None;
        self.logged_at = None;
    }

    fn update(&mut self, mut next: AnnBuildProgress) -> ProgressUpdate {
        next.revision = self.snapshot.revision;
        let boundary = next.generation != self.snapshot.generation
            || next.stage != self.snapshot.stage
            || next.recovery_needed != self.snapshot.recovery_needed;
        if next != self.snapshot {
            next.revision = next.revision.wrapping_add(1);
        }
        let now = Instant::now();
        let emit = boundary
            || self
                .emitted_at
                .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1));
        let log = boundary
            || self
                .logged_at
                .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1));
        if emit {
            self.emitted_at = Some(now);
        }
        if log {
            self.logged_at = Some(now);
        }
        self.snapshot = next.clone();
        ProgressUpdate {
            snapshot: next,
            emit,
            log,
        }
    }

    fn checkpoint(
        &mut self,
        checkpoint: &AnnBuildCheckpoint,
        recovery_needed: bool,
        running: bool,
        live_built: Option<u64>,
    ) -> ProgressUpdate {
        let stage = match checkpoint.phase.as_str() {
            stage @ ("freeze" | "materialize" | "hash_flat" | "build" | "copy_ann" | "publish"
            | "cleanup" | "complete") => stage,
            _ => "preparing",
        };
        let durable_built = checkpoint.complete.as_ref().map_or(0, |file| file.rows);
        let built = live_built
            .unwrap_or_else(|| {
                if stage == "build"
                    && self.snapshot.stage == "build"
                    && self.snapshot.generation == Some(checkpoint.generation)
                {
                    self.snapshot.built_rows.max(durable_built)
                } else {
                    durable_built
                }
            })
            .min(checkpoint.frozen_rows);
        let (processed, total) = match stage {
            "freeze" => (
                checkpoint.frozen_rows,
                checkpoint.expected_rows.max(checkpoint.frozen_rows),
            ),
            "materialize" | "hash_flat" => (checkpoint.materialized_rows, checkpoint.frozen_rows),
            _ => (built, checkpoint.frozen_rows),
        };
        let done = matches!(stage, "cleanup" | "complete");
        self.update(AnnBuildProgress {
            recovery_needed: recovery_needed && !done,
            generation: Some(checkpoint.generation),
            phase: if done {
                "idle"
            } else if running {
                "running"
            } else {
                "queued"
            }
            .into(),
            stage: stage.into(),
            running: running && !done,
            processed,
            total,
            frozen_rows: checkpoint.frozen_rows,
            materialized_rows: checkpoint.materialized_rows,
            built_rows: built,
            percent: construction_percent(checkpoint, built),
            wait_reason: None,
            revision: 0,
        })
    }
}

fn fraction(completed: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        completed.min(total) as f64 / total as f64
    }
}

fn construction_percent(checkpoint: &AnnBuildCheckpoint, built: u64) -> Option<f64> {
    // Counts drive each stage; elapsed time never advances the bar. Reserve
    // the final range for copying and publishing so a built graph cannot
    // appear complete before it becomes a queryable generation.
    let percent = match checkpoint.phase.as_str() {
        "freeze" if checkpoint.expected_rows.max(checkpoint.frozen_rows) > 0 => {
            25.0 * fraction(
                checkpoint.frozen_rows,
                checkpoint.expected_rows.max(checkpoint.frozen_rows),
            )
        }
        "materialize" => {
            25.0 + 25.0 * fraction(checkpoint.materialized_rows, checkpoint.frozen_rows)
        }
        "hash_flat" => 50.0,
        "build" => 50.0 + 45.0 * fraction(built, checkpoint.frozen_rows),
        "copy_ann" => 95.0 + 4.0 * fraction(checkpoint.copy_offset, checkpoint.graph_bytes),
        "publish" => 99.0,
        "cleanup" | "complete" => 100.0,
        _ => return None,
    };
    Some(percent)
}

fn is_recovery_checkpoint(
    checkpoint: &AnnBuildCheckpoint,
    dataset: &str,
    published_generation: Option<u64>,
) -> bool {
    checkpoint.dataset_id == dataset
        && checkpoint.model_fingerprint == super::resumable::model_fingerprint()
        && !matches!(checkpoint.phase.as_str(), "cleanup" | "complete")
        && published_generation != Some(checkpoint.generation)
}

fn publish(app: &AppHandle, update: ProgressUpdate) {
    let progress = update.snapshot;
    if update.log {
        tracing::info!(
            generation = ?progress.generation,
            stage = %progress.stage,
            completed = progress.processed,
            total = progress.total,
            frozen_rows = progress.frozen_rows,
            materialized_rows = progress.materialized_rows,
            built_rows = progress.built_rows,
            percent = ?progress.percent,
            recovery = progress.recovery_needed,
            "[CLIP:ANN] build progress"
        );
    }
    if update.emit {
        let _ = app.emit("clip-ann-build-progress", progress);
    }
}

impl ClipAnnState {
    pub(crate) fn build_progress(
        &self,
        task: Option<&BackgroundTaskState>,
        read_wait: Option<&str>,
    ) -> AnnBuildProgress {
        let mut progress = self
            .progress_state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot
            .clone();
        if !super::enabled() {
            progress.recovery_needed = false;
            progress.running = false;
            progress.phase = "idle".into();
        }
        progress.resolve(task, read_wait)
    }

    pub(super) fn update_build_progress(
        &self,
        app: &AppHandle,
        token: u64,
        checkpoint: &AnnBuildCheckpoint,
        running: bool,
        live_built: Option<u64>,
    ) {
        let arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token {
            return;
        }
        let update = self
            .progress_state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .checkpoint(checkpoint, arm.rebuild_required, running, live_built);
        drop(arm);
        publish(app, update);
    }

    pub(super) fn restore_recovery_progress(
        &self,
        app: &AppHandle,
        storage: &StorageState,
        token: u64,
    ) -> Result<(), String> {
        let dataset = storage.processing_dataset_id()?;
        let published_generation = storage
            .get_derived_ann_generation(DerivedIndexKind::ClipImage)?
            .map(|manifest| manifest.generation);
        let checkpoint = match storage.ann_build_checkpoint() {
            Err(TaskError::InvalidCheckpoint(_)) => None,
            result => result.map_err(|error| error.to_string())?,
        }
        .filter(|checkpoint| is_recovery_checkpoint(checkpoint, &dataset, published_generation));
        let has_vectors = checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.expected_rows > 0)
            || storage.has_query_visible_embeddings(DerivedIndexKind::ClipImage)?;
        let arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token || !arm.rebuild_required {
            return Ok(());
        }
        let mut state = self
            .progress_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let update = if let Some(checkpoint) = checkpoint {
            state.checkpoint(&checkpoint, true, false, None)
        } else {
            state.update(AnnBuildProgress {
                recovery_needed: has_vectors,
                phase: if has_vectors { "queued" } else { "idle" }.into(),
                stage: if has_vectors { "preparing" } else { "idle" }.into(),
                ..AnnBuildProgress::default()
            })
        };
        drop(state);
        drop(arm);
        publish(app, update);
        Ok(())
    }

    pub(super) fn clear_build_progress(&self, app: &AppHandle, token: u64) {
        let arm = self.arm.lock().unwrap_or_else(|error| error.into_inner());
        if arm.token != token {
            return;
        }
        let update = self
            .progress_state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .update(AnnBuildProgress::default());
        drop(arm);
        publish(app, update);
    }

    pub(super) fn emit_build_progress(&self, app: &AppHandle) {
        let progress = self
            .progress_state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot
            .clone();
        let _ = app.emit("clip-ann-build-progress", progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AnnCheckpointFile;

    fn checkpoint() -> AnnBuildCheckpoint {
        AnnBuildCheckpoint {
            runtime_generation: Some(0),
            generation: 7,
            dataset_id: "fixture".into(),
            encryption_key_id: "fixture-key".into(),
            snapshot_id: "fixture-snapshot".into(),
            model_fingerprint: super::super::resumable::model_fingerprint(),
            covered_epoch: 0,
            scan_upper: "z".into(),
            scan_cursor: String::new(),
            expected_rows: 100,
            frozen_rows: 0,
            key_bytes: 0,
            phase: "freeze".into(),
            materialized_rows: 0,
            materialized_key_bytes: 0,
            flat_checksum: None,
            graph_bytes: 0,
            complete: None,
            previous: None,
            copy_offset: 0,
        }
    }

    #[test]
    fn recovery_progress_tracks_real_rows_and_finishes_only_after_publication() {
        let mut checkpoint = checkpoint();
        let mut state = AnnProgressState::default();
        checkpoint.frozen_rows = 25;
        let frozen = state.checkpoint(&checkpoint, true, true, None).snapshot;
        assert_eq!((frozen.processed, frozen.total), (25, 100));
        assert_eq!(frozen.percent, Some(6.25));
        checkpoint.frozen_rows = 100;
        checkpoint.phase = "materialize".into();
        checkpoint.materialized_rows = 25;
        assert_eq!(
            state
                .checkpoint(&checkpoint, true, true, None)
                .snapshot
                .percent,
            Some(31.25)
        );
        checkpoint.phase = "build".into();
        checkpoint.materialized_rows = 100;
        let graph = state.checkpoint(&checkpoint, true, true, Some(40)).snapshot;
        assert_eq!(graph.built_rows, 40);
        assert_eq!(graph.percent, Some(68.0));
        let paused = state.checkpoint(&checkpoint, true, false, None).snapshot;
        assert_eq!(paused.built_rows, 40);
        assert_eq!(paused.percent, graph.percent);
        assert!(paused.recovery_needed);
        checkpoint.complete = Some(AnnCheckpointFile {
            name: "graph".into(),
            checksum: "digest".into(),
            rows: 100,
        });
        checkpoint.phase = "copy_ann".into();
        checkpoint.graph_bytes = 1000;
        checkpoint.copy_offset = 500;
        assert_eq!(
            state
                .checkpoint(&checkpoint, true, true, None)
                .snapshot
                .percent,
            Some(97.0)
        );
        checkpoint.phase = "publish".into();
        let publishing = state.checkpoint(&checkpoint, true, true, None).snapshot;
        assert_eq!(publishing.percent, Some(99.0));
        assert!(publishing.recovery_needed);
        checkpoint.phase = "cleanup".into();
        let published = state.checkpoint(&checkpoint, false, false, None).snapshot;
        assert_eq!(published.percent, Some(100.0));
        assert!(!published.recovery_needed);
        assert!(!published.running);
    }

    #[test]
    fn ordinary_generation_refresh_does_not_request_a_search_bar_task() {
        let mut checkpoint = checkpoint();
        checkpoint.frozen_rows = 40;
        let progress = AnnProgressState::default()
            .checkpoint(&checkpoint, false, true, None)
            .snapshot;
        assert!(progress.running);
        assert!(!progress.recovery_needed);
    }

    #[test]
    fn recovery_does_not_resume_a_checkpoint_for_the_damaged_published_generation() {
        let mut checkpoint = checkpoint();
        checkpoint.phase = "publish".into();
        assert!(!is_recovery_checkpoint(&checkpoint, "fixture", Some(7)));
        assert!(is_recovery_checkpoint(&checkpoint, "fixture", Some(6)));
        assert!(!is_recovery_checkpoint(
            &checkpoint,
            "another-dataset",
            None
        ));
        checkpoint.phase = "cleanup".into();
        assert!(!is_recovery_checkpoint(&checkpoint, "fixture", None));
    }

    #[test]
    fn recovering_a_lost_graph_copy_reports_only_the_surviving_built_prefix() {
        let mut checkpoint = checkpoint();
        checkpoint.frozen_rows = 100;
        checkpoint.materialized_rows = 100;
        checkpoint.phase = "copy_ann".into();
        checkpoint.complete = Some(AnnCheckpointFile {
            name: "graph".into(),
            checksum: "digest".into(),
            rows: 100,
        });
        let mut state = AnnProgressState::default();
        state.checkpoint(&checkpoint, true, true, None);
        checkpoint.phase = "build".into();
        checkpoint.complete.as_mut().unwrap().rows = 40;
        let recovered = state.checkpoint(&checkpoint, true, false, None).snapshot;
        assert_eq!(recovered.built_rows, 40);
        assert_eq!(recovered.percent, Some(68.0));
    }

    #[test]
    fn restarting_progress_uses_the_durable_graph_prefix_and_a_new_revision() {
        let mut checkpoint = checkpoint();
        checkpoint.phase = "build".into();
        checkpoint.frozen_rows = 100;
        checkpoint.materialized_rows = 100;
        checkpoint.complete = Some(AnnCheckpointFile {
            name: "graph".into(),
            checksum: "digest".into(),
            rows: 40,
        });
        let mut state = AnnProgressState::default();
        let previous = state.checkpoint(&checkpoint, true, true, Some(80)).snapshot;
        state.clear();
        let resumed = state.checkpoint(&checkpoint, true, false, None).snapshot;
        assert_eq!(resumed.built_rows, 40);
        assert!(resumed.revision > previous.revision);
        assert_eq!(resumed.percent, Some(68.0));
    }

    #[test]
    fn scheduling_waits_preserve_counted_construction_progress() {
        let mut checkpoint = checkpoint();
        checkpoint.frozen_rows = 25;
        let progress = AnnProgressState::default()
            .checkpoint(&checkpoint, true, false, None)
            .snapshot;
        let mut task = BackgroundTaskState {
            task_kind: "ann_build".into(),
            ready_since_ms: 0,
            next_attempt_at_ms: 0,
            failure_count: 0,
            last_served_seq: 0,
            last_error: Some("waiting_for_ac_power".into()),
            last_completed_at_ms: None,
            status: "retry_wait".into(),
            manual_pending: false,
            manual_in_flight: false,
        };
        let waiting = progress.clone().resolve(Some(&task), None);
        assert_eq!(waiting.phase, "queued");
        assert_eq!(waiting.wait_reason.as_deref(), Some("waiting_for_ac_power"));
        assert_eq!(waiting.percent, progress.percent);
        task.failure_count = 1;
        assert_eq!(
            progress.clone().resolve(Some(&task), None).phase,
            "retry_wait"
        );
        task.status = "queued".into();
        assert_eq!(
            progress
                .resolve(Some(&task), Some("waiting_for_unlock"))
                .phase,
            "waiting_for_unlock"
        );
    }
}
