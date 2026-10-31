//! Bounded ONNX residency shared by foreground search and background inference.
use crate::ml_protocol::MlSemanticSession;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Conservative session estimates include the parsed tokenizer and weights.
/// CLIP text + MiniLM + the small classifier fit; the large reranker is exclusive.
const RESIDENT_BUDGET_MIB: usize = 640;
const IDLE_TTL: Duration = Duration::from_secs(300);

struct Entry<T> {
    kind: MlSemanticSession,
    used: Instant,
    value: T,
}

pub struct SessionCache<T> {
    entries: VecDeque<Entry<T>>,
    active: Option<MlSemanticSession>,
}

impl<T> Default for SessionCache<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            active: None,
        }
    }
}

impl<T> SessionCache<T> {
    pub fn active(&self) -> Option<&T> {
        let kind = self.active?;
        self.entries
            .iter()
            .find(|entry| entry.kind == kind)
            .map(|entry| &entry.value)
    }

    pub fn resident_sessions(&self) -> Vec<MlSemanticSession> {
        self.entries.iter().map(|entry| entry.kind).collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.active = None;
    }

    /// Evict before allocating a new session. A requested resident survives
    /// pressure/expiry, so the supervisor's warm deadline remains valid.
    pub fn prepare(
        &mut self,
        kind: MlSemanticSession,
        now: Instant,
        memory_pressure: bool,
    ) -> bool {
        self.entries.retain(|entry| {
            entry.kind == kind || (!memory_pressure && now.duration_since(entry.used) < IDLE_TTL)
        });
        if let Some(index) = self.entries.iter().position(|entry| entry.kind == kind) {
            let mut entry = self.entries.remove(index).expect("resident index exists");
            entry.used = now;
            self.entries.push_back(entry);
            self.active = Some(kind);
            return true;
        }
        self.active = None;
        // Keeping both complete CLIP branches duplicates their text weights.
        self.entries
            .retain(|entry| !clip_siblings(entry.kind, kind));
        if kind == MlSemanticSession::BgeRerankerV2M3 {
            self.entries.clear();
        } else {
            self.entries
                .retain(|entry| entry.kind != MlSemanticSession::BgeRerankerV2M3);
            while self
                .entries
                .iter()
                .map(|entry| estimate_mib(entry.kind))
                .sum::<usize>()
                + estimate_mib(kind)
                > RESIDENT_BUDGET_MIB
            {
                if self.entries.pop_front().is_none() {
                    break;
                }
            }
        }
        false
    }

    pub fn insert(&mut self, kind: MlSemanticSession, value: T, now: Instant) {
        self.entries.push_back(Entry {
            kind,
            value,
            used: now,
        });
        self.active = Some(kind);
    }

    pub fn active_mut(&mut self) -> Option<&mut T> {
        let kind = self.active?;
        self.entries
            .iter_mut()
            .find(|entry| entry.kind == kind)
            .map(|entry| &mut entry.value)
    }
}

fn clip_siblings(a: MlSemanticSession, b: MlSemanticSession) -> bool {
    use MlSemanticSession::*;
    matches!(
        (a, b),
        (ChineseClipText, ChineseClipImage) | (ChineseClipImage, ChineseClipText)
    )
}

fn estimate_mib(kind: MlSemanticSession) -> usize {
    use MlSemanticSession::*;
    match kind {
        ChineseClipText => 160,
        ChineseClipImage => 240,
        MinilmL12 => 400,
        BgeSmallZh => 64,
        BgeRerankerV2M3 => 1024,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MlSemanticSession::*;

    fn load(cache: &mut SessionCache<usize>, kind: MlSemanticSession, now: Instant) {
        assert!(!cache.prepare(kind, now, false));
        cache.insert(kind, estimate_mib(kind), now);
    }

    #[test]
    fn alternating_searches_and_classification_keep_search_sessions_resident() {
        let mut cache = SessionCache::default();
        let now = Instant::now();
        for kind in [ChineseClipText, MinilmL12, BgeSmallZh] {
            load(&mut cache, kind, now);
        }
        for kind in [
            ChineseClipText,
            MinilmL12,
            BgeSmallZh,
            MinilmL12,
            ChineseClipText,
        ] {
            assert!(cache.prepare(kind, now, false));
            assert_eq!(cache.active(), Some(&estimate_mib(kind)));
        }
        assert_eq!(cache.resident_sessions().len(), 3);
    }

    #[test]
    fn image_session_replaces_text_branch_without_evicting_recent_minilm() {
        let mut cache = SessionCache::default();
        let now = Instant::now();
        load(&mut cache, ChineseClipText, now);
        load(&mut cache, BgeSmallZh, now);
        load(&mut cache, MinilmL12, now);
        load(&mut cache, ChineseClipImage, now);
        assert_eq!(cache.resident_sessions(), vec![MinilmL12, ChineseClipImage]);
        assert!(cache.prepare(MinilmL12, now, false));
        load(&mut cache, ChineseClipText, now);
        assert_eq!(cache.resident_sessions(), vec![MinilmL12, ChineseClipText]);
    }

    #[test]
    fn reranker_and_memory_pressure_release_other_sessions_before_loading() {
        let mut cache = SessionCache::default();
        let now = Instant::now();
        load(&mut cache, MinilmL12, now);
        load(&mut cache, ChineseClipText, now);
        assert!(cache.prepare(MinilmL12, now, true));
        assert_eq!(cache.resident_sessions(), vec![MinilmL12]);
        load(&mut cache, BgeRerankerV2M3, now);
        assert_eq!(cache.resident_sessions(), vec![BgeRerankerV2M3]);
        load(&mut cache, MinilmL12, now);
        assert_eq!(cache.resident_sessions(), vec![MinilmL12]);
    }

    #[test]
    fn idle_eviction_and_failed_load_do_not_advertise_an_active_model() {
        let mut cache = SessionCache::default();
        let now = Instant::now();
        load(&mut cache, ChineseClipText, now);
        load(&mut cache, MinilmL12, now);
        assert!(cache.prepare(MinilmL12, now + IDLE_TTL, false));
        assert_eq!(cache.resident_sessions(), vec![MinilmL12]);
        assert!(!cache.prepare(ChineseClipText, now + IDLE_TTL, false));
        assert!(cache.active().is_none());
        assert_eq!(cache.resident_sessions(), vec![MinilmL12]);
        cache.clear();
        assert!(cache.resident_sessions().is_empty());
    }
}
