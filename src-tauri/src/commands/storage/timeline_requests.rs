//! Cancellation tickets scoped to the calling window, including cancel-before-start.
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type RequestKey = (String, String);

#[derive(Default)]
pub(super) struct TimelineRequests {
    inner: Mutex<Requests>,
}

#[derive(Default)]
struct Requests {
    active: HashMap<RequestKey, Arc<AtomicBool>>,
    // IPC delivery can race cancellation with registration. Keep only a bounded
    // number of tombstones; they contain no screenshot data or credentials.
    cancelled: VecDeque<RequestKey>,
}

pub(super) struct Ticket<'a> {
    registry: &'a TimelineRequests,
    key: RequestKey,
    pub cancelled: Arc<AtomicBool>,
}

impl TimelineRequests {
    pub fn begin(&self, window: &str, id: &str) -> Result<Ticket<'_>, String> {
        let key = (window.to_owned(), id.to_owned());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active.contains_key(&key) {
            return Err("Duplicate timeline request".into());
        }
        let was_cancelled = inner.cancelled.iter().position(|entry| entry == &key);
        if let Some(index) = was_cancelled {
            inner.cancelled.remove(index);
        }
        let cancelled = Arc::new(AtomicBool::new(was_cancelled.is_some()));
        inner.active.insert(key.clone(), cancelled.clone());
        Ok(Ticket {
            registry: self,
            key,
            cancelled,
        })
    }

    pub fn cancel(&self, window: &str, id: &str) {
        let key = (window.to_owned(), id.to_owned());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = inner.active.get(&key) {
            active.store(true, Ordering::Release);
        } else if !inner.cancelled.contains(&key) {
            if inner.cancelled.len() == 64 {
                inner.cancelled.pop_front();
            }
            inner.cancelled.push_back(key);
        }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.registry
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_handles_ipc_races_and_isolates_windows_and_requests() {
        let requests = TimelineRequests::default();
        requests.cancel("main", "old");
        let old = requests.begin("main", "old").unwrap();
        assert!(old.cancelled.load(Ordering::Acquire));
        let current = requests.begin("main", "new").unwrap();
        let other = requests.begin("other", "new").unwrap();
        requests.cancel("main", "new");
        assert!(current.cancelled.load(Ordering::Acquire));
        assert!(!other.cancelled.load(Ordering::Acquire));
        assert!(requests.begin("other", "new").is_err());
        drop(other);
        drop(current);
        drop(old);
        assert!(requests.inner.lock().unwrap().active.is_empty());
        for n in 0..1000 {
            requests.cancel("main", &n.to_string());
        }
        assert_eq!(requests.inner.lock().unwrap().cancelled.len(), 64);
    }
}
