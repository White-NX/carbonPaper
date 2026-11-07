//! Request-local, content-free progress for diagnosing vector retrieval.

use std::sync::Mutex;

#[derive(Clone, Debug)]
pub(crate) struct VectorQuerySnapshot {
    pub phase: &'static str,
    pub mode: &'static str,
    pub rows_scanned: usize,
    pub pages_scanned: u32,
    pub total_rows: Option<usize>,
    pub estimated_cache_bytes: Option<usize>,
    pub cache_budget_bytes: Option<usize>,
    pub epoch_ms: f64,
    pub load_wait_ms: f64,
    pub read_open_ms: f64,
    pub stats_ms: f64,
    pub scan_ms: f64,
    pub visibility_ms: f64,
}

impl Default for VectorQuerySnapshot {
    fn default() -> Self {
        Self {
            phase: "not_started",
            mode: "not_started",
            rows_scanned: 0,
            pages_scanned: 0,
            total_rows: None,
            estimated_cache_bytes: None,
            cache_budget_bytes: None,
            epoch_ms: 0.0,
            load_wait_ms: 0.0,
            read_open_ms: 0.0,
            stats_ms: 0.0,
            scan_ms: 0.0,
            visibility_ms: 0.0,
        }
    }
}

#[derive(Default)]
pub(crate) struct VectorQueryMetrics(Mutex<VectorQuerySnapshot>);

impl VectorQueryMetrics {
    pub(crate) fn update(&self, edit: impl FnOnce(&mut VectorQuerySnapshot)) {
        edit(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
    }

    pub(crate) fn snapshot(&self) -> VectorQuerySnapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
