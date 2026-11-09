use crate::protocol::BrokerError;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const LOG_DIRECTORY: &str = "logs";
const LOG_FILE: &str = "service.log";
const ROTATED_LOG_FILE: &str = "service.log.1";
const SETUP_LOG_FILE: &str = "setup.log";
const ROTATED_SETUP_LOG_FILE: &str = "setup.log.1";
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

static LOGGER: OnceLock<ServiceLogger> = OnceLock::new();

/// Numeric timings only; never include caller identity, task data or keys.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct RequestTimings {
    pub total_ms: u64,
    pub verify_ms: u64,
    pub ledger_wait_ms: u64,
    pub worker_wait_ms: u64,
    pub ledger_exec_ms: u64,
}

pub fn init(state_root: &Path) {
    let _ = LOGGER.set(ServiceLogger::new(state_root.join(LOG_DIRECTORY)));
}

/// Setup writes only fixed, redacted fields to a user-readable protected
/// directory. Its failures remain available without access to the key ledger.
pub(super) fn init_setup(system_root: &Path) {
    let _ = LOGGER.set(ServiceLogger::with_files(
        system_root.join(LOG_DIRECTORY),
        SETUP_LOG_FILE,
        ROTATED_SETUP_LOG_FILE,
    ));
}

pub fn event(level: &str, event: &str, stage: &str, error: Option<BrokerError>) {
    if let Some(logger) = LOGGER.get() {
        logger.event(level, event, stage, error);
    }
}

pub(super) fn timed_event(
    level: &str,
    event: &str,
    stage: &str,
    error: Option<BrokerError>,
    timings: &RequestTimings,
) {
    if let Some(logger) = LOGGER.get() {
        logger.event_with_timings(level, event, stage, error, Some(timings));
    }
}

struct ServiceLogger {
    directory: PathBuf,
    file_name: &'static str,
    rotated_file_name: &'static str,
    lock: Mutex<()>,
}

impl ServiceLogger {
    fn new(directory: PathBuf) -> Self {
        Self::with_files(directory, LOG_FILE, ROTATED_LOG_FILE)
    }

    fn with_files(
        directory: PathBuf,
        file_name: &'static str,
        rotated_file_name: &'static str,
    ) -> Self {
        Self {
            directory,
            file_name,
            rotated_file_name,
            lock: Mutex::new(()),
        }
    }

    fn event(&self, level: &str, event: &str, stage: &str, error: Option<BrokerError>) {
        self.event_with_timings(level, event, stage, error, None);
    }

    fn event_with_timings(
        &self,
        level: &str,
        event: &str,
        stage: &str,
        error: Option<BrokerError>,
        timings: Option<&RequestTimings>,
    ) {
        let Ok(_guard) = self.lock.lock() else {
            return;
        };
        let _ = self.write_event(level, event, stage, error, timings);
    }

    fn write_event(
        &self,
        level: &str,
        event: &str,
        stage: &str,
        error: Option<BrokerError>,
        timings: Option<&RequestTimings>,
    ) -> std::io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let current = self.directory.join(self.file_name);
        rotate_if_needed(&current, &self.directory.join(self.rotated_file_name))?;
        let mut file = OpenOptions::new().create(true).append(true).open(current)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        write!(
            file,
            "time_unix={timestamp} level={level} event={event} stage={stage} error={}",
            error.map_or("none", BrokerError::code)
        )?;
        if let Some(timings) = timings {
            write!(
                file,
                " total_ms={} verify_ms={} ledger_wait_ms={} worker_wait_ms={} ledger_exec_ms={}",
                timings.total_ms,
                timings.verify_ms,
                timings.ledger_wait_ms,
                timings.worker_wait_ms,
                timings.ledger_exec_ms
            )?;
        }
        writeln!(file)
    }
}

fn rotate_if_needed(current: &Path, rotated: &Path) -> std::io::Result<()> {
    let Ok(metadata) = current.metadata() else {
        return Ok(());
    };
    if metadata.len() < MAX_LOG_BYTES {
        return Ok(());
    }
    if rotated.exists() {
        fs::remove_file(rotated)?;
    }
    fs::rename(current, rotated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn setup_log_identifies_the_failed_upgrade_stage_without_private_data() {
        let directory = tempfile::tempdir().expect("create setup log directory");
        let logger = ServiceLogger::with_files(
            directory.path().to_path_buf(),
            SETUP_LOG_FILE,
            ROTATED_SETUP_LOG_FILE,
        );
        logger.event(
            "error",
            "install_failed",
            "read_previous_runtime_manifest",
            Some(BrokerError::Integrity),
        );
        let output = fs::read_to_string(directory.path().join(SETUP_LOG_FILE)).unwrap();
        assert!(output.contains(
            "event=install_failed stage=read_previous_runtime_manifest error=app_bound_integrity_failure"
        ));
        assert_eq!(output.split_whitespace().count(), 5);
        assert!(!directory.path().join(LOG_FILE).exists());
    }

    #[test]
    fn logger_appends_redacted_fixed_fields() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let logger = ServiceLogger::new(directory.path().join(LOG_DIRECTORY));

        logger.event(
            "warn",
            "request_failed",
            "verify_caller",
            Some(BrokerError::AccessDenied),
        );
        logger.event("info", "service_ready", "serve", None);

        let output = fs::read_to_string(directory.path().join(LOG_DIRECTORY).join(LOG_FILE))
            .expect("read service log");
        assert!(output.contains("event=request_failed stage=verify_caller"));
        assert!(output.contains("error=app_bound_access_denied"));
        assert!(output.contains("event=service_ready stage=serve error=none"));
        assert!(!output.contains("S-1-5-21"));
        assert!(!output.contains("task_id"));
    }

    #[test]
    fn logger_rotates_an_oversized_file() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let logs = directory.path().join(LOG_DIRECTORY);
        fs::create_dir(&logs).expect("create log directory");
        let current = logs.join(LOG_FILE);
        let file = File::create(&current).expect("create service log");
        file.set_len(MAX_LOG_BYTES).expect("expand service log");
        drop(file);
        let logger = ServiceLogger::new(logs.clone());

        logger.event("info", "service_ready", "serve", None);

        assert!(logs.join(ROTATED_LOG_FILE).exists());
        assert!(fs::read_to_string(current)
            .expect("read current log")
            .contains("event=service_ready"));
    }

    #[test]
    fn timed_events_append_only_fixed_numeric_fields() {
        let directory = tempfile::tempdir().expect("create test directory");
        let logger = ServiceLogger::new(directory.path().join(LOG_DIRECTORY));
        logger.event_with_timings(
            "warn",
            "connection_failed",
            "finish_consumer",
            Some(BrokerError::Unavailable),
            Some(&RequestTimings {
                total_ms: 8000,
                verify_ms: 2000,
                ledger_wait_ms: 5990,
                worker_wait_ms: 10,
                ledger_exec_ms: 0,
            }),
        );
        let output = fs::read_to_string(directory.path().join(LOG_DIRECTORY).join(LOG_FILE))
            .expect("read timed log");

        let fields: std::collections::HashMap<_, _> = output
            .split_whitespace()
            .map(|field| field.split_once('=').expect("key=value log field"))
            .collect();
        for (key, expected) in [
            ("level", "warn"),
            ("event", "connection_failed"),
            ("stage", "finish_consumer"),
            ("error", "app_bound_unavailable"),
            ("total_ms", "8000"),
            ("verify_ms", "2000"),
            ("ledger_wait_ms", "5990"),
            ("worker_wait_ms", "10"),
            ("ledger_exec_ms", "0"),
        ] {
            assert_eq!(fields.get(key), Some(&expected), "{key}");
        }
        assert!(fields["time_unix"].parse::<u64>().is_ok());
        assert_eq!(fields.len(), 10, "no extra fields may expose private data");
    }

    #[test]
    fn logging_failure_is_not_reported_to_the_caller() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let occupied = directory.path().join("occupied");
        File::create(&occupied).expect("create occupied path");
        let logger = ServiceLogger::new(occupied);

        logger.event(
            "error",
            "service_failed",
            "startup",
            Some(BrokerError::Storage),
        );
    }
}
