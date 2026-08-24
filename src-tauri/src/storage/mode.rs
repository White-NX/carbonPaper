//! Database journal-mode metadata, eligibility checks, and controlled WAL
//! transition support.

use super::connection::{self, JournalMode};
use super::policy::disk_totals_for_path;
use super::StorageState;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MODE_TABLE: &str = "database_mode_metadata";
const TRANSITION_STABLE: &str = "stable";
const TRANSITIONING: &str = "transitioning";
const TRANSITION_FAILED: &str = "failed";
const SAFETY_FREE_BYTES: u64 = 64 * 1024 * 1024;
const SQLITE_JOURNAL_MODES: [&str; 6] = ["delete", "truncate", "persist", "memory", "wal", "off"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DatabaseModeMetadata {
    pub actual_journal_mode: String,
    pub requested_journal_mode: String,
    pub transition_state: String,
    pub transition_id: Option<String>,
    pub previous_journal_mode: Option<String>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DatabaseModeEligibility {
    pub current_journal_mode: String,
    pub target_journal_mode: String,
    pub writable_filesystem: bool,
    pub local_filesystem: bool,
    pub available_free_bytes: Option<u64>,
    pub required_free_bytes: u64,
    pub can_transition: bool,
    pub reasons: Vec<String>,
}

fn now_text() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn transition_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos:x}", std::process::id())
}

fn normalize_mode(mode: &str) -> Result<String, String> {
    let normalized = mode.trim().to_ascii_lowercase();
    if SQLITE_JOURNAL_MODES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(format!("Unsupported SQLite journal mode: {normalized}"))
    }
}

#[cfg(windows)]
fn is_local_filesystem(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};

    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let wide = resolved
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut volume_path = [0u16; 512];
    unsafe {
        if GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut volume_path).is_err() {
            return false;
        }
        // DRIVE_REMOVABLE, DRIVE_FIXED, and DRIVE_RAMDISK are local. In
        // particular, DRIVE_REMOTE rejects mapped network drives that do not
        // have a UNC spelling.
        matches!(GetDriveTypeW(PCWSTR(volume_path.as_ptr())), 2 | 3 | 6)
    }
}

#[cfg(not(windows))]
fn is_local_filesystem(path: &Path) -> bool {
    !path.to_string_lossy().starts_with("//")
}

fn writable_probe(directory: &Path) -> Result<(), String> {
    let mut probe_parent = directory.to_path_buf();
    while !probe_parent.exists() {
        let Some(parent) = probe_parent.parent() else {
            break;
        };
        probe_parent = parent.to_path_buf();
    }
    if !probe_parent.is_dir() {
        return Err(format!(
            "No existing directory to probe for write access: {}",
            probe_parent.display()
        ));
    }
    let name = format!(
        ".carbonpaper-write-probe-{}-{}",
        std::process::id(),
        transition_id()
    );
    let path = probe_parent.join(name);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("Filesystem is not writable: {error}"))?;
        file.write_all(b"carbonpaper")
            .map_err(|error| format!("Filesystem write probe failed: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("Filesystem sync probe failed: {error}"))
    })();
    let _ = fs::remove_file(&path);
    result
}

fn database_group_size(directory: &Path) -> u64 {
    ["screenshots.db", "screenshots.db-wal", "screenshots.db-shm"]
        .iter()
        .filter_map(|name| fs::metadata(directory.join(name)).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn evaluate_eligibility(
    current_mode: &str,
    writable_result: Result<(), String>,
    local_filesystem: bool,
    available_free_bytes: Option<u64>,
    required_free_bytes: u64,
) -> DatabaseModeEligibility {
    let writable = writable_result.is_ok();
    let current = current_mode.trim().to_ascii_lowercase();
    let free_ok = available_free_bytes
        .map(|free| free >= required_free_bytes)
        .unwrap_or(false);
    let mut reasons = Vec::new();
    if let Err(error) = writable_result {
        reasons.push(error);
    }
    if !local_filesystem {
        reasons.push("Database directory is on a non-local filesystem".to_string());
    }
    if available_free_bytes.is_none() {
        reasons.push("Unable to determine free space on the database volume".to_string());
    } else if !free_ok {
        reasons.push(format!(
            "Insufficient free space: required {required_free_bytes} bytes"
        ));
    }
    let can_transition =
        current == "delete" || (current == "wal" && writable && local_filesystem && free_ok);
    if current == "delete" {
        reasons.push("Database already uses DELETE journal mode".to_string());
    } else if current != "wal" {
        reasons.push(format!(
            "Controlled transition only supports WAL, got {current}"
        ));
    }
    DatabaseModeEligibility {
        current_journal_mode: current,
        target_journal_mode: "delete".to_string(),
        writable_filesystem: writable,
        local_filesystem,
        available_free_bytes,
        required_free_bytes,
        can_transition,
        reasons,
    }
}

fn read_metadata(conn: &Connection) -> Result<DatabaseModeMetadata, String> {
    conn.query_row(
        &format!(
            "SELECT actual_journal_mode, requested_journal_mode, transition_state,
                    transition_id, previous_journal_mode, last_error, started_at,
                    completed_at, updated_at FROM {MODE_TABLE} WHERE id = 1"
        ),
        [],
        |row| {
            Ok(DatabaseModeMetadata {
                actual_journal_mode: row.get(0)?,
                requested_journal_mode: row.get(1)?,
                transition_state: row.get(2)?,
                transition_id: row.get(3)?,
                previous_journal_mode: row.get(4)?,
                last_error: row.get(5)?,
                started_at: row.get(6)?,
                completed_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        },
    )
    .map_err(|error| format!("Failed to read database mode metadata: {error}"))
}

fn write_metadata(
    conn: &Connection,
    actual: &str,
    requested: &str,
    state: &str,
    id: Option<&str>,
    previous: Option<&str>,
    last_error: Option<&str>,
    started: Option<&str>,
    completed: Option<&str>,
) -> Result<(), String> {
    let actual = normalize_mode(actual)?;
    let requested = normalize_mode(requested)?;
    conn.execute(
        &format!(
            "INSERT INTO {MODE_TABLE}
             (id, actual_journal_mode, requested_journal_mode, transition_state,
              transition_id, previous_journal_mode, last_error, started_at,
              completed_at, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET
               actual_journal_mode = excluded.actual_journal_mode,
               requested_journal_mode = excluded.requested_journal_mode,
               transition_state = excluded.transition_state,
               transition_id = excluded.transition_id,
               previous_journal_mode = excluded.previous_journal_mode,
               last_error = excluded.last_error,
               started_at = excluded.started_at,
               completed_at = excluded.completed_at,
               updated_at = CURRENT_TIMESTAMP"
        ),
        params![actual, requested, state, id, previous, last_error, started, completed],
    )
    .map_err(|error| format!("Failed to write database mode metadata: {error}"))?;
    Ok(())
}

impl StorageState {
    pub(crate) fn initialize_database_mode_metadata(
        &self,
        conn: &Connection,
        actual_mode: &str,
    ) -> Result<(), String> {
        let actual_mode = normalize_mode(actual_mode)?;
        let existing = conn
            .query_row(
                &format!("SELECT 1 FROM {MODE_TABLE} WHERE id = 1"),
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if existing {
            let metadata = read_metadata(conn)?;
            // Startup records what SQLite actually opened. An interrupted
            // request whose target mode is already active is complete; one
            // that did not reach its target remains diagnosable as failed.
            let interrupted = metadata.transition_state == TRANSITIONING;
            let reached_interrupted_target =
                interrupted && metadata.requested_journal_mode == actual_mode;
            let stable_drift = metadata.transition_state == TRANSITION_STABLE
                && metadata.requested_journal_mode != actual_mode;
            let state = if reached_interrupted_target {
                TRANSITION_STABLE
            } else if interrupted || stable_drift {
                TRANSITION_FAILED
            } else {
                metadata.transition_state.as_str()
            };
            let last_error = if reached_interrupted_target {
                None
            } else if interrupted && metadata.last_error.is_none() {
                Some("Database mode transition was interrupted before startup".to_string())
            } else if stable_drift && metadata.last_error.is_none() {
                Some(format!(
                    "Database journal mode changed outside the controlled transition: requested {}, actual {}",
                    metadata.requested_journal_mode, actual_mode
                ))
            } else {
                metadata.last_error.clone()
            };
            let completed_at = if reached_interrupted_target {
                Some(now_text())
            } else {
                metadata.completed_at.clone()
            };
            let requested_mode = normalize_mode(&metadata.requested_journal_mode)
                .unwrap_or_else(|_| actual_mode.clone());
            return write_metadata(
                conn,
                &actual_mode,
                &requested_mode,
                state,
                if reached_interrupted_target {
                    None
                } else {
                    metadata.transition_id.as_deref()
                },
                metadata.previous_journal_mode.as_deref(),
                last_error.as_deref(),
                metadata.started_at.as_deref(),
                completed_at.as_deref(),
            );
        }
        write_metadata(
            conn,
            &actual_mode,
            &actual_mode,
            TRANSITION_STABLE,
            None,
            None,
            None,
            None,
            Some(&now_text()),
        )
    }

    pub(crate) fn database_mode_metadata(&self) -> Result<DatabaseModeMetadata, String> {
        let guard = self.get_connection_named("database_mode_metadata")?;
        read_metadata(
            guard
                .as_ref()
                .ok_or_else(|| "Database not initialized".to_string())?,
        )
    }

    fn database_mode_eligibility_under_maintenance(
        &self,
        current_mode: &str,
    ) -> DatabaseModeEligibility {
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let writable_result = writable_probe(&data_dir);
        let local = is_local_filesystem(&data_dir);
        let required = database_group_size(&data_dir)
            .saturating_mul(2)
            .saturating_add(SAFETY_FREE_BYTES);
        let available = disk_totals_for_path(&data_dir).map(|(_, free)| free);
        evaluate_eligibility(current_mode, writable_result, local, available, required)
    }

    pub(crate) fn check_database_mode_eligibility(
        &self,
    ) -> Result<DatabaseModeEligibility, String> {
        let _maintenance = self.database_maintenance("database_mode_eligibility");
        let guard = self.get_connection_named("database_mode_eligibility")?;
        let connection = guard
            .as_ref()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let status = connection::inspect_connection(connection)
            .map_err(|error| format!("Failed to inspect database journal mode: {error}"))?;
        Ok(self.database_mode_eligibility_under_maintenance(&status.journal_mode))
    }

    pub(crate) fn transition_wal_to_delete(&self) -> Result<DatabaseModeMetadata, String> {
        let _maintenance = self.database_maintenance("database_mode_wal_to_delete");
        let mut guard = self.get_connection_named("database_mode_wal_to_delete")?;
        let connection = guard
            .as_mut()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let status = connection::inspect_connection(connection)
            .map_err(|error| format!("Failed to inspect database journal mode: {error}"))?;
        let current = normalize_mode(&status.journal_mode)?;
        if current == "delete" {
            write_metadata(
                connection,
                "delete",
                "delete",
                TRANSITION_STABLE,
                None,
                None,
                None,
                None,
                Some(&now_text()),
            )?;
            return read_metadata(connection);
        }
        if current != "wal" {
            return Err(format!(
                "Controlled transition only supports WAL, got {current}"
            ));
        }
        let eligibility = self.database_mode_eligibility_under_maintenance(&current);
        if !eligibility.can_transition {
            return Err(format!(
                "Database is not eligible for WAL to DELETE transition: {}",
                eligibility.reasons.join("; ")
            ));
        }

        let id = transition_id();
        let started = now_text();
        write_metadata(
            connection,
            "wal",
            "delete",
            TRANSITIONING,
            Some(&id),
            Some("wal"),
            None,
            Some(&started),
            None,
        )?;
        let transition_result = (|| {
            let checkpointed: i64 = connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
                .map_err(|error| format!("Failed to checkpoint WAL before transition: {error}"))?;
            if checkpointed != 0 {
                return Err(format!(
                    "WAL checkpoint reported busy status {checkpointed}"
                ));
            }
            connection::set_journal_mode(connection, JournalMode::Delete).map_err(|error| {
                format!("Failed to switch SQLite journal mode to DELETE: {error}")
            })?;
            let final_status = connection::inspect_connection(connection)
                .map_err(|error| format!("Failed to verify DELETE journal mode: {error}"))?;
            if final_status.journal_mode != "delete" {
                return Err(format!(
                    "SQLite retained journal mode {}",
                    final_status.journal_mode
                ));
            }
            let data_dir = self
                .data_dir
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            for sidecar in ["screenshots.db-wal", "screenshots.db-shm"] {
                let path = data_dir.join(sidecar);
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "Failed to remove obsolete WAL sidecar {}: {error}",
                            path.display()
                        ));
                    }
                }
            }
            write_metadata(
                connection,
                "delete",
                "delete",
                TRANSITION_STABLE,
                None,
                Some("wal"),
                None,
                Some(&started),
                Some(&now_text()),
            )?;
            read_metadata(connection)
        })();
        match transition_result {
            Ok(metadata) => Ok(metadata),
            Err(error) => {
                let actual_after = connection::inspect_connection(connection)
                    .map(|status| status.journal_mode)
                    .unwrap_or_else(|_| current.clone());
                let metadata_error = write_metadata(
                    connection,
                    &actual_after,
                    "delete",
                    TRANSITION_FAILED,
                    Some(&id),
                    Some("wal"),
                    Some(&error),
                    Some(&started),
                    Some(&now_text()),
                )
                .err();
                if let Some(metadata_error) = metadata_error {
                    Err(format!(
                        "{error}; failed to persist transition failure: {metadata_error}"
                    ))
                } else {
                    Err(error)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn create_mode_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE database_mode_metadata (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                actual_journal_mode TEXT NOT NULL,
                requested_journal_mode TEXT NOT NULL,
                transition_state TEXT NOT NULL,
                transition_id TEXT,
                previous_journal_mode TEXT,
                last_error TEXT,
                started_at TEXT,
                completed_at TEXT,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .unwrap();
    }

    #[test]
    fn metadata_round_trip_records_actual_mode_and_delete_default() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let conn = Connection::open_in_memory().unwrap();
        create_mode_table(&conn);
        storage
            .initialize_database_mode_metadata(&conn, "delete")
            .unwrap();
        let metadata = read_metadata(&conn).unwrap();
        assert_eq!(metadata.actual_journal_mode, "delete");
        assert_eq!(metadata.requested_journal_mode, "delete");
        assert_eq!(metadata.transition_state, TRANSITION_STABLE);
    }

    #[test]
    fn metadata_preserves_non_delete_sqlite_mode() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let conn = Connection::open_in_memory().unwrap();
        create_mode_table(&conn);

        storage
            .initialize_database_mode_metadata(&conn, "truncate")
            .unwrap();
        let metadata = read_metadata(&conn).unwrap();
        assert_eq!(metadata.actual_journal_mode, "truncate");
        assert_eq!(metadata.requested_journal_mode, "truncate");
    }

    #[test]
    fn startup_completes_interrupted_transition_when_target_is_active() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let conn = Connection::open_in_memory().unwrap();
        create_mode_table(&conn);
        write_metadata(
            &conn,
            "wal",
            "delete",
            TRANSITIONING,
            Some("transition-1"),
            Some("wal"),
            None,
            Some("started"),
            None,
        )
        .unwrap();

        storage
            .initialize_database_mode_metadata(&conn, "delete")
            .unwrap();

        let metadata = read_metadata(&conn).unwrap();
        assert_eq!(metadata.actual_journal_mode, "delete");
        assert_eq!(metadata.requested_journal_mode, "delete");
        assert_eq!(metadata.transition_state, TRANSITION_STABLE);
        assert!(metadata.transition_id.is_none());
        assert!(metadata.last_error.is_none());
        assert!(metadata.completed_at.is_some());
    }

    #[test]
    fn startup_marks_uncontrolled_mode_drift_failed() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let conn = Connection::open_in_memory().unwrap();
        create_mode_table(&conn);
        storage
            .initialize_database_mode_metadata(&conn, "delete")
            .unwrap();

        storage
            .initialize_database_mode_metadata(&conn, "wal")
            .unwrap();

        let metadata = read_metadata(&conn).unwrap();
        assert_eq!(metadata.actual_journal_mode, "wal");
        assert_eq!(metadata.requested_journal_mode, "delete");
        assert_eq!(metadata.transition_state, TRANSITION_FAILED);
        assert!(metadata
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("outside the controlled transition"));
    }

    #[test]
    fn storage_initialization_creates_delete_mode_metadata() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(
            &credential,
            b"mode-metadata-public-key",
        )
        .unwrap();
        let storage = StorageState::new(temp.path().to_path_buf(), credential);

        storage.initialize().unwrap();

        let metadata = storage.database_mode_metadata().unwrap();
        assert_eq!(metadata.actual_journal_mode, "delete");
        assert_eq!(metadata.requested_journal_mode, "delete");
        assert_eq!(metadata.transition_state, TRANSITION_STABLE);
    }

    #[test]
    fn eligibility_reports_wal_requirements_without_touching_sqlite() {
        let eligibility = evaluate_eligibility(
            "wal",
            Ok(()),
            true,
            Some(128 * 1024 * 1024),
            64 * 1024 * 1024,
        );
        assert!(eligibility.can_transition);
        assert!(eligibility.writable_filesystem);
        assert!(eligibility.local_filesystem);
        assert!(eligibility.reasons.is_empty());

        let insufficient = evaluate_eligibility("wal", Ok(()), true, Some(1), 64);
        assert!(!insufficient.can_transition);
        assert!(insufficient
            .reasons
            .iter()
            .any(|reason| reason.contains("Insufficient free space")));

        let unavailable = evaluate_eligibility(
            "wal",
            Err("Filesystem is not writable".into()),
            false,
            None,
            64,
        );
        assert!(!unavailable.can_transition);
        assert!(!unavailable.writable_filesystem);
        assert!(!unavailable.local_filesystem);
        assert!(unavailable.reasons.len() >= 3);
    }

    #[test]
    fn wal_transition_checkpoints_rows_and_cleans_sidecars() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(&credential, b"mode-test-public-key")
            .unwrap();
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        storage.initialize().unwrap();

        {
            let mut guard = storage
                .get_connection_named("mode_test_enable_wal")
                .unwrap();
            let connection = guard.as_mut().unwrap();
            connection
                .pragma_update(None, "wal_autocheckpoint", 0)
                .unwrap();
            connection::set_journal_mode(connection, JournalMode::Wal).unwrap();
            connection
                .execute(
                    "INSERT INTO app_metadata(key, value) VALUES ('mode-test', 'committed')",
                    [],
                )
                .unwrap();
        }
        assert!(temp.path().join("screenshots.db-wal").exists());
        assert!(temp.path().join("screenshots.db-shm").exists());

        let metadata = storage.transition_wal_to_delete().unwrap();
        assert_eq!(metadata.actual_journal_mode, "delete");
        assert_eq!(metadata.requested_journal_mode, "delete");
        assert_eq!(metadata.transition_state, TRANSITION_STABLE);

        let guard = storage.get_connection_named("mode_test_verify").unwrap();
        let connection = guard.as_ref().unwrap();
        let value: String = connection
            .query_row(
                "SELECT value FROM app_metadata WHERE key = 'mode-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "committed");
        assert!(!temp.path().join("screenshots.db-wal").exists());
        assert!(!temp.path().join("screenshots.db-shm").exists());
    }

    #[test]
    fn local_write_probe_accepts_existing_temp_directory() {
        let temp = tempdir().unwrap();
        assert!(writable_probe(temp.path()).is_ok());
        assert!(is_local_filesystem(temp.path()));
    }
}
