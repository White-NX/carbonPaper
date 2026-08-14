//! Resident vector caches for the derived indexes.
//!
//! The exact-scan read path re-materialized every vector out of the encrypted
//! store on each query. Measured on a ~9.5k-row hot layer that costs ~425 ms
//! per query, of which the dot products themselves are under a millisecond —
//! the rest is b-tree traversal, page decryption and per-row allocation. The
//! whole matrix is ~14 MiB, so it is held resident while the user is querying
//! and released once they stop: NL cluster search is an explicit submit-and-read
//! interaction, not a keystroke-driven one, so most of the time nobody is
//! searching and the memory is pure waste.
//!
//! Freshness rests on `derived_index_state.data_epoch`: every trigger that can
//! change the query-visible set advances it (see `schema.rs`). The write path
//! updates resident rows in place and records the new epoch, so a capture that
//! dual-writes a vector every few seconds does not force a reload. Anything the
//! write path cannot observe — notably the screenshot soft-delete trigger, which
//! deletes embedding rows from inside SQLite — surfaces as an epoch mismatch and
//! reloads.
//!
//! Lock order is always the database mutex before the cache lock. Scoring never
//! touches SQLite, and the cache guard is dropped before the visibility
//! re-check takes the database mutex, which is not reentrant.
//!
//! Each index kind gets its own matrix and its own idle clock. `semantic_text`
//! serves the natural-language grouping view and Smart Cluster calibration;
//! `clip_image` serves the main search box. Somebody using one is no reason to
//! hold the other's memory, and the two are not even the same width.
//!
//! A durable index may cover more history than its resident exact-scan matrix
//! can safely occupy. Admission is therefore budgeted per kind; an index that
//! does not fit is still searched exactly, page by page, with only the query's
//! top-K heap retained. The budget changes the cache mode, never the corpus.

use super::derived_index::{
    decode_vector, read_derived_data_epoch, visible_embedding_cache_stats_sql,
    visible_embedding_page_sql, DerivedIndexKind, ScoredSubject,
};
use super::StorageState;
use rusqlite::{params, Connection};
use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Candidates scored past `k`, so a subject deleted in the window between the
/// epoch read and the scan can be dropped without a second retrieval pass.
const VISIBILITY_MARGIN: usize = 16;

/// Idle window after which the resident matrix is released. One NL cluster
/// session is a burst of a few queries; five minutes outlives the burst without
/// holding the memory for the hours until the next one.
pub const SEMANTIC_CACHE_IDLE_TTL: Duration = Duration::from_secs(300);

/// Hard resident-cache budgets. They bound the private heap even when the
/// corresponding durable index has no retention window. The CLIP budget is
/// intentionally large enough for the current ~52k-row corpus, while a larger
/// history uses the paged exact-scan path below instead of silently truncating
/// the searchable set.
pub const SEMANTIC_TEXT_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const CLIP_IMAGE_CACHE_MAX_BYTES: usize = 128 * 1024 * 1024;

/// Conservative allocator overhead used by the admission estimate. Rust does
/// not expose an allocator's per-allocation bookkeeping, so the estimate errs
/// high rather than letting a nominal budget become an RSS limit surprise.
const ESTIMATED_ALLOCATOR_OVERHEAD_BYTES: usize = 16;
const HASHMAP_CONTROL_BYTES: usize = 1;

fn cache_budget(index_kind: DerivedIndexKind) -> usize {
    match index_kind {
        DerivedIndexKind::SemanticText => SEMANTIC_TEXT_CACHE_MAX_BYTES,
        DerivedIndexKind::ClipImage => CLIP_IMAGE_CACHE_MAX_BYTES,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// A bounded top-K accumulator. The previous implementation materialised one
/// `(score, index)` tuple for every row before selecting the survivors, which
/// made query-time scratch memory grow with the corpus in addition to the
/// resident matrix. Keeping only the worst survivor in a min-heap makes the
/// scratch bound `O(k)` and works for both resident and paged scans.
struct HeapCandidate<T> {
    score: f32,
    tie_key: String,
    value: T,
}

impl<T> PartialEq for HeapCandidate<T> {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.tie_key == other.tie_key
    }
}

impl<T> Eq for HeapCandidate<T> {}

impl<T> PartialOrd for HeapCandidate<T> {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for HeapCandidate<T> {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.tie_key.cmp(&self.tie_key))
    }
}

struct TopK<T> {
    want: usize,
    heap: BinaryHeap<Reverse<HeapCandidate<T>>>,
}

impl<T> TopK<T> {
    fn new(want: usize) -> Self {
        Self {
            want,
            heap: BinaryHeap::with_capacity(want),
        }
    }

    fn push_with_key(&mut self, score: f32, tie_key: &str, value: impl FnOnce() -> T) {
        if self.want == 0 {
            return;
        }
        if self.heap.len() < self.want {
            self.heap.push(Reverse(HeapCandidate {
                score,
                tie_key: tie_key.to_string(),
                value: value(),
            }));
            return;
        }
        let replace = self
            .heap
            .peek()
            .map(|worst| {
                score
                    .total_cmp(&worst.0.score)
                    .then_with(|| worst.0.tie_key.as_str().cmp(tie_key))
                    .is_gt()
            })
            .unwrap_or(true);
        if replace {
            let _ = self.heap.pop();
            self.heap.push(Reverse(HeapCandidate {
                score,
                tie_key: tie_key.to_string(),
                value: value(),
            }));
        }
    }

    fn into_sorted(mut self) -> Vec<HeapCandidate<T>> {
        let mut entries: Vec<HeapCandidate<T>> = self
            .heap
            .drain()
            .map(|Reverse(candidate)| candidate)
            .collect();
        entries.sort_unstable_by(|a, b| b.cmp(a));
        entries
    }
}

/// Four independent accumulators: f32 addition is not associative, so a single
/// accumulator forces the compiler to keep the loop scalar.
fn dot_product_wide(query: &[f32], row: &[f32]) -> f32 {
    let mut acc = [0.0f32; 4];
    let mut query_chunks = query.chunks_exact(4);
    let mut row_chunks = row.chunks_exact(4);
    for (q, r) in query_chunks.by_ref().zip(row_chunks.by_ref()) {
        acc[0] += q[0] * r[0];
        acc[1] += q[1] * r[1];
        acc[2] += q[2] * r[2];
        acc[3] += q[3] * r[3];
    }
    let mut total = acc[0] + acc[1] + acc[2] + acc[3];
    for (q, r) in query_chunks
        .remainder()
        .iter()
        .zip(row_chunks.remainder().iter())
    {
        total += q * r;
    }
    total
}

/// Row-major matrix of query-visible vectors plus the epoch it reflects.
pub(super) struct SemanticVectorCache {
    dimensions: usize,
    matrix: Vec<f32>,
    subjects: Vec<String>,
    positions: HashMap<String, usize>,
    epoch: i64,
}

impl SemanticVectorCache {
    fn new(epoch: i64) -> Self {
        Self {
            dimensions: 0,
            matrix: Vec::new(),
            subjects: Vec::new(),
            positions: HashMap::new(),
            epoch,
        }
    }

    fn try_with_capacity(epoch: i64, rows: usize, dimensions: usize) -> Result<Self, String> {
        let matrix_values = rows
            .checked_mul(dimensions)
            .ok_or_else(|| "Resident cache allocation size overflow".to_string())?;
        let mut cache = Self::new(epoch);
        cache
            .matrix
            .try_reserve_exact(matrix_values)
            .map_err(|error| format!("Failed to reserve resident vector matrix: {error}"))?;
        cache
            .subjects
            .try_reserve_exact(rows)
            .map_err(|error| format!("Failed to reserve resident subject keys: {error}"))?;
        cache
            .positions
            .try_reserve(rows)
            .map_err(|error| format!("Failed to reserve resident subject positions: {error}"))?;
        cache.dimensions = dimensions;
        Ok(cache)
    }

    pub(super) fn rows(&self) -> usize {
        self.subjects.len()
    }

    pub(super) fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub(super) fn matrix_bytes(&self) -> usize {
        self.matrix.len().saturating_mul(std::mem::size_of::<f32>())
    }

    fn estimated_hashmap_bucket_count(capacity: usize) -> usize {
        // hashbrown keeps a small control-byte group alongside the buckets and
        // leaves headroom before reporting the public element capacity. The
        // exact table layout is an implementation detail, so this deliberately
        // rounds up by 8/7 and lets the admission estimate remain conservative.
        capacity.saturating_mul(8).saturating_add(6) / 7
    }

    /// Conservative private-heap estimate based on actual container
    /// capacities and owned string buffers. This is the value used for cache
    /// admission and diagnostics; `matrix_bytes` remains available when a test
    /// or benchmark specifically needs the logical float payload.
    pub(super) fn allocated_bytes(&self) -> usize {
        let matrix = self
            .matrix
            .capacity()
            .saturating_mul(std::mem::size_of::<f32>());
        let subject_slots = self
            .subjects
            .capacity()
            .saturating_mul(std::mem::size_of::<String>());
        let subject_buffers = self
            .subjects
            .iter()
            .map(String::capacity)
            .fold(0usize, |total, capacity| total.saturating_add(capacity));
        let position_buckets = Self::estimated_hashmap_bucket_count(self.positions.capacity())
            .saturating_mul(
                std::mem::size_of::<String>()
                    + std::mem::size_of::<usize>()
                    + HASHMAP_CONTROL_BYTES,
            );
        let position_buffers = self
            .positions
            .keys()
            .map(String::capacity)
            .fold(0usize, |total, capacity| total.saturating_add(capacity));
        let string_allocations = self
            .subjects
            .len()
            .saturating_add(self.positions.len())
            .saturating_mul(ESTIMATED_ALLOCATOR_OVERHEAD_BYTES);
        let container_allocations = [
            self.matrix.capacity(),
            self.subjects.capacity(),
            self.positions.capacity(),
        ]
        .into_iter()
        .filter(|capacity| *capacity > 0)
        .count()
        .saturating_mul(ESTIMATED_ALLOCATOR_OVERHEAD_BYTES);
        matrix
            .saturating_add(subject_slots)
            .saturating_add(subject_buffers)
            .saturating_add(position_buckets)
            .saturating_add(position_buffers)
            .saturating_add(string_allocations)
            .saturating_add(container_allocations)
    }

    /// Returns whether appending this row can stay within `budget` without
    /// growing any backing container. The caller uses this before `push`, so a
    /// late row appearing after the admission-count query cannot trigger a
    /// doubling allocation that briefly blows past the hard resident limit.
    fn can_push_within_budget(
        &self,
        subject_key: &str,
        key_capacity: usize,
        dimensions: usize,
        budget: usize,
    ) -> bool {
        if dimensions == 0 || self.allocated_bytes() > budget {
            return false;
        }
        if let Some(index) = self.positions.get(subject_key).copied() {
            let Some(start) = index.checked_mul(self.dimensions) else {
                return false;
            };
            return start
                .checked_add(dimensions)
                .map(|end| end <= self.matrix.capacity())
                .unwrap_or(false);
        }
        if self.dimensions != 0 && dimensions != self.dimensions {
            return false;
        }
        let matrix_fits = self
            .matrix
            .len()
            .checked_add(dimensions)
            .map(|len| len <= self.matrix.capacity())
            .unwrap_or(false);
        if !matrix_fits
            || self.subjects.len() >= self.subjects.capacity()
            || self.positions.len() >= self.positions.capacity()
        {
            return false;
        }
        let key_capacity = key_capacity.max(subject_key.len());
        let extra = key_capacity
            .saturating_mul(2)
            .saturating_add(ESTIMATED_ALLOCATOR_OVERHEAD_BYTES.saturating_mul(2));
        self.allocated_bytes()
            .checked_add(extra)
            .map(|bytes| bytes <= budget)
            .unwrap_or(false)
    }

    /// Appends a loaded row. Returns false when the row does not match the
    /// cache's dimension contract, which the caller reports and skips.
    fn push(&mut self, subject_key: String, vector: Vec<f32>) -> bool {
        if self.subjects.is_empty() {
            self.dimensions = vector.len();
        }
        if vector.len() != self.dimensions || vector.is_empty() {
            return false;
        }
        let index = self.subjects.len();
        self.matrix.extend_from_slice(&vector);
        self.positions.insert(subject_key.clone(), index);
        self.subjects.push(subject_key);
        true
    }

    /// In-place update from the write path. Returns false when the vector does
    /// not fit the resident contract, which invalidates the cache instead.
    fn upsert(&mut self, subject_key: &str, vector: &[f32]) -> bool {
        if self.subjects.is_empty() && self.dimensions == 0 {
            self.dimensions = vector.len();
        }
        if vector.is_empty() || vector.len() != self.dimensions {
            return false;
        }
        match self.positions.get(subject_key).copied() {
            Some(index) => {
                let start = index * self.dimensions;
                self.matrix[start..start + self.dimensions].copy_from_slice(vector);
            }
            None => {
                let index = self.subjects.len();
                self.matrix.extend_from_slice(vector);
                self.positions.insert(subject_key.to_string(), index);
                self.subjects.push(subject_key.to_string());
            }
        }
        true
    }

    /// Removes a subject by moving the final row into the hole, keeping the
    /// matrix contiguous for scanning.
    fn remove(&mut self, subject_key: &str) {
        let Some(index) = self.positions.remove(subject_key) else {
            return;
        };
        let last = self.subjects.len() - 1;
        if index != last {
            let dimensions = self.dimensions;
            let (head, tail) = self.matrix.split_at_mut(last * dimensions);
            head[index * dimensions..(index + 1) * dimensions].copy_from_slice(&tail[..dimensions]);
            let moved = self.subjects[last].clone();
            self.positions.insert(moved, index);
        }
        self.subjects.swap_remove(index);
        self.matrix.truncate(last * self.dimensions);
    }

    /// Scores every resident row and returns the best `want`, highest first.
    fn top_candidates(&self, query: &[f32], want: usize) -> Vec<ScoredSubject> {
        if want == 0 || self.subjects.is_empty() || query.len() != self.dimensions {
            return Vec::new();
        }
        let mut top = TopK::new(want);
        for (index, row) in self.matrix.chunks_exact(self.dimensions).enumerate() {
            top.push_with_key(dot_product_wide(query, row), &self.subjects[index], || {
                index as u32
            });
        }
        top.into_sorted()
            .into_iter()
            .map(|candidate| ScoredSubject {
                subject_key: self.subjects[candidate.value as usize].clone(),
                score: candidate.score,
            })
            .collect()
    }
}

/// Rejects a query whose width does not match the stored vectors.
///
/// `top_candidates` answers "nothing scored" for a foreign dimension, which is
/// the right thing for a scoring routine but the wrong thing for a caller
/// deciding *why* it got nothing: the read path would report an empty index and
/// send someone looking for a missing migration when the real cause is a model
/// whose vectors no longer fit the store. An empty store keeps the old
/// behavior, because there is no contract to violate yet.
fn check_query_dimensions(query: &[f32], cache: &SemanticVectorCache) -> Result<(), String> {
    if cache.rows() == 0 || query.len() == cache.dimensions() {
        return Ok(());
    }
    Err(format!(
        "dimension_mismatch: query has {} dimension(s), the stored index has {}",
        query.len(),
        cache.dimensions()
    ))
}

#[derive(Debug, Clone, Copy)]
struct VisibleCacheStats {
    rows: usize,
    dimensions: Option<usize>,
    subject_key_bytes: usize,
}

fn read_visible_cache_stats(
    conn: &Connection,
    index_kind: DerivedIndexKind,
) -> Result<VisibleCacheStats, String> {
    let (rows, min_dimensions, max_dimensions, subject_key_bytes): (
        i64,
        Option<i64>,
        Option<i64>,
        i64,
    ) = conn
        .query_row(
            &visible_embedding_cache_stats_sql(),
            [index_kind.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| format!("Failed to inspect resident vector cache shape: {error}"))?;
    let rows = usize::try_from(rows).map_err(|_| format!("Invalid resident row count: {rows}"))?;
    let subject_key_bytes = usize::try_from(subject_key_bytes)
        .map_err(|_| format!("Invalid resident subject-key byte count: {subject_key_bytes}"))?;
    let dimensions = match (min_dimensions, max_dimensions) {
        (None, None) if rows == 0 => None,
        (Some(min), Some(max)) if min == max && min > 0 => Some(
            usize::try_from(min)
                .map_err(|_| format!("Invalid resident vector dimensions: {min}"))?,
        ),
        (None, None) => {
            return Err("Resident vector dimensions are missing".to_string());
        }
        (min, max) => {
            return Err(format!(
                "Resident vector dimensions are mixed or invalid: min={min:?} max={max:?}"
            ));
        }
    };
    Ok(VisibleCacheStats {
        rows,
        dimensions,
        subject_key_bytes,
    })
}

/// Conservative admission estimate before any matrix allocation happens.
/// `subject_key_bytes` is counted twice because the current mutable cache owns
/// one key in `subjects` and a second clone in `positions`.
fn estimated_cache_bytes(
    rows: usize,
    dimensions: usize,
    subject_key_bytes: usize,
) -> Option<usize> {
    let matrix = rows
        .checked_mul(dimensions)?
        .checked_mul(std::mem::size_of::<f32>())?;
    let subject_slots = rows.checked_mul(std::mem::size_of::<String>())?;
    let subject_buffers = subject_key_bytes;
    // Reserve a deliberately high two-bucket-per-row allowance for hash table
    // control bytes and `(String, usize)` slots. `HashMap::with_capacity` is
    // free to round its bucket count upward on a particular hashbrown build.
    let position_buckets = rows.checked_mul(2)?.checked_mul(
        std::mem::size_of::<String>() + std::mem::size_of::<usize>() + HASHMAP_CONTROL_BYTES,
    )?;
    let position_buffers = subject_key_bytes;
    let string_allocations = rows
        .checked_mul(2)?
        .checked_mul(ESTIMATED_ALLOCATOR_OVERHEAD_BYTES)?;
    let container_allocations = if rows == 0 {
        0
    } else {
        ESTIMATED_ALLOCATOR_OVERHEAD_BYTES.saturating_mul(3)
    };
    matrix
        .checked_add(subject_slots)?
        .checked_add(subject_buffers)?
        .checked_add(position_buckets)?
        .checked_add(position_buffers)?
        .checked_add(string_allocations)
        .and_then(|bytes| bytes.checked_add(container_allocations))
}

enum CacheQueryMode {
    Resident {
        cache: SemanticVectorCache,
        candidates: Vec<ScoredSubject>,
        pages: u32,
        rows: usize,
        estimated_bytes: usize,
    },
    Streaming {
        candidates: Vec<ScoredSubject>,
        pages: u32,
        rows: usize,
        estimated_bytes: usize,
    },
}

/// Rows read per page of the resident-cache load.
///
/// The load is the one scan in this module unbounded by a retention window:
/// `clip_image` covers the whole history, measured at 51,931 rows / 101 MiB on
/// the 2026-08-06 development corpus. Read as a single statement it holds a
/// SQLite SHARED lock for its full duration, and this database runs in rollback
/// journal mode (`journal_mode = delete`, no WAL), where a writer cannot commit
/// until every SHARED lock clears — so capture stalls for the whole scan no
/// matter which connection the scan runs on. Measured on a synthetic 60k-row
/// corpus, one statement pushed the worst capture commit to 660 ms; 4096-row
/// pages brought it to 59 ms, at the cost of scan time nothing waits on.
///
/// 4096 rather than a smaller page because the cost is not linear: each page
/// re-seeks the index, and 2048 measured *worse* on the real corpus (17.2 s
/// against 2.6 s) once the page stopped covering a b-tree node cleanly.
const SCAN_PAGE_ROWS: i64 = 4096;

/// Walk visible vectors a page at a time. The callback owns only the current
/// decoded row; dropping `rows` at the end of each loop releases SQLite's
/// SHARED lock before the next page and keeps the callback's memory bounded.
fn scan_visible_embedding_pages<F>(
    conn: &Connection,
    index_kind: DerivedIndexKind,
    deadline: Option<Instant>,
    mut on_row: F,
) -> Result<(u32, usize), String>
where
    F: FnMut(String, Vec<f32>) -> Result<(), String>,
{
    let mut statement = conn
        .prepare(&visible_embedding_page_sql())
        .map_err(|error| format!("Failed to prepare resident vector scan: {error}"))?;
    let mut cursor = String::new();
    let mut pages = 0u32;
    let mut total_rows = 0usize;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("query_deadline_exceeded_during_exact_scan".to_string());
        }
        let mut page_rows = 0usize;
        {
            let mut rows = statement
                .query(params![index_kind.as_str(), &cursor, SCAN_PAGE_ROWS])
                .map_err(|error| format!("Failed to scan resident vectors: {error}"))?;
            while let Some(row) = rows
                .next()
                .map_err(|error| format!("Failed to read resident vector row: {error}"))?
            {
                let subject_key: String = row
                    .get(0)
                    .map_err(|error| format!("Failed to read resident subject key: {error}"))?;
                let dimensions: i64 = row
                    .get(1)
                    .map_err(|error| format!("Failed to read resident dimensions: {error}"))?;
                let blob: Vec<u8> = row
                    .get(2)
                    .map_err(|error| format!("Failed to read resident vector blob: {error}"))?;
                let dimensions = usize::try_from(dimensions)
                    .map_err(|_| "Invalid resident vector dimensions".to_string())?;
                let vector = decode_vector(&blob, dimensions)?;
                // Advance the keyset cursor before invoking the callback. If
                // it rejects a row, the next page still makes progress.
                cursor.clear();
                cursor.push_str(&subject_key);
                on_row(subject_key, vector)?;
                page_rows += 1;
                total_rows = total_rows
                    .checked_add(1)
                    .ok_or_else(|| "Resident vector row count overflow".to_string())?;
            }
        }
        pages = pages.saturating_add(1);
        if page_rows < SCAN_PAGE_ROWS as usize {
            break;
        }
    }
    Ok((pages, total_rows))
}

impl StorageState {
    /// The matrix and idle clock belonging to one index kind.
    fn cache_slot(
        &self,
        index_kind: DerivedIndexKind,
    ) -> (
        &std::sync::RwLock<Option<SemanticVectorCache>>,
        &std::sync::atomic::AtomicU64,
    ) {
        match index_kind {
            DerivedIndexKind::SemanticText => {
                (&self.semantic_vector_cache, &self.semantic_cache_used_at)
            }
            DerivedIndexKind::ClipImage => (&self.clip_vector_cache, &self.clip_cache_used_at),
        }
    }

    fn cache_load_lock(&self, index_kind: DerivedIndexKind) -> &std::sync::Mutex<()> {
        match index_kind {
            DerivedIndexKind::SemanticText => &self.semantic_cache_load_lock,
            DerivedIndexKind::ClipImage => &self.clip_cache_load_lock,
        }
    }

    /// Production scans use an independent SQLCipher read connection. Unit
    /// tests use an in-memory database attached only to the process-wide
    /// connection, so they deliberately take that connection instead.
    pub(super) fn with_vector_scan_connection<T>(
        &self,
        caller: &'static str,
        operation: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(test)]
        {
            let guard = self.get_connection_named(caller)?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            operation(conn)
        }
        #[cfg(not(test))]
        {
            let conn = self.open_read_connection_named(caller)?;
            operation(&conn)
        }
    }

    /// Cosine top-K over the resident matrix, loading it on first use.
    pub(super) fn semantic_topk_resident(
        &self,
        index_kind: DerivedIndexKind,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredSubject>, String> {
        self.semantic_topk_resident_with_deadline(index_kind, query, k, None)
    }

    pub(super) fn semantic_topk_resident_with_deadline(
        &self,
        index_kind: DerivedIndexKind,
        query: &[f32],
        k: usize,
        deadline: Option<Instant>,
    ) -> Result<Vec<ScoredSubject>, String> {
        self.semantic_topk_resident_with_budget_and_deadline(
            index_kind,
            query,
            k,
            cache_budget(index_kind),
            deadline,
        )
    }

    /// Same query path with an explicit budget for focused regression tests.
    /// Production callers use [`cache_budget`] through
    /// [`Self::semantic_topk_resident`].
    pub(super) fn semantic_topk_resident_with_budget(
        &self,
        index_kind: DerivedIndexKind,
        query: &[f32],
        k: usize,
        resident_budget: usize,
    ) -> Result<Vec<ScoredSubject>, String> {
        self.semantic_topk_resident_with_budget_and_deadline(
            index_kind,
            query,
            k,
            resident_budget,
            None,
        )
    }

    fn semantic_topk_resident_with_budget_and_deadline(
        &self,
        index_kind: DerivedIndexKind,
        query: &[f32],
        k: usize,
        resident_budget: usize,
        deadline: Option<Instant>,
    ) -> Result<Vec<ScoredSubject>, String> {
        if k == 0 {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        let (cache_lock, used_at) = self.cache_slot(index_kind);
        let reset_generation = self.semantic_cache_reset_generation.load(Ordering::Acquire);
        let mut current_epoch = {
            let guard = self.get_connection_named("semantic_cache_epoch")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            read_derived_data_epoch(conn, index_kind)?
        };
        if self.semantic_cache_reset_generation.load(Ordering::Acquire) != reset_generation {
            return Err("storage_changed_during_vector_scan".to_string());
        }
        let epoch_ms = elapsed_ms(started);

        let want = k.saturating_add(VISIBILITY_MARGIN);
        let mut candidates = {
            let guard = cache_lock.read().unwrap_or_else(|error| error.into_inner());
            match guard.as_ref() {
                Some(cache) if cache.epoch == current_epoch => {
                    check_query_dimensions(query, cache)?;
                    Some(cache.top_candidates(query, want.min(cache.rows())))
                }
                _ => None,
            }
        };

        let mut mode = "resident_hit";
        if candidates.is_none() {
            // A concurrent first query should wait for one loader and then use
            // its result rather than materialising a second full matrix.
            let _load_guard = self
                .cache_load_lock(index_kind)
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            current_epoch = {
                let guard = self.get_connection_named("semantic_cache_epoch_reload")?;
                let conn = guard.as_ref().ok_or("Database not initialized")?;
                read_derived_data_epoch(conn, index_kind)?
            };
            if self.semantic_cache_reset_generation.load(Ordering::Acquire) != reset_generation {
                return Err("storage_changed_during_vector_scan".to_string());
            }
            candidates = {
                let guard = cache_lock.read().unwrap_or_else(|error| error.into_inner());
                match guard.as_ref() {
                    Some(cache) if cache.epoch == current_epoch => {
                        check_query_dimensions(query, cache)?;
                        Some(cache.top_candidates(query, want.min(cache.rows())))
                    }
                    _ => None,
                }
            };
            if candidates.is_none() {
                // Do not keep a stale matrix alive while the replacement is
                // being scanned. This is the common path after a trigger-driven
                // delete or another mutation the write hook could not absorb.
                {
                    let mut guard = cache_lock
                        .write()
                        .unwrap_or_else(|error| error.into_inner());
                    if guard
                        .as_ref()
                        .map(|cache| cache.epoch != current_epoch)
                        .unwrap_or(false)
                    {
                        *guard = None;
                        used_at.store(0, Ordering::Relaxed);
                    }
                }
                let loaded = self.load_or_stream_topk(
                    index_kind,
                    current_epoch,
                    query,
                    want,
                    resident_budget,
                    deadline,
                )?;
                if self.semantic_cache_reset_generation.load(Ordering::Acquire) != reset_generation
                {
                    return Err("storage_changed_during_vector_scan".to_string());
                }
                match loaded {
                    CacheQueryMode::Resident {
                        cache,
                        candidates: top,
                        pages,
                        rows,
                        estimated_bytes,
                    } => {
                        tracing::debug!(
                            "[SEMANTIC] resident cache admitted kind={} rows={} pages={} estimated_bytes={} budget={}",
                            index_kind.as_str(),
                            rows,
                            pages,
                            estimated_bytes,
                            resident_budget
                        );
                        let mut guard = cache_lock
                            .write()
                            .unwrap_or_else(|error| error.into_inner());
                        // `reset_semantic_vector_cache` increments the
                        // generation before waiting for this lock. Re-check
                        // while holding it, otherwise a reset that won the
                        // lock between the earlier check and this assignment
                        // could be followed by publication of an old-file
                        // matrix after the reset had already cleared the slot.
                        if self.semantic_cache_reset_generation.load(Ordering::Acquire)
                            != reset_generation
                        {
                            return Err("storage_changed_during_vector_scan".to_string());
                        }
                        *guard = Some(cache);
                        used_at.store(now_ms(), Ordering::Relaxed);
                        candidates = Some(top);
                        mode = "resident_load";
                    }
                    CacheQueryMode::Streaming {
                        candidates: top,
                        pages,
                        rows,
                        estimated_bytes,
                    } => {
                        tracing::debug!(
                            "[SEMANTIC] streaming exact scan kind={} rows={} pages={} estimated_cache_bytes={} budget={}",
                            index_kind.as_str(),
                            rows,
                            pages,
                            estimated_bytes,
                            resident_budget
                        );
                        used_at.store(0, Ordering::Relaxed);
                        candidates = Some(top);
                        mode = "streaming";
                    }
                }
            }
        }
        if self.semantic_cache_reset_generation.load(Ordering::Acquire) != reset_generation {
            return Err("storage_changed_during_vector_scan".to_string());
        }
        if mode == "resident_hit" {
            used_at.store(now_ms(), Ordering::Relaxed);
        }
        let score_ms = elapsed_ms(started) - epoch_ms;

        let candidates = candidates.unwrap_or_default();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        // The epoch check already covers deletions, because the soft-delete
        // trigger's `DELETE FROM derived_embeddings` fires the epoch trigger in
        // turn. Re-checking the survivors anyway keeps "never surface a deleted
        // screenshot" from depending on nested-trigger semantics. The statement
        // is cached: re-parsing it per candidate dominated the whole query in an
        // unoptimized dev build, where the bundled SQLCipher is also built -O0.
        let candidate_count = candidates.len();
        let visible = {
            let guard = self.get_connection_named("semantic_cache_visibility")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            // The two kinds are keyed differently — a screenshot id against an
            // image hash — so the predicate differs even though the question
            // does not.
            let sql = match index_kind {
                DerivedIndexKind::SemanticText => {
                    "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id = ?1 AND is_deleted = 0)"
                }
                DerivedIndexKind::ClipImage => {
                    "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ?1 AND is_deleted = 0)"
                }
            };
            let mut statement = conn
                .prepare_cached(sql)
                .map_err(|error| format!("Failed to prepare visibility re-check: {error}"))?;
            let mut visible = Vec::with_capacity(k.min(candidate_count));
            for candidate in candidates {
                if visible.len() == k {
                    break;
                }
                let active: bool = match index_kind {
                    DerivedIndexKind::SemanticText => {
                        let screenshot_id =
                            candidate.subject_key.parse::<i64>().map_err(|error| {
                                format!(
                                    "Invalid semantic derived subject key '{}': {error}",
                                    candidate.subject_key
                                )
                            })?;
                        statement.query_row([screenshot_id], |row| row.get(0))
                    }
                    DerivedIndexKind::ClipImage => {
                        statement.query_row([&candidate.subject_key], |row| row.get(0))
                    }
                }
                .map_err(|error| format!("Failed to re-check derived subject: {error}"))?;
                if active {
                    visible.push(candidate);
                }
            }
            visible
        };
        if self.semantic_cache_reset_generation.load(Ordering::Acquire) != reset_generation {
            return Err("storage_changed_during_vector_scan".to_string());
        }
        tracing::debug!(
            "[SEMANTIC] {} topk k={k} epoch={epoch_ms:.1}ms score={score_ms:.1}ms visibility={:.1}ms mode={mode} candidates={candidate_count} returned={}",
            index_kind.as_str(),
            elapsed_ms(started) - epoch_ms - score_ms,
            visible.len()
        );
        Ok(visible)
    }

    /// Loads one index kind's matrix, or performs a bounded-memory exact scan
    /// when admitting the full set would exceed the resident budget.
    ///
    /// Two locks matter here and they are released for different reasons. The
    /// database mutex is taken only for the epoch read, because the scan runs on
    /// an independent read-only connection: a 51,931-row CLIP load would
    /// otherwise hold the process-wide, non-reentrant mutex the reverse-IPC
    /// storage bridge also needs. That alone is not enough — under this
    /// database's rollback journal mode a reader blocks a writer's commit
    /// whatever connection it holds — so the scan is also paginated, and the
    /// SQLite SHARED lock is released between pages. [`SCAN_PAGE_ROWS`] records
    /// the measurements.
    ///
    /// The epoch is read *before* the scan, which is the conservative order. A
    /// write landing mid-scan may or may not be picked up by a later page, but
    /// it also advances `data_epoch` past the value this matrix carries, so the
    /// next query sees a mismatch and reloads rather than trusting a matrix that
    /// straddled a write. Reading the epoch afterwards would invert that: the
    /// matrix would claim a freshness the pages before the write do not have.
    fn load_or_stream_topk(
        &self,
        index_kind: DerivedIndexKind,
        epoch: i64,
        query: &[f32],
        want: usize,
        resident_budget: usize,
        deadline: Option<Instant>,
    ) -> Result<CacheQueryMode, String> {
        self.with_vector_scan_connection("load_semantic_vector_cache", |conn| {
            let stats = read_visible_cache_stats(conn, index_kind)?;
            if let Some(dimensions) = stats.dimensions {
                if query.len() != dimensions {
                    return Err(format!(
                        "dimension_mismatch: query has {} dimension(s), the stored index has {}",
                        query.len(),
                        dimensions
                    ));
                }
            }
            let dimensions = stats.dimensions.unwrap_or(query.len());
            // An index larger than the addressable resident representation is
            // still perfectly searchable by the streaming path. Treat an
            // estimate overflow as "does not fit" rather than turning a
            // memory-policy decision into a query error.
            let estimated_bytes = estimated_cache_bytes(
                stats.rows,
                dimensions,
                stats.subject_key_bytes,
            )
            .unwrap_or(usize::MAX);
            // `stats` is a point-in-time admission hint. A writer may add rows
            // after it completes, so the scan below also refuses any append
            // that would need a backing-container growth.
            let mut peak_cache_bytes = estimated_bytes;
            let mut cache = if estimated_bytes <= resident_budget {
                let candidate = SemanticVectorCache::try_with_capacity(epoch, stats.rows, dimensions)?;
                let allocated_bytes = candidate.allocated_bytes();
                peak_cache_bytes = peak_cache_bytes.max(allocated_bytes);
                if allocated_bytes <= resident_budget {
                    Some(candidate)
                } else {
                    tracing::warn!(
                        "[SEMANTIC] resident cache reservation exceeded budget kind={} allocated_bytes={} budget={}; using streaming scan",
                        index_kind.as_str(),
                        allocated_bytes,
                        resident_budget
                    );
                    None
                }
            } else {
                None
            };
            let top_want = want.min(stats.rows);
            let mut top = TopK::new(top_want);
            let mut skipped = 0u64;
            let (pages, rows) = scan_visible_embedding_pages(conn, index_kind, deadline, |subject, vector| {
                if vector.len() != query.len() {
                    skipped = skipped.saturating_add(1);
                    return Ok(());
                }
                let score = dot_product_wide(query, &vector);
                if let Some(resident) = cache.as_ref() {
                    peak_cache_bytes = peak_cache_bytes.max(resident.allocated_bytes());
                    top.push_with_key(score, &subject, || subject.clone());
                    let fits = resident.can_push_within_budget(
                        &subject,
                        subject.capacity(),
                        vector.len(),
                        resident_budget,
                    );
                    if fits {
                        let pushed = cache
                            .as_mut()
                            .expect("resident cache exists after capacity check")
                            .push(subject, vector);
                        if !pushed {
                            cache = None;
                        } else if let Some(resident) = cache.as_ref() {
                            let allocated_bytes = resident.allocated_bytes();
                            peak_cache_bytes = peak_cache_bytes.max(allocated_bytes);
                            if allocated_bytes > resident_budget {
                                // The preflight is intentionally conservative,
                                // but keep the postcondition explicit for
                                // allocator rounding and future container
                                // changes.
                                cache = None;
                            }
                        }
                    } else {
                        tracing::debug!(
                            "[SEMANTIC] resident cache growth would exceed budget kind={} rows={}; switching to streaming",
                            index_kind.as_str(),
                            resident.rows()
                        );
                        cache = None;
                    }
                } else {
                    let tie_key = subject.clone();
                    top.push_with_key(score, &tie_key, || subject);
                }
                Ok(())
            })?;
            if skipped > 0 {
                tracing::warn!(
                    "[SEMANTIC] resident cache skipped {skipped} row(s) with a foreign dimension"
                );
            }
            let candidates = top
                .into_sorted()
                .into_iter()
                .map(|candidate| ScoredSubject {
                    subject_key: candidate.value,
                    score: candidate.score,
                })
                .collect();
            if let Some(cache) = cache {
                let allocated_bytes = cache.allocated_bytes();
                if allocated_bytes <= resident_budget {
                    return Ok(CacheQueryMode::Resident {
                        cache,
                        candidates,
                        pages,
                        rows,
                        estimated_bytes,
                    });
                }
                tracing::warn!(
                    "[SEMANTIC] resident cache allocation exceeded budget kind={} allocated_bytes={} budget={}; serving this query without caching",
                    index_kind.as_str(),
                    allocated_bytes,
                    resident_budget
                );
                return Ok(CacheQueryMode::Streaming {
                    candidates,
                    pages,
                    rows,
                    estimated_bytes: peak_cache_bytes.max(allocated_bytes),
                });
            }
            Ok(CacheQueryMode::Streaming {
                candidates,
                pages,
                rows,
                estimated_bytes: peak_cache_bytes,
            })
        })
    }

    /// Write-path hook: keeps the resident rows current so continuous capture
    /// does not invalidate the whole matrix every few seconds. Called with the
    /// database mutex held; must never re-enter a storage method that locks it.
    ///
    /// The delta is applied only when the cache was current *before* this write
    /// (`epoch_before`). A cache that already missed an unobservable mutation —
    /// the screenshot soft-delete trigger, say — must not be stamped with the
    /// new epoch, or it would claim freshness while still holding a row SQLite
    /// has dropped.
    pub(super) fn note_semantic_cache_write(
        &self,
        index_kind: DerivedIndexKind,
        subject_key: &str,
        vector: &[f32],
        epoch_before: i64,
        epoch_after: i64,
    ) {
        let (cache_lock, _) = self.cache_slot(index_kind);
        let mut guard = cache_lock
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(cache) = guard.as_mut() else {
            return;
        };
        if cache.epoch != epoch_before {
            // Already behind: let the next query reload rather than stack a
            // delta on a matrix of unknown content.
            return;
        }
        if epoch_after == epoch_before {
            // The visible-set triggers did not fire, so this write did not
            // become query-visible. Adding it would put a row in the matrix
            // that a SQL reader would not return.
            return;
        }
        if !cache.can_push_within_budget(
            subject_key,
            subject_key.len(),
            vector.len(),
            cache_budget(index_kind),
        ) {
            // Do not let a live capture force a Vec/HashMap growth beyond the
            // resident budget and then free it after the fact. The next query
            // will take the bounded streaming path if the corpus no longer
            // fits.
            *guard = None;
            return;
        }
        if cache.upsert(subject_key, vector) {
            cache.epoch = epoch_after;
            if cache.allocated_bytes() > cache_budget(index_kind) {
                tracing::debug!(
                    "[SEMANTIC] dropping {} resident cache after a write crossed its {} byte budget",
                    index_kind.as_str(),
                    cache_budget(index_kind)
                );
                *guard = None;
            }
        } else {
            // Foreign dimension: drop the cache rather than serve a matrix that
            // no longer matches the store.
            *guard = None;
        }
    }

    /// Write-path hook for explicit subject deletion. Same staleness rule as
    /// [`Self::note_semantic_cache_write`].
    pub(super) fn note_semantic_cache_removal(
        &self,
        index_kind: DerivedIndexKind,
        subject_key: &str,
        epoch_before: i64,
        epoch_after: i64,
    ) {
        let (cache_lock, _) = self.cache_slot(index_kind);
        let mut guard = cache_lock
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(cache) = guard.as_mut() else {
            return;
        };
        if cache.epoch != epoch_before || epoch_after == epoch_before {
            return;
        }
        cache.remove(subject_key);
        cache.epoch = epoch_after;
    }

    /// Drops every resident matrix unconditionally.
    ///
    /// Required whenever the underlying database connection is swapped: the
    /// freshness check compares `cache.epoch` against `derived_index_state`'s
    /// counter, and that counter is per-database. After a backup import points
    /// this same `StorageState` at a different file, an unreset cache whose
    /// epoch happens to match the new database's counter would score queries
    /// against the *old* database's vectors — the id-based visibility re-check
    /// cannot catch it, because ids that exist in both files pass.
    ///
    /// Both kinds are dropped together because both are keyed to the file that
    /// just went away.
    pub(super) fn reset_semantic_vector_cache(&self) {
        // Publish the boundary before clearing the slots. A loader that is
        // already in flight will observe the new generation and refuse to
        // install its old-file result after this point.
        self.semantic_cache_reset_generation
            .fetch_add(1, Ordering::Release);
        for index_kind in [DerivedIndexKind::SemanticText, DerivedIndexKind::ClipImage] {
            let (cache_lock, used_at) = self.cache_slot(index_kind);
            *cache_lock
                .write()
                .unwrap_or_else(|error| error.into_inner()) = None;
            used_at.store(0, Ordering::Relaxed);
        }
    }

    /// Releases each matrix no query has touched for `ttl`.
    ///
    /// Independently, because the two serve different searches: a user working
    /// through the grouping view should not be paying to keep image vectors
    /// resident, and vice versa.
    pub fn evict_semantic_vector_cache_if_idle(&self, ttl: Duration) -> bool {
        let mut released = false;
        for index_kind in [DerivedIndexKind::SemanticText, DerivedIndexKind::ClipImage] {
            released |= self.evict_one_vector_cache_if_idle(index_kind, ttl);
        }
        released
    }

    fn evict_one_vector_cache_if_idle(&self, index_kind: DerivedIndexKind, ttl: Duration) -> bool {
        let (cache_lock, used_at_cell) = self.cache_slot(index_kind);
        let used_at = used_at_cell.load(Ordering::Relaxed);
        if used_at == 0 {
            return false;
        }
        if now_ms().saturating_sub(used_at) < ttl.as_millis() as u64 {
            return false;
        }
        let mut guard = cache_lock
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(cache) = guard.as_ref() else {
            used_at_cell.store(0, Ordering::Relaxed);
            return false;
        };
        tracing::debug!(
            "[SEMANTIC] releasing {} KiB resident {} cache after {:?} idle",
            cache.allocated_bytes() / 1024,
            index_kind.as_str(),
            ttl
        );
        *guard = None;
        used_at_cell.store(0, Ordering::Relaxed);
        true
    }

    /// Diagnostics: estimated private-heap bytes across both resident caches,
    /// zero when nothing is cached. This includes container capacities and the
    /// duplicated subject-key buffers, unlike the old matrix-only readout.
    pub fn semantic_vector_cache_bytes(&self) -> usize {
        [DerivedIndexKind::SemanticText, DerivedIndexKind::ClipImage]
            .into_iter()
            .map(|index_kind| {
                let (cache_lock, _) = self.cache_slot(index_kind);
                cache_lock
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .map(SemanticVectorCache::allocated_bytes)
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Diagnostics: logical f32 payload bytes across both resident caches.
    /// Kept separate from [`Self::semantic_vector_cache_bytes`] so callers can
    /// distinguish vector payload from allocation/container overhead.
    pub fn semantic_vector_cache_matrix_bytes(&self) -> usize {
        [DerivedIndexKind::SemanticText, DerivedIndexKind::ClipImage]
            .into_iter()
            .map(|index_kind| {
                let (cache_lock, _) = self.cache_slot(index_kind);
                cache_lock
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .map(SemanticVectorCache::matrix_bytes)
                    .unwrap_or(0)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_of(rows: &[(&str, Vec<f32>)]) -> SemanticVectorCache {
        let mut cache = SemanticVectorCache::new(1);
        for (key, vector) in rows {
            assert!(cache.push((*key).to_string(), vector.clone()));
        }
        cache
    }

    #[test]
    fn wide_dot_product_matches_the_scalar_definition() {
        let query: Vec<f32> = (0..384).map(|i| (i as f32) * 0.001).collect();
        let row: Vec<f32> = (0..384).map(|i| ((384 - i) as f32) * 0.002).collect();
        let scalar: f32 = query.iter().zip(&row).map(|(a, b)| a * b).sum();
        let wide = dot_product_wide(&query, &row);
        assert!(
            (scalar - wide).abs() < 1e-3,
            "scalar {scalar} vs wide {wide}"
        );
    }

    #[test]
    fn wide_dot_product_handles_a_non_multiple_of_four() {
        let query = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        let row = vec![2.0f32, 2.0, 2.0, 2.0, 2.0];
        assert!((dot_product_wide(&query, &row) - 30.0).abs() < 1e-6);
    }

    #[test]
    fn top_candidates_rank_by_descending_score() {
        let cache = cache_of(&[
            ("1", vec![1.0, 0.0]),
            ("2", vec![0.8, 0.6]),
            ("3", vec![0.0, 1.0]),
        ]);
        let ranked = cache.top_candidates(&[1.0, 0.0], 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].subject_key, "1");
        assert_eq!(ranked[1].subject_key, "2");
        assert!(ranked[0].score >= ranked[1].score);
    }

    #[test]
    fn bounded_topk_matches_full_sort_including_tied_subject_keys() {
        let rows = [
            (0.5, "z"),
            (0.9, "b"),
            (0.9, "a"),
            (0.4, "c"),
            (0.9, "d"),
            (0.9, "c"),
            (-0.1, "aa"),
        ];
        let mut full: Vec<ScoredSubject> = rows
            .iter()
            .map(|(score, subject_key)| ScoredSubject {
                subject_key: (*subject_key).to_string(),
                score: *score,
            })
            .collect();
        full.sort_unstable_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.subject_key.cmp(&b.subject_key))
        });
        full.truncate(4);

        let mut bounded = TopK::new(4);
        for (score, subject_key) in rows {
            bounded.push_with_key(score, subject_key, || subject_key.to_string());
        }
        let bounded: Vec<ScoredSubject> = bounded
            .into_sorted()
            .into_iter()
            .map(|candidate| ScoredSubject {
                subject_key: candidate.value,
                score: candidate.score,
            })
            .collect();
        assert_eq!(bounded, full);
    }

    #[test]
    fn expired_deadline_stops_streaming_exact_before_scoring_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE derived_index_jobs (
                index_kind TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                status TEXT NOT NULL,
                model_id TEXT NOT NULL,
                model_revision TEXT NOT NULL,
                embedding_version INTEGER NOT NULL,
                source_fingerprint TEXT NOT NULL
            );
            CREATE TABLE derived_embeddings (
                index_kind TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                model_id TEXT NOT NULL,
                model_revision TEXT NOT NULL,
                embedding_version INTEGER NOT NULL,
                source_fingerprint TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector_f32 BLOB NOT NULL
            );
            "#,
        )
        .unwrap();
        let error = scan_visible_embedding_pages(
            &conn,
            DerivedIndexKind::ClipImage,
            Some(Instant::now()),
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error, "query_deadline_exceeded_during_exact_scan");
    }

    #[test]
    fn top_candidates_reject_a_foreign_query_dimension() {
        let cache = cache_of(&[("1", vec![1.0, 0.0])]);
        assert!(cache.top_candidates(&[1.0, 0.0, 0.0], 5).is_empty());
    }

    #[test]
    fn a_foreign_query_dimension_is_reported_rather_than_read_as_an_empty_index() {
        let cache = cache_of(&[("1", vec![1.0, 0.0])]);
        let error = check_query_dimensions(&[1.0, 0.0, 0.0], &cache).unwrap_err();
        assert!(error.starts_with("dimension_mismatch:"), "{error}");
        assert!(check_query_dimensions(&[1.0, 0.0], &cache).is_ok());
        // Nothing stored yet: an empty index is the honest answer, not a
        // dimension complaint about a contract that does not exist.
        let empty = SemanticVectorCache::new(1);
        assert!(check_query_dimensions(&[1.0, 0.0, 0.0], &empty).is_ok());
    }

    #[test]
    fn upsert_replaces_in_place_and_appends_new_subjects() {
        let mut cache = cache_of(&[("1", vec![1.0, 0.0])]);
        assert!(cache.upsert("1", &[0.0, 1.0]));
        assert!(cache.upsert("2", &[1.0, 0.0]));
        assert_eq!(cache.rows(), 2);
        let ranked = cache.top_candidates(&[0.0, 1.0], 1);
        assert_eq!(ranked[0].subject_key, "1");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn remove_keeps_the_matrix_and_positions_consistent() {
        let mut cache = cache_of(&[
            ("1", vec![1.0, 0.0]),
            ("2", vec![0.0, 1.0]),
            ("3", vec![0.6, 0.8]),
        ]);
        cache.remove("1");
        assert_eq!(cache.rows(), 2);
        assert_eq!(cache.matrix.len(), 4);
        // "3" was moved into the hole; its vector must still score as itself.
        let ranked = cache.top_candidates(&[0.6, 0.8], 2);
        assert_eq!(ranked[0].subject_key, "3");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        cache.remove("missing");
        assert_eq!(cache.rows(), 2);
    }

    #[test]
    #[ignore = "measurement, not an assertion; run with --release"]
    fn bench_scoring_a_hot_layer_sized_matrix() {
        let (rows, dimensions) = (9558usize, 384usize);
        let mut cache = SemanticVectorCache::new(1);
        for row in 0..rows {
            let vector: Vec<f32> = (0..dimensions)
                .map(|i| (((row * 31 + i * 7) % 1000) as f32) / 1000.0)
                .collect();
            assert!(cache.push(row.to_string(), vector));
        }
        let query: Vec<f32> = (0..dimensions)
            .map(|i| ((i % 997) as f32) / 997.0)
            .collect();
        let _ = cache.top_candidates(&query, 26);
        let runs = 20;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            assert_eq!(cache.top_candidates(&query, 26).len(), 26);
        }
        let per_query = started.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!(
            "top_candidates over {rows}x{dimensions}: {per_query:.3} ms/query, {} KiB resident",
            cache.allocated_bytes() / 1024
        );
    }
}
