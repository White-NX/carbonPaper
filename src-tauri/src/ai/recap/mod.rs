//! Bounded, evidence-backed daily recaps. Classification is optional.
mod privacy;
mod progress;
mod runner;
mod screening;
pub(crate) mod search;
mod selection;
mod summary;
pub mod types;
mod unified;
mod views;

pub(crate) use privacy::PrivacyFingerprint;
pub use runner::{generate, read_day, read_progress, start_scheduler, RecapRuntime};
pub use search::{
    prepare as prepare_search, search as search_activities, status as search_status,
    RecapSearchQuery, RecapSearchResult, RecapSearchRuntime, RecapSearchStatus,
};
pub use types::*;
pub use unified::{search as search_unified, UnifiedSearchQuery, UnifiedSearchResult};
pub(crate) use views::with_current_day;
pub use views::{read_day_view, read_records, RecapRecordPage};
