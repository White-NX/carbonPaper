//! Storage Management Module - SQLCipher Encrypted SQLite Database and File Storage
//!
//! This module provides:
//! 1. Encrypted storage of screenshots
//! 2. Screenshot metadata and OCR results
//! 3. OCR data storage and search

mod encryption;
mod image_io;
mod link_scoring;
pub mod migration;
mod policy;
mod process;
mod schema;
mod screenshot;
mod search;
pub mod smart_cluster;
pub mod task;
mod types;

#[allow(unused_imports)]
pub use image_io::{read_encrypted_image_as_base64, read_image_as_base64};
pub use types::*;

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
    CredentialManagerState,
};
use rusqlite::{Connection, OpenFlags};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Error returned by background-only reads of encrypted screenshot content.
/// `AuthRequired` is intentionally distinct so callers can defer work without
/// treating a locked CNG key as a processing failure or displaying system UI.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BackgroundReadError {
    #[error("authentication required")]
    AuthRequired,
    #[error("{0}")]
    Other(String),
}

impl From<String> for BackgroundReadError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

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
    hmac_migration_cancel_requested: AtomicBool,
    hmac_migration_in_progress: AtomicBool,
    lazy_indexer_shutdown: AtomicBool,
    /// Diagnostic: tracks which operation currently holds the DB mutex
    lock_holder: Mutex<&'static str>,
    /// Approximate OCR row count for O(1) IDF lookups (initialized from DB, maintained on insert/delete)
    ocr_row_count: AtomicU64,
    /// Whether dedup migration has already been performed this session
    dedup_migrated: AtomicBool,
    /// Whether bitmap index migration has already been attempted this session
    bitmap_index_migrated: AtomicBool,
    /// Whether thumbnail warmup has already completed this session
    pub(crate) thumbnail_warmup_done: AtomicBool,
    /// Whether startup VACUUM is currently running
    startup_vacuum_in_progress: AtomicBool,
}

struct NamedConnectionGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<Connection>>,
    lock_holder: &'a Mutex<&'static str>,
}

impl Deref for NamedConnectionGuard<'_> {
    type Target = Option<Connection>;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for NamedConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for NamedConnectionGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut holder) = self.lock_holder.lock() {
            *holder = "";
        }
    }
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
            hmac_migration_cancel_requested: AtomicBool::new(false),
            hmac_migration_in_progress: AtomicBool::new(false),
            lazy_indexer_shutdown: AtomicBool::new(false),
            lock_holder: Mutex::new(""),
            ocr_row_count: AtomicU64::new(0),
            dedup_migrated: AtomicBool::new(false),
            bitmap_index_migrated: AtomicBool::new(false),
            thumbnail_warmup_done: AtomicBool::new(false),
            startup_vacuum_in_progress: AtomicBool::new(false),
        }
    }

    /// Convert an absolute image path to a relative path (relative to data_dir).
    /// Uses forward slashes for consistency across platforms.
    fn to_relative_image_path(&self, abs_path: &Path) -> String {
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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
            self.data_dir
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .join(rel_path)
        }
    }

    /// Request cancellation of an ongoing migration.
    pub fn request_migration_cancel(&self) -> bool {
        self.migration_cancel_requested
            .store(true, Ordering::SeqCst);
        self.migration_in_progress.load(Ordering::SeqCst)
    }

    /// Request cancellation of an ongoing HMAC migration.
    pub fn request_hmac_migration_cancel(&self) -> bool {
        self.hmac_migration_cancel_requested
            .store(true, Ordering::SeqCst);
        self.hmac_migration_in_progress.load(Ordering::SeqCst)
    }

    pub fn is_migration_in_progress(&self) -> bool {
        self.migration_in_progress.load(Ordering::SeqCst)
    }

    pub fn is_migration_cancel_requested(&self) -> bool {
        self.migration_cancel_requested.load(Ordering::SeqCst)
    }

    pub fn is_hmac_migration_in_progress(&self) -> bool {
        self.hmac_migration_in_progress.load(Ordering::SeqCst)
    }

    pub fn is_hmac_migration_cancel_requested(&self) -> bool {
        self.hmac_migration_cancel_requested.load(Ordering::SeqCst)
    }

    pub fn is_startup_vacuum_in_progress(&self) -> bool {
        self.startup_vacuum_in_progress.load(Ordering::SeqCst)
    }

    /// Acquire DB connection with caller identification for diagnostic logging.
    fn get_connection_named(
        &self,
        caller: &'static str,
    ) -> Result<NamedConnectionGuard<'_>, String> {
        let wait_start = std::time::Instant::now();
        let current_holder = self.lock_holder.lock().ok().map(|g| *g).unwrap_or("?");
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(NamedConnectionGuard {
            guard,
            lock_holder: &self.lock_holder,
        })
    }

    /// Open an independent SQLCipher read-only connection for read-heavy paths.
    ///
    /// The database key is derived from the public key, matching initialize().
    /// This does not require the private key or an unlocked credential session.
    pub(crate) fn open_read_connection_named(
        &self,
        caller: &'static str,
    ) -> Result<Connection, String> {
        let started = std::time::Instant::now();
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let public_key = get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())?;
        let db_key = derive_db_key_from_public_key(&public_key);
        let db_path = data_dir.join("screenshots.db");
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("Failed to open read database connection: {}", e))?;
        let key_hex = hex::encode(&db_key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex))
            .map_err(|e| format!("Failed to set read database key: {}", e))?;
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|e| format!("Read database key verification failed: {}", e))?;

        let elapsed = started.elapsed();
        if elapsed.as_millis() >= 250 {
            tracing::warn!(
                "[DIAG:DB] read connection open slow caller={} elapsed={:?}",
                caller,
                elapsed
            );
        }
        Ok(conn)
    }

    /// Returns whether the current credential session is unlocked/valid.
    pub fn is_session_valid(&self) -> bool {
        self.credential_state.is_session_valid()
    }
}
