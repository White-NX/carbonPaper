//! Application-owned Windows Hello request coordination across WebViews.
//!
//! Only overlapping requests share a result. A later explicit request always
//! runs the supplied verification again, even while the session is valid.

use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

type VerificationResult = Result<(), String>;
type PendingVerification = watch::Receiver<Option<VerificationResult>>;

#[derive(Default)]
pub(crate) struct VerificationCoordinator {
    in_flight: Mutex<Option<PendingVerification>>,
}

impl VerificationCoordinator {
    pub(crate) async fn run<F>(self: &Arc<Self>, operation: F) -> VerificationResult
    where
        F: Future<Output = VerificationResult> + Send + 'static,
    {
        let mut result = {
            let mut in_flight = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pending) = in_flight.as_ref() {
                pending.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                *in_flight = Some(receiver.clone());
                let coordinator = Arc::clone(self);
                // The supervisor and its worker outlive any individual caller.
                // Dropping a WebView request must not release admission while
                // native UI (or the post-authentication work) is still running.
                tokio::spawn(async move {
                    let outcome = tokio::spawn(operation).await.unwrap_or_else(|error| {
                        Err(format!("Authentication task failed: {error}"))
                    });
                    let mut in_flight = coordinator
                        .in_flight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    // Publish and release under one lock: waiters retain this
                    // result, while the next admitted request starts fresh.
                    sender.send_replace(Some(outcome));
                    *in_flight = None;
                });
                receiver
            }
        };

        loop {
            if let Some(outcome) = result.borrow_and_update().clone() {
                return outcome;
            }
            if result.changed().await.is_err() {
                return Err("Authentication task ended without a result".to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn overlapping_windows_share_the_first_operation_and_its_completion() {
        let coordinator = Arc::new(VerificationCoordinator::default());
        let verifications = Arc::new(AtomicUsize::new(0));
        let completions = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let count = verifications.clone();
        let completed = completions.clone();
        let mut main_request = Box::pin(coordinator.run(async move {
            count.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            finish_rx.await.unwrap();
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        assert!(futures::poll!(&mut main_request).is_pending());
        started_rx.await.unwrap();
        let mut settings_request = Box::pin(coordinator.run(async {
            panic!("the second window must not replace the first window's operation");
        }));
        assert!(futures::poll!(&mut settings_request).is_pending());
        finish_tx.send(()).unwrap();
        assert_eq!(main_request.await, Ok(()));
        assert_eq!(settings_request.await, Ok(()));
        assert_eq!(verifications.load(Ordering::SeqCst), 1);
        assert_eq!(completions.load(Ordering::SeqCst), 1);

        let count = verifications.clone();
        assert_eq!(
            coordinator
                .run(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await,
            Ok(())
        );
        assert_eq!(verifications.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancellation_is_shared_and_a_later_click_can_retry() {
        let coordinator = Arc::new(VerificationCoordinator::default());
        let (finish_tx, finish_rx) = oneshot::channel();
        let mut first = Box::pin(coordinator.run(async move {
            finish_rx.await.unwrap();
            Err("User cancelled authentication".to_string())
        }));
        let mut second = Box::pin(coordinator.run(async { Ok(()) }));
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        finish_tx.send(()).unwrap();
        let expected = Err("User cancelled authentication".to_string());
        assert_eq!(first.await, expected);
        assert_eq!(second.await, expected);
        assert_eq!(coordinator.run(async { Ok(()) }).await, Ok(()));
    }

    #[tokio::test]
    async fn dropping_all_callers_does_not_admit_another_native_prompt() {
        let coordinator = Arc::new(VerificationCoordinator::default());
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let mut first = Box::pin(coordinator.run(async move {
            // A blocking task models the native verification that cannot be
            // aborted just because the caller's async request was dropped.
            tokio::task::spawn_blocking(move || {
                started_tx.send(()).unwrap();
                finish_rx.blocking_recv().unwrap();
            })
            .await
            .unwrap();
            Ok(())
        }));
        assert!(futures::poll!(&mut first).is_pending());
        started_rx.await.unwrap();
        drop(first);
        let mut next = Box::pin(coordinator.run(async {
            panic!("native verification is still in flight");
        }));
        assert!(futures::poll!(&mut next).is_pending());
        finish_tx.send(()).unwrap();
        assert_eq!(next.await, Ok(()));
        assert_eq!(coordinator.run(async { Ok(()) }).await, Ok(()));
    }

    #[tokio::test]
    async fn worker_failure_reaches_all_waiters_and_releases_admission() {
        let coordinator = Arc::new(VerificationCoordinator::default());
        let (finish_tx, finish_rx) = oneshot::channel();
        let mut first = Box::pin(coordinator.run(async move {
            finish_rx.await.unwrap();
            panic!("simulated verification worker failure");
        }));
        let mut second = Box::pin(coordinator.run(async { Ok(()) }));
        assert!(futures::poll!(&mut first).is_pending());
        assert!(futures::poll!(&mut second).is_pending());
        finish_tx.send(()).unwrap();
        let failure = first.await.unwrap_err();
        assert!(failure.contains("Authentication task failed"));
        assert_eq!(second.await, Err(failure));
        assert_eq!(coordinator.run(async { Ok(()) }).await, Ok(()));
    }
}
