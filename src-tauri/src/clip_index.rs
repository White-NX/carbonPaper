//! M2.5 step 8 — Rust-owned Chinese-CLIP capture indexing.
//!
//! Enqueues captured screenshot images for CLIP vector indexing, manages
//! background/foreground index workers, performs periodic repair scans, and
//! Legacy Chroma migration export remains a separate read-only monitor path.

use crate::clip_migration::{
    clip_job_spec, clip_memory_uri, diagnostic_code, validate_clip_vector,
};
use crate::credential_manager::CredentialManagerState;
use crate::idle::IdleState;
use crate::ml_protocol::{MlImageInput, MlSemanticModel};
use crate::semantic_runtime::{IndexRunProgress, SemanticRuntimeState};
use crate::storage::{
    BackgroundReadError, DerivedEmbeddingWrite, DerivedIndexJobSpec, DerivedIndexKind, StorageState,
};
use chrono::{Duration as ChronoDuration, Utc};
use image::RgbImage;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// How often the worker asks whether the machine has gone idle.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Subjects claimed from the ledger per drain.
const DRAIN_BATCH: usize = 8;

/// Images submitted to the worker per request.
///
/// Kept at 1 image to avoid exceeding protocol payload byte limits on high-resolution displays.
const ENCODE_CHUNK: usize = 1;

/// Attempts before a subject stops being claimable. Shared value with the
/// MiniLM ledger so one backlog figure means the same thing in both.
pub(crate) const MAX_ATTEMPTS: u32 = 5;

const RETRY_BACKOFF_MINUTES: i64 = 30;

/// Timeout guard for a single image read, decode, resize, and forward pass.
const EMBED_TIMEOUT: Duration = Duration::from_secs(180);

/// Rows examined per repair or reaper pass.
const MAINTENANCE_BATCH: u32 = 256;

/// Bounded time window for automatic repair scans, catching recently missed enqueues.
const REPAIR_SCAN_WINDOW: &str = "-7 days";

/// Maximum consecutive wait budget for manual indexing runs when held by foreground queries.
const MANUAL_FOREGROUND_WAIT_BUDGET: Duration = Duration::from_secs(5 * 60);

const FOREGROUND_POLL: Duration = Duration::from_millis(250);
const FOREGROUND_QUERY_STOP: &str = "foreground_query";
const WAITED_OUT_BY_FOREGROUND: &str = "foreground_query_held_the_worker";
const STOPPED_BY_USER: &str = "stopped_by_user";
const MAINTENANCE_STARTED: &str = "maintenance_started";

pub const CLIP_INDEX_PROGRESS_EVENT: &str = "clip-index-progress";

/// Recorded when an image carries no OCR text at all.
///
/// `worker_process.py` gates CLIP indexing on `ocr_text.strip()`, so a
/// text-less screenshot was never part of this corpus. The SQL candidate scan
/// cannot see that — the text is encrypted — so the decision is recorded in the
/// ledger, which is the only thing that stops the scan re-deciding it once a
/// minute.
const EMPTY_SOURCE_CODE: &str = "empty_source";
const EMPTY_SOURCE_REASON: &str =
    "no screenshot sharing this image hash has any OCR text, so Python would not have indexed it";

/// Memory ceiling for capture-prepared CLIP pixels.
///
/// One entry is the CLIP input itself — 224x224x3, about 147 KB — so this
/// admits roughly 445 screenshots. At one capture a minute that is about seven
/// hours of backlog, deliberately the same order as [`CAPTURE_PIXEL_TTL`] so
/// neither bound is the one that always fires first.
const CAPTURE_PIXEL_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// How long prepared pixels may wait for an idle window.
///
/// These are plaintext thumbnails of the user's screen held in process memory,
/// and they are the one place this path relaxes the storage model — everything
/// else is encrypted the moment it leaves the capture path. The TTL bounds that
/// exposure. Expiry loses no work permanently: the ledger row survives, and the
/// ordinary decrypt path re-encodes the image once the user unlocks.
const CAPTURE_PIXEL_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A screenshot already resized to the CLIP input, waiting for an idle window.
struct PreparedCapture {
    /// Recorded at capture time, when corpus membership was decided from OCR
    /// text that had not been encrypted yet. A locked pass cannot re-derive it.
    screenshot_id: i64,
    width: u32,
    height: u32,
    rgb: Arc<[u8]>,
    prepared_at: Instant,
}

static PREPARED_CAPTURES: OnceLock<Mutex<HashMap<String, PreparedCapture>>> = OnceLock::new();

fn prepared_captures() -> &'static Mutex<HashMap<String, PreparedCapture>> {
    PREPARED_CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

static CLIP_TARGET_SIZE: OnceLock<Option<(u32, u32)>> = OnceLock::new();

/// The CLIP input size, read once from the pinned preprocessor config.
///
/// This deliberately does not go through
/// `semantic_models.rs::resolve_semantic_model`: that verifies every pinned
/// file, which means a SHA-256 pass over a 177 MB model, and nothing on the
/// capture path can afford it. Skipping the check is safe here because a
/// tampered config cannot produce a wrong vector — the worker verifies the same
/// file before it loads the model and refuses rather than encoding against it,
/// so the worst case is a pre-resize nobody uses.
///
/// `None` — no config yet, because the model has not been downloaded — means
/// captures are not prepared at all, and the pipeline behaves exactly as it did
/// before this path existed.
fn clip_target_size() -> Option<(u32, u32)> {
    *CLIP_TARGET_SIZE.get_or_init(|| {
        let descriptor = crate::semantic_models::descriptor(MlSemanticModel::ChineseClip);
        let relative = descriptor.preprocessor_file?;
        let appdata = crate::resource_utils::file_in_local_appdata()?;
        // The same two roots, in the same order, that model resolution searches.
        ["models-onnx", "models"].into_iter().find_map(|root| {
            let bytes = std::fs::read(appdata.join(root).join(relative)).ok()?;
            crate::clip_preprocess::target_size_from_config(&bytes)
        })
    })
}

/// Resize a freshly captured screenshot to the CLIP input and hold it for the
/// idle worker.
///
/// This is what lets capture-side CLIP indexing run with the session locked.
/// The pixels are already decrypted and already in memory here; resizing them
/// now means the encode never reads the encrypted file back, and that read is
/// the only step in the whole pipeline that needs Windows Hello.
///
/// The resize is not free — roughly 0.25 s per source megapixel — but it is
/// work the idle worker would have paid anyway, moved to where the plaintext
/// already is. What is deliberately *not* done here is the model inference:
/// that is the part that must wait for an idle window.
///
/// Call this only for a screenshot that carries OCR text, since only those are
/// in the corpus.
pub fn remember_captured_pixels(screenshot_id: i64, image_hash: &str, image: &RgbImage) {
    if image_hash.is_empty() {
        return;
    }
    let Some((width, height)) = clip_target_size() else {
        return;
    };
    let resized = crate::clip_preprocess::pillow_bicubic_resize_rgb(image, width, height);
    let entry = PreparedCapture {
        screenshot_id,
        width,
        height,
        rgb: Arc::from(resized.into_raw().into_boxed_slice()),
        prepared_at: Instant::now(),
    };
    let mut cache = prepared_captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    drop_expired(&mut cache);
    make_room(&mut cache, entry.rgb.len());
    cache.insert(image_hash.to_string(), entry);
}

fn drop_expired(cache: &mut HashMap<String, PreparedCapture>) {
    cache.retain(|_, entry| entry.prepared_at.elapsed() < CAPTURE_PIXEL_TTL);
}

/// Drop the oldest entries until `incoming` bytes fit inside the budget.
///
/// Oldest first, rather than refusing the newcomer: the capture that has waited
/// longest is the one most likely to have been overtaken by an unlock, after
/// which the ordinary decrypt path can encode it at no extra cost.
fn make_room(cache: &mut HashMap<String, PreparedCapture>, incoming: usize) {
    if incoming > CAPTURE_PIXEL_BUDGET_BYTES {
        cache.clear();
        return;
    }
    let mut used: usize = cache.values().map(|entry| entry.rgb.len()).sum();
    while used + incoming > CAPTURE_PIXEL_BUDGET_BYTES {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.prepared_at)
            .map(|(hash, _)| hash.clone())
        else {
            return;
        };
        match cache.remove(&oldest) {
            Some(removed) => used = used.saturating_sub(removed.rgb.len()),
            None => return,
        }
    }
}

/// The screenshot a prepared capture was recorded against, if it is still held.
fn prepared_screenshot_id(image_hash: &str) -> Option<i64> {
    let mut cache = prepared_captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    drop_expired(&mut cache);
    cache.get(image_hash).map(|entry| entry.screenshot_id)
}

/// The prepared pixels for one image, as the decoded shape the encoder wants.
fn prepared_image(image_hash: &str) -> Option<DecodedImage> {
    let cache = prepared_captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let entry = cache.get(image_hash)?;
    if entry.prepared_at.elapsed() >= CAPTURE_PIXEL_TTL {
        return None;
    }
    Some(DecodedImage {
        width: entry.width,
        height: entry.height,
        rgb: entry.rgb.to_vec(),
    })
}

/// Release prepared pixels once they are no longer owed an encode.
fn forget_prepared(image_hash: &str) {
    let mut cache = prepared_captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    cache.remove(image_hash);
}

/// Whether any capture is currently holding prepared pixels.
///
/// A locked pass has nothing else it can encode, so this is what decides
/// between narrowing the pass and refusing it outright.
fn has_prepared_captures() -> bool {
    let mut cache = prepared_captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    drop_expired(&mut cache);
    !cache.is_empty()
}

/// Queue one freshly captured screenshot's image for CLIP indexing.
///
/// Called from the OCR commit path, next to the MiniLM enqueue. A failure here
/// is recoverable and not worth failing the capture over: the worker's repair
/// scan finds any eligible image with no ledger row, which is exactly what a
/// missed enqueue leaves behind.
///
/// `has_text` is the same `ocr_text.strip()` rule Python applied, answered by
/// the caller from the OCR results it is holding rather than read back out of
/// the database. That matters beyond saving a query: the stored text is
/// encrypted, so deriving this here would make the enqueue itself require an
/// unlocked session, and a capture taken while the app is locked would never
/// get a ledger row.
pub fn enqueue_captured_screenshot(
    storage: &StorageState,
    image_hash: &str,
    has_text: bool,
) -> Result<(), String> {
    if image_hash.is_empty() {
        return Ok(());
    }
    let spec = clip_job_spec(image_hash);
    // A later duplicate of the same pixels that *does* carry text clears the
    // exclusion through `ensure_derived_index_job`, because the exclusion is
    // fingerprinted and the repair scan re-offers the subject once the ledger
    // row no longer matches.
    if has_text {
        storage.ensure_derived_index_job(&spec)?;
        return Ok(());
    }
    storage.exclude_derived_index_subject(&spec, EMPTY_SOURCE_CODE, EMPTY_SOURCE_REASON)?;
    Ok(())
}

/// Whether any live screenshot sharing each hash carries OCR text.
///
/// The corpus rule for one image, applied to a batch. Returns the hashes that
/// belong in the index; everything else in `hashes` is an exclusion.
fn indexable_hashes(
    storage: &StorageState,
    hashes: &[String],
) -> Result<HashMap<String, i64>, BackgroundReadError> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let mapped = storage
        .map_image_hashes_to_screenshot_ids(hashes)
        .map_err(BackgroundReadError::Other)?;
    let ids: Vec<i64> = mapped.values().flatten().copied().collect();
    // One decryption pass for the whole batch: `min_chars` of 1 stops each
    // screenshot's OCR read at the first block, since all this needs to know is
    // whether any text exists.
    let texts = storage.get_ocr_text_prefixes_by_screenshot_ids_silent(&ids, 1)?;
    let mut indexable = HashMap::with_capacity(hashes.len());
    for hash in hashes {
        // Newest first, so the representative is the most recent capture of
        // these pixels that still carries text — the row whose metadata best
        // describes what the user would recognise.
        let representative = mapped.get(hash).and_then(|ids| {
            ids.iter().copied().find(|id| {
                texts
                    .get(id)
                    .map(|text| !text.trim().is_empty())
                    .unwrap_or(false)
            })
        });
        if let Some(id) = representative {
            indexable.insert(hash.clone(), id);
        }
    }
    Ok(indexable)
}

/// Idle-gated capture indexing and orphan repair.
pub async fn run_clip_index_worker(app: AppHandle) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        // A manual run, a MiniLM pass, or a Smart Cluster scoring pass holds
        // this for its whole duration. Skipping is the right answer rather than
        // queuing behind it: whatever holds the guard is using the worker this
        // pass would have to evict a model to reach.
        let Ok(_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
            continue;
        };
        match run_pass(&app, PassMode::Idle).await {
            Ok(outcome) if outcome.refused.is_none() && outcome.stopped_because.is_none() => {
                // A first base generation is useful as soon as *any* migrated
                // vectors exist. Pending captures become the exact tail and
                // must not postpone acceleration of the existing corpus.
                if let Err(error) = crate::clip_ann::maybe_rebuild(&app, false).await {
                    tracing::warn!("[CLIP:ANN] idle rebuild failed: {error}");
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("[CLIP:INDEX] idle pass failed: {error}"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PassMode {
    Idle,
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipIndexRunSummary {
    pub started: bool,
    pub indexed: u64,
    pub failed: u64,
    pub remaining: Option<u64>,
    pub stalled: Option<u64>,
    pub skipped_reason: Option<String>,
}

#[derive(Default)]
pub struct ClipIndexRunState {
    running: AtomicBool,
    stop_requested: AtomicBool,
    processed: AtomicU64,
    indexed: AtomicU64,
    total: AtomicU64,
}

impl ClipIndexRunState {
    /// Ask the running pass to stop after the image it is encoding. A request in
    /// flight cannot be interrupted, so this is prompt rather than immediate.
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn stopped_by_user(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// Counters as they stand, for a caller that polls rather than listens.
    ///
    /// The progress event is the primary channel and stays that way; this exists
    /// for the search box, which can mount in the middle of a run that has been
    /// going for hours and would otherwise show nothing until the next chunk
    /// lands. Reads four atomics and touches neither the worker nor the
    /// database, which is what makes it safe on a poll loop.
    pub fn progress(&self) -> IndexRunProgress {
        IndexRunProgress {
            running: self.running.load(Ordering::SeqCst),
            processed: self.processed.load(Ordering::SeqCst),
            indexed: self.indexed.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
        }
    }

    fn begin(self: &Arc<Self>) -> ActiveRun {
        self.stop_requested.store(false, Ordering::SeqCst);
        self.processed.store(0, Ordering::SeqCst);
        self.indexed.store(0, Ordering::SeqCst);
        self.total.store(0, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        ActiveRun(self.clone())
    }

    fn report_chunk(&self, app: &AppHandle, processed: u64, indexed: u64) {
        let processed_total = self.processed.fetch_add(processed, Ordering::SeqCst) + processed;
        let indexed_total = self.indexed.fetch_add(indexed, Ordering::SeqCst) + indexed;
        let _ = app.emit(
            CLIP_INDEX_PROGRESS_EVENT,
            serde_json::json!({
                "processed": processed_total,
                "indexed": indexed_total,
                "total": self.total.load(Ordering::SeqCst),
            }),
        );
    }
}

struct ActiveRun(Arc<ClipIndexRunState>);

impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// Whether background CLIP work may run right now.
///
/// Maintenance mode binds a manual run too: the user consenting to spend their
/// own CPU does not make a concurrent rewrite of the derived store safe, and the
/// step-7 migration is exactly such a rewrite.
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

#[derive(Default)]
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

/// How much of the history the repair scan may sweep this pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RepairScope {
    /// The step-7 migration still owns this index. An eligible image with no
    /// ledger row is one the copy has not reached, not one whose enqueue was
    /// missed, and the two are indistinguishable in SQL — so the scan stands
    /// down entirely rather than re-encoding what the copy is about to deliver.
    /// New captures are unaffected: they are enqueued by the capture path.
    Suspended,
    /// Recent screenshots only, which is what a missed enqueue looks like.
    Recent,
    /// The whole history. Only reachable through a backfill the user was shown
    /// an estimate for and agreed to.
    Full,
}

/// The scope decision itself, separated from the reads that supply it so the
/// table it implements can be tested rather than only described.
fn scope_from_state(migration_settled: bool, decision: Option<&str>) -> RepairScope {
    if !migration_settled {
        return RepairScope::Suspended;
    }
    match decision {
        Some(BACKFILL_APPROVED) => RepairScope::Full,
        _ => RepairScope::Recent,
    }
}

/// The age bound one scope implies, as a bound SQLite datetime modifier.
fn scan_window(scope: RepairScope) -> Option<&'static str> {
    match scope {
        RepairScope::Full => None,
        _ => Some(REPAIR_SCAN_WINDOW),
    }
}

/// Decide how far the repair scan may reach, from durable state alone.
///
/// Both inputs are one-way transitions a user or a migration writes once, so
/// this is a cheap read of `app_metadata` rather than anything the pass has to
/// reason about. The sentinel read goes through the cache
/// `clip_query.rs::migration_settled` keeps for the read path, since it is the
/// same question asked of the same never-cleared row.
fn repair_scope(storage: &StorageState) -> RepairScope {
    let settled = crate::clip_query::migration_settled(storage);
    let decision = storage
        .get_backfill_decision(DerivedIndexKind::ClipImage)
        .unwrap_or(None);
    scope_from_state(settled, decision.as_deref())
}

pub(crate) const BACKFILL_APPROVED: &str = "approved";
pub(crate) const BACKFILL_DECLINED: &str = "declined";

/// Ceiling on how many times a manual run re-runs the repair scan.
///
/// The loop ends when a scan makes no progress, which is the real condition;
/// this only stops a candidate that can neither be queued nor excluded — a
/// screenshot deleted between the two statements, say — from spinning the loop
/// forever. At `MAINTENANCE_BATCH` per pass it admits roughly a million images.
const MAX_REPAIR_PASSES: usize = 4096;

async fn run_pass(app: &AppHandle, mode: PassMode) -> Result<PassOutcome, String> {
    if !may_run(app, mode) {
        return Ok(PassOutcome::refused(match mode {
            PassMode::Manual => "maintenance_in_progress",
            PassMode::Idle => "not_idle",
        }));
    }
    if mode == PassMode::Idle {
        if app
            .state::<Arc<SemanticRuntimeState>>()
            .foreground_waiting()
        {
            return Ok(PassOutcome::refused(FOREGROUND_QUERY_STOP));
        }
    } else if let Some(reason) = stand_aside_for_foreground(app).await {
        return Ok(PassOutcome::refused(reason));
    }

    let storage = app.state::<Arc<StorageState>>().inner().clone();
    // Reading an image out of the store decrypts its content key through CNG,
    // so a locked session cannot reach the encrypted history at all. It can
    // still encode captures whose pixels were resized into memory at capture
    // time, which is what `remember_captured_pixels` exists for. So a lock
    // narrows the pass to those rather than refusing it — and refuses only when
    // there are none, which keeps the ledger untouched rather than marking a
    // batch `waiting_for_auth` on every tick.
    let locked = !storage.is_session_valid();
    if locked && !has_prepared_captures() {
        return Ok(PassOutcome::refused("session_locked"));
    }

    match mode {
        PassMode::Idle => {
            // Both of these read the encrypted store, so a locked pass skips
            // them and does nothing but drain what is already prepared. They
            // run on the next unlocked tick.
            if !locked {
                reap_orphans(storage.clone()).await?;
                let scope_storage = storage.clone();
                let scope = tokio::task::spawn_blocking(move || repair_scope(&scope_storage))
                    .await
                    .unwrap_or(RepairScope::Suspended);
                reconcile_missing(storage.clone(), scope).await?;
            }
            drain_queue(app, storage, mode, locked).await
        }
        PassMode::Manual => {
            reap_orphans(storage.clone()).await?;
            let scope_storage = storage.clone();
            let scope = tokio::task::spawn_blocking(move || repair_scope(&scope_storage))
                .await
                .unwrap_or(RepairScope::Suspended);
            // Queue everything in scope before draining, so the progress total
            // is the whole job rather than one scan's worth of it. A user who
            // approved a backfill and was quoted a duration for it should not
            // have to press the button once per 256 images to collect on that
            // quote — which is what a single scan per press would mean.
            let mut passes = 0;
            while passes < MAX_REPAIR_PASSES {
                passes += 1;
                if reconcile_missing(storage.clone(), scope).await? == 0 {
                    break;
                }
                if app.state::<Arc<ClipIndexRunState>>().stopped_by_user() {
                    break;
                }
            }
            let run = app.state::<Arc<ClipIndexRunState>>().inner().clone();
            let counting = storage.clone();
            let total = tokio::task::spawn_blocking(move || {
                counting
                    .derived_index_backlog(DerivedIndexKind::ClipImage, MAX_ATTEMPTS)
                    .map(|backlog| backlog.claimable)
                    .unwrap_or(0)
            })
            .await
            .unwrap_or(0);
            run.total.store(total, Ordering::SeqCst);
            drain_until_done(app, storage).await
        }
    }
}

/// Wait out a foreground query rather than competing with it.
///
/// `None` once the worker is free. The budget is spent within one call and
/// starts again at the next, so it bounds a run that cannot make progress
/// rather than a run that has been politely yielding for a long time — see
/// [`MANUAL_FOREGROUND_WAIT_BUDGET`].
async fn stand_aside_for_foreground(app: &AppHandle) -> Option<&'static str> {
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let mut waited = Duration::ZERO;
    loop {
        if !semantic.foreground_waiting() {
            return None;
        }
        let run = app.state::<Arc<ClipIndexRunState>>();
        if run.stopped_by_user() {
            return Some(STOPPED_BY_USER);
        }
        if crate::maintenance::is_active() {
            return Some(MAINTENANCE_STARTED);
        }
        if waited >= MANUAL_FOREGROUND_WAIT_BUDGET {
            return Some(WAITED_OUT_BY_FOREGROUND);
        }
        tokio::time::sleep(FOREGROUND_POLL).await;
        waited += FOREGROUND_POLL;
    }
}

/// Drain the whole queue for a manual run, resuming across foreground queries.
///
/// There is no run deadline. A backfill of a full history is measured in hours
/// — 0.15 s plus 0.25 s per source megapixel, per image — so any fixed ceiling
/// would end most runs partway and report a reason that describes none of what
/// happened. What bounds this instead is the work itself, which is finite, and
/// the stop button, which the user can reach even with the session locked.
async fn drain_until_done(
    app: &AppHandle,
    storage: Arc<StorageState>,
) -> Result<PassOutcome, String> {
    let mut total = PassOutcome::default();
    loop {
        // Manual runs hold an authenticated session by construction:
        // `clip_index_run_now` checks it before anything else.
        let outcome = drain_queue(app, storage.clone(), PassMode::Manual, false).await?;
        total.indexed += outcome.indexed;
        total.failed += outcome.failed;
        match outcome.stopped_because {
            // Nothing left to claim.
            None if outcome.indexed == 0 && outcome.failed == 0 => return Ok(total),
            None => {}
            // A foreground query took the worker: wait for it and claim again,
            // rather than ending a run somebody started.
            Some(FOREGROUND_QUERY_STOP) => {
                if let Some(reason) = stand_aside_for_foreground(app).await {
                    total.stopped_because = Some(reason);
                    return Ok(total);
                }
            }
            Some(reason) => {
                total.stopped_because = Some(reason);
                return Ok(total);
            }
        }
    }
}

/// One claimed job: its ledger identity, the image to encode, and the lease
/// that authorizes the commit.
struct ClaimedJob {
    spec: DerivedIndexJobSpec,
    image_hash: String,
    lease_token: String,
}

async fn drain_queue(
    app: &AppHandle,
    storage: Arc<StorageState>,
    mode: PassMode,
    locked: bool,
) -> Result<PassOutcome, String> {
    let claimed = claim_batch(storage.clone(), locked).await?;
    if claimed.is_empty() {
        return Ok(PassOutcome::default());
    }
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let run = app.state::<Arc<ClipIndexRunState>>().inner().clone();
    let mut pending: VecDeque<ClaimedJob> = claimed.into();
    let mut indexed = 0usize;
    let mut outcome = PassOutcome::default();
    let mut failure: Option<String> = None;

    while !pending.is_empty() {
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
                "the pass stopped before this image was encoded",
            )
            .await;
            outcome.stopped_because = Some(reason);
            break;
        }

        let chunk: Vec<ClaimedJob> = pending.drain(..ENCODE_CHUNK.min(pending.len())).collect();
        let chunk_len = chunk.len() as u64;
        match encode_chunk(app, &semantic, storage.clone(), chunk).await {
            Ok(encoded) => {
                outcome.indexed += encoded as u64;
                if mode == PassMode::Manual {
                    run.report_chunk(app, chunk_len, encoded as u64);
                }
                indexed += encoded;
            }
            Err(error) => {
                outcome.failed += chunk_len;
                if mode == PassMode::Manual {
                    run.report_chunk(app, chunk_len, 0);
                }
                // Whatever broke the worker breaks every remaining chunk the
                // same way, and charging the retry budget for an attempt that
                // was never made would spend it on the worker's behalf.
                if !pending.is_empty() {
                    release_claims(
                        storage.clone(),
                        Vec::from(std::mem::take(&mut pending)),
                        "batch_aborted",
                        "an earlier image of the same pass failed to encode",
                    )
                    .await;
                }
                failure = Some(error);
                break;
            }
        }
    }

    if indexed > 0 {
        tracing::info!("[CLIP:INDEX] indexed {} image(s)", indexed);
    }
    match failure {
        Some(error) if mode == PassMode::Manual => {
            outcome.stopped_because = Some("encode_failed");
            tracing::warn!("[CLIP:INDEX] manual run stopped: {error}");
            Ok(outcome)
        }
        Some(error) => Err(error),
        None => Ok(outcome),
    }
}

/// Read, decode, and encode one chunk, then commit it.
///
/// An image that cannot be read or decoded is discarded rather than retried:
/// the bytes on disk are stable, so the next attempt fails identically. An
/// encode failure is a worker problem and does charge the retry budget.
async fn encode_chunk(
    app: &AppHandle,
    semantic: &Arc<SemanticRuntimeState>,
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
) -> Result<usize, String> {
    let read_storage = storage.clone();
    let hashes: Vec<String> = claimed.iter().map(|job| job.image_hash.clone()).collect();
    let decoded = tokio::task::spawn_blocking(move || load_images(&read_storage, &hashes))
        .await
        .map_err(|error| format!("image read task failed: {error}"))?;

    // Partition before submitting: an unreadable image must not cost the
    // readable ones in the same chunk their attempt.
    let mut encodable = Vec::with_capacity(claimed.len());
    let mut unreadable = Vec::new();
    for (job, image) in claimed.into_iter().zip(decoded) {
        match image {
            Ok(image) => encodable.push((job, image)),
            Err(error) => unreadable.push((job, error)),
        }
    }
    for (job, error) in unreadable {
        let _ = storage.mark_derived_index_job_discarded(
            &job.spec,
            &job.lease_token,
            "image_unreadable",
            &error,
        );
        forget_prepared(&job.image_hash);
        tracing::warn!("[CLIP:INDEX] discarded {}: {error}", job.spec.subject_key);
    }
    if encodable.is_empty() {
        return Ok(0);
    }

    // One contiguous body with per-image offsets, which is the shape
    // `MlRequest::EmbedImage` carries.
    let mut body = Vec::new();
    let mut inputs = Vec::with_capacity(encodable.len());
    for (_, image) in &encodable {
        inputs.push(MlImageInput {
            width: image.width,
            height: image.height,
            stride: image.width as usize * 3,
            offset: body.len(),
            body_len: image.rgb.len(),
        });
        body.extend_from_slice(&image.rgb);
    }

    let embedded = semantic
        .embed_image(
            app.clone(),
            MlSemanticModel::ChineseClip,
            inputs,
            body,
            EMBED_TIMEOUT,
            // CPU only; the module header records why.
            false,
        )
        .await;

    let jobs: Vec<ClaimedJob> = encodable.into_iter().map(|(job, _)| job).collect();
    let vectors = match embedded {
        Ok(result) if result.vectors.len() == jobs.len() => result.vectors,
        Ok(result) => {
            let error = format!(
                "semantic worker returned {} vectors for {} images",
                result.vectors.len(),
                jobs.len()
            );
            fail_claims(storage, jobs, "embed_mismatch", &error).await;
            return Err(error);
        }
        Err(error) => {
            fail_claims(storage, jobs, "embed_failed", &error).await;
            return Err(format!("embed failed: {error}"));
        }
    };

    commit_batch(storage, jobs, vectors).await
}

/// One screenshot decoded into the packed RGB the CLIP preprocessor expects.
struct DecodedImage {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

/// Read and decode each image, keeping per-image failures per-image.
///
/// Prepared capture pixels win over the stored file. They are the same pixels
/// resized by the same function to the same target, so the vector is identical
/// either way — but this branch needs no decryption, which is what lets the
/// pass run with the session locked, and it skips the decode and resize that
/// dominate the per-image cost (roughly 0.25 s per source megapixel against a
/// constant ~0.15 s of inference).
///
/// `read_image_bytes_silent` accepts the same `memory://{hash}` path the Chroma
/// collection was keyed under, so the subject key needs no translation to reach
/// the bytes. "Silent" matters: this runs unattended, and CNG must not raise a
/// consent dialog behind an idle worker.
fn load_images(storage: &StorageState, hashes: &[String]) -> Vec<Result<DecodedImage, String>> {
    hashes
        .iter()
        .map(|hash| {
            if let Some(prepared) = prepared_image(hash) {
                return Ok(prepared);
            }
            let (bytes, _format) = storage
                .read_image_bytes_silent(&clip_memory_uri(hash))
                .map_err(|error| format!("failed to read the image: {error}"))?;
            let decoded = image::load_from_memory(&bytes)
                .map_err(|error| format!("failed to decode the image: {error}"))?;
            // RGB8 because that is what `preprocess_clip_images` reconstructs;
            // the alpha of a transparent capture is dropped here rather than
            // being resized into a channel the model does not read.
            let rgb = decoded.to_rgb8();
            let (width, height) = (rgb.width(), rgb.height());
            if width == 0 || height == 0 {
                return Err("the image has a zero dimension".to_string());
            }
            Ok(DecodedImage {
                width,
                height,
                rgb: rgb.into_raw(),
            })
        })
        .collect()
}

/// Claim up to one batch, re-checking each subject's corpus membership.
///
/// Membership is re-derived rather than trusted from the ledger for the same
/// reason MiniLM rebuilds its source text: a job queued at capture time may
/// have been overtaken — here by the deletion of the only screenshot that
/// carried text for those pixels.
///
/// A locked pass cannot re-derive it, because the OCR text it is derived from
/// is encrypted. So a locked pass claims only subjects that still hold prepared
/// pixels, whose membership was settled at capture time against a specific
/// screenshot, and it confirms that screenshot is still live — a check that
/// reads `is_deleted` and decrypts nothing. Everything else stays queued rather
/// than being excluded on a guess.
async fn claim_batch(storage: Arc<StorageState>, locked: bool) -> Result<Vec<ClaimedJob>, String> {
    tokio::task::spawn_blocking(move || -> Result<Vec<ClaimedJob>, String> {
        let jobs = storage.claimable_derived_index_jobs(
            DerivedIndexKind::ClipImage,
            MAX_ATTEMPTS,
            DRAIN_BATCH as u32,
        )?;
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        let hashes: Vec<String> = jobs
            .iter()
            .map(|job| job.spec.subject_key.clone())
            .collect();
        let (indexable, live_ids) = if locked {
            (
                HashMap::new(),
                storage.map_image_hashes_to_screenshot_ids(&hashes)?,
            )
        } else {
            (
                indexable_hashes(&storage, &hashes).map_err(|error| error.to_string())?,
                HashMap::new(),
            )
        };

        let mut claimed = Vec::with_capacity(jobs.len());
        for job in jobs {
            if locked {
                // No prepared pixels means nothing this pass can do with the
                // subject. Leave it queued; an unlocked pass will decide.
                let Some(screenshot_id) = prepared_screenshot_id(&job.spec.subject_key) else {
                    continue;
                };
                // The capture-time answer holds only as long as the screenshot
                // it was given for. If that row is gone, another screenshot may
                // still share these pixels while carrying no text at all, and
                // the corpus rule would exclude it — a call this pass cannot
                // make. Drop the pixels and let an unlocked pass decide.
                let still_live = live_ids
                    .get(&job.spec.subject_key)
                    .is_some_and(|ids| ids.contains(&screenshot_id));
                if !still_live {
                    forget_prepared(&job.spec.subject_key);
                    continue;
                }
                match storage.mark_derived_index_job_processing(&job.spec) {
                    Ok(lease_token) => claimed.push(ClaimedJob {
                        image_hash: job.spec.subject_key.clone(),
                        spec: job.spec,
                        lease_token,
                    }),
                    Err(error) => tracing::debug!(
                        "[CLIP:INDEX] could not claim {}: {error}",
                        job.spec.subject_key
                    ),
                }
                continue;
            }
            let Some(_screenshot_id) = indexable.get(&job.spec.subject_key).copied() else {
                // Nothing to index. Record the exclusion, or the repair scan
                // hands the subject straight back; if even that fails the
                // screenshot itself is gone and the row goes with it.
                let recorded = storage
                    .exclude_derived_index_subject(
                        &job.spec,
                        EMPTY_SOURCE_CODE,
                        EMPTY_SOURCE_REASON,
                    )
                    .unwrap_or(false);
                if !recorded {
                    let _ = storage.delete_derived_index_subject(
                        DerivedIndexKind::ClipImage,
                        &job.spec.subject_key,
                    );
                }
                // The pixels are no longer owed an encode either way.
                forget_prepared(&job.spec.subject_key);
                continue;
            };
            match storage.mark_derived_index_job_processing(&job.spec) {
                Ok(lease_token) => claimed.push(ClaimedJob {
                    image_hash: job.spec.subject_key.clone(),
                    spec: job.spec,
                    lease_token,
                }),
                Err(error) => tracing::debug!(
                    "[CLIP:INDEX] could not claim {}: {error}",
                    job.spec.subject_key
                ),
            }
        }
        Ok(claimed)
    })
    .await
    .map_err(|error| format!("claim task failed: {error}"))?
}

async fn commit_batch(
    storage: Arc<StorageState>,
    claimed: Vec<ClaimedJob>,
    vectors: Vec<Vec<f32>>,
) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || {
        let mut indexed = 0usize;
        for (job, vector) in claimed.into_iter().zip(vectors) {
            if let Err(error) = validate_clip_vector(&vector) {
                // A zero or non-finite vector would poison every cosine score
                // it touches. Discard rather than retry: the input is stable,
                // so the next attempt produces the same bad vector.
                let _ = storage.mark_derived_index_job_discarded(
                    &job.spec,
                    &job.lease_token,
                    "invalid_vector",
                    &error,
                );
                forget_prepared(&job.image_hash);
                tracing::warn!("[CLIP:INDEX] discarded {}: {error}", job.spec.subject_key);
                continue;
            }
            let write = DerivedEmbeddingWrite {
                job: job.spec.clone(),
                lease_token: job.lease_token.clone(),
                vector: vector.clone(),
            };
            match storage.commit_derived_embedding(&write) {
                Ok(()) => {
                    // The encode this capture was held for is done.
                    forget_prepared(&job.image_hash);
                    indexed += 1;
                }
                Err(error) => tracing::warn!(
                    "[CLIP:INDEX] commit failed for {}: {error}",
                    job.spec.subject_key
                ),
            }
        }
        Ok(indexed)
    })
    .await
    .map_err(|error| format!("commit task failed: {error}"))?
}

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

/// Drop CLIP rows with no live screenshot behind them.
///
/// A safety net rather than a live path — see the module header. It also cleans
/// up after the step-7 migration, which can import a row for a screenshot the
/// user deleted between the snapshot and the commit.
async fn reap_orphans(storage: Arc<StorageState>) -> Result<(), String> {
    let removed = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        let subjects = storage.list_orphaned_clip_image_subjects(MAINTENANCE_BATCH)?;
        let mut removed = 0;
        for subject in &subjects {
            if storage
                .delete_derived_index_subject(DerivedIndexKind::ClipImage, subject)
                .is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    })
    .await
    .map_err(|error| format!("orphan reaper task failed: {error}"))??;
    if removed > 0 {
        tracing::info!("[CLIP:INDEX] removed {removed} orphaned image vector(s)");
    }
    Ok(())
}

/// Queue eligible images that have no ledger row, as far back as `scope` allows.
///
/// Returns how many subjects the pass resolved — queued plus excluded — which
/// is what tells a manual run's loop that there is nothing left to find.
async fn reconcile_missing(
    storage: Arc<StorageState>,
    scope: RepairScope,
) -> Result<usize, String> {
    if scope == RepairScope::Suspended {
        return Ok(0);
    }
    let window = scan_window(scope);
    let (queued, excluded) =
        tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
            let hashes = storage.list_clip_image_index_candidates(window, MAINTENANCE_BATCH)?;
            if hashes.is_empty() {
                return Ok((0, 0));
            }
            let indexable =
                indexable_hashes(&storage, &hashes).map_err(|error| error.to_string())?;
            let mut queued = 0;
            let mut excluded = 0;
            for hash in &hashes {
                let spec = clip_job_spec(hash);
                if indexable.contains_key(hash) {
                    if storage.ensure_derived_index_job(&spec).is_ok() {
                        queued += 1;
                    }
                } else if storage
                    .exclude_derived_index_subject(&spec, EMPTY_SOURCE_CODE, EMPTY_SOURCE_REASON)
                    .unwrap_or(false)
                {
                    excluded += 1;
                }
            }
            Ok((queued, excluded))
        })
        .await
        .map_err(|error| format!("reconcile task failed: {error}"))??;
    if queued > 0 || excluded > 0 {
        tracing::info!("[CLIP:INDEX] repair queued {queued}, excluded {excluded}");
    }
    Ok(queued + excluded)
}

/// Drain the CLIP queue now, at the user's request.
///
/// Single-flight against the idle worker through the shared pass guard. It
/// ignores the idle signal and nothing else — maintenance mode, a locked
/// session, and the ledger's retry budget all still apply.
#[tauri::command]
pub async fn clip_index_run_now(
    app: AppHandle,
    credential: tauri::State<'_, Arc<CredentialManagerState>>,
    run: tauri::State<'_, Arc<ClipIndexRunState>>,
) -> Result<ClipIndexRunSummary, String> {
    crate::commands::check_auth_required(&credential)?;
    let Ok(_guard) = crate::semantic_runtime::BACKGROUND_PASS_GUARD.try_lock() else {
        return Ok(ClipIndexRunSummary {
            started: false,
            indexed: 0,
            failed: 0,
            remaining: None,
            stalled: None,
            skipped_reason: Some("another_pass_running".to_string()),
        });
    };
    let run = run.inner().clone();
    if run.is_running() {
        return Ok(ClipIndexRunSummary {
            started: false,
            indexed: 0,
            failed: 0,
            remaining: None,
            stalled: None,
            skipped_reason: Some("already_running".to_string()),
        });
    }
    let _active = run.begin();

    let outcome = run_pass(&app, PassMode::Manual).await?;
    if outcome.refused.is_none() && outcome.stopped_because.is_none() {
        if let Err(error) = crate::clip_ann::maybe_rebuild(&app, false).await {
            tracing::warn!("[CLIP:ANN] manual rebuild failed: {error}");
        }
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let backlog = tokio::task::spawn_blocking(move || {
        storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, MAX_ATTEMPTS)
            .ok()
    })
    .await
    .ok()
    .flatten();

    Ok(ClipIndexRunSummary {
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

/// Ask the running manual pass to stop, after the image it is encoding.
///
/// Returns whether there was a run to stop, so a settings panel reopened after
/// the run ended on its own does not report a cancellation that never happened.
///
/// Deliberately *not* session-guarded, for the reason
/// `minilm_index.rs::semantic_index_stop_now` records: this halts work and
/// touches no user data, and requiring an unlock to *stop* something would be
/// backwards on a machine whose session locked while the run was going. The
/// argument is stronger here than there. A MiniLM run clears its queue in
/// seconds; a CLIP backfill of a full history runs for hours, so outliving a
/// session lock is its ordinary case rather than its unlucky one.
///
/// Everything claimed and not yet encoded goes back to the queue without being
/// charged a retry attempt — a user interrupting a pass is not those images
/// failing — so a stop is also a pause: pressing the run button again resumes
/// from the same ledger.
#[tauri::command]
pub async fn clip_index_stop_now(
    window: tauri::Window,
    run: tauri::State<'_, Arc<ClipIndexRunState>>,
) -> Result<bool, String> {
    crate::commands::check_main_window(&window)?;
    if !run.is_running() {
        return Ok(false);
    }
    run.request_stop();
    tracing::info!("[CLIP:INDEX] manual run asked to stop");
    Ok(true)
}

/// What a CLIP backfill would cover, and why it is being offered.
///
/// Deliberately not one "failed" number. A migration that skipped rows because
/// their screenshots were deleted did nothing wrong, and reporting that as a
/// failure would alarm every user who has ever emptied their history; a
/// migration that could not decode a stored vector did fail, and hiding it
/// inside the same total would be the opposite mistake. The two populations
/// that actually need encoding are the third and fourth fields, and they are
/// the only ones the estimate covers.
#[derive(Debug, Clone, Serialize)]
pub struct ClipBackfillOffer {
    /// Whether the step-7 copy has settled. Until it has, nothing is offered:
    /// the right answer to an unfinished copy is to let it finish, which costs
    /// a float copy rather than hours of inference.
    pub migration_settled: bool,
    /// `approved`, `declined`, or absent when the user has not been asked.
    pub decision: Option<String>,
    /// There is work to offer and no decision recorded yet.
    pub should_ask: bool,
    /// Images with no vector and no ledger row: the migration had nothing to
    /// copy for them, usually because Python never indexed them.
    pub never_indexed: u64,
    /// Queued images whose encode retry budget is spent. Nothing clears these
    /// on its own.
    pub stalled: u64,
    /// Migration rows skipped because no live screenshot matches them. Ordinary
    /// consequence of deleting a screenshot; nothing to re-encode.
    pub skipped_deleted: u64,
    /// Migration rows that could not be imported for any other reason.
    pub failed_imports: u64,
    /// Estimated wall-clock encode time for `never_indexed`, from the measured
    /// cost model. Not a promise: it assumes the machine that measured it and
    /// counts an unknown screenshot size as 1080p.
    pub estimated_seconds: u64,
    pub migration_status: Option<String>,
    /// The migration is ready to inspect, but the full-history census was
    /// deferred until the machine is idle or the user explicitly refreshes.
    pub diagnostics_deferred: bool,
}

const BACKFILL_OFFER_CACHE_TTL: Duration = Duration::from_secs(30);

struct CachedClipBackfillOffer {
    loaded_at: Instant,
    offer: ClipBackfillOffer,
}

static BACKFILL_OFFER_CACHE: OnceLock<tokio::sync::Mutex<Option<CachedClipBackfillOffer>>> =
    OnceLock::new();

fn terminal_backfill_offer(migration_settled: bool, decision: Option<String>) -> ClipBackfillOffer {
    ClipBackfillOffer {
        migration_settled,
        decision,
        should_ask: false,
        never_indexed: 0,
        stalled: 0,
        skipped_deleted: 0,
        failed_imports: 0,
        estimated_seconds: 0,
        migration_status: None,
        diagnostics_deferred: false,
    }
}

fn deferred_backfill_offer() -> ClipBackfillOffer {
    ClipBackfillOffer {
        diagnostics_deferred: true,
        ..terminal_backfill_offer(true, None)
    }
}

async fn invalidate_backfill_offer_cache() {
    if let Some(cache) = BACKFILL_OFFER_CACHE.get() {
        *cache.lock().await = None;
    }
}

/// Encode cost, fitted to the 2026-08-04 measurements in the roadmap.
///
/// `t ≈ 0.15 s + 0.25 s × source megapixels`, which reproduces all four
/// measured points (0.38 s at 720p, 0.67 s at 1080p, 1.08 s at 1440p, 2.21 s at
/// 4K) to within 1.5%. It is nearly linear in the *source* pixel count because
/// CLIP resizes everything to 224² and the inference is therefore constant: what
/// scales is decode and resize. A count alone would be off by a factor of six
/// between a 720p machine and a 4K one, which is the difference between a
/// useful estimate and a misleading one.
const ENCODE_FIXED_SECONDS: f64 = 0.15;
const ENCODE_SECONDS_PER_MEGAPIXEL: f64 = 0.25;
/// 1920×1080, for rows written before `screenshots.width`/`height` existed.
const ASSUMED_MEGAPIXELS: f64 = 2.0736;

/// Diagnostic codes that mean "correctly skipped" rather than "could not be
/// imported". Shared with the side that records them, so the two cannot drift.
const EXPECTED_SKIP_CODES: &[&str] = &[
    diagnostic_code::ORPHAN_DOCUMENT_ID,
    diagnostic_code::SCREENSHOT_DISAPPEARED,
    diagnostic_code::SNAPSHOT_ROW_MISSING,
];
/// Recorded against the run rather than against a row, so it belongs to neither
/// population.
const RUN_LEVEL_CODE: &str = diagnostic_code::RUN_FAILED;

/// What a backfill would cost and what it would fix, for the dialog that asks.
#[tauri::command]
pub async fn get_clip_backfill_offer(
    app: AppHandle,
    allow_expensive: Option<bool>,
) -> Result<ClipBackfillOffer, String> {
    let allow_expensive = allow_expensive.unwrap_or(false);
    let requested_at = Instant::now();
    let cache = BACKFILL_OFFER_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    let mut cache_guard = cache.lock().await;
    if let Some(cached) = cache_guard.as_ref() {
        let cache_ttl = if cached.offer.diagnostics_deferred || !cached.offer.migration_settled {
            Duration::from_secs(5)
        } else {
            BACKFILL_OFFER_CACHE_TTL
        };
        let satisfies_request = !allow_expensive || !cached.offer.diagnostics_deferred;
        if satisfies_request
            && (cached.loaded_at >= requested_at || cached.loaded_at.elapsed() < cache_ttl)
        {
            return Ok(cached.offer.clone());
        }
    }

    let machine_is_idle = app
        .state::<Arc<IdleState>>()
        .is_idle
        .load(Ordering::Relaxed);
    let load_expensive_diagnostics = allow_expensive || machine_is_idle;
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let offer = tokio::task::spawn_blocking(move || -> Result<ClipBackfillOffer, String> {
        let migration_settled = crate::clip_query::migration_settled(&storage);
        let decision = storage
            .get_backfill_decision(DerivedIndexKind::ClipImage)
            .unwrap_or(None);
        if !migration_settled || decision.is_some() {
            return Ok(terminal_backfill_offer(migration_settled, decision));
        }
        if !load_expensive_diagnostics {
            return Ok(deferred_backfill_offer());
        }

        let work = storage
            .clip_image_backfill_work(ASSUMED_MEGAPIXELS)
            .unwrap_or_default();
        let backlog = storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, MAX_ATTEMPTS)
            .unwrap_or_default();

        let run = storage
            .get_latest_derived_migration_run(DerivedIndexKind::ClipImage)
            .unwrap_or(None);
        let (skipped_deleted, failed_imports) = match &run {
            Some(run) => {
                let census = storage
                    .count_derived_migration_errors_by_code(&run.run_id)
                    .unwrap_or_default();
                let skipped: u64 = EXPECTED_SKIP_CODES
                    .iter()
                    .filter_map(|code| census.get(*code))
                    .sum();
                // Everything not explicitly expected counts as a failure, so a
                // diagnostic code added later shows up as something wrong
                // rather than vanishing from both totals.
                let total: u64 = census
                    .iter()
                    .filter(|(code, _)| code.as_str() != RUN_LEVEL_CODE)
                    .map(|(_, count)| *count)
                    .sum();
                (skipped, total.saturating_sub(skipped))
            }
            None => (0, 0),
        };

        let estimated_seconds = (work.images as f64 * ENCODE_FIXED_SECONDS
            + work.megapixels * ENCODE_SECONDS_PER_MEGAPIXEL)
            .round()
            .max(0.0) as u64;
        let has_work = work.images > 0 || backlog.exhausted > 0;
        Ok(ClipBackfillOffer {
            migration_settled,
            should_ask: migration_settled && decision.is_none() && has_work,
            decision,
            never_indexed: work.images,
            stalled: backlog.exhausted,
            skipped_deleted,
            failed_imports,
            estimated_seconds,
            migration_status: run.map(|run| run.status),
            diagnostics_deferred: false,
        })
    })
    .await
    .map_err(|error| format!("backfill offer task failed: {error}"))??;
    *cache_guard = Some(CachedClipBackfillOffer {
        loaded_at: Instant::now(),
        offer: offer.clone(),
    });
    Ok(offer)
}

/// Record the user's answer to that offer.
///
/// `approved` is what widens the repair scan from recent screenshots to the
/// whole history; `declined` leaves it narrow. Either way the answer is durable
/// (`app_metadata`), because re-asking at every launch would be its own kind of
/// nagging and because a decision that evaporates on restart is not a decision.
/// It stays changeable: the settings card offers the same choice afterwards.
#[tauri::command]
pub async fn set_clip_backfill_decision(
    credential: tauri::State<'_, Arc<CredentialManagerState>>,
    app: AppHandle,
    decision: String,
) -> Result<ClipBackfillOffer, String> {
    crate::commands::check_auth_required(&credential)?;
    if !matches!(decision.as_str(), BACKFILL_APPROVED | BACKFILL_DECLINED) {
        return Err(format!("Unknown backfill decision: {decision}"));
    }
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let recorded = decision.clone();
    tokio::task::spawn_blocking(move || {
        storage.set_backfill_decision(DerivedIndexKind::ClipImage, &recorded)
    })
    .await
    .map_err(|error| format!("backfill decision task failed: {error}"))??;
    tracing::info!("[CLIP:INDEX] backfill {decision} by the user");
    invalidate_backfill_offer_cache().await;
    get_clip_backfill_offer(app, Some(false)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(bytes: usize, age: Duration) -> PreparedCapture {
        PreparedCapture {
            screenshot_id: 1,
            width: 1,
            height: 1,
            rgb: Arc::from(vec![0u8; bytes].into_boxed_slice()),
            prepared_at: Instant::now()
                .checked_sub(age)
                .expect("test ages fit inside the monotonic clock"),
        }
    }

    #[test]
    fn prepared_pixels_below_the_budget_are_all_kept() {
        let mut cache = HashMap::new();
        cache.insert("a".to_string(), prepared(1024, Duration::ZERO));
        cache.insert("b".to_string(), prepared(1024, Duration::ZERO));
        make_room(&mut cache, 1024);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn the_oldest_prepared_capture_is_evicted_first() {
        // Oldest first is the deliberate choice: the capture that has waited
        // longest is the likeliest to have been overtaken by an unlock, after
        // which the ordinary decrypt path can encode it for free.
        let half = CAPTURE_PIXEL_BUDGET_BYTES / 2;
        let mut cache = HashMap::new();
        cache.insert("old".to_string(), prepared(half, Duration::from_secs(600)));
        cache.insert("new".to_string(), prepared(half, Duration::from_secs(1)));
        make_room(&mut cache, half);
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("new"));
    }

    #[test]
    fn an_entry_larger_than_the_whole_budget_does_not_spin_the_eviction_loop() {
        // Guards the loop's termination: without the early return, an incoming
        // entry that can never fit would evict everything and then keep looking
        // for something else to drop.
        let mut cache = HashMap::new();
        cache.insert("a".to_string(), prepared(1024, Duration::ZERO));
        make_room(&mut cache, CAPTURE_PIXEL_BUDGET_BYTES + 1);
        assert!(cache.is_empty());
    }

    #[test]
    fn expired_prepared_pixels_are_dropped() {
        // The TTL is what bounds how long plaintext screen thumbnails live in
        // process memory, so it has to actually fire.
        let mut cache = HashMap::new();
        cache.insert(
            "stale".to_string(),
            prepared(16, CAPTURE_PIXEL_TTL + Duration::from_secs(1)),
        );
        cache.insert("fresh".to_string(), prepared(16, Duration::from_secs(5)));
        drop_expired(&mut cache);
        assert!(!cache.contains_key("stale"));
        assert!(cache.contains_key("fresh"));
    }

    #[test]
    fn the_pixel_budget_holds_a_meaningful_number_of_captures() {
        // The budget is stated in bytes but reasoned about in screenshots; if
        // the CLIP input ever grew, this is what would catch the budget
        // silently becoming a handful of images.
        let entry = 224 * 224 * 3;
        assert!(CAPTURE_PIXEL_BUDGET_BYTES / entry >= 400);
    }

    #[test]
    fn one_image_per_chunk_bounds_the_foreground_wait() {
        // A CLIP forward pass cannot be interrupted once submitted, so the
        // chunk size *is* the worst case a search arriving mid-pass inherits.
        assert_eq!(ENCODE_CHUNK, 1);
        assert!(DRAIN_BATCH >= ENCODE_CHUNK);
    }

    #[test]
    fn the_embed_timeout_covers_a_cold_model_load() {
        // The CLIP ONNX file is 177 MB and is verified in full on every swap,
        // and the load lands inside the first request of a pass.
        assert!(EMBED_TIMEOUT >= Duration::from_secs(120));
    }

    #[test]
    fn clip_vectors_are_the_declared_width() {
        use crate::clip_migration::CLIP_DIMENSIONS;
        // Guards against the index quietly accepting a MiniLM row if a caller
        // ever passed the wrong model.
        assert_eq!(CLIP_DIMENSIONS, 512);
        assert!(validate_clip_vector(&vec![0.1; CLIP_DIMENSIONS]).is_ok());
        assert!(validate_clip_vector(&vec![0.1; 384]).is_err());
    }

    fn estimate_one(megapixels: f64) -> f64 {
        ENCODE_FIXED_SECONDS + megapixels * ENCODE_SECONDS_PER_MEGAPIXEL
    }

    #[test]
    fn terminal_backfill_states_do_not_request_full_history_diagnostics() {
        let migrating = terminal_backfill_offer(false, None);
        assert!(!migrating.migration_settled);
        assert!(!migrating.should_ask);
        assert_eq!(migrating.never_indexed, 0);

        let decided = terminal_backfill_offer(true, Some(BACKFILL_DECLINED.to_string()));
        assert_eq!(decided.decision.as_deref(), Some(BACKFILL_DECLINED));
        assert!(!decided.should_ask);
        assert_eq!(decided.estimated_seconds, 0);

        let deferred = deferred_backfill_offer();
        assert!(deferred.migration_settled);
        assert!(deferred.diagnostics_deferred);
        assert!(!deferred.should_ask);
    }

    #[test]
    fn the_assumed_size_is_the_measured_1080p_point() {
        // Rows written before `screenshots.width`/`height` existed are counted
        // at this size, so it has to be one of the measured points rather than
        // a round number.
        assert!((ASSUMED_MEGAPIXELS - 1920.0 * 1080.0 / 1_000_000.0).abs() < 1e-9);
        assert!((estimate_one(ASSUMED_MEGAPIXELS) - 0.67).abs() < 0.02);
    }

    #[test]
    fn the_repair_scan_window_is_a_bound_sqlite_modifier() {
        // Bound as a parameter by `list_clip_image_index_candidates`, never
        // interpolated: a literal that stopped being a constant would otherwise
        // be an injection point in a datetime modifier.
        assert!(REPAIR_SCAN_WINDOW.starts_with('-'));
        assert!(REPAIR_SCAN_WINDOW.ends_with("days"));
    }

    #[test]
    fn only_an_approved_backfill_widens_the_scan_to_the_whole_history() {
        // An unfinished migration suspends the scan outright: that is the case
        // where "no ledger row" means "the copy has not reached it yet", and
        // answering it by re-encoding is what this gate exists to stop. A
        // recorded answer cannot override that, because the copy is cheaper
        // than the backfill and has not had its chance.
        assert_eq!(scope_from_state(false, None), RepairScope::Suspended);
        assert_eq!(
            scope_from_state(false, Some(BACKFILL_APPROVED)),
            RepairScope::Suspended
        );
        // Settled but unanswered stays narrow, so nothing sweeps the history
        // before somebody is shown what it would cost.
        assert_eq!(scope_from_state(true, None), RepairScope::Recent);
        assert_eq!(
            scope_from_state(true, Some(BACKFILL_DECLINED)),
            RepairScope::Recent
        );
        assert_eq!(
            scope_from_state(true, Some(BACKFILL_APPROVED)),
            RepairScope::Full
        );
        // An unreadable value is not an approval.
        assert_eq!(scope_from_state(true, Some("yes")), RepairScope::Recent);

        assert_eq!(scan_window(RepairScope::Full), None);
        assert_eq!(scan_window(RepairScope::Recent), Some(REPAIR_SCAN_WINDOW));
    }

    #[test]
    fn a_skipped_row_is_not_reported_as_a_failed_one() {
        // Deleting a screenshot orphans its Chroma id, so any collection with
        // deletion history produces these in quantity. Counting them as
        // failures would tell every such user that their migration broke.
        for code in EXPECTED_SKIP_CODES {
            assert_ne!(*code, diagnostic_code::LEGACY_VECTOR_DECODE_FAILED);
            assert_ne!(*code, diagnostic_code::INVALID_VECTOR);
        }
        assert!(EXPECTED_SKIP_CODES.contains(&diagnostic_code::ORPHAN_DOCUMENT_ID));
        // The run-level code belongs to neither column.
        assert!(!EXPECTED_SKIP_CODES.contains(&RUN_LEVEL_CODE));
    }
}
