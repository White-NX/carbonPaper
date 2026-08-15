//! M2.5 step 6 — the Rust Smart Cluster scoring worker.
//!
//! Replaces `monitor/smart_cluster_worker.py` as the default drainer of the
//! `smart_cluster_pending` queue. The pipeline is the same three stages Python
//! ran — MiniLM cosine prefilter against each cluster's anchor, cross-encoder
//! rerank of whatever survives, assignment above the cluster's threshold — with
//! the same constants, because the thresholds those constants produced are
//! already on disk.
//!
//! **Why this moved with calibration rather than after it.** The threshold in
//! `smart_clusters.threshold` is produced by the calibration query and consumed
//! here. Cutting one over without the other would leave a number derived from
//! Python's DirectML logits being applied to Rust's CPU logits — the 2026-07-20
//! audit measured top-1 changing on 20.5% of queries between those two
//! providers on this exact model. The result would be assignments that quietly
//! over- or under-fire against a number the user never sees.
//!
//! **What happens to the thresholds already on disk.** They were all produced
//! by the retired scorer, and neither trusting them nor demanding that every
//! user recalibrate is acceptable. Instead each threshold records its scorer
//! (`storage/schema.rs`, the `threshold_*` columns), and a cluster whose
//! recorded scorer is absent or different has its threshold **re-derived** from
//! the calibration examples stored next to it: the same positives and negatives
//! the user picked, re-scored with the current scorer, through the same formula
//! the calibration UI applies. Nothing is invented and the user is not asked to
//! redo work they already did. A cluster whose examples can no longer support a
//! threshold — every positive deleted, say — is given up on rather than scored
//! against a number nobody can vouch for: the verdict is written to the row so
//! the 570 MB cross-encoder is not loaded once a minute to re-reach it, the UI
//! says how many clusters are in that state, and re-saving the examples clears
//! it. Only a *transient* failure, such as a rerank that timed out, is retried.

//!
//! **Prefilter vectors come from the derived store, not from a live encode.**
//! Python fetched them from Chroma and fell back to encoding on miss. Here the
//! vector is guaranteed to exist before the queue entry does: `minilm_index.rs`
//! calls `enqueue_smart_cluster_pending` only after `commit_derived_embedding`
//! succeeds. A subject with no visible vector is therefore not a miss to repair
//! but a screenshot whose row was deleted or invalidated after enqueue, and
//! leaving it out of this pass is correct.
//!
//! **Two things this pass is not allowed to monopolize.**
//!
//! The first is the other background pass. Capture indexing (`minilm_index.rs`)
//! polls on the same 60-second cadence and gates on the same idle signal, so the
//! two wake together — and they want different models from an engine that keeps
//! exactly one resident, so running at once means taking turns evicting a
//! session rather than finishing sooner. Both claim
//! `semantic_runtime::BACKGROUND_PASS_GUARD` first; an idle tick that loses it
//! skips, and a forced drain waits.
//!
//! The second is the user. A foreground NL query has five seconds to reach the
//! same single-slot worker (`semantic_query.rs::QUERY_EMBED_TIMEOUT`) against
//! this pass's three hundred, and it announces itself through
//! `SemanticRuntimeState::foreground_lease` rather than through the idle signal,
//! which `idle.rs` refreshes only every ten seconds — far too slow for a search
//! typed a second after the user sits down. Both passes check that signal
//! between clusters, between commit groups, and — through
//! `RerankPriority::Background` — between rerank chunks, which is what bounds
//! the wait at one document rather than at one batch.
//!
//! **A forced drain observes it too, and waits rather than ending.** It used to
//! read straight past it on the grounds that the user had pressed its button.
//! That is a sound argument about consent and the wrong answer about cost: a
//! reranked calibration query wants the same cross-encoder this pass is
//! feeding, so interleaving does not share the worker between them, it halves
//! both; a plain search wants MiniLM instead and evicts what this pass is
//! holding ([`crate::semantic_runtime::BACKGROUND_PASS_GUARD`] states that
//! cost). Waiting and resuming keeps the button meaning what it says without
//! making the query it collides with pay for it; [`stand_aside_for_foreground`]
//! is where that happens, and [`FORCED_FOREGROUND_WAIT_BUDGET`] is what stops a
//! drain sitting out a whole calibration session in silence.
//!
//! **Standing down has to leave progress behind.** Every one of those checks
//! can end a pass part-way through a batch, and the idle gate closes the moment
//! the user touches the machine. A queue entry is only safe to delete once
//! every enabled cluster has scored it, so the pass commits in small groups
//! (`MAX_COMMIT_PAIRS`) instead of at the end: an interruption costs the group
//! in flight and nothing else. Deleting only at the end, against a queue read
//! `ORDER BY queued_at ASC`, meant every interrupted pass restarted at the same
//! head — a machine whose idle windows are shorter than a batch takes to score
//! would redo the same work forever and never reach a newly captured screenshot.

use crate::idle::IdleState;
use crate::ml_protocol::MlSemanticModel;
use crate::rerank::{
    build_rerank_document, ScorerIdentity, SmartClusterQueueOwner, RERANK_OCR_SNIPPET_CHARS,
};
use crate::semantic_runtime::{SemanticRuntimeState, FOREGROUND_POLL_INTERVAL};
use crate::storage::smart_cluster::{
    anchor_text_hash, SmartClusterScorer, SmartClusterScoringTarget,
};
use crate::storage::{DerivedIndexKind, StorageState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// How often the worker asks whether there is work and whether it may run.
/// Matches Python's `TICK_INTERVAL_SECS`.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Snapshots drained per pass. Python used 32 idle / 128 forced; the same split
/// is kept so a manual run still clears a visible amount in one go.
const IDLE_BATCH: i64 = 32;
const FORCED_BATCH: i64 = 128;

/// MiniLM cosine cutoff before a pair reaches the cross-encoder. Python's
/// `PREFILTER_THRESHOLD`. Changing it would change which pairs are scored at
/// all, which no stored threshold accounts for.
const PREFILTER_THRESHOLD: f32 = 0.40;

/// Ceiling on `(snapshot × cluster)` pairs considered in one pass, mirroring
/// Python's `MAX_PREFILTER_PAIRS`. With many enabled clusters the batch shrinks
/// rather than the pair set growing without bound.
const MAX_PREFILTER_PAIRS: i64 = 4096;

/// Ceiling on `(snapshot × cluster)` pairs in one commit group — the unit of
/// work that survives, or is lost, together.
///
/// A queue entry may only be deleted once every enabled cluster has scored it,
/// so this is what an interruption costs: the pass repeats at most this many
/// pairs next time. Small enough that a scoring pass makes progress in idle
/// windows far shorter than a full batch takes, large enough that the two
/// database reads per group stay negligible next to the cross-encoder work
/// they feed.
const MAX_COMMIT_PAIRS: i64 = 16;

/// Deadline for one cluster's rerank call chain, including a cold 570 MB load.
const RERANK_TIMEOUT: Duration = Duration::from_secs(300);

/// Deadline for embedding the anchor texts. Short: they are a handful of
/// sentences, and MiniLM is usually already resident from capture indexing.
const ANCHOR_EMBED_TIMEOUT: Duration = Duration::from_secs(120);

/// Wall-clock ceiling on one forced drain, so "run now" cannot become an
/// unbounded foreground job on a machine with a deep queue.
const FORCED_DEADLINE: Duration = Duration::from_secs(600);

/// How long a forced drain may spend, in total, standing aside for foreground
/// queries before it gives up and says so.
///
/// Waiting out one query is right: a reranked calibration query is at most
/// `MAX_RERANK_RESULTS * RERANK_OVERFETCH` documents at 0.31–1.18 s each, so a
/// couple of minutes on the slowest machine measured, and the queue is not
/// going anywhere. Waiting out a calibration *session* is not: the drain would
/// spend its whole [`FORCED_DEADLINE`] never reaching the worker and then log
/// that it ran out of time, which reads as a deep queue rather than as a run
/// that politely never started.
///
/// Counted across the drain rather than per pause, because it is the total the
/// user is unknowingly spending that decides whether their click accomplished
/// anything. Ending here costs nothing but the button: the idle pass picks the
/// same queue up a minute later.
const FORCED_FOREGROUND_WAIT_BUDGET: Duration = Duration::from_secs(180);

/// The one stand-down reason a forced drain resumes from instead of ending on.
///
/// Every other reason [`stand_down_reason`] gives is a verdict about whether
/// the pass should exist at all — the user came back, pressed stop, or the
/// clock ran out. This one is a verdict about *when*, so a forced drain answers
/// it by waiting; an idle pass answers it by ending, because its next tick is a
/// minute away and nobody asked for it.
const FOREGROUND_QUERY_STOP: &str = "a foreground query holds the semantic worker";

/// Reason for a forced drain that spent [`FORCED_FOREGROUND_WAIT_BUDGET`]
/// waiting for a worker it never got. Distinguished from the wall-clock budget
/// because the two say different things: one queue is deep, the other machine
/// was busy answering that same user's searches.
const WAITED_OUT_BY_FOREGROUND: &str =
    "foreground queries held the semantic worker for the whole wait budget";

/// The stand-down reasons a pass ends on, as opposed to the one it may resume
/// from.
///
/// Named rather than written inline because each is produced from two places —
/// [`stand_down_reason`] and [`stand_aside_for_foreground`] — and read by
/// comparison against [`FOREGROUND_QUERY_STOP`]. Two of them colliding by value
/// is the failure the comparison cannot survive, and three copies of a string
/// literal is how that collision gets written by accident.
const STOP_REQUESTED: &str = "stop was requested";
const FORCED_DEADLINE_REACHED: &str = "the forced drain reached its wall-clock budget";
const MAY_NOT_RUN: &str = "the pass may no longer run";

/// Worker state shared with the Tauri commands that drive it.
#[derive(Default)]
pub struct SmartClusterWorkerState {
    /// A pass is executing right now.
    running: AtomicBool,
    /// The executing pass is a user-requested one.
    force_running: AtomicBool,
    /// Set by `smart_cluster_drain_now`, cleared when the pass picks it up.
    drain_requested: AtomicBool,
    /// Set by `smart_cluster_stop_drain`; the forced pass checks it between
    /// clusters and between batches.
    abort_requested: AtomicBool,
    /// Assignment rows written since process start, for the status command.
    ///
    /// Counts writes, not distinct snapshots: `record_smart_cluster_assignment`
    /// is an upsert, so a snapshot in a commit group that was interrupted and
    /// re-scored is counted again. Bounded by the group size, and the number is
    /// a log and diagnostic line rather than anything the UI presents as an
    /// archive count.
    assigned_total: AtomicU64,
    /// Clusters skipped in the last pass because their threshold could not be
    /// re-derived for the current scorer.
    unverifiable_thresholds: AtomicU64,
    /// The last queue-ownership verdict written to the log, so an arrangement
    /// that needs the user to do something is explained when it changes rather
    /// than once a minute forever.
    last_owner_note: AtomicU8,
}

impl SmartClusterWorkerState {
    pub fn request_drain_now(&self) {
        self.abort_requested.store(false, Ordering::SeqCst);
        self.drain_requested.store(true, Ordering::SeqCst);
    }

    pub fn request_stop_drain(&self) {
        self.abort_requested.store(true, Ordering::SeqCst);
    }

    fn take_drain_request(&self) -> bool {
        self.drain_requested.swap(false, Ordering::SeqCst)
    }

    fn aborted(&self) -> bool {
        self.abort_requested.load(Ordering::SeqCst)
    }

    /// Log who owns the queue, but only when the answer changes.
    ///
    /// Two of the three answers are states the user has to resolve, and neither
    /// is visible anywhere else: `rerank_runtime` has no settings control, so
    /// it is edited in the registry and its two sides take effect at different
    /// times. Saying nothing would leave a queue that stops moving with no
    /// explanation anywhere.
    fn note_queue_owner(&self, owner: SmartClusterQueueOwner) {
        let code = match owner {
            SmartClusterQueueOwner::Rust => 1u8,
            SmartClusterQueueOwner::Python => 2,
            SmartClusterQueueOwner::Neither => 3,
        };
        if self.last_owner_note.swap(code, Ordering::SeqCst) == code {
            return;
        }
        match owner {
            SmartClusterQueueOwner::Rust => {
                tracing::info!("[SMART_CLUSTER] this worker owns the scoring queue")
            }
            SmartClusterQueueOwner::Python => tracing::info!(
                "[SMART_CLUSTER] standing down: the running monitor was started with \
                 rerank_runtime=python and its worker owns the queue. Restart the monitor to \
                 hand scoring back to Rust."
            ),
            SmartClusterQueueOwner::Neither => tracing::warn!(
                "[SMART_CLUSTER] nothing is draining the scoring queue: rerank_runtime=python, \
                 but the Python worker only starts when the monitor is spawned with that value. \
                 Restart the monitor for the rollback to take effect."
            ),
        }
    }

    pub fn status(&self) -> SmartClusterWorkerStatus {
        SmartClusterWorkerStatus {
            backend: "rust",
            is_running: self.running.load(Ordering::SeqCst),
            is_force_running: self.force_running.load(Ordering::SeqCst),
            assigned_total: self.assigned_total.load(Ordering::SeqCst),
            unverifiable_thresholds: self.unverifiable_thresholds.load(Ordering::SeqCst),
            scorer: ScorerIdentity::current(),
        }
    }
}

/// Read-only diagnostic for the worker, the shape `monitor_smart_cluster_worker_status`
/// used to forward from Python plus the scorer identity step 6 makes relevant.
#[derive(Debug, Clone, Serialize)]
pub struct SmartClusterWorkerStatus {
    pub backend: &'static str,
    pub is_running: bool,
    pub is_force_running: bool,
    pub assigned_total: u64,
    /// Enabled clusters whose threshold could not be verified or re-derived for
    /// the current scorer, and which were therefore not scored.
    pub unverifiable_thresholds: u64,
    pub scorer: ScorerIdentity,
}

/// Background loop. Wakes on the tick or on a drain request.
pub async fn run_smart_cluster_worker(app: AppHandle) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tick = Instant::now() - POLL_INTERVAL;
    loop {
        ticker.tick().await;
        let state = app.state::<Arc<SmartClusterWorkerState>>().inner().clone();
        let forced = state.take_drain_request();
        if !forced && last_tick.elapsed() < POLL_INTERVAL {
            continue;
        }
        last_tick = Instant::now();
        {
            let monitor = app.try_state::<crate::monitor::MonitorState>();
            state.note_queue_owner(crate::rerank::smart_cluster_queue_owner(monitor.as_deref()));
        }
        state.running.store(true, Ordering::SeqCst);
        state.force_running.store(forced, Ordering::SeqCst);
        // Serialized against capture indexing. Both loops poll every 60 s on
        // the same idle signal and were spawned seconds apart, so they wake
        // together; and they want different models from an engine that keeps
        // one resident, so running them at once means taking turns evicting a
        // 570 MB session instead of finishing either pass sooner.
        //
        // An idle tick that loses the guard skips: the next one is a minute
        // away and the queue is not going anywhere. A forced drain waits for
        // it, because the user pressed a button and an index pass releases the
        // guard in bounded time; `run_pass` re-checks the abort flag first
        // thing, so a stop pressed while waiting still takes effect.
        let guard = if forced {
            Some(crate::semantic_runtime::BACKGROUND_PASS_GUARD.lock().await)
        } else {
            crate::semantic_runtime::BACKGROUND_PASS_GUARD
                .try_lock()
                .ok()
        };
        match guard {
            Some(_guard) => {
                if let Err(error) = run_pass(&app, &state, forced).await {
                    tracing::warn!("[SMART_CLUSTER] pass failed: {error}");
                }
            }
            None => tracing::debug!(
                "[SMART_CLUSTER] skipping this tick: another background pass holds the semantic worker"
            ),
        }
        state.force_running.store(false, Ordering::SeqCst);
        state.running.store(false, Ordering::SeqCst);
    }
}

/// Whether a pass may run at all.
fn may_run(app: &AppHandle, forced: bool) -> bool {
    if crate::maintenance::is_active() {
        return false;
    }
    // The one-release rollback, asked as "who owns the queue" rather than as
    // "what does the switch say". Those differ while a monitor spawned with
    // `rerank_runtime = python` is running: its worker is draining, and this
    // one must stand down even if the key has since been set back to `rust`.
    //
    // `semantic_runtime` is deliberately *not* consulted here. That lever
    // chooses which runtime answers an NL query, and there is no Python side
    // left for this pass to fall back to: the prefilter vectors live in the
    // Rust derived store, which the Python worker cannot read, and the Python
    // scorer only starts when `rerank_runtime = python` handed it the queue at
    // spawn. Honoring `semantic_runtime` here would not move the work to
    // Python, it would stop the queue being drained at all. `rerank_runtime` is
    // the lever for this pass, and it moves calibration with it.
    if !crate::rerank::rust_owns_smart_cluster_queue(
        app.try_state::<crate::monitor::MonitorState>().as_deref(),
    ) {
        return false;
    }
    if forced {
        return true;
    }
    // A foreground NL query is queued for the same single-slot worker on a 5 s
    // budget, against this pass's 300 s. Checked separately from the idle gate
    // because the idle signal is up to ten seconds stale (`idle.rs` polls every
    // 10 s) and the query that follows the user returning arrives well inside
    // that window.
    //
    // Only the idle gate is asked here. The foreground lease binds a forced
    // drain as well, but it is not a reason for one to stop existing — it is a
    // reason to wait — so `stand_down_reason` is where both modes read it and
    // `run_pass` is where they part ways.
    app.state::<Arc<IdleState>>()
        .is_idle
        .load(Ordering::Relaxed)
}

/// Why this pass has to stop where it is, or `None` if it may keep going.
///
/// Every reason here leaves the remaining work queued, so the loops all respond
/// the same way: stop where they are and hand the answer up. It exists as one
/// function because the checks have to happen in more than one loop — between
/// batches, between commit groups, between clusters, and between threshold
/// re-derivations — and a check that only some of those loops perform is how a
/// bound ends up not binding.
///
/// The wall-clock deadline is forced-only. An idle pass runs a single batch and
/// is bounded by the idle gate closing, which is the honest bound for it; a
/// forced drain has no such gate, which is exactly why it needs a clock.
///
/// [`FOREGROUND_QUERY_STOP`] is the one answer that does not mean the same
/// thing to both modes — an idle pass ends on it and a forced drain waits it
/// out — so it is returned ahead of the idle gate rather than folded into "may
/// no longer run". A reason a caller cannot tell apart is a reason it cannot
/// act differently on.
fn stand_down_reason(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    forced: bool,
    deadline: Instant,
) -> Option<&'static str> {
    if forced {
        if state.aborted() {
            return Some(STOP_REQUESTED);
        }
        if Instant::now() >= deadline {
            return Some(FORCED_DEADLINE_REACHED);
        }
    }
    if app
        .state::<Arc<SemanticRuntimeState>>()
        .foreground_waiting()
    {
        return Some(FOREGROUND_QUERY_STOP);
    }
    if !may_run(app, forced) {
        return Some(MAY_NOT_RUN);
    }
    None
}

/// Wait out a foreground query rather than competing with it, and say why the
/// drain must end if one of its own bounds expires first.
///
/// `None` once the worker is free. `waited` accumulates across every call in a
/// drain, because [`FORCED_FOREGROUND_WAIT_BUDGET`] is a budget for the drain
/// and not for any one pause.
///
/// The ways out other than the worker coming free are the ones the drain
/// already had — the stop button, the wall-clock deadline, maintenance mode and
/// the rest of `may_run` — checked here because a pause is exactly where a
/// drain is most likely to be sitting when one of them fires. Nothing is held
/// while this waits: the batch it left released its groups uncommitted and took
/// no database lock with it, so a drain that never resumes leaves the queue
/// exactly as it found it.
///
/// Idle passes never call this. Standing aside costs a wait, and a wait is only
/// worth paying for work somebody is waiting on; an idle tick that ends here
/// would come back a minute later anyway.
async fn stand_aside_for_foreground(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    deadline: Instant,
    waited: &mut Duration,
) -> Option<&'static str> {
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    if !semantic.foreground_waiting() {
        return None;
    }
    let started = Instant::now();
    tracing::info!("[SMART_CLUSTER] forced drain standing aside for a foreground query");
    let outcome = loop {
        if state.aborted() {
            break Some(STOP_REQUESTED);
        }
        let now = Instant::now();
        if now >= deadline {
            break Some(FORCED_DEADLINE_REACHED);
        }
        if *waited + now.duration_since(started) >= FORCED_FOREGROUND_WAIT_BUDGET {
            break Some(WAITED_OUT_BY_FOREGROUND);
        }
        // Maintenance mode and the queue-ownership verdict can both change
        // under a drain that is doing nothing, and neither is worth resuming
        // into. The idle gate is deliberately not consulted: a forced drain
        // does not have one, and waiting is not the moment to grow one.
        if !may_run(app, true) {
            break Some(MAY_NOT_RUN);
        }
        if !semantic.foreground_waiting() {
            break None;
        }
        tokio::time::sleep(FOREGROUND_POLL_INTERVAL).await;
    };
    *waited += started.elapsed();
    match outcome {
        None => tracing::info!(
            "[SMART_CLUSTER] forced drain resuming after {:.1}s; the foreground query is done",
            started.elapsed().as_secs_f64()
        ),
        Some(reason) => tracing::info!(
            "[SMART_CLUSTER] forced drain ending while stood aside: {reason} \
             (waited {:.1}s in total)",
            waited.as_secs_f64()
        ),
    }
    outcome
}

async fn run_pass(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    forced: bool,
) -> Result<(), String> {
    if !may_run(app, forced) {
        return Ok(());
    }
    // The deadline is the forced drain's runaway clock and starts at the click
    // rather than at the first score, so a drain that spends its opening
    // minutes waiting is honest about having spent them. An idle pass never
    // reads it.
    let deadline = Instant::now() + FORCED_DEADLINE;
    let mut waited = Duration::ZERO;
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    // Unattended work must never ask Rust to decrypt protected data before the
    // user has unlocked the session.
    if !storage.is_session_valid() {
        return Ok(());
    }

    let mut scored_anything = false;
    // Clusters whose re-derivation failed for a reason that may not repeat — a
    // rerank error or timeout. Remembered for the length of this pass so a
    // forced drain does not re-attempt, and re-pay for, the same failure once
    // per batch. Deliberately not persisted: the next pass is a minute away and
    // a transient failure deserves another try by then.
    let mut rederive_failed_this_pass: HashSet<i64> = HashSet::new();
    let result = loop {
        // One read of the lease per iteration, and waiting is the response to
        // it rather than a separate check in front of it. A pre-check would
        // race: the lease can be taken again in the moment between a wait
        // returning and the stand-down check reading it, and a forced drain
        // that broke on that would end for the very reason it is supposed to
        // wait out — intermittently, which is the worst way for it to happen.
        if let Some(reason) = stand_down_reason(app, state, forced, deadline) {
            if !forced || reason != FOREGROUND_QUERY_STOP {
                if forced {
                    tracing::info!(
                        "[SMART_CLUSTER] forced drain stopped between batches: {reason}"
                    );
                }
                break Ok(());
            }
            // Waiting before claiming a batch, not after: a batch read and then
            // held across a pause is a batch nothing else can take either, and
            // the drain would leave its groups uncommitted one cluster later
            // anyway. Looping back rather than falling through keeps the lease
            // read in one place.
            if let Some(reason) =
                stand_aside_for_foreground(app, state, deadline, &mut waited).await
            {
                tracing::info!("[SMART_CLUSTER] forced drain stopped between batches: {reason}");
                break Ok(());
            }
            continue;
        }
        let batch = match run_batch(
            app,
            state,
            storage.clone(),
            forced,
            deadline,
            &mut rederive_failed_this_pass,
        )
        .await
        {
            Ok(batch) => batch,
            Err(error) => break Err(error),
        };
        scored_anything = true;
        // A foreground query is the one interruption a forced drain walks back
        // into rather than ends on, and the wait at the top of the loop is what
        // makes "walks back in" mean "once it clears" instead of "immediately".
        // The poll interval is charged to the wait budget here rather than left
        // to the top of the loop so that a lease flapping across the batch read
        // — back-to-back queries from a calibration session — cannot turn this
        // into one database round trip per iteration for the length of the
        // deadline.
        if forced && batch.stopped_because == Some(FOREGROUND_QUERY_STOP) {
            tokio::time::sleep(FOREGROUND_POLL_INTERVAL).await;
            waited += FOREGROUND_POLL_INTERVAL;
            continue;
        }
        if batch.stopped_because.is_some() || !batch.more || !forced {
            // An idle pass processes one batch per tick, as Python did; the tick
            // is a minute away and the queue is not going anywhere.
            break Ok(());
        }
    };

    // The cross-encoder is 570 MB and the engine keeps one model resident, so a
    // finished pass releases it rather than leaving an idle machine holding it
    // until the next capture-indexing run happens to evict it. Python's worker
    // unloaded its reranker at the same point.
    //
    // Conditioned on what is actually resident, not on whether a pass ran: a
    // pass that found an empty queue never loaded anything, and unloading then
    // would evict the MiniLM session the capture indexer is about to reuse.
    //
    // Skipped outright while a foreground query is waiting, even though the
    // model to free is exactly the one in its way. An unload is itself a
    // request against the single slot, and the queue is first-come — so it
    // would land *behind* the query it was meant to help and then free the
    // MiniLM session that query had just paid to load. The next pass is a
    // minute away and will release it then.
    if scored_anything && !semantic.foreground_waiting() && reranker_is_resident(app) {
        if let Err(error) = semantic.unload_model(app.clone()).await {
            tracing::debug!("[SMART_CLUSTER] reranker unload after the pass failed: {error}");
        }
    }
    result
}

fn reranker_is_resident(app: &AppHandle) -> bool {
    let expected = crate::semantic_models::descriptor(MlSemanticModel::BgeRerankerV2M3).model_id;
    app.state::<Arc<SemanticRuntimeState>>()
        .status()
        .loaded_model
        .is_some_and(|loaded| loaded == expected)
}

/// What one batch concluded.
///
/// The reason is carried out rather than collapsed into "no more work" because
/// exactly one of them — [`FOREGROUND_QUERY_STOP`] — is something a forced
/// drain resumes from, and a caller that could not tell it apart would either
/// end a drain the user is watching or spin re-reading the queue for the length
/// of the deadline.
struct BatchProgress {
    /// Whether the queue is likely to still hold work. Meaningless when the
    /// batch stopped early, since it never got far enough to find out.
    more: bool,
    /// Why the batch stopped part-way, if it did. `None` means it finished.
    stopped_because: Option<&'static str>,
}

impl BatchProgress {
    /// A batch that ran to the end. `more` is what the queue count said.
    fn completed(more: bool) -> Self {
        Self {
            more,
            stopped_because: None,
        }
    }

    /// A batch that stopped part-way. Whatever it committed stands; the rest is
    /// still queued, so there is by construction more work.
    fn stopped(reason: &'static str) -> Self {
        Self {
            more: true,
            stopped_because: Some(reason),
        }
    }
}

/// Process one batch.
async fn run_batch(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    storage: Arc<StorageState>,
    forced: bool,
    deadline: Instant,
    rederive_failed_this_pass: &mut HashSet<i64>,
) -> Result<BatchProgress, String> {
    let read = {
        let storage = storage.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<BatchInputs>, String> {
            let pending = storage.count_smart_cluster_pending()?;
            if pending <= 0 {
                return Ok(None);
            }
            let targets = storage.list_smart_cluster_scoring_targets()?;
            let batch_size = batch_size_for(forced, targets.len());
            if targets.is_empty() {
                // No enabled clusters. Python drained the queue anyway so it
                // could not grow without bound while clusters are switched off,
                // and the same reasoning applies: nothing will ever score these.
                let stale = storage.peek_smart_cluster_pending_batch(batch_size)?;
                if !stale.is_empty() {
                    storage.delete_smart_cluster_pending_ids(&stale)?;
                }
                return Ok(None);
            }
            let ids = storage.peek_smart_cluster_pending_batch(batch_size)?;
            if ids.is_empty() {
                return Ok(None);
            }
            let remaining = (pending - ids.len() as i64).max(0);
            Ok(Some(BatchInputs {
                targets,
                ids,
                remaining,
            }))
        })
        .await
        .map_err(|error| format!("smart cluster read task failed: {error}"))??
    };
    let Some(inputs) = read else {
        return Ok(BatchProgress::completed(false));
    };

    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let scorer = ScorerIdentity::current();

    // Thresholds first: a cluster whose threshold cannot be vouched for must
    // not score anything this pass, and re-deriving it may make it usable.
    let resolution = resolve_thresholds(
        app,
        state,
        &semantic,
        storage.clone(),
        inputs.targets,
        &scorer,
        forced,
        deadline,
        rederive_failed_this_pass,
    )
    .await;
    state
        .unverifiable_thresholds
        .store(resolution.unverifiable as u64, Ordering::SeqCst);
    if let Some(reason) = resolution.interrupted {
        // Resolution stood down part-way, so `usable` is a prefix of the
        // enabled clusters rather than all of them. Scoring against a prefix
        // would be a silent loss of coverage: the queue entries are deleted
        // once every cluster in `usable` has scored them, so the clusters this
        // stopped short of would never see these screenshots again. Nothing is
        // deleted and nothing is scored; the next pass starts over.
        return Ok(BatchProgress::stopped(reason));
    }
    let targets = resolution.usable;
    if targets.is_empty() {
        if resolution.retryable > 0 {
            // At least one cluster failed for a reason that may not repeat, so
            // the ids stay queued for a pass that can score them.
            return Ok(BatchProgress::completed(false));
        }
        // Every enabled cluster has been given up on: its saved examples cannot
        // produce a threshold under this scorer, and nothing but a
        // recalibration will change that. Draining is the same call the
        // no-enabled-clusters branch above makes, for the same reason — these
        // ids will never be scored, and holding them would leave a queue that
        // grows with every capture, a status line that claims work is pending,
        // and a pass that wakes to re-discover the same verdict every minute.
        // The warning banner keeps saying which clusters need attention, and a
        // rescan re-enqueues the window once they have it.
        delete_pending(storage, &inputs.ids).await?;
        return Ok(BatchProgress::completed(inputs.remaining > 0));
    }

    let anchors = embed_anchors(app, &semantic, storage.clone(), &targets).await?;

    // The batch is scored and committed in small groups rather than as one
    // unit. What forces this is that the pass can be interrupted at any point —
    // the user comes back and the idle gate closes, a foreground query takes
    // the worker, a forced drain is cancelled — and a queue entry is only safe
    // to delete once every enabled cluster has had its turn at it. Committing
    // the whole batch at the end meant an interrupted pass left all 32 ids
    // queued, and `peek_smart_cluster_pending_batch` orders by `queued_at ASC`,
    // so the next pass began at the same head. On a machine whose idle windows
    // are shorter than one batch takes to score, the queue never advanced past
    // its first entries: work already done was thrown away and repeated, and
    // newly captured screenshots were never reached.
    //
    // A group is bounded in `(snapshot × cluster)` pairs, not snapshots,
    // because the pairs are what cost cross-encoder time. Grouping cannot move
    // a score: a cross-encoder evaluates each `(query, document)` pair on its
    // own, and the prefilter compares one stored vector against one anchor.
    let group_size = commit_group_size(targets.len());
    let mut assigned = 0u64;
    let mut interrupted: Option<&'static str> = None;
    for group in inputs.ids.chunks(group_size) {
        if let Some(reason) = stand_down_reason(app, state, forced, deadline) {
            tracing::debug!("[SMART_CLUSTER] leaving the batch before a commit group: {reason}");
            interrupted = Some(reason);
            break;
        }
        let documents = load_documents(storage.clone(), group).await?;
        if documents.is_empty() {
            // Every snapshot in this group was deleted between enqueue and now;
            // the queue entries have nothing left to describe.
            delete_pending(storage.clone(), group).await?;
            continue;
        }
        let vectors = load_prefilter_vectors(storage.clone(), documents.keys().copied()).await?;

        for target in &targets {
            // Checked before every cross-encoder call rather than once per
            // group, because one call is the granularity at which this pass can
            // actually be stopped: the request cannot be interrupted once it is
            // submitted. Whatever was already assigned stands and every group
            // committed before this one stays committed; only this group is
            // scored again next pass.
            if let Some(reason) = stand_down_reason(app, state, forced, deadline) {
                tracing::debug!("[SMART_CLUSTER] leaving the batch between clusters: {reason}");
                interrupted = Some(reason);
                break;
            }
            let Some(anchor) = anchors.get(&target.id) else {
                continue;
            };
            let candidates = prefilter(anchor, &vectors, &documents);
            if candidates.is_empty() {
                continue;
            }
            let docs: Vec<String> = candidates.iter().map(|id| documents[id].clone()).collect();
            let scores = match crate::rerank::rerank_documents(
                app,
                &semantic,
                &target.anchor_text,
                &docs,
                crate::rerank::RerankBudget::Total(RERANK_TIMEOUT),
                // Background for a forced drain too; see [`SCORING_PRIORITY`].
                SCORING_PRIORITY,
                // Not the query the calibration page is watching, so it neither
                // advances that page's progress bar nor answers its stop button.
                None,
            )
            .await
            {
                Ok(scores) => scores,
                Err(error) if crate::rerank::is_yield(&error) => {
                    // Not a failure: a foreground query arrived and this pass
                    // gave up the worker before submitting the next chunk. Only
                    // the current group stays queued.
                    tracing::debug!(
                        "[SMART_CLUSTER] standing down for a foreground query; \
                         the current group stays queued"
                    );
                    interrupted = Some(FOREGROUND_QUERY_STOP);
                    break;
                }
                Err(error) => {
                    tracing::warn!(
                        "[SMART_CLUSTER] rerank failed for cluster {}: {error}",
                        target.id
                    );
                    continue;
                }
            };
            let recorded =
                record_assignments(storage.clone(), target, &candidates, &scores).await?;
            if recorded > 0 {
                // Published as it happens rather than at the end of the batch,
                // so an interrupted pass reports the work it actually did.
                state.assigned_total.fetch_add(recorded, Ordering::SeqCst);
                assigned += recorded;
            }
        }
        if interrupted.is_some() {
            // Committed groups stay committed; this one is scored again next
            // pass, which is the whole of what standing down now costs.
            break;
        }
        delete_pending(storage.clone(), group).await?;
    }

    if assigned > 0 {
        tracing::info!("[SMART_CLUSTER] recorded {assigned} assignment(s)");
    }
    match interrupted {
        Some(reason) => Ok(BatchProgress::stopped(reason)),
        None => Ok(BatchProgress::completed(inputs.remaining > 0)),
    }
}

/// How many snapshots are scored against every enabled cluster before the
/// queue entries for them are deleted.
///
/// This is the unit of work an interruption can cost, so it is bounded by the
/// cross-encoder pairs it implies rather than by a snapshot count: with one
/// cluster enabled a group is 16 snapshots, with sixteen it is one snapshot.
/// The smaller the group the more often the pass touches the database and the
/// less it loses when interrupted; [`MAX_COMMIT_PAIRS`] is where that trade is
/// set.
fn commit_group_size(cluster_count: usize) -> usize {
    (MAX_COMMIT_PAIRS / cluster_count.max(1) as i64).max(1) as usize
}

/// The rerank contract every pass in this module runs under, forced or not.
///
/// [`crate::rerank::RerankPriority`] names what a call does to the worker — how
/// big a chunk it submits, and whether it checks between chunks — not who asked
/// for it. A forced drain is still different from an idle tick, in a bigger
/// batch, in ignoring the idle gate, and in waiting and resuming rather than
/// ending; none of those require holding the worker through somebody else's
/// query. It used to run `Foreground` here, which bought it a whole
/// `rerank_documents` call with no check inside it — a commit group's
/// candidates when scoring, around nineteen seconds at [`MAX_COMMIT_PAIRS`] on
/// the slowest machine measured, and a cluster's entire saved example set when
/// re-deriving a threshold, which nothing bounds at all. A foreground query has
/// five seconds to reach the same single slot.
const SCORING_PRIORITY: crate::rerank::RerankPriority = crate::rerank::RerankPriority::Background;

struct BatchInputs {
    targets: Vec<SmartClusterScoringTarget>,
    ids: Vec<i64>,
    remaining: i64,
}

/// Shrink the batch so `(snapshots × clusters)` stays inside the pair budget.
fn batch_size_for(forced: bool, cluster_count: usize) -> i64 {
    let base = if forced { FORCED_BATCH } else { IDLE_BATCH };
    let budget = MAX_PREFILTER_PAIRS / (cluster_count.max(1) as i64);
    base.min(budget.max(1))
}

/// What one pass over the enabled clusters concluded about their thresholds.
struct ThresholdResolution {
    /// Clusters that may be scored this pass.
    usable: Vec<SmartClusterScoringTarget>,
    /// Enabled clusters that will not be scored, for the status banner.
    unverifiable: usize,
    /// How many of those might succeed later. Zero means every unusable cluster
    /// has been given up on, which is what lets the caller drain a queue that
    /// nothing will ever score.
    retryable: usize,
    /// Why the pass stood down before every enabled cluster had been
    /// considered, if it did.
    ///
    /// `usable` is then a prefix rather than a verdict on the whole set, which
    /// the caller must not score against — see `run_batch`.
    interrupted: Option<&'static str>,
}

/// Keep only the clusters whose threshold this build's scorer can be compared
/// against, re-deriving where the stored provenance does not match.
///
/// Re-derivation is expensive — it loads the 570 MB cross-encoder — and it can
/// fail in two very different ways. A cluster whose saved examples can no longer
/// produce a threshold will fail identically forever, so that verdict is written
/// to the row and the cluster is skipped from then on without touching the
/// model. A rerank that errored or timed out may well succeed next time, so it
/// is remembered only for the length of this pass.
///
/// The stand-down check sits in front of the re-derivation rather than at the
/// top of the loop on purpose. Everything above it is a comparison of strings
/// already in memory, so a cluster that needs no work should still be resolved
/// while the pass is winding down; what must not start once the user is back,
/// the clock has run out, or stop has been pressed is another cross-encoder
/// call, which is the only thing in here that costs anything.
async fn resolve_thresholds(
    app: &AppHandle,
    state: &Arc<SmartClusterWorkerState>,
    semantic: &Arc<SemanticRuntimeState>,
    storage: Arc<StorageState>,
    targets: Vec<SmartClusterScoringTarget>,
    scorer: &ScorerIdentity,
    forced: bool,
    deadline: Instant,
    failed_this_pass: &mut HashSet<i64>,
) -> ThresholdResolution {
    let fingerprint = scorer.fingerprint();
    let mut resolution = ThresholdResolution {
        usable: Vec::with_capacity(targets.len()),
        unverifiable: 0,
        retryable: 0,
        interrupted: None,
    };
    for mut target in targets {
        if scorer.matches_stored(
            Some(&target.scorer.model_id),
            Some(&target.scorer.model_revision),
            Some(&target.scorer.variant),
            Some(&target.scorer.provider),
        ) {
            resolution.usable.push(target);
            continue;
        }
        if failed_this_pass.contains(&target.id) {
            resolution.unverifiable += 1;
            resolution.retryable += 1;
            continue;
        }
        if target.rederive_failed_scorer.as_deref() == Some(fingerprint.as_str()) {
            // Already established, on an earlier pass, that this cluster's
            // examples cannot produce a threshold under this scorer. Re-asking
            // would load the cross-encoder to reach the same answer.
            resolution.unverifiable += 1;
            continue;
        }
        if let Some(reason) = stand_down_reason(app, state, forced, deadline) {
            tracing::debug!(
                "[SMART_CLUSTER] stopping threshold resolution before cluster {}: {reason}",
                target.id
            );
            resolution.interrupted = Some(reason);
            break;
        }
        match rederive_threshold(app, semantic, storage.clone(), &target, scorer).await {
            Ok(Some(threshold)) => {
                tracing::info!(
                    "[SMART_CLUSTER] re-derived threshold for cluster {} under the current scorer: {:.4} -> {threshold:.4}",
                    target.id,
                    target.threshold
                );
                target.threshold = threshold;
                target.scorer = SmartClusterScorer {
                    model_id: scorer.model_id.clone(),
                    model_revision: scorer.model_revision.clone(),
                    variant: scorer.variant.clone(),
                    provider: scorer.provider.clone(),
                };
                target.scorer_recorded = true;
                target.rederive_failed_scorer = None;
                resolution.usable.push(target);
            }
            Ok(None) => {
                resolution.unverifiable += 1;
                tracing::warn!(
                    "[SMART_CLUSTER] cluster {} has no usable calibration examples; giving up on its threshold until it is recalibrated",
                    target.id
                );
                if let Err(error) =
                    mark_unverifiable(storage.clone(), target.id, fingerprint.clone()).await
                {
                    // The verdict is a cost optimization, not a correctness
                    // requirement: without it the cluster is simply re-examined
                    // next pass. Worth a line in the log, not a failed pass.
                    tracing::warn!(
                        "[SMART_CLUSTER] could not record the unverifiable verdict for cluster {}: {error}",
                        target.id
                    );
                }
            }
            Err(error) if crate::rerank::is_yield(&error) => {
                // A foreground query took the worker before this cluster was
                // asked anything. That is not a verdict about the cluster, so
                // it must not be recorded as one: marking it retryable would
                // leave it out of `usable` while the batch went on to score and
                // then *delete* the queue entries against the clusters that had
                // already resolved, and this one would never see those
                // screenshots again. Ending resolution instead costs the pass
                // the batch it had not started and nothing else.
                tracing::debug!(
                    "[SMART_CLUSTER] threshold re-derivation for cluster {} stood down for a foreground query",
                    target.id
                );
                resolution.interrupted = Some(FOREGROUND_QUERY_STOP);
                break;
            }
            Err(error) => {
                resolution.unverifiable += 1;
                resolution.retryable += 1;
                failed_this_pass.insert(target.id);
                tracing::warn!(
                    "[SMART_CLUSTER] could not re-derive the threshold for cluster {}: {error}",
                    target.id
                );
            }
        }
    }
    resolution
}

async fn mark_unverifiable(
    storage: Arc<StorageState>,
    cluster_id: i64,
    fingerprint: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        storage.mark_smart_cluster_threshold_unverifiable(cluster_id, &fingerprint)
    })
    .await
    .map_err(|error| format!("unverifiable mark task failed: {error}"))?
}

/// Re-score a cluster's saved calibration examples and recompute its threshold.
///
/// `Ok(None)` means the examples can no longer support a threshold — there are
/// no positives left, typically because the screenshots were deleted. That is a
/// recalibration prompt, and the caller records it rather than retrying it.
async fn rederive_threshold(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    storage: Arc<StorageState>,
    target: &SmartClusterScoringTarget,
    scorer: &ScorerIdentity,
) -> Result<Option<f64>, String> {
    let cluster_id = target.id;
    let examples = {
        let storage = storage.clone();
        tokio::task::spawn_blocking(move || storage.list_smart_cluster_examples(cluster_id))
            .await
            .map_err(|error| format!("example read task failed: {error}"))??
    };
    if examples.is_empty() {
        return Ok(None);
    }
    let ids: Vec<i64> = examples
        .iter()
        .map(|example| example.screenshot_id)
        .collect();
    let documents = load_documents(storage.clone(), &ids).await?;
    let scored: Vec<(bool, String)> = examples
        .iter()
        .filter_map(|example| {
            documents
                .get(&example.screenshot_id)
                .map(|document| (example.is_positive, document.clone()))
        })
        .collect();
    // The threshold is derived from the *lowest positive* score, so a cluster
    // whose positive examples have been deleted cannot produce one however many
    // negatives survive. Answering that here rather than after `compute_threshold`
    // is what keeps the answer from costing a 570 MB model load and a rerank of
    // examples whose scores are about to be discarded.
    if !scored.iter().any(|(is_positive, _)| *is_positive) {
        return Ok(None);
    }
    let docs: Vec<String> = scored.iter().map(|(_, doc)| doc.clone()).collect();
    let scores = crate::rerank::rerank_documents(
        app,
        semantic,
        &target.anchor_text,
        &docs,
        crate::rerank::RerankBudget::Total(RERANK_TIMEOUT),
        // Background here for the same reason as the scoring call above; see
        // [`SCORING_PRIORITY`].
        SCORING_PRIORITY,
        None,
    )
    .await?;

    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    for ((is_positive, _), score) in scored.iter().zip(scores) {
        if *is_positive {
            positives.push(f64::from(score));
        } else {
            negatives.push(f64::from(score));
        }
    }
    let Some(threshold) = compute_threshold(&positives, &negatives) else {
        return Ok(None);
    };
    let scorer = SmartClusterScorer {
        model_id: scorer.model_id.clone(),
        model_revision: scorer.model_revision.clone(),
        variant: scorer.variant.clone(),
        provider: scorer.provider.clone(),
    };
    tokio::task::spawn_blocking(move || {
        storage.update_smart_cluster_threshold_with_scorer(cluster_id, threshold, &scorer)
    })
    .await
    .map_err(|error| format!("threshold write task failed: {error}"))??;
    Ok(Some(threshold))
}

/// The calibration threshold formula, ported from `NlClusterView.jsx`.
///
/// ```text
/// base = min(positive) * 0.85
/// if no negatives: base
/// else:            max(base, max(negative) * 1.05)
/// ```
///
/// Kept here rather than re-implemented per caller so a re-derived threshold
/// and a freshly calibrated one cannot come out of different arithmetic. Ported
/// exactly, including the part that looks odd: reranker outputs are raw logits
/// and are routinely negative, so `max(negative) * 1.05` moves *down* for a
/// negative ceiling and the outer `max` is what keeps `base` in that case.
/// Changing this is a behavior change to calibration, not a porting decision.
///
/// `None` when no positive example has a usable score — the recalibration
/// case, not a value to invent.
pub fn compute_threshold(positives: &[f64], negatives: &[f64]) -> Option<f64> {
    let min_positive = positives
        .iter()
        .copied()
        .filter(|score| score.is_finite())
        .fold(f64::INFINITY, f64::min);
    if !min_positive.is_finite() {
        return None;
    }
    let base = min_positive * 0.85;
    let max_negative = negatives
        .iter()
        .copied()
        .filter(|score| score.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    if !max_negative.is_finite() {
        return Some(base);
    }
    Some(base.max(max_negative * 1.05))
}

/// Build the cross-encoder document for each screenshot in the batch.
async fn load_documents(
    storage: Arc<StorageState>,
    ids: &[i64],
) -> Result<HashMap<i64, String>, String> {
    let ids = ids.to_vec();
    tokio::task::spawn_blocking(move || -> Result<HashMap<i64, String>, String> {
        let summaries = storage
            .get_screenshot_summaries_by_ids_silent(&ids)
            .map_err(|error| error.to_string())?;
        let ocr = storage
            .get_ocr_text_prefixes_by_screenshot_ids_silent(&ids, RERANK_OCR_SNIPPET_CHARS)
            .map_err(|error| error.to_string())?;
        Ok(summaries
            .into_iter()
            .map(|summary| {
                let document = build_rerank_document(
                    summary.process_name.as_deref().unwrap_or(""),
                    summary.window_title.as_deref().unwrap_or(""),
                    ocr.get(&summary.id).map(String::as_str).unwrap_or(""),
                );
                (summary.id, document)
            })
            .collect())
    })
    .await
    .map_err(|error| format!("document read task failed: {error}"))?
}

/// Read the stored MiniLM vectors for the batch.
async fn load_prefilter_vectors(
    storage: Arc<StorageState>,
    ids: impl Iterator<Item = i64>,
) -> Result<HashMap<i64, Vec<f32>>, String> {
    let subjects: Vec<String> = ids.map(|id| id.to_string()).collect();
    tokio::task::spawn_blocking(move || -> Result<HashMap<i64, Vec<f32>>, String> {
        let raw = storage
            .get_query_visible_embeddings_by_subjects(DerivedIndexKind::SemanticText, &subjects)?;
        Ok(raw
            .into_iter()
            .filter_map(|(subject, vector)| subject.parse::<i64>().ok().map(|id| (id, vector)))
            .collect())
    })
    .await
    .map_err(|error| format!("vector read task failed: {error}"))?
}

/// Whether a cluster's stored anchor encoding may be used as-is.
///
/// Both conditions are about the same question — would encoding the anchor
/// again today produce this vector? — and both have to hold. The text hash
/// catches an edited anchor, and the model identity catches a build whose
/// bi-encoder moved underneath a vector that was correct when it was written.
/// Getting this wrong is not a slow pass but a wrong one: every assignment
/// would be decided by a prefilter comparing today's snapshots against a
/// yesterday's anchor.
fn reusable_anchor_vector<'a>(
    target: &'a SmartClusterScoringTarget,
    anchor_hash: &str,
    descriptor: &crate::semantic_models::SemanticModelDescriptor,
) -> Option<&'a [f32]> {
    let cached = target.anchor_vector.as_ref()?;
    let matches = cached.source_hash == anchor_hash
        && cached.model_id == descriptor.model_id
        && cached.model_revision == descriptor.revision;
    matches.then(|| cached.vector.as_slice())
}

/// Get each cluster's anchor vector, encoding only the ones that need it.
///
/// **Why this is cached now, having deliberately not been before.** The earlier
/// reasoning was that re-encoding a handful of short sentences is cheaper than
/// a cache whose invalidation can go wrong, and the encode itself really is
/// about two milliseconds. What that missed is that the cost of *needing*
/// MiniLM here is not the encode: the engine keeps exactly one model resident,
/// and the rest of this pass wants the 570 MB cross-encoder. Asking for an
/// anchor encode at the head of every batch therefore bought a model swap per
/// batch — MiniLM in at 0.50 s, the cross-encoder back over it at 1.2 s warm —
/// which on a queue deep enough to need a hundred batches is most of a forced
/// drain's wall-clock budget spent loading models.
///
/// The invalidation worry is answered by keying on content rather than on
/// events. The row stores the hash of the anchor text the vector came from and
/// the model that made it, and either one failing to match means re-encode.
/// Nothing has to remember to clear anything when a cluster is renamed,
/// deleted, disabled, or re-calibrated, which is the class of mistake the
/// original comment was avoiding.
///
/// A write-back failure is logged and otherwise ignored: the vector in hand is
/// correct either way, and the only consequence is that the next batch encodes
/// it again.
async fn embed_anchors(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    storage: Arc<StorageState>,
    targets: &[SmartClusterScoringTarget],
) -> Result<HashMap<i64, Vec<f32>>, String> {
    let descriptor = crate::semantic_models::descriptor(MlSemanticModel::MinilmL12);
    let mut anchors: HashMap<i64, Vec<f32>> = HashMap::with_capacity(targets.len());
    let mut cold: Vec<(i64, String, String)> = Vec::new();
    for target in targets {
        if target.anchor_text.trim().is_empty() {
            continue;
        }
        let hash = anchor_text_hash(&target.anchor_text);
        match reusable_anchor_vector(target, &hash, descriptor) {
            Some(vector) => {
                anchors.insert(target.id, vector.to_vec());
            }
            None => cold.push((target.id, target.anchor_text.clone(), hash)),
        }
    }
    if cold.is_empty() {
        // The whole point: a steady-state pass never touches MiniLM, so the
        // cross-encoder it is about to use stays resident.
        return Ok(anchors);
    }

    for chunk in cold.chunks(crate::ml_protocol::MAX_SEMANTIC_BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(_, text, _)| text.clone()).collect();
        let embedded = semantic
            .embed_text(
                app.clone(),
                MlSemanticModel::MinilmL12,
                texts,
                ANCHOR_EMBED_TIMEOUT,
                false,
            )
            .await?;
        if embedded.vectors.len() != chunk.len() {
            return Err(format!(
                "model_mismatch: anchor encode returned {} vectors for {} clusters",
                embedded.vectors.len(),
                chunk.len()
            ));
        }
        for ((id, _, hash), vector) in chunk.iter().zip(embedded.vectors) {
            let write = {
                let storage = storage.clone();
                let vector = vector.clone();
                let hash = hash.clone();
                let id = *id;
                tokio::task::spawn_blocking(move || {
                    storage.update_smart_cluster_anchor_vector(
                        id,
                        &vector,
                        &hash,
                        descriptor.model_id,
                        descriptor.revision,
                    )
                })
                .await
            };
            match write {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    "[SMART_CLUSTER] could not cache the anchor vector for cluster {id}: {error}"
                ),
                Err(error) => tracing::warn!(
                    "[SMART_CLUSTER] anchor vector cache task failed for cluster {id}: {error}"
                ),
            }
            anchors.insert(*id, vector);
        }
    }
    Ok(anchors)
}

/// Snapshots whose stored vector is close enough to the anchor to be worth a
/// cross-encoder pass. Both sides are L2-normalized, so the dot product is the
/// cosine Python compared against the same cutoff.
fn prefilter(
    anchor: &[f32],
    vectors: &HashMap<i64, Vec<f32>>,
    documents: &HashMap<i64, String>,
) -> Vec<i64> {
    let mut candidates: Vec<i64> = documents
        .keys()
        .copied()
        .filter(|id| {
            vectors
                .get(id)
                .is_some_and(|vector| cosine(anchor, vector) >= PREFILTER_THRESHOLD)
        })
        .collect();
    // A HashMap iteration order is arbitrary; sorting keeps one pass's rerank
    // batches reproducible, which matters when reading logs against a run.
    candidates.sort_unstable();
    candidates
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() {
        return f32::NEG_INFINITY;
    }
    left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>()
}

async fn record_assignments(
    storage: Arc<StorageState>,
    target: &SmartClusterScoringTarget,
    candidates: &[i64],
    scores: &[f32],
) -> Result<u64, String> {
    let cluster_id = target.id;
    let threshold = target.threshold;
    let matches: Vec<(i64, f64)> = candidates
        .iter()
        .copied()
        .zip(scores.iter().copied())
        .filter(|(_, score)| f64::from(*score) >= threshold)
        .map(|(id, score)| (id, f64::from(score)))
        .collect();
    if matches.is_empty() {
        return Ok(0);
    }
    tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let mut recorded = 0u64;
        for (screenshot_id, score) in matches {
            match storage.record_smart_cluster_assignment(cluster_id, screenshot_id, score) {
                Ok(()) => recorded += 1,
                Err(error) => tracing::warn!(
                    "[SMART_CLUSTER] failed to record assignment {cluster_id}/{screenshot_id}: {error}"
                ),
            }
        }
        Ok(recorded)
    })
    .await
    .map_err(|error| format!("assignment write task failed: {error}"))?
}

async fn delete_pending(storage: Arc<StorageState>, ids: &[i64]) -> Result<(), String> {
    let ids = ids.to_vec();
    tokio::task::spawn_blocking(move || storage.delete_smart_cluster_pending_ids(&ids))
        .await
        .map_err(|error| format!("pending delete task failed: {error}"))?
}

// ==================== Command surface ====================
//
// Deliberately not three new Tauri commands. `monitor_smart_cluster_worker_status`,
// `monitor_smart_cluster_drain_now`, and `monitor_smart_cluster_stop_drain`
// already exist and already forward to Python; step 6 makes them branch on
// `rerank_runtime` instead. The frontend contract does not move, and the
// rollback lever switches the whole surface — status, force-run, and cancel —
// in one place rather than leaving the UI talking to two workers at once.

/// Status in the JSON shape the Python handler returned, plus the scorer
/// identity that only became meaningful at step 6.
///
/// `pending_count` is not decoration. `SmartClustersView` computes
/// `is_running && pending_count > 0`, so a payload that omitted the queue depth
/// would report a worker that never runs and would never show the stop button,
/// however busy the drain actually was.
pub fn status_value(state: &Arc<SmartClusterWorkerState>, pending_count: i64) -> serde_json::Value {
    let status = state.status();
    serde_json::json!({
        "status": "success",
        "backend": status.backend,
        "is_running": status.is_running,
        "is_force_running": status.is_force_running,
        "pending_count": pending_count,
        "assigned_total": status.assigned_total,
        "unverifiable_thresholds": status.unverifiable_thresholds,
        "scorer": status.scorer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::smart_cluster::CachedAnchorVector;

    #[test]
    fn the_status_payload_keeps_the_two_fields_the_ui_reads() {
        // `SmartClustersView` polls `is_running` / `is_force_running` to decide
        // whether to offer "run now" or "stop" — and gates both on
        // `pending_count > 0`, so dropping the queue depth would leave a stop
        // button that never appears.
        let state = Arc::new(SmartClusterWorkerState::default());
        let value = status_value(&state, 4);
        assert_eq!(value["status"], "success");
        assert_eq!(value["is_running"], false);
        assert_eq!(value["is_force_running"], false);
        assert_eq!(value["pending_count"], 4);
        assert_eq!(value["backend"], "rust");
        assert_eq!(value["scorer"]["provider"], "cpu");
    }

    #[test]
    fn a_drain_request_is_consumed_exactly_once() {
        // The loop polls for the flag; leaving it set would restart the forced
        // pass on every tick after the first.
        let state = SmartClusterWorkerState::default();
        assert!(!state.take_drain_request());
        state.request_drain_now();
        assert!(state.take_drain_request());
        assert!(!state.take_drain_request());
    }

    #[test]
    fn asking_to_drain_clears_a_stale_abort() {
        // Otherwise an abort from the previous run would cancel the next one
        // before it started.
        let state = SmartClusterWorkerState::default();
        state.request_stop_drain();
        assert!(state.aborted());
        state.request_drain_now();
        assert!(!state.aborted());
    }

    #[test]
    fn the_threshold_formula_matches_the_calibration_ui() {
        // base = min(positive) * 0.85 when no negative is supplied.
        let threshold = compute_threshold(&[4.0, 6.0], &[]).unwrap();
        assert!((threshold - 3.4).abs() < 1e-9, "{threshold}");

        // With negatives it is max(base, max(negative) * 1.05).
        let threshold = compute_threshold(&[4.0], &[3.5]).unwrap();
        assert!((threshold - 3.675).abs() < 1e-9, "{threshold}");

        // A weak negative leaves base standing.
        let threshold = compute_threshold(&[4.0], &[1.0]).unwrap();
        assert!((threshold - 3.4).abs() < 1e-9, "{threshold}");

        // Raw logits are routinely negative, and the ported formula multiplies
        // the negative ceiling by 1.05, which lowers it. The outer max is what
        // keeps that from dragging the threshold below base.
        let threshold = compute_threshold(&[-1.0], &[-2.0]).unwrap();
        assert!((threshold - (-0.85)).abs() < 1e-9, "{threshold}");
    }

    #[test]
    fn a_cluster_with_no_positives_yields_no_threshold() {
        // The recalibration case: every positive example was deleted, so there
        // is nothing to derive a cutoff from and inventing one would silently
        // change which screenshots the cluster claims.
        assert!(compute_threshold(&[], &[1.0]).is_none());
        assert!(compute_threshold(&[], &[]).is_none());
        assert!(compute_threshold(&[f64::NAN], &[]).is_none());
    }

    #[test]
    fn the_prefilter_cutoff_and_batch_sizes_match_the_python_worker() {
        // These three constants decided every threshold now on disk. Changing
        // any of them changes which pairs are scored at all, which no stored
        // threshold accounts for.
        assert_eq!(PREFILTER_THRESHOLD, 0.40);
        assert_eq!(IDLE_BATCH, 32);
        assert_eq!(FORCED_BATCH, 128);
    }

    #[test]
    fn a_batch_that_stopped_part_way_still_reports_work_remaining() {
        // `more` is what makes a forced drain come back for another batch, and
        // an interruption by definition leaves the batch it was in unfinished.
        // Reporting "no more work" there would end the drain at the first
        // foreground query it met.
        let stopped = BatchProgress::stopped(FOREGROUND_QUERY_STOP);
        assert!(stopped.more);
        assert_eq!(stopped.stopped_because, Some(FOREGROUND_QUERY_STOP));

        let finished = BatchProgress::completed(false);
        assert!(!finished.more);
        assert!(finished.stopped_because.is_none());
    }

    #[test]
    fn the_reason_a_forced_drain_resumes_from_is_distinguishable() {
        // The drain waits on exactly one of `stand_down_reason`'s answers and
        // ends on the rest, and it tells them apart by value. Two reasons that
        // compared equal would make it either wait out a stop button or end on
        // a query it should have waited for.
        //
        // Compared against the constants the code actually returns, not against
        // copies written here: a test holding its own copy of a string is a test
        // that agrees with itself about a collision it cannot see.
        for other in [
            STOP_REQUESTED,
            FORCED_DEADLINE_REACHED,
            MAY_NOT_RUN,
            WAITED_OUT_BY_FOREGROUND,
        ] {
            assert_ne!(other, FOREGROUND_QUERY_STOP);
        }
    }

    #[test]
    fn a_forced_drain_cannot_wait_out_its_own_deadline() {
        // The wait budget has to leave time to actually score something, or the
        // button becomes a way to spend ten minutes doing nothing and then log
        // that the queue was deep. It also has to outlast one whole reranked
        // calibration query, or the common collision — search, then drain —
        // ends the drain rather than sequencing it.
        assert!(FORCED_FOREGROUND_WAIT_BUDGET < FORCED_DEADLINE);
        let worst_query = Duration::from_millis(1180)
            * (crate::rerank::MAX_RERANK_RESULTS * crate::rerank::RERANK_OVERFETCH);
        assert!(FORCED_FOREGROUND_WAIT_BUDGET > worst_query);
    }

    #[test]
    fn no_pass_in_this_module_holds_the_worker_through_a_foreground_query() {
        // Both modes run their cross-encoder calls under the background
        // contract, which is what bounds a foreground query's wait at one
        // document rather than at one commit group. A forced drain is still
        // different, but in scheduling — a bigger batch, no idle gate, and a
        // pass that waits and resumes — not in what it does to the worker while
        // somebody is blocked on it.
        //
        // This is the invariant `rerank.rs::FOREGROUND_RERANK_CHUNK` leans on
        // to justify fourteen inter-chunk gaps where there used to be one.
        assert_eq!(SCORING_PRIORITY, crate::rerank::RerankPriority::Background);
        assert_ne!(SCORING_PRIORITY, crate::rerank::RerankPriority::Foreground);
    }

    #[test]
    fn the_batch_shrinks_as_enabled_clusters_multiply() {
        assert_eq!(batch_size_for(false, 1), IDLE_BATCH);
        assert_eq!(batch_size_for(true, 1), FORCED_BATCH);
        // 4096 pairs over 200 clusters leaves room for 20 snapshots.
        assert_eq!(batch_size_for(true, 200), 20);
        // Never zero, however many clusters are enabled.
        assert!(batch_size_for(false, 100_000) >= 1);
    }

    #[test]
    fn a_commit_group_is_bounded_by_pairs_so_an_interruption_costs_the_same_either_way() {
        // What an interrupted pass repeats is a group, and what a group costs
        // is its cross-encoder pairs — so the snapshot count falls as clusters
        // multiply rather than the work per group rising with them.
        assert_eq!(commit_group_size(1), MAX_COMMIT_PAIRS as usize);
        assert_eq!(commit_group_size(2), 8);
        assert_eq!(commit_group_size(16), 1);
        // Never zero, which would make `chunks()` panic and the pass never run.
        assert_eq!(commit_group_size(0), MAX_COMMIT_PAIRS as usize);
        assert_eq!(commit_group_size(100_000), 1);
    }

    #[test]
    fn a_batch_is_committed_in_more_than_one_group() {
        // The point of the group: an idle batch that is interrupted part-way
        // must leave the groups it finished deleted from the queue. If a group
        // could span a whole batch, an interruption would put the pass back at
        // the same queue head — `peek_smart_cluster_pending_batch` orders by
        // `queued_at ASC` — and the queue would never advance on a machine
        // whose idle windows are shorter than a batch.
        for clusters in [1usize, 2, 3, 8, 64, 200] {
            let batch = batch_size_for(false, clusters) as usize;
            let group = commit_group_size(clusters);
            assert!(group >= 1, "clusters={clusters}");
            assert!(
                group < batch,
                "clusters={clusters} batch={batch} group={group}"
            );
        }
        // Concretely, at the shipped idle batch of 32 with one cluster enabled,
        // a pass commits twice rather than once.
        assert_eq!(batch_size_for(false, 1) as usize / commit_group_size(1), 2);
    }

    #[test]
    fn the_ownership_note_is_written_once_per_change() {
        // `rerank_runtime` has no settings control, so the states that need the
        // user to restart the monitor are only ever visible in the log. Once
        // per change, not once per minute.
        let state = SmartClusterWorkerState::default();
        assert_eq!(state.last_owner_note.load(Ordering::SeqCst), 0);
        state.note_queue_owner(SmartClusterQueueOwner::Rust);
        assert_eq!(state.last_owner_note.load(Ordering::SeqCst), 1);
        state.note_queue_owner(SmartClusterQueueOwner::Rust);
        assert_eq!(state.last_owner_note.load(Ordering::SeqCst), 1);
        state.note_queue_owner(SmartClusterQueueOwner::Python);
        assert_eq!(state.last_owner_note.load(Ordering::SeqCst), 2);
        state.note_queue_owner(SmartClusterQueueOwner::Neither);
        assert_eq!(state.last_owner_note.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cosine_of_normalized_vectors_is_their_dot_product() {
        let anchor = vec![0.6, 0.8];
        assert!((cosine(&anchor, &[0.6, 0.8]) - 1.0).abs() < 1e-6);
        assert!((cosine(&anchor, &[-0.6, -0.8]) + 1.0).abs() < 1e-6);
        // A dimension mismatch scores as "never a candidate" rather than
        // panicking or silently comparing a prefix.
        assert_eq!(cosine(&anchor, &[1.0]), f32::NEG_INFINITY);
    }

    #[test]
    fn prefilter_keeps_only_documented_subjects_with_a_close_enough_vector() {
        let anchor = vec![1.0, 0.0];
        let documents = HashMap::from([
            (1, "close".to_string()),
            (2, "far".to_string()),
            (3, "no vector".to_string()),
        ]);
        let vectors = HashMap::from([
            (1, vec![0.9, 0.436]),
            (2, vec![0.1, 0.995]),
            // 3 is deliberately absent: its embedding was deleted or
            // invalidated after the queue entry was written.
            (4, vec![1.0, 0.0]),
        ]);
        // 4 has a vector but no document, so it is not in this batch at all.
        assert_eq!(prefilter(&anchor, &vectors, &documents), vec![1]);
    }

    #[test]
    fn a_stored_threshold_from_the_retired_scorer_is_not_silently_reused() {
        // Every threshold written before step 6 has empty provenance columns,
        // which must never satisfy the current scorer — that is what routes it
        // into re-derivation instead of into a comparison.
        let scorer = ScorerIdentity::current();
        let legacy = SmartClusterScorer {
            model_id: String::new(),
            model_revision: String::new(),
            variant: String::new(),
            provider: String::new(),
        };
        assert!(!scorer.matches_stored(
            Some(&legacy.model_id),
            Some(&legacy.model_revision),
            Some(&legacy.variant),
            Some(&legacy.provider),
        ));
    }

    /// A target with no provenance, i.e. one that will be routed into
    /// re-derivation.
    fn legacy_target(id: i64, rederive_failed_scorer: Option<String>) -> SmartClusterScoringTarget {
        SmartClusterScoringTarget {
            id,
            anchor_text: "receipts".to_string(),
            threshold: 1.0,
            scorer: SmartClusterScorer {
                model_id: String::new(),
                model_revision: String::new(),
                variant: String::new(),
                provider: String::new(),
            },
            scorer_recorded: false,
            rederive_failed_scorer,
            anchor_vector: None,
        }
    }

    #[test]
    fn a_recorded_verdict_stops_the_re_derivation_from_being_attempted_again() {
        // The expensive half of a failed re-derivation is the 570 MB
        // cross-encoder load, and a cluster whose positive examples were
        // deleted fails identically every time. `resolve_thresholds` reaches
        // `rederive_threshold` only when neither test below holds, so these two
        // are what keep an idle machine from reloading the model once a minute
        // forever.
        let fingerprint = ScorerIdentity::current().fingerprint();
        let given_up = legacy_target(1, Some(fingerprint.clone()));
        assert_eq!(
            given_up.rederive_failed_scorer.as_deref(),
            Some(fingerprint.as_str())
        );

        // A verdict recorded under a different scorer is not this build's
        // verdict, so the cluster gets its one attempt here.
        let other_scorer =
            legacy_target(2, Some("bge-reranker-v2-m3|r1|uint8|directml".to_string()));
        assert_ne!(
            other_scorer.rederive_failed_scorer.as_deref(),
            Some(fingerprint.as_str())
        );

        // And a cluster that has never been given up on always gets one.
        assert_eq!(legacy_target(3, None).rederive_failed_scorer, None);
    }

    #[test]
    fn the_fingerprint_distinguishes_every_field_that_moves_the_logits() {
        let current = ScorerIdentity::current();
        let mut directml = current.clone();
        directml.provider = "directml".to_string();
        assert_ne!(current.fingerprint(), directml.fingerprint());

        let mut other_revision = current.clone();
        other_revision.model_revision = format!("{}-next", current.model_revision);
        assert_ne!(current.fingerprint(), other_revision.fingerprint());
    }

    #[test]
    fn only_positive_examples_can_produce_a_threshold() {
        // `rederive_threshold` short-circuits on exactly this before it reranks
        // anything, which is what makes the give-up cheap rather than a model
        // load followed by `compute_threshold` returning None.
        assert!(compute_threshold(&[], &[-2.0, -3.0]).is_none());
        assert!(compute_threshold(&[-1.0], &[-2.0]).is_some());
    }

    fn cached_target(cached: CachedAnchorVector) -> SmartClusterScoringTarget {
        let mut target = legacy_target(1, None);
        target.anchor_vector = Some(cached);
        target
    }

    fn fresh_cache(anchor_text: &str) -> CachedAnchorVector {
        let descriptor = crate::semantic_models::descriptor(MlSemanticModel::MinilmL12);
        CachedAnchorVector {
            vector: vec![0.6, 0.8],
            source_hash: anchor_text_hash(anchor_text),
            model_id: descriptor.model_id.to_string(),
            model_revision: descriptor.revision.to_string(),
        }
    }

    #[test]
    fn a_cached_anchor_is_reused_only_when_re_encoding_would_reproduce_it() {
        let descriptor = crate::semantic_models::descriptor(MlSemanticModel::MinilmL12);
        // `legacy_target` anchors on "receipts", which is what the cache below
        // was built from, so this is the steady-state hit that keeps MiniLM out
        // of the scoring path entirely.
        let target = cached_target(fresh_cache("receipts"));
        let hash = anchor_text_hash(&target.anchor_text);
        assert_eq!(
            reusable_anchor_vector(&target, &hash, descriptor),
            Some([0.6f32, 0.8].as_slice())
        );

        // An edited anchor. Reusing here would prefilter today's snapshots
        // against the description the user replaced, and nothing downstream
        // would notice.
        let edited = cached_target(fresh_cache("invoices"));
        assert!(reusable_anchor_vector(&edited, &hash, descriptor).is_none());

        // A build whose bi-encoder moved under a vector that was correct when
        // it was written.
        let mut other_model = fresh_cache("receipts");
        other_model.model_revision = format!("{}-next", descriptor.revision);
        assert!(reusable_anchor_vector(&cached_target(other_model), &hash, descriptor).is_none());

        let mut other_id = fresh_cache("receipts");
        other_id.model_id = "some-other-encoder".to_string();
        assert!(reusable_anchor_vector(&cached_target(other_id), &hash, descriptor).is_none());

        // A cluster that has never been encoded is a cold cache, not an error.
        assert!(reusable_anchor_vector(&legacy_target(1, None), &hash, descriptor).is_none());
    }

    #[test]
    fn an_anchor_hash_tracks_the_text_it_was_taken_from() {
        assert_eq!(anchor_text_hash("receipts"), anchor_text_hash("receipts"));
        assert_ne!(anchor_text_hash("receipts"), anchor_text_hash("receipt"));
        // Whitespace is part of the text the encoder sees, so it is part of the
        // identity too.
        assert_ne!(anchor_text_hash("receipts"), anchor_text_hash("receipts "));
    }

    #[test]
    fn an_interrupted_threshold_resolution_is_not_a_verdict_on_the_whole_set() {
        // The field exists so `run_batch` can tell "these are the usable
        // clusters" from "these are the clusters I got to before stopping".
        // Scoring against the second and then deleting the queue entries would
        // silently cost every cluster the resolution had not reached yet its
        // view of those screenshots — and unlike a skipped batch, nothing would
        // ever bring them back.
        let complete = ThresholdResolution {
            usable: vec![legacy_target(1, None)],
            unverifiable: 0,
            retryable: 0,
            interrupted: None,
        };
        assert!(complete.interrupted.is_none());

        let stopped_early = ThresholdResolution {
            usable: vec![legacy_target(1, None)],
            unverifiable: 0,
            retryable: 0,
            interrupted: Some(FOREGROUND_QUERY_STOP),
        };
        // Same non-empty `usable`, opposite handling.
        assert!(stopped_early.interrupted.is_some());
        assert_eq!(stopped_early.usable.len(), complete.usable.len());
        // And the reason travels with it, because a forced drain resumes from
        // this one and ends on the others.
        assert_eq!(stopped_early.interrupted, Some(FOREGROUND_QUERY_STOP));
    }
}
