//! Narrow app-bound key broker. No UI, model runtime, or archive credentials.
pub mod crypto;
pub mod ledger;
pub mod manifest;
pub mod protocol;
#[cfg(windows)]
pub mod windows;

pub use protocol::{BrokerError, Result};
