//! Short-lived, encrypted result sets. Source updates leave their order intact;
//! authentication, privacy and database boundaries still invalidate them.
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{de::DeserializeOwned, Serialize};
use zeroize::Zeroizing;

use super::search::SearchContext;
use crate::credential_manager::{decrypt_with_master_key, encrypt_with_master_key};

const IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_SETS: usize = 8;
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct SearchPages<T> {
    entries: VecDeque<Entry<T>>,
}

struct Entry<T> {
    used: Instant,
    snapshot: Arc<SearchSnapshot<T>>,
}

pub(super) struct SearchSnapshot<T> {
    pub token: String,
    pub header: T,
    context: SearchContext,
    query_key: String,
    key: Zeroizing<[u8; 32]>,
    encrypted_items: Vec<u8>,
}

impl<T> Default for SearchPages<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }
}

impl<T> SearchPages<T> {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn find(
        &mut self,
        context: &SearchContext,
        query_key: &str,
        token: Option<&str>,
        offset: usize,
    ) -> Option<Arc<SearchSnapshot<T>>> {
        self.prune(context);
        if token.is_none() && offset == 0 {
            return None;
        }
        let index = self.entries.iter().rposition(|entry| {
            entry.snapshot.query_key == query_key
                && token.is_none_or(|token| entry.snapshot.token == token)
        })?;
        let mut entry = self.entries.remove(index)?;
        entry.used = Instant::now();
        let snapshot = entry.snapshot.clone();
        self.entries.push_back(entry);
        Some(snapshot)
    }

    pub fn insert<I: Serialize>(
        &mut self,
        context: SearchContext,
        query_key: String,
        header: T,
        items: &I,
    ) -> Result<Arc<SearchSnapshot<T>>, String> {
        let bytes = Zeroizing::new(serde_json::to_vec(items).map_err(|_| "RECAP_INVALID_CACHE")?);
        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());
        let encrypted_items = encrypt_with_master_key(key.as_ref(), &bytes)
            .map_err(|_| "RECAP_INVALID_CACHE")?;
        let mut token = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut token);
        let snapshot = Arc::new(SearchSnapshot {
            token: hex::encode(token),
            header,
            context,
            query_key,
            key,
            encrypted_items,
        });
        self.prune(&snapshot.context);
        let size = snapshot.encrypted_items.len();
        if size <= MAX_BYTES {
            while self.entries.len() >= MAX_SETS || self.bytes() + size > MAX_BYTES {
                self.entries.pop_front();
            }
            self.entries.push_back(Entry {
                used: Instant::now(),
                snapshot: snapshot.clone(),
            });
        }
        Ok(snapshot)
    }

    fn bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.snapshot.encrypted_items.len())
            .sum()
    }

    fn prune(&mut self, context: &SearchContext) {
        self.entries.retain(|entry| {
            entry.used.elapsed() <= IDLE_TTL && entry.snapshot.context == *context
        });
    }
}

impl<T> SearchSnapshot<T> {
    pub fn items<I: DeserializeOwned>(&self) -> Result<I, String> {
        let bytes = Zeroizing::new(
            decrypt_with_master_key(self.key.as_ref(), &self.encrypted_items)
                .map_err(|_| "RECAP_INVALID_CACHE")?,
        );
        serde_json::from_slice(&bytes).map_err(|_| "RECAP_INVALID_CACHE".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> SearchContext {
        SearchContext {
            generation: 1,
            privacy: "policy".into(),
            serial: 0,
            session: Some(Instant::now()),
        }
    }

    #[test]
    fn keeps_independent_orders_and_encrypts_resident_text() {
        let context = context();
        let mut pages = SearchPages::default();
        let first = pages
            .insert(context.clone(), "query".into(), (), &vec!["private activity", "second"])
            .unwrap();
        let second = pages
            .insert(context.clone(), "query".into(), (), &vec!["new", "private activity", "second"])
            .unwrap();
        assert!(!first.encrypted_items.windows(16).any(|bytes| bytes == b"private activity"));
        assert_ne!(first.token, second.token);
        assert_eq!(
            pages.find(&context, "query", Some(&first.token), 1).unwrap()
                .items::<Vec<String>>().unwrap(),
            vec!["private activity", "second"]
        );
        assert!(pages.find(&context, "different filters", Some(&first.token), 1).is_none());
        assert!(pages.find(&context, "query", None, 0).is_none());
    }

    #[test]
    fn expires_and_bounds_sets_and_fences_security_context() {
        let context = context();
        let mut pages = SearchPages::default();
        let first = pages.insert(context.clone(), "query".into(), (), &vec![1]).unwrap();
        pages.entries[0].used = Instant::now() - IDLE_TTL - Duration::from_secs(1);
        assert!(pages.find(&context, "query", Some(&first.token), 1).is_none());
        for i in 0..MAX_SETS + 1 {
            pages.insert(context.clone(), "query".into(), (), &vec![i]).unwrap();
        }
        assert_eq!(pages.entries.len(), MAX_SETS);
        for changed in [
            SearchContext { generation: 2, ..context.clone() },
            SearchContext { privacy: "new policy".into(), ..context.clone() },
            SearchContext { serial: 1, ..context.clone() },
            SearchContext { session: None, ..context.clone() },
        ] {
            let entry = pages.insert(context.clone(), "query".into(), (), &vec![1]).unwrap();
            assert!(pages.find(&changed, "query", Some(&entry.token), 1).is_none());
        }
        pages.clear();
        assert!(pages.entries.is_empty());
    }
}
