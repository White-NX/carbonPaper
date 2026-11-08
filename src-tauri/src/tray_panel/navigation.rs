//! One pending, finite navigation intent survives a recreated main WebView.

use super::model::ReadIdentity;
use serde::Serialize;
use std::time::{Duration, Instant};
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Navigation {
    Timeline,
    Search { query: String },
    Snapshot { id: i64, timestamp_ms: i64 },
}

impl Drop for Navigation {
    fn drop(&mut self) {
        if let Self::Search { query } = self {
            query.zeroize();
        }
    }
}

pub(super) struct PendingNavigation {
    pub intent: Navigation,
    pub identity: Option<ReadIdentity>,
    created: Instant,
}

impl PendingNavigation {
    pub fn new(intent: Navigation, identity: Option<ReadIdentity>) -> Self {
        Self {
            intent,
            identity,
            created: Instant::now(),
        }
    }

    pub fn admitted(&self, current: Option<ReadIdentity>) -> bool {
        self.created.elapsed() < Duration::from_secs(5 * 60)
            && current.is_some()
            && self
                .identity
                .is_none_or(|identity| Some(identity) == current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_handoffs_require_the_original_session_and_database() {
        let original = ReadIdentity {
            session: Instant::now(),
            database: 3,
        };
        let request = PendingNavigation::new(
            Navigation::Snapshot {
                id: 7,
                timestamp_ms: 1_700_000_000_123,
            },
            Some(original),
        );
        assert!(request.admitted(Some(original)));
        assert!(!request.admitted(None));
        assert!(!request.admitted(Some(ReadIdentity {
            database: 4,
            ..original
        })));
        assert!(!request.admitted(Some(ReadIdentity {
            session: Instant::now(),
            ..original
        })));
    }

    #[test]
    fn searches_entered_while_locked_wait_for_unlock_and_do_not_replay_indefinitely() {
        let current = ReadIdentity {
            session: Instant::now(),
            database: 1,
        };
        let mut request = PendingNavigation::new(
            Navigation::Search {
                query: "meeting".into(),
            },
            None,
        );
        assert!(!request.admitted(None));
        assert!(request.admitted(Some(current)));
        request.created = Instant::now() - Duration::from_secs(5 * 60);
        assert!(!request.admitted(Some(current)));
    }
}
