//! M2.5 step 6 — Rust cross-encoder consumer layer.
//!
//! Provides batching, progress reporting, stall handling, and scorer identity tracking
//! for cross-encoder reranking operations.

use crate::ml_protocol::{MlProvider, MlSemanticModel};
use crate::semantic_models::descriptor;
use crate::semantic_runtime::SemanticRuntimeState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// OCR characters that go into a reranked document.
///
/// Deliberately *not* `MINILM_OCR_SNIPPET_CHARS` (200). The bi-encoder index
/// and the cross-encoder document are different contracts serving different
/// models. Persisted Smart Cluster thresholds were calibrated with the
/// 600-character cross-encoder document contract; matching 200 here would
/// change every score relative to those thresholds.
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

/// Ceiling on `n_results` for a reranked query.
pub const MAX_RERANK_RESULTS: u32 = 30;

/// Stall timeout for a single chunk of a user-facing reranked query.
///
/// Ensures the query ends if the scoring worker stops producing results,
/// without penalizing slower hardware during chunk evaluation.
pub const RERANK_CHUNK_STALL: Duration = Duration::from_secs(180);

/// Runaway guard ceiling for an unmonitored reranked query.
pub const RERANK_QUERY_CEILING: Duration = Duration::from_secs(15 * 60);

/// How a rerank call is bounded in time.
///
/// Two shapes because two situations. A background pass has nobody watching
/// and no way to be told to stop, so it must end on its own and a single total
/// deadline is exactly right. A user-facing query has both, so bounding the
/// total is the wrong instrument — it punishes a slow machine for being slow,
/// which the user can already see and act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankBudget {
    /// One deadline for the whole call, chunks included.
    Total(Duration),
    /// A budget each chunk gets on its own, under a ceiling on the whole call.
    PerChunk { chunk: Duration, ceiling: Duration },
}

impl RerankBudget {
    /// The budget a user-facing query runs under.
    pub fn interactive() -> Self {
        Self::PerChunk {
            chunk: RERANK_CHUNK_STALL,
            ceiling: RERANK_QUERY_CEILING,
        }
    }
}

/// What a [`RerankBudget`] means once a call is actually running: when the call
/// is over, and what the chunk about to be submitted may spend.
///
/// Separated from the chunk loop because the difference between the two budget
/// shapes lives entirely in this arithmetic, and inside an async function that
/// needs a worker and an `AppHandle` it can only be checked by running a rerank.
/// Here it is a pure function of the budget and the clock, so both shapes can be
/// stated as the properties they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RerankClock {
    /// When the whole call is over, whichever shape the budget has. Fixed when
    /// the call starts: a chunk finishing never moves it.
    deadline: Instant,
    /// What one chunk may spend, when a chunk is bounded separately from the
    /// call. `None` means the chunk may spend everything the call has left,
    /// which is what makes `Total` a budget for the pass rather than for a
    /// chunk of it.
    chunk: Option<Duration>,
}

impl RerankClock {
    fn start(budget: RerankBudget, now: Instant) -> Self {
        match budget {
            RerankBudget::Total(total) => Self {
                deadline: now + total,
                chunk: None,
            },
            RerankBudget::PerChunk { chunk, ceiling } => Self {
                deadline: now + ceiling,
                chunk: Some(chunk),
            },
        }
    }

    /// What the chunk about to be submitted may spend, or `None` when the call
    /// is out of budget and must stop instead of submitting.
    ///
    /// Never `Some(ZERO)`: a zero timeout travels to the worker as
    /// `timeout_ms: 0`, so handing one out would submit a chunk that cannot
    /// succeed and would be charged for the attempt.
    fn allowance(&self, now: Instant) -> Option<Duration> {
        let remaining = self.deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            return None;
        }
        // Never more than what is left overall, so the per-chunk shape stays
        // bounded by its ceiling on the last chunk rather than overrunning it.
        Some(self.chunk.map_or(remaining, |chunk| chunk.min(remaining)))
    }
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
/// `SmartClusterCreateView.jsx` reads `available` to decide whether to tell
/// the user the feature is not set up yet. It has to describe the engine that
/// will actually serve the query: answering from Python while Rust reranks
/// would warn on a screen that works whenever Python is stopped, and stay
/// silent when the file Rust loads
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
        .is_some_and(|semantic| reranker_resident(semantic.inner()));
    reranker_status_payload(
        available,
        loaded,
        &model_dir.join(descriptor.model_file).display().to_string(),
    )
}

/// Whether the cross-encoder is the model currently resident in the engine.
///
/// Answers the calibration screen's status query, which is what lets that
/// screen distinguish "the model is being read" from "the model is missing".
///
/// Not a "can this query skip the load?" test, on any path that reranks. Such a
/// path retrieves first, and the retrieval encodes the query with MiniLM, which
/// evicts the cross-encoder ([`crate::semantic_runtime::BACKGROUND_PASS_GUARD`]
/// states what that costs). Every reranked query therefore pays the 570 MB read
/// before its first chunk, which is why `semantic_query.rs` announces that load
/// unconditionally rather than asking this.
pub fn reranker_resident(semantic: &Arc<SemanticRuntimeState>) -> bool {
    let descriptor = descriptor(MlSemanticModel::BgeRerankerV2M3);
    semantic
        .status()
        .loaded_model
        .is_some_and(|loaded| loaded == descriptor.model_id)
}

fn reranker_status_payload(available: bool, loaded: bool, model_path: &str) -> serde_json::Value {
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
/// search that arrived at the start of a chunk could not be served inside its
/// deadline and would now be reported unavailable.
///
/// And the reduction is free. Per-document cost was measured flat from batch 1
/// to batch 8 at every thread count (1.248 / 1.246 / 1.259 / 1.301 s at one
/// thread), because a sequential single-threaded session has no batch
/// parallelism to exploit. Larger chunks bought latency risk and nothing else.
pub const BACKGROUND_RERANK_CHUNK: usize = 1;

/// Documents per foreground rerank request.
///
/// **Do not raise this without confirming that every pass still stands aside
/// for the foreground lease.** Eight leaves fourteen inter-chunk gaps on the
/// default request where 64 left one, and a gap is where another pass can
/// evict the cross-encoder and charge the next chunk a full reload —
/// [`crate::semantic_runtime::BACKGROUND_PASS_GUARD`] states that cost, which
/// runs to about half a chunk's worth of scoring. Fourteen of them outweigh
/// everything else on this constant. The query holds a `foreground_lease` so
/// they cannot happen, both user-initiated passes stand aside for it, and both
/// submit under [`RerankPriority::Background`] so that standing aside takes one
/// document rather than one batch. Reverting any of that puts this back to 64.
///
/// Was `MAX_RERANK_DOCUMENTS`, the largest the protocol allows, on the
/// reasoning that a user-facing query should get the fewest round trips. That
/// reasoning assumed the query had nothing to say until it was finished. Now it
/// reports after every chunk, and the chunk is what the progress bar advances
/// by, so 64 meant a bar that moved twice on the default request and a stop
/// button that took a third of the query to be noticed.
///
/// Eight is free in throughput terms: per-document cost was measured flat from
/// batch 1 to batch 8 at every thread count, because the session is sequential
/// and has no batch parallelism to exploit. What it buys is fifteen progress
/// steps on the default request instead of two, and a cancel that lands within
/// single-digit seconds.
pub const FOREGROUND_RERANK_CHUNK: usize = 8;

/// Who a rerank is being run for.
///
/// Decides both the chunk size and whether the call stands down when a
/// foreground query appears. Passed explicitly rather than inferred from the
/// caller — though as it stands only one caller passes `Foreground`, which is
/// the honest shape of it: this names what a call does to the worker, not who
/// asked for the work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RerankPriority {
    /// A user is blocked on this exact result. Never yields, because there is
    /// nothing it could yield to that matters more.
    ///
    /// Only the reranked natural-language query uses this. A forced Smart
    /// Cluster drain does not, even though a user pressed its button: the
    /// button asks for the queue to be drained, not for a search typed a minute
    /// later to wait a commit group for the worker.
    Foreground,
    /// Every pass that feeds the worker on its own schedule, idle or forced.
    /// Small chunks, and stands down between them the moment a foreground
    /// request wants the worker.
    Background,
}

impl RerankPriority {
    fn chunk_size(self) -> usize {
        match self {
            Self::Foreground => FOREGROUND_RERANK_CHUNK,
            Self::Background => BACKGROUND_RERANK_CHUNK,
        }
    }
}

/// Progress event for the reranked natural-language query, one per finished
/// chunk plus one for each phase that precedes the first.
///
/// A dropped event costs a progress line and never correctness: the command's
/// return value is the authoritative outcome, and the view reconciles against
/// it when the promise settles.
pub const NL_RERANK_PROGRESS_EVENT: &str = "nl-rerank-progress";

/// Error prefix marking a reranked query the user stopped.
///
/// Distinguished from every other error so callers can report a user-requested
/// stop as cancellation instead of a model or retrieval failure.
pub const CANCELLED_BY_USER: &str = "cancelled_by_user";

/// Whether an error from [`rerank_documents`] is the user's stop.
pub fn is_cancelled(error: &str) -> bool {
    error.starts_with(CANCELLED_BY_USER)
}

/// Stage of a reranked query, reported for progress tracking.
#[derive(Clone, Copy, Debug)]
pub enum RerankPhase {
    /// Bi-encoder query encoding and initial vector retrieval.
    Retrieving,
    /// Loading the cross-encoder model from disk into memory.
    LoadingModel,
    /// Scoring documents in chunks (`scored` out of `total`).
    Reranking,
}

impl RerankPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Retrieving => "retrieving",
            Self::LoadingModel => "loading_model",
            Self::Reranking => "reranking",
        }
    }
}

/// The running user-facing reranked query: what it has finished, and whether
/// the user has asked it to stop.
///
/// One query holds this at a time, but a query outlives the view that started
/// it: closing the calibration page unmounts the view and leaves the query
/// scoring. So a claim is a serial number rather than a flag, and only the
/// holder of the current serial reports progress or releases the state. A
/// superseded query reads as cancelled, because nobody is waiting on it.
#[derive(Default)]
pub struct RerankQueryState {
    /// Serial of the query holding this state; zero when none does.
    holder: AtomicU64,
    /// Source of claim serials. Never reset, so a serial is never reused.
    next_serial: AtomicU64,
    /// Set by `nl_rerank_stop_now`; checked between chunks. Cleared when the
    /// next query starts, so a stop arriving as one finishes cannot cancel the
    /// one after it.
    cancel_requested: AtomicBool,
    scored: AtomicU64,
    total: AtomicU64,
}

impl RerankQueryState {
    /// Ask the running query to stop after the chunk it is scoring. A chunk
    /// cannot be interrupted once submitted to the worker, so this is prompt
    /// rather than immediate.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.holder.load(Ordering::SeqCst) != 0
    }

    /// Claim the query and reset its counters. The returned guard releases the
    /// claim on every path out, including the error and cancel ones.
    pub fn begin(self: &Arc<Self>) -> ActiveRerankQuery {
        let serial = self.next_serial.fetch_add(1, Ordering::SeqCst) + 1;
        // The stop is cleared before the claim and the counters after it, so
        // the query being displaced can neither keep a stop meant for it nor
        // add a chunk to the counters of the one taking over.
        self.cancel_requested.store(false, Ordering::SeqCst);
        self.holder.store(serial, Ordering::SeqCst);
        self.scored.store(0, Ordering::SeqCst);
        self.total.store(0, Ordering::SeqCst);
        ActiveRerankQuery {
            state: self.clone(),
            serial,
        }
    }

    fn emit(&self, app: &AppHandle, phase: RerankPhase) {
        let _ = app.emit(
            NL_RERANK_PROGRESS_EVENT,
            serde_json::json!({
                "phase": phase.label(),
                "scored": self.scored.load(Ordering::SeqCst),
                "total": self.total.load(Ordering::SeqCst),
            }),
        );
    }
}

/// Holds the claim for the lifetime of one user-facing reranked query.
pub struct ActiveRerankQuery {
    state: Arc<RerankQueryState>,
    serial: u64,
}

impl ActiveRerankQuery {
    /// Whether this query still holds the state it claimed.
    fn is_current(&self) -> bool {
        self.state.holder.load(Ordering::SeqCst) == self.serial
    }

    /// Whether this query should stop: the user asked it to, or a later query
    /// took the state from it.
    fn cancelled(&self) -> bool {
        !self.is_current() || self.state.cancel_requested.load(Ordering::SeqCst)
    }

    /// Announce a phase that has no denominator of its own.
    pub fn report_phase(&self, app: &AppHandle, phase: RerankPhase) {
        if self.is_current() {
            self.state.emit(app, phase);
        }
    }

    /// Record the documents one finished chunk scored.
    fn report_chunk(&self, app: &AppHandle, scored: u64, total: u64) {
        if self.record_chunk(scored, total) {
            self.state.emit(app, RerankPhase::Reranking);
        }
    }

    /// Add one chunk to the counters, and say whether it counted — a
    /// superseded query belongs to nobody's progress bar.
    fn record_chunk(&self, scored: u64, total: u64) -> bool {
        if !self.is_current() {
            return false;
        }
        self.state.total.store(total, Ordering::SeqCst);
        self.state.scored.fetch_add(scored, Ordering::SeqCst);
        true
    }
}

impl Drop for ActiveRerankQuery {
    fn drop(&mut self) {
        let _ =
            self.state
                .holder
                .compare_exchange(self.serial, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// Error prefix marking a background rerank that stood down for a foreground
/// query rather than failing.
///
/// It travels as an error because that is the only channel `rerank_documents`
/// has, but it means the opposite of one: nothing was attempted, nothing is
/// broken, and the caller should leave its work queued rather than charge a
/// retry budget or log a fault. What the caller does next is the one thing that
/// differs between passes — an idle one ends and waits for its next tick, a
/// forced one waits for the lease to clear and claims a fresh batch.
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
/// One deadline spans every chunk for a background call, so a pass cannot
/// quietly take `chunks × timeout`. A user-facing call is bounded per chunk
/// instead; see [`RerankBudget`].
///
/// A background call additionally checks between chunks whether a foreground
/// request is waiting, and stops if one is. The check is between chunks rather
/// than inside them because the worker runs a request to completion; a small
/// `BACKGROUND_RERANK_CHUNK` is what makes "between chunks" soon enough to
/// matter.
///
/// `watcher` is the user-facing query being reported and stopped, and only the
/// reranked natural-language query ever passes one. Every Smart Cluster call is
/// `None`, forced drain included: that drain is work a user asked for, but it
/// is not the query the calibration page is watching, so advancing that page's
/// progress bar or answering its stop button would be reporting one run's
/// progress on another run's screen.
pub async fn rerank_documents(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    query: &str,
    documents: &[String],
    budget: RerankBudget,
    priority: RerankPriority,
    watcher: Option<&ActiveRerankQuery>,
) -> Result<Vec<f32>, String> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let clock = RerankClock::start(budget, Instant::now());
    let mut scores = Vec::with_capacity(documents.len());
    for chunk in documents.chunks(priority.chunk_size()) {
        if let Some(watcher) = watcher {
            if watcher.cancelled() {
                return Err(format!(
                    "{CANCELLED_BY_USER}: stopped after {} of {} documents",
                    scores.len(),
                    documents.len()
                ));
            }
        }
        if priority == RerankPriority::Background && semantic.foreground_waiting() {
            return Err(format!(
                "{YIELDED_TO_FOREGROUND}: stood down after {} of {} documents so a foreground \
                 query could reach the semantic worker",
                scores.len(),
                documents.len()
            ));
        }
        // What this chunk may spend, and `None` when the call has nothing left
        // to spend at all.
        let Some(allowance) = clock.allowance(Instant::now()) else {
            return Err(format!(
                "timeout: reranking ran out of budget after {} of {} documents",
                scores.len(),
                documents.len()
            ));
        };
        let result = semantic
            .rerank(
                app.clone(),
                query.to_string(),
                chunk.to_vec(),
                allowance,
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
        if let Some(watcher) = watcher {
            watcher.report_chunk(app, chunk.len() as u64, documents.len() as u64);
        }
    }
    Ok(scores)
}

/// Ask the running reranked natural-language query to stop.
///
/// Returns whether there was one to stop, so a view that pressed the button as
/// the query was settling on its own does not report a cancellation that never
/// happened.
///
/// Not session-guarded, for the reason `semantic_index_stop_now` records:
/// this halts work and touches no user data, and requiring an unlock to *stop*
/// something would be exactly backwards on a machine whose session locked while
/// the query was running.
///
/// The query checks between chunks, so a stop takes effect within one chunk of
/// `FOREGROUND_RERANK_CHUNK` documents. Nothing is written on either side of
/// this, so a stopped query costs only the CPU it had already spent.
#[tauri::command]
pub async fn nl_rerank_stop_now(window: tauri::Window, app: AppHandle) -> Result<bool, String> {
    crate::commands::check_main_window(&window)?;
    let query = app.state::<Arc<RerankQueryState>>().inner().clone();
    if !query.is_running() {
        return Ok(false);
    }
    query.request_cancel();
    tracing::info!("[SEMANTIC] reranked query asked to stop");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-document rerank cost on the slowest configuration measured on
    /// 2026-08-01: 1.18 s, two cores at one intra-op thread. Rounded up, so a
    /// bound that clears this clears the measurement with room to spare.
    const SLOWEST_DOCUMENT: Duration = Duration::from_millis(1250);

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
        assert!(calibration_documents > crate::ml_protocol::MAX_RERANK_DOCUMENTS);
        // And the foreground chunk stays inside that cap, which is what the
        // protocol validator rejects a request for exceeding.
        assert!(FOREGROUND_RERANK_CHUNK <= crate::ml_protocol::MAX_RERANK_DOCUMENTS);
    }

    #[test]
    fn the_result_cap_bounds_what_one_query_can_cost() {
        // The cap is on results, but what it is really bounding is documents,
        // because that is what the cross-encoder is paid per. The retired 120
        // option asked for 480 of them, which exceeded the old whole-query
        // budget even on the machine the per-document cost was measured on.
        assert_eq!(MAX_RERANK_RESULTS, 30);
        let documents = MAX_RERANK_RESULTS as usize * RERANK_OVERFETCH as usize;
        assert_eq!(documents, 120);
        // Slowest per-document cost measured, rounded up.
        let worst_case = SLOWEST_DOCUMENT * documents as u32;
        assert!(worst_case < RERANK_QUERY_CEILING);
        // And the ceiling is a runaway guard, not a budget the slowest machine
        // is expected to brush against.
        assert!(worst_case * 4 < RERANK_QUERY_CEILING);
    }

    #[test]
    fn a_foreground_chunk_stays_well_inside_its_stall_budget() {
        // Reaching the stall budget has to mean the worker stopped producing,
        // not that the machine is slow, or the budget is measuring the wrong
        // thing again. One chunk at the slowest per-document cost measured:
        let slowest_chunk = SLOWEST_DOCUMENT * FOREGROUND_RERANK_CHUNK as u32;
        assert!(slowest_chunk * 10 < RERANK_CHUNK_STALL);
        // The first chunk also absorbs a cold 570 MB model load, which is the
        // reason the budget is minutes rather than seconds.
        assert!(RERANK_CHUNK_STALL >= Duration::from_secs(120));
    }

    #[test]
    fn a_background_call_keeps_one_deadline_for_the_whole_pass() {
        // Nobody is watching a background pass and nothing can tell it to stop,
        // so a total deadline is the only thing that ends a runaway. What that
        // requires of the arithmetic is that finishing a chunk buys nothing:
        // every chunk's allowance runs out at the *same instant*, whichever
        // chunk it is and however long the ones before it took. That instant is
        // the deadline, and `chunks × budget` is therefore not reachable.
        const TOTAL: Duration = Duration::from_secs(300);
        let start = Instant::now();
        let clock = RerankClock::start(RerankBudget::Total(TOTAL), start);
        for elapsed in [0, 1, 100, 299] {
            let at = start + Duration::from_secs(elapsed);
            let allowance = clock.allowance(at).expect("budget left");
            assert_eq!(
                (at + allowance).duration_since(start),
                TOTAL,
                "a chunk starting {elapsed}s in was given an allowance that outlives the pass"
            );
        }
        // And the pass stops rather than submitting a chunk it cannot pay for.
        assert_eq!(clock.allowance(start + TOTAL), None);
        assert_eq!(
            clock.allowance(start + TOTAL + Duration::from_secs(60)),
            None
        );
    }

    #[test]
    fn a_user_facing_call_renews_its_allowance_for_every_chunk() {
        // The opposite property, and the reason the shape exists: a chunk gets
        // its own budget measured from when *it* started, so a machine that
        // takes fifteen minutes over a query it is visibly making progress on
        // is not cut off for being slow. This is what the retired 120 s
        // whole-query timeout got wrong.
        let start = Instant::now();
        let clock = RerankClock::start(RerankBudget::interactive(), start);
        for elapsed in [0, 180, 600] {
            let at = start + Duration::from_secs(elapsed);
            assert_eq!(
                clock.allowance(at),
                Some(RERANK_CHUNK_STALL),
                "the chunk starting {elapsed}s in was not given a full stall budget"
            );
        }
        // Until the ceiling is close enough that a whole chunk would overrun
        // it. The runaway guard wins there, or it would not be a guard.
        let near_ceiling = start + RERANK_QUERY_CEILING - Duration::from_secs(30);
        assert_eq!(clock.allowance(near_ceiling), Some(Duration::from_secs(30)));
        assert_eq!(clock.allowance(start + RERANK_QUERY_CEILING), None);
    }

    #[test]
    fn the_two_budget_shapes_answer_differently_for_the_same_chunk() {
        // Pinned as a contrast, using the same number for both shapes so that
        // only the shape differs. The first chunk cannot tell them apart, which
        // is why a test that stops at the first chunk — or at the enum's
        // shape — proves nothing about either branch.
        let start = Instant::now();
        let total = RerankClock::start(RerankBudget::Total(RERANK_CHUNK_STALL), start);
        let per_chunk = RerankClock::start(RerankBudget::interactive(), start);
        assert_eq!(total.allowance(start), per_chunk.allowance(start));

        // A later chunk can: one is spending a budget down, the other is
        // renewing it.
        let late = start + Duration::from_secs(100);
        assert_eq!(total.allowance(late), Some(Duration::from_secs(80)));
        assert_eq!(per_chunk.allowance(late), Some(RERANK_CHUNK_STALL));

        // And at the number both were given, one pass is over while the other
        // is twelve minutes from its ceiling.
        let spent = start + RERANK_CHUNK_STALL;
        assert_eq!(total.allowance(spent), None);
        assert_eq!(per_chunk.allowance(spent), Some(RERANK_CHUNK_STALL));
    }

    #[test]
    fn a_user_facing_query_runs_under_the_two_budgets_written_for_it() {
        // The constants each carry an argument in their doc comments about what
        // they are bounding; `interactive` is where those arguments are
        // actually applied, and a transposition here would silently bound the
        // query at three minutes total.
        assert_eq!(
            RerankBudget::interactive(),
            RerankBudget::PerChunk {
                chunk: RERANK_CHUNK_STALL,
                ceiling: RERANK_QUERY_CEILING,
            }
        );
        assert!(RERANK_CHUNK_STALL < RERANK_QUERY_CEILING);
    }

    #[test]
    fn a_stop_is_not_an_error_that_falls_back_to_python() {
        // Both travel as errors because that is the only channel the chunk loop
        // has, and both mean something other than "this failed" — so both are
        // recognized by prefix rather than by string equality, since each
        // carries its own progress detail.
        let stopped = format!("{CANCELLED_BY_USER}: stopped after 24 of 120 documents");
        assert!(is_cancelled(&stopped));
        assert!(!is_yield(&stopped));
        assert!(!is_cancelled("timeout: reranking ran out of budget"));
        assert!(!is_cancelled("model_mismatch: reranker returned 3 scores"));
    }

    #[test]
    fn a_query_state_reports_its_stop_only_while_it_is_running() {
        let state = Arc::new(RerankQueryState::default());
        assert!(!state.is_running());
        state.request_cancel();
        {
            let active = state.begin();
            // `begin` clears a stop left over from the previous query, so a
            // click that landed as the last one settled cannot cancel this one.
            assert!(!active.cancelled());
            assert!(state.is_running());
            state.request_cancel();
            assert!(active.cancelled());
        }
        // The guard releases the claim however the query ended, including by
        // the cancel that was just requested.
        assert!(!state.is_running());
    }

    #[test]
    fn a_superseded_query_neither_counts_nor_releases() {
        // The calibration view was closed mid-query and reopened: the first
        // query is still scoring when the second claims the state.
        let state = Arc::new(RerankQueryState::default());
        let first = state.begin();
        assert!(first.record_chunk(8, 120));

        let second = state.begin();
        assert_eq!(state.scored.load(Ordering::SeqCst), 0);

        assert!(first.cancelled());
        assert!(!second.cancelled());
        assert!(!first.record_chunk(8, 120));
        assert_eq!(state.scored.load(Ordering::SeqCst), 0);

        // The abandoned query ending must not answer for the running one, or
        // the stop button reports that there is nothing left to stop.
        drop(first);
        assert!(state.is_running());
        drop(second);
        assert!(!state.is_running());
    }

    #[test]
    fn a_stop_outlives_the_query_that_supersedes_it() {
        // The user pressed stop, then closed the page and started another
        // query before the chunk boundary that would have honoured it. `begin`
        // clears the flag, so being superseded is what carries the stop.
        let state = Arc::new(RerankQueryState::default());
        let first = state.begin();
        state.request_cancel();
        let _second = state.begin();
        assert!(first.cancelled());
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
        // `SmartClusterCreateView.jsx` tells the user the feature is not ready
        // when `available === false`. A missing key would silently become
        // "unavailable".
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
        assert!(BACKGROUND_RERANK_CHUNK < crate::ml_protocol::MAX_RERANK_DOCUMENTS);
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

        // A user-facing rerank no longer takes the largest chunk the protocol
        // allows. It reports after every chunk and can be stopped between them,
        // so the chunk is the resolution of both the progress bar and the stop
        // button — and 64 gave the default request a bar that moved twice.
        assert_eq!(
            RerankPriority::Foreground.chunk_size(),
            FOREGROUND_RERANK_CHUNK
        );
        let steps = (MAX_RERANK_RESULTS as usize * RERANK_OVERFETCH as usize)
            .div_ceil(FOREGROUND_RERANK_CHUNK);
        assert!(
            steps >= 10,
            "a progress bar that advances {steps} times over a whole query is a spinner"
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
