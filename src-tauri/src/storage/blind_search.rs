//! Shared bounded HMAC bigram recall for screenshot blocks and recap activities.
use super::blind_index::BlindIndex;
use super::search_plan::{BigramGroup, QueryPlan};
use super::{search_rank, StorageState};
use roaring::RoaringBitmap;
#[cfg(test)]
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A recalled row id together with how many of the query's bigram groups it
/// matched. Every pass but the typo-tolerant one reports "all of them".
pub(super) type GroupHit = (u32, u32);

/// What the typo-tolerant pass found: the surviving rows, and how many groups
/// each of them was checked against.
type FuzzyRecall = (Vec<GroupHit>, u32);

/// A search slow enough to report at `warn`, so the default log level catches
/// it without anyone having to reproduce the query with `RUST_LOG` turned up.
pub(super) const SLOW_SEARCH_REPORT: Duration = Duration::from_secs(5);

/// Values per `IN (...)` statement. Comfortably under SQLite's parameter
/// ceiling and large enough that the bounded passes below issue one statement.
pub(super) const SQL_CHUNK: usize = 500;

/// Posting lists larger than this are left on disk once the intersection has
/// something to work with.
///
/// Measured on a 2.3 GiB library: the nine bigrams of `Separation` came to
/// 1.4 MiB in total and cost 27 ms to read and deserialize, so the ceiling
/// sits far above an ordinary bigram. It only bites on the handful — `on`,
/// `at`, `re` — that cover a large fraction of every text block, and those are
/// exactly the ones an intersection learns nothing from.
pub(super) const MAX_POSTING_BYTES: usize = 4 * 1024 * 1024;

/// The running intersection stops once it is this small.
///
/// Every surviving candidate is checked against decrypted text anyway, so
/// intersecting further trades a certain deserialization cost for an uncertain
/// reduction. This is what keeps the common bigrams of a long word from ever
/// being read.
pub(super) const INTERSECT_EARLY_STOP: u64 = 256;

/// …but never before this many groups have been applied, so a long query is
/// not declared narrow enough on the strength of its first bigram.
pub(super) const MIN_GROUPS_BEFORE_EARLY_STOP: usize = 3;

/// Rows considered by recall per row the caller asked for.
///
/// The first page fixes the result order cached for later pages, but recall
/// still only needs enough evidence to fill that initial request. The floor
/// gives ranking room to discard bitmap false positives without making every
/// small search escalate through all tiers.
pub(super) const VERIFY_PER_REQUESTED_ROW: usize = 4;
pub(super) const VERIFY_FLOOR: usize = 64;

/// Hard ceiling on rows decrypted for one search. A query broad enough to
/// exceed it is ranked on its most recent candidates and reports
/// `complete=false`, rather than turning into thousands of RSA operations.
pub(super) const VERIFY_CAP: usize = 800;

/// Bigram groups a typo is allowed to destroy. One misread character kills the
/// two bigrams it took part in, so two covers a single OCR error.
pub(super) const MAX_FUZZY_TOLERANCE: usize = 2;

/// Candidates the typo-tolerant pass may propose. Beyond this it lowers its
/// tolerance rather than hand the ranker a set it cannot afford to check.
pub(super) const FUZZY_UNION_CAP: u64 = 20_000;

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
pub(super) const FUZZY_LOAD_BYTES: u64 = 2 * 1024 * 1024;

/// The share of the decryption budget held for the typo-tolerant pass.
///
/// One in four, so a strict pass that fills the budget with blocks that merely
/// scatter the query's bigrams cannot starve the near misses out. See
/// `reserve_for_fuzzy`.
pub(super) const FUZZY_BUDGET_SHARE: usize = 4;

/// Blocks the cross-block pass resolves to screenshots. Bounds both the
/// statements it issues and the containment checks that follow.
pub(super) const SCREENSHOT_PASS_CAP: usize = 2_000;

/// Rows a query too short for the index may read directly.
///
/// A single character produces no bigram, so nothing in the index describes
/// it. Rather than answer with recent captures that do not contain it — which
/// is what used to happen — such a query reads the newest blocks and checks
/// them, within the same budget any other search spends on decryption. The
/// window is narrow, and deliberately so: a complete answer needs a unigram
/// index, which is a different piece of work.
pub(super) const SCAN_WINDOW: usize = VERIFY_CAP;

/// How long a computed result order stays reusable.
///
/// Long enough that scrolling through pages of one search never recomputes it,
/// short enough that a capture taken during the scroll shows up on the next
/// search rather than being hidden for the session.
pub(super) const SEARCH_ORDER_TTL: Duration = Duration::from_secs(60);

pub(super) fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Splits a sequential run of work into named stages.
///
/// `lap` returns the time since the previous `lap` — or since construction —
/// and re-arms, so a loop body can charge each pass to the right stage.
pub(super) struct Stopwatch(Instant);

impl Stopwatch {
    pub(super) fn start() -> Self {
        Self(Instant::now())
    }

    pub(super) fn lap(&mut self) -> Duration {
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
pub(super) struct SearchCounts {
    pub(super) keywords: usize,
    pub(super) bigrams: usize,
    /// Token hashes asked about, and how many the index held.
    pub(super) probed: usize,
    pub(super) present: usize,
    /// Posting lists deserialized, and their serialized size.
    pub(super) loaded: usize,
    pub(super) loaded_bytes: u64,
    /// Candidate rows recall proposed, after filtering.
    pub(super) candidates: u64,
    /// …of which the typo-tolerant pass contributed. A query spelled the way
    /// the capture spells it should show a small number here; a large one
    /// means the strict passes found little and the answer rests on tolerance.
    pub(super) tolerated: usize,
    /// `IN (...)` statements issued outside the final page fetch.
    pub(super) statements: usize,
    /// Rows decrypted for ranking, and how many the ranking kept.
    pub(super) verified: usize,
    pub(super) kept: usize,
    /// Kept rows whose text matched only within the edit budget rather than
    /// literally. Worth watching: this is the recall the plaintext rerank
    /// would otherwise have thrown away, and a query spelled correctly should
    /// show few of them.
    pub(super) near: usize,
    pub(super) rows_returned: usize,
}

#[derive(Default)]
pub(super) struct SearchStages {
    /// Fetching the HMAC key and opening the read connection.
    pub(super) setup: Duration,
    /// Resolving the process, category and time filters.
    pub(super) filters: Duration,
    /// Splitting the query and computing its bigram groups.
    pub(super) planning: Duration,
    /// Asking the index which token hashes exist and how large they are.
    pub(super) probing: Duration,
    /// Reading posting lists and deserializing them.
    pub(super) bitmaps: Duration,
    /// Intersections, unions and candidate ordering.
    pub(super) combining: Duration,
    /// Mapping candidate blocks onto screenshots.
    pub(super) resolving: Duration,
    /// Fetching candidates, decrypting them and scoring them.
    pub(super) verifying: Duration,
    /// The SQL part of `verifying`, before any key unwrap or plaintext work.
    pub(super) candidate_sql: Duration,
    /// Fetching the page and decrypting the rest of its columns.
    pub(super) materializing: Duration,
    /// The SQL part of `materializing`.
    pub(super) page_sql: Duration,
}

/// Per-query timing report for [`StorageState::search_text`], emitted on drop.
///
/// The search has many exits, and several are an early `return Ok(vec![])`
/// taken *after* the expensive part. Reporting from `Drop` means the branch
/// that loads a hundred megabytes of posting lists and then finds nothing is
/// measured like any other, which is exactly the case worth seeing.
pub(super) struct SearchTelemetry {
    pub(super) started: Instant,
    /// The deepest tier the search had to escalate to, not necessarily the one
    /// the returned rows came from — a phrase hit that did not fill the page
    /// still reports the tier that was tried next.
    pub(super) tier: &'static str,
    pub(super) cached: bool,
    /// Whether every candidate was ranked. `false` means the answer is the
    /// best of the most recent candidates rather than the best overall.
    pub(super) complete: bool,
    pub(super) counts: SearchCounts,
    pub(super) stages: SearchStages,
}

impl SearchTelemetry {
    pub(super) fn new() -> Self {
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
             resolve={:.1} verify={:.1} page={:.1} candidate_sql={:.1} page_sql={:.1}",
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
            millis(self.stages.candidate_sql),
            millis(self.stages.page_sql),
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
pub(super) enum Tier {
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
    pub(super) fn label(self) -> &'static str {
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
pub(super) struct Candidate {
    pub(super) row_id: i64,
    pub(super) tier: Tier,
    /// Bigram groups matched, out of how many were checked. Only the fuzzy
    /// tier reports anything other than "all of them".
    pub(super) hits: u32,
    pub(super) total: u32,
}

/// A bigram's lookups, before the index has been asked which exist.
pub(super) struct PlannedGroup {
    pub(super) bigram: String,
    pub(super) hashes: Vec<String>,
}

/// The same after probing: only the variants the index holds, each with the
/// serialized size that stands in for its cardinality.
pub(super) struct HashedGroup {
    pub(super) bigram: String,
    pub(super) present: Vec<(String, usize)>,
}

impl HashedGroup {
    /// Canonical full-bitmap bytes, used to order intersections and budget
    /// recall consistently across storage formats. Chunk headers may add a
    /// little physical I/O, which is reported separately in SearchCounts.
    pub(super) fn bytes(&self) -> usize {
        self.present.iter().map(|(_, size)| *size).sum()
    }

    pub(super) fn is_present(&self) -> bool {
        !self.present.is_empty()
    }
}

pub(super) fn plan_groups<'a>(
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
pub(super) struct PostingStore<'a> {
    pub(super) index: BlindIndex<'a>,
    pub(super) layouts: HashMap<String, bool>,
    pub(super) sizes: HashMap<String, usize>,
    pub(super) cache: HashMap<String, Rc<RoaringBitmap>>,
}

impl<'a> PostingStore<'a> {
    #[cfg(test)]
    pub(super) fn new(conn: &'a Connection) -> Result<Self, String> {
        Ok(Self {
            index: BlindIndex::open(conn)?,
            layouts: HashMap::new(),
            sizes: HashMap::new(),
            cache: HashMap::new(),
        })
    }

    fn from_index(index: BlindIndex<'a>) -> Self {
        Self {
            index,
            layouts: HashMap::new(),
            sizes: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Asks the index which of these hashes exist and how large they are, in
    /// one statement per [`SQL_CHUNK`].
    ///
    /// Inline BLOB lengths and chunk directory metrics let the planner order
    /// work without deserializing or transferring any bitmap payload.
    pub(super) fn probe(
        &mut self,
        hashes: &[String],
        counts: &mut SearchCounts,
    ) -> Result<(), String> {
        let batch = self.index.probe(hashes)?;
        for posting in batch.postings {
            self.layouts.insert(posting.hash.clone(), posting.chunked);
            self.sizes.insert(posting.hash, posting.bytes);
        }
        counts.probed += hashes.len();
        counts.statements += batch.statements;
        counts.present = self.sizes.len();
        Ok(())
    }

    pub(super) fn resolve(&self, planned: &[PlannedGroup]) -> Vec<HashedGroup> {
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

    pub(super) fn load(
        &mut self,
        group: &HashedGroup,
        counts: &mut SearchCounts,
    ) -> Result<Rc<RoaringBitmap>, String> {
        if let Some(cached) = self.cache.get(&group.bigram) {
            return Ok(Rc::clone(cached));
        }

        let mut union = RoaringBitmap::new();
        for (hash, _) in &group.present {
            let hint = self.layouts.get(hash).copied().unwrap_or(false);
            let Some(posting) = self.index.read(hash, hint)? else {
                continue;
            };
            counts.loaded += posting.fragments;
            counts.loaded_bytes += posting.bytes as u64;
            union |= posting.bitmap;
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
pub(super) fn intersect_groups(
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
/// `None` when the query's edit budget is exhausted by missing groups or the
/// superset would be too large to check. `max_destroyed` comes from the same
/// plaintext edit budget used by the reranker; a character substitution can
/// consume two adjacent bigram groups.
pub(super) fn fuzzy_candidates(
    store: &mut PostingStore<'_>,
    groups: &[&HashedGroup],
    max_destroyed: usize,
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
    // A missing posting is evidence that the query's spelling is absent from
    // the corpus, not evidence that the candidate matched fewer groups. Keep
    // those misses in the edit budget. One character substitution can destroy
    // two adjacent bigrams.
    let missing = groups.len().saturating_sub(ordered.len());
    if max_destroyed == 0 || missing > max_destroyed || affordable.is_empty() {
        return Ok(None);
    }
    let mut tolerance = max_destroyed
        .saturating_sub(missing)
        .min(MAX_FUZZY_TOLERANCE)
        .min(affordable.len().saturating_sub(1));

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
        if tolerance == 0 {
            return Ok(None);
        }
        tolerance -= 1;
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
/// Seek from the end instead of visiting the entire posting list to retain
/// its tail. Work stays bounded by `cap` even for a very common short query.
pub(super) fn newest_ids(bitmap: &RoaringBitmap, cap: usize) -> Vec<i64> {
    bitmap.iter().rev().take(cap).map(i64::from).collect()
}

type CrossBlockRecall<'a> = dyn FnMut(&[RoaringBitmap], Option<&RoaringBitmap>, &mut SearchCounts) -> Result<Vec<i64>, String>
    + 'a;

/// Proposes candidate rows, escalating through the tiers only as far as
/// the requested page needs.
///
/// Each tier is looser and more expensive than the one before it, so a
/// query that the phrase pass answers never pays for the rest. The
/// escalation threshold oversamples on purpose: a candidate is only a
/// claim about bigrams, and the ranker will drop the ones whose text does
/// not bear it out.
#[allow(clippy::too_many_arguments)]
pub(super) fn recall(
    index: BlindIndex<'_>,
    plan: &QueryPlan,
    hmac_key: &[u8],
    fuzzy: bool,
    allowed: Option<&RoaringBitmap>,
    escalate_until: usize,
    telemetry: &mut SearchTelemetry,
    watch: &mut Stopwatch,
    mut cross_block: Option<&mut CrossBlockRecall<'_>>,
) -> Result<(Vec<Candidate>, Tier), String> {
    let mut store = PostingStore::from_index(index);
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
    if let Some(hits) = intersect_groups(&mut store, &phrase_refs, fuzzy, &mut telemetry.counts)? {
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

    if multi_word && candidates.len() < escalate_until && cross_block.is_some() {
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
                cross_block.as_mut().unwrap()(&per_keyword, allowed, &mut telemetry.counts)?;
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
        if let Some((hits, checked)) = fuzzy_candidates(
            &mut store,
            &flat,
            search_rank::edit_budget(plan.phrase.len())
                .saturating_mul(2)
                .min(MAX_FUZZY_TOLERANCE),
            allowed,
            &mut telemetry.counts,
        )? {
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
pub(super) fn reserve_for_fuzzy(candidates: &mut Vec<Candidate>, budget: usize) {
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
pub(super) fn push_candidates(
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
        let row_id = id as i64;
        if !seen.insert(row_id) {
            continue;
        }
        into.push(Candidate {
            row_id,
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
pub(super) fn bounded_hits(
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
pub(super) fn bounded_fuzzy_hits(
    mut hits: Vec<GroupHit>,
    allowed: Option<&RoaringBitmap>,
) -> Vec<GroupHit> {
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
