//! Shared quantum and queue execution for image and text indexes. Pipelines
//! own input preparation and commit validation; this module owns yielding,
//! retries and settlement of the leases that a pass has claimed.

mod automatic;
pub(crate) use automatic::{run_automatic_quantum, AutomaticIndex};

use crate::background_scheduler::{deferred_release_note, AutomaticSliceContext};
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{DerivedIndexJobSpec, DerivedIndexKind, StorageState};
use crate::task_error::{TaskError, TaskResult};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub(crate) const MAX_ATTEMPTS: u32 = 5;
const RETRY_BACKOFF_MINUTES: i64 = 30;
/// Manual drains may resume after this condition clears; idle slices finish.
pub(crate) const FOREGROUND_QUERY_STOP: &str = "foreground_query";
pub(crate) const STOPPED_BY_USER: &str = "stopped_by_user";
pub(crate) const MAINTENANCE_STARTED: &str = "maintenance_started";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PassMode {
    Idle,
    Manual,
}

impl PassMode {
    pub(crate) fn is_manual(self) -> bool {
        self == Self::Manual
    }
    pub(crate) fn yields_after_chunk(self) -> bool {
        self == Self::Idle
    }
}

pub(crate) fn scheduled_pass_mode(manual: bool) -> PassMode {
    if manual {
        PassMode::Manual
    } else {
        PassMode::Idle
    }
}

pub(crate) fn may_run(app: &AppHandle, mode: PassMode) -> bool {
    !crate::maintenance::is_active()
        && (mode.is_manual()
            || crate::background_scheduler::environment_gate_reason(
                app,
                crate::background_scheduler::EnvironmentPolicy::Automatic,
            )
            .is_none())
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexRunSummary {
    /// Accepted by the durable scheduler.
    pub queued: bool,
    pub started: bool,
    pub indexed: u64,
    pub failed: u64,
    /// None when the backlog could not be read.
    pub remaining: Option<u64>,
    pub stalled: Option<u64>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct PassOutcome {
    pub indexed: u64,
    pub failed: u64,
    pub stopped_because: Option<&'static str>,
    pub refused: Option<&'static str>,
}

impl PassOutcome {
    pub(crate) fn refused(reason: &'static str) -> Self {
        Self {
            refused: Some(reason),
            ..Self::default()
        }
    }
}

pub(crate) trait JobLease: Send + 'static {
    fn spec(&self) -> &DerivedIndexJobSpec;
    fn lease_token(&self) -> &str;
}

pub(crate) struct EncodeChunkFailure<J> {
    pub error: TaskError,
    pub claimed: Vec<J>,
    pub deferred_reason: Option<&'static str>,
}

pub(crate) trait IndexPipeline {
    type Job: JobLease;
    const KIND: DerivedIndexKind;
    const CHUNK_SIZE: usize;

    fn claim(
        storage: Arc<StorageState>,
        locked: bool,
    ) -> impl Future<Output = TaskResult<Vec<Self::Job>>> + Send;
    fn encode<'a>(
        app: &'a AppHandle,
        semantic: &'a Arc<SemanticRuntimeState>,
        storage: Arc<StorageState>,
        claimed: Vec<Self::Job>,
        context: Option<(&'a AutomaticSliceContext, bool)>,
    ) -> impl Future<Output = Result<u64, EncodeChunkFailure<Self::Job>>> + Send + 'a;
    fn stopped_by_user(app: &AppHandle) -> bool;
    fn report_chunk(app: &AppHandle, processed: u64, indexed: u64);
}

pub(crate) async fn drain_queue<P: IndexPipeline>(
    app: &AppHandle,
    storage: Arc<StorageState>,
    mode: PassMode,
    locked: bool,
    fast_retry_used: &mut bool,
    automatic_context: Option<(&AutomaticSliceContext, bool)>,
) -> TaskResult<PassOutcome> {
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    if let Some((quantum, made_progress)) = automatic_context {
        if let Some(reason) = quantum.stop_reason(&semantic, made_progress) {
            return Ok(PassOutcome {
                stopped_because: Some(reason.as_str()),
                ..PassOutcome::default()
            });
        }
    }
    let mut pending: VecDeque<P::Job> = P::claim(storage.clone(), locked).await?.into();
    let mut outcome = PassOutcome::default();
    let mut failure = None;
    while !pending.is_empty() {
        let stopped = if mode.is_manual() && P::stopped_by_user(app) {
            Some(STOPPED_BY_USER)
        } else if !may_run(app, mode) {
            Some(if mode.is_manual() {
                MAINTENANCE_STARTED
            } else {
                "idle_lost"
            })
        } else if semantic.foreground_waiting() {
            Some(FOREGROUND_QUERY_STOP)
        } else {
            None
        };
        if let Some(reason) = stopped {
            release_claims(
                storage.clone(),
                pending.drain(..).collect(),
                reason,
                "the pass stopped before this subject was encoded",
            )
            .await;
            outcome.stopped_because = Some(reason);
            break;
        }

        let chunk: Vec<_> = pending.drain(..P::CHUNK_SIZE.min(pending.len())).collect();
        let chunk_len = chunk.len() as u64;
        let encoded = P::encode(app, &semantic, storage.clone(), chunk, automatic_context).await;
        let encoded = match encoded {
            Err(error) if error.deferred_reason.is_some() => {
                let reason = error
                    .deferred_reason
                    .expect("deferred failure has a reason");
                let mut unattempted = error.claimed;
                unattempted.extend(pending.drain(..));
                release_claims(
                    storage.clone(),
                    unattempted,
                    reason,
                    deferred_release_note(reason),
                )
                .await;
                outcome.stopped_because = Some(reason);
                break;
            }
            Err(error)
                if mode.is_manual()
                    && !*fast_retry_used
                    && error.error.is_transient_worker_failure() =>
            {
                *fast_retry_used = true;
                tracing::warn!("[INDEX] {} manual encode failed; restarting worker for one immediate retry: {}", P::KIND.as_str(), error.error);
                semantic.restart_for_manual_retry();
                P::encode(app, &semantic, storage.clone(), error.claimed, None).await
            }
            other => other,
        };
        match encoded {
            Ok(indexed) => {
                outcome.indexed += indexed;
                if mode.is_manual() {
                    P::report_chunk(app, chunk_len, indexed);
                }
                if mode.yields_after_chunk() && !pending.is_empty() {
                    release_claims(
                        storage.clone(),
                        pending.drain(..).collect(),
                        "slice_yielded",
                        "the scheduler completed one index batch",
                    )
                    .await;
                    break;
                }
            }
            Err(error) => {
                // Unattempted subjects never pay for a preceding worker failure.
                outcome.failed += chunk_len;
                if !pending.is_empty() {
                    release_claims(
                        storage.clone(),
                        pending.drain(..).collect(),
                        "batch_aborted",
                        "an earlier chunk of the same pass failed to encode",
                    )
                    .await;
                }
                settle_failed_claims(storage.clone(), error.claimed, &error.error).await;
                failure = Some(error.error);
                break;
            }
        }
    }
    if outcome.indexed > 0 {
        tracing::info!(
            "[INDEX] {} indexed {} subject(s)",
            P::KIND.as_str(),
            outcome.indexed
        );
    }
    // The command already returned after enqueueing; only the scheduler can
    // enforce durable backoff, so a failure must reach it as an error.
    match failure {
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

async fn release_claims<J: JobLease>(
    storage: Arc<StorageState>,
    claimed: Vec<J>,
    reason_code: &'static str,
    reason: &'static str,
) {
    let _ = tokio::task::spawn_blocking(move || {
        for job in claimed {
            let _ = storage.requeue_derived_index_job(
                job.spec(),
                job.lease_token(),
                reason_code,
                reason,
            );
        }
    })
    .await;
}

async fn fail_claims<J: JobLease>(storage: Arc<StorageState>, claimed: Vec<J>, error: String) {
    let retry = (Utc::now() + ChronoDuration::minutes(RETRY_BACKOFF_MINUTES))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let _ = tokio::task::spawn_blocking(move || {
        for job in claimed {
            let _ = storage.mark_derived_index_job_failed(
                job.spec(),
                job.lease_token(),
                "embed_failed",
                &error,
                Some(&retry),
            );
        }
    })
    .await;
}

async fn settle_failed_claims<J: JobLease>(
    storage: Arc<StorageState>,
    claimed: Vec<J>,
    error: &TaskError,
) {
    if error.is_deterministic_worker_failure() {
        fail_claims(storage, claimed, error.to_string()).await;
    } else {
        release_claims(
            storage,
            claimed,
            "worker_retry",
            "worker failure; scheduler will retry the batch",
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_scheduled_manual_request_uses_the_complete_drain_mode() {
        // The scheduler owns admission, not the meaning of the user's action.
        // Once admitted, a manual request must keep the worker until the queue
        // is empty (or a real manual stop condition fires).
        let mode = scheduled_pass_mode(true);
        assert_eq!(mode, PassMode::Manual);
        assert!(mode.is_manual());
        assert!(!mode.yields_after_chunk());
        assert_eq!(scheduled_pass_mode(false), PassMode::Idle);
    }

    #[test]
    fn a_refused_pass_reports_no_work_rather_than_silence() {
        // The summary has to distinguish "ran and found nothing" from "never
        // ran": both have `indexed = 0`, and only the second has a reason.
        let refused = PassOutcome::refused("session_locked");
        assert_eq!(refused.indexed, 0);
        assert_eq!(refused.refused, Some("session_locked"));

        let empty = PassOutcome::default();
        assert_eq!(empty.indexed, 0);
        assert!(empty.refused.is_none());
        assert!(empty.stopped_because.is_none());
    }
}
