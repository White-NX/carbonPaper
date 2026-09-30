//! Bounded, evidence-backed daily recaps. Classification is optional.
mod runner;
mod progress;
mod screening;
mod selection;
pub mod types;

pub use runner::{generate, read_day, read_progress, start_scheduler, RecapRuntime};
pub use types::*;
