//! M2.5 step 6 — the Rust cross-encoder consumer layer.
//!
//! `semantic_engine::rerank` and the `Rerank` protocol operation have existed
//! since M2.2; what was missing was anything that called them. This module is
//! that layer: the document contract shared by both consumers, the batching
//! that a 64-document protocol cap forces on a path that over-fetches 120, the
//! rollback switch, and the scorer identity that a stored threshold is
//! measured against.
//!
//! **Why calibration and the scoring worker move together.** A Smart Cluster's
//! threshold is derived from calibration rerank scores and then written to
//! `smart_clusters.threshold`, where the background worker compares its own
//! logits against it. Two different scorers on those two sides means
//! assignments that quietly over- or under-fire against a number the user
//! never sees. And the two sides genuinely disagree here: Python's reranker
//! prefers DirectML (`reranker.py`, provider list), while the Rust engine
//! refuses DirectML for this model outright after the 2026-07-20 parity audit.
//! Same ONNX file, different provider, different logits.
//!
//! **Provenance rather than a flag day.** Every threshold already on disk was
//! produced by the Python DirectML scorer. Rather than either trusting it or
//! demanding that the user recalibrate, each threshold records the scorer that
//! produced it, and a threshold whose scorer no longer exists is re-derived
//! from the calibration examples that are already stored alongside it. See
//! `smart_cluster_scoring.rs`.

use crate::registry_config;
use crate::ml_protocol::{MlProvider, MlSemanticModel, MAX_RERANK_DOCUMENTS};
use crate::semantic_models::descriptor;
use crate::semantic_runtime::SemanticRuntimeState;
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// OCR characters that go into a reranked document.
///
/// Deliberately *not* `MINILM_OCR_SNIPPET_CHARS` (200). The bi-encoder index
/// and the cross-encoder document are different contracts serving different
/// models: Python's retrieval path passes `ocr_snippet_chars = 600`
/// (`task_clustering.py::query_by_text`) and its worker uses the same 600
/// (`smart_cluster_worker.py::OCR_SNIPPET_CHARS`). Matching 200 here would
/// change every score relative to the thresholds already on disk.
pub const RERANK_OCR_SNIPPET_CHARS: usize = 600;

/// The reranker variant this build scores with.
///
/// Pinned rather than selectable. `semantic_models.rs` resolves exactly
/// `onnx/model_uint8.onnx`, and `model_management.rs` installs exactly that
/// file, so the multi-variant dropdown Python grew was already offering
/// choices that could not load. The value is still reported in responses and
/// recorded in threshold provenance, because "which variant produced this
/// number" stays a real question even when there is only one answer.
pub const RERANK_VARIANT: &str = "uint8";

/// Documents the calibration path pulls from the bi-encoder per requested
/// result. Matches Python's `rerank_overfetch` default.
pub const RERANK_OVERFETCH: u32 = 4;

/// Deadline for one user-facing reranked query, covering retrieval, every
/// rerank chunk, and a cold 570 MB model load.
///
/// Far longer than the 5 s the non-reranked path allows, and deliberately so:
/// this path is only ever reached from Smart Cluster calibration, where the
/// user has asked for a preview and is watching a progress line, and where
/// there is no second backend to fall back to once the cutover lands.
pub const RERANK_QUERY_TIMEOUT: Duration = Duration::from_secs(120);

const RERANK_RUNTIMES: &[&str] = &["python", "rust"];
const DEFAULT_RERANK_RUNTIME: &str = "rust";

/// Which reranker serves Smart Cluster calibration and background scoring.
///
/// The flag rule's one-release rollback: `python` returns both to
/// `monitor/reranker.py`, together, for the same reason they cut over
/// together. Unset or unrecognized resolves to the shipped default.
pub fn rerank_runtime() -> String {
    match registry_config::get_string("rerank_runtime") {
        Some(value) if RERANK_RUNTIMES.contains(&value.as_str()) => value,
        _ => DEFAULT_RERANK_RUNTIME.to_string(),
    }
}

pub fn rust_rerank_selected() -> bool {
    rerank_runtime() == "rust"
}

/// Who is draining `smart_cluster_pending` right now.
///
/// Not the same question as which value `rerank_runtime` holds, because the two
/// sides read that value at different times: Rust reads the registry on every
/// pass, Python reads its environment once at startup. The switch alone
/// therefore describes an intention, and this describes the arrangement that is
/// actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartClusterQueueOwner {
    /// The Rust worker in this process.
    Rust,
    /// The Python worker in the monitor process that is running right now.
    Python,
    /// Nobody. The switch hands the queue to Python, but the monitor running
    /// right now — if one is running at all — was started before that and left
    /// its worker unstarted. The queue simply waits, which is the honest
    /// outcome: the user asked for the Python scorer and the Rust one taking
    /// the work anyway would be the rollback lever doing nothing.
    Neither,
}

/// The ownership rule, with both inputs supplied so it can be read and tested
/// as the truth table it is.
///
/// A live Python worker wins over the switch, in both directions. Setting the
/// key back to `rust` while that worker is running does not start a second
/// drainer; it takes effect when the monitor next starts.
pub fn queue_owner_from(rust_selected: bool, python_worker_live: bool) -> SmartClusterQueueOwner {
    if python_worker_live {
        SmartClusterQueueOwner::Python
    } else if rust_selected {
        SmartClusterQueueOwner::Rust
    } else {
        SmartClusterQueueOwner::Neither
    }
}

/// The current owner. `None` for the monitor state means no monitor is managed
/// in this process, which is the case in tests.
pub fn smart_cluster_queue_owner(
    monitor: Option<&crate::monitor::MonitorState>,
) -> SmartClusterQueueOwner {
    queue_owner_from(
        rust_rerank_selected(),
        monitor.is_some_and(|state| state.python_owns_smart_cluster_queue()),
    )
}

/// Whether the Rust side may act on the queue: drain it, and answer the status,
/// force-run, and cancel commands for it.
pub fn rust_owns_smart_cluster_queue(monitor: Option<&crate::monitor::MonitorState>) -> bool {
    smart_cluster_queue_owner(monitor) == SmartClusterQueueOwner::Rust
}

/// The scorer that produced a number, recorded next to that number.
///
/// Four fields because four things can change the logits independently: the
/// model, its pinned revision, the quantization variant, and the execution
/// provider. The 2026-07-20 audit is the reason `provider` is in here rather
/// than assumed constant — it found top-1 changing on 20.5% of queries between
/// CPU and DirectML on this very model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScorerIdentity {
    pub model_id: String,
    pub model_revision: String,
    pub variant: String,
    pub provider: String,
}

impl ScorerIdentity {
    /// The scorer this build uses. CPU is not a guess: `provider_supports_model`
    /// refuses DirectML for `bge-reranker-v2-m3`, so every Rust rerank runs on
    /// CPU regardless of what the caller prefers.
    pub fn current() -> Self {
        let descriptor = descriptor(MlSemanticModel::BgeRerankerV2M3);
        Self {
            model_id: descriptor.model_id.to_string(),
            model_revision: descriptor.revision.to_string(),
            variant: RERANK_VARIANT.to_string(),
            provider: provider_label(MlProvider::Cpu).to_string(),
        }
    }

    /// Whether a stored threshold was produced by this build's scorer. A
    /// threshold that fails this is not wrong, it is *unverifiable*, and the
    /// caller has to decide what to do about it rather than compare against it.
    pub fn matches_stored(
        &self,
        model_id: Option<&str>,
        model_revision: Option<&str>,
        variant: Option<&str>,
        provider: Option<&str>,
    ) -> bool {
        model_id == Some(self.model_id.as_str())
            && model_revision == Some(self.model_revision.as_str())
            && variant == Some(self.variant.as_str())
            && provider == Some(self.provider.as_str())
    }

    /// The four fields as one comparable string.
    ///
    /// Used where a scorer has to be recorded as a plain value rather than as
    /// four columns — the "re-deriving this cluster's threshold is impossible"
    /// verdict, which is a fact about a scorer but not provenance of a number.
    /// The separator cannot occur in a model id, revision, variant, or provider
    /// label, all of which come from `semantic_models.rs` constants.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.model_id, self.model_revision, self.variant, self.provider
        )
    }
}

fn provider_label(provider: MlProvider) -> &'static str {
    match provider {
        MlProvider::Cpu => "cpu",
        MlProvider::DirectMl => "directml",
    }
}

/// The reranker availability payload, in the shape Python's
/// `nl_cluster_reranker_status` returned.
///
/// `NlClusterView` reads `available` to decide whether to warn that a
/// calibration query cannot be reranked, and `loaded_variant` to label the
/// scores. Both have to describe the engine that will actually serve the
/// query: answering from Python while Rust reranks would warn on a screen that
/// works whenever Python is stopped, and stay silent when the file Rust loads
/// is missing.
///
/// `available_variants` carries exactly one entry rather than the list Python
/// enumerated from disk. Only `model_uint8.onnx` is ever installed and the
/// engine pins it, so the longer list was offering choices that could not load.
pub fn reranker_status_value(app: &AppHandle) -> serde_json::Value {
    let descriptor = descriptor(MlSemanticModel::BgeRerankerV2M3);
    let (model_dir, available) = crate::model_management::reranker_install_status();
    let loaded = app
        .try_state::<Arc<SemanticRuntimeState>>()
        .map(|semantic| semantic.status())
        .and_then(|status| status.loaded_model)
        .is_some_and(|loaded| loaded == descriptor.model_id);
    reranker_status_payload(
        available,
        loaded,
        &model_dir.join(descriptor.model_file).display().to_string(),
    )
}

fn reranker_status_payload(
    available: bool,
    loaded: bool,
    model_path: &str,
) -> serde_json::Value {
    serde_json::json!({
        "status": "success",
        "available": available,
        "loaded": loaded,
        // Null until something has actually loaded it, matching Python, where
        // `loaded_variant` was unset until the first rerank.
        "loaded_variant": if loaded {
            serde_json::Value::String(RERANK_VARIANT.to_string())
        } else {
            serde_json::Value::Null
        },
        "provider": provider_label(MlProvider::Cpu),
        "available_variants": if available { vec![RERANK_VARIANT] } else { Vec::new() },
        "model_path": model_path,
        "backend": "rust",
    })
}

/// Build the cross-encoder document for one screenshot.
///
/// Field-for-field what Python assembles on both of its rerank paths: the
/// non-empty parts of `process | title | OCR`, joined by `" | "`, with
/// `"(empty)"` when nothing survives. The empty-string fallback matters —
/// the tokenizer would otherwise see a bare query paired with nothing, and the
/// logit for that pair is not comparable to the rest of the batch.
pub fn build_rerank_document(process: &str, title: &str, ocr_text: &str) -> String {
    let snippet: String = ocr_text.chars().take(RERANK_OCR_SNIPPET_CHARS).collect();
    let parts: Vec<&str> = [process, title, snippet.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return "(empty)".to_string();
    }
    parts.join(" | ")
}

/// Documents per background rerank request.
///
/// Not a throughput choice — a latency one. The protocol allows 64
/// (`MAX_RERANK_DOCUMENTS`), and a background pass submitting 64 documents to a
/// CPU-only 568-million-parameter cross-encoder holds the single request slot
/// (`semantic_runtime.rs::acquire_request_slot`) for the whole of it, while a
/// foreground NL query has 5 s to acquire that same slot. Since a background
/// request in flight cannot be interrupted, the only way to bound what a
/// foreground caller waits for is to bound how long one background request can
/// last, and this constant is that bound.
///
/// Chunking cannot move a score: a cross-encoder evaluates each
/// `(query, document)` pair independently, which is what makes the batching
/// below legitimate in the first place.
///
/// **One, because the measurement this constant was waiting on came back and
/// said the opposite of what the previous value assumed.** Measured on the
/// shipped session configuration — one intra-op thread, sequential execution —
/// a realistic 325-token document costs 1.18 s. The previous value of 4
/// therefore held the slot for 4.72 s, against a foreground budget of 5.0 s
/// (`semantic_query.rs::QUERY_EMBED_TIMEOUT`) that also has to cover swapping
/// the 570 MB cross-encoder out for MiniLM (0.50 s). The margin was −0.22 s: a
/// search that arrived at the start of a chunk could not be served at all, and
/// fell back to Python — a fallback the Python removal is in the middle of
/// taking away.
///
/// And the reduction is free. Per-document cost was measured flat from batch 1
/// to batch 8 at every thread count (1.248 / 1.246 / 1.259 / 1.301 s at one
/// thread), because a sequential single-threaded session has no batch
/// parallelism to exploit. Larger chunks bought latency risk and nothing else.
pub const BACKGROUND_RERANK_CHUNK: usize = 1;

/// Who a rerank is being run for.
///
/// Decides both the chunk size and whether the call stands down when a
/// foreground query appears. Passed explicitly rather than inferred from the
/// caller, because the Smart Cluster worker is background on an idle pass and
/// foreground on the "run now" the user just clicked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RerankPriority {
    /// A user is waiting on this. Takes the largest chunk the protocol allows
    /// and never yields — including a forced Smart Cluster drain, which is a
    /// button the user pressed.
    Foreground,
    /// Idle-gated background scoring. Small chunks, and stands down between
    /// them the moment a foreground request wants the worker.
    Background,
}

impl RerankPriority {
    fn chunk_size(self) -> usize {
        match self {
            Self::Foreground => MAX_RERANK_DOCUMENTS,
            Self::Background => BACKGROUND_RERANK_CHUNK,
        }
    }
}

/// Error prefix marking a background rerank that stood down for a foreground
/// query rather than failing.
///
/// It travels as an error because that is the only channel `rerank_documents`
/// has, but it means the opposite of one: nothing was attempted, nothing is
/// broken, and the caller should leave its work queued for a later pass instead
/// of charging a retry budget or logging a fault.
pub const YIELDED_TO_FOREGROUND: &str = "yielded_to_foreground";

/// Whether an error from [`rerank_documents`] is the stand-down marker.
pub fn is_yield(error: &str) -> bool {
    error.starts_with(YIELDED_TO_FOREGROUND)
}

/// Score every document against one query, in protocol-sized chunks.
///
/// The batching is not an optimization, it is a correctness requirement: the
/// calibration path over-fetches `n_results * RERANK_OVERFETCH` — 120 documents
/// at the defaults — against a `MAX_RERANK_DOCUMENTS` cap of 64, so a single
/// request would be rejected outright by the protocol validator before a byte
/// reached the worker.
///
/// Chunking is safe for this model because a cross-encoder scores each
/// `(query, document)` pair independently; the raw logits are comparable across
/// calls in a way that, say, a softmax over the batch would not be. Python
/// already relies on this — `reranker.py` runs its own `batch_size = 8` loop.
///
/// One deadline spans every chunk, so a query cannot quietly take
/// `chunks × timeout`.
///
/// A background call additionally checks between chunks whether a foreground
/// request is waiting, and stops if one is. The check is between chunks rather
/// than inside them because the worker runs a request to completion; a small
/// `BACKGROUND_RERANK_CHUNK` is what makes "between chunks" soon enough to
/// matter.
pub async fn rerank_documents(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    query: &str,
    documents: &[String],
    timeout: Duration,
    priority: RerankPriority,
) -> Result<Vec<f32>, String> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + timeout;
    let mut scores = Vec::with_capacity(documents.len());
    for chunk in documents.chunks(priority.chunk_size()) {
        if priority == RerankPriority::Background && semantic.foreground_waiting() {
            return Err(format!(
                "{YIELDED_TO_FOREGROUND}: stood down after {} of {} documents so a foreground \
                 query could reach the semantic worker",
                scores.len(),
                documents.len()
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timeout: reranking ran out of budget after {} of {} documents",
                scores.len(),
                documents.len()
            ));
        }
        let result = semantic
            .rerank(
                app.clone(),
                query.to_string(),
                chunk.to_vec(),
                remaining,
                // DirectML parity is not approved for this model; asking for it
                // only costs a provider negotiation before the fallback to CPU.
                false,
            )
            .await?;
        if result.scores.len() != chunk.len() {
            return Err(format!(
                "model_mismatch: reranker returned {} scores for {} documents",
                result.scores.len(),
                chunk.len()
            ));
        }
        scores.extend(result.scores);
    }
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_matches_the_python_join_contract() {
        assert_eq!(
            build_rerank_document("code.exe", "main.rs", "fn main"),
            "code.exe | main.rs | fn main"
        );
        // Only the non-empty parts are joined, so a missing title does not
        // leave a " |  | " gap that changes the tokenization.
        assert_eq!(
            build_rerank_document("code.exe", "", "fn main"),
            "code.exe | fn main"
        );
        assert_eq!(build_rerank_document("", "", ""), "(empty)");
    }

    #[test]
    fn the_ocr_snippet_is_the_reranker_length_not_the_index_length() {
        // The bi-encoder index truncates at 200 characters and the cross-encoder
        // document at 600. Collapsing them would silently move every score away
        // from the thresholds already stored against the 600-character contract.
        assert_eq!(RERANK_OCR_SNIPPET_CHARS, 600);
        assert_ne!(
            RERANK_OCR_SNIPPET_CHARS,
            crate::minilm_migration::MINILM_OCR_SNIPPET_CHARS
        );

        let ocr: String = std::iter::repeat_n('字', 900).collect();
        let document = build_rerank_document("p", "t", &ocr);
        // Counted in characters, as Python's `[:600]` slice is — a byte-wise
        // truncation would cut a multi-byte character in half and drift.
        assert_eq!(document.chars().count(), "p | t | ".chars().count() + 600);
    }

    #[test]
    fn the_calibration_over_fetch_exceeds_one_protocol_batch() {
        // The reason chunking exists rather than being an optimization: the
        // default calibration request is 30 * 4 documents against a cap of 64.
        let calibration_documents = 30 * RERANK_OVERFETCH as usize;
        assert!(calibration_documents > MAX_RERANK_DOCUMENTS);
    }

    #[test]
    fn the_runtime_switch_keeps_its_rollback_value() {
        assert!(RERANK_RUNTIMES.contains(&"python"));
        assert!(RERANK_RUNTIMES.contains(&DEFAULT_RERANK_RUNTIME));
        assert_eq!(DEFAULT_RERANK_RUNTIME, "rust");
    }

    #[test]
    fn a_live_python_worker_keeps_the_queue_until_the_monitor_restarts() {
        // The switch is read live on this side and once at startup on Python's,
        // so flipping it back to `rust` under a monitor that was started with
        // `python` must not wake a second drainer. The spawned value wins.
        assert_eq!(
            queue_owner_from(true, true),
            SmartClusterQueueOwner::Python
        );
        assert_eq!(
            queue_owner_from(false, true),
            SmartClusterQueueOwner::Python
        );
    }

    #[test]
    fn the_rust_worker_drains_only_when_no_python_worker_is_running() {
        assert_eq!(queue_owner_from(true, false), SmartClusterQueueOwner::Rust);
    }

    #[test]
    fn a_rollback_that_has_not_taken_effect_yet_leaves_the_queue_unowned() {
        // `rerank_runtime = python` under a monitor started with `rust`: that
        // monitor left its worker unstarted, and this one stands down because
        // the user asked for the Python scorer. Nothing drains until the
        // monitor restarts, and the worker loop says so in the log rather than
        // quietly scoring with the backend the user just rolled away from.
        assert_eq!(
            queue_owner_from(false, false),
            SmartClusterQueueOwner::Neither
        );
    }

    #[test]
    fn a_stored_threshold_from_another_scorer_is_not_accepted() {
        let current = ScorerIdentity::current();
        assert!(current.matches_stored(
            Some(&current.model_id),
            Some(&current.model_revision),
            Some(&current.variant),
            Some(&current.provider),
        ));
        // The Python scorer that produced every threshold now on disk: same
        // model, same variant, DirectML instead of CPU.
        assert!(!current.matches_stored(
            Some(&current.model_id),
            Some(&current.model_revision),
            Some(&current.variant),
            Some("directml"),
        ));
        // And a threshold written before provenance existed records nothing.
        assert!(!current.matches_stored(None, None, None, None));
    }

    #[test]
    fn the_current_scorer_is_cpu_because_the_engine_refuses_anything_else() {
        assert_eq!(ScorerIdentity::current().provider, "cpu");
        assert_eq!(ScorerIdentity::current().variant, RERANK_VARIANT);
        assert_eq!(ScorerIdentity::current().model_id, "bge-reranker-v2-m3");
    }

    #[test]
    fn the_reranker_status_keeps_the_fields_the_calibration_screen_reads() {
        // `task_api.js::getRerankerStatus` normalizes exactly these keys, and
        // `NlClusterView` warns on `available === false` and prints
        // `loaded_variant`. A missing key would silently become "unavailable".
        let installed = reranker_status_payload(true, true, "C:/models/model_uint8.onnx");
        assert_eq!(installed["status"], "success");
        assert_eq!(installed["available"], true);
        assert_eq!(installed["loaded"], true);
        assert_eq!(installed["loaded_variant"], RERANK_VARIANT);
        assert_eq!(installed["provider"], "cpu");
        assert_eq!(installed["available_variants"][0], RERANK_VARIANT);
        assert_eq!(installed["model_path"], "C:/models/model_uint8.onnx");
    }

    #[test]
    fn an_uninstalled_reranker_offers_no_variant_to_load() {
        // The old Python payload enumerated whatever ONNX files were on disk.
        // Here the single pinned variant is offered only when it is actually
        // installed, so the UI cannot present a choice that would fail to load.
        let missing = reranker_status_payload(false, false, "");
        assert_eq!(missing["available"], false);
        assert!(missing["loaded_variant"].is_null());
        assert_eq!(missing["available_variants"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_background_chunk_fits_inside_the_foreground_query_budget() {
        // The bound on how long a foreground query can wait. A background
        // request cannot be interrupted once submitted, so the chunk size *is*
        // the worst-case wait, and it has to fit inside the 5 s budget
        // (`semantic_query.rs::QUERY_EMBED_TIMEOUT`) together with the model
        // swap that query still has to pay for.
        assert!(BACKGROUND_RERANK_CHUNK < MAX_RERANK_DOCUMENTS);
        assert!(BACKGROUND_RERANK_CHUNK >= 1);
        assert_eq!(
            RerankPriority::Background.chunk_size(),
            BACKGROUND_RERANK_CHUNK
        );

        // The arithmetic that picked the value, kept executable so raising the
        // chunk has to argue with the measurement rather than around it. Both
        // figures are measured on the shipped session configuration: 1.18 s for
        // a realistic 325-token document, 0.50 s to load MiniLM after the
        // cross-encoder is evicted. A chunk of 4 lands at 5.22 s and does not
        // fit, which is what it was before this was measured.
        const DOC_MS: u64 = 1_180;
        const MINILM_SWAP_MS: u64 = 500;
        const QUERY_BUDGET_MS: u64 = 5_000;
        let worst_wait_ms = BACKGROUND_RERANK_CHUNK as u64 * DOC_MS + MINILM_SWAP_MS;
        assert!(
            worst_wait_ms < QUERY_BUDGET_MS,
            "a background chunk of {BACKGROUND_RERANK_CHUNK} leaves a foreground query \
             {worst_wait_ms} ms of work inside a {QUERY_BUDGET_MS} ms budget"
        );

        // A user-facing rerank keeps the largest chunk the protocol allows:
        // nothing is waiting behind it that it should make room for.
        assert_eq!(
            RerankPriority::Foreground.chunk_size(),
            MAX_RERANK_DOCUMENTS
        );
    }

    #[test]
    fn standing_down_is_distinguishable_from_every_other_error() {
        // The callers branch on this to decide between "leave the work queued
        // and say nothing" and "log a fault". Confusing the two either fills
        // the log with warnings every time somebody searches, or silently
        // swallows a broken worker.
        let yielded = format!("{YIELDED_TO_FOREGROUND}: stood down after 4 of 32 documents");
        assert!(is_yield(&yielded));
        assert!(!is_yield("timeout: reranking ran out of budget"));
        assert!(!is_yield("model_mismatch: reranker returned 3 scores"));
        assert!(!is_yield(""));
    }
}
