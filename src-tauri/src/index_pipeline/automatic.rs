//! Automatic index quanta share admission, progress and stop transitions.
//! Domain implementations supply one request and optional completion work.

use crate::background_scheduler::{
    task_feature_enabled, AutomaticSliceContext, AutomaticSliceStopReason, BackgroundTaskKind,
    ScheduledSliceResult,
};
use crate::semantic_runtime::SemanticRuntimeState;
use crate::task_error::TaskResult;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub(crate) trait AutomaticIndex {
    const TASK: BackgroundTaskKind;

    fn run_request<'a>(
        app: &'a AppHandle,
        run_maintenance: bool,
        context: (&'a AutomaticSliceContext, bool),
    ) -> impl Future<Output = TaskResult<ScheduledSliceResult>> + Send + 'a;

    fn finish(app: &AppHandle) -> impl Future<Output = ()> + Send {
        let _ = app;
        async {}
    }
}

pub(crate) async fn run_automatic_quantum<P: AutomaticIndex>(
    app: &AppHandle,
    quantum: &AutomaticSliceContext,
) -> TaskResult<ScheduledSliceResult> {
    let mut runner = IndexQuantum::<P> {
        app,
        quantum,
        semantic: app.state::<Arc<SemanticRuntimeState>>().inner().clone(),
        pipeline: std::marker::PhantomData,
    };
    run_quantum(&mut runner).await
}

struct IndexQuantum<'a, P> {
    app: &'a AppHandle,
    quantum: &'a AutomaticSliceContext,
    semantic: Arc<SemanticRuntimeState>,
    pipeline: std::marker::PhantomData<P>,
}

trait QuantumRunner {
    fn kind(&self) -> BackgroundTaskKind;
    fn stop_reason(&self, made_progress: bool) -> Option<AutomaticSliceStopReason>;
    fn feature_enabled(&self) -> bool;
    fn is_background(&self) -> bool;
    fn elapsed(&self) -> Duration;
    fn run_request(
        &mut self,
        run_maintenance: bool,
        made_progress: bool,
    ) -> impl Future<Output = TaskResult<ScheduledSliceResult>> + Send;
    fn finish(&mut self) -> impl Future<Output = ()> + Send;
}

impl<P: AutomaticIndex> QuantumRunner for IndexQuantum<'_, P> {
    fn kind(&self) -> BackgroundTaskKind {
        P::TASK
    }
    fn stop_reason(&self, made_progress: bool) -> Option<AutomaticSliceStopReason> {
        self.quantum.stop_reason(&self.semantic, made_progress)
    }
    fn feature_enabled(&self) -> bool {
        task_feature_enabled(P::TASK, false)
    }
    fn is_background(&self) -> bool {
        crate::background_policy::is_background()
    }
    fn elapsed(&self) -> Duration {
        self.quantum.elapsed()
    }
    fn run_request(
        &mut self,
        run_maintenance: bool,
        made_progress: bool,
    ) -> impl Future<Output = TaskResult<ScheduledSliceResult>> + Send {
        P::run_request(self.app, run_maintenance, (self.quantum, made_progress))
    }
    fn finish(&mut self) -> impl Future<Output = ()> + Send {
        P::finish(self.app)
    }
}

async fn run_quantum(runner: &mut impl QuantumRunner) -> TaskResult<ScheduledSliceResult> {
    let kind = runner.kind();
    let mut batches = 0u32;
    let mut has_more = true;
    let mut processed = 0u64;
    let mut run_maintenance = !runner.is_background();
    loop {
        if let Some(reason) = runner.stop_reason(batches > 0) {
            if batches == 0 {
                return Ok(initial_skip(reason.as_str()));
            }
            tracing::info!(
                "[SCHEDULER] automatic model quantum yielded task={} batches={} elapsed_ms={} reason={}",
                kind.as_str(), batches, runner.elapsed().as_millis(), reason.as_str(),
            );
            runner.finish().await;
            return Ok(ScheduledSliceResult::complete(has_more).with_processed(processed));
        }

        // Check between requests so a changed feature setting parks the task
        // even after earlier progress. CLIP remains enabled by scheduler policy.
        if !runner.feature_enabled() {
            return Ok(ScheduledSliceResult::skipped("disabled").with_processed(processed));
        }
        let result = runner.run_request(run_maintenance, batches > 0).await?;
        run_maintenance = false;
        processed = processed.saturating_add(result.processed);
        if let Some(reason) = result.skipped_reason {
            // A refused request does not complete the queue or run completion
            // work. Earlier commits remain valid and the task stays retryable.
            return Ok(if batches == 0 {
                initial_skip(&reason).with_processed(processed)
            } else {
                ScheduledSliceResult::complete(has_more).with_processed(processed)
            });
        }
        batches = batches.saturating_add(1);
        has_more = result.has_more;
        if runner.is_background() {
            return Ok(ScheduledSliceResult::complete(has_more).with_processed(processed));
        }
        if !has_more {
            tracing::info!(
                "[SCHEDULER] automatic model quantum completed task={} batches={} elapsed_ms={}",
                kind.as_str(),
                batches,
                runner.elapsed().as_millis(),
            );
            runner.finish().await;
            return Ok(ScheduledSliceResult::complete(false).with_processed(processed));
        }
    }
}

fn initial_skip(reason: &str) -> ScheduledSliceResult {
    if reason == AutomaticSliceStopReason::ManualRequestPending.as_str() {
        ScheduledSliceResult::complete(true)
    } else {
        ScheduledSliceResult::skipped(reason)
    }
}

#[cfg(test)]
mod tests;
