//! Startup journal-mode policy and safe WAL/DELETE conversion.
//!
//! SQLite owns the effective mode. Failed conversions are logged and retried at
//! the next initialization, without a separate transition ledger.

use super::connection::{self, JournalMode};
use super::policy::disk_totals_for_path;
use super::StorageState;
use rusqlite::Connection;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const SAFETY_FREE_BYTES: u64 = 64 * 1024 * 1024;
const SQLITE_JOURNAL_MODES: [&str; 6] = ["delete", "truncate", "persist", "memory", "wal", "off"];

// Keep the existing environment variable name for compatibility with launch scripts.
pub(crate) const WAL_EXPERIMENT_ENV: &str = "CARBONPAPER_WAL_EXPERIMENT";

/// The journal mode selected once for the lifetime of a process.
///
/// WAL is the default in development and release builds. The environment
/// override can still request DELETE for compatibility or troubleshooting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DatabaseModePolicy {
    Delete,
    #[default]
    Wal,
}

impl DatabaseModePolicy {
    pub(crate) const fn target(self) -> JournalMode {
        match self {
            Self::Delete => JournalMode::Delete,
            Self::Wal => JournalMode::Wal,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.target().as_str()
    }
}

/// Parse the compatibility override without depending on the build profile or
/// mutating the process environment in tests. Invalid values warn and use
/// the default WAL policy so this optional flag cannot prevent startup.
pub(crate) fn parse_database_mode_policy(raw_value: Option<&str>) -> DatabaseModePolicy {
    let value = raw_value.map(str::trim).filter(|value| !value.is_empty());
    match value {
        None => DatabaseModePolicy::default(),
        Some("0") => DatabaseModePolicy::Delete,
        Some("1") => DatabaseModePolicy::Wal,
        Some(value) => {
            tracing::warn!(
                "Invalid {WAL_EXPERIMENT_ENV} value {value:?}; expected 1 or 0; using WAL startup policy"
            );
            DatabaseModePolicy::default()
        }
    }
}

/// Resolve the startup policy once, before the `StorageState` is constructed.
pub(crate) fn database_mode_policy_from_environment() -> DatabaseModePolicy {
    let raw_value = std::env::var(WAL_EXPERIMENT_ENV).ok();
    parse_database_mode_policy(raw_value.as_deref())
}

fn normalize_mode(mode: &str) -> Result<String, String> {
    let normalized = mode.trim().to_ascii_lowercase();
    if SQLITE_JOURNAL_MODES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(format!("Unsupported SQLite journal mode: {normalized}"))
    }
}

fn is_supported_mode_conversion(current_mode: &str, target_mode: &str) -> bool {
    matches!(
        (current_mode, target_mode),
        ("delete", "wal") | ("wal", "delete")
    )
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
        rand::random::<u64>()
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
    target_mode: &str,
    writable_result: Result<(), String>,
    local_filesystem: bool,
    available_free_bytes: Option<u64>,
    required_free_bytes: u64,
) -> Result<(), String> {
    if current_mode == target_mode {
        return Ok(());
    }
    if !is_supported_mode_conversion(current_mode, target_mode) {
        return Err(format!("Controlled startup transition only supports DELETE and WAL, got {current_mode} -> {target_mode}"));
    }
    let mut reasons = Vec::new();
    if let Err(error) = writable_result {
        reasons.push(error);
    }
    if !local_filesystem {
        reasons.push("Database directory is on a non-local filesystem".to_string());
    }
    match available_free_bytes {
        None => reasons.push("Unable to determine free space on the database volume".to_string()),
        Some(free) if free < required_free_bytes => reasons.push(format!(
            "Insufficient free space: required {required_free_bytes} bytes"
        )),
        Some(_) => {}
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons.join("; "))
    }
}

impl StorageState {
    fn check_startup_mode_conversion(&self, current: &str, target: &str) -> Result<(), String> {
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let required = database_group_size(&data_dir)
            .saturating_mul(2)
            .saturating_add(SAFETY_FREE_BYTES);
        evaluate_eligibility(
            current,
            target,
            writable_probe(&data_dir),
            is_local_filesystem(&data_dir),
            disk_totals_for_path(&data_dir).map(|(_, free)| free),
            required,
        )
    }

    fn remove_wal_sidecars(&self) -> Result<(), String> {
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
        Ok(())
    }

    /// Switch an already-open connection between the two supported modes and
    /// verify SQLite's effective result before returning.
    fn transition_connection_to_target(
        &self,
        connection: &Connection,
        current_mode: &str,
        target_mode: &str,
    ) -> Result<connection::ConnectionStatus, String> {
        match (current_mode, target_mode) {
            ("delete", "wal") => connection::set_journal_mode(connection, JournalMode::Wal)
                .map_err(|error| format!("Failed to switch SQLite journal mode to WAL: {error}")),
            ("wal", "delete") => {
                let checkpoint = connection::checkpoint_wal_truncate(connection).map_err(|error| {
                    format!("Failed to checkpoint WAL before DELETE transition: {error}")
                })?;
                if checkpoint.busy != 0 {
                    return Err(format!(
                        "WAL checkpoint reported busy status {} (log_frames={}, checkpointed_frames={})",
                        checkpoint.busy,
                        checkpoint.log_frames,
                        checkpoint.checkpointed_frames
                    ));
                }
                let status = connection::set_journal_mode(connection, JournalMode::Delete)
                    .map_err(|error| {
                        format!("Failed to switch SQLite journal mode to DELETE: {error}")
                    })?;
                if status.journal_mode != "delete" {
                    return Err(format!(
                        "SQLite retained journal mode {}",
                        status.journal_mode
                    ));
                }
                self.remove_wal_sidecars()?;
                Ok(status)
            }
            _ => Err(format!(
                "Controlled startup transition only supports DELETE and WAL, got {current_mode} -> {target_mode}"
            )),
        }
    }

    fn defer_startup_transition(
        &self,
        connection: &Connection,
        current_status: &connection::ConnectionStatus,
        target: &str,
        error: &str,
    ) -> connection::ConnectionStatus {
        // A conversion can reach its target before sidecar cleanup fails.
        let effective =
            connection::inspect_connection(connection).unwrap_or_else(|_| current_status.clone());
        tracing::warn!(
            "Deferring startup journal mode transition {} -> {}; continuing with {}: {}",
            current_status.journal_mode,
            target,
            effective.journal_mode,
            error
        );
        effective
    }

    /// Apply the process policy before primary or independent connections are
    /// published. A failed supported conversion keeps the database available.
    pub(crate) fn apply_startup_database_mode(
        &self,
        connection: &Connection,
        current_status: &connection::ConnectionStatus,
    ) -> Result<connection::ConnectionStatus, String> {
        let current = normalize_mode(&current_status.journal_mode)?;
        let target = self.database_mode_policy.as_str();
        if current == target {
            if target == JournalMode::Delete.as_str() {
                self.remove_wal_sidecars()?;
            }
            return connection::inspect_connection(connection).map_err(|error| {
                format!("Failed to verify startup journal mode {target}: {error}")
            });
        }
        if !is_supported_mode_conversion(&current, target) {
            return Err(format!("Controlled startup transition only supports DELETE and WAL, got {current} -> {target}"));
        }
        match self
            .check_startup_mode_conversion(&current, target)
            .and_then(|()| self.transition_connection_to_target(connection, &current, target))
        {
            Ok(status) => Ok(status),
            Err(error) => {
                Ok(self.defer_startup_transition(connection, current_status, target, &error))
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

    #[test]
    fn database_mode_policy_defaults_to_wal_in_all_builds() {
        assert_eq!(DatabaseModePolicy::default(), DatabaseModePolicy::Wal);
        for value in [None, Some(""), Some("  "), Some("1"), Some(" 1 ")] {
            assert_eq!(parse_database_mode_policy(value), DatabaseModePolicy::Wal);
        }
    }

    #[test]
    fn database_mode_policy_allows_an_explicit_delete_override() {
        for value in ["0", " 0 "] {
            assert_eq!(
                parse_database_mode_policy(Some(value)),
                DatabaseModePolicy::Delete
            );
        }
    }

    #[test]
    fn invalid_database_mode_values_use_default_wal_policy() {
        for value in ["true", "false", "2", "wal", "-1", "invalid", " 2 "] {
            assert_eq!(
                parse_database_mode_policy(Some(value)),
                DatabaseModePolicy::Wal,
                "invalid optional flag {value:?} must retain the default"
            );
        }
    }

    #[test]
    fn explicit_delete_policy_creates_delete_mode() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(
            &credential,
            b"mode-metadata-public-key",
        )
        .unwrap();
        let storage = StorageState::new_with_mode_policy(
            temp.path().to_path_buf(),
            credential,
            parse_database_mode_policy(Some("0")),
        );

        storage.initialize().unwrap();

        let status = connection::inspect_connection(
            &storage
                .open_read_connection_named("mode_test_status")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status.journal_mode, "delete");
    }

    #[test]
    fn invalid_database_mode_value_allows_storage_initialization() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(&credential, b"invalid-mode-public-key")
            .unwrap();
        let storage = StorageState::new_with_mode_policy(
            temp.path().to_path_buf(),
            credential,
            parse_database_mode_policy(Some("true")),
        );

        storage
            .initialize()
            .expect("invalid optional flag must not block storage startup");

        let status = connection::inspect_connection(
            &storage
                .open_read_connection_named("mode_test_status")
                .unwrap(),
        )
        .unwrap();
        assert!(storage.is_initialized());
        assert_eq!(status.journal_mode, "wal");
    }

    #[test]
    fn eligibility_reports_wal_requirements_without_touching_sqlite() {
        let eligibility = evaluate_eligibility(
            "wal",
            "delete",
            Ok(()),
            true,
            Some(128 * 1024 * 1024),
            64 * 1024 * 1024,
        );
        assert!(eligibility.is_ok());

        let insufficient = evaluate_eligibility("wal", "delete", Ok(()), true, Some(1), 64);
        assert!(insufficient
            .unwrap_err()
            .contains("Insufficient free space"));

        let unavailable = evaluate_eligibility(
            "wal",
            "delete",
            Err("Filesystem is not writable".into()),
            false,
            None,
            64,
        );
        let error = unavailable.unwrap_err();
        assert!(error.contains("not writable"));
        assert!(error.contains("non-local filesystem"));
        assert!(error.contains("Unable to determine free space"));
    }

    #[test]
    fn same_mode_wal_startup_does_not_require_conversion_space() {
        let eligibility = evaluate_eligibility(
            "wal",
            "wal",
            Err("Filesystem is not writable".into()),
            false,
            Some(1),
            64,
        );

        assert!(eligibility.is_ok());
    }

    #[test]
    fn deferred_startup_transition_keeps_effective_mode() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let connection = Connection::open(temp.path().join("deferred-transition.db")).unwrap();
        let current_status = connection::set_journal_mode(&connection, JournalMode::Wal).unwrap();
        let status = storage.defer_startup_transition(
            &connection,
            &current_status,
            "delete",
            "Insufficient free space: test",
        );
        assert_eq!(status.journal_mode, "wal");
    }

    #[test]
    fn default_startup_verifies_wal_mode_before_publish() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(&credential, b"wal-startup-key")
            .unwrap();
        let storage = StorageState::new(temp.path().to_path_buf(), credential);

        storage.initialize().unwrap();

        let status = connection::inspect_connection(
            &storage
                .open_read_connection_named("mode_test_status")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status.journal_mode, "wal");
        let read = storage
            .open_read_connection_named("wal_startup_mode_read")
            .unwrap();
        let status = connection::inspect_connection(&read).unwrap();
        assert_eq!(status.journal_mode, "wal");
        assert_eq!(status.synchronous, "FULL");
    }

    #[test]
    fn delete_startup_policy_reverts_an_existing_wal_database() {
        let temp = tempdir().unwrap();
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        crate::credential_manager::save_public_key_to_file(&credential, b"wal-revert-key").unwrap();
        let wal_storage = StorageState::new_with_mode_policy(
            temp.path().to_path_buf(),
            credential.clone(),
            DatabaseModePolicy::Wal,
        );
        wal_storage.initialize().unwrap();
        {
            let guard = wal_storage.get_connection_named("wal_revert_seed").unwrap();
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO app_metadata(key, value) VALUES ('wal-revert', 'preserved')",
                    [],
                )
                .unwrap();
        }
        wal_storage.shutdown().unwrap();

        let delete_storage = StorageState::new_with_mode_policy(
            temp.path().to_path_buf(),
            credential,
            parse_database_mode_policy(Some("0")),
        );
        delete_storage.initialize().unwrap();
        let status = connection::inspect_connection(
            &delete_storage
                .open_read_connection_named("mode_test_status")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status.journal_mode, "delete");
        let guard = delete_storage
            .get_connection_named("wal_revert_verify")
            .unwrap();
        let value: String = guard
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT value FROM app_metadata WHERE key = 'wal-revert'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "preserved");
        assert!(!temp.path().join("screenshots.db-wal").exists());
        assert!(!temp.path().join("screenshots.db-shm").exists());
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

        let status = {
            let guard = storage
                .get_connection_named("mode_test_transition")
                .unwrap();
            storage
                .transition_connection_to_target(guard.as_ref().unwrap(), "wal", "delete")
                .unwrap()
        };
        assert_eq!(status.journal_mode, "delete");

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
