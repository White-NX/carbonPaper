//! BGE classification inference and its diagnostic.
//!
//! Classification orchestration and learned anchors remain in the Python
//! post-process worker for now. The expensive text embedding call is served by
//! the shared Rust semantic worker by default, over authenticated reverse IPC.

use crate::ml_protocol::{
    MlSemanticModel, MAX_SEMANTIC_BATCH, MAX_SEMANTIC_TEXT_BYTES, MAX_SEMANTIC_TEXT_ITEM_BYTES,
};
use crate::semantic_runtime::SemanticRuntimeState;
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const EMBED_TIMEOUT: Duration = Duration::from_secs(120);
const FOREGROUND_WAIT_BUDGET: Duration = Duration::from_secs(15);
const BACKGROUND_BATCH_WAIT_BUDGET: Duration = Duration::from_secs(15);
const MAX_TEXTS_PER_REQUEST: usize = 512;

#[derive(Debug, Clone, Default)]
struct ClassificationDiagnosticInner {
    last_error: Option<String>,
    failure_count: u64,
    success_count: u64,
    last_elapsed_ms: Option<f64>,
}

impl ClassificationDiagnosticInner {
    fn record_rust_success(&mut self, elapsed_ms: f64) {
        self.last_error = None;
        self.success_count = self.success_count.saturating_add(1);
        self.last_elapsed_ms = Some(elapsed_ms);
    }

    fn record_rust_failure(&mut self, error: &str, elapsed_ms: f64) {
        self.last_error = Some(truncate_error(error));
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_elapsed_ms = Some(elapsed_ms);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassificationBackendStatus {
    pub last_error: Option<String>,
    pub failure_count: u64,
    pub success_count: u64,
    pub last_elapsed_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct BgeEmbeddingResponse {
    pub backend: &'static str,
    pub model_id: &'static str,
    pub dimensions: usize,
    pub vectors: Vec<Vec<f32>>,
    pub elapsed_ms: f64,
}

static DIAGNOSTIC: OnceLock<Mutex<ClassificationDiagnosticInner>> = OnceLock::new();

fn diagnostic() -> &'static Mutex<ClassificationDiagnosticInner> {
    DIAGNOSTIC.get_or_init(|| Mutex::new(ClassificationDiagnosticInner::default()))
}

pub fn backend_status() -> ClassificationBackendStatus {
    let inner = diagnostic()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ClassificationBackendStatus {
        last_error: inner.last_error.clone(),
        failure_count: inner.failure_count,
        success_count: inner.success_count,
        last_elapsed_ms: inner.last_elapsed_ms,
    }
}

fn record_rust_success(elapsed_ms: f64) {
    let mut inner = diagnostic()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    inner.record_rust_success(elapsed_ms);
}

fn record_rust_failure(error: &str, elapsed_ms: f64) {
    let mut inner = diagnostic()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inner.record_rust_failure(error, elapsed_ms);
}

fn truncate_error(error: &str) -> String {
    const MAX_CHARS: usize = 512;
    error.chars().take(MAX_CHARS).collect()
}

fn directml_preference(configured: bool, game_mode_suppressed: bool) -> bool {
    configured && !game_mode_suppressed
}

/// Read the effective provider preference at each classification chunk.
///
/// Game mode can change while a large BGE request is being drained. Reading
/// only the registry value once would let later chunks keep using a DirectML
/// semantic worker after the monitor has suppressed GPU inference.
fn current_directml_preference(app: &AppHandle) -> bool {
    let monitor = app.state::<crate::monitor::MonitorState>();
    directml_preference(
        crate::registry_config::get_bool("use_dml").unwrap_or(false),
        monitor.is_dml_suppressed(),
    )
}

fn validate_texts(texts: &[String]) -> Result<(), String> {
    if texts.is_empty() {
        return Err("invalid_request: BGE text list is empty".to_string());
    }
    if texts.len() > MAX_TEXTS_PER_REQUEST {
        return Err(format!(
            "invalid_request: BGE text list exceeds {MAX_TEXTS_PER_REQUEST} items"
        ));
    }
    let total_bytes = texts.iter().try_fold(0usize, |total, text| {
        let item_bytes = text.len();
        if item_bytes > MAX_SEMANTIC_TEXT_ITEM_BYTES {
            return Err(format!(
                "limit_exceeded: BGE text item exceeds limit: {item_bytes} > {MAX_SEMANTIC_TEXT_ITEM_BYTES} bytes"
            ));
        }
        total
            .checked_add(item_bytes)
            .ok_or_else(|| "invalid_request: BGE text length overflow".to_string())
    })?;
    if total_bytes > MAX_SEMANTIC_TEXT_BYTES {
        return Err(format!(
            "limit_exceeded: BGE text payload exceeds {MAX_SEMANTIC_TEXT_BYTES} bytes"
        ));
    }
    Ok(())
}

async fn wait_for_foreground(
    state: &Arc<SemanticRuntimeState>,
    deadline: Instant,
) -> Result<(), String> {
    let started = Instant::now();
    while state.foreground_waiting() {
        if started.elapsed() >= FOREGROUND_WAIT_BUDGET {
            return Err(
                "foreground_busy: BGE classification stood aside for a foreground query"
                    .to_string(),
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                "foreground_busy: BGE classification deadline expired while standing aside for a foreground query"
                    .to_string(),
            );
        }
        tokio::time::sleep(std::cmp::min(
            crate::semantic_runtime::FOREGROUND_POLL_INTERVAL,
            remaining,
        ))
        .await;
    }
    Ok(())
}

fn semantic_text_chunks(texts: &[String]) -> std::slice::Chunks<'_, String> {
    texts.chunks(MAX_SEMANTIC_BATCH)
}

async fn acquire_background_batch_slot<'a>(
    gate: &'a tokio::sync::Mutex<()>,
    deadline: Instant,
) -> Result<tokio::sync::MutexGuard<'a, ()>, String> {
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), gate.lock())
        .await
        .map_err(|_| {
            "background_busy: BGE classification stood aside for another background model batch"
                .to_string()
        })
}

// Keep one deadline across all protocol-sized requests. A fresh timeout per
// chunk would multiply the bridge's 120-second budget by the number of chunks.
async fn embed_bge_chunks(
    app: &AppHandle,
    state: &Arc<SemanticRuntimeState>,
    texts: &[String],
    deadline: Instant,
) -> Result<(usize, Vec<Vec<f32>>), String> {
    let mut dimensions = None;
    let mut vectors = Vec::with_capacity(texts.len());

    for (chunk_index, chunk) in semantic_text_chunks(texts).enumerate() {
        wait_for_foreground(state, deadline).await?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timeout: BGE classification deadline expired".to_string());
        }

        let prefer_directml = current_directml_preference(app);
        let result = state
            .embed_text(
                app.clone(),
                MlSemanticModel::BgeSmallZh,
                chunk.to_vec(),
                remaining,
                prefer_directml,
            )
            .await
            .map_err(|error| {
                if state.foreground_waiting() {
                    format!(
                        "foreground_busy: BGE classification yielded after a foreground query arrived: {error}"
                    )
                } else {
                    error
                }
            })?;

        if result.vectors.len() != chunk.len() {
            return Err(format!(
                "protocol: BGE response contains {} vectors for {} texts in chunk {}",
                result.vectors.len(),
                chunk.len(),
                chunk_index + 1
            ));
        }
        if result
            .vectors
            .iter()
            .any(|vector| vector.len() != result.dimensions)
        {
            return Err(format!(
                "protocol: BGE response contains a mismatched vector dimension in chunk {}",
                chunk_index + 1
            ));
        }
        if let Some(expected_dimensions) = dimensions {
            if expected_dimensions != result.dimensions {
                return Err(format!(
                    "protocol: BGE response dimension changed between chunks: expected {}, got {}",
                    expected_dimensions, result.dimensions
                ));
            }
        } else {
            dimensions = Some(result.dimensions);
        }
        vectors.extend(result.vectors);
    }

    let dimensions = dimensions.ok_or_else(|| {
        "protocol: BGE response did not contain any vector dimensions".to_string()
    })?;
    if vectors.len() != texts.len() {
        return Err(format!(
            "protocol: BGE response contains {} vectors for {} texts",
            vectors.len(),
            texts.len()
        ));
    }
    Ok((dimensions, vectors))
}

pub async fn embed_bge_texts(
    app: AppHandle,
    texts: Vec<String>,
) -> Result<BgeEmbeddingResponse, String> {
    validate_texts(&texts)?;
    let started = Instant::now();
    let deadline = started + EMBED_TIMEOUT;
    let state = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let scheduler = app
        .try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
        .map(|state| state.inner().clone());
    let external_background = state.external_background_lease();
    if let Some(scheduler) = &scheduler {
        scheduler.wake();
    }
    let result = async {
        // Do not claim the background scheduler while a foreground query is
        // already active. Re-checking inside `embed_bge_chunks` closes the race
        // between this observation, lock acquisition, and every later chunk.
        wait_for_foreground(&state, deadline).await?;
        let slot_deadline = std::cmp::min(deadline, Instant::now() + BACKGROUND_BATCH_WAIT_BUDGET);
        let _background_batch = acquire_background_batch_slot(
            &crate::semantic_runtime::BACKGROUND_PASS_GUARD,
            slot_deadline,
        )
        .await?;
        embed_bge_chunks(&app, &state, &texts, deadline).await
    }
    .await;
    drop(external_background);
    if let Some(scheduler) = &scheduler {
        scheduler.wake();
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok((dimensions, vectors)) => {
            record_rust_success(elapsed_ms);
            Ok(BgeEmbeddingResponse {
                backend: "rust",
                model_id: "bge-small-zh-v1.5",
                dimensions,
                vectors,
                elapsed_ms,
            })
        }
        Err(error) => {
            record_rust_failure(&error, elapsed_ms);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directml_preference_yields_to_game_mode_suppression() {
        assert!(!directml_preference(false, false));
        assert!(directml_preference(true, false));
        assert!(!directml_preference(true, true));
    }

    #[test]
    fn embedding_request_limits_bound_reverse_ipc_work() {
        assert!(validate_texts(&["text".to_string()]).is_ok());
        assert!(validate_texts(&[]).is_err());
        assert!(validate_texts(&vec![String::new(); MAX_TEXTS_PER_REQUEST]).is_ok());
        assert!(validate_texts(&vec![String::new(); MAX_TEXTS_PER_REQUEST + 1]).is_err());
        assert!(validate_texts(&["x".repeat(MAX_SEMANTIC_TEXT_ITEM_BYTES + 1)]).is_err());
        let multibyte_payload = vec!["中".repeat(1_000); 100];
        let error = validate_texts(&multibyte_payload).unwrap_err();
        assert!(error.contains("payload"), "{error}");
    }

    #[test]
    fn production_batches_stay_within_the_semantic_worker_limit() {
        let texts = vec!["anchor".to_string(); MAX_SEMANTIC_BATCH * 6 + 11];
        let sizes: Vec<usize> = semantic_text_chunks(&texts)
            .map(|chunk| chunk.len())
            .collect();

        let mut expected = vec![MAX_SEMANTIC_BATCH; 6];
        expected.push(11);
        assert_eq!(sizes, expected);
        assert!(sizes.iter().all(|size| *size <= MAX_SEMANTIC_BATCH));
    }

    #[test]
    fn rust_failures_are_recorded_without_a_fallback_counter() {
        let mut diagnostic = ClassificationDiagnosticInner::default();
        diagnostic.record_rust_failure("worker_stopped: test", 12.0);
        assert_eq!(diagnostic.failure_count, 1);
        assert!(diagnostic.last_error.is_some());
    }

    #[tokio::test]
    async fn classification_batch_does_not_bypass_a_busy_background_slot() {
        let gate = tokio::sync::Mutex::new(());
        let held = gate.lock().await;
        let result =
            acquire_background_batch_slot(&gate, Instant::now() + Duration::from_millis(25)).await;

        let error = match result {
            Ok(_) => panic!("classification unexpectedly acquired the occupied batch slot"),
            Err(error) => error,
        };
        assert!(error.starts_with("background_busy:"));
        drop(held);
        assert!(
            acquire_background_batch_slot(&gate, Instant::now() + Duration::from_millis(25),)
                .await
                .is_ok()
        );
    }
}
