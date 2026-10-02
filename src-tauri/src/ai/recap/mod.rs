//! Bounded, evidence-backed daily recaps. Classification is optional.
mod privacy;
mod progress;
mod runner;
mod screening;
mod selection;
mod summary;
pub mod types;
mod views;

pub(crate) use privacy::PrivacyFingerprint;
pub use runner::{generate, read_day, read_progress, start_scheduler, RecapRuntime};
pub use types::*;
pub use views::{read_day_view, read_records, RecapRecordPage};
