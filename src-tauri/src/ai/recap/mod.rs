//! Bounded, evidence-backed daily recaps. Classification is optional.
mod privacy;
mod progress;
mod runner;
mod screening;
mod selection;
mod summary;
pub mod types;

pub(crate) use privacy::PrivacyFingerprint;
pub use runner::{generate, read_day, read_progress, start_scheduler, RecapRuntime};
pub use types::*;
