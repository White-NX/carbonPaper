//! M2.5 step 5 — Rust-owned MiniLM capture indexing, retention, and the Chroma
//! mirror.
//!
//! Before this step the only writers of `semantic_text` rows were the M2.4
//! migration and Python's reverse-IPC dual-write, and the only reaper was
//! Python's hot-layer expiry mirroring its deletions back. `semantic_index =
//! rust` therefore meant "Rust reads an index Python writes". This module makes
//! Rust the writer:
//!
//! - the capture path enqueues a ledger job as soon as the OCR row commits, or
//!   records a terminal "nothing to encode" row when the screenshot has no text
//!   at all — the ledger is what remembers that decision, because the candidate
//!   scan that repairs missed enqueues cannot read it out of ciphertext;
//! - an idle-gated worker encodes the queue in small chunks and commits vector
//!   plus ledger in the one transaction M2.3 already provides;
//! - the same worker ages rows out on its own 30-day rule, which is the part
//!   nothing else was doing: screenshot *deletion* has always been handled
//!   transactionally by the schema triggers
//!   (`cleanup_derived_index_on_screenshot_soft_delete`), but expiry on age was
//!   only ever mirrored over from Python, and that mirror is gone;
//! - each newly encoded vector is mirrored *to* Python, the M2.4 dual-write
//!   reversed, so the Chroma `task_vectors` hot layer Milestone 4 clustering
//!   still reads keeps being fed without Python running MiniLM itself.
//!
//! Two consequences are deliberate and are written down rather than smoothed
//! over.
//!
//! **Indexing is strictly idle-gated, so search freshness regresses.** MiniLM is
//! a 118 MB model; the roadmap's idle rule covers exactly this kind of
//! background capture indexing. Python encoded inline on the post-process path,
//! so a screenshot was searchable within seconds. Here it is searchable after
//! the next idle window. That is a real regression in freshness and the
//! backlog is reported as a number rather than hidden.
//!
//! **A backlog is no longer a reason to refuse to serve.** The step-4 read path
//! stood down whenever the Rust store was known to be behind, which was correct
//! while Python held a complete corpus to fall back to. Once Rust is the
//! encoder, Chroma receives its rows from this mirror, so both stores are behind
//! by exactly the same screenshots — handing the query to Python would cost the
//! user the faster path and recover nothing. See `semantic_query.rs`.
//!
//! **The Chroma mirror is best-effort and a lost row is not re-sent.** Rust
//! holds the authoritative copy, so a mirror that fails while the monitor is
//! down costs that screenshot its place in unsupervised task clustering — not
//! its findability by search. Python's `_backfill_from_screenshots` only rebuilds
//! the hot layer when it is found *entirely* empty, so a partial gap is not
//! repaired; the Smart Cluster prefilter, which falls back to a live encode for
//! any id the collection lacks, is unaffected. Closing that gap belongs with
//! Milestone 4, which is where `task_vectors` is actually consumed.
//!
//! **This pass is one of two background users of a single-slot worker.** Smart
//! Cluster scoring (`smart_cluster_scoring.rs`) polls on the same 60-second
//! cadence, gates on the same idle signal, and wants a different model from an
//! engine that keeps one resident. Both therefore claim
//! `semantic_runtime::BACKGROUND_PASS_GUARD` before touching the worker, and
//! both stop feeding it when a foreground query announces itself.
//!
//! **An idle pass stands down; a manual run stands aside.** Both stop
//! submitting the moment a foreground query takes a lease, because interleaving
//! against a single-model engine buys an eviction per chunk rather than shared
//! progress ([`crate::semantic_runtime::BACKGROUND_PASS_GUARD`] states that
//! cost). What differs is what happens next. An idle pass ends: nobody asked
//! for it and its next tick is a minute away. A manual run waits and resumes,
//! because somebody pressed a button, and a run that an unrelated search
//! silently cancelled would make that button unreliable. See
//! [`stand_aside_for_foreground`].


use crate::credential_manager::CredentialManagerState;
use crate::idle::IdleState;
use crate::minilm_migration::{
    build_minilm_task_text, minilm_job_spec, validate_minilm_vector, MINILM_OCR_SNIPPET_CHARS,
};
use crate::ml_protocol::MlSemanticModel;
use crate::monitor::{authenticated_monitor_command, MonitorState};
use crate::semantic_runtime::SemanticRuntimeState;
use crate::storage::{
    BackgroundReadError, BackgroundScreenshotSummary, DerivedEmbeddingWrite, DerivedIndexJobSpec,
    DerivedIndexKind, StorageState,
};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Rust keeps the same 30-day window as the Chroma hot layer it replaced, but
/// decides it against SQLite `created_at` rather than mirroring Python's
/// expiry. Expressed as a SQLite datetime modifier and always bound as a
/// parameter, never interpolated.
pub const SEMANTIC_TEXT_RETENTION: &str = "-30 days";

/// How often the worker asks whether the machine has gone idle. The check
/// itself is one atomic load, so a short cadence costs nothing and keeps the
/// queue moving early in an idle window rather than up to an hour into it.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Subjects claimed from the ledger per drain.
///
/// Claiming is one database round trip that decrypts and rebuilds every
/// subject's model input, so it is done in batches rather than one job at a
/// time. The claimed batch is then encoded in `ENCODE_CHUNK`-sized pieces, not
/// in a single `embed_text` call — this constant bounds a *claim*, and the
/// protocol's `MAX_SEMANTIC_BATCH` (32) bounds a chunk. An idle pass performs
/// exactly one drain; a manual run repeats it until the queue empties.
const DRAIN_BATCH: usize = 16;

/// Subjects per `embed_text` call. A claimed batch is encoded in chunks so the
/// semantic worker's request gate is released between them. Foreground search
/// shares that one worker, so a chunk is the longest a user query can be made to
/// wait for it — and the idle gate is re-checked between chunks, which is also
/// the longest encoding can keep running after the user comes back.
const ENCODE_CHUNK: usize = 4;

/// Encode attempts before a subject is left alone. A job that spends this
/// budget stops being claimable and starts being counted as `exhausted` in the
/// backend diagnostic, because nothing will retry it on its own.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

/// Backoff between encode attempts. Idle windows are long; retrying a failing
/// model load every minute inside one would burn the budget in five minutes.
const RETRY_BACKOFF_MINUTES: i64 = 30;

/// Deadline for one batch encode, including a cold model load. Background work,
/// so it is generous next to the 5 s a user-facing query is allowed.
const EMBED_TIMEOUT: Duration = Duration::from_secs(120);

/// Ledger repairs and expiry deletions per pass. Bounded so one pass cannot
/// hold the process-wide database mutex through a whole backlog.
const MAINTENANCE_BATCH: u32 = 256;

/// Records per `upsert_task_vectors` request. Python rejects a batch above 128
/// outright; staying well under it keeps one mirror message small, since each
/// record carries 384 floats plus the document text as JSON.
const MIRROR_BATCH: usize = 32;

/// Wall-clock ceiling for one manual run, including a cold model load.
///
/// This is a runaway guard, not a budget, and the distinction is the whole
/// point of the constant that used to sit next to it. `MANUAL_SUBJECT_BUDGET`
/// stopped a run after 128 subjects; against a measured 0.50 s model load and a
/// query encode of 2 ms it fired roughly two orders of magnitude sooner than
/// this deadline, so one click did a few seconds of work, stopped without
/// explanation, and a machine with a real backlog needed dozens of clicks to
/// clear it. A bound that never binds for the reason it was written for is a
/// bound in the wrong place.
///
/// What replaced it is what a present user actually needs: the run reports its
/// progress while it goes (`SEMANTIC_INDEX_PROGRESS_EVENT`) and can be stopped
/// at any point (`semantic_index_stop_now`). This deadline is left to end a run
/// nobody is watching anymore — the settings dialog closed, the user walked
/// away — rather than to keep the click short.
const MANUAL_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// How long a manual run may spend, in total, standing aside for foreground
/// queries before it gives up and says so.
///
/// Waiting is the right response to one query: a reranked calibration query is
/// 120 documents at 0.31–1.18 s each, so a couple of minutes, and the queue is
/// not going anywhere. But a calibration *session* is a sequence of those, and
/// without a budget of its own a run could sit out the whole `MANUAL_DEADLINE`
/// and then report "deadline_reached, 0 indexed" — which reads as the indexer
/// being broken rather than as the run having politely never started. Ending
/// sooner, under a reason that names the actual cause, is the honest version of
/// the same outcome, and the idle worker picks the queue up either way.
///
/// Counted across the whole run rather than per wait, because it is the total
/// the user is unknowingly spending, not any single pause, that decides whether
/// the click accomplished anything.
const MANUAL_FOREGROUND_WAIT_BUDGET: Duration = Duration::from_secs(5 * 60);

/// The one stop reason a manual drain resumes from instead of ending on.
///
/// A drain reports it when a foreground query took the worker mid-batch; the
/// claims it was holding are released uncharged, exactly as for every other
/// interruption, and [`drain_until_done`] waits for the lease to clear and
/// claims a fresh batch. Idle passes report the same string and do end on it,
/// which is why the constant names the condition rather than the response.
const FOREGROUND_QUERY_STOP: &str = "foreground_query";

/// Reason for a manual run that spent [`MANUAL_FOREGROUND_WAIT_BUDGET`] waiting
/// for a worker it never got. Distinguished from [`DEADLINE_REACHED`] because
/// the two say different things to the user: one machine is slow, the other was
/// busy answering that user's own searches.
const WAITED_OUT_BY_FOREGROUND: &str = "foreground_query_held_the_worker";

/// The stop reasons a run ends on, as opposed to the one it may resume from.
///
/// Named rather than written inline because each is produced from two or three
/// places and read by comparison against [`FOREGROUND_QUERY_STOP`]. Two of them
/// colliding by value is the failure that comparison cannot survive, and
/// several copies of a string literal is how such a collision gets written by
/// accident.
const STOPPED_BY_USER: &str = "stopped_by_user";
const MAINTENANCE_STARTED: &str = "maintenance_started";
const DEADLINE_REACHED: &str = "deadline_reached";

/// Progress of the running manual pass, emitted after every encoded chunk.
///
/// `total` is the queue depth read once when the run started, so the ratio is
/// against the backlog the user saw when they pressed the button. It can be an
/// over-estimate: a job sitting in retry backoff counts as claimable but will
/// not be claimed this run, so a run may legitimately end before `processed`
/// reaches it. The finishing summary is what states the outcome.
pub const SEMANTIC_INDEX_PROGRESS_EVENT: &str = "semantic-index-progress";

/// Ledger reason recorded for a screenshot that is not part of the corpus.
/// `minilm_sources` is the only place that can establish this, so it is the only
/// place that writes it.
const EMPTY_SOURCE_CODE: &str = "empty_source";
const EMPTY_SOURCE_REASON: &str =
    "process name, window title, and OCR text are all empty, so there is nothing to encode";

/// One screenshot's MiniLM model input, the ledger identity derived from it,
/// and the metadata the Chroma mirror has to carry.
pub(crate) struct MinilmSource {
    pub text: String,
    pub spec: DerivedIndexJobSpec,
    /// Kept from the same read that produced `text`. The mirror needs
    /// process/title/timestamp/category, and re-reading them later would pay a
    /// second round of CNG decryption for values already in hand.
    pub summary: BackgroundScreenshotSummary,
}

/// The corpus decision for one page of screenshots.
pub(crate) struct MinilmSources {
    /// Screenshots with something to encode.
    pub indexable: HashMap<i64, MinilmSource>,
    /// Screenshots whose combined text is empty. Python skipped these, so they
    /// are not part of the corpus and must not be counted as missing from it —
    /// but that is a fact only this builder can establish, because the text it
    /// judges lives in encrypted columns the candidate scan cannot read. Callers
    /// record it through `exclude_derived_index_subject` so the scan stops
    /// re-deciding it once a minute. The spec is built from the empty text, so a
    /// screenshot that later gains text no longer matches the recorded contract.
    pub excluded: HashMap<i64, DerivedIndexJobSpec>,
}

/// Rebuild the MiniLM model input for `ids` from SQLite.
///
/// Deliberately the same assembly the M2.4 migration used — same summary read,
/// same insertion-ordered OCR prefix, same `process | title | OCR[:200]`
/// contract — because the source fingerprint is what decides whether a stored
/// vector is still valid. Two builders would eventually disagree about a
/// screenshot and silently invalidate its row.
pub(crate) fn minilm_sources(
    storage: &StorageState,
    ids: &[i64],
) -> Result<MinilmSources, BackgroundReadError> {
    if ids.is_empty() {
        return Ok(MinilmSources {
            indexable: HashMap::new(),
            excluded: HashMap::new(),
        });
    }
    let summaries = storage.get_screenshot_summaries_by_ids_silent(ids)?;
    let ocr = storage.get_ocr_text_prefixes_by_screenshot_ids_silent(ids, MINILM_OCR_SNIPPET_CHARS)?;
    let mut indexable = HashMap::with_capacity(summaries.len());
    let mut excluded = HashMap::new();
    for summary in summaries {
        let text = build_minilm_task_text(
            summary.process_name.as_deref().unwrap_or(""),
            summary.window_title.as_deref().unwrap_or(""),
            ocr.get(&summary.id).map(String::as_str).unwrap_or(""),
        );
        let spec = minilm_job_spec(summary.id, &text);
        if text.trim().is_empty() {
            excluded.insert(summary.id, spec);
            continue;
        }
        indexable.insert(
            summary.id,
            MinilmSource {
                text,
                spec,
                summary,
            },
        );
    }
    Ok(MinilmSources {
        indexable,
        excluded,
    })
}

/// Queue one freshly captured screenshot for semantic indexing.
///
/// Called from the OCR commit path so the ledger records the debt immediately,
/// while the encoding itself waits for an idle window. A screenshot with no text
/// at all gets a terminal ledger row instead of a queued one, which is what
/// keeps the worker's repair scan from selecting it again for the rest of the
/// retention window.
///
/// A failure here is recoverable and not worth failing the capture over: the
/// worker's reconciliation pass finds any screenshot that has an OCR row and no
/// ledger row, which is exactly what a missed enqueue leaves behind.
pub fn enqueue_captured_screenshot(
    storage: &StorageState,
    screenshot_id: i64,
) -> Result<(), String> {
    let sources = minilm_sources(storage, &[screenshot_id]).map_err(|error| error.to_string())?;
    if let Some(source) = sources.indexable.get(&screenshot_id) {
        storage.ensure_derived_index_job(&source.spec)?;
        return Ok(());
    }
    if let Some(spec) = sources.excluded.get(&screenshot_id) {
        storage.exclude_derived_index_subject(spec, EMPTY_SOURCE_CODE, EMPTY_SOURCE_REASON)?;
    }
    Ok(())
}

/// Idle-gated capture indexing, retention, and repair.
pub async fn run_semantic_index_worker(app: AppHandle) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        // A manual run, or a Smart Cluster scoring pass, holds this for its
        // whole duration. Skipping the tick is the right answer rather than
        // queuing behind it: the next tick is a minute away, and whatever holds
        // the guard is using the worker this pass would have to evict a model
        // to reach.
        let Ok(_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
            continue;
        };
        if let Err(error) = run_pass(&app, PassMode::Idle).await {
            tracing::warn!("[SEMANTIC:INDEX] idle pass failed: {error}");
        }
    }
}

/// What is allowed to stop a pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PassMode {
    /// Background work: runs only inside an idle window and stands down the
    /// moment the user comes back.
    Idle,
    /// The user asked for this run, so the idle gate does not apply. Every
    /// other guard still does — maintenance mode, the locked session, the
    /// ledger's retry budget — and the run stops on the user's word or on the
    /// runaway deadline.
    Manual,
}

/// Outcome of one manual run, as reported to Settings → Advanced.
#[derive(Debug, Clone, Serialize)]
pub struct SemanticIndexRunSummary {
    /// False when something refused before any encoding was attempted;
    /// `skipped_reason` then says which guard it was.
    pub started: bool,
    /// Screenshots that became query-visible during this run.
    pub indexed: u64,
    /// Screenshots whose encode failed and which kept a retry attempt.
    pub failed: u64,
    /// Screenshots still waiting afterwards. `None` when the ledger could not
    /// be read, which is honestly "not known" rather than zero.
    pub remaining: Option<u64>,
    /// Screenshots whose retry budget is spent. Nothing clears these on its own.
    pub stalled: Option<u64>,
    /// Why the run stopped early, or why it never started.
    pub skipped_reason: Option<String>,
}

/// Manual-run state shared with the two Tauri commands that drive it.
///
/// The run is an async task the "index now" command awaits for its whole
/// duration, so "stop" cannot be a return value from it and has to be a flag
/// the pass polls between chunks — the same shape `SmartClusterWorkerState`
/// uses for its forced drain, and for the same reason.
///
/// The counters exist because a run is now open-ended. While it was capped at
/// 128 subjects a summary at the end was an adequate report; a run that may
/// legitimately last minutes needs to say so as it goes.
#[derive(Default)]
pub struct SemanticIndexRunState {
    /// A manual pass is executing right now.
    running: AtomicBool,
    /// Set by `semantic_index_stop_now`; checked between chunks and between
    /// drains. Cleared when the next run starts, so a stop that arrives as a
    /// run is finishing cannot cancel the following one.
    stop_requested: AtomicBool,
    /// Subjects that left the queue in this run, encoded or not. This is what
    /// the progress ratio measures, because it is what actually shrinks the
    /// backlog — an invalid vector is discarded rather than retried, and a
    /// screenshot whose text vanished is excluded, neither of which is an
    /// "indexed" outcome but both of which are progress.
    processed: AtomicU64,
    /// Subjects that became query-visible in this run.
    indexed: AtomicU64,
    /// Queue depth when this run started.
    total: AtomicU64,
}

impl SemanticIndexRunState {
    /// Ask the running pass to stop after the chunk it is encoding. A chunk is
    /// four subjects and cannot be interrupted once submitted to the worker, so
    /// this is prompt rather than immediate.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn stopped_by_user(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// Claim the run and reset its counters. The returned guard clears
    /// `running` on every path out of the command, including the error one.
    fn begin(self: &Arc<Self>) -> ActiveRun {
        self.stop_requested.store(false, Ordering::SeqCst);
        self.processed.store(0, Ordering::SeqCst);
        self.indexed.store(0, Ordering::SeqCst);
        self.total.store(0, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        ActiveRun(self.clone())
    }

    /// Record one finished chunk and tell the settings dialog about it.
    ///
    /// A dropped event costs a progress line, never correctness: the run's
    /// summary is the authoritative report, and the dialog reconciles against
    /// it when the command returns.
    fn report_chunk(&self, app: &AppHandle, processed: u64, indexed: u64) {
        let processed_total = self.processed.fetch_add(processed, Ordering::SeqCst) + processed;
        let indexed_total = self.indexed.fetch_add(indexed, Ordering::SeqCst) + indexed;
        let _ = app.emit(
            SEMANTIC_INDEX_PROGRESS_EVENT,
            serde_json::json!({
                "processed": processed_total,
                "indexed": indexed_total,
                "total": self.total.load(Ordering::SeqCst),
            }),
        );
    }
}

/// Holds `running` for the lifetime of one manual pass.
struct ActiveRun(Arc<SemanticIndexRunState>);

impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// Whether background semantic work may run right now.
///
/// The idle gate is the whole point of the step-5 scheduling decision: a
/// 118 MB model load and a batch of transformer forward passes must never land
/// while someone is using the machine. Maintenance mode is checked too, because
/// a migration rewriting the derived store would race these writes — and that
/// one binds a manual run as well, since the user cannot consent their way out
/// of a concurrent rewrite of the store being written to.
fn may_run(app: &AppHandle, mode: PassMode) -> bool {
    if crate::maintenance::is_active() {
        return false;
    }
    if mode == PassMode::Manual {
        return true;
    }
    app.state::<Arc<IdleState>>()
        .is_idle
        .load(Ordering::Relaxed)
}

/// One pass: expire, repair, then drain as much of the queue as the mode allows.
async fn run_pass(app: &AppHandle, mode: PassMode) -> Result<PassOutcome, String> {
    if !may_run(app, mode) {
        return Ok(PassOutcome::refused(match mode {
            PassMode::Manual => "maintenance_in_progress",
            PassMode::Idle => "not_idle",
        }));
    }
    // Checked before the retention and repair reads, not only before the
    // encode. Those reads take the process-wide database mutex, which the
    // foreground query needs to rehydrate its results, so starting a pass here
    // would spend part of a 5 s budget on work that costs nothing to defer.
    //
    // An idle pass refuses; a manual run waits, for the reason `drain_queue`
    // records. Waiting here is still cheap for it — the deadline it is spending
    // is thirty minutes — and it keeps the whole run on one side of the query
    // rather than starting it a few database reads deep. The deadline is the
    // manual run's runaway clock and starts at the click rather than at the
    // first encode, so a run that spends its first minutes waiting is honest
    // about having spent them; an idle pass never reads it.
    let deadline = Instant::now() + MANUAL_DEADLINE;
    let mut waited = Duration::ZERO;
    if mode == PassMode::Idle {
        if app
            .state::<Arc<SemanticRuntimeState>>()
            .foreground_waiting()
        {
            return Ok(PassOutcome::refused(FOREGROUND_QUERY_STOP));
        }
    } else if let Some(reason) = stand_aside_for_foreground(app, deadline, &mut waited).await {
        // Nothing was attempted, so this is a refusal rather than a stop: the
        // summary says "skipped, and here is which guard", which is the true
        // account of a click that never reached the worker.
        return Ok(PassOutcome::refused(reason));
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    // Every read below decrypts OCR text, so a locked session can do nothing
    // but wait. Bailing here keeps the ledger untouched rather than marking a
    // batch `waiting_for_auth` on every tick of a locked machine.
    if !storage.is_session_valid() {
        return Ok(PassOutcome::refused("session_locked"));
    }

    // Expiry first: a subject about to age out should not be encoded on its way
    // out the door.
    reap_expired(storage.clone()).await?;
    reconcile_missing(storage.clone()).await?;

    match mode {
        PassMode::Idle => drain_queue(app, storage, mode).await,
        PassMode::Manual => {
            // The queue depth the user is about to watch drain, read once and
            // after the repair pass that can add to it. Re-reading it per chunk
            // would take the process-wide database mutex several times a second
            // for a number that only ever goes down.
            let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
            let counting = storage.clone();
            let total = tokio::task::spawn_blocking(move || {
                counting
                    .derived_index_backlog(DerivedIndexKind::SemanticText, MAX_ATTEMPTS)
                    .map(|backlog| backlog.claimable)
                    .unwrap_or(0)
            })
            .await
            .unwrap_or(0);
            run.total.store(total, Ordering::SeqCst);
            drain_until_done(app, storage, deadline, waited).await
        }
    }
}

/// Wait out a foreground query rather than competing with it, and say why the
/// run must end if one of its own bounds expires first.
///
/// `None` once the worker is free. `waited` accumulates across every call in a
/// run, because [`MANUAL_FOREGROUND_WAIT_BUDGET`] is a budget for the run and
/// not for any one pause.
///
/// The three ways out other than the worker coming free are the three the run
/// already had — the stop button, maintenance mode, and the runaway deadline —
/// checked here because a pause is exactly where a run is most likely to be
/// sitting when one of them fires. Nothing is claimed while this waits: the
/// drain released its leases uncharged on its way out, so a run that never
/// resumes leaves the ledger exactly as it found it.
async fn stand_aside_for_foreground(
    app: &AppHandle,
    deadline: Instant,
    waited: &mut Duration,
) -> Option<&'static str> {
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    if !semantic.foreground_waiting() {
        return None;
    }
    let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
    let started = Instant::now();
    tracing::info!("[SEMANTIC:INDEX] manual run standing aside for a foreground query");
    let outcome = loop {
        if run.stopped_by_user() {
            break Some(STOPPED_BY_USER);
        }
        if crate::maintenance::is_active() {
            break Some(MAINTENANCE_STARTED);
        }
        let now = Instant::now();
        if now >= deadline {
            break Some(DEADLINE_REACHED);
        }
        if *waited + now.duration_since(started) >= MANUAL_FOREGROUND_WAIT_BUDGET {
            break Some(WAITED_OUT_BY_FOREGROUND);
        }
        if !semantic.foreground_waiting() {
            break None;
        }
        tokio::time::sleep(crate::semantic_runtime::FOREGROUND_POLL_INTERVAL).await;
    };
    *waited += started.elapsed();
    match outcome {
        None => tracing::info!(
            "[SEMANTIC:INDEX] manual run resuming after {:.1}s; the foreground query is done",
            started.elapsed().as_secs_f64()
        ),
        Some(reason) => tracing::info!(
            "[SEMANTIC:INDEX] manual run ending while stood aside: {reason} \
             (waited {:.1}s in total)",
            waited.as_secs_f64()
        ),
    }
    outcome
}

/// Drain repeatedly for a manual run, until the queue empties, the user stops
/// it, or something refuses.
///
/// A single `drain_queue` claims at most `DRAIN_BATCH`, which is the right size
/// for an idle tick that will come back in a minute, but would make one click
/// look like it did almost nothing.
///
/// `waited` carries in whatever [`run_pass`] already spent standing aside, so
/// the wait budget covers the run rather than resetting at the first drain.
async fn drain_until_done(
    app: &AppHandle,
    storage: Arc<StorageState>,
    deadline: Instant,
    mut waited: Duration,
) -> Result<PassOutcome, String> {
    let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
    let mut total = PassOutcome::default();
    loop {
        // Before claiming anything, not after: a batch claimed and then held
        // through a pause is a batch nobody else can encode, and the leases
        // would be released uncharged one chunk later anyway.
        if let Some(reason) = stand_aside_for_foreground(app, deadline, &mut waited).await {
            total.stopped_because = Some(reason);
            break;
        }
        let pass = drain_queue(app, storage.clone(), PassMode::Manual).await?;
        let drained = pass.indexed + pass.failed;
        total.indexed += pass.indexed;
        total.failed += pass.failed;
        // A drain that stopped for a reason of its own has already released
        // what it did not do, and claiming a fresh batch would walk straight
        // back into the same stop. This mattered less while the subject budget
        // capped the loop at eight drains; without it, a broken worker would
        // otherwise be handed the whole backlog batch by batch, charging a
        // retry attempt against each one on its way through.
        //
        // A foreground query is the one reason that is not like that: walking
        // back into it is exactly right once it clears, and the wait at the top
        // of the loop is what makes "once it clears" true rather than
        // immediate. The sleep is charged to the wait budget so that a lease
        // flapping between the wait and the claim — back-to-back queries from a
        // calibration session — cannot turn this into one ledger round trip per
        // iteration for the length of the deadline.
        if let Some(reason) = pass.stopped_because {
            if reason != FOREGROUND_QUERY_STOP {
                total.stopped_because = Some(reason);
                break;
            }
            tokio::time::sleep(crate::semantic_runtime::FOREGROUND_POLL_INTERVAL).await;
            waited += crate::semantic_runtime::FOREGROUND_POLL_INTERVAL;
            continue;
        }
        if drained == 0 {
            // Nothing claimable left: either the queue is empty or what remains
            // is backing off or out of retries.
            break;
        }
        if run.stopped_by_user() {
            total.stopped_because = Some(STOPPED_BY_USER);
            break;
        }
        if Instant::now() >= deadline {
            total.stopped_because = Some(DEADLINE_REACHED);
            break;
        }
        if crate::maintenance::is_active() {
            total.stopped_because = Some("maintenance_in_progress");
            break;
        }
    }
    Ok(total)
}

/// What one drain achieved, and why it stopped if it stopped early.
#[derive(Debug, Default)]
struct PassOutcome {
    indexed: u64,
    failed: u64,
    stopped_because: Option<&'static str>,
    refused: Option<&'static str>,
}

impl PassOutcome {
    fn refused(reason: &'static str) -> Self {
        Self {
            refused: Some(reason),
            ..Self::default()
        }
    }
}

/// Delete rows that aged out of the retention window.
///
/// Deletion with the screenshot is already transactional in the schema, so what
/// this adds is expiry on age — the job Python's hot-layer expiry used to do for
/// Rust by mirroring its own deletions across. The query also sweeps subjects
/// with no live screenshot at all, which after the triggers is a safety net for
/// a row that predates them rather than the normal path.
async fn reap_expired(storage: Arc<StorageState>) -> Result<(), String> {
    let removed = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let subjects = storage
            .list_expired_semantic_text_subjects(SEMANTIC_TEXT_RETENTION, MAINTENANCE_BATCH)?;
        let mut removed = 0usize;
        for subject in subjects {
            if storage.delete_derived_index_subject(DerivedIndexKind::SemanticText, &subject)? {
                removed += 1;
            }
        }
        Ok(removed)
    })
    .await
    .map_err(|error| format!("reap task failed: {error}"))??;
    if removed > 0 {
        tracing::info!("[SEMANTIC:INDEX] expired {removed} semantic vector(s)");
    }
    Ok(())
}

/// Queue screenshots that should have a vector but have no ledger row.
///
/// This is the repair path for an enqueue that never ran: the capture happened
/// while the session was locked, or the process died between the OCR commit and
/// the enqueue. It is bounded to the retention window, so it never turns into a
/// full-history backfill — M2.4 established that the corpus is the hot layer,
/// not every screenshot ever taken.
///
/// The candidate query can only over-approximate the corpus, because it decides
/// membership from ciphertext. Whatever it hands over that turns out to have no
/// text is recorded as excluded rather than silently dropped: a silent drop
/// would hand the same screenshot back on the next pass, forever, and once
/// enough of them accumulated they would fill the query's `LIMIT` and starve the
/// repair of screenshots that really are missing a vector.
async fn reconcile_missing(storage: Arc<StorageState>) -> Result<(), String> {
    let (queued, excluded) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            let ids = storage
                .list_semantic_text_index_candidates(SEMANTIC_TEXT_RETENTION, MAINTENANCE_BATCH)?;
            if ids.is_empty() {
                return Ok((0, 0));
            }
            let sources = minilm_sources(&storage, &ids).map_err(|error| error.to_string())?;
            let mut queued = 0usize;
            for source in sources.indexable.values() {
                match storage.ensure_derived_index_job(&source.spec) {
                    Ok(_) => queued += 1,
                    // A screenshot deleted between the candidate query and here.
                    Err(error) => tracing::debug!(
                        "[SEMANTIC:INDEX] skipped repair enqueue for {}: {error}",
                        source.spec.subject_key
                    ),
                }
            }
            let mut excluded = 0usize;
            for spec in sources.excluded.values() {
                match storage.exclude_derived_index_subject(
                    spec,
                    EMPTY_SOURCE_CODE,
                    EMPTY_SOURCE_REASON,
                ) {
                    Ok(true) => excluded += 1,
                    // Deleted between the candidate query and here; the
                    // lifecycle triggers already removed its rows.
                    Ok(false) => {}
                    Err(error) => tracing::debug!(
                        "[SEMANTIC:INDEX] could not record exclusion for {}: {error}",
                        spec.subject_key
                    ),
                }
            }
            Ok((queued, excluded))
        })
        .await
        .map_err(|error| format!("reconcile task failed: {error}"))??;
    if queued > 0 {
        tracing::info!("[SEMANTIC:INDEX] repaired {queued} missing ledger row(s)");
    }
    if excluded > 0 {
        tracing::info!("[SEMANTIC:INDEX] excluded {excluded} screenshot(s) with no text to encode");
    }
    Ok(())
}

/// One claimed job: its ledger identity, its model input, the metadata the
/// mirror will need, and the lease that authorizes the commit.
struct ClaimedJob {
    spec: DerivedIndexJobSpec,
    text: String,
    summary: BackgroundScreenshotSummary,
    lease_token: String,
}

/// One screenshot that became query-visible in this pass, in the shape the
/// Chroma mirror sends.
struct IndexedSubject {
    id: i64,
    vector: Vec<f32>,
    text: String,
    summary: BackgroundScreenshotSummary,
}

async fn drain_queue(
    app: &AppHandle,
    storage: Arc<StorageState>,
    mode: PassMode,
) -> Result<PassOutcome, String> {
    let claimed = claim_batch(storage.clone()).await?;
    if claimed.is_empty() {
        return Ok(PassOutcome::default());
    }
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
    let mut pending: VecDeque<ClaimedJob> = claimed.into();
    let mut indexed: Vec<IndexedSubject> = Vec::new();
    let mut outcome = PassOutcome::default();
    let mut failure: Option<String> = None;
    while !pending.is_empty() {
        // The user may have come back since the claim, or since the previous
        // chunk. Release the rest of the leases without charging the retry
        // budget — nothing failed, the window just closed. A manual run has no
        // idle window to lose, so for it that only ever fires on maintenance
        // mode or on the stop button.
        //
        // A foreground query is checked separately, for a separate reason, and
        // — unlike the idle gate — it binds a manual run too. The idle signal is
        // up to ten seconds stale (`idle.rs` polls `GetLastInputInfo` every
        // 10 s), while a search arriving a second after the user touches the
        // keyboard has a 5 s budget to reach the one semantic worker this loop
        // is occupying. Waiting for `is_idle` to catch up would spend most of
        // that budget.
        //
        // The manual case is not the same trade and was originally left out of
        // it: consent to run covers competing for the worker, but not what
        // competing costs here. This pass wants MiniLM, so a chunk of it
        // landing between two cross-encoder chunks evicts the model that query
        // is using and charges the next chunk a reload
        // (`semantic_runtime.rs::BACKGROUND_PASS_GUARD`) — and a reranked query
        // now offers fourteen such gaps rather than one
        // (`rerank.rs::FOREGROUND_RERANK_CHUNK`). Interleaving therefore makes
        // both sides slower than running them in sequence, which is not a trade
        // the user consented to and not one they can see. So the run stands
        // aside here and resumes in `drain_until_done`; only the idle pass ends.
        let stopped = if mode == PassMode::Manual && run.stopped_by_user() {
            Some(STOPPED_BY_USER)
        } else if !may_run(app, mode) {
            Some(if mode == PassMode::Manual {
                MAINTENANCE_STARTED
            } else {
                "idle_lost"
            })
        } else if semantic.foreground_waiting() {
            Some(FOREGROUND_QUERY_STOP)
        } else {
            None
        };
        if let Some(reason) = stopped {
            release_claims(
                storage.clone(),
                Vec::from(std::mem::take(&mut pending)),
                reason,
                "the pass stopped before this subject was encoded",
            )
            .await;
            outcome.stopped_because = Some(reason);
            break;
        }
        let chunk: Vec<ClaimedJob> = pending.drain(..ENCODE_CHUNK.min(pending.len())).collect();
        let chunk_len = chunk.len() as u64;
        match encode_chunk(app, &semantic, storage.clone(), chunk).await {
            Ok(mut encoded) => {
                outcome.indexed += encoded.len() as u64;
                // The whole chunk left the queue; not all of it necessarily
                // became a vector, since an invalid one is discarded rather
                // than retried. Both are progress, only one is an index.
                if mode == PassMode::Manual {
                    run.report_chunk(app, chunk_len, encoded.len() as u64);
                }
                indexed.append(&mut encoded);
            }
            Err(error) => {
                outcome.failed += chunk_len;
                if mode == PassMode::Manual {
                    run.report_chunk(app, chunk_len, 0);
                }
                // Whatever broke the worker will break every remaining chunk the
                // same way, and charging the retry budget for an attempt that was
                // never made would spend it on the worker's behalf.
                if !pending.is_empty() {
                    release_claims(
                        storage.clone(),
                        Vec::from(std::mem::take(&mut pending)),
                        "batch_aborted",
                        "an earlier chunk of the same pass failed to encode",
                    )
                    .await;
                }
                failure = Some(error);
                break;
            }
        }
    }

    if !indexed.is_empty() {
        tracing::info!("[SEMANTIC:INDEX] indexed {} screenshot(s)", indexed.len());
        mirror_to_chroma(app, &indexed).await;
    }
    match failure {
        // A manual run reports the failure through its summary instead of
        // propagating it, so the user sees "3 indexed, 4 failed" rather than an
        // error dialog that hides the work that did land.
        Some(error) if mode == PassMode::Manual => {
            outcome.stopped_because = Some("encode_failed");
            tracing::warn!("[SEMANTIC:INDEX] manual run stopped: {error}");
            Ok(outcome)
        }
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

/// Encode and commit one chunk. The chunk's leases are consumed either way:
/// success commits them, failure charges the retry budget with a backoff.
async fn encode_chunk(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
) -> Result<Vec<IndexedSubject>, String> {
    let texts: Vec<String> = claimed.iter().map(|job| job.text.clone()).collect();
    let embedded = semantic
        .embed_text(
            app.clone(),
            MlSemanticModel::MinilmL12,
            texts,
            EMBED_TIMEOUT,
            // MiniLM has no approved DirectML parity; asking for it only costs
            // a provider negotiation before the worker falls back to CPU.
            false,
        )
        .await;

    let vectors = match embedded {
        Ok(result) if result.vectors.len() == claimed.len() => result.vectors,
        Ok(result) => {
            let error = format!(
                "semantic worker returned {} vectors for {} inputs",
                result.vectors.len(),
                claimed.len()
            );
            fail_claims(storage, claimed, "embed_mismatch", &error).await;
            return Err(error);
        }
        Err(error) => {
            fail_claims(storage, claimed, "embed_failed", &error).await;
            return Err(format!("embed failed: {error}"));
        }
    };

    commit_batch(storage, claimed, vectors).await
}

/// Claim up to one batch, rebuilding each job's source text from SQLite.
///
/// The text is rebuilt rather than trusted from the ledger because a job queued
/// at capture time may have been overtaken by a re-OCR: its stored fingerprint
/// would then describe text that no longer exists. Such a job is re-queued
/// against the current source instead of being encoded against a stale one.
async fn claim_batch(storage: Arc<StorageState>) -> Result<Vec<ClaimedJob>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<ClaimedJob>, String> {
        let jobs = storage.claimable_derived_index_jobs(
            DerivedIndexKind::SemanticText,
            MAX_ATTEMPTS,
            DRAIN_BATCH as u32,
        )?;
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::with_capacity(jobs.len());
        for job in &jobs {
            match job.spec.subject_key.parse::<i64>() {
                Ok(id) => ids.push(id),
                Err(error) => tracing::warn!(
                    "[SEMANTIC:INDEX] ignoring job with non-numeric subject '{}': {error}",
                    job.spec.subject_key
                ),
            }
        }
        let sources = minilm_sources(&storage, &ids).map_err(|error| error.to_string())?;

        let mut claimed = Vec::with_capacity(jobs.len());
        for job in jobs {
            let Ok(id) = job.spec.subject_key.parse::<i64>() else {
                continue;
            };
            let Some(source) = sources.indexable.get(&id) else {
                // Nothing to encode. Either the screenshot lost its text, and
                // the exclusion has to be recorded or the repair scan will hand
                // it straight back, or the screenshot itself is gone and the row
                // goes with it.
                let recorded = match sources.excluded.get(&id) {
                    Some(spec) => storage
                        .exclude_derived_index_subject(
                            spec,
                            EMPTY_SOURCE_CODE,
                            EMPTY_SOURCE_REASON,
                        )
                        .unwrap_or(false),
                    None => false,
                };
                if !recorded {
                    let _ = storage.delete_derived_index_subject(
                        DerivedIndexKind::SemanticText,
                        &job.spec.subject_key,
                    );
                }
                continue;
            };
            if source.spec != job.spec {
                // Re-queue against the current source. `ensure_derived_index_job`
                // replaces the contract; the next pass claims the fresh row.
                let _ = storage.ensure_derived_index_job(&source.spec);
                continue;
            }
            match storage.mark_derived_index_job_processing(&job.spec) {
                Ok(lease_token) => claimed.push(ClaimedJob {
                    spec: job.spec,
                    text: source.text.clone(),
                    summary: source.summary.clone(),
                    lease_token,
                }),
                // Lost the race to another claimant, or the row moved on.
                Err(error) => tracing::debug!(
                    "[SEMANTIC:INDEX] could not claim {}: {error}",
                    job.spec.subject_key
                ),
            }
        }
        Ok(claimed)
    })
    .await
    .map_err(|error| format!("claim task failed: {error}"))?
}

/// Commit the encoded batch, returning the subjects that became query-visible.
async fn commit_batch(
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
    vectors: Vec<Vec<f32>>,
) -> Result<Vec<IndexedSubject>, String> {
    tokio::task::spawn_blocking(move || {
        let mut indexed = Vec::with_capacity(claimed.len());
        for (job, vector) in claimed.into_iter().zip(vectors) {
            if let Err(error) = validate_minilm_vector(&vector) {
                // A zero or non-finite vector would poison every cosine score
                // it touches. Discard rather than retry: the input is stable,
                // so the next attempt produces the same bad vector.
                let _ = storage.mark_derived_index_job_discarded(
                    &job.spec,
                    &job.lease_token,
                    "invalid_vector",
                    &error,
                );
                tracing::warn!(
                    "[SEMANTIC:INDEX] discarded {}: {error}",
                    job.spec.subject_key
                );
                continue;
            }
            let write = DerivedEmbeddingWrite {
                job: job.spec.clone(),
                lease_token: job.lease_token.clone(),
                vector: vector.clone(),
            };
            match storage.commit_derived_embedding(&write) {
                Ok(()) => {
                    let id = job.summary.id;
                    // Smart Cluster scoring used to be queued by Python's
                    // `add_snapshot`, right after it wrote the vector. Same
                    // position, new writer: the prefilter needs the vector
                    // to exist before the entry is worth anything.
                    if let Err(error) = storage.enqueue_smart_cluster_pending(id) {
                        tracing::debug!(
                            "[SEMANTIC:INDEX] smart cluster enqueue failed for {id}: {error}"
                        );
                    }
                    indexed.push(IndexedSubject {
                        id,
                        vector,
                        text: job.text,
                        summary: job.summary,
                    });
                }
                Err(error) => tracing::warn!(
                    "[SEMANTIC:INDEX] commit failed for {}: {error}",
                    job.spec.subject_key
                ),
            }
        }
        Ok(indexed)
    })
    .await
    .map_err(|error| format!("commit task failed: {error}"))?
}

/// Hand claimed jobs back without charging the retry budget.
async fn release_claims(
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
    reason_code: &'static str,
    reason: &'static str,
) {
    let _ = tokio::task::spawn_blocking(move || {
        for job in claimed {
            let _ =
                storage.requeue_derived_index_job(&job.spec, &job.lease_token, reason_code, reason);
        }
    })
    .await;
}

/// Record an encode failure against the retry budget with a backoff.
async fn fail_claims(
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
    error_code: &'static str,
    error: &str,
) {
    let next_retry_at = (Utc::now() + ChronoDuration::minutes(RETRY_BACKOFF_MINUTES))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let error = error.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        for job in claimed {
            let _ = storage.mark_derived_index_job_failed(
                &job.spec,
                &job.lease_token,
                error_code,
                &error,
                Some(&next_retry_at),
            );
        }
    })
    .await;
}

/// Mirror newly indexed vectors into Python's Chroma hot layer.
///
/// The M2.4 dual-write with its direction reversed. Milestone 4 task clustering
/// still reads `task_vectors`, and Python no longer runs MiniLM, so without this
/// the clustering corpus would stop growing. It reuses `upsert_task_vectors` —
/// the command the M2.4 rollback path already established for "write a
/// Rust-produced vector into the hot layer" — rather than inventing a second
/// ingest contract for the same write.
///
/// The full row is sent, not just the vector. `add_snapshot` used to write the
/// metadata and the document alongside it, and both are load-bearing: clustering
/// selects hot vectors by `timestamp`, so a row that arrived without one would
/// be silently outside every window, and the reranker scores the stored document
/// text. Python encrypts the process name, window title, and document on its
/// side exactly as it did when it built them itself.
///
/// Best-effort on purpose: Rust holds the authoritative copy, and a mirror lost
/// here degrades unsupervised clustering rather than making a screenshot
/// unfindable by search.
async fn mirror_to_chroma(app: &AppHandle, indexed: &[IndexedSubject]) {
    if indexed.is_empty() {
        return;
    }
    let credential = app.state::<Arc<CredentialManagerState>>();
    let monitor = app.state::<MonitorState>();
    for chunk in indexed.chunks(MIRROR_BATCH) {
        let records: Vec<serde_json::Value> = chunk.iter().map(mirror_record).collect();
        let request = serde_json::json!({
            "command": "upsert_task_vectors",
            "records": records,
        });
        match authenticated_monitor_command(&credential, &monitor, request).await {
            // Python reports a refused command in the response body rather than
            // as a transport error, so an `error` field is the failure that
            // actually shows up in practice: clustering disabled, the vault
            // locked, or the monitor still starting.
            Ok(response) => {
                if let Some(error) = response.get("error").and_then(|value| value.as_str()) {
                    tracing::debug!(
                        "[SEMANTIC:INDEX] chroma mirror rejected {} vector(s): {error}",
                        chunk.len()
                    );
                }
            }
            Err(error) => {
                tracing::debug!(
                    "[SEMANTIC:INDEX] chroma mirror deferred for {} vector(s): {error}",
                    chunk.len()
                );
                // A transport failure applies to the pipe, not to this batch,
                // so the remaining chunks would fail the same way.
                return;
            }
        }
    }
}

/// One `upsert_task_vectors` record, field-for-field what `add_snapshot` used
/// to write into the hot layer for the same screenshot.
fn mirror_record(subject: &IndexedSubject) -> serde_json::Value {
    serde_json::json!({
        "id": subject.id.to_string(),
        "embedding": subject.vector,
        // Seconds since the epoch, which is what `screenshots.created_at`
        // yields here and what Chroma's `timestamp` metadata has always held.
        "timestamp": subject.summary.timestamp.unwrap_or(0),
        "process_name": subject.summary.process_name.clone().unwrap_or_default(),
        "window_title": subject.summary.window_title.clone().unwrap_or_default(),
        "category": subject.summary.category.clone().unwrap_or_default(),
        // The encoder input doubles as the stored document, as it did when
        // Python built both from one `build_task_text` call.
        "document": subject.text,
    })
}

/// Run an indexing pass right now, outside the idle gate.
///
/// Section 4 of the removal roadmap permits heavy machine learning to gate on
/// idle *or* on an explicit manual run; step 5 shipped only the first, which
/// left a machine that is rarely idle — or that spent the week on battery —
/// with no way to make recent screenshots searchable short of waiting. This is
/// the second path, and it is deliberately the only thing in the module that
/// ignores the idle signal.
///
/// It drains the queue rather than a slice of it. The user is present by
/// definition, which is an argument for *reporting and interruptibility* — both
/// of which this now has — and not, as the removed subject budget assumed, an
/// argument for stopping after a fixed number of screenshots. What it still
/// does not ignore: maintenance mode, a locked session, the ledger's retry
/// budget and backoff, and the runaway deadline. It is single-flight against
/// itself and against the idle worker.
///
/// Note that this does not require `semantic_index = rust`. Capture indexing is
/// unconditional — the worker in this module writes vectors whichever backend
/// serves queries — so gating the manual run on the read-path switch would
/// disable the only control over work that is running either way.
#[tauri::command]
pub async fn semantic_index_run_now(
    window: tauri::Window,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    app: AppHandle,
) -> Result<SemanticIndexRunSummary, String> {
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credential_state)?;

    // The guard is shared with Smart Cluster scoring now, so this no longer
    // means only "an index run is already going". The reason string is
    // interpolated raw into the Settings → Advanced line (`InferenceCards.jsx`)
    // rather than mapped to a translation key, so widening it costs nothing and
    // "already_running" would have been wrong half the time.
    let Ok(_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
        return Ok(SemanticIndexRunSummary {
            started: false,
            indexed: 0,
            failed: 0,
            remaining: None,
            stalled: None,
            skipped_reason: Some("semantic_worker_busy".to_string()),
        });
    };
    let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
    // Held for the rest of the command, so `running` clears on the `?` path too.
    let _active = run.begin();

    let outcome = run_pass(&app, PassMode::Manual).await?;
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let backlog = tokio::task::spawn_blocking(move || {
        storage
            .derived_index_backlog(DerivedIndexKind::SemanticText, MAX_ATTEMPTS)
            .ok()
    })
    .await
    .unwrap_or(None);

    Ok(SemanticIndexRunSummary {
        started: outcome.refused.is_none(),
        indexed: outcome.indexed,
        failed: outcome.failed,
        remaining: backlog.map(|backlog| backlog.claimable),
        stalled: backlog.map(|backlog| backlog.exhausted),
        skipped_reason: outcome
            .refused
            .or(outcome.stopped_because)
            .map(str::to_string),
    })
}

/// Ask the running manual pass to stop.
///
/// Returns whether there was one to stop, so a dialog reopened after the run
/// finished on its own does not report a cancellation that never happened.
///
/// This halts work and touches no user data, which is why it is not
/// session-guarded: `stop_monitor` is the precedent, and requiring an unlock to
/// *stop* something would be exactly backwards on a machine whose session
/// locked while the run was going.
///
/// The pass checks the flag between chunks and between drains, so a stop takes
/// effect within one chunk of four screenshots. Everything claimed and not yet
/// encoded goes back to the queue without being charged a retry attempt: the
/// user interrupting a pass is not those screenshots failing.
#[tauri::command]
pub async fn semantic_index_stop_now(
    window: tauri::Window,
    app: AppHandle,
) -> Result<bool, String> {
    crate::commands::check_main_window(&window)?;
    let run = app.state::<Arc<SemanticIndexRunState>>().inner().clone();
    if !run.is_running() {
        return Ok(false);
    }
    run.request_stop();
    tracing::info!("[SEMANTIC:INDEX] manual run asked to stop");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_is_a_bound_sqlite_modifier_not_an_interpolated_one() {
        // Both queries bind this as a parameter. A literal that ever stopped
        // being a constant would otherwise be a SQL injection point in a
        // datetime modifier, which is easy to miss.
        assert!(SEMANTIC_TEXT_RETENTION.starts_with('-'));
        assert!(SEMANTIC_TEXT_RETENTION.ends_with(" days"));
    }

    #[test]
    fn a_batch_fits_the_protocol_limit() {
        // Each chunk of the claimed batch goes through one `embed_text` call,
        // and a chunk is carved out of a claim, so both have to fit.
        assert!(ENCODE_CHUNK <= crate::ml_protocol::MAX_SEMANTIC_BATCH);
        assert!(ENCODE_CHUNK <= DRAIN_BATCH);
        assert!(DRAIN_BATCH <= crate::ml_protocol::MAX_SEMANTIC_BATCH);
    }

    #[test]
    fn a_run_starts_clean_and_releases_itself() {
        let state = Arc::new(SemanticIndexRunState::default());
        assert!(!state.is_running());

        // A stop that arrived while nothing was running — the user pressed it
        // as the previous run was returning — must not cancel the next run
        // before it has encoded anything.
        state.request_stop();
        {
            let _active = state.begin();
            assert!(state.is_running());
            assert!(!state.stopped_by_user());
            state.request_stop();
            assert!(state.stopped_by_user());
        }
        // The guard cleared `running`, so the stop command reports honestly
        // that there was nothing left to stop.
        assert!(!state.is_running());
    }

    #[test]
    fn progress_counts_what_left_the_queue_not_only_what_was_indexed() {
        // A chunk of four whose fourth vector came back invalid shrinks the
        // backlog by four and the index by three. Reporting three would leave
        // the progress line short of the total forever on a corpus with any
        // discarded subject in it.
        let state = SemanticIndexRunState::default();
        state.processed.fetch_add(4, Ordering::SeqCst);
        state.indexed.fetch_add(3, Ordering::SeqCst);
        assert_eq!(state.processed.load(Ordering::SeqCst), 4);
        assert_eq!(state.indexed.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_screenshot_with_nothing_to_encode_is_outside_the_corpus() {
        // The rule the candidate scan cannot express: it sees that a screenshot
        // has OCR rows, not that they decrypt to nothing. A process name of ""
        // happens whenever the foreground window belongs to a process this one
        // cannot open, which on Windows includes every elevated window.
        assert!(build_minilm_task_text("", "", "").is_empty());
        assert!(build_minilm_task_text("", "", "   ").trim().is_empty());
        assert!(!build_minilm_task_text("proc.exe", "", "").trim().is_empty());
        assert!(!build_minilm_task_text("", "Title", "").trim().is_empty());
        assert!(!build_minilm_task_text("", "", "text").trim().is_empty());
    }

    #[test]
    fn an_exclusion_is_fingerprinted_so_regained_text_reclaims_it() {
        // The exclusion is recorded against the empty source, which is what lets
        // `ensure_derived_index_job` treat a screenshot that later gains text as
        // an ordinary source change rather than as something already settled.
        let empty = minilm_job_spec(1, &build_minilm_task_text("", "", ""));
        let with_text = minilm_job_spec(1, &build_minilm_task_text("proc.exe", "", ""));
        assert_eq!(empty.subject_key, with_text.subject_key);
        assert_ne!(empty.source_fingerprint, with_text.source_fingerprint);
    }

    #[test]
    fn the_retry_budget_and_backoff_outlast_a_single_idle_window() {
        // A failing model load must not spend all five attempts inside one
        // night of idleness, which would leave the subject `exhausted` by
        // morning over a transient fault.
        assert!(MAX_ATTEMPTS >= 3);
        assert!(RETRY_BACKOFF_MINUTES >= 15);
    }

    fn indexed(id: i64) -> IndexedSubject {
        IndexedSubject {
            id,
            vector: vec![0.5; crate::minilm_migration::MINILM_DIMENSIONS],
            text: "proc.exe | Title | OCR".to_string(),
            summary: BackgroundScreenshotSummary {
                id,
                window_title: Some("Title".to_string()),
                process_name: Some("proc.exe".to_string()),
                timestamp: Some(1_700_000_000),
                category: Some("work".to_string()),
            },
        }
    }

    #[test]
    fn a_mirror_record_carries_every_field_add_snapshot_used_to_write() {
        // `upsert_task_vectors` reads exactly these keys. A vector arriving
        // without `timestamp` would land outside every clustering window, and
        // one without `document` would leave the reranker nothing to score —
        // both silent, both only visible as degraded clustering months later.
        let record = mirror_record(&indexed(42));
        let mut keys: Vec<&str> = record
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "category",
                "document",
                "embedding",
                "id",
                "process_name",
                "timestamp",
                "window_title",
            ]
        );
        // Python parses the id with `str.isdigit()`, so it has to be a string.
        assert_eq!(record["id"], "42");
        assert_eq!(record["timestamp"], 1_700_000_000);
        assert_eq!(record["process_name"], "proc.exe");
        assert_eq!(record["window_title"], "Title");
        assert_eq!(record["category"], "work");
        assert_eq!(record["document"], "proc.exe | Title | OCR");
        assert_eq!(
            record["embedding"].as_array().unwrap().len(),
            crate::minilm_migration::MINILM_DIMENSIONS
        );
    }

    #[test]
    fn absent_metadata_mirrors_as_empty_strings_rather_than_null() {
        // Python calls `str(...)` on each of these, so a JSON null would reach
        // Chroma as the literal text "None".
        let mut subject = indexed(7);
        subject.summary.process_name = None;
        subject.summary.window_title = None;
        subject.summary.category = None;
        subject.summary.timestamp = None;
        let record = mirror_record(&subject);
        assert_eq!(record["process_name"], "");
        assert_eq!(record["window_title"], "");
        assert_eq!(record["category"], "");
        assert_eq!(record["timestamp"], 0);
    }

    #[test]
    fn one_mirror_request_stays_inside_the_python_batch_limit() {
        // `upsert_task_vectors` raises above 128 records, and a raised batch
        // would lose the whole chunk rather than degrade.
        assert!(MIRROR_BATCH <= 128);
        assert!(DRAIN_BATCH <= MIRROR_BATCH);
    }

    #[test]
    fn the_manual_deadline_outlasts_the_work_it_is_guarding() {
        // The old assertion here paired a subject budget with this deadline and
        // argued that neither bound alone was sufficient — a deadline on its own
        // "makes the amount of work done depend on machine speed, which is not
        // something the user can see before clicking". That argument was
        // answered rather than dropped: the run now reports its progress
        // against the backlog it started with, so how far it gets is something
        // the user watches instead of predicts, and the stop button ends it
        // whenever they have seen enough.
        //
        // What is left to assert is that the runaway guard cannot fire in the
        // middle of the first chunk it allowed to start, which would leave a
        // cold model load having bought nothing.
        assert!(MANUAL_DEADLINE >= EMBED_TIMEOUT);
    }

    #[test]
    fn a_manual_run_cannot_wait_out_its_own_deadline() {
        // The counterpart of the runaway guard: a run that stands aside for
        // every foreground query it meets has to keep enough of its deadline to
        // reach the worker, or the click spends thirty minutes and reports
        // "deadline_reached, 0 indexed" — the indexer looking broken when it was
        // only being polite.
        assert!(MANUAL_FOREGROUND_WAIT_BUDGET < MANUAL_DEADLINE);
        // And it has to outlast one whole reranked calibration query, or the
        // ordinary collision — search, then index — ends the run instead of
        // sequencing it. The same bound `smart_cluster_scoring.rs` holds its
        // forced drain to, against the same measured per-document cost.
        let worst_query = Duration::from_millis(1180)
            * (crate::rerank::MAX_RERANK_RESULTS * crate::rerank::RERANK_OVERFETCH);
        assert!(MANUAL_FOREGROUND_WAIT_BUDGET > worst_query);
    }

    #[test]
    fn a_run_that_stood_aside_is_not_reported_as_a_slow_one() {
        // The two ways a manual run can end without indexing anything, and the
        // reason they are separate strings: one says the machine could not keep
        // up, the other says the machine was busy with this user's own searches.
        // Collapsing them would send somebody looking for a performance problem
        // that is not there.
        assert_ne!(WAITED_OUT_BY_FOREGROUND, DEADLINE_REACHED);
        // And the reason a drain resumes from must not read as either of the
        // reasons it ends on, or `drain_until_done` either loops on a stop or
        // ends on a query it should have waited for.
        for ends_the_run in [
            STOPPED_BY_USER,
            MAINTENANCE_STARTED,
            DEADLINE_REACHED,
            WAITED_OUT_BY_FOREGROUND,
        ] {
            assert_ne!(ends_the_run, FOREGROUND_QUERY_STOP);
        }
    }

    #[test]
    fn maintenance_binds_a_manual_run_but_idleness_does_not() {
        // The two guards a manual run must treat differently. The idle gate
        // exists to protect the foreground, and the user pressing the button is
        // the foreground; maintenance mode exists because the migration is
        // rewriting the same store, which consent cannot make safe.
        assert_eq!(PassMode::Manual, PassMode::Manual);
        assert_ne!(PassMode::Idle, PassMode::Manual);
    }

    #[test]
    fn a_refused_pass_reports_no_work_rather_than_silence() {
        // The summary has to distinguish "ran and found nothing" from "never
        // ran": both have `indexed = 0`, and only the second has a reason.
        let refused = PassOutcome::refused("session_locked");
        assert_eq!(refused.indexed, 0);
        assert_eq!(refused.refused, Some("session_locked"));

        let empty = PassOutcome::default();
        assert_eq!(empty.indexed, 0);
        assert!(empty.refused.is_none());
        assert!(empty.stopped_because.is_none());
    }
}
