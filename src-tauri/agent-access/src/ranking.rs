//! Exact page ordering with a bounded number of candidate identities.
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Hit {
    pub score: f64,
    pub secondary: Option<i64>,
    pub row: i64,
    pub screenshot: i64,
}
impl Ord for Hit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.secondary.cmp(&other.secondary))
            .then_with(|| other.row.cmp(&self.row))
            .then_with(|| self.screenshot.cmp(&other.screenshot))
    }
}
impl PartialOrd for Hit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Hit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Hit {}

pub(super) struct PageRanking {
    capacity: usize,
    ordered: BTreeSet<Hit>,
    by_screenshot: HashMap<i64, Hit>,
}
impl PageRanking {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ordered: BTreeSet::new(),
            by_screenshot: HashMap::new(),
        }
    }
    pub fn insert(&mut self, hit: Hit) {
        if let Some(previous) = self.by_screenshot.get(&hit.screenshot) {
            if *previous >= hit {
                return;
            }
            self.ordered.remove(previous);
        } else if self.ordered.len() == self.capacity {
            // The cutoff can only rise. A discarded screenshot can re-enter
            // with a better row; its previous row cannot affect a later page.
            if self.ordered.first().is_none_or(|cutoff| hit <= *cutoff) {
                return;
            }
            let evicted = self.ordered.pop_first().unwrap();
            self.by_screenshot.remove(&evicted.screenshot);
        }
        self.ordered.insert(hit);
        self.by_screenshot.insert(hit.screenshot, hit);
    }
    pub fn page(self, offset: usize) -> impl Iterator<Item = Hit> {
        self.ordered.into_iter().rev().skip(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicted_screenshot_can_reenter_with_its_best_row_and_stable_tie() {
        let mut ranked = PageRanking::new(2);
        for (shot, row, score) in [
            (1, 1, 1.),
            (2, 4, 2.),
            (3, 6, 3.),
            (1, 3, 3.),
            (1, 2, 3.),
            (2, 5, 2.),
        ] {
            ranked.insert(Hit {
                score,
                secondary: None,
                row,
                screenshot: shot,
            });
        }
        assert_eq!(ranked.page(0).map(|h| h.row).collect::<Vec<_>>(), [2, 6]);
    }
}
