use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use ort::session::RunOptions;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("OCR operation was cancelled")]
/// Error returned when an OCR request is cancelled.
pub struct OcrCancelled;

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    active_run: Mutex<Option<Arc<RunOptions>>>,
}

#[derive(Clone, Default)]
/// Thread-safe, single-use cancellation signal for one OCR request.
///
/// Cancellation is terminal: create a new token for every OCR request. Calling
/// [`OcrCancellationToken::cancel`] asks an active ONNX Runtime invocation to
/// terminate and prevents later pipeline stages or recognition batches from
/// starting.
pub struct OcrCancellationToken {
    state: Arc<CancellationState>,
}

impl OcrCancellationToken {
    /// Creates a token in the non-cancelled state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation and signals the active ONNX Runtime run, if any.
    ///
    /// ONNX Runtime termination is cooperative. The active operator or
    /// execution provider may take additional time to reach a cancellation
    /// checkpoint.
    pub fn cancel(&self) {
        let _ = self.try_cancel();
    }

    /// Requests cancellation and reports an error if ONNX Runtime rejects the
    /// termination signal.
    pub fn try_cancel(&self) -> Result<()> {
        self.state.cancelled.store(true, Ordering::Release);
        let active_run = self
            .state
            .active_run
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(active_run) = active_run {
            active_run
                .terminate()
                .map_err(|err| anyhow!(err.to_string()))?;
        }
        Ok(())
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Returns [`OcrCancelled`] when cancellation has been requested.
    pub fn checkpoint(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(OcrCancelled.into());
        }
        Ok(())
    }

    pub(crate) fn begin_onnx_run(&self) -> Result<ActiveRunOptions> {
        self.checkpoint()?;
        let options = Arc::new(RunOptions::new().map_err(|err| anyhow!(err.to_string()))?);
        {
            let mut active_run = self
                .state
                .active_run
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *active_run = Some(Arc::clone(&options));
        }

        if self.is_cancelled() {
            let _ = options.terminate();
            self.clear_active_run(&options);
            return Err(OcrCancelled.into());
        }

        Ok(ActiveRunOptions {
            token: self.clone(),
            options,
        })
    }

    fn clear_active_run(&self, options: &Arc<RunOptions>) {
        let mut active_run = self
            .state
            .active_run
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active_run
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, options))
        {
            *active_run = None;
        }
    }

    #[cfg(feature = "tokio")]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    #[cfg(test)]
    pub(crate) fn has_active_onnx_run(&self) -> bool {
        self.state
            .active_run
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

pub(crate) struct ActiveRunOptions {
    token: OcrCancellationToken,
    options: Arc<RunOptions>,
}

impl ActiveRunOptions {
    pub(crate) fn options(&self) -> &RunOptions {
        &self.options
    }
}

impl Drop for ActiveRunOptions {
    fn drop(&mut self) {
        self.token.clear_active_run(&self.options);
    }
}

/// Returns whether an `anyhow` error represents cooperative OCR cancellation.
pub fn is_cancelled_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OcrCancelled>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_terminal_and_detectable() {
        let token = OcrCancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();

        assert!(token.is_cancelled());
        let error = token.checkpoint().unwrap_err();
        assert!(is_cancelled_error(&error));
    }

    #[test]
    fn cancellation_before_onnx_registration_prevents_run() {
        let token = OcrCancellationToken::new();
        token.cancel();

        let error = match token.begin_onnx_run() {
            Ok(_) => panic!("cancelled token unexpectedly created run options"),
            Err(error) => error,
        };

        assert!(is_cancelled_error(&error));
    }
}
