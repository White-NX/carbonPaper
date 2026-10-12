//! A generation-scoped incremental overlay and a query-scoped scoring snapshot.
//! Only changed vector payloads are loaded again. In-flight readers retain their
//! original base/overlay pair while publication or later writes advance the cache.
use super::*;
use std::collections::HashMap;

#[derive(Clone)]
struct TailRow {
    key: Arc<str>,
    vector: Option<Arc<Vec<f32>>>,
}

#[derive(Clone, Default)]
pub(super) struct TailData {
    rows: Vec<TailRow>,
    positions: HashMap<Arc<str>, usize>,
}

#[derive(Default)]
struct CachedTail {
    storage_generation: Option<u64>,
    epoch: Option<u64>,
    data: Arc<TailData>,
}

impl CachedTail {
    fn apply(
        &mut self,
        epoch: u64,
        changes: Vec<DerivedAnnTailRow>,
        limit: usize,
    ) -> Result<Arc<TailData>, String> {
        if self.epoch.is_some_and(|cached| epoch < cached) {
            return Err("ann_data_epoch_regressed".to_string());
        }
        let added = changes
            .iter()
            .filter(|row| !self.data.positions.contains_key(row.subject_key.as_str()))
            .count();
        if self.data.rows.len().saturating_add(added) > limit {
            return Err(format!(
                "ann_tail_too_large:{}",
                self.data.rows.len().saturating_add(added)
            ));
        }
        if !changes.is_empty() {
            // Copy-on-write clones only keys and vector Arcs when another query
            // pins this version, never the entire f32 matrix.
            let data = Arc::make_mut(&mut self.data);
            for change in changes {
                let vector = change.vector.map(Arc::new);
                if let Some(&position) = data.positions.get(change.subject_key.as_str()) {
                    data.rows[position].vector = vector;
                } else {
                    let key: Arc<str> = change.subject_key.into();
                    data.positions.insert(Arc::clone(&key), data.rows.len());
                    data.rows.push(TailRow { key, vector });
                }
            }
        }
        self.epoch = Some(epoch);
        Ok(Arc::clone(&self.data))
    }
}

#[derive(Default)]
pub(super) struct TailCache(Mutex<CachedTail>);

impl TailCache {
    pub(super) fn load(
        &self,
        storage: &StorageState,
        covered_epoch: u64,
        deadline: Option<Instant>,
    ) -> Result<Arc<TailData>, String> {
        let mut cache = self.0.lock().unwrap_or_else(|error| error.into_inner());
        check_ann_deadline(deadline)?;
        let storage_generation = storage.db_generation();
        if cache
            .storage_generation
            .is_some_and(|bound| bound != storage_generation)
        {
            return Err("ann_storage_generation_changed".to_string());
        }
        let (epoch, changes) = storage.derived_ann_tail_updates(
            DerivedIndexKind::ClipImage,
            covered_epoch,
            cache.epoch,
            MAX_TAIL_ROWS,
            deadline,
        )?;
        if storage.db_generation() != storage_generation {
            return Err("ann_storage_generation_changed".to_string());
        }
        let data = cache.apply(epoch, changes, MAX_TAIL_ROWS as usize)?;
        cache.storage_generation = Some(storage_generation);
        Ok(data)
    }

    pub(super) fn evict(&self) {
        if let Ok(mut cache) = self.0.try_lock() {
            *cache = CachedTail {
                storage_generation: cache.storage_generation,
                ..Default::default()
            };
        }
    }
}

pub(crate) struct ClipAnnSnapshot {
    generation: Arc<GenerationReader>,
    tail: Arc<TailData>,
    query: Vec<f32>,
    tail_scores: Vec<(usize, f32)>,
}

impl ClipAnnSnapshot {
    pub(super) fn new(
        generation: Arc<GenerationReader>,
        tail: Arc<TailData>,
        query: &[f32],
        deadline: Option<Instant>,
    ) -> Result<Self, String> {
        let mut tail_scores = Vec::with_capacity(tail.rows.len());
        for (index, row) in tail.rows.iter().enumerate() {
            if index % 64 == 0 {
                check_ann_deadline(deadline)?;
            }
            if let Some(vector) = &row.vector {
                if vector.len() == query.len() {
                    tail_scores.push((index, dot_product(query, vector)));
                }
            }
        }
        Ok(Self {
            generation,
            tail,
            query: query.to_vec(),
            tail_scores,
        })
    }

    fn scores(&self) -> impl Iterator<Item = (&str, f32)> {
        self.tail_scores
            .iter()
            .map(|(index, score)| (self.tail.rows[*index].key.as_ref(), *score))
    }

    pub(crate) fn query(
        &self,
        k: usize,
        deadline: Option<Instant>,
    ) -> Result<ClipAnnQuery, String> {
        let candidates = ann_candidates_with_tail(
            &self.generation,
            &self.query,
            k,
            deadline,
            |key| self.tail.positions.contains_key(key),
            self.scores(),
        )?;
        Ok(ClipAnnQuery {
            candidates,
            mode: "ann",
            tail_rows: self.tail.rows.len(),
        })
    }

    pub(crate) fn exact(
        &self,
        k: usize,
        deadline: Option<Instant>,
    ) -> Result<ClipAnnQuery, String> {
        let candidates = exact_candidates_with_tail(
            &self.generation,
            &self.query,
            k,
            deadline,
            |key| self.tail.positions.contains_key(key),
            self.scores(),
        )?;
        Ok(ClipAnnQuery {
            candidates,
            mode: "flat_exact",
            tail_rows: self.tail.rows.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, vector: Option<Vec<f32>>) -> DerivedAnnTailRow {
        DerivedAnnTailRow {
            subject_key: key.to_string(),
            vector,
        }
    }

    #[test]
    fn refresh_preserves_pinned_vectors_tombstones_and_unchanged_allocations() {
        let mut cache = CachedTail::default();
        let old = cache
            .apply(
                1,
                vec![
                    row("a", Some(vec![1.0, 0.0])),
                    row("b", Some(vec![0.0, 1.0])),
                ],
                3,
            )
            .unwrap();
        let unchanged = cache.apply(1, Vec::new(), 3).unwrap();
        assert!(Arc::ptr_eq(&old, &unchanged));
        let next = cache
            .apply(2, vec![row("a", None), row("c", Some(vec![0.5, 0.5]))], 3)
            .unwrap();
        assert_eq!(
            old.rows[0].vector.as_deref().unwrap().as_slice(),
            &[1.0, 0.0]
        );
        assert!(next.rows[0].vector.is_none());
        assert!(Arc::ptr_eq(
            old.rows[1].vector.as_ref().unwrap(),
            next.rows[1].vector.as_ref().unwrap()
        ));
        assert_eq!(next.positions["c"], 2);
        let before_failure = Arc::clone(&cache.data);
        assert!(cache
            .apply(3, vec![row("d", Some(vec![1.0, 0.0]))], 3)
            .is_err());
        assert_eq!(cache.epoch, Some(2));
        assert!(Arc::ptr_eq(&before_failure, &cache.data));
        assert!(cache.apply(1, Vec::new(), 3).is_err());
    }

    #[test]
    fn query_snapshot_keeps_scores_and_deletions_stable_across_refreshes() {
        let temp = tempfile::tempdir().unwrap();
        let reader = Arc::new(super::super::tests::test_reader(
            temp.path(),
            41,
            &["a", "b"],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        ));
        let rows = vec![
            row("a", None),
            row("b", Some(vec![0.8, 0.6])),
            row("new", Some(vec![0.9, 0.1])),
        ];
        let mut cache = CachedTail::default();
        let tail = cache.apply(1, rows.clone(), 10).unwrap();
        let snapshot = ClipAnnSnapshot::new(Arc::clone(&reader), tail, &[1.0, 0.0], None).unwrap();
        let expected = ann_candidates(&reader, &rows, &[1.0, 0.0], 10, None).unwrap();
        cache.apply(2, vec![row("new", None)], 10).unwrap();
        assert_eq!(snapshot.query(10, None).unwrap().candidates, expected);
        assert_eq!(snapshot.exact(10, None).unwrap().candidates, expected);
        let next =
            ClipAnnSnapshot::new(reader, Arc::clone(&cache.data), &[1.0, 0.0], None).unwrap();
        assert_eq!(next.query(10, None).unwrap().candidates.len(), 1);
    }

    #[test]
    fn cache_rejects_database_replacement_even_after_memory_eviction() {
        let temp = tempfile::tempdir().unwrap();
        let storage = crate::storage::test_storage_with_schema(temp.path().to_path_buf());
        let cache = TailCache::default();
        let pinned = cache.load(&storage, 0, None).unwrap();
        storage.bump_db_generation();
        assert!(cache
            .load(&storage, 0, None)
            .err()
            .unwrap()
            .contains("storage_generation_changed"));
        cache.evict();
        assert!(cache
            .load(&storage, 0, None)
            .err()
            .unwrap()
            .contains("storage_generation_changed"));
        assert!(pinned.rows.is_empty());
    }
}
