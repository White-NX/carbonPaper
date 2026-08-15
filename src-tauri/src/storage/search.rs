//! Text search: blind bigram bitmap recall, then a rerank on decrypted text.
//!
//! The index can only answer "which text blocks contain this bigram". Two
//! things follow, and the shape of this module is the consequence of both.
//!
//! Recall has to *narrow*. Intersecting posting lists rarest first, and
//! stopping as soon as the running set is small, means the selective bigrams
//! of a query do all the work and the common ones are never read. The previous
//! implementation unioned every bigram of every keyword instead, which for
//! `Six Degrees of Separation` meant walking most of the table before the
//! first row was ranked.
//!
//! Ranking has to *verify*. Bigram membership says nothing about order, so a
//! paragraph scattering all nine bigrams of `Separation` looks exactly like a
//! block that says the word. Once a candidate's text is decrypted there is no
//! reason to keep guessing, so every ordering decision the user sees is made
//! against the real characters — see [`search_rank`].

use crate::credential_manager::decrypt_with_master_key;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use roaring::RoaringBitmap;
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::{Duration, Instant};

use super::screenshot::unwrap_batch_parallel;
use super::search_plan::{BigramGroup, QueryPlan};
use super::search_rank;
use super::{wire_time, BackgroundReadError, SearchResult, StorageState};

type SearchSqlParam = Box<dyn ToSql>;

/// A recalled row id together with how many of the query's bigram groups it
/// matched. Every pass but the typo-tolerant one reports "all of them".
type GroupHit = (u32, u32);

/// What the typo-tolerant pass found: the surviving rows, and how many groups
/// each of them was checked against.
type FuzzyRecall = (Vec<GroupHit>, u32);

/// A search slow enough to report at `warn`, so the default log level catches
/// it without anyone having to reproduce the query with `RUST_LOG` turned up.
const SLOW_SEARCH_REPORT: Duration = Duration::from_secs(5);

/// Values per `IN (...)` statement. Comfortably under SQLite's parameter
/// ceiling and large enough that the bounded passes below issue one statement.
const SQL_CHUNK: usize = 500;

/// Posting lists larger than this are left on disk once the intersection has
/// something to work with.
///
/// Measured on a 2.3 GiB library: the nine bigrams of `Separation` came to
/// 1.4 MiB in total and cost 27 ms to read and deserialize, so the ceiling
/// sits far above an ordinary bigram. It only bites on the handful — `on`,
/// `at`, `re` — that cover a large fraction of every text block, and those are
/// exactly the ones an intersection learns nothing from.
const MAX_POSTING_BYTES: usize = 4 * 1024 * 1024;

/// The running intersection stops once it is this small.
///
/// Every surviving candidate is checked against decrypted text anyway, so
/// intersecting further trades a certain deserialization cost for an uncertain
/// reduction. This is what keeps the common bigrams of a long word from ever
/// being read.
const INTERSECT_EARLY_STOP: u64 = 256;

/// …but never before this many groups have been applied, so a long query is
/// not declared narrow enough on the strength of its first bigram.
const MIN_GROUPS_BEFORE_EARLY_STOP: usize = 3;

/// Rows considered by recall per row the caller asked for.
///
/// The first page fixes the result order cached for later pages, but recall
/// still only needs enough evidence to fill that initial request. The floor
/// gives ranking room to discard bitmap false positives without making every
/// small search escalate through all tiers.
const VERIFY_PER_REQUESTED_ROW: usize = 4;
const VERIFY_FLOOR: usize = 64;

/// Hard ceiling on rows decrypted for one search. A query broad enough to
/// exceed it is ranked on its most recent candidates and reports
/// `complete=false`, rather than turning into thousands of RSA operations.
const VERIFY_CAP: usize = 800;

/// Bigram groups a typo is allowed to destroy. One misread character kills the
/// two bigrams it took part in, so two covers a single OCR error.
const MAX_FUZZY_TOLERANCE: usize = 2;

/// Candidates the typo-tolerant pass may propose. Beyond this it lowers its
/// tolerance rather than hand the ranker a set it cannot afford to check.
const FUZZY_UNION_CAP: u64 = 20_000;

/// Posting-list bytes the typo-tolerant pass may deserialize while building its
/// seed and counting how much of the query each candidate matched.
///
/// The pass now runs on every search rather than only when the strict passes
/// came up short, because it is the only one that can propose a row the query
/// misspells — and whether a row is a misspelling is a question only the
/// decrypted text can answer. Running always means bounding always. Two
/// mebibytes is roughly 40 ms at the measured 20 ms per mebibyte, and it is
/// comfortably more than a whole ordinary query needs: the nine bigrams of
/// `Separation` came to 1.4 MiB between them. Lists already in memory from an
/// earlier pass cost nothing against it.
const FUZZY_LOAD_BYTES: u64 = 2 * 1024 * 1024;

/// The share of the decryption budget held for the typo-tolerant pass.
///
/// One in four, so a strict pass that fills the budget with blocks that merely
/// scatter the query's bigrams cannot starve the near misses out. See
/// `reserve_for_fuzzy`.
const FUZZY_BUDGET_SHARE: usize = 4;

/// Blocks the cross-block pass resolves to screenshots. Bounds both the
/// statements it issues and the containment checks that follow.
const SCREENSHOT_PASS_CAP: usize = 2_000;

/// Rows a query too short for the index may read directly.
///
/// A single character produces no bigram, so nothing in the index describes
/// it. Rather than answer with recent captures that do not contain it — which
/// is what used to happen — such a query reads the newest blocks and checks
/// them, within the same budget any other search spends on decryption. The
/// window is narrow, and deliberately so: a complete answer needs a unigram
/// index, which is a different piece of work.
const SCAN_WINDOW: usize = VERIFY_CAP;

/// How long a computed result order stays reusable.
///
/// Long enough that scrolling through pages of one search never recomputes it,
/// short enough that a capture taken during the scroll shows up on the next
/// search rather than being hidden for the session.
const SEARCH_ORDER_TTL: Duration = Duration::from_secs(60);

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Splits a sequential run of work into named stages.
///
/// `lap` returns the time since the previous `lap` — or since construction —
/// and re-arms, so a loop body can charge each pass to the right stage.
struct Stopwatch(Instant);

impl Stopwatch {
    fn start() -> Self {
        Self(Instant::now())
    }

    fn lap(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = now - self.0;
        self.0 = now;
        elapsed
    }
}

/// How much work one search actually did.
///
/// Counts matter as much as durations: a query is slow either because it
/// touched an enormous number of rows or because it decrypted a handful of
/// expensive ones, and those two call for opposite fixes.
#[derive(Default)]
struct SearchCounts {
    keywords: usize,
    bigrams: usize,
    /// Token hashes asked about, and how many the index held.
    probed: usize,
    present: usize,
    /// Posting lists deserialized, and their serialized size.
    loaded: usize,
    loaded_bytes: u64,
    /// Candidate rows recall proposed, after filtering.
    candidates: u64,
    /// …of which the typo-tolerant pass contributed. A query spelled the way
    /// the capture spells it should show a small number here; a large one
    /// means the strict passes found little and the answer rests on tolerance.
    tolerated: usize,
    /// `IN (...)` statements issued outside the final page fetch.
    statements: usize,
    /// Rows decrypted for ranking, and how many the ranking kept.
    verified: usize,
    kept: usize,
    /// Kept rows whose text matched only within the edit budget rather than
    /// literally. Worth watching: this is the recall the plaintext rerank
    /// would otherwise have thrown away, and a query spelled correctly should
    /// show few of them.
    near: usize,
    rows_returned: usize,
}

#[derive(Default)]
struct SearchStages {
    /// Fetching the HMAC key and opening the read connection.
    setup: Duration,
    /// Resolving the process, category and time filters.
    filters: Duration,
    /// Splitting the query and computing its bigram groups.
    planning: Duration,
    /// Asking the index which token hashes exist and how large they are.
    probing: Duration,
    /// Reading posting lists and deserializing them.
    bitmaps: Duration,
    /// Intersections, unions and candidate ordering.
    combining: Duration,
    /// Mapping candidate blocks onto screenshots.
    resolving: Duration,
    /// Decrypting candidates and scoring them.
    verifying: Duration,
    /// Fetching the page and decrypting the rest of its columns.
    materializing: Duration,
}

/// Per-query timing report for [`StorageState::search_text`], emitted on drop.
///
/// The search has many exits, and several are an early `return Ok(vec![])`
/// taken *after* the expensive part. Reporting from `Drop` means the branch
/// that loads a hundred megabytes of posting lists and then finds nothing is
/// measured like any other, which is exactly the case worth seeing.
struct SearchTelemetry {
    started: Instant,
    /// The deepest tier the search had to escalate to, not necessarily the one
    /// the returned rows came from — a phrase hit that did not fill the page
    /// still reports the tier that was tried next.
    tier: &'static str,
    cached: bool,
    /// Whether every candidate was ranked. `false` means the answer is the
    /// best of the most recent candidates rather than the best overall.
    complete: bool,
    counts: SearchCounts,
    stages: SearchStages,
}

impl SearchTelemetry {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            tier: "unresolved",
            cached: false,
            complete: true,
            counts: SearchCounts::default(),
            stages: SearchStages::default(),
        }
    }
}

impl Drop for SearchTelemetry {
    fn drop(&mut self) {
        let total = self.started.elapsed();
        let report = format!(
            "[SEARCH] tier={} cached={} complete={} total={:.1}ms keywords={} bigrams={} \
             probe={}/{} load={} ({:.1}KiB) candidates={} tolerated={} stmts={} verified={} \
             kept={} near={} rows={} | \
             setup={:.1} filters={:.1} plan={:.1} probe={:.1} bitmaps={:.1} combine={:.1} \
             resolve={:.1} verify={:.1} page={:.1}",
            self.tier,
            self.cached,
            self.complete,
            millis(total),
            self.counts.keywords,
            self.counts.bigrams,
            self.counts.present,
            self.counts.probed,
            self.counts.loaded,
            self.counts.loaded_bytes as f64 / 1024.0,
            self.counts.candidates,
            self.counts.tolerated,
            self.counts.statements,
            self.counts.verified,
            self.counts.kept,
            self.counts.near,
            self.counts.rows_returned,
            millis(self.stages.setup),
            millis(self.stages.filters),
            millis(self.stages.planning),
            millis(self.stages.probing),
            millis(self.stages.bitmaps),
            millis(self.stages.combining),
            millis(self.stages.resolving),
            millis(self.stages.verifying),
            millis(self.stages.materializing),
        );

        if total >= SLOW_SEARCH_REPORT {
            tracing::warn!("{}", report);
        } else {
            tracing::debug!("{}", report);
        }
    }
}

/// How a candidate was reached. Ordered from most to least evidence, which is
/// also the order candidates are cut in when there are more than the ranker
/// can afford to decrypt.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tier {
    /// Every bigram of the whole query — cross-word ones included — in one
    /// text block. The narrowest thing the index can be asked.
    Phrase,
    /// Every bigram of every keyword in one text block, without requiring the
    /// words to be adjacent.
    Block,
    /// Each keyword in some block of the same screenshot.
    Screenshot,
    /// All but a couple of bigram groups, which is what an OCR typo looks
    /// like from the index's side.
    Fuzzy,
    /// Read straight from the newest blocks because the query is too short to
    /// have an index entry at all.
    Scan,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Tier::Phrase => "phrase",
            Tier::Block => "block",
            Tier::Screenshot => "screenshot",
            Tier::Fuzzy => "fuzzy",
            Tier::Scan => "scan",
        }
    }
}

/// One row recall proposed, with the evidence behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Candidate {
    ocr_id: i64,
    tier: Tier,
    /// Bigram groups matched, out of how many were checked. Only the fuzzy
    /// tier reports anything other than "all of them".
    hits: u32,
    total: u32,
}

/// A bigram's lookups, before the index has been asked which exist.
struct PlannedGroup {
    bigram: String,
    hashes: Vec<String>,
}

/// The same after probing: only the variants the index holds, each with the
/// serialized size that stands in for its cardinality.
struct HashedGroup {
    bigram: String,
    present: Vec<(String, usize)>,
}

impl HashedGroup {
    /// Upper bound on what deserializing this group costs, and the key the
    /// intersection orders by. Bytes rather than cardinality because SQLite
    /// can report a blob's length without reading it.
    fn bytes(&self) -> usize {
        self.present.iter().map(|(_, size)| *size).sum()
    }

    fn is_present(&self) -> bool {
        !self.present.is_empty()
    }
}

fn plan_groups<'a>(
    groups: impl IntoIterator<Item = &'a BigramGroup>,
    hmac_key: &[u8],
) -> Vec<PlannedGroup> {
    groups
        .into_iter()
        .map(|group| PlannedGroup {
            bigram: group.bigram.clone(),
            hashes: group
                .variants
                .iter()
                .map(|variant| StorageState::compute_hmac_hash(variant, hmac_key))
                .collect(),
        })
        .collect()
}

/// Posting lists, read once and kept for the passes that follow.
///
/// A bigram of the query is asked for by the phrase pass, again by the keyword
/// pass and possibly a third time by the typo-tolerant pass. Reading it once
/// is the difference between three deserializations and one.
struct PostingStore<'a> {
    conn: &'a Connection,
    sizes: HashMap<String, usize>,
    cache: HashMap<String, Rc<RoaringBitmap>>,
}

impl<'a> PostingStore<'a> {
    fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            sizes: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Asks the index which of these hashes exist and how large they are, in
    /// one statement per [`SQL_CHUNK`].
    ///
    /// `length(postings_blob)` is the point of this: SQLite answers it from
    /// the record header, so the planner can order its work by posting-list
    /// size without deserializing — or even transferring — a single bitmap.
    fn probe(&mut self, hashes: &[String], counts: &mut SearchCounts) -> Result<(), String> {
        for chunk in hashes.chunks(SQL_CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT token_hash, length(postings_blob)
                   FROM blind_bitmap_index WHERE token_hash IN ({placeholders})"
            );
            let params: Vec<&dyn ToSql> = chunk.iter().map(|hash| hash as &dyn ToSql).collect();
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare posting probe: {}", e))?;
            let rows = stmt
                .query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| format!("Failed to probe postings: {}", e))?;
            for (hash, size) in rows.filter_map(Result::ok) {
                self.sizes.insert(hash, size.max(0) as usize);
            }
            counts.probed += chunk.len();
            counts.statements += 1;
        }
        counts.present = self.sizes.len();
        Ok(())
    }

    fn resolve(&self, planned: &[PlannedGroup]) -> Vec<HashedGroup> {
        planned
            .iter()
            .map(|group| HashedGroup {
                bigram: group.bigram.clone(),
                present: group
                    .hashes
                    .iter()
                    .filter_map(|hash| self.sizes.get(hash).map(|size| (hash.clone(), *size)))
                    .collect(),
            })
            .collect()
    }

    /// The union of a bigram's case variants, deserialized at most once.
    /// Whether this group's posting list is already in memory, so a caller
    /// working to a byte budget knows it costs nothing to use again.
    fn is_loaded(&self, group: &HashedGroup) -> bool {
        self.cache.contains_key(&group.bigram)
    }

    fn load(
        &mut self,
        group: &HashedGroup,
        counts: &mut SearchCounts,
    ) -> Result<Rc<RoaringBitmap>, String> {
        if let Some(cached) = self.cache.get(&group.bigram) {
            return Ok(Rc::clone(cached));
        }

        let mut union = RoaringBitmap::new();
        for (hash, _) in &group.present {
            let blob: Option<Vec<u8>> = self
                .conn
                .query_row(
                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                    params![hash],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("Failed to query bitmap: {}", e))?;
            let Some(blob) = blob else { continue };
            counts.loaded += 1;
            counts.loaded_bytes += blob.len() as u64;
            let bitmap = RoaringBitmap::deserialize_from(&blob[..])
                .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
            union |= bitmap;
        }

        let shared = Rc::new(union);
        self.cache.insert(group.bigram.clone(), Rc::clone(&shared));
        Ok(shared)
    }
}

/// Intersects bigram groups rarest first, stopping once the result is small.
///
/// `tolerate_missing` decides what an unindexed bigram means. Fuzzy searches
/// skip it — the spelling was probably never captured — while strict ones
/// treat it as proof that nothing can match.
///
/// Returns `None` when the query cannot be answered from the index at all,
/// which is different from an empty intersection.
fn intersect_groups(
    store: &mut PostingStore<'_>,
    groups: &[&HashedGroup],
    tolerate_missing: bool,
    counts: &mut SearchCounts,
) -> Result<Option<RoaringBitmap>, String> {
    let mut ordered: Vec<&HashedGroup> = Vec::with_capacity(groups.len());
    for group in groups {
        if group.is_present() {
            ordered.push(group);
        } else if !tolerate_missing {
            return Ok(None);
        }
    }
    if ordered.is_empty() {
        return Ok(None);
    }
    ordered.sort_by_key(|group| group.bytes());

    let mut running: Option<RoaringBitmap> = None;
    let mut applied = 0usize;
    for group in ordered {
        // The first group has to be read whatever it costs; after that a
        // posting list this large cannot narrow anything worth the read.
        if applied > 0 && group.bytes() > MAX_POSTING_BYTES {
            break;
        }
        let bitmap = store.load(group, counts)?;
        running = Some(match running.take() {
            None => (*bitmap).clone(),
            Some(mut accumulated) => {
                accumulated &= &*bitmap;
                accumulated
            }
        });
        applied += 1;

        let remaining = running.as_ref().map_or(0, RoaringBitmap::len);
        if remaining == 0 {
            break;
        }
        if applied >= MIN_GROUPS_BEFORE_EARLY_STOP && remaining <= INTERSECT_EARLY_STOP {
            break;
        }
    }

    Ok(running)
}

/// Candidates matching all but a couple of the query's bigram groups.
///
/// A row matching at least `k` of `n` groups must appear in at least one of
/// any `n - k + 1` of them, so unioning the *rarest* `n - k + 1` is a sound
/// superset — and, because they are the rarest, a small one. The previous
/// fuzzy mode unioned all `n`, common bigrams included, which is why searching
/// one long word walked several hundred thousand rows.
///
/// This is what reaches a block the query misspells. One dropped character
/// destroys the two bigrams it took part in, so `Separtion` shares seven of
/// `Separation`'s bigrams and `中华人共和国` four of `中华人民共和国`'s — near
/// enough for the tolerance, and out of reach of any pass that insists on all
/// of them. Deciding whether such a row really is the word the user meant is
/// left to the plaintext rerank, which can see the characters; see
/// `search_rank.rs`.
///
/// Returns the surviving rows with the number of groups each matched, or
/// `None` when the query is too short for a tolerance to mean anything or the
/// superset would be too large to check.
fn fuzzy_candidates(
    store: &mut PostingStore<'_>,
    groups: &[&HashedGroup],
    allowed: Option<&RoaringBitmap>,
    counts: &mut SearchCounts,
) -> Result<Option<FuzzyRecall>, String> {
    let mut ordered: Vec<&HashedGroup> = groups
        .iter()
        .copied()
        .filter(|group| group.is_present())
        .collect();
    ordered.sort_by_key(|group| group.bytes());

    // The seed union is the first thing this pass loads. Keep oversized or
    // unaffordable groups out of it as well as out of the counting pass below;
    // otherwise the budget only protects the second half of the algorithm.
    let affordable: Vec<&HashedGroup> = ordered
        .iter()
        .copied()
        .filter(|group| {
            store.is_loaded(group)
                || (group.bytes() <= MAX_POSTING_BYTES && group.bytes() as u64 <= FUZZY_LOAD_BYTES)
        })
        .collect();
    let mut tolerance = (ordered.len() / 4)
        .min(MAX_FUZZY_TOLERANCE)
        .min(affordable.len().saturating_sub(1));
    if tolerance == 0 {
        return Ok(None);
    }

    // Keep one budget for both the seed union and the later counting pass.
    // Groups loaded by the seed are cached, so their second use costs zero.
    let mut spent = 0u64;
    let superset = loop {
        let mut union = RoaringBitmap::new();
        let mut loaded_groups = 0usize;
        for group in affordable.iter().take(tolerance + 1) {
            let cost = if store.is_loaded(group) {
                0
            } else {
                group.bytes() as u64
            };
            if spent.saturating_add(cost) > FUZZY_LOAD_BYTES {
                break;
            }
            spent += cost;
            union |= &*store.load(group, counts)?;
            loaded_groups += 1;
        }
        if loaded_groups < tolerance + 1 {
            if tolerance == 0 {
                return Ok(None);
            }
            tolerance -= 1;
            if tolerance == 0 {
                return Ok(None);
            }
            continue;
        }
        let filtered = match allowed {
            Some(filter) => &union & filter,
            None => union,
        };
        if filtered.len() <= FUZZY_UNION_CAP {
            break filtered;
        }
        tolerance -= 1;
        if tolerance == 0 {
            return Ok(None);
        }
    };

    let mut hits: HashMap<u32, u32> = HashMap::with_capacity(superset.len() as usize);
    let mut checked = 0u32;
    for group in &ordered {
        // Groups are ordered rarest first, so the budget buys the most
        // selective evidence and the ones it cannot afford are the common
        // bigrams that would have separated nothing. Leaving them out lowers
        // the bar every candidate has to clear rather than skewing it.
        let cost = if store.is_loaded(group) {
            0
        } else {
            group.bytes() as u64
        };
        if !store.is_loaded(group)
            && (group.bytes() > MAX_POSTING_BYTES || spent.saturating_add(cost) > FUZZY_LOAD_BYTES)
        {
            continue;
        }
        spent = spent.saturating_add(cost);
        let bitmap = store.load(group, counts)?;
        for id in (&*bitmap & &superset).iter() {
            *hits.entry(id).or_insert(0) += 1;
        }
        checked += 1;
    }

    let required = checked.saturating_sub(tolerance as u32).max(1);
    let kept: Vec<GroupHit> = hits
        .into_iter()
        .filter(|(_, count)| *count >= required)
        .collect();
    if checked == 0 {
        return Ok(None);
    }
    Ok(Some((kept, checked)))
}

/// The `cap` highest ids of a bitmap, largest first.
///
/// Roaring iterates ascending and row ids grow with capture time, so this is
/// "the most recent `cap` candidates" — the right ones to keep when a set is
/// too large to check in full.
fn newest_ids(bitmap: &RoaringBitmap, cap: usize) -> Vec<i64> {
    if cap == 0 {
        return Vec::new();
    }
    let mut window: VecDeque<u32> = VecDeque::with_capacity(cap + 1);
    for id in bitmap.iter() {
        if window.len() == cap {
            window.pop_front();
        }
        window.push_back(id);
    }
    window.into_iter().rev().map(|id| id as i64).collect()
}

fn distinct_screenshots_of(
    conn: &Connection,
    ocr_ids: &[i64],
    counts: &mut SearchCounts,
) -> Result<Vec<i64>, String> {
    let mut screenshots: HashSet<i64> = HashSet::new();
    for chunk in ocr_ids.chunks(SQL_CHUNK) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT DISTINCT screenshot_id FROM ocr_results
              WHERE id IN ({placeholders}) AND is_deleted = 0"
        );
        let params: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare screenshot resolve: {}", e))?;
        let rows = stmt
            .query_map(params.as_slice(), |row| row.get::<_, i64>(0))
            .map_err(|e| format!("Failed to resolve screenshot ids: {}", e))?;
        screenshots.extend(rows.filter_map(Result::ok));
        counts.statements += 1;
    }
    Ok(screenshots.into_iter().collect())
}

/// Blocks of screenshots that hold every keyword, each keyword possibly in a
/// different block.
///
/// Bounded on purpose. The previous implementation turned every candidate of
/// every keyword into screenshot ids five hundred at a time, so a four-word
/// query issued thousands of statements before anything was ranked. Here only
/// the rarest keyword is resolved; the others are answered by asking their
/// bitmaps whether they contain a block of that screenshot, which costs
/// nothing.
fn screenshot_pass(
    conn: &Connection,
    per_keyword: &[RoaringBitmap],
    allowed: Option<&RoaringBitmap>,
    counts: &mut SearchCounts,
) -> Result<Vec<i64>, String> {
    if per_keyword.len() < 2 || per_keyword.iter().any(RoaringBitmap::is_empty) {
        return Ok(Vec::new());
    }
    let Some(seed) = per_keyword.iter().min_by_key(|bitmap| bitmap.len()) else {
        return Ok(Vec::new());
    };

    let seed_ids = match allowed {
        Some(filter) => {
            let filtered = seed & filter;
            newest_ids(&filtered, SCREENSHOT_PASS_CAP)
        }
        None => newest_ids(seed, SCREENSHOT_PASS_CAP),
    };
    let screenshots = distinct_screenshots_of(conn, &seed_ids, counts)?;
    if screenshots.is_empty() {
        return Ok(Vec::new());
    }

    let mut blocks_by_screenshot: HashMap<i64, Vec<i64>> = HashMap::new();
    for chunk in screenshots.chunks(SQL_CHUNK) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, screenshot_id FROM ocr_results
              WHERE screenshot_id IN ({placeholders}) AND is_deleted = 0"
        );
        let params: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare block lookup: {}", e))?;
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("Failed to look up blocks: {}", e))?;
        for (block, screenshot) in rows.filter_map(Result::ok) {
            blocks_by_screenshot
                .entry(screenshot)
                .or_default()
                .push(block);
        }
        counts.statements += 1;
    }

    let mut matched: Vec<i64> = Vec::new();
    for blocks in blocks_by_screenshot.values() {
        let mut hit_blocks: Vec<i64> = Vec::new();
        let complete = per_keyword.iter().all(|keyword| {
            let mut found = false;
            for block in blocks {
                if keyword.contains(*block as u32) {
                    hit_blocks.push(*block);
                    found = true;
                }
            }
            found
        });
        if complete {
            matched.extend(hit_blocks);
        }
    }
    // One seed can expand to every matching block in its screenshot. Apply
    // the row filter to that expansion before the caller's verification cap.
    if let Some(filter) = allowed {
        matched.retain(|id| filter.contains(*id as u32));
    }
    matched.sort_unstable();
    matched.dedup();
    Ok(matched)
}

/// OCR rows whose screenshot passes the process, category and time filters.
///
/// `None` means no filter is set, which is the common case and skips the query
/// entirely. Building this as a bitmap applies the filter to the whole
/// candidate set at once, before pagination and before anything is decrypted.
/// The previous code resolved candidates to screenshots five hundred at a time
/// and only checked the time bound *after* decrypting a page, so a search with
/// a date range could return a short page while matches remained.
fn allowed_ocr_rows(
    conn: &Connection,
    process_names: Option<&[String]>,
    categories: Option<&[String]>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    counts: &mut SearchCounts,
) -> Result<Option<RoaringBitmap>, String> {
    let processes = process_names.filter(|names| !names.is_empty());
    let categories = categories.filter(|values| !values.is_empty());
    if processes.is_none() && categories.is_none() && start_time.is_none() && end_time.is_none() {
        return Ok(None);
    }

    // With a process filter, fixing screenshots as the outer loop lets the
    // process index narrow captures before their blocks are visited.
    let from_clause = if processes.is_some() {
        "FROM screenshots s INDEXED BY idx_screenshots_process_deleted_created_at
         CROSS JOIN ocr_results r INDEXED BY idx_ocr_deleted_screenshot
                    ON r.screenshot_id = s.id"
    } else {
        "FROM screenshots s JOIN ocr_results r ON r.screenshot_id = s.id"
    };
    let mut clauses = vec![
        "s.is_deleted = 0".to_string(),
        "r.is_deleted = 0".to_string(),
    ];
    let mut params: Vec<SearchSqlParam> = Vec::new();

    if let Some(names) = processes {
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        clauses.push(format!("s.process_name IN ({placeholders})"));
        params.extend(
            names
                .iter()
                .cloned()
                .map(|name| Box::new(name) as SearchSqlParam),
        );
    }
    if let Some(values) = categories {
        let placeholders = values.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        clauses.push(format!("s.category IN ({placeholders})"));
        params.extend(
            values
                .iter()
                .cloned()
                .map(|category| Box::new(category) as SearchSqlParam),
        );
    }
    if let Some(start) = start_time {
        clauses.push("s.created_at >= ?".to_string());
        params.push(Box::new(sql_timestamp(start)));
    }
    if let Some(end) = end_time {
        clauses.push("s.created_at <= ?".to_string());
        params.push(Box::new(sql_timestamp(end)));
    }

    let sql = format!("SELECT r.id {from_clause} WHERE {}", clauses.join(" AND "));
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare filter query: {}", e))?;
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
        .map_err(|e| format!("Failed to execute filter query: {}", e))?;

    let mut allowed = RoaringBitmap::new();
    for id in rows.filter_map(Result::ok) {
        allowed.insert(id as u32);
    }
    counts.statements += 1;
    Ok(Some(allowed))
}

fn sql_timestamp(seconds: f64) -> String {
    DateTime::<Utc>::from_timestamp(seconds as i64, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn sqlite_data_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .map_err(|e| format!("Failed to read database data version: {}", e))
}

fn build_empty_search_sql(
    process_names: Option<&[String]>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    categories: Option<&[String]>,
    limit: i32,
    offset: i32,
) -> (String, Vec<SearchSqlParam>) {
    let has_process_filter = process_names.is_some_and(|names| !names.is_empty());
    let from_clause = if has_process_filter {
        // CROSS JOIN fixes screenshots as the outer loop, so the process/time
        // index narrows candidates before OCR rows are visited and sorted.
        "FROM screenshots s INDEXED BY idx_screenshots_process_deleted_created_at
         CROSS JOIN ocr_results r INDEXED BY idx_ocr_deleted_screenshot
                    ON r.screenshot_id = s.id"
    } else {
        "FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id"
    };
    let mut sql = format!("SELECT {SEARCH_RESULT_COLUMNS} {from_clause}");
    let mut where_clauses = vec![
        "s.is_deleted = 0".to_string(),
        "r.is_deleted = 0".to_string(),
    ];
    let mut params: Vec<SearchSqlParam> = Vec::new();

    if let Some(names) = process_names.filter(|names| !names.is_empty()) {
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        where_clauses.push(format!("s.process_name IN ({placeholders})"));
        params.extend(
            names
                .iter()
                .cloned()
                .map(|name| Box::new(name) as SearchSqlParam),
        );
    }
    if let Some(start) = start_time {
        where_clauses.push("s.created_at >= ?".to_string());
        params.push(Box::new(sql_timestamp(start)));
    }
    if let Some(end) = end_time {
        where_clauses.push("s.created_at <= ?".to_string());
        params.push(Box::new(sql_timestamp(end)));
    }
    if let Some(categories) = categories.filter(|categories| !categories.is_empty()) {
        let placeholders = categories.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        where_clauses.push(format!("s.category IN ({placeholders})"));
        params.extend(
            categories
                .iter()
                .cloned()
                .map(|category| Box::new(category) as SearchSqlParam),
        );
    }

    sql.push_str(" WHERE ");
    sql.push_str(&where_clauses.join(" AND "));
    sql.push_str(" ORDER BY s.created_at DESC, r.id DESC LIMIT ? OFFSET ?");
    params.push(Box::new(limit));
    params.push(Box::new(offset));

    (sql, params)
}

/// The projection every search branch selects.
///
/// [`decode_search_row`] reads these positionally and two tests below assert on
/// fixed indices, so all query shapes in this file have to agree on the column
/// list down to its order. One constant makes that true by construction
/// instead of by review.
const SEARCH_RESULT_COLUMNS: &str =
    "r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
         r.box_x1, r.box_y1, r.box_x2, r.box_y2,
         r.box_x3, r.box_y3, r.box_x4, r.box_y4,
         s.image_path, s.window_title_enc, s.process_name,
         s.content_key_encrypted,
         CAST(strftime('%s', r.created_at) AS INTEGER) AS created_ts,
         CAST(strftime('%s', s.created_at) AS INTEGER) AS screenshot_created_ts,
         s.category";

/// One projected row, still encrypted.
///
/// Decoding is kept separate from decryption so the `rusqlite` statement is
/// finished with before any CNG round-trip happens.
struct RawSearchRow {
    id: i64,
    screenshot_id: i64,
    text_enc: Option<Vec<u8>>,
    text_key_enc: Option<Vec<u8>>,
    confidence: f64,
    box_coords: Vec<Vec<f64>>,
    image_path: String,
    window_title_enc: Option<Vec<u8>>,
    process_name: Option<String>,
    screenshot_key_enc: Option<Vec<u8>>,
    created_ts: Option<i64>,
    screenshot_created_ts: Option<i64>,
    category: Option<String>,
}

fn decode_search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSearchRow> {
    Ok(RawSearchRow {
        id: row.get(0)?,
        screenshot_id: row.get(1)?,
        text_enc: row.get(2)?,
        text_key_enc: row.get(3)?,
        confidence: row.get(4)?,
        box_coords: vec![
            vec![row.get(5)?, row.get(6)?],
            vec![row.get(7)?, row.get(8)?],
            vec![row.get(9)?, row.get(10)?],
            vec![row.get(11)?, row.get(12)?],
        ],
        image_path: row.get(13)?,
        window_title_enc: row.get(14)?,
        process_name: row.get(15)?,
        screenshot_key_enc: row.get(16)?,
        created_ts: row.get(17)?,
        screenshot_created_ts: row.get(18)?,
        category: row.get(19)?,
    })
}

/// A candidate's encrypted text, fetched before the ranker can look at it.
struct CandidateRow {
    id: i64,
    screenshot_id: i64,
    screenshot_ts: Option<i64>,
    text_enc: Vec<u8>,
    text_key_enc: Vec<u8>,
}

/// The same, decrypted.
struct VerifiedRow {
    id: i64,
    screenshot_id: i64,
    screenshot_ts: Option<i64>,
    text: String,
}

/// A candidate the ranker has an opinion about.
struct Scored {
    ocr_id: i64,
    screenshot_id: i64,
    screenshot_ts: Option<i64>,
    score: f64,
}

/// The ordered result of a text search, kept just long enough for the next
/// page to arrive.
///
/// Infinite scroll asks for the same query again with a larger offset. Without
/// this, recall and reranking run again for every page with a larger recall
/// budget, so page two can reorder page one's prefix and repeat or skip rows.
pub(super) struct CachedSearchOrder {
    key: String,
    created: Instant,
    /// SQLite invalidates this when another connection commits any write. The
    /// value is meaningful only on `version_connection`, so that connection
    /// stays alive with the cached order instead of comparing values sampled
    /// from unrelated per-search connections.
    data_version: i64,
    version_connection: Connection,
    /// Protects against an old-file search publishing after a database swap.
    reset_generation: u64,
    /// Result rows in final order. A multi-keyword search stores one
    /// representative block per screenshot, so this is always OCR row ids.
    ordered: Vec<i64>,
    /// Whether recall fit inside the verification ceiling. This is diagnostic
    /// only: a partial bounded order is still reused so later pages cannot
    /// reshuffle the prefix the user already saw.
    complete: bool,
}

fn order_cache_key(
    query: &str,
    fuzzy: bool,
    process_names: Option<&[String]>,
    categories: Option<&[String]>,
    start_time: Option<f64>,
    end_time: Option<f64>,
) -> String {
    fn joined(values: Option<&[String]>) -> String {
        let mut values: Vec<&str> = values.unwrap_or(&[]).iter().map(String::as_str).collect();
        values.sort_unstable();
        values.join("\u{1e}")
    }

    // Unit separators keep a process named like the query from colliding with
    // it once everything is one string.
    format!(
        "{fuzzy}\u{1f}{query}\u{1f}{}\u{1f}{}\u{1f}{:?}\u{1f}{:?}",
        joined(process_names),
        joined(categories),
        start_time,
        end_time
    )
}

impl StorageState {
    /// Compute HMAC hash for blind index.
    pub(super) fn compute_hmac_hash(text: &str, hmac_key: &[u8]) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;

        let mut mac =
            HmacSha256::new_from_slice(hmac_key).expect("HMAC key length should be valid");
        mac.update(text.as_bytes());
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    /// Compute static hash for non-sensitive dedup (e.g. icons, link sets)
    pub(crate) fn compute_static_hash(text: &str) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;
        const STATIC_KEY: &[u8] = b"CarbonPaper-Search-HMAC-Key-v1";

        let mut mac =
            HmacSha256::new_from_slice(STATIC_KEY).expect("HMAC key length should be valid");
        mac.update(text.as_bytes());
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    /// Bigram tokenization (punctuation filtered).
    ///
    /// Whitespace is filtered along with punctuation, so the bigrams of a
    /// multi-word line span its word boundaries: `Six Degrees` contributes
    /// `xD`. That is what lets a phrase query ask the index something its
    /// individual words cannot — see `search_plan.rs::QueryPlan`.
    pub(crate) fn bigram_tokenize(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text
            .chars()
            .filter(|c| c.is_alphanumeric() || Self::is_cjk(*c))
            .collect();
        if chars.len() < 2 {
            return HashSet::new(); // ignore texts too short for bigrams
        }

        chars.windows(2).map(|w| w.iter().collect()).collect()
    }

    pub(super) fn is_cjk(ch: char) -> bool {
        let code = ch as u32;
        matches!(
            code,
            0x4E00..=0x9FFF        // CJK Unified Ideographs
            | 0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
            | 0x20000..=0x2A6DF    // Extension B
            | 0x2A700..=0x2B73F    // Extension C
            | 0x2B740..=0x2B81F    // Extension D
            | 0x2B820..=0x2CEAF    // Extension E/F
            | 0xF900..=0xFAFF      // CJK Compatibility Ideographs
            | 0x2F800..=0x2FA1F    // CJK Compatibility Ideographs Supplement
        )
    }

    /// Runs a search projection and turns its rows into decrypted results.
    ///
    /// `sql` is expected to select [`SEARCH_RESULT_COLUMNS`]. Rows are grouped
    /// by screenshot and each group decrypted on a worker holding one CNG
    /// session, so a page pays the provider/key-open round-trip once per
    /// thread rather than once per row — measured at roughly 24 ms per row
    /// before, for a page of forty. `known_text` carries text the ranker has
    /// already decrypted, which is every row of the first page.
    fn materialize_search_rows(
        &self,
        conn: &Connection,
        sql: &str,
        param_refs: &[&dyn ToSql],
        known_text: &HashMap<i64, String>,
    ) -> Result<Vec<SearchResult>, String> {
        let raw_rows: Vec<RawSearchRow> = {
            let mut stmt = conn
                .prepare(sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;
            let rows = stmt
                .query_map(param_refs, decode_search_row)
                .map_err(|e| format!("Failed to execute search query: {}", e))?
                .filter_map(Result::ok)
                .collect();
            rows
        };

        // One batch per screenshot: the content key that decrypts the window
        // title is shared by every text block of a capture, so unwrapping it
        // once per capture removes most of the remaining round-trips.
        let mut order: Vec<i64> = Vec::new();
        let mut grouped: HashMap<i64, Vec<RawSearchRow>> = HashMap::new();
        for raw in raw_rows {
            let screenshot_id = raw.screenshot_id;
            grouped.entry(screenshot_id).or_insert_with(|| {
                order.push(screenshot_id);
                Vec::new()
            });
            grouped
                .get_mut(&screenshot_id)
                .expect("entry was just inserted")
                .push(raw);
        }
        let batches: Vec<Vec<RawSearchRow>> = order
            .into_iter()
            .filter_map(|id| grouped.remove(&id))
            .collect();

        let decrypted = unwrap_batch_parallel(batches, |session, rows| {
            let unwrap = |ciphertext: &[u8]| session.unwrap_row_key(ciphertext);
            let mut screenshot_key = rows
                .first()
                .and_then(|row| row.screenshot_key_enc.as_ref())
                .and_then(|encrypted| unwrap(encrypted).ok());

            let results: Vec<SearchResult> = rows
                .into_iter()
                .map(|raw| {
                    let text = match known_text.get(&raw.id) {
                        Some(text) => text.clone(),
                        None => match (raw.text_enc.as_ref(), raw.text_key_enc.as_ref()) {
                            (Some(data), Some(key)) => {
                                Self::decrypt_payload_with_unwrap(data, key, &unwrap)
                                    .ok()
                                    .and_then(|value| String::from_utf8(value).ok())
                                    .unwrap_or_default()
                            }
                            _ => String::new(),
                        },
                    };
                    let window_title =
                        match (raw.window_title_enc.as_ref(), screenshot_key.as_ref()) {
                            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                                .ok()
                                .and_then(|value| String::from_utf8(value).ok()),
                            _ => None,
                        };

                    SearchResult {
                        id: raw.id,
                        screenshot_id: raw.screenshot_id,
                        text,
                        confidence: raw.confidence,
                        box_coords: raw.box_coords,
                        image_path: raw.image_path,
                        window_title,
                        process_name: raw.process_name,
                        category: raw.category,
                        created_at: wire_time::from_optional_seconds(raw.created_ts),
                        screenshot_created_at: wire_time::from_optional_seconds(
                            raw.screenshot_created_ts,
                        ),
                        timestamp: raw.screenshot_created_ts,
                    }
                })
                .collect();

            if let Some(key) = screenshot_key.as_mut() {
                Self::zeroize_bytes(key);
            }
            Ok(results)
        })
        .map_err(describe_read_error)?;

        Ok(decrypted.into_iter().flatten().collect())
    }

    /// Materializes rows by id and restores the ranking order the SQL
    /// `ORDER BY` cannot express.
    fn materialize_ordered(
        &self,
        conn: &Connection,
        ids: &[i64],
        known_text: &HashMap<i64, String>,
    ) -> Result<Vec<SearchResult>, String> {
        let mut results: Vec<SearchResult> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(SQL_CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT {SEARCH_RESULT_COLUMNS}
                   FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id
                  WHERE r.id IN ({placeholders})
                    AND r.is_deleted = 0
                    AND s.is_deleted = 0"
            );
            let param_refs: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
            results.extend(self.materialize_search_rows(
                conn,
                &sql,
                param_refs.as_slice(),
                known_text,
            )?);
        }

        let rank: HashMap<i64, usize> = ids
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position))
            .collect();
        results.sort_by_key(|result| rank.get(&result.id).copied().unwrap_or(usize::MAX));
        Ok(results)
    }

    /// Fetches the encrypted text of candidate rows, dropping any whose
    /// screenshot or block has been deleted since recall proposed it.
    fn load_candidate_rows(
        &self,
        conn: &Connection,
        ids: &[i64],
        counts: &mut SearchCounts,
    ) -> Result<Vec<CandidateRow>, String> {
        let mut rows: Vec<CandidateRow> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(SQL_CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted,
                        CAST(strftime('%s', s.created_at) AS INTEGER)
                   FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id
                  WHERE r.id IN ({placeholders})
                    AND r.is_deleted = 0
                    AND s.is_deleted = 0"
            );
            let params: Vec<&dyn ToSql> = chunk.iter().map(|id| id as &dyn ToSql).collect();
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare candidate fetch: {}", e))?;
            let fetched = stmt
                .query_map(params.as_slice(), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                })
                .map_err(|e| format!("Failed to fetch candidates: {}", e))?;
            for (id, screenshot_id, text_enc, text_key_enc, screenshot_ts) in
                fetched.filter_map(Result::ok)
            {
                let (Some(text_enc), Some(text_key_enc)) = (text_enc, text_key_enc) else {
                    continue;
                };
                rows.push(CandidateRow {
                    id,
                    screenshot_id,
                    screenshot_ts,
                    text_enc,
                    text_key_enc,
                });
            }
            counts.statements += 1;
        }
        Ok(rows)
    }

    /// Decrypts candidate text, one CNG session per worker thread.
    ///
    /// A row that cannot be decrypted comes back with empty text rather than
    /// failing the search: it will score nothing and be dropped, which is the
    /// same outcome as never having proposed it. A locked session is different
    /// and does fail, because every remaining row would fail the same way.
    fn decrypt_candidates(rows: Vec<CandidateRow>) -> Result<Vec<VerifiedRow>, String> {
        unwrap_batch_parallel(rows, |session, row| {
            let text = Self::decrypt_payload_with_unwrap(&row.text_enc, &row.text_key_enc, &|c| {
                session.unwrap_row_key(c)
            });
            let text = match text {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(BackgroundReadError::AuthRequired) => {
                    return Err(BackgroundReadError::AuthRequired)
                }
                Err(_) => String::new(),
            };
            Ok(VerifiedRow {
                id: row.id,
                screenshot_id: row.screenshot_id,
                screenshot_ts: row.screenshot_ts,
                text,
            })
        })
        .map_err(describe_read_error)
    }

    fn cached_order(&self, key: &str, reset_generation: u64) -> Option<(Vec<i64>, bool)> {
        let mut guard = self
            .search_order_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let valid = guard.as_ref().is_some_and(|cached| {
            cached.key == key
                && cached.reset_generation == reset_generation
                && self
                    .semantic_cache_reset_generation
                    .load(AtomicOrdering::Acquire)
                    == reset_generation
                && cached.created.elapsed() <= SEARCH_ORDER_TTL
                && sqlite_data_version(&cached.version_connection)
                    .is_ok_and(|current| current == cached.data_version)
        });
        if !valid {
            // Besides avoiding repeated failed checks, dropping the entry here
            // closes a watcher that may still point at a replaced database.
            *guard = None;
            return None;
        }
        let cached = guard.as_ref().expect("cache validity was checked above");
        Some((cached.ordered.clone(), cached.complete))
    }

    fn store_order(
        &self,
        key: String,
        ordered: &[i64],
        complete: bool,
        data_version: i64,
        reset_generation: u64,
        version_connection: Connection,
    ) {
        let mut guard = self
            .search_order_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Both checks happen while holding the publication lock. Otherwise a
        // database reset could clear the cache between an earlier check and
        // this assignment, allowing an old-file result to reappear afterward.
        if self
            .semantic_cache_reset_generation
            .load(AtomicOrdering::Acquire)
            != reset_generation
            || sqlite_data_version(&version_connection)
                .map_or(true, |current| current != data_version)
        {
            return;
        }
        *guard = Some(CachedSearchOrder {
            key,
            created: Instant::now(),
            data_version,
            version_connection,
            reset_generation,
            ordered: ordered.to_vec(),
            complete,
        });
    }

    /// Drops the cached result order. Called when the backing database is
    /// swapped, where every id in it stops meaning anything.
    pub(super) fn clear_search_order_cache(&self) {
        *self
            .search_order_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Search text using the blind bigram bitmap index.
    #[allow(clippy::too_many_arguments)]
    pub fn search_text(
        &self,
        query: &str,
        limit: i32,
        offset: i32,
        fuzzy: bool,
        process_names: Option<Vec<String>>,
        start_time: Option<f64>,
        end_time: Option<f64>,
        categories: Option<Vec<String>>,
    ) -> Result<Vec<SearchResult>, String> {
        let mut telemetry = SearchTelemetry::new();
        let mut watch = Stopwatch::start();

        let limit = limit.max(0);
        let offset = offset.max(0);
        let requested = (offset as usize).saturating_add(limit as usize);

        let hmac_key = self.credential_state.get_hmac_key()?;
        let conn = self.open_read_connection_named("search_text")?;
        telemetry.stages.setup += watch.lap();

        let plan = QueryPlan::build(query);
        telemetry.counts.keywords = plan.keywords.len();
        telemetry.counts.bigrams = plan.phrase_groups.len();
        telemetry.stages.planning += watch.lap();

        // Nothing to search for: the landing view and any query too short to
        // have produced an index entry fall back to the most recent captures.
        if plan.phrase.is_empty() {
            telemetry.tier = "recent";
            let (sql, params) = build_empty_search_sql(
                process_names.as_deref(),
                start_time,
                end_time,
                categories.as_deref(),
                limit,
                offset,
            );
            let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
            let results =
                self.materialize_search_rows(&conn, &sql, param_refs.as_slice(), &HashMap::new())?;
            telemetry.stages.materializing += watch.lap();
            telemetry.counts.rows_returned = results.len();
            return Ok(results);
        }

        let cache_key = order_cache_key(
            query,
            fuzzy,
            process_names.as_deref(),
            categories.as_deref(),
            start_time,
            end_time,
        );
        let reset_generation = self
            .semantic_cache_reset_generation
            .load(AtomicOrdering::Acquire);
        if let Some((ordered, complete)) = self.cached_order(&cache_key, reset_generation) {
            telemetry.tier = "cached";
            telemetry.cached = true;
            telemetry.complete = complete;
            telemetry.counts.candidates = ordered.len() as u64;
            let page = page_of(&ordered, offset as usize, limit as usize);
            let results = self.materialize_ordered(&conn, &page, &HashMap::new())?;
            telemetry.stages.materializing += watch.lap();
            telemetry.counts.rows_returned = results.len();
            return Ok(results);
        }
        // This value is sampled on the very connection that will be retained
        // as the cache watcher. If any writer commits while recall/ranking is
        // running, `store_order` observes the change and declines to cache the
        // mixed snapshot.
        let data_version = sqlite_data_version(&conn)?;

        let allowed = allowed_ocr_rows(
            &conn,
            process_names.as_deref(),
            categories.as_deref(),
            start_time,
            end_time,
            &mut telemetry.counts,
        )?;
        telemetry.stages.filters += watch.lap();

        // The first uncached request establishes the order used by every later
        // page. Rank its whole bounded candidate set below, and reuse that
        // order even when a later request asks past its end; recomputing with a
        // larger budget is what used to duplicate or skip rows at boundaries.
        let budget = requested
            .saturating_mul(VERIFY_PER_REQUESTED_ROW)
            .clamp(VERIFY_FLOOR, VERIFY_CAP);

        let mut candidates = if plan.has_index_terms() {
            let (candidates, reached) = self.recall(
                &conn,
                &plan,
                &hmac_key,
                fuzzy,
                allowed.as_ref(),
                budget,
                &mut telemetry,
                &mut watch,
            )?;
            telemetry.tier = reached.label();
            candidates
        } else {
            telemetry.tier = Tier::Scan.label();
            let ids =
                recent_candidate_ids(&conn, allowed.as_ref(), SCAN_WINDOW, &mut telemetry.counts)?;
            ids.into_iter()
                .map(|ocr_id| Candidate {
                    ocr_id,
                    tier: Tier::Scan,
                    hits: 0,
                    total: 0,
                })
                .collect()
        };
        telemetry.counts.candidates = candidates.len() as u64;

        // The order candidates are checked in, decided by the cheapest
        // evidence available: the tier that proposed the row, then how much of
        // the query it matched, then how recent it is.
        candidates.sort_by(|left, right| {
            left.tier
                .cmp(&right.tier)
                .then_with(|| right.hits.cmp(&left.hits))
                .then_with(|| right.ocr_id.cmp(&left.ocr_id))
        });
        reserve_for_fuzzy(&mut candidates, budget);
        // Exactly the ceiling is conservatively reported as partial: a helper
        // may already have trimmed a wider bitmap down to this many rows.
        let complete = candidates.len() < VERIFY_CAP;
        candidates.truncate(VERIFY_CAP);
        telemetry.complete = complete;
        telemetry.stages.combining += watch.lap();

        self.rank_and_page(
            conn,
            &plan,
            candidates,
            cache_key,
            offset as usize,
            limit as usize,
            data_version,
            reset_generation,
            complete,
            &mut telemetry,
            &mut watch,
        )
    }

    /// Decrypts candidates, ranks them on their text and returns the requested
    /// page.
    ///
    /// This is where a candidate stops being a claim about bigrams and starts
    /// being a result. Everything above it proposes rows; nothing above it
    /// decides their order.
    ///
    /// Ranking keeps a row whose text contains one of the query's words either
    /// literally or within a small edit budget, and it separates the two: the
    /// literal and near matches are ranked together on their text, and behind
    /// them come the rows the typo-tolerant pass proposed whose text says
    /// nothing recognisable at all. That tail is ordered on bigram evidence
    /// alone and only ever reached by a search that could not fill its page
    /// otherwise, which is the one situation where a weak guess beats an empty
    /// result.
    ///
    /// Every row in the recalled candidate set is checked in this call. The
    /// resulting order is then immutable for its cache lifetime, including
    /// when a later page reaches beyond an intentionally partial recall.
    #[allow(clippy::too_many_arguments)]
    fn rank_and_page(
        &self,
        conn: Connection,
        plan: &QueryPlan,
        candidates: Vec<Candidate>,
        cache_key: String,
        offset: usize,
        limit: usize,
        data_version: i64,
        reset_generation: u64,
        complete: bool,
        telemetry: &mut SearchTelemetry,
        watch: &mut Stopwatch,
    ) -> Result<Vec<SearchResult>, String> {
        let evidence: HashMap<i64, Candidate> = candidates
            .iter()
            .map(|candidate| (candidate.ocr_id, *candidate))
            .collect();

        let mut texts: HashMap<i64, String> = HashMap::new();
        let mut scored: Vec<Scored> = Vec::new();
        let mut tolerated: Vec<Scored> = Vec::new();
        let ids: Vec<i64> = candidates
            .iter()
            .map(|candidate| candidate.ocr_id)
            .collect();
        let rows = self.load_candidate_rows(&conn, &ids, &mut telemetry.counts)?;
        let verified = Self::decrypt_candidates(rows)?;
        telemetry.counts.verified += verified.len();

        for row in verified {
            let ranked = search_rank::score_text(plan, &row.text);
            // A row whose text reads nothing like the query is a bitmap false
            // positive: the right bigrams in the wrong order. It is kept only
            // when the typo-tolerant pass proposed it, and then behind every
            // row whose text said something.
            let (into, score) = if ranked.matched {
                if !ranked.literal {
                    telemetry.counts.near += 1;
                }
                (&mut scored, ranked.score)
            } else {
                match evidence.get(&row.id) {
                    Some(candidate) if candidate.tier == Tier::Fuzzy => (
                        &mut tolerated,
                        search_rank::fuzzy_prior(candidate.hits, candidate.total),
                    ),
                    _ => continue,
                }
            };

            texts.insert(row.id, row.text);
            into.push(Scored {
                ocr_id: row.id,
                screenshot_id: row.screenshot_id,
                screenshot_ts: row.screenshot_ts,
                score,
            });
        }
        telemetry.complete = complete;
        telemetry.stages.verifying += watch.lap();

        let by_score = |left: &Scored, right: &Scored| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.screenshot_ts.cmp(&left.screenshot_ts))
                .then_with(|| right.ocr_id.cmp(&left.ocr_id))
        };
        scored.sort_by(by_score);
        tolerated.sort_by(by_score);
        scored.append(&mut tolerated);

        // A multi-word search reports captures, not text blocks — the words
        // may well be spread across several. Keeping the first occurrence of
        // each screenshot after sorting makes the representative the block
        // that ranked best, where it used to be whichever block happened to
        // have the largest row id.
        if plan.has_distinct_keyword_pass() {
            let mut seen: HashSet<i64> = HashSet::new();
            scored.retain(|row| seen.insert(row.screenshot_id));
        }
        telemetry.counts.kept = scored.len();

        let ordered: Vec<i64> = scored.iter().map(|row| row.ocr_id).collect();
        let page = page_of(&ordered, offset, limit);
        texts.retain(|id, _| page.contains(id));
        let results = self.materialize_ordered(&conn, &page, &texts)?;
        telemetry.stages.materializing += watch.lap();
        telemetry.counts.rows_returned = results.len();
        self.store_order(
            cache_key,
            &ordered,
            complete,
            data_version,
            reset_generation,
            conn,
        );
        Ok(results)
    }

    /// Proposes candidate rows, escalating through the tiers only as far as
    /// the requested page needs.
    ///
    /// Each tier is looser and more expensive than the one before it, so a
    /// query that the phrase pass answers never pays for the rest. The
    /// escalation threshold oversamples on purpose: a candidate is only a
    /// claim about bigrams, and the ranker will drop the ones whose text does
    /// not bear it out.
    #[allow(clippy::too_many_arguments)]
    fn recall(
        &self,
        conn: &Connection,
        plan: &QueryPlan,
        hmac_key: &[u8],
        fuzzy: bool,
        allowed: Option<&RoaringBitmap>,
        escalate_until: usize,
        telemetry: &mut SearchTelemetry,
        watch: &mut Stopwatch,
    ) -> Result<(Vec<Candidate>, Tier), String> {
        let mut store = PostingStore::new(conn);
        let phrase_planned = plan_groups(&plan.phrase_groups, hmac_key);
        // The flat list is what makes the rarest bigram of *any* keyword
        // narrow the candidates first, instead of each keyword being resolved
        // on its own and only then combined.
        let flat_planned = plan_groups(plan.flat_keyword_groups(), hmac_key);
        let keyword_planned: Vec<Vec<PlannedGroup>> = plan
            .keyword_groups
            .iter()
            .map(|groups| plan_groups(groups, hmac_key))
            .collect();

        let mut hashes: Vec<String> = phrase_planned
            .iter()
            .chain(keyword_planned.iter().flatten())
            .flat_map(|group| group.hashes.iter().cloned())
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        store.probe(&hashes, &mut telemetry.counts)?;
        telemetry.stages.probing += watch.lap();

        let phrase_groups = store.resolve(&phrase_planned);
        let flat_groups = store.resolve(&flat_planned);
        let keyword_groups: Vec<Vec<HashedGroup>> = keyword_planned
            .iter()
            .map(|planned| store.resolve(planned))
            .collect();

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        let mut reached = Tier::Phrase;

        let phrase_refs: Vec<&HashedGroup> = phrase_groups.iter().collect();
        let phrase_total = phrase_refs.len() as u32;
        if let Some(hits) =
            intersect_groups(&mut store, &phrase_refs, fuzzy, &mut telemetry.counts)?
        {
            push_candidates(
                &mut candidates,
                &mut seen,
                bounded_hits(&hits, phrase_total, allowed).into_iter(),
                Tier::Phrase,
                phrase_total,
                allowed,
            );
        }
        telemetry.stages.bitmaps += watch.lap();

        // Every later tier is about words that are not adjacent, so a
        // single-word query has nothing to escalate to except typo tolerance.
        let multi_word = plan.has_distinct_keyword_pass();
        // A query whose words are all too short to be indexed leaves the flat
        // list empty; the phrase's own bigrams are then the only thing to ask.
        let flat: Vec<&HashedGroup> = if flat_groups.is_empty() {
            phrase_refs.clone()
        } else {
            flat_groups.iter().collect()
        };

        if multi_word && candidates.len() < escalate_until {
            reached = Tier::Block;
            let total = flat.len() as u32;
            if let Some(hits) = intersect_groups(&mut store, &flat, fuzzy, &mut telemetry.counts)? {
                push_candidates(
                    &mut candidates,
                    &mut seen,
                    bounded_hits(&hits, total, allowed).into_iter(),
                    Tier::Block,
                    total,
                    allowed,
                );
            }
            telemetry.stages.bitmaps += watch.lap();
        }

        if multi_word && candidates.len() < escalate_until {
            reached = Tier::Screenshot;
            let mut per_keyword: Vec<RoaringBitmap> = Vec::with_capacity(keyword_groups.len());
            for groups in &keyword_groups {
                let refs: Vec<&HashedGroup> = groups.iter().collect();
                match intersect_groups(&mut store, &refs, fuzzy, &mut telemetry.counts)? {
                    Some(hits) => per_keyword.push(hits),
                    None => {
                        per_keyword.clear();
                        break;
                    }
                }
            }
            telemetry.stages.bitmaps += watch.lap();

            if !per_keyword.is_empty() {
                let mut blocks =
                    screenshot_pass(conn, &per_keyword, allowed, &mut telemetry.counts)?;
                if blocks.len() > VERIFY_CAP {
                    blocks = blocks.split_off(blocks.len() - VERIFY_CAP);
                }
                let total = flat.len() as u32;
                push_candidates(
                    &mut candidates,
                    &mut seen,
                    blocks.into_iter().map(|id| (id as u32, total)),
                    Tier::Screenshot,
                    total,
                    allowed,
                );
            }
            telemetry.stages.resolving += watch.lap();
        }

        // The typo-tolerant pass runs whether or not the strict ones filled the
        // budget. It is the only pass that can propose a row the query
        // misspells, and a strict pass is perfectly capable of returning a
        // budget's worth of blocks that scatter the query's bigrams without
        // containing its words — in which case stopping here would answer a
        // search for `Separtion` with noise while the block reading
        // `Separation` sat one bigram away, unasked for.
        if fuzzy {
            if candidates.len() < escalate_until {
                reached = Tier::Fuzzy;
            }
            let before = candidates.len();
            if let Some((hits, checked)) =
                fuzzy_candidates(&mut store, &flat, allowed, &mut telemetry.counts)?
            {
                push_candidates(
                    &mut candidates,
                    &mut seen,
                    bounded_fuzzy_hits(hits, allowed).into_iter(),
                    Tier::Fuzzy,
                    checked,
                    allowed,
                );
            }
            telemetry.counts.tolerated = candidates.len() - before;
            telemetry.stages.bitmaps += watch.lap();
        }

        Ok((candidates, reached))
    }
}

/// Reorders candidates so the typo-tolerant tier keeps a share of the
/// decryption budget.
///
/// Tiers are ordered by strength of evidence, which is the right way to decide
/// what to look at first and the wrong way to decide what never gets looked at
/// at all. Only the first `budget` candidates are decrypted, so a strict pass
/// that proposes that many on its own decides the answer by itself — and it
/// has no way of knowing that every one of its rows will be dropped for not
/// containing the query's words. Holding one slot in `FUZZY_BUDGET_SHARE` back
/// guarantees the near misses are at least read before that verdict is
/// reached.
///
/// Everything not promoted keeps its relative order, so a search whose strict
/// passes were right pays nothing but the reordering itself.
///
/// Expects `candidates` sorted by tier, which is what leaves the tolerated
/// rows in one run at the end.
fn reserve_for_fuzzy(candidates: &mut Vec<Candidate>, budget: usize) {
    let reserve = budget / FUZZY_BUDGET_SHARE;
    let strict_head = budget.saturating_sub(reserve);
    if reserve == 0 || candidates.len() <= budget {
        return;
    }

    let first_tolerated = candidates
        .iter()
        .position(|candidate| candidate.tier == Tier::Fuzzy);
    // Nothing to promote, or the tolerated rows already start inside the head.
    let Some(first_tolerated) = first_tolerated.filter(|at| *at > strict_head) else {
        return;
    };

    let tail = candidates.split_off(strict_head);
    let (strict_tail, tolerated) = tail.split_at(first_tolerated - strict_head);
    candidates.extend_from_slice(&tolerated[..reserve.min(tolerated.len())]);
    candidates.extend_from_slice(strict_tail);
    candidates.extend_from_slice(&tolerated[reserve.min(tolerated.len())..]);
}

/// Adds recall results that pass the filters and have not been proposed by an
/// earlier, stronger tier.
fn push_candidates(
    into: &mut Vec<Candidate>,
    seen: &mut HashSet<i64>,
    ids: impl Iterator<Item = GroupHit>,
    tier: Tier,
    total: u32,
    allowed: Option<&RoaringBitmap>,
) {
    for (id, hits) in ids {
        if allowed.is_some_and(|filter| !filter.contains(id)) {
            continue;
        }
        let ocr_id = id as i64;
        if !seen.insert(ocr_id) {
            continue;
        }
        into.push(Candidate {
            ocr_id,
            tier,
            hits,
            total,
        });
    }
}

/// Recall results, trimmed to what the ranker could ever look at.
///
/// Past [`VERIFY_CAP`] the surplus can only be cut again by recency, so
/// keeping it means building and sorting a list the size of the table to throw
/// almost all of it away. This is the guard that stops a query whose bigrams
/// happen to be common ones from producing a full-table candidate set — the
/// failure the old fuzzy union produced on every multi-word search.
fn bounded_hits(
    hits: &RoaringBitmap,
    total: u32,
    allowed: Option<&RoaringBitmap>,
) -> Vec<GroupHit> {
    // Apply the filter before choosing the newest `VERIFY_CAP` rows. Cutting
    // the unfiltered bitmap first can discard every allowed row when the
    // filter selects an older slice of the table.
    if let Some(filter) = allowed {
        let filtered = hits & filter;
        if filtered.len() as usize <= VERIFY_CAP {
            return filtered.iter().map(|id| (id, total)).collect();
        }
        return newest_ids(&filtered, VERIFY_CAP)
            .into_iter()
            .map(|id| (id as u32, total))
            .collect();
    }
    if hits.len() as usize <= VERIFY_CAP {
        return hits.iter().map(|id| (id, total)).collect();
    }
    newest_ids(hits, VERIFY_CAP)
        .into_iter()
        .map(|id| (id as u32, total))
        .collect()
}

/// The same for the typo-tolerant pass, which ranks by how much of the query
/// survived before falling back to recency.
fn bounded_fuzzy_hits(mut hits: Vec<GroupHit>, allowed: Option<&RoaringBitmap>) -> Vec<GroupHit> {
    if let Some(filter) = allowed {
        hits.retain(|(id, _)| filter.contains(*id));
    }
    if hits.len() > VERIFY_CAP {
        hits.sort_unstable_by(|left, right| {
            right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0))
        });
        hits.truncate(VERIFY_CAP);
    }
    hits
}

/// The newest text blocks, for a query the index cannot describe.
///
/// With a filter in force the allowed set is already a bitmap, and its highest
/// ids are its newest rows; without one, SQLite walks the primary key
/// backwards. Either way the caller gets a bounded window it can afford to
/// decrypt.
fn recent_candidate_ids(
    conn: &Connection,
    allowed: Option<&RoaringBitmap>,
    window: usize,
    counts: &mut SearchCounts,
) -> Result<Vec<i64>, String> {
    if let Some(allowed) = allowed {
        return Ok(newest_ids(allowed, window));
    }

    let mut stmt = conn
        .prepare(
            "SELECT r.id
               FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id
              WHERE r.is_deleted = 0 AND s.is_deleted = 0
              ORDER BY r.id DESC LIMIT ?",
        )
        .map_err(|e| format!("Failed to prepare recent scan: {}", e))?;
    let ids = stmt
        .query_map(params![window as i64], |row| row.get::<_, i64>(0))
        .map_err(|e| format!("Failed to scan recent rows: {}", e))?
        .filter_map(Result::ok)
        .collect();
    counts.statements += 1;
    Ok(ids)
}

fn page_of(ordered: &[i64], offset: usize, limit: usize) -> Vec<i64> {
    let start = offset.min(ordered.len());
    let end = start.saturating_add(limit).min(ordered.len());
    ordered[start..end].to_vec()
}

fn describe_read_error(error: BackgroundReadError) -> String {
    match error {
        BackgroundReadError::AuthRequired => {
            "Authentication required to read search results".to_string()
        }
        BackgroundReadError::Other(message) => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use std::sync::Arc;

    fn search_fixture() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory search database");
        conn.execute_batch(
            "CREATE TABLE screenshots (
                 id INTEGER PRIMARY KEY,
                 image_path TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 is_deleted INTEGER NOT NULL DEFAULT 0,
                 process_name TEXT,
                 window_title_enc BLOB,
                 content_key_encrypted BLOB,
                 category TEXT
             );
             CREATE TABLE ocr_results (
                 id INTEGER PRIMARY KEY,
                 screenshot_id INTEGER NOT NULL,
                 text_enc BLOB,
                 text_key_encrypted BLOB,
                 confidence REAL NOT NULL DEFAULT 1,
                 box_x1 REAL NOT NULL DEFAULT 0,
                 box_y1 REAL NOT NULL DEFAULT 0,
                 box_x2 REAL NOT NULL DEFAULT 0,
                 box_y2 REAL NOT NULL DEFAULT 0,
                 box_x3 REAL NOT NULL DEFAULT 0,
                 box_y3 REAL NOT NULL DEFAULT 0,
                 box_x4 REAL NOT NULL DEFAULT 0,
                 box_y4 REAL NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL,
                 is_deleted INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE blind_bitmap_index (
                 token_hash TEXT PRIMARY KEY,
                 postings_blob BLOB NOT NULL
             );
             CREATE INDEX idx_screenshots_process_deleted_created_at
                 ON screenshots(process_name, is_deleted, created_at);
             CREATE INDEX idx_ocr_deleted_screenshot
                 ON ocr_results(is_deleted, screenshot_id);",
        )
        .expect("search fixture schema");
        conn
    }

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        (temp, storage)
    }

    fn serialize(ids: &[u32]) -> Vec<u8> {
        let bitmap: RoaringBitmap = ids.iter().copied().collect();
        let mut blob = Vec::new();
        bitmap.serialize_into(&mut blob).expect("serialize bitmap");
        blob
    }

    /// Writes posting lists for bigrams spelled exactly as `text` spells them,
    /// which is what the write path does.
    fn index_text(conn: &Connection, hmac_key: &[u8], ids: &[u32], text: &str) {
        for bigram in StorageState::bigram_tokenize(text) {
            let hash = StorageState::compute_hmac_hash(&bigram, hmac_key);
            let existing: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                    params![&hash],
                    |row| row.get(0),
                )
                .optional()
                .expect("read posting list");
            let mut bitmap = existing
                .map(|blob| RoaringBitmap::deserialize_from(&blob[..]).expect("deserialize"))
                .unwrap_or_default();
            bitmap.extend(ids.iter().copied());
            let mut blob = Vec::new();
            bitmap.serialize_into(&mut blob).expect("serialize");
            conn.execute(
                "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob)
                 VALUES (?1, ?2)",
                params![&hash, &blob],
            )
            .expect("write posting list");
        }
    }

    fn hashed(conn: &Connection, text: &str, hmac_key: &[u8]) -> Vec<HashedGroup> {
        let planned = plan_groups(&super::super::search_plan::bigram_groups(text), hmac_key);
        let mut store = PostingStore::new(conn);
        let hashes: Vec<String> = planned
            .iter()
            .flat_map(|group| group.hashes.iter().cloned())
            .collect();
        store
            .probe(&hashes, &mut SearchCounts::default())
            .expect("probe");
        store.resolve(&planned)
    }

    #[test]
    fn empty_search_applies_process_filter_before_limit() {
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots
                 (id, image_path, created_at, process_name)
             VALUES
                 (1, 'newest.enc', '2026-08-12 12:00:00', 'newest.exe'),
                 (2, 'target.enc', '2026-08-12 11:00:00', 'target.exe');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (101, 1, '2026-08-12 12:00:01'),
                 (102, 1, '2026-08-12 12:00:02'),
                 (103, 1, '2026-08-12 12:00:03'),
                 (104, 1, '2026-08-12 12:00:04'),
                 (201, 2, '2026-08-12 11:00:01'),
                 (202, 2, '2026-08-12 11:00:02');",
        )
        .expect("search fixture rows");

        let processes = vec!["target.exe".to_string()];
        let (sql, params) = build_empty_search_sql(Some(&processes), None, None, None, 2, 0);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
        let query_plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("prepare empty process search plan")
            .query_map(param_refs.as_slice(), |row| row.get(3))
            .expect("explain empty process search")
            .map(Result::unwrap)
            .collect();
        assert!(query_plan
            .iter()
            .any(|detail| detail.contains("idx_screenshots_process_deleted_created_at")));
        assert!(query_plan
            .iter()
            .any(|detail| detail.contains("idx_ocr_deleted_screenshot")));

        let mut stmt = conn.prepare(&sql).expect("empty process search");
        let rows: Vec<(i64, i64, Option<String>)> = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(15)?))
            })
            .expect("execute empty process search")
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            rows,
            vec![
                (202, 2, Some("target.exe".to_string())),
                (201, 2, Some("target.exe".to_string())),
            ]
        );
    }

    #[test]
    fn search_projection_returns_capture_time_as_unix_seconds() {
        // Issue #166: this query used to hand `screenshots.created_at` through
        // exactly as SQLite wrote it — UTC wall clock with no zone marker — and
        // the frontend read that as local time, so every hit was displayed one
        // UTC offset behind the timeline. Selecting seconds leaves nothing to
        // interpret, and `wire_time` renders the string the frontend sees.
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots (id, image_path, created_at, process_name)
             VALUES (1, 'shot.enc', '2026-08-11 06:07:40', 'code.exe');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (11, 1, '2026-08-11 06:09:12');",
        )
        .expect("wire format fixture rows");

        let (sql, params) = build_empty_search_sql(None, None, None, None, 10, 0);
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|param| param.as_ref()).collect();
        let (created_ts, screenshot_created_ts): (Option<i64>, Option<i64>) = conn
            .prepare(&sql)
            .expect("prepare projection query")
            .query_row(param_refs.as_slice(), |row| {
                Ok((row.get(17)?, row.get(18)?))
            })
            .expect("read projection row");

        assert_eq!(screenshot_created_ts, Some(1_786_428_460));
        assert_eq!(
            wire_time::from_optional_seconds(screenshot_created_ts),
            "2026-08-11T06:07:40Z"
        );
        // The OCR row is written when recognition finishes, so it trails the
        // capture — which is why the two are reported separately.
        assert!(created_ts > screenshot_created_ts);
    }

    #[test]
    fn projection_and_decoder_agree_on_column_order() {
        // `decode_search_row` reads by position, which is only safe because
        // every branch selects `SEARCH_RESULT_COLUMNS` instead of spelling out
        // its own list. This test is what makes that agreement checkable: each
        // column gets a distinct value, and the assertions say where it landed.
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots
                 (id, image_path, created_at, process_name, window_title_enc,
                  content_key_encrypted, category)
             VALUES
                 (7, 'shot.enc', '2026-08-11 06:07:40', 'code.exe', X'03', X'AA', 'work');
             INSERT INTO ocr_results
                 (id, screenshot_id, text_enc, text_key_encrypted, confidence,
                  box_x1, box_y1, box_x2, box_y2, box_x3, box_y3, box_x4, box_y4,
                  created_at)
             VALUES
                 (42, 7, X'01', X'02', 0.75, 1, 2, 3, 4, 5, 6, 7, 8,
                  '2026-08-11 06:09:12');",
        )
        .expect("decoder fixture rows");

        let sql = format!(
            "SELECT {SEARCH_RESULT_COLUMNS}
             FROM ocr_results r
             JOIN screenshots s ON r.screenshot_id = s.id"
        );
        let raw = conn
            .prepare(&sql)
            .expect("prepare decoder query")
            .query_row([], decode_search_row)
            .expect("decode projected row");

        assert_eq!(raw.id, 42);
        assert_eq!(raw.screenshot_id, 7);
        assert_eq!(raw.text_enc.as_deref(), Some(&[0x01u8][..]));
        assert_eq!(raw.text_key_enc.as_deref(), Some(&[0x02u8][..]));
        assert_eq!(raw.confidence, 0.75);
        assert_eq!(
            raw.box_coords,
            vec![
                vec![1.0, 2.0],
                vec![3.0, 4.0],
                vec![5.0, 6.0],
                vec![7.0, 8.0],
            ]
        );
        assert_eq!(raw.image_path, "shot.enc");
        assert_eq!(raw.window_title_enc.as_deref(), Some(&[0x03u8][..]));
        assert_eq!(raw.process_name.as_deref(), Some("code.exe"));
        assert_eq!(raw.screenshot_key_enc.as_deref(), Some(&[0xAAu8][..]));
        assert_eq!(raw.created_ts, Some(1_786_428_552));
        assert_eq!(raw.screenshot_created_ts, Some(1_786_428_460));
        assert_eq!(raw.category.as_deref(), Some("work"));
    }

    #[test]
    fn a_lowercase_query_finds_capitalised_text() {
        // The defect the case expansion exists for. The index holds `Se`
        // because that is how the text was written; the query asks for `se`.
        let conn = search_fixture();
        let hmac_key = b"unit-test-hmac-key-for-search-ok!";
        index_text(&conn, hmac_key, &[11], "Six Degrees of Separation");

        let groups = hashed(&conn, "separation", hmac_key);
        assert!(
            groups.iter().all(HashedGroup::is_present),
            "every bigram of the lowercase query should resolve to a posting list"
        );

        let mut store = PostingStore::new(&conn);
        let refs: Vec<&HashedGroup> = groups.iter().collect();
        let hits = intersect_groups(&mut store, &refs, false, &mut SearchCounts::default())
            .expect("intersect")
            .expect("query is answerable from the index");
        assert_eq!(hits.iter().collect::<Vec<_>>(), vec![11]);
    }

    #[test]
    fn the_phrase_pass_uses_cross_word_bigrams() {
        // Both rows contain every word of the query. Only the first contains
        // the phrase, and only the phrase pass can tell them apart, because
        // `xD` and `fS` exist in the index but in no single keyword.
        let conn = search_fixture();
        let hmac_key = b"unit-test-hmac-key-for-search-ok!";
        index_text(&conn, hmac_key, &[1], "Six Degrees of Separation");
        index_text(
            &conn,
            hmac_key,
            &[2],
            "Degrees of freedom, six of them, before separation",
        );

        let mut store = PostingStore::new(&conn);
        let phrase = hashed(&conn, "Six Degrees of Separation", hmac_key);
        let refs: Vec<&HashedGroup> = phrase.iter().collect();
        let hits = intersect_groups(&mut store, &refs, false, &mut SearchCounts::default())
            .expect("intersect")
            .expect("query is answerable");
        assert_eq!(hits.iter().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn intersection_reads_the_rarest_posting_lists_first() {
        // `on` covers everything and says nothing; `xq` covers one row. The
        // intersection has to be driven by the second, or a common bigram
        // decides how much work a search does.
        let conn = search_fixture();
        let common: Vec<u32> = (1..=5_000).collect();
        conn.execute(
            "INSERT INTO blind_bitmap_index (token_hash, postings_blob) VALUES ('common', ?1)",
            params![serialize(&common)],
        )
        .expect("write common posting list");
        conn.execute(
            "INSERT INTO blind_bitmap_index (token_hash, postings_blob) VALUES ('rare', ?1)",
            params![serialize(&[4_242])],
        )
        .expect("write rare posting list");

        let mut store = PostingStore::new(&conn);
        let mut counts = SearchCounts::default();
        store
            .probe(&["common".to_string(), "rare".to_string()], &mut counts)
            .expect("probe");
        let groups = store.resolve(&[
            PlannedGroup {
                bigram: "on".to_string(),
                hashes: vec!["common".to_string()],
            },
            PlannedGroup {
                bigram: "xq".to_string(),
                hashes: vec!["rare".to_string()],
            },
        ]);
        assert!(groups[0].bytes() > groups[1].bytes());

        let refs: Vec<&HashedGroup> = groups.iter().collect();
        let hits = intersect_groups(&mut store, &refs, false, &mut counts)
            .expect("intersect")
            .expect("answerable");
        assert_eq!(hits.iter().collect::<Vec<_>>(), vec![4_242]);
    }

    #[test]
    fn a_missing_bigram_is_fatal_only_in_strict_mode() {
        let conn = search_fixture();
        let mut store = PostingStore::new(&conn);
        let groups = store.resolve(&[PlannedGroup {
            bigram: "zz".to_string(),
            hashes: vec!["absent".to_string()],
        }]);
        let refs: Vec<&HashedGroup> = groups.iter().collect();

        assert!(
            intersect_groups(&mut store, &refs, false, &mut SearchCounts::default())
                .expect("intersect")
                .is_none()
        );
        assert!(
            intersect_groups(&mut store, &refs, true, &mut SearchCounts::default())
                .expect("intersect")
                .is_none(),
            "tolerating the miss still leaves nothing to intersect"
        );
    }

    #[test]
    fn the_fuzzy_pass_unions_only_the_rarest_groups() {
        // Six groups, tolerance one, so the superset is the two rarest. The
        // row that is missing one group survives; a row sharing only the
        // common groups does not.
        let conn = search_fixture();
        let common: Vec<u32> = (1..=200).collect();
        let mut planned = Vec::new();
        for (index, ids) in [
            &common[..],
            &common[..],
            &common[..],
            &common[..],
            &[7, 9][..],
            &[7][..],
        ]
        .iter()
        .enumerate()
        {
            let hash = format!("token{index}");
            conn.execute(
                "INSERT INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)",
                params![&hash, serialize(ids)],
            )
            .expect("write posting list");
            planned.push(PlannedGroup {
                bigram: format!("b{index}"),
                hashes: vec![hash],
            });
        }

        let mut store = PostingStore::new(&conn);
        let mut counts = SearchCounts::default();
        let hashes: Vec<String> = planned
            .iter()
            .flat_map(|group| group.hashes.iter().cloned())
            .collect();
        store.probe(&hashes, &mut counts).expect("probe");
        let groups = store.resolve(&planned);
        let refs: Vec<&HashedGroup> = groups.iter().collect();

        let (mut kept, checked) = fuzzy_candidates(&mut store, &refs, None, &mut counts)
            .expect("fuzzy pass")
            .expect("tolerance is meaningful for six groups");
        kept.sort_unstable();
        assert_eq!(checked, 6);
        // Row 7 is in all six, row 9 misses one — both within tolerance.
        assert_eq!(kept, vec![(7, 6), (9, 5)]);
    }

    #[test]
    fn the_fuzzy_pass_declines_queries_too_short_to_tolerate_a_typo() {
        let conn = search_fixture();
        let mut store = PostingStore::new(&conn);
        let groups = store.resolve(&[PlannedGroup {
            bigram: "ab".to_string(),
            hashes: vec!["absent".to_string()],
        }]);
        let refs: Vec<&HashedGroup> = groups.iter().collect();
        assert!(
            fuzzy_candidates(&mut store, &refs, None, &mut SearchCounts::default())
                .expect("fuzzy pass")
                .is_none()
        );
    }

    #[test]
    fn the_fuzzy_seed_never_loads_a_posting_list_over_its_byte_budget() {
        let conn = search_fixture();
        let mut store = PostingStore::new(&conn);
        let groups: Vec<HashedGroup> = (0..8)
            .map(|index| HashedGroup {
                bigram: format!("b{index}"),
                present: vec![(
                    format!("oversized-{index}"),
                    FUZZY_LOAD_BYTES as usize + index + 1,
                )],
            })
            .collect();
        let refs: Vec<&HashedGroup> = groups.iter().collect();
        let mut counts = SearchCounts::default();

        assert!(fuzzy_candidates(&mut store, &refs, None, &mut counts)
            .expect("fuzzy pass")
            .is_none());
        assert_eq!(counts.loaded, 0);
        assert_eq!(counts.loaded_bytes, 0);
        assert!(store.cache.is_empty());
    }

    #[test]
    fn the_fuzzy_pass_shares_one_load_budget_between_seed_and_counting() {
        let conn = search_fixture();
        let mut store = PostingStore::new(&conn);
        let group_bytes = FUZZY_LOAD_BYTES as usize / 2 + 1;
        let groups: Vec<HashedGroup> = (0..8)
            .map(|index| HashedGroup {
                bigram: format!("b{index}"),
                present: vec![(format!("bounded-{index}"), group_bytes)],
            })
            .collect();
        let refs: Vec<&HashedGroup> = groups.iter().collect();

        let _ = fuzzy_candidates(&mut store, &refs, None, &mut SearchCounts::default())
            .expect("fuzzy pass");
        // Loading a second list would cross the 2 MiB ceiling. It must not be
        // admitted later merely because the counting phase reset its counter.
        assert_eq!(store.cache.len(), 1);
    }

    #[test]
    fn a_misspelled_query_still_recalls_the_correctly_spelled_block() {
        // `Separtion` drops a character from `Separation`, which destroys the
        // bigram `rt`… except the index holds `rt` from somewhere else, so it
        // is a group the strict intersection insists on and the target block
        // does not have. Making it the *rarest* group is what puts it beyond
        // the intersection's early stop, so this is the case where a strict
        // pass genuinely cannot reach the block the user meant.
        let conn = search_fixture();
        let hmac_key = b"unit-test-hmac-key-for-search-ok!";
        let phrase = "Six Degrees of Separation";
        index_text(&conn, hmac_key, &[11], phrase);
        let crowd: Vec<u32> = (1..=50).collect();
        index_text(&conn, hmac_key, &crowd, phrase);
        index_text(&conn, hmac_key, &[99], "xrtz");

        let groups = hashed(&conn, "Separtion", hmac_key);
        assert!(
            groups.iter().all(HashedGroup::is_present),
            "the misspelling's bigrams all exist in the index, `rt` included"
        );

        let refs: Vec<&HashedGroup> = groups.iter().collect();
        let mut store = PostingStore::new(&conn);
        let strict = intersect_groups(&mut store, &refs, true, &mut SearchCounts::default())
            .expect("intersect")
            .expect("every group resolves");
        assert!(
            strict.is_empty(),
            "the strict pass cannot reach a block missing one of the query's bigrams"
        );

        let (kept, checked) =
            fuzzy_candidates(&mut store, &refs, None, &mut SearchCounts::default())
                .expect("fuzzy pass")
                .expect("nine characters tolerate a typo");
        assert_eq!(checked, 8);
        let recalled: HashSet<u32> = kept.iter().map(|(id, _)| *id).collect();
        assert!(
            recalled.contains(&11),
            "the block spelling the word correctly should be proposed"
        );
        // The row that only supplied the destroyed bigram matched one group of
        // eight and stays out.
        assert!(!recalled.contains(&99));
    }

    #[test]
    fn the_tolerant_tier_keeps_a_share_of_the_decryption_budget() {
        // A strict pass that proposes more rows than the ranker can decrypt
        // would otherwise decide the answer alone — including when every one
        // of its rows is about to be dropped for not containing the query.
        let budget = 100;
        let mut candidates: Vec<Candidate> = (0..200)
            .map(|ocr_id| Candidate {
                ocr_id,
                tier: Tier::Phrase,
                hits: 4,
                total: 4,
            })
            .chain((200..250).map(|ocr_id| Candidate {
                ocr_id,
                tier: Tier::Fuzzy,
                hits: 3,
                total: 4,
            }))
            .collect();
        reserve_for_fuzzy(&mut candidates, budget);

        assert_eq!(candidates.len(), 250);
        let unique: HashSet<i64> = candidates
            .iter()
            .map(|candidate| candidate.ocr_id)
            .collect();
        assert_eq!(unique.len(), 250, "reordering must not lose or repeat rows");

        let head = &candidates[..budget];
        let tolerated = head
            .iter()
            .filter(|candidate| candidate.tier == Tier::Fuzzy)
            .count();
        assert_eq!(tolerated, budget / FUZZY_BUDGET_SHARE);
        // …and the strict rows keep both their share of the head and their
        // order within it.
        assert!(head[..budget - tolerated]
            .iter()
            .all(|candidate| candidate.tier == Tier::Phrase));
        assert!(head[..budget - tolerated]
            .windows(2)
            .all(|pair| pair[0].ocr_id < pair[1].ocr_id));
    }

    #[test]
    fn a_search_the_strict_passes_answer_is_left_alone() {
        let mut candidates: Vec<Candidate> = (0..200)
            .map(|ocr_id| Candidate {
                ocr_id,
                tier: Tier::Phrase,
                hits: 4,
                total: 4,
            })
            .collect();
        let untouched = candidates.clone();
        reserve_for_fuzzy(&mut candidates, 100);
        assert_eq!(candidates, untouched);

        // Nor is anything moved when every candidate is decrypted anyway.
        let mut short: Vec<Candidate> = untouched[..40].to_vec();
        let before = short.clone();
        reserve_for_fuzzy(&mut short, 100);
        assert_eq!(short, before);
    }

    #[test]
    fn filters_resolve_to_the_rows_they_allow() {
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots
                 (id, image_path, created_at, process_name, category)
             VALUES
                 (1, 'other.enc', '2026-08-12 12:00:00', 'other.exe', 'play'),
                 (2, 'target.enc', '2026-08-12 11:00:00', 'target.exe', 'work');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (6, 1, '2026-08-12 12:00:06'),
                 (5, 1, '2026-08-12 12:00:05'),
                 (2, 2, '2026-08-12 11:00:02'),
                 (1, 2, '2026-08-12 11:00:01');",
        )
        .expect("filter fixture rows");

        let mut counts = SearchCounts::default();
        assert!(allowed_ocr_rows(&conn, None, None, None, None, &mut counts)
            .expect("no filter")
            .is_none());

        let processes = vec!["target.exe".to_string()];
        let allowed = allowed_ocr_rows(&conn, Some(&processes), None, None, None, &mut counts)
            .expect("process filter")
            .expect("a filter was set");
        assert_eq!(allowed.iter().collect::<Vec<_>>(), vec![1, 2]);
        // One statement, whatever the candidate count — the old path issued
        // one per five hundred candidates.
        assert_eq!(counts.statements, 1);

        // The time bound is resolved here too, so it constrains candidates
        // before pagination instead of trimming an already-decrypted page.
        let recent = allowed_ocr_rows(
            &conn,
            None,
            None,
            Some(1_786_536_000.0), // 2026-08-12 12:00:00Z
            None,
            &mut counts,
        )
        .expect("time filter")
        .expect("a filter was set");
        assert_eq!(recent.iter().collect::<Vec<_>>(), vec![5, 6]);
    }

    #[test]
    fn the_screenshot_pass_resolves_only_the_rarest_keyword() {
        // Screenshot 1 holds one keyword in each of two blocks; screenshot 2
        // holds only one of them.
        let conn = search_fixture();
        conn.execute_batch(
            "INSERT INTO screenshots (id, image_path, created_at) VALUES
                 (1, 'a.enc', '2026-08-12 12:00:00'),
                 (2, 'b.enc', '2026-08-12 11:00:00');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES
                 (10, 1, '2026-08-12 12:00:01'),
                 (11, 1, '2026-08-12 12:00:02'),
                 (20, 2, '2026-08-12 11:00:01'),
                 (21, 2, '2026-08-12 11:00:02');",
        )
        .expect("screenshot pass fixture");

        let first: RoaringBitmap = [10u32, 20].into_iter().collect();
        let second: RoaringBitmap = [11u32].into_iter().collect();
        let mut counts = SearchCounts::default();
        let blocks =
            screenshot_pass(&conn, &[first, second], None, &mut counts).expect("screenshot pass");
        assert_eq!(blocks, vec![10, 11]);
        // The rarest keyword is resolved to its screenshots, then those
        // screenshots' blocks are fetched: two statements, not one per five
        // hundred candidates per keyword.
        assert_eq!(counts.statements, 2);
    }

    #[test]
    fn the_screenshot_seed_is_filtered_before_it_is_capped() {
        let conn = search_fixture();
        let allowed_rows = 10u32;
        let newest_rows = SCREENSHOT_PASS_CAP as u32 + 5;
        let mut sql = String::from(
            "INSERT INTO screenshots (id, image_path, created_at) VALUES
                 (1, 'allowed.enc', '2026-08-12 10:00:00'),
                 (2, 'noise.enc', '2026-08-12 12:00:00');
             INSERT INTO ocr_results (id, screenshot_id, created_at) VALUES ",
        );
        let mut rows = Vec::new();
        for id in 1..=allowed_rows {
            rows.push(format!("({id}, 1, '2026-08-12 10:00:00')"));
        }
        for id in (allowed_rows + 1)..=(allowed_rows + newest_rows) {
            rows.push(format!("({id}, 2, '2026-08-12 12:00:00')"));
        }
        sql.push_str(&rows.join(","));
        sql.push(';');
        conn.execute_batch(&sql).expect("screenshot cap fixture");

        let seed: RoaringBitmap = (1..=(allowed_rows + newest_rows)).collect();
        // Keep the second keyword slightly broader so the first bitmap is the
        // seed. If the seed were capped before this filter, only the newer
        // noise screenshot would survive the cap.
        let other: RoaringBitmap = (1..=(allowed_rows + newest_rows + 1)).collect();
        let allowed: RoaringBitmap = (1..=allowed_rows).collect();
        let mut counts = SearchCounts::default();
        let blocks = screenshot_pass(&conn, &[seed, other], Some(&allowed), &mut counts)
            .expect("filtered screenshot pass");

        assert_eq!(
            blocks,
            (1..=allowed_rows).map(i64::from).collect::<Vec<_>>()
        );
    }

    #[test]
    fn newest_ids_keeps_the_most_recent_candidates() {
        let bitmap: RoaringBitmap = (1u32..=10).collect();
        assert_eq!(newest_ids(&bitmap, 3), vec![10, 9, 8]);
        assert_eq!(newest_ids(&bitmap, 0), Vec::<i64>::new());
        assert_eq!(newest_ids(&bitmap, 20).len(), 10);
    }

    #[test]
    fn recall_never_proposes_more_rows_than_the_ranker_can_check() {
        // A bigram common enough to cover the table cannot be allowed to turn
        // into a candidate list the size of the table.
        let wide: RoaringBitmap = (1u32..=(VERIFY_CAP as u32 * 3)).collect();
        let bounded = bounded_hits(&wide, 4, None);
        assert_eq!(bounded.len(), VERIFY_CAP);
        // …and what survives is the newest, which is the order the cut would
        // have applied anyway.
        assert_eq!(bounded[0].0, VERIFY_CAP as u32 * 3);
        assert_eq!(bounded[0].1, 4);

        let narrow: RoaringBitmap = [3u32, 1, 2].into_iter().collect();
        assert_eq!(bounded_hits(&narrow, 9, None).len(), 3);

        // The fuzzy pass keeps whichever candidates matched most of the query.
        let noisy: Vec<(u32, u32)> = (1..=(VERIFY_CAP as u32 * 2))
            .map(|id| (id, if id == 5 { 9 } else { 1 }))
            .collect();
        let kept = bounded_fuzzy_hits(noisy, None);
        assert_eq!(kept.len(), VERIFY_CAP);
        assert_eq!(kept[0], (5, 9));
    }

    #[test]
    fn filters_are_applied_before_recall_is_capped() {
        let wide: RoaringBitmap = (1u32..=(VERIFY_CAP as u32 * 3)).collect();
        let allowed: RoaringBitmap = (1u32..=10).collect();
        let strict = bounded_hits(&wide, 4, Some(&allowed));
        assert_eq!(
            strict.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            (1u32..=10).collect::<Vec<_>>()
        );

        let fuzzy: Vec<GroupHit> = wide.iter().map(|id| (id, id % 7)).collect();
        let tolerant = bounded_fuzzy_hits(fuzzy, Some(&allowed));
        assert_eq!(tolerant.len(), allowed.len() as usize);
        assert!(tolerant.iter().all(|(id, _)| allowed.contains(*id)));
    }

    #[test]
    fn sqlite_data_version_changes_after_another_connection_commits() {
        let temp = tempfile::tempdir().expect("temp database directory");
        let path = temp.path().join("data-version.db");
        let reader = Connection::open(&path).expect("open reader");
        reader
            .execute_batch("CREATE TABLE rows (id INTEGER PRIMARY KEY);")
            .expect("create fixture table");
        let writer = Connection::open(&path).expect("open writer");
        let before = sqlite_data_version(&reader).expect("initial data version");

        writer
            .execute("INSERT INTO rows DEFAULT VALUES", [])
            .expect("commit external write");

        assert_ne!(
            sqlite_data_version(&reader).expect("updated data version"),
            before
        );
    }

    #[test]
    fn cached_partial_orders_are_reused_until_the_database_changes() {
        let temp = tempfile::tempdir().expect("temp database directory");
        let path = temp.path().join("search-cache.db");
        let watcher = Connection::open(&path).expect("open cache watcher");
        watcher
            .execute_batch("CREATE TABLE rows (id INTEGER PRIMARY KEY);")
            .expect("create fixture table");
        let version = sqlite_data_version(&watcher).expect("initial data version");
        let writer = Connection::open(&path).expect("open writer");
        let (_state_dir, storage) = test_storage();

        storage.store_order(
            "query".to_string(),
            &[9, 8, 7, 6],
            false,
            version,
            0,
            watcher,
        );

        let (ordered, complete) = storage
            .cached_order("query", 0)
            .expect("partial order remains pageable");
        assert_eq!(ordered, vec![9, 8, 7, 6]);
        assert!(!complete);
        assert_eq!(page_of(&ordered, 2, 2), vec![7, 6]);

        writer
            .execute("INSERT INTO rows DEFAULT VALUES", [])
            .expect("commit external write");
        assert!(storage.cached_order("query", 0).is_none());
    }

    #[test]
    fn pagination_slices_without_running_off_the_end() {
        let ordered = vec![9, 8, 7];
        assert_eq!(page_of(&ordered, 0, 2), vec![9, 8]);
        assert_eq!(page_of(&ordered, 2, 2), vec![7]);
        assert_eq!(page_of(&ordered, 5, 2), Vec::<i64>::new());
        assert_eq!(page_of(&ordered, 0, 0), Vec::<i64>::new());
    }

    #[test]
    fn the_cache_key_separates_queries_from_their_filters() {
        let query = order_cache_key("code.exe", true, None, None, None, None);
        let filter = order_cache_key("", true, Some(&["code.exe".to_string()]), None, None, None);
        assert_ne!(query, filter);

        // Filter order is not part of the query's identity.
        let one = order_cache_key(
            "x",
            true,
            Some(&["a".to_string(), "b".to_string()]),
            None,
            None,
            None,
        );
        let other = order_cache_key(
            "x",
            true,
            Some(&["b".to_string(), "a".to_string()]),
            None,
            None,
            None,
        );
        assert_eq!(one, other);

        assert_ne!(
            order_cache_key("x", true, None, None, None, None),
            order_cache_key("x", false, None, None, None, None)
        );
    }
}
