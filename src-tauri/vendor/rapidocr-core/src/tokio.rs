//! Tokio convenience wrapper backed by a dedicated, single-owner OCR worker.

use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use ::tokio as tokio_runtime;
use anyhow::{anyhow, Result as AnyResult};
use image::RgbImage;
use thiserror::Error;
use tokio_runtime::sync::{mpsc, oneshot};

use crate::{
    cancellation::{is_cancelled_error, OcrCancellationToken, OcrCancelled},
    config::RapidOcrConfig,
    types::TimedOcrOutput,
    RapidOcr,
};

const DEFAULT_QUEUE_CAPACITY: usize = 1;

#[derive(Debug, Error)]
/// Errors produced by the Tokio OCR worker and its request tasks.
pub enum TokioOcrError {
    /// The bounded request queue is full.
    #[error("Tokio OCR request queue is full")]
    QueueFull,
    /// The worker has stopped or is shutting down.
    #[error("Tokio OCR worker is not available")]
    WorkerStopped,
    /// The dedicated worker thread panicked.
    #[error("Tokio OCR worker thread panicked")]
    WorkerPanicked,
    /// The OCR request was cooperatively cancelled.
    #[error("OCR request was cancelled")]
    Cancelled,
    /// The timeout elapsed and cancellation was requested.
    #[error("OCR request timed out after {0:?}")]
    TimedOut(Duration),
    /// OCR initialization or execution failed.
    #[error(transparent)]
    Ocr(#[from] anyhow::Error),
}

#[derive(Debug, Error)]
#[error("Tokio OCR worker thread panicked")]
struct WorkerPanic;

enum WorkerInput {
    Image(Arc<RgbImage>),
    Path(PathBuf),
}

struct WorkerRequest {
    input: WorkerInput,
    cancellation: OcrCancellationToken,
    response: oneshot::Sender<AnyResult<TimedOcrOutput>>,
}

#[derive(Default)]
struct WorkerState {
    shutdown: AtomicBool,
    active: Mutex<Option<OcrCancellationToken>>,
}

impl WorkerState {
    fn set_active(&self, cancellation: OcrCancellationToken) {
        *self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancellation);
    }

    fn clear_active(&self, cancellation: &OcrCancellationToken) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .as_ref()
            .is_some_and(|current| current.ptr_eq(cancellation))
        {
            *active = None;
        }
    }

    fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(active) = active {
            active.cancel();
        }
    }
}

/// Tokio-facing OCR service backed by one dedicated worker thread.
///
/// The worker owns one [`RapidOcr`] instance and processes requests
/// sequentially. This avoids blocking Tokio executor threads and prevents
/// concurrent use of the stateful ONNX Runtime sessions.
///
/// Dropping the service requests cooperative shutdown and detaches the worker
/// thread. Call [`TokioRapidOcr::shutdown`] when the caller must wait until the
/// active request has stopped and the worker's resources have been released.
pub struct TokioRapidOcr {
    sender: Option<mpsc::Sender<WorkerRequest>>,
    worker: Option<JoinHandle<()>>,
    state: Arc<WorkerState>,
}

impl TokioRapidOcr {
    /// Starts a worker with a bounded queue capacity of one request.
    pub async fn new(config: RapidOcrConfig) -> Result<Self, TokioOcrError> {
        Self::new_with_queue_capacity(config, DEFAULT_QUEUE_CAPACITY).await
    }

    /// Starts a worker with the requested bounded queue capacity.
    pub async fn new_with_queue_capacity(
        config: RapidOcrConfig,
        queue_capacity: usize,
    ) -> Result<Self, TokioOcrError> {
        if queue_capacity == 0 {
            return Err(TokioOcrError::Ocr(anyhow!(
                "Tokio OCR queue capacity must be greater than zero"
            )));
        }

        let (sender, receiver) = mpsc::channel(queue_capacity);
        let (init_sender, init_receiver) = oneshot::channel();
        let state = Arc::new(WorkerState::default());
        let worker_state = Arc::clone(&state);
        let worker = thread::Builder::new()
            .name("rapidocr-tokio-worker".to_string())
            .spawn(move || worker_main(config, receiver, worker_state, init_sender))
            .map_err(|error| TokioOcrError::Ocr(error.into()))?;

        match init_receiver.await {
            Ok(Ok(())) => Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
                state,
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(TokioOcrError::Ocr(error))
            }
            Err(_) => {
                let panicked = worker.join().is_err();
                if panicked {
                    Err(TokioOcrError::WorkerPanicked)
                } else {
                    Err(TokioOcrError::WorkerStopped)
                }
            }
        }
    }

    /// Enqueues an owned/shared RGB image without waiting for queue capacity.
    pub fn submit_image(
        &self,
        image: Arc<RgbImage>,
    ) -> Result<OcrTask<TimedOcrOutput>, TokioOcrError> {
        self.submit(WorkerInput::Image(image))
    }

    /// Enqueues an image path without waiting for queue capacity.
    pub fn submit_path(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<OcrTask<TimedOcrOutput>, TokioOcrError> {
        self.submit(WorkerInput::Path(path.into()))
    }

    /// Runs OCR on an image and awaits completion.
    pub async fn run_image(&self, image: Arc<RgbImage>) -> Result<TimedOcrOutput, TokioOcrError> {
        self.submit_image(image)?.wait().await
    }

    /// Runs OCR on a path and awaits completion.
    pub async fn run_path(
        &self,
        path: impl Into<PathBuf>,
    ) -> Result<TimedOcrOutput, TokioOcrError> {
        self.submit_path(path)?.wait().await
    }

    /// Runs OCR with a timeout that includes time spent in the request queue.
    ///
    /// When the duration elapses, cancellation is requested and this method
    /// waits for the worker to observe it and finish cleaning up before
    /// returning [`TokioOcrError::TimedOut`]. It is therefore a cooperative,
    /// not hard-real-time, deadline.
    pub async fn run_image_with_timeout(
        &self,
        image: Arc<RgbImage>,
        timeout: Duration,
    ) -> Result<TimedOcrOutput, TokioOcrError> {
        self.submit_image(image)?.timeout(timeout).await
    }

    /// Runs path-based OCR with a cooperative timeout.
    pub async fn run_path_with_timeout(
        &self,
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<TimedOcrOutput, TokioOcrError> {
        self.submit_path(path)?.timeout(timeout).await
    }

    /// Cancels the active request, closes the queue, and joins the worker.
    pub async fn shutdown(mut self) -> Result<(), TokioOcrError> {
        self.begin_shutdown();
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        tokio_runtime::task::spawn_blocking(move || worker.join())
            .await
            .map_err(|_| TokioOcrError::WorkerPanicked)?
            .map_err(|_| TokioOcrError::WorkerPanicked)
    }

    fn submit(&self, input: WorkerInput) -> Result<OcrTask<TimedOcrOutput>, TokioOcrError> {
        if self.state.shutdown.load(Ordering::Acquire) {
            return Err(TokioOcrError::WorkerStopped);
        }
        let cancellation = OcrCancellationToken::new();
        let (response, receiver) = oneshot::channel();
        let request = WorkerRequest {
            input,
            cancellation: cancellation.clone(),
            response,
        };
        self.sender
            .as_ref()
            .ok_or(TokioOcrError::WorkerStopped)?
            .try_send(request)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TokioOcrError::QueueFull,
                mpsc::error::TrySendError::Closed(_) => TokioOcrError::WorkerStopped,
            })?;
        Ok(OcrTask {
            cancellation,
            receiver: Some(receiver),
            completed: false,
        })
    }

    fn begin_shutdown(&mut self) {
        self.state.begin_shutdown();
        self.sender.take();
    }
}

impl Drop for TokioRapidOcr {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

/// Handle for one queued or running Tokio OCR request.
///
/// Dropping an incomplete task requests cancellation. Use
/// [`OcrTask::cancel_and_wait`] or [`OcrTask::timeout`] when the caller must
/// wait until the worker has finished cleaning up.
pub struct OcrTask<T> {
    cancellation: OcrCancellationToken,
    receiver: Option<oneshot::Receiver<AnyResult<T>>>,
    completed: bool,
}

impl<T> OcrTask<T> {
    /// Requests cooperative cancellation without waiting for completion.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Waits for the request to finish.
    pub async fn wait(mut self) -> Result<T, TokioOcrError> {
        self.receive().await
    }

    /// Requests cancellation and waits for worker cleanup.
    pub async fn cancel_and_wait(mut self) -> Result<T, TokioOcrError> {
        self.cancellation.cancel();
        self.receive().await
    }

    /// Waits until the timeout, then cancels and drains the worker response.
    pub async fn timeout(mut self, timeout: Duration) -> Result<T, TokioOcrError> {
        let receiver = self.receiver.as_mut().ok_or(TokioOcrError::WorkerStopped)?;
        match tokio_runtime::time::timeout(timeout, receiver).await {
            Ok(response) => {
                self.receiver.take();
                self.completed = true;
                map_response(response)
            }
            Err(_) => {
                self.cancellation.cancel();
                if let Some(receiver) = self.receiver.take() {
                    let _ = receiver.await;
                }
                self.completed = true;
                Err(TokioOcrError::TimedOut(timeout))
            }
        }
    }

    #[cfg(test)]
    fn cancellation_token(&self) -> &OcrCancellationToken {
        &self.cancellation
    }

    async fn receive(&mut self) -> Result<T, TokioOcrError> {
        let receiver = self.receiver.take().ok_or(TokioOcrError::WorkerStopped)?;
        let response = receiver.await;
        self.completed = true;
        map_response(response)
    }
}

impl<T> Drop for OcrTask<T> {
    fn drop(&mut self) {
        if !self.completed {
            self.cancellation.cancel();
        }
    }
}

fn map_response<T>(
    response: Result<AnyResult<T>, oneshot::error::RecvError>,
) -> Result<T, TokioOcrError> {
    match response {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) if is_cancelled_error(&error) => Err(TokioOcrError::Cancelled),
        Ok(Err(error)) if error.downcast_ref::<WorkerPanic>().is_some() => {
            Err(TokioOcrError::WorkerPanicked)
        }
        Ok(Err(error)) => Err(TokioOcrError::Ocr(error)),
        Err(_) => Err(TokioOcrError::WorkerStopped),
    }
}

fn worker_main(
    config: RapidOcrConfig,
    mut receiver: mpsc::Receiver<WorkerRequest>,
    state: Arc<WorkerState>,
    init_sender: oneshot::Sender<AnyResult<()>>,
) {
    let mut ocr = match RapidOcr::new(config) {
        Ok(ocr) => ocr,
        Err(error) => {
            let _ = init_sender.send(Err(error));
            return;
        }
    };
    if init_sender.send(Ok(())).is_err() {
        return;
    }

    while let Some(request) = receiver.blocking_recv() {
        if state.shutdown.load(Ordering::Acquire) {
            let _ = request.response.send(Err(OcrCancelled.into()));
            break;
        }
        if request.cancellation.is_cancelled() {
            let _ = request.response.send(Err(OcrCancelled.into()));
            continue;
        }

        state.set_active(request.cancellation.clone());
        let result = catch_unwind(AssertUnwindSafe(|| match request.input {
            WorkerInput::Image(image) => {
                ocr.run_image_cancellable_timed(&image, &request.cancellation)
            }
            WorkerInput::Path(path) => ocr.run_path_cancellable_timed(path, &request.cancellation),
        }));
        state.clear_active(&request.cancellation);

        match result {
            Ok(result) => {
                let _ = request.response.send(result);
            }
            Err(_) => {
                let _ = request.response.send(Err(WorkerPanic.into()));
                state.begin_shutdown();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_task() -> (
        OcrTask<()>,
        oneshot::Sender<AnyResult<()>>,
        OcrCancellationToken,
    ) {
        let cancellation = OcrCancellationToken::new();
        let observed = cancellation.clone();
        let (sender, receiver) = oneshot::channel();
        (
            OcrTask {
                cancellation,
                receiver: Some(receiver),
                completed: false,
            },
            sender,
            observed,
        )
    }

    #[tokio_runtime::test]
    async fn dropping_pending_task_requests_cancellation() {
        let (task, _sender, observed) = pending_task();

        drop(task);

        assert!(observed.is_cancelled());
    }

    #[tokio_runtime::test]
    async fn timeout_cancels_and_waits_for_worker_response() {
        let (task, sender, observed) = pending_task();
        let worker_observed = observed.clone();
        std::thread::spawn(move || {
            while !worker_observed.is_cancelled() {
                std::thread::yield_now();
            }
            let _ = sender.send(Err(OcrCancelled.into()));
        });

        let error = task.timeout(Duration::from_millis(10)).await.unwrap_err();

        assert!(matches!(error, TokioOcrError::TimedOut(_)));
        assert!(observed.is_cancelled());
    }

    #[tokio_runtime::test]
    async fn explicit_cancel_is_reported_as_cancelled() {
        let (task, sender, observed) = pending_task();
        task.cancel();
        let _ = sender.send(Err(OcrCancelled.into()));

        let error = task.wait().await.unwrap_err();

        assert!(matches!(error, TokioOcrError::Cancelled));
        assert!(observed.is_cancelled());
    }

    #[tokio_runtime::test]
    async fn worker_panic_marker_is_reported_as_worker_panicked() {
        let (task, sender, _observed) = pending_task();
        let _ = sender.send(Err(WorkerPanic.into()));

        let error = task.wait().await.unwrap_err();

        assert!(matches!(error, TokioOcrError::WorkerPanicked));
    }

    #[test]
    fn task_exposes_the_same_cancellation_token() {
        let (task, _sender, observed) = pending_task();
        assert!(task.cancellation_token().ptr_eq(&observed));
    }

    #[test]
    fn public_tokio_handles_are_send_and_service_is_sync() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send::<OcrTask<TimedOcrOutput>>();
        assert_send_sync::<TokioRapidOcr>();
    }
}
