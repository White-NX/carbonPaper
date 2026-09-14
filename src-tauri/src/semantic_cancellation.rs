//! Request IDs fence cancellation, including requests waiting to start loading.
use ort::session::RunOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct RequestControl {
    pub id: u64,
    cancelled: AtomicBool,
    options: Mutex<Option<Arc<RunOptions>>>,
}

impl RequestControl {
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err("cancelled: semantic request was revoked".into())
        } else {
            Ok(())
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(options) = self
            .options
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = options.terminate();
        }
    }
    pub fn run_options(&self) -> Result<Arc<RunOptions>, String> {
        self.check()?;
        let mut options = self.options.lock().unwrap_or_else(|e| e.into_inner());
        self.check()?;
        if let Some(options) = options.as_ref() {
            return Ok(options.clone());
        }
        let new = Arc::new(RunOptions::new().map_err(|e| format!("inference: run options: {e}"))?);
        *options = Some(new.clone());
        Ok(new)
    }
}

#[derive(Default)]
pub struct CancellationRegistry {
    active: Mutex<Option<Arc<RequestControl>>>,
}

impl CancellationRegistry {
    /// Called by the independent reader *before* it queues a request, closing
    /// the cancel-before-execute race without retaining arbitrary old IDs.
    pub fn register(&self, id: u64) -> Arc<RequestControl> {
        let control = Arc::new(RequestControl {
            id,
            cancelled: AtomicBool::new(false),
            options: Mutex::new(None),
        });
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = Some(control.clone());
        control
    }
    pub fn cancel(&self, id: u64) {
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|c| c.id == id)
        {
            active.cancel();
        }
    }
    pub fn finish(&self, id: u64) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.as_ref().is_some_and(|c| c.id == id) {
            active.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_before_loading_and_late_control_cannot_affect_the_next_request() {
        let registry = CancellationRegistry::default();
        let first = registry.register(8);
        registry.cancel(8);
        assert!(first.check().is_err());
        registry.finish(8);
        let next = registry.register(9);
        registry.cancel(8);
        registry.finish(8);
        assert!(next.check().is_ok());
        registry.cancel(9);
        assert!(next.check().is_err());
    }
}
