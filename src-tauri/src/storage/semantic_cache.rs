//! Resident vector cache for the `semantic_text` derived index.
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

use super::derived_index::{
    decode_vector, read_derived_data_epoch, visible_embedding_scan_sql, DerivedIndexKind,
    ScoredSubject,
};
use super::StorageState;
use rusqlite::params;
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Candidates scored past `k`, so a subject deleted in the window between the
/// epoch read and the scan can be dropped without a second retrieval pass.
const VISIBILITY_MARGIN: usize = 16;

/// Idle window after which the resident matrix is released. One NL cluster
/// session is a burst of a few queries; five minutes outlives the burst without
/// holding the memory for the hours until the next one.
pub const SEMANTIC_CACHE_IDLE_TTL: Duration = Duration::from_secs(300);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
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

    pub(super) fn rows(&self) -> usize {
        self.subjects.len()
    }

    pub(super) fn resident_bytes(&self) -> usize {
        self.matrix.len() * std::mem::size_of::<f32>()
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
        let mut scored: Vec<(f32, u32)> = self
            .matrix
            .chunks_exact(self.dimensions)
            .enumerate()
            .map(|(index, row)| (dot_product_wide(query, row), index as u32))
            .collect();
        let descending = |a: &(f32, u32), b: &(f32, u32)| {
            b.0.partial_cmp(&a.0).unwrap_or(CmpOrdering::Equal)
        };
        let take = want.min(scored.len());
        // Only the surviving candidates need ordering; the rest stay unsorted.
        scored.select_nth_unstable_by(take - 1, descending);
        scored.truncate(take);
        scored.sort_unstable_by(descending);
        scored
            .into_iter()
            .map(|(score, index)| ScoredSubject {
                subject_key: self.subjects[index as usize].clone(),
                score,
            })
            .collect()
    }
}

impl StorageState {
    /// Cosine top-K over the resident matrix, loading it on first use.
    pub(super) fn semantic_topk_resident(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredSubject>, String> {
        let started = Instant::now();
        let current_epoch = {
            let guard = self.get_connection_named("semantic_cache_epoch")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            read_derived_data_epoch(conn, DerivedIndexKind::SemanticText)?
        };
        let epoch_ms = elapsed_ms(started);

        let want = k.saturating_add(VISIBILITY_MARGIN);
        let mut candidates = {
            let guard = self
                .semantic_vector_cache
                .read()
                .unwrap_or_else(|error| error.into_inner());
            match guard.as_ref() {
                Some(cache) if cache.epoch == current_epoch => Some(cache.top_candidates(query, want)),
                _ => None,
            }
        };

        let reloaded = candidates.is_none();
        if candidates.is_none() {
            let loaded = self.load_semantic_vector_cache()?;
            let top = loaded.top_candidates(query, want);
            let mut guard = self
                .semantic_vector_cache
                .write()
                .unwrap_or_else(|error| error.into_inner());
            *guard = Some(loaded);
            candidates = Some(top);
        }
        self.semantic_cache_used_at.store(now_ms(), Ordering::Relaxed);
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
            let mut statement = conn
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id = ?1 AND is_deleted = 0)",
                )
                .map_err(|error| format!("Failed to prepare visibility re-check: {error}"))?;
            let mut visible = Vec::with_capacity(k);
            for candidate in candidates {
                if visible.len() == k {
                    break;
                }
                let screenshot_id = candidate.subject_key.parse::<i64>().map_err(|error| {
                    format!(
                        "Invalid semantic derived subject key '{}': {error}",
                        candidate.subject_key
                    )
                })?;
                let active: bool = statement
                    .query_row([screenshot_id], |row| row.get(0))
                    .map_err(|error| format!("Failed to re-check derived subject: {error}"))?;
                if active {
                    visible.push(candidate);
                }
            }
            visible
        };
        tracing::debug!(
            "[SEMANTIC] topk k={k} epoch={epoch_ms:.1}ms score={score_ms:.1}ms visibility={:.1}ms reloaded={reloaded} candidates={candidate_count} returned={}",
            elapsed_ms(started) - epoch_ms - score_ms,
            visible.len()
        );
        Ok(visible)
    }

    /// Reads the epoch and every visible row under one connection guard, so the
    /// matrix and the epoch it is tagged with cannot straddle a write.
    fn load_semantic_vector_cache(&self) -> Result<SemanticVectorCache, String> {
        let guard = self.get_connection_named("load_semantic_vector_cache")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let epoch = read_derived_data_epoch(conn, DerivedIndexKind::SemanticText)?;
        let mut cache = SemanticVectorCache::new(epoch);
        let mut statement = conn
            .prepare(&visible_embedding_scan_sql())
            .map_err(|error| format!("Failed to prepare resident vector scan: {error}"))?;
        let mut rows = statement
            .query(params![DerivedIndexKind::SemanticText.as_str()])
            .map_err(|error| format!("Failed to scan resident vectors: {error}"))?;
        let mut skipped = 0u64;
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
            if !cache.push(subject_key, vector) {
                skipped += 1;
            }
        }
        if skipped > 0 {
            // A mixed-dimension store cannot happen under one model contract;
            // report it rather than silently ranking a partial corpus.
            tracing::warn!(
                "[SEMANTIC] resident cache skipped {skipped} row(s) with a foreign dimension"
            );
        }
        tracing::debug!(
            "[SEMANTIC] resident cache loaded {} rows ({} KiB) at epoch {}",
            cache.rows(),
            cache.resident_bytes() / 1024,
            cache.epoch
        );
        Ok(cache)
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
        subject_key: &str,
        vector: &[f32],
        epoch_before: i64,
        epoch_after: i64,
    ) {
        let mut guard = self
            .semantic_vector_cache
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
        if cache.upsert(subject_key, vector) {
            cache.epoch = epoch_after;
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
        subject_key: &str,
        epoch_before: i64,
        epoch_after: i64,
    ) {
        let mut guard = self
            .semantic_vector_cache
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

    /// Drops the resident matrix unconditionally.
    ///
    /// Required whenever the underlying database connection is swapped: the
    /// freshness check compares `cache.epoch` against `derived_index_state`'s
    /// counter, and that counter is per-database. After a backup import points
    /// this same `StorageState` at a different file, an unreset cache whose
    /// epoch happens to match the new database's counter would score queries
    /// against the *old* database's vectors — the id-based visibility re-check
    /// cannot catch it, because ids that exist in both files pass.
    pub(super) fn reset_semantic_vector_cache(&self) {
        *self
            .semantic_vector_cache
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.semantic_cache_used_at.store(0, Ordering::Relaxed);
    }

    /// Releases the matrix when no query has touched it for `ttl`.
    pub fn evict_semantic_vector_cache_if_idle(&self, ttl: Duration) -> bool {
        let used_at = self.semantic_cache_used_at.load(Ordering::Relaxed);
        if used_at == 0 {
            return false;
        }
        if now_ms().saturating_sub(used_at) < ttl.as_millis() as u64 {
            return false;
        }
        let mut guard = self
            .semantic_vector_cache
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let Some(cache) = guard.as_ref() else {
            self.semantic_cache_used_at.store(0, Ordering::Relaxed);
            return false;
        };
        tracing::debug!(
            "[SEMANTIC] releasing {} KiB resident vector cache after {:?} idle",
            cache.resident_bytes() / 1024,
            ttl
        );
        *guard = None;
        self.semantic_cache_used_at.store(0, Ordering::Relaxed);
        true
    }

    /// Diagnostics: resident vector bytes, zero when nothing is cached.
    pub fn semantic_vector_cache_bytes(&self) -> usize {
        self.semantic_vector_cache
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(SemanticVectorCache::resident_bytes)
            .unwrap_or(0)
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
        assert!((scalar - wide).abs() < 1e-3, "scalar {scalar} vs wide {wide}");
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
    fn top_candidates_reject_a_foreign_query_dimension() {
        let cache = cache_of(&[("1", vec![1.0, 0.0])]);
        assert!(cache.top_candidates(&[1.0, 0.0, 0.0], 5).is_empty());
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
            cache.resident_bytes() / 1024
        );
    }
}
