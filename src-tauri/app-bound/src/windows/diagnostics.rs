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
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

static LOGGER: OnceLock<ServiceLogger> = OnceLock::new();

pub fn init(state_root: &Path) {
    let _ = LOGGER.set(ServiceLogger::new(state_root.join(LOG_DIRECTORY)));
}

pub fn event(level: &str, event: &str, stage: &str, error: Option<BrokerError>) {
    if let Some(logger) = LOGGER.get() {
        logger.event(level, event, stage, error);
    }
}

struct ServiceLogger {
    directory: PathBuf,
    lock: Mutex<()>,
}

impl ServiceLogger {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            lock: Mutex::new(()),
        }
    }

    fn event(&self, level: &str, event: &str, stage: &str, error: Option<BrokerError>) {
        let Ok(_guard) = self.lock.lock() else {
            return;
        };
        let _ = self.write_event(level, event, stage, error);
    }

    fn write_event(
        &self,
        level: &str,
        event: &str,
        stage: &str,
        error: Option<BrokerError>,
    ) -> std::io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let current = self.directory.join(LOG_FILE);
        rotate_if_needed(&current, &self.directory.join(ROTATED_LOG_FILE))?;
        let mut file = OpenOptions::new().create(true).append(true).open(current)?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        writeln!(
            file,
            "time_unix={timestamp} level={level} event={event} stage={stage} error={}",
            error.map_or("none", BrokerError::code)
        )
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
