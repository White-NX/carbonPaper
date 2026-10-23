use super::*;
use crate::task_error::TaskError;
use std::collections::VecDeque;

struct FakeQuantum {
    kind: BackgroundTaskKind,
    results: VecDeque<TaskResult<ScheduledSliceResult>>,
    stops: VecDeque<Option<AutomaticSliceStopReason>>,
    enabled: VecDeque<bool>,
    background: bool,
    requests: Vec<(bool, bool)>,
    finished: usize,
}

impl FakeQuantum {
    fn new(kind: BackgroundTaskKind, results: Vec<TaskResult<ScheduledSliceResult>>) -> Self {
        Self {
            kind,
            results: results.into(),
            stops: VecDeque::new(),
            enabled: VecDeque::new(),
            background: false,
            requests: Vec::new(),
            finished: 0,
        }
    }
}

impl QuantumRunner for FakeQuantum {
    fn kind(&self) -> BackgroundTaskKind {
        self.kind
    }
    fn stop_reason(&self, _made_progress: bool) -> Option<AutomaticSliceStopReason> {
        self.stops.get(self.requests.len()).copied().flatten()
    }
    fn feature_enabled(&self) -> bool {
        self.enabled
            .get(self.requests.len())
            .copied()
            .unwrap_or(true)
    }
    fn is_background(&self) -> bool {
        self.background
    }
    fn elapsed(&self) -> Duration {
        Duration::ZERO
    }
    async fn run_request(
        &mut self,
        run_maintenance: bool,
        made_progress: bool,
    ) -> TaskResult<ScheduledSliceResult> {
        self.requests.push((run_maintenance, made_progress));
        self.results.pop_front().expect("unexpected extra batch")
    }
    async fn finish(&mut self) {
        self.finished += 1;
    }
}

fn kinds() -> [BackgroundTaskKind; 2] {
    [
        BackgroundTaskKind::SemanticIndex,
        BackgroundTaskKind::ClipIndex,
    ]
}

#[tokio::test]
async fn completed_quanta_maintain_once_and_accumulate_progress() {
    for kind in kinds() {
        let mut runner = FakeQuantum::new(
            kind,
            vec![
                Ok(ScheduledSliceResult::complete(true).with_processed(2)),
                Ok(ScheduledSliceResult::complete(false).with_processed(3)),
            ],
        );
        let result = run_quantum(&mut runner).await.unwrap();
        assert!(!result.has_more);
        assert!(result.skipped_reason.is_none());
        assert_eq!(result.processed, 5);
        assert_eq!(runner.requests, vec![(true, false), (false, true)]);
        assert_eq!(runner.finished, 1);
    }
}

#[tokio::test]
async fn first_batch_yields_distinguish_manual_requests_from_external_work() {
    for kind in kinds() {
        for reason in [
            AutomaticSliceStopReason::ManualRequestPending,
            AutomaticSliceStopReason::ExternalBackgroundRequest,
        ] {
            let mut runner = FakeQuantum::new(kind, vec![]);
            runner.stops.push_back(Some(reason));
            let result = run_quantum(&mut runner).await.unwrap();
            assert!(runner.requests.is_empty());
            assert_eq!(runner.finished, 0);
            if reason == AutomaticSliceStopReason::ManualRequestPending {
                assert!(result.has_more);
                assert!(result.skipped_reason.is_none());
            } else {
                assert_eq!(result.skipped_reason.as_deref(), Some(reason.as_str()));
            }
        }
    }
}

#[tokio::test]
async fn a_budget_yield_preserves_earlier_work_and_runs_completion_work() {
    for kind in kinds() {
        let mut runner = FakeQuantum::new(
            kind,
            vec![Ok(ScheduledSliceResult::complete(true).with_processed(4))],
        );
        runner.stops = vec![None, Some(AutomaticSliceStopReason::BudgetExpired)].into();
        let result = run_quantum(&mut runner).await.unwrap();
        assert!(result.has_more);
        assert!(result.skipped_reason.is_none());
        assert_eq!(result.processed, 4);
        assert_eq!(runner.finished, 1);
        assert_eq!(runner.requests.len(), 1);
    }
}

#[tokio::test]
async fn a_feature_disabled_between_batches_parks_the_task() {
    let mut runner = FakeQuantum::new(
        BackgroundTaskKind::SemanticIndex,
        vec![Ok(ScheduledSliceResult::complete(true).with_processed(4))],
    );
    runner.enabled = vec![true, false].into();
    let result = run_quantum(&mut runner).await.unwrap();
    assert_eq!(result.skipped_reason.as_deref(), Some("disabled"));
    assert_eq!(result.processed, 4);
    assert_eq!(runner.requests.len(), 1);
    assert_eq!(runner.finished, 0);
}

#[tokio::test]
async fn a_refused_request_preserves_progress_without_completion_work() {
    for kind in kinds() {
        let mut runner = FakeQuantum::new(
            kind,
            vec![
                Ok(ScheduledSliceResult::complete(true).with_processed(4)),
                Ok(ScheduledSliceResult::skipped("waiting_for_unlock")),
            ],
        );
        let result = run_quantum(&mut runner).await.unwrap();
        assert!(result.has_more);
        assert_eq!(result.processed, 4);
        assert!(result.skipped_reason.is_none());
        assert_eq!(runner.finished, 0);

        let mut first = FakeQuantum::new(
            kind,
            vec![Ok(ScheduledSliceResult::skipped("waiting_for_unlock"))],
        );
        let result = run_quantum(&mut first).await.unwrap();
        assert_eq!(result.skipped_reason.as_deref(), Some("waiting_for_unlock"));
        assert_eq!(first.finished, 0);
    }
}

#[tokio::test]
async fn background_profiles_stop_after_one_batch_without_admission_maintenance() {
    for kind in kinds() {
        let mut runner = FakeQuantum::new(
            kind,
            vec![Ok(ScheduledSliceResult::complete(true).with_processed(1))],
        );
        runner.background = true;
        let result = run_quantum(&mut runner).await.unwrap();
        assert!(result.has_more);
        assert_eq!(result.processed, 1);
        assert_eq!(runner.requests, vec![(false, false)]);
        assert_eq!(runner.finished, 0);
    }
}

#[tokio::test]
async fn worker_failures_reach_the_scheduler_without_finishing_the_queue() {
    for kind in kinds() {
        let mut runner = FakeQuantum::new(kind, vec![Err(TaskError::AuthenticationRequired)]);
        assert!(matches!(
            run_quantum(&mut runner).await,
            Err(TaskError::AuthenticationRequired)
        ));
        assert_eq!(runner.finished, 0);
    }
}
