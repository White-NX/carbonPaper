//! Window-scoped CLIP cancellation, including cancel-before-registration IPC races.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type RequestKey = (String, String);

#[derive(Default)]
pub(crate) struct ClipQueryRequests(Mutex<Requests>);

#[derive(Default)]
struct Requests {
    active: HashMap<RequestKey, Arc<AtomicBool>>,
    cancelled: VecDeque<RequestKey>,
}

pub(crate) struct Ticket<'a> {
    registry: &'a ClipQueryRequests,
    key: RequestKey,
    pub cancelled: Arc<AtomicBool>,
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 {
        return Err("Invalid CLIP request id".into());
    }
    Ok(())
}

impl ClipQueryRequests {
    pub(crate) fn begin(&self, window: &str, id: &str) -> Result<Ticket<'_>, String> {
        validate_id(id)?;
        let key = (window.to_owned(), id.to_owned());
        let mut requests = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if requests.active.contains_key(&key) {
            return Err("Duplicate CLIP request id".into());
        }
        let previous = requests
            .cancelled
            .iter()
            .position(|candidate| candidate == &key);
        if let Some(index) = previous {
            requests.cancelled.remove(index);
        }
        let cancelled = Arc::new(AtomicBool::new(previous.is_some()));
        requests.active.insert(key.clone(), cancelled.clone());
        Ok(Ticket {
            registry: self,
            key,
            cancelled,
        })
    }

    pub(crate) fn cancel(&self, window: &str, id: &str) -> Result<(), String> {
        validate_id(id)?;
        let key = (window.to_owned(), id.to_owned());
        let mut requests = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(active) = requests.active.get(&key) {
            active.store(true, Ordering::Release);
        } else if !requests.cancelled.contains(&key) {
            if requests.cancelled.len() == 64 {
                requests.cancelled.pop_front();
            }
            requests.cancelled.push_back(key);
        }
        Ok(())
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.registry
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_cancellation_is_window_scoped_and_bounds_cancel_before_start_tickets() {
        let requests = ClipQueryRequests::default();
        requests.cancel("main", "old").unwrap();
        let old = requests.begin("main", "old").unwrap();
        assert!(old.cancelled.load(Ordering::Acquire));
        let current = requests.begin("main", "current").unwrap();
        let other = requests.begin("other", "current").unwrap();
        requests.cancel("main", "current").unwrap();
        assert!(current.cancelled.load(Ordering::Acquire));
        assert!(!other.cancelled.load(Ordering::Acquire));
        assert!(requests.begin("other", "current").is_err());
        drop((old, current, other));
        assert!(requests.0.lock().unwrap().active.is_empty());
        for id in 0..1000 {
            requests.cancel("main", &id.to_string()).unwrap();
        }
        assert_eq!(requests.0.lock().unwrap().cancelled.len(), 64);
    }
}
