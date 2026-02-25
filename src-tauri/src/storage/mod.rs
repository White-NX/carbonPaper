//! Storage Management Module - SQLCipher Encrypted SQLite Database and File Storage
//!
//! This module provides:
//! 1. Encrypted storage of screenshots
//! 2. Screenshot metadata and OCR results
//! 3. OCR data storage and search

mod encryption;
mod image_io;
mod link_scoring;
mod migration;
mod plaintext;
mod policy;
mod process;
mod schema;
mod screenshot;
mod search;
mod types;

#[allow(unused_imports)]
pub use image_io::{read_encrypted_image_as_base64, read_image_as_base64};
pub use types::*;

use crate::credential_manager::CredentialManagerState;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// StorageState manages the encrypted database connection, data directory paths, and migration state.
/// It provides methods for initializing storage, saving/loading screenshots and OCR results,
/// and migrating the data directory.
pub struct StorageState {
    /// Database connection
    db: Mutex<Option<Connection>>,
    /// Data directory (contains database, screenshots, logs, etc.)
    pub data_dir: Mutex<PathBuf>,
    pub screenshot_dir: Mutex<PathBuf>,
    /// Credential manager state for encryption key management
    credential_state: Arc<CredentialManagerState>,
    initialized: Mutex<bool>,
    migration_cancel_requested: AtomicBool,
    migration_in_progress: AtomicBool,
    /// Diagnostic: tracks which operation currently holds the DB mutex
    lock_holder: Mutex<&'static str>,
    /// Approximate OCR row count for O(1) IDF lookups (initialized from DB, maintained on insert/delete)
    ocr_row_count: AtomicU64,
    /// Whether dedup migration has already been performed this session
    dedup_migrated: AtomicBool,
}

impl StorageState {
    pub fn new(data_dir: PathBuf, credential_state: Arc<CredentialManagerState>) -> Self {
        let screenshot_dir = data_dir.join("screenshots");

        Self {
            db: Mutex::new(None),
            data_dir: Mutex::new(data_dir),
            screenshot_dir: Mutex::new(screenshot_dir),
            credential_state,
            initialized: Mutex::new(false),
            migration_cancel_requested: AtomicBool::new(false),
            migration_in_progress: AtomicBool::new(false),
            lock_holder: Mutex::new(""),
            ocr_row_count: AtomicU64::new(0),
            dedup_migrated: AtomicBool::new(false),
        }
    }

    /// Convert an absolute image path to a relative path (relative to data_dir).
    /// Uses forward slashes for consistency across platforms.
    fn to_relative_image_path(&self, abs_path: &Path) -> String {
        let data_dir = self.data_dir.lock().unwrap().clone();
        match abs_path.strip_prefix(&data_dir) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => abs_path.to_string_lossy().replace('\\', "/"),
        }
    }

    /// Resolve a (possibly relative) image path to an absolute PathBuf.
    /// If the path is already absolute, return it as-is for backward compatibility.
    fn resolve_image_path(&self, rel_path: &str) -> PathBuf {
        let p = Path::new(rel_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.data_dir.lock().unwrap().join(rel_path)
        }
    }

    /// Request cancellation of an ongoing migration.
    pub fn request_migration_cancel(&self) -> bool {
        self.migration_cancel_requested.store(true, Ordering::SeqCst);
        self.migration_in_progress.load(Ordering::SeqCst)
    }

    fn is_migration_cancel_requested(&self) -> bool {
        self.migration_cancel_requested.load(Ordering::SeqCst)
    }

    /// Acquire DB connection with caller identification for diagnostic logging.
    fn get_connection_named(
        &self,
        caller: &'static str,
    ) -> Result<std::sync::MutexGuard<'_, Option<Connection>>, String> {
        let wait_start = std::time::Instant::now();
        let current_holder = self.lock_holder.lock().ok().map(|g| *g).unwrap_or("?");
        let guard = self.db.lock().unwrap();
        let wait_dur = wait_start.elapsed();
        // Update lock holder to current caller
        if let Ok(mut h) = self.lock_holder.lock() {
            *h = caller;
        }
        if wait_dur.as_secs() >= 10 {
            tracing::warn!(
                "[DIAG:DB] Mutex wait took {:?} for '{}' (was held by '{}')",
                wait_dur,
                caller,
                current_holder
            );
        }
        if guard.is_none() {
            return Err("Database not initialized".to_string());
        }
        Ok(guard)
    }
}
