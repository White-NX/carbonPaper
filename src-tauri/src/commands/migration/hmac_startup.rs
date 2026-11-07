//! Authentication admits one application-owned upgrade. Views only observe it.

use crate::credential_manager::CredentialManagerState;
use crate::storage::StorageState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Default, Serialize)]
pub(crate) struct HmacProgress {
    pub is_running: bool,
    pub phase: String,
    pub processed: usize,
    pub total: usize,
    pub error: Option<String>,
    pub revision: u64,
}

#[derive(Default)]
pub struct HmacMigrationRuntime {
    active: AtomicBool,
    progress: Mutex<HmacProgress>,
}

impl HmacMigrationRuntime {
    pub(super) fn snapshot(&self) -> HmacProgress {
        self.progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn claim(&self) -> bool {
        if self
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *progress = HmacProgress {
            revision: progress.revision + 1,
            ..Default::default()
        };
        true
    }

    fn begin(&self) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.is_running = true;
        progress.phase = "preparing".into();
        progress.revision += 1;
    }

    fn update(&self, phase: &str, processed: usize, total: usize) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.phase = phase.into();
        progress.processed = processed;
        progress.total = total;
        progress.revision += 1;
    }

    fn finish(&self, result: &Result<(), String>) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.is_running = false;
        // A lost session defers admission to the next explicit unlock.
        progress.error = result
            .as_ref()
            .err()
            .filter(|error| error.as_str() != "AUTH_REQUIRED")
            .cloned();
        progress.revision += 1;
        self.active.store(false, Ordering::Release);
    }
}

fn publish(app: &AppHandle, runtime: &HmacMigrationRuntime) {
    let _ = app.emit("hmac-migration-progress", runtime.snapshot());
}

pub(crate) fn spawn_auto_hmac_migration(app: AppHandle) {
    if !crate::storage_startup::is_ready(&app)
        || !app
            .state::<Arc<CredentialManagerState>>()
            .is_session_valid()
    {
        return;
    }
    let runtime = app.state::<Arc<HmacMigrationRuntime>>().inner().clone();
    if !runtime.claim() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let result = run_claimed(app.clone(), runtime.clone(), true).await;
        runtime.finish(&result);
        publish(&app, &runtime);
        if let Err(error) = result {
            if error != "AUTH_REQUIRED" {
                tracing::warn!("Automatic search-index upgrade failed: {error}");
            }
        } else {
            let _ = app.emit("hmac-migration-complete", ());
        }
    });
}

pub(crate) async fn run_manual(app: AppHandle) -> Result<(), String> {
    let runtime = app.state::<Arc<HmacMigrationRuntime>>().inner().clone();
    if !runtime.claim() {
        return Err("ALREADY_RUNNING".into());
    }
    let result = run_claimed(app.clone(), runtime.clone(), false).await;
    runtime.finish(&result);
    publish(&app, &runtime);
    if result.is_ok() {
        let _ = app.emit("hmac-migration-complete", ());
    }
    result
}

async fn run_claimed(
    app: AppHandle,
    runtime: Arc<HmacMigrationRuntime>,
    automatic: bool,
) -> Result<(), String> {
    let credentials = app.state::<Arc<CredentialManagerState>>().inner().clone();
    // Dataset replacement must not interleave the migration's cursor batches.
    let _admission = loop {
        crate::commands::check_auth_required(&credentials)?;
        match crate::maintenance::runtime_control_guard() {
            Ok(permit) => break permit,
            Err(error) if automatic && error == crate::maintenance::MAINTENANCE_IN_PROGRESS => {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(error) => return Err(error),
        }
    };
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    tokio::task::spawn_blocking(move || {
        crate::commands::check_auth_required(&credentials)?;
        if !storage.check_hmac_migration_status()? {
            return Ok(());
        }
        // Admission probes and archives needing no work must not flash a
        // progress window during an otherwise ordinary unlock.
        runtime.begin();
        publish(&app, &runtime);
        storage.run_hmac_migration(|phase, processed, total| {
            runtime.update(phase, processed, total);
            publish(&app, &runtime);
        })
    })
    .await
    .map_err(|error| format!("Search-index upgrade task failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_unlocks_admit_one_upgrade_and_recreated_views_read_its_progress() {
        let runtime = Arc::new(HmacMigrationRuntime::default());
        let workers = (0..8)
            .map(|_| {
                let runtime = runtime.clone();
                std::thread::spawn(move || runtime.claim())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|claimed| *claimed)
                .count(),
            1
        );
        runtime.begin();
        runtime.update("indexing", 512, 1000);
        let existing = runtime.snapshot();
        assert!(existing.is_running);
        assert_eq!((existing.processed, existing.total), (512, 1000));
        assert!(!runtime.claim());
        runtime.finish(&Ok(()));
        let completed = runtime.snapshot();
        assert!(!completed.is_running);
        assert!(completed.revision > existing.revision);
    }

    #[test]
    fn an_empty_upgrade_probe_never_reports_a_running_job() {
        let runtime = HmacMigrationRuntime::default();
        assert!(runtime.claim());
        assert!(!runtime.snapshot().is_running);
        runtime.finish(&Ok(()));
        assert!(!runtime.snapshot().is_running);
        assert!(runtime.snapshot().error.is_none());
    }

    #[test]
    fn lost_authentication_defers_without_an_error_and_a_new_unlock_can_retry() {
        let runtime = HmacMigrationRuntime::default();
        assert!(runtime.claim());
        runtime.finish(&Err("AUTH_REQUIRED".into()));
        assert!(!runtime.snapshot().is_running);
        assert!(runtime.snapshot().error.is_none());
        assert!(runtime.claim());
        runtime.finish(&Err("disk failure".into()));
        assert_eq!(runtime.snapshot().error.as_deref(), Some("disk failure"));
        assert!(runtime.claim());
        assert!(runtime.snapshot().error.is_none());
    }
}
