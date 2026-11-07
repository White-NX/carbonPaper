//! Per-query timings and routing context. Never retain or log query text or user content.

use super::ClipQueryRequest;
use crate::clip_ann::ClipSearchStatus;
use crate::ml_protocol::MlSemanticTimings;
use crate::storage::VectorQueryMetrics;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct RequestShape {
    limit: u32,
    offset: u32,
    process_filters: usize,
    category_filters: usize,
    has_start_time: bool,
    has_end_time: bool,
    include_ocr: bool,
}

#[derive(Clone)]
struct Progress {
    stage: &'static str,
    stage_started: Instant,
    timings_ms: BTreeMap<&'static str, f64>,
    ann: ClipSearchStatus,
    retrieval: &'static str,
    fallback_reason: &'static str,
    ann_attempts: u32,
    ann_tail_rows: Option<usize>,
    candidates: usize,
    filtered_candidates: usize,
    embedding: Option<MlSemanticTimings>,
}

pub(super) struct QueryDiagnostics {
    pub(super) started: tokio::time::Instant,
    id: u64,
    request: RequestShape,
    progress: Mutex<Progress>,
    pub(super) vectors: Arc<VectorQueryMetrics>,
}

impl QueryDiagnostics {
    pub(super) fn new(request: &ClipQueryRequest<'_>, ann: ClipSearchStatus) -> Self {
        Self {
            started: tokio::time::Instant::now(),
            id: NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed),
            request: RequestShape {
                limit: request.limit,
                offset: request.offset,
                process_filters: request.process_names.len(),
                category_filters: request.categories.len(),
                has_start_time: request.start_time.is_some(),
                has_end_time: request.end_time.is_some(),
                include_ocr: request.include_ocr,
            },
            progress: Mutex::new(Progress {
                stage: "admission",
                stage_started: Instant::now(),
                timings_ms: BTreeMap::new(),
                ann,
                retrieval: "unselected",
                fallback_reason: "none",
                ann_attempts: 0,
                ann_tail_rows: None,
                candidates: 0,
                filtered_candidates: 0,
                embedding: None,
            }),
            vectors: Arc::new(VectorQueryMetrics::default()),
        }
    }

    pub(super) fn begin(&self, stage: &'static str) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.stage = stage;
        progress.stage_started = Instant::now();
    }

    pub(super) fn finish(&self, stage: &'static str, elapsed: Duration, error: Option<&str>) {
        let elapsed_ms = milliseconds(elapsed);
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            *progress.timings_ms.entry(stage).or_default() += elapsed_ms;
        }
        if let Some(error) = error {
            tracing::warn!(
                query_id = self.id,
                stage,
                elapsed_ms,
                error_kind = error_kind(error),
                "[CLIP] query stage failed"
            );
        } else {
            tracing::debug!(
                query_id = self.id,
                stage,
                elapsed_ms,
                "[CLIP] query stage completed"
            );
        }
    }

    pub(super) fn ann_status(&self, ann: ClipSearchStatus) {
        self.progress.lock().unwrap_or_else(|e| e.into_inner()).ann = ann;
    }

    pub(super) fn fallback(&self, reason: &'static str) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fallback_reason = reason;
    }

    pub(super) fn fallback_if_unset(&self, reason: &'static str) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if progress.fallback_reason == "none" {
            progress.fallback_reason = reason;
        }
    }

    pub(super) fn retrieval(&self, mode: &'static str) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retrieval = mode;
    }

    pub(super) fn ann_attempt(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.ann_attempts += 1;
    }

    pub(super) fn ann_candidates(&self, candidates: usize, tail_rows: usize) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        progress.candidates = candidates;
        progress.ann_tail_rows = Some(tail_rows);
    }

    pub(super) fn filtered(&self, candidates: usize) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .filtered_candidates = candidates;
    }

    pub(super) fn embedding(&self, timings: MlSemanticTimings) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .embedding = Some(timings);
    }

    fn log(
        &self,
        level: tracing::Level,
        event: &'static str,
        error: Option<&str>,
        returned: Option<usize>,
    ) {
        // Logging can wait on a slow disk. Release the progress lock before emitting it.
        let progress = self
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let vector_scan = self.vectors.snapshot();
        macro_rules! record {
            ($level:expr) => {
                tracing::event!($level,
                    query_id = self.id,
                    stage = progress.stage,
                    stage_ms = milliseconds(progress.stage_started.elapsed()),
                    total_ms = milliseconds(self.started.elapsed()),
                    debug_build = cfg!(debug_assertions),
                    ann_state = progress.ann.ann_state,
                    ann_generation = ?progress.ann.ann_generation,
                    ann_unavailable_reason = ?progress.ann.fallback_reason,
                    retrieval = progress.retrieval,
                    fallback_reason = progress.fallback_reason,
                    ann_attempts = progress.ann_attempts,
                    ann_tail_rows = ?progress.ann_tail_rows,
                    candidates = progress.candidates,
                    filtered_candidates = progress.filtered_candidates,
                    returned = ?returned,
                    error_kind = error.map(error_kind).unwrap_or("none"),
                    limit = self.request.limit,
                    offset = self.request.offset,
                    process_filters = self.request.process_filters,
                    category_filters = self.request.category_filters,
                    has_start_time = self.request.has_start_time,
                    has_end_time = self.request.has_end_time,
                    include_ocr = self.request.include_ocr,
                    embedding = ?progress.embedding,
                    timings_ms = ?progress.timings_ms,
                    vector_scan = ?vector_scan,
                    "[CLIP] {event}")
            };
        }
        match level {
            tracing::Level::ERROR => record!(tracing::Level::ERROR),
            tracing::Level::WARN => record!(tracing::Level::WARN),
            _ => record!(tracing::Level::DEBUG),
        }
    }
}

pub(super) fn query_stage<T>(
    diagnostics: &QueryDiagnostics,
    stage: &'static str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    diagnostics.begin(stage);
    let started = Instant::now();
    let result = operation();
    diagnostics.finish(
        stage,
        started.elapsed(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

pub(super) async fn query_stage_async<T>(
    diagnostics: &QueryDiagnostics,
    stage: &'static str,
    operation: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    diagnostics.begin(stage);
    let started = Instant::now();
    let result = operation.await;
    diagnostics.finish(
        stage,
        started.elapsed(),
        result.as_ref().err().map(String::as_str),
    );
    result
}

/// Observe the whole Rust query, including readiness checks and model/worker waits.
pub(super) async fn observe_query(
    query: impl Future<Output = Result<Vec<serde_json::Value>, String>>,
    diagnostics: &QueryDiagnostics,
    slow_after: Duration,
    budget: Duration,
) -> Result<Vec<serde_json::Value>, String> {
    tokio::pin!(query);
    let slow = tokio::time::sleep_until(diagnostics.started + slow_after);
    let deadline = tokio::time::sleep_until(diagnostics.started + budget);
    tokio::pin!(slow, deadline);
    let mut warned = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut deadline => {
                diagnostics.log(tracing::Level::ERROR, "query timed out", Some("query_deadline_exceeded"), None);
                return Err("query_deadline_exceeded".into());
            }
            result = &mut query => {
                match &result {
                    Err(error) if error.starts_with("query_deadline_exceeded") => {
                        diagnostics.log(tracing::Level::ERROR, "query timed out", Some(error), None);
                    }
                    Err(error) => diagnostics.log(tracing::Level::WARN, "query failed", Some(error), None),
                    Ok(results) => diagnostics.log(
                        if warned || diagnostics.started.elapsed() > slow_after {
                            tracing::Level::WARN
                        } else { tracing::Level::DEBUG },
                        "query completed", None, Some(results.len())),
                }
                return result;
            }
            _ = &mut slow, if !warned => {
                warned = true;
                diagnostics.log(tracing::Level::WARN, "query exceeded slow threshold", None, None);
            }
        }
    }
}

fn error_kind(error: &str) -> &'static str {
    if error.starts_with("query_deadline_exceeded") {
        "deadline"
    } else if error.contains("AUTH_REQUIRED") {
        "authorization"
    } else if error.starts_with("embed_failed") {
        "embedding"
    } else if error.starts_with("migration_") || error == "maintenance_in_progress" {
        "storage_not_ready"
    } else if error.starts_with("storage_changed")
        || error.starts_with("ann_storage_generation_changed")
    {
        "storage_changed"
    } else if error.starts_with("scan_task_failed") {
        "query_task"
    } else {
        "retrieval"
    }
}

fn milliseconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 100_000.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_query::{QUERY_SLOW_WARN_AFTER, QUERY_TOTAL_TIMEOUT};
    use std::io::{self, Write};
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn diagnostics() -> Arc<QueryDiagnostics> {
        let request = ClipQueryRequest::new("private search text", Some(10), None, &[], None, None);
        let diagnostics = Arc::new(QueryDiagnostics::new(
            &request,
            ClipSearchStatus {
                ready: false,
                ann_state: "exact_fallback",
                ann_generation: None,
                fallback_reason: Some("generation_missing"),
            },
        ));
        diagnostics.begin("exact");
        diagnostics.fallback("generation_missing");
        diagnostics.retrieval("sql_exact");
        diagnostics.vectors.update(|progress| {
            progress.phase = "vector_scan";
            progress.mode = "streaming";
            progress.rows_scanned = 4096;
            progress.total_rows = Some(100_000);
        });
        diagnostics
    }

    fn subscriber(logs: Arc<Mutex<Vec<u8>>>) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || LogWriter(logs.clone()))
            .finish()
    }

    fn logs(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn slow_query_warns_at_twenty_seconds_and_times_out_at_sixty_with_routing_context() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = diagnostics();
        let task = tokio::spawn(
            async move {
                observe_query(
                    std::future::pending(),
                    &diagnostics,
                    QUERY_SLOW_WARN_AFTER,
                    QUERY_TOTAL_TIMEOUT,
                )
                .await
            }
            .with_subscriber(subscriber(buffer.clone())),
        );
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(19)).await;
        assert!(!logs(&buffer).contains("WARN"));
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        let warning = logs(&buffer);
        assert!(warning.contains("WARN"));
        assert!(warning.contains("stage=\"exact\""));
        assert!(warning.contains("retrieval=\"sql_exact\""));
        assert!(warning.contains("generation_missing"));
        assert!(warning.contains("rows_scanned: 4096"));
        assert!(!warning.contains("ERROR"));
        assert!(!warning.contains("private search text"));
        tokio::time::advance(Duration::from_secs(39)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(task.await.unwrap().unwrap_err(), "query_deadline_exceeded");
        assert!(logs(&buffer).contains("ERROR"));
    }

    #[tokio::test(start_paused = true)]
    async fn slow_success_returns_results_without_logging_their_content() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = diagnostics();
        let expected = vec![serde_json::json!({"window_title": "private window content"})];
        let results = async {
            observe_query(
                async {
                    tokio::time::sleep(Duration::from_secs(21)).await;
                    Ok(expected.clone())
                },
                &diagnostics,
                QUERY_SLOW_WARN_AFTER,
                QUERY_TOTAL_TIMEOUT,
            )
            .await
        }
        .with_subscriber(subscriber(buffer.clone()))
        .await
        .unwrap();
        assert_eq!(results, expected);
        let output = logs(&buffer);
        assert!(output.contains("WARN"));
        assert!(output.contains("query completed"));
        assert!(!output.contains("ERROR"));
        assert!(!output.contains("private search text"));
        assert!(!output.contains("private window content"));
    }
}
