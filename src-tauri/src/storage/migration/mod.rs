//! Unified migration management for storage.

pub mod bitmap_index;
pub mod data_dir;
pub mod dedup;
pub mod hmac;
pub mod plaintext;

use std::sync::atomic::{AtomicBool, Ordering};

/// RAII guard that resets migration flags when dropped.
pub struct MigrationRunGuard<'a> {
    pub in_progress: &'a AtomicBool,
    pub cancel_requested: &'a AtomicBool,
}

impl<'a> MigrationRunGuard<'a> {
    pub fn new(in_progress: &'a AtomicBool, cancel_requested: &'a AtomicBool) -> Self {
        Self {
            in_progress,
            cancel_requested,
        }
    }
}

impl Drop for MigrationRunGuard<'_> {
    fn drop(&mut self) {
        self.in_progress.store(false, Ordering::SeqCst);
        self.cancel_requested.store(false, Ordering::SeqCst);
    }
}
