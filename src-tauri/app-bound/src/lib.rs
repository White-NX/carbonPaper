//! Narrow app-bound key broker. No UI, model runtime, or archive credentials.
#[cfg(all(feature = "development-runtime", not(debug_assertions)))]
compile_error!("The app-bound development runtime cannot be compiled into a release build");

pub mod crypto;
#[cfg(all(feature = "development-runtime", windows))]
pub mod development;
pub mod ledger;
pub mod manifest;
pub mod protocol;
#[cfg(windows)]
pub mod windows;

pub use protocol::{BrokerError, Result};
