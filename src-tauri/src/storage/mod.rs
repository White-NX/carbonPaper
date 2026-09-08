//! Storage Management Module - SQLCipher Encrypted SQLite Database and File Storage
//!
//! This module provides:
//! 1. Encrypted storage of screenshots
//! 2. Screenshot metadata and OCR results
//! 3. OCR data storage and search

mod background_scheduler;
mod connection;
pub(crate) mod database_snapshot;
mod derived_index;
mod derived_migration;
mod document_ref;
mod encryption;
mod image_io;
mod link_scoring;
pub mod migration;
mod mode;
mod policy;
mod process;
mod schema;
mod screenshot;
mod search;
mod search_plan;
mod search_rank;
mod semantic_cache;
pub mod smart_cluster;
pub mod task;
mod types;
pub(crate) mod wire_time;

pub use background_scheduler::BackgroundTaskState;
#[allow(unused_imports)]
pub use derived_index::*;
#[allow(unused_imports)]
pub use derived_migration::*;
pub use document_ref::STALE_DOCUMENT_REF_GENERATION;
#[allow(unused_imports)]
pub use image_io::{read_encrypted_image_as_base64, read_image_as_base64};
pub(crate) use mode::{
    database_mode_policy_from_environment, DatabaseModeEligibility, DatabaseModeMetadata,
};
pub(crate) use policy::disk_totals_for_path;
#[allow(unused_imports)]
pub use semantic_cache::SEMANTIC_CACHE_IDLE_TTL;
pub use types::*;

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
    CredentialManagerState,
};
use rusqlite::{Connection, OpenFlags};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

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
    /// Journal mode selected once for this process. The default constructor
    /// intentionally remains DELETE for tests and all non-experimental paths.
    database_mode_policy: mode::DatabaseModePolicy,
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
    /// Serializes derived-index sidecar publication without participating in
    /// the data-directory/database lock ordering.
    derived_generation_publish_lock: Mutex<()>,
    /// Long-running query activity versus database maintenance. Queries hold a
    /// shared permit across vector selection and hydration. Maintenance takes
    /// the exclusive side first so nested independent reads can finish without
    /// deadlocking a waiting maintenance operation.
    foreground_db_activity: connection::ActivityGate,
    /// Every independent SQLCipher connection holds a shared permit for its
    /// complete lifetime. Backup, restore, VACUUM, and directory migration
    /// drain this gate before touching database state.
    independent_db_activity: connection::ActivityGate,
    /// Non-zero from the moment maintenance starts waiting. Search-order
    /// publication checks this before retaining its long-lived data-version
    /// connection, allowing maintenance to drain all independent handles.
    database_maintenance_pending: Arc<AtomicUsize>,
    /// Resident `semantic_text` vectors for the exact-scan read path. Loaded on
    /// first query, kept current by the write path, released when idle. The
    /// per-kind budget may choose the paged exact fallback instead of retaining
    /// this slot; that fallback still searches the complete durable index.
    /// Lock order: always acquired after the database mutex, never before.
    semantic_vector_cache: RwLock<Option<semantic_cache::SemanticVectorCache>>,
    /// Unix millis of the last resident-cache use; 0 when nothing is cached.
    semantic_cache_used_at: AtomicU64,
    /// The same, for `clip_image`. A separate matrix rather than a second
    /// entry in one: the two are different widths, are used by different
    /// searches, and go idle independently, so sharing an eviction clock would
    /// make one search pay for the other's silence.
    clip_vector_cache: RwLock<Option<semantic_cache::SemanticVectorCache>>,
    clip_cache_used_at: AtomicU64,
    /// Serializes cold resident-cache loads per index kind. Without this, two
    /// concurrent first queries can each materialize a full matrix before
    /// either one publishes it, defeating the cache budget at the exact point
    /// where memory is already under pressure.
    semantic_cache_load_lock: Mutex<()>,
    clip_cache_load_lock: Mutex<()>,
    /// Incremented whenever the backing database is swapped and all resident
    /// caches are reset. A scan that started against the old file must not
    /// publish its result after that boundary.
    semantic_cache_reset_generation: AtomicU64,
    /// Ordered ids of the most recent text search, so paging through one
    /// result set does not re-run recall and reranking for every page. The
    /// order itself holds no plaintext and expires on its own; see
    /// `search.rs::CachedSearchOrder`.
    search_order_cache: Mutex<Option<search::CachedSearchOrder>>,
    /// Incremented on every close and every open of the backing database file,
    /// always while the `db` mutex is held. A background writer that captured
    /// this value before being scheduled can tell whether it is still facing
    /// the same database; row ids are only meaningful within one generation,
    /// so a write that crosses a backup restore or a data-directory switch
    /// would otherwise land on an unrelated screenshot that happens to share
    /// the id. See `document_ref.rs::save_screenshot_document_ref_for_generation`.
    db_generation: AtomicU64,
}

struct NamedConnectionGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<Connection>>,
    lock_holder: &'a Mutex<&'static str>,
    caller: &'static str,
    acquired_at: std::time::Instant,
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
        let hold_duration = self.acquired_at.elapsed();
        if hold_duration.as_secs() >= 1 {
            tracing::warn!(
                "[DIAG:DB] Mutex hold took {:?} for '{}'",
                hold_duration,
                self.caller
            );
        }
        if let Ok(mut holder) = self.lock_holder.lock() {
            *holder = "";
        }
    }
}

impl StorageState {
    pub fn new(data_dir: PathBuf, credential_state: Arc<CredentialManagerState>) -> Self {
        Self::new_with_mode_policy(data_dir, credential_state, mode::DatabaseModePolicy::Delete)
    }

    pub(crate) fn new_with_mode_policy(
        data_dir: PathBuf,
        credential_state: Arc<CredentialManagerState>,
        database_mode_policy: mode::DatabaseModePolicy,
    ) -> Self {
        let screenshot_dir = data_dir.join("screenshots");

        Self {
            db: Mutex::new(None),
            data_dir: Mutex::new(data_dir),
            screenshot_dir: Mutex::new(screenshot_dir),
            credential_state,
            database_mode_policy,
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
            derived_generation_publish_lock: Mutex::new(()),
            foreground_db_activity: connection::ActivityGate::default(),
            independent_db_activity: connection::ActivityGate::default(),
            database_maintenance_pending: Arc::new(AtomicUsize::new(0)),
            semantic_vector_cache: RwLock::new(None),
            semantic_cache_used_at: AtomicU64::new(0),
            clip_vector_cache: RwLock::new(None),
            clip_cache_used_at: AtomicU64::new(0),
            semantic_cache_load_lock: Mutex::new(()),
            clip_cache_load_lock: Mutex::new(()),
            semantic_cache_reset_generation: AtomicU64::new(0),
            search_order_cache: Mutex::new(None),
            db_generation: AtomicU64::new(0),
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

    pub(crate) fn foreground_db_read(&self) -> connection::ActivityReadGuard {
        self.foreground_db_activity.read("foreground_query")
    }

    pub(crate) fn database_maintenance(
        &self,
        caller: &'static str,
    ) -> connection::DatabaseMaintenanceGuard {
        let pending = connection::MaintenanceRequestGuard::new(Arc::clone(
            &self.database_maintenance_pending,
        ));
        let foreground = self.foreground_db_activity.write(caller);
        // A cached search order owns an independent connection indefinitely.
        // Once publication is disabled above, clearing it makes the remaining
        // independent-read drain finite.
        self.clear_search_order_cache();
        let independent_reads = self.independent_db_activity.write(caller);
        connection::DatabaseMaintenanceGuard::from_parts(pending, foreground, independent_reads)
    }

    pub(crate) fn try_database_maintenance(
        &self,
        caller: &'static str,
    ) -> Option<connection::DatabaseMaintenanceGuard> {
        let pending = connection::MaintenanceRequestGuard::new(Arc::clone(
            &self.database_maintenance_pending,
        ));
        let foreground = self.foreground_db_activity.try_write(caller)?;
        self.clear_search_order_cache();
        let independent_reads = self.independent_db_activity.try_write(caller)?;
        Some(connection::DatabaseMaintenanceGuard::from_parts(
            pending,
            foreground,
            independent_reads,
        ))
    }

    pub(crate) fn is_database_maintenance_pending(&self) -> bool {
        self.database_maintenance_pending.load(Ordering::Acquire) > 0
    }

    pub(crate) fn derived_generation_publish_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.derived_generation_publish_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn database_key(&self) -> Result<Vec<u8>, String> {
        let public_key = get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())?;
        Ok(derive_db_key_from_public_key(&public_key))
    }

    /// Prepare the live primary connection for a closed, static snapshot and
    /// return its effective journal mode while the caller owns maintenance.
    pub(crate) fn prepare_database_snapshot_under_maintenance(&self) -> Result<String, String> {
        let guard = self.get_connection_named("database_journal_mode_snapshot")?;
        let connection = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let status = connection::inspect_connection(connection)
            .map_err(|error| format!("Failed to inspect database journal mode: {error}"))?;
        if status.journal_mode == "wal" {
            connection::preserve_wal_sidecars_on_close(connection)?;
        }
        Ok(status.journal_mode)
    }

    pub(crate) fn is_initialized(&self) -> bool {
        *self
            .initialized
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
        if guard.is_none() {
            return Err("Database not initialized".to_string());
        }
        // Update lock holder to current caller
        if let Ok(mut h) = self.lock_holder.lock() {
            *h = caller;
        }
        if wait_dur.as_secs() >= 10 {
            tracing::warn!(
                "[DIAG:DB] Mutex wait took {:?} for '{}' (holder at wait start: '{}')",
                wait_dur,
                caller,
                current_holder
            );
        }
        Ok(NamedConnectionGuard {
            guard,
            lock_holder: &self.lock_holder,
            caller,
            acquired_at: std::time::Instant::now(),
        })
    }

    /// Open an independent SQLCipher read-only connection for read-heavy paths.
    ///
    /// The database key is derived from the public key, matching initialize().
    /// This does not require the private key or an unlocked credential session.
    pub(crate) fn open_read_connection_named(
        &self,
        caller: &'static str,
    ) -> Result<connection::IndependentReadConnection, String> {
        let started = std::time::Instant::now();
        let activity = self.independent_db_activity.read(caller);
        if !self.is_initialized() {
            return Err("Database not initialized".to_string());
        }
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
        let status = connection::configure_sqlcipher_connection(&conn, &db_key)
            .map_err(|e| format!("Failed to configure read database connection: {}", e))?;

        let elapsed = started.elapsed();
        if elapsed.as_millis() >= 250 {
            tracing::warn!(
                "[DIAG:DB] read connection open slow caller={} elapsed={:?}",
                caller,
                elapsed
            );
        }
        tracing::debug!(
            "[DIAG:DB] read connection configured caller={} sqlite_version={} sqlite_source_id={} cipher_version={} journal_mode={} synchronous={}",
            caller,
            status.engine.sqlite_version,
            status.engine.sqlite_source_id,
            status.engine.cipher_version,
            status.journal_mode,
            status.synchronous
        );
        Ok(connection::IndependentReadConnection::new(conn, activity))
    }

    /// Returns whether the current credential session is unlocked/valid.
    pub fn is_session_valid(&self) -> bool {
        self.credential_state.is_session_valid()
    }

    /// Whether this process holds the opt-in lease for unattended protected
    /// reads. Unlike the UI session, this survives foreground locking but not
    /// application restart, preference disablement, or key-cache clearing.
    pub fn is_background_authorized(&self) -> bool {
        self.credential_state.background_authorized()
    }

    /// Silent read helpers are also used by explicit user-initiated jobs. A
    /// live UI session therefore authorizes them even when unattended work is
    /// disabled, while automatic scheduling requires the background lease.
    pub(crate) fn is_silent_read_authorized(&self) -> bool {
        self.is_session_valid() || self.is_background_authorized()
    }

    /// The identity of the currently open database file.
    ///
    /// Capture this before handing work to a background task, then pass it back
    /// to the write call. The value changes whenever the database is closed or
    /// reopened, which is exactly when previously collected row ids stop being
    /// meaningful.
    pub fn db_generation(&self) -> u64 {
        self.db_generation.load(Ordering::Acquire)
    }

    /// Records that the backing database file is being swapped.
    ///
    /// Callers must already hold the `db` mutex, so that a writer which passed
    /// the generation check under that same lock cannot be overtaken by a swap
    /// before its statement runs.
    pub(super) fn bump_db_generation(&self) {
        self.db_generation.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, TransactionBehavior};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const READ_TEST_TIMESTAMP: i64 = 1_700_000_040;
    const THREAD_TIMEOUT: Duration = Duration::from_secs(5);

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("create temporary storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        (temp, storage)
    }

    #[test]
    fn try_maintenance_fails_cleanly_when_foreground_is_active() {
        let (_temp, storage) = test_storage();
        let foreground_reader = storage
            .foreground_db_activity
            .read("test_foreground_reader");

        assert!(storage
            .try_database_maintenance("test_maintenance")
            .is_none());
        assert!(!storage.is_database_maintenance_pending());

        drop(foreground_reader);
        let maintenance = storage
            .try_database_maintenance("test_maintenance_after_reader")
            .expect("maintenance succeeds after foreground reader exits");
        assert!(storage.is_database_maintenance_pending());
        drop(maintenance);
        assert!(!storage.is_database_maintenance_pending());
    }

    #[test]
    fn try_maintenance_releases_foreground_when_independent_read_is_active() {
        let (_temp, storage) = test_storage();
        let independent_reader = storage
            .independent_db_activity
            .read("test_independent_reader");

        assert!(storage
            .try_database_maintenance("test_maintenance")
            .is_none());
        assert!(!storage.is_database_maintenance_pending());
        let foreground = storage
            .foreground_db_activity
            .try_write("test_foreground_after_failure")
            .expect("failed maintenance releases its foreground permit");
        drop(foreground);

        drop(independent_reader);
        assert!(storage
            .try_database_maintenance("test_maintenance_after_reader")
            .is_some());
    }

    #[test]
    fn independent_read_connection_cannot_open_before_storage_initializes() {
        let (_temp, storage) = test_storage();
        let error = match storage.open_read_connection_named("read_before_initialize") {
            Ok(_) => panic!("startup must publish storage before independent reads"),
            Err(error) => error,
        };
        assert_eq!(error, "Database not initialized");
    }

    fn initialized_read_storage(
        policy: mode::DatabaseModePolicy,
    ) -> (tempfile::TempDir, Arc<StorageState>) {
        let temp = tempfile::tempdir().expect("temporary encrypted storage");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        // These reads need only a SQLCipher key derived from the public bytes.
        // No test creates or unlocks the user's Windows CNG key.
        crate::credential_manager::save_public_key_to_file(&credential, b"read-test-public-key")
            .unwrap();
        let storage = Arc::new(StorageState::new_with_mode_policy(
            temp.path().to_path_buf(),
            credential,
            policy,
        ));
        storage.initialize().expect("initialize encrypted storage");
        assert_eq!(
            storage
                .database_mode_metadata()
                .unwrap()
                .actual_journal_mode,
            policy.as_str()
        );
        {
            let guard = storage.get_connection_named("seed_read_fixture").unwrap();
            let conn = guard.as_ref().unwrap();
            conn.execute_batch(
                "INSERT INTO screenshots
                    (id, image_path, image_hash, process_name, created_at, is_deleted)
                 VALUES
                    (1, 'one.enc', 'one', 'Editor', '2023-11-14 22:14:00', 0),
                    (2, 'two.enc', 'two', 'Editor', '2023-11-14 22:14:10', 0),
                    (3, 'three.enc', 'three', 'Browser', '2023-11-14 22:15:00', 0),
                    (4, 'deleted.enc', 'deleted', 'Deleted', '2023-11-14 22:14:00', 1);",
            )
            .unwrap();
            for (id, screenshot_id, x, y, deleted) in [
                (10, 1, 20.0, 5.0, 0),
                (11, 1, 0.0, 5.0, 0),
                (12, 1, 0.0, 25.0, 0),
                (13, 1, 0.0, 0.0, 1),
                (14, 2, 0.0, 0.0, 0),
            ] {
                conn.execute(
                    "INSERT INTO ocr_results
                        (id, screenshot_id, text_hash, text, confidence,
                         box_x1, box_y1, box_x2, box_y2, box_x3, box_y3, box_x4, box_y4,
                         created_at, is_deleted)
                     VALUES (?1, ?2, '', 'legacy plaintext', 0.75,
                             ?3, ?4, ?5, ?4, ?5, ?6, ?3, ?6,
                             '2023-11-14 22:14:00', ?7)",
                    params![id, screenshot_id, x, y, x + 10.0, y + 10.0, deleted],
                )
                .unwrap();
            }
        }
        (temp, storage)
    }

    fn assert_read_fixture(storage: &StorageState, browser_process: &str) {
        let ocr = storage.get_screenshot_ocr_results(1).expect("OCR detail");
        assert_eq!(
            ocr.iter().map(|row| row.id).collect::<Vec<_>>(),
            [11, 10, 12]
        );
        assert!(ocr
            .iter()
            .all(|row| row.screenshot_id == 1 && row.text.is_empty()));
        assert_eq!(ocr[0].confidence, 0.75);
        assert_eq!(
            ocr[0].box_coords,
            vec![
                vec![0.0, 5.0],
                vec![10.0, 5.0],
                vec![10.0, 15.0],
                vec![0.0, 15.0]
            ]
        );
        assert_eq!(
            ocr[0].created_at,
            wire_time::from_sqlite_utc("2023-11-14 22:14:00")
        );

        let density = storage
            .get_screenshot_density(
                READ_TEST_TIMESTAMP as f64,
                (READ_TEST_TIMESTAMP + 120) as f64,
                60,
                0,
            )
            .expect("timeline density");
        assert_eq!(
            density
                .iter()
                .map(|bucket| (bucket.timestamp, bucket.count))
                .collect::<Vec<_>>(),
            [(READ_TEST_TIMESTAMP, 2), (READ_TEST_TIMESTAMP + 60, 1)]
        );
        assert_eq!(
            storage
                .list_distinct_processes()
                .expect("process statistics"),
            vec![("Editor".to_string(), 2), (browser_process.to_string(), 1)]
        );
    }

    #[test]
    fn ui_reads_finish_while_primary_writer_is_held() {
        for policy in [
            mode::DatabaseModePolicy::Wal,
            mode::DatabaseModePolicy::Delete,
        ] {
            let (_temp, storage) = initialized_read_storage(policy);
            let mut primary = storage.get_connection_named("held_primary_writer").unwrap();
            let tx = primary
                .as_mut()
                .unwrap()
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute(
                "UPDATE screenshots SET process_name = 'Updated' WHERE id = 3",
                [],
            )
            .unwrap();

            let reader_storage = Arc::clone(&storage);
            let (finished_tx, finished_rx) = mpsc::channel();
            let reader = thread::spawn(move || {
                // All three application entry points must finish before the
                // writer releases its mutex, and see only committed records.
                assert_read_fixture(&reader_storage, "Browser");
                finished_tx.send(()).unwrap();
            });
            let finished = finished_rx.recv_timeout(THREAD_TIMEOUT);
            let write_result = if finished.is_ok() {
                tx.commit()
            } else {
                tx.rollback()
            };
            // Release the writer even on failure so a regressed reader can exit.
            drop(primary);
            reader.join().expect("read worker exits");
            finished.expect("UI reads must not wait for the primary database mutex");
            write_result.expect("writer commits after concurrent reads");
            assert_read_fixture(&storage, "Updated");
        }
    }

    #[test]
    fn shutdown_drains_independent_reads_and_reopens_storage() {
        for policy in [
            mode::DatabaseModePolicy::Wal,
            mode::DatabaseModePolicy::Delete,
        ] {
            let (_temp, storage) = initialized_read_storage(policy);
            assert_read_fixture(&storage, "Browser");
            drop(
                storage
                    .try_database_maintenance("after_ui_reads")
                    .expect("UI reads release permits"),
            );
            let generation = storage.db_generation();
            let reader = storage
                .open_read_connection_named("held_read_transaction")
                .unwrap();
            reader
                .execute_batch("BEGIN; SELECT id FROM screenshots LIMIT 1;")
                .unwrap();
            assert!(storage
                .try_database_maintenance("held_reader_check")
                .is_none());

            let shutdown_storage = Arc::clone(&storage);
            let (finished_tx, finished_rx) = mpsc::channel();
            let shutdown = thread::spawn(move || {
                finished_tx.send(shutdown_storage.shutdown()).unwrap();
            });
            let deadline = Instant::now() + THREAD_TIMEOUT;
            while !storage.is_database_maintenance_pending()
                && !shutdown.is_finished()
                && Instant::now() < deadline
            {
                thread::yield_now();
            }
            let pending = storage.is_database_maintenance_pending();
            let early_completion = finished_rx.try_recv().ok();
            let closed_early = early_completion.is_some();
            drop(reader);
            let result = early_completion.unwrap_or_else(|| {
                finished_rx
                    .recv_timeout(THREAD_TIMEOUT)
                    .expect("shutdown finishes after reader closes")
            });
            shutdown.join().expect("shutdown worker exits");
            result.expect("storage shutdown succeeds");
            assert!(pending, "shutdown must wait for independent readers");
            assert!(
                !closed_early,
                "shutdown must not overtake a live read connection"
            );
            assert!(!storage.is_initialized());

            storage.initialize().expect("reopen after maintenance");
            assert!(storage.db_generation() > generation);
            assert_read_fixture(&storage, "Browser");
            let committed = storage
                .commit_screenshot(1, None, Some("work"), None)
                .unwrap();
            assert_eq!(committed.screenshot_id, Some(1));
            {
                let read = storage
                    .open_read_connection_named("verify_reopened_write")
                    .unwrap();
                let record: (String, String) = read
                    .query_row(
                        "SELECT status, category FROM screenshots WHERE id = 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(record, ("committed".to_string(), "work".to_string()));
            }
            storage.shutdown().expect("subsequent shutdown succeeds");
        }
    }
}
