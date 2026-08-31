//! Shared SQLCipher connection setup and journal-mode diagnostics.

use rusqlite::{Connection, OptionalExtension, Result};
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Keep lock waits bounded and explicit on every connection.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Preserve the existing durability policy while DELETE remains the default
/// journal mode. WAL experiments can choose a different policy explicitly.
const SYNCHRONOUS_FULL: &str = "FULL";

/// Minimum SQLite version carried by the SQLCipher bundle for this release.
/// SQLite encodes versions as major * 1_000_000 + minor * 1_000 + patch.
pub(crate) const MIN_SQLITE_VERSION: &str = "3.51.3";
pub(crate) const MIN_SQLITE_VERSION_NUMBER: i64 = 3_051_003;
#[cfg(test)]
pub(crate) const BUNDLED_SQLCIPHER_VERSION: &str = "4.14.0";
#[cfg(test)]
pub(crate) const BUNDLED_SQLITE_SOURCE_ID: &str =
    "2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6alt1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqliteEngineIdentity {
    pub(crate) sqlite_version: String,
    pub(crate) sqlite_source_id: String,
    pub(crate) cipher_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionStatus {
    pub(crate) journal_mode: String,
    pub(crate) synchronous: String,
    pub(crate) engine: SqliteEngineIdentity,
}

#[derive(Default)]
struct ActivityState {
    active_readers: usize,
    active_writer: bool,
    waiting_writers: usize,
}

#[derive(Default)]
struct ActivityGateInner {
    state: Mutex<ActivityState>,
    changed: Condvar,
}

/// A blocking, writer-preferred activity gate.
///
/// The gate deliberately owns its state through an `Arc` rather than exposing
/// a standard-library lock guard. Maintenance commands can therefore hold the
/// exclusive permit while awaiting monitor shutdown or performing filesystem
/// work without borrowing a `StorageState` or keeping a `MutexGuard` alive.
#[derive(Clone, Default)]
pub(crate) struct ActivityGate {
    inner: Arc<ActivityGateInner>,
}

impl ActivityGate {
    pub(crate) fn read(&self, caller: &'static str) -> ActivityReadGuard {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while state.active_writer || state.waiting_writers > 0 {
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        state.active_readers += 1;
        drop(state);

        ActivityReadGuard {
            inner: Arc::clone(&self.inner),
            caller,
            acquired_at: Instant::now(),
        }
    }

    pub(crate) fn write(&self, caller: &'static str) -> ActivityWriteGuard {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.waiting_writers += 1;
        while state.active_writer || state.active_readers > 0 {
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        state.waiting_writers -= 1;
        state.active_writer = true;
        drop(state);

        ActivityWriteGuard {
            inner: Arc::clone(&self.inner),
            caller,
            acquired_at: Instant::now(),
        }
    }

    pub(crate) fn try_write(&self, caller: &'static str) -> Option<ActivityWriteGuard> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.active_writer || state.active_readers > 0 || state.waiting_writers > 0 {
            return None;
        }
        state.active_writer = true;
        drop(state);

        Some(ActivityWriteGuard {
            inner: Arc::clone(&self.inner),
            caller,
            acquired_at: Instant::now(),
        })
    }
}

pub(crate) struct ActivityReadGuard {
    inner: Arc<ActivityGateInner>,
    caller: &'static str,
    acquired_at: Instant,
}

impl Drop for ActivityReadGuard {
    fn drop(&mut self) {
        let held_for = self.acquired_at.elapsed();
        if held_for.as_secs() >= 1 {
            tracing::debug!(
                "[DIAG:DB] activity read held for {:?} by '{}'",
                held_for,
                self.caller
            );
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active_readers = state.active_readers.saturating_sub(1);
        self.inner.changed.notify_all();
    }
}

pub(crate) struct ActivityWriteGuard {
    inner: Arc<ActivityGateInner>,
    caller: &'static str,
    acquired_at: Instant,
}

pub(crate) struct MaintenanceRequestGuard {
    pending: Arc<AtomicUsize>,
}

impl MaintenanceRequestGuard {
    pub(crate) fn new(pending: Arc<AtomicUsize>) -> Self {
        pending.fetch_add(1, Ordering::AcqRel);
        Self { pending }
    }
}

impl Drop for MaintenanceRequestGuard {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for ActivityWriteGuard {
    fn drop(&mut self) {
        let held_for = self.acquired_at.elapsed();
        if held_for.as_secs() >= 1 {
            tracing::debug!(
                "[DIAG:DB] activity maintenance held for {:?} by '{}'",
                held_for,
                self.caller
            );
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active_writer = false;
        self.inner.changed.notify_all();
    }
}

/// Exclusive permit for operations that may close, replace, or compact the
/// database. The long-query gate is acquired first, then the independent-read
/// gate; all callers must preserve this order.
pub(crate) struct DatabaseMaintenanceGuard {
    _independent_reads: ActivityWriteGuard,
    _foreground: ActivityWriteGuard,
    _pending: MaintenanceRequestGuard,
}

impl DatabaseMaintenanceGuard {
    pub(crate) fn from_parts(
        pending: MaintenanceRequestGuard,
        foreground: ActivityWriteGuard,
        independent_reads: ActivityWriteGuard,
    ) -> Self {
        Self {
            _independent_reads: independent_reads,
            _foreground: foreground,
            _pending: pending,
        }
    }
}

/// Independent SQLCipher read connection with an activity permit tied to its
/// lifetime. The connection is dropped before the permit, so maintenance can
/// only proceed after SQLite has released the read handle.
pub(crate) struct IndependentReadConnection {
    conn: Option<Connection>,
    _activity: ActivityReadGuard,
}

impl IndependentReadConnection {
    pub(crate) fn new(conn: Connection, activity: ActivityReadGuard) -> Self {
        Self {
            conn: Some(conn),
            _activity: activity,
        }
    }
}

impl Deref for IndependentReadConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("independent connection is present until drop")
    }
}

impl Drop for IndependentReadConnection {
    fn drop(&mut self) {
        // Close SQLite before `_activity` is released. Maintenance that wakes
        // on the permit must never race a still-open Windows file handle.
        drop(self.conn.take());
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalMode {
    Delete,
    Wal,
}

impl JournalMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Wal => "wal",
        }
    }
}

/// Configure an encrypted connection in the required SQLCipher order.
///
/// The key pragma is deliberately the first SQLite statement. The busy timeout
/// follows immediately so key verification also waits for transient locks;
/// settings that inspect or change database state are applied only after key
/// verification succeeds.
pub(crate) fn configure_sqlcipher_connection(
    conn: &Connection,
    db_key: &[u8],
) -> Result<ConnectionStatus> {
    let key_hex = hex::encode(db_key);
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;

    // Verify the key before reading or changing any other database state.
    conn.execute_batch("SELECT count(*) FROM sqlite_master;")?;
    let status = configure_connection_pragmas(conn)?;
    ensure_supported_sqlite_engine(&status.engine, true)?;
    Ok(status)
}

/// Apply the connection-local policy shared by primary and independent reads.
pub(crate) fn configure_connection_pragmas(conn: &Connection) -> Result<ConnectionStatus> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "synchronous", SYNCHRONOUS_FULL)?;
    inspect_connection(conn)
}

/// Read the effective connection state without changing the journal mode.
pub(crate) fn inspect_connection(conn: &Connection) -> Result<ConnectionStatus> {
    let engine = inspect_sqlite_engine(conn)?;
    ensure_supported_sqlite_engine(&engine, false)?;

    let journal_mode =
        conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?;
    let synchronous = conn.pragma_query_value(None, "synchronous", |row| {
        let value: i64 = row.get(0)?;
        let label = match value {
            0 => "OFF",
            1 => "NORMAL",
            2 => "FULL",
            3 => "EXTRA",
            _ => "UNKNOWN",
        };
        Ok(label.to_string())
    })?;

    Ok(ConnectionStatus {
        journal_mode: journal_mode.trim().to_ascii_lowercase(),
        synchronous,
        engine,
    })
}

/// Query the SQLite and SQLCipher identities from the linked runtime.
///
/// These values are collected at runtime instead of inferred from the Rust
/// dependency graph so a release log can prove which native library was
/// actually linked and opened the database.
pub(crate) fn inspect_sqlite_engine(conn: &Connection) -> Result<SqliteEngineIdentity> {
    let (sqlite_version, sqlite_source_id): (String, String) =
        conn.query_row("SELECT sqlite_version(), sqlite_source_id()", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let cipher_version = conn
        .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .optional()?
        .unwrap_or_default();

    Ok(SqliteEngineIdentity {
        sqlite_version,
        sqlite_source_id,
        cipher_version,
    })
}

fn ensure_supported_sqlite_engine(
    engine: &SqliteEngineIdentity,
    require_sqlcipher: bool,
) -> Result<()> {
    let version_number = parse_sqlite_version_number(&engine.sqlite_version).ok_or_else(|| {
        engine_compatibility_error(format!(
            "SQLCipher reported an invalid SQLite version '{}'; minimum supported version is {}",
            engine.sqlite_version, MIN_SQLITE_VERSION
        ))
    })?;

    if version_number < MIN_SQLITE_VERSION_NUMBER {
        return Err(engine_compatibility_error(format!(
            "SQLCipher SQLite version {} is below the required baseline {}; source_id='{}', cipher_version='{}'",
            engine.sqlite_version,
            MIN_SQLITE_VERSION,
            engine.sqlite_source_id,
            engine.cipher_version
        )));
    }
    if engine.sqlite_source_id.trim().is_empty() {
        return Err(engine_compatibility_error(
            "SQLCipher returned an empty SQLite source id".to_string(),
        ));
    }
    if require_sqlcipher && engine.cipher_version.trim().is_empty() {
        return Err(engine_compatibility_error(
            "The linked SQLite runtime does not expose SQLCipher cipher_version".to_string(),
        ));
    }

    Ok(())
}

fn parse_sqlite_version_number(version: &str) -> Option<i64> {
    let mut components = version.split('.');
    let major = components.next()?.parse::<i64>().ok()?;
    let minor = components.next()?.parse::<i64>().ok()?;
    let patch = components.next()?.parse::<i64>().ok()?;
    if components.next().is_some() || major < 0 || minor < 0 || patch < 0 {
        return None;
    }
    Some(major * 1_000_000 + minor * 1_000 + patch)
}

fn engine_compatibility_error(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(message),
    )
}

/// Keep WAL and shared-memory sidecars after the final connection closes.
///
/// SQLite normally checkpoints and removes both files when the last WAL
/// connection exits. Backup and data-directory migration need the closed,
/// static file group to retain those members long enough to copy and hash
/// them, so they enable this per-file-handle control immediately before
/// shutting down the primary connection.
pub(crate) fn preserve_wal_sidecars_on_close(conn: &Connection) -> Result<(), String> {
    let mut enabled = 1i32;
    let result = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            b"main\0".as_ptr().cast(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut enabled as *mut i32).cast(),
        )
    };
    if result == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(format!(
            "Failed to preserve WAL sidecars for snapshot: SQLite error {result} ({})",
            rusqlite::ffi::code_to_str(result)
        ))
    }
}

/// Request a journal mode and verify SQLite's effective result.
///
/// Startup and controlled mode transitions use this helper and receive a clear
/// diagnostic when a read-only connection, filesystem, or SQLite build cannot
/// establish the requested mode.
pub(crate) fn set_journal_mode(
    conn: &Connection,
    requested: JournalMode,
) -> Result<ConnectionStatus, String> {
    let requested = requested.as_str();

    let actual = conn
        .pragma_update_and_check(None, "journal_mode", requested, |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| {
            format!(
                "Failed to set SQLite journal mode to '{}': {}",
                requested, error
            )
        })?
        .trim()
        .to_ascii_lowercase();

    if actual != requested {
        return Err(format!(
            "SQLite did not establish requested journal mode '{}'; effective mode is '{}'",
            requested, actual
        ));
    }

    inspect_connection(conn).map_err(|error| {
        format!(
            "Failed to inspect SQLite connection after setting journal mode: {}",
            error
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, OpenFlags};
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn shared_pragmas_are_explicit_and_reported() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        let status = configure_connection_pragmas(&conn).expect("configure connection");

        assert_eq!(status.journal_mode, "memory");
        assert_eq!(status.synchronous, "FULL");
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("read foreign_keys");
        assert_eq!(foreign_keys, 1);
        let busy_timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("read busy_timeout");
        assert_eq!(busy_timeout, BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn bundled_sqlcipher_reports_supported_runtime_identity() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        let engine = inspect_sqlite_engine(&conn).expect("inspect SQLite engine");

        assert_eq!(engine.sqlite_version, MIN_SQLITE_VERSION);
        assert_eq!(engine.sqlite_source_id, BUNDLED_SQLITE_SOURCE_ID);
        assert!(
            engine
                .cipher_version
                .split_whitespace()
                .next()
                .is_some_and(|version| version == BUNDLED_SQLCIPHER_VERSION),
            "unexpected SQLCipher runtime version: {}",
            engine.cipher_version
        );

        let status = configure_connection_pragmas(&conn).expect("configure connection");
        assert_eq!(status.engine, engine);
    }

    #[test]
    fn sqlite_baseline_guard_rejects_older_runtime() {
        let engine = SqliteEngineIdentity {
            sqlite_version: "3.45.3".into(),
            sqlite_source_id: "fixture-source".into(),
            cipher_version: format!("{} community", BUNDLED_SQLCIPHER_VERSION),
        };

        let error = ensure_supported_sqlite_engine(&engine, true)
            .expect_err("an older SQLite runtime must be rejected");
        assert!(error.to_string().contains(MIN_SQLITE_VERSION));
    }

    #[test]
    fn configuration_preserves_existing_journal_mode() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("journal-mode.db");
        let conn = Connection::open(path).expect("open database");
        let established: String = conn
            .pragma_update_and_check(None, "journal_mode", "TRUNCATE", |row| row.get(0))
            .expect("set fixture journal mode");
        assert_eq!(established.to_ascii_lowercase(), "truncate");

        let status = configure_connection_pragmas(&conn).expect("configure connection");
        assert_eq!(status.journal_mode, "truncate");
    }

    #[test]
    fn journal_mode_requests_verify_sqlite_result() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("requested-journal-mode.db");
        let conn = Connection::open(path).expect("open database");

        let status = set_journal_mode(&conn, JournalMode::Wal).expect("enable WAL for the test");
        assert_eq!(status.journal_mode, "wal");

        let status = set_journal_mode(&conn, JournalMode::Delete).expect("restore DELETE");
        assert_eq!(status.journal_mode, "delete");
    }

    #[test]
    fn persistent_wal_control_keeps_sidecars_after_last_connection_closes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("persistent-wal.db");
        let conn = Connection::open(&path).expect("open database");
        conn.execute_batch("CREATE TABLE records(value TEXT);")
            .expect("create table");
        conn.pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable autocheckpoint");
        set_journal_mode(&conn, JournalMode::Wal).expect("enable WAL");
        conn.execute("INSERT INTO records VALUES ('committed')", [])
            .expect("insert");
        assert!(path.with_extension("db-wal").exists());
        assert!(path.with_extension("db-shm").exists());

        preserve_wal_sidecars_on_close(&conn).expect("preserve sidecars");
        drop(conn);

        assert!(path.with_file_name("persistent-wal.db-wal").exists());
        assert!(path.with_file_name("persistent-wal.db-shm").exists());
    }

    #[test]
    fn encrypted_database_reopens_with_shared_read_settings() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("encrypted.db");
        let key = [0x5a; 32];

        {
            let conn = Connection::open(&path).expect("open encrypted database");
            let status = configure_sqlcipher_connection(&conn, &key)
                .expect("configure primary encrypted connection");
            assert_eq!(status.journal_mode, "delete");
            conn.execute_batch(
                "CREATE TABLE sample (value TEXT NOT NULL); INSERT INTO sample VALUES ('ready');",
            )
            .expect("write encrypted fixture");
        }

        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("reopen encrypted database read-only");
        let status = configure_sqlcipher_connection(&conn, &key)
            .expect("configure read-only encrypted connection");
        assert_eq!(status.journal_mode, "delete");
        assert_eq!(status.synchronous, "FULL");
        let value: String = conn
            .query_row("SELECT value FROM sample", [], |row| row.get(0))
            .expect("read encrypted fixture");
        assert_eq!(value, "ready");
    }

    #[test]
    fn encrypted_database_rejects_an_incorrect_key() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("wrong-key.db");
        let key = [0x3a; 32];

        {
            let conn = Connection::open(&path).expect("open encrypted database");
            configure_sqlcipher_connection(&conn, &key).expect("configure database");
            conn.execute_batch("CREATE TABLE sample (value TEXT NOT NULL);")
                .expect("create encrypted fixture");
        }

        let conn = Connection::open(&path).expect("reopen encrypted database");
        let error = configure_sqlcipher_connection(&conn, &[0x3b; 32])
            .expect_err("an incorrect key must fail before pragmas are applied");
        assert!(!error.to_string().trim().is_empty());
    }

    #[test]
    fn key_verification_waits_for_transient_database_lock() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("locked-encrypted.db");
        let key = [0x7b; 32];

        let writer = Connection::open(&path).expect("open encrypted database");
        configure_sqlcipher_connection(&writer, &key).expect("configure writer connection");
        writer
            .execute_batch("CREATE TABLE sample (value TEXT); BEGIN EXCLUSIVE;")
            .expect("hold exclusive database lock");

        let reader = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open read-only database");
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let outcome = configure_sqlcipher_connection(&reader, &key)
                .map(|status| status.journal_mode)
                .map_err(|error| error.to_string());
            finished_tx.send(outcome).unwrap();
        });

        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader starts configuration");
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "key verification should wait instead of failing immediately"
        );

        writer
            .execute_batch("COMMIT;")
            .expect("release database lock");
        let journal_mode = finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader finishes after lock release")
            .expect("reader configuration succeeds");
        assert_eq!(journal_mode, "delete");
        worker.join().unwrap();
    }

    #[test]
    fn maintenance_waits_for_readers_and_blocks_new_readers() {
        let gate = ActivityGate::default();
        let held = gate.read("test_reader");
        let (writer_started_tx, writer_started_rx) = mpsc::channel();
        let (writer_release_tx, writer_release_rx) = mpsc::channel();
        let writer_gate = gate.clone();
        let writer = thread::spawn(move || {
            let _writer = writer_gate.write("test_writer");
            writer_started_tx.send(()).unwrap();
            writer_release_rx.recv().unwrap();
        });

        assert!(writer_started_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());
        drop(held);
        writer_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer acquires after reader release");

        let (reader_started_tx, reader_started_rx) = mpsc::channel();
        let (reader_release_tx, reader_release_rx) = mpsc::channel();
        let reader_gate = gate.clone();
        let reader = thread::spawn(move || {
            let _reader = reader_gate.read("test_blocked_reader");
            reader_started_tx.send(()).unwrap();
            reader_release_rx.recv().unwrap();
        });
        assert!(reader_started_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());

        writer_release_tx.send(()).unwrap();
        reader_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader resumes after maintenance");
        reader_release_tx.send(()).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn maintenance_guard_drains_both_activity_domains() {
        let foreground = ActivityGate::default();
        let independent = ActivityGate::default();
        let _foreground_reader = foreground.read("test_foreground_reader");
        let _independent_reader = independent.read("test_independent_reader");
        let foreground_for_writer = foreground.clone();
        let independent_for_writer = independent.clone();
        let (foreground_acquired_tx, foreground_acquired_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let (maintenance_acquired_tx, maintenance_acquired_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let pending = MaintenanceRequestGuard::new(Arc::new(AtomicUsize::new(0)));
            let foreground_guard = foreground_for_writer.write("test_maintenance");
            foreground_acquired_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            let independent_guard = independent_for_writer.write("test_maintenance");
            let _guard =
                DatabaseMaintenanceGuard::from_parts(pending, foreground_guard, independent_guard);
            maintenance_acquired_tx.send(()).unwrap();
        });

        assert!(foreground_acquired_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());
        drop(_foreground_reader);
        foreground_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("maintenance acquires foreground domain");
        continue_tx.send(()).unwrap();
        assert!(maintenance_acquired_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());
        drop(_independent_reader);
        maintenance_acquired_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("maintenance acquires after both readers drain");
        worker.join().unwrap();
    }

    #[test]
    fn independent_connection_drops_before_activity_permit() {
        let gate = ActivityGate::default();
        let conn = Connection::open_in_memory().expect("open database");
        let reader = IndependentReadConnection::new(conn, gate.read("test_connection"));
        assert_eq!(reader.is_autocommit(), true);
        drop(reader);
        assert!(gate.try_write("test_maintenance").is_some());
    }

    #[test]
    fn read_only_journal_mode_failure_names_request_and_sqlite_error() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let path = temp.path().join("read-only-journal-mode.db");
        {
            let conn = Connection::open(&path).expect("create database");
            conn.execute_batch("CREATE TABLE sample (value TEXT);")
                .expect("create fixture");
        }

        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open read-only database");
        let error = set_journal_mode(&conn, JournalMode::Wal).expect_err("WAL must be rejected");
        assert!(
            error.contains("wal"),
            "diagnostic should name request: {error}"
        );
        assert!(
            error.to_ascii_lowercase().contains("readonly")
                || error.to_ascii_lowercase().contains("read-only")
                || error.contains("effective mode is"),
            "diagnostic should preserve SQLite failure context: {error}"
        );
    }
}
