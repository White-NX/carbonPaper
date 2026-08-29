//! Repair for stale OCR ids left in the blind bitmap index.

use crate::migration_support;
use crate::storage::{BlindIndexRepairProgress, StorageState};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

const RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize)]
pub struct BlindIndexRepairStatus {
    pub running: bool,
    pub phase: String,
    pub processed: u64,
    pub total: u64,
    pub changed_postings: u64,
    pub deleted_postings: u64,
    pub removed_ocr_ids: u64,
    pub failed: u64,
    pub last_error: Option<String>,
}

impl Default for BlindIndexRepairStatus {
    fn default() -> Self {
        Self {
            running: false,
            phase: "idle".to_string(),
            processed: 0,
            total: 0,
            changed_postings: 0,
            deleted_postings: 0,
            removed_ocr_ids: 0,
            failed: 0,
            last_error: None,
        }
    }
}

pub struct BlindIndexRepairState {
    running: AtomicBool,
    status: Mutex<BlindIndexRepairStatus>,
}

impl Default for BlindIndexRepairState {
    fn default() -> Self {
        Self::new()
    }
}

impl BlindIndexRepairState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            status: Mutex::new(BlindIndexRepairStatus::default()),
        }
    }

    fn update(&self, update: impl FnOnce(&mut BlindIndexRepairStatus)) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        update(&mut status);
    }

    fn snapshot(&self) -> BlindIndexRepairStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

pub fn spawn_blind_index_auto_repair(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let storage = app.state::<Arc<StorageState>>().inner().clone();
            let needed =
                match tokio::task::spawn_blocking(move || storage.is_blind_index_repair_needed())
                    .await
                {
                    Ok(Ok(needed)) => needed,
                    Ok(Err(error)) => {
                        tracing::warn!("[BLIND_INDEX_REPAIR] readiness check failed: {error}");
                        return;
                    }
                    Err(error) => {
                        tracing::warn!("[BLIND_INDEX_REPAIR] readiness task failed: {error}");
                        return;
                    }
                };
            if !needed {
                return;
            }

            if crate::maintenance::is_active() {
                tokio::time::sleep(RETRY_INTERVAL).await;
                continue;
            }

            let state = app.state::<Arc<BlindIndexRepairState>>().inner().clone();
            match try_start_repair(&app, &state) {
                Ok(_) => {
                    while state.running.load(Ordering::SeqCst) {
                        tokio::time::sleep(RETRY_INTERVAL).await;
                    }
                    if state.snapshot().failed > 0 {
                        return;
                    }
                }
                Err(error) if error == crate::maintenance::MAINTENANCE_IN_PROGRESS => {
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
                Err(error) => {
                    tracing::warn!("[BLIND_INDEX_REPAIR] automatic repair not started: {error}");
                    return;
                }
            }
        }
    });
}

fn try_start_repair(
    app: &AppHandle,
    state: &Arc<BlindIndexRepairState>,
) -> Result<BlindIndexRepairStatus, String> {
    if state
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(state.snapshot());
    }

    let Some(maintenance) = crate::maintenance::enter("blind_index_repair") else {
        state.running.store(false, Ordering::SeqCst);
        return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.to_string());
    };

    state.update(|status| {
        *status = BlindIndexRepairStatus {
            running: true,
            phase: "scanning_valid_ocr".to_string(),
            ..BlindIndexRepairStatus::default()
        };
    });

    let worker_app = app.clone();
    let worker_state = state.clone();
    tauri::async_runtime::spawn(async move {
        run_repair(worker_app, worker_state, maintenance).await;
    });

    Ok(state.snapshot())
}

async fn run_repair(
    app: AppHandle,
    state: Arc<BlindIndexRepairState>,
    maintenance: crate::maintenance::MaintenanceGuard,
) {
    tracing::info!("[BLIND_INDEX_REPAIR] starting stale-posting repair");

    let restore = migration_support::pause_capture_for_maintenance(&app).await;
    let result = match restore.as_ref() {
        Ok(_) => {
            let storage = app.state::<Arc<StorageState>>().inner().clone();
            let progress_state = state.clone();
            tokio::task::spawn_blocking(move || {
                storage.run_blind_index_repair(move |progress: BlindIndexRepairProgress| {
                    progress_state.update(|status| {
                        status.running = true;
                        status.phase = "repairing_blind_index".to_string();
                        status.processed = progress.processed_postings;
                        status.total = progress.total_postings;
                        status.changed_postings = progress.changed_postings;
                        status.deleted_postings = progress.deleted_postings;
                        status.removed_ocr_ids = progress.removed_ocr_ids;
                        status.failed = 0;
                        status.last_error = None;
                    });
                })
            })
            .await
            .map_err(|error| format!("Blind-index repair task failed: {error}"))
            .and_then(|result| result)
        }
        Err(error) => Err(error.clone()),
    };

    if let Ok(restore) = restore.as_ref() {
        migration_support::restore_monitor_after_migration(&app, restore).await;
    }

    match result {
        Ok(summary) => {
            tracing::info!(
                "[BLIND_INDEX_REPAIR] complete: processed_postings={}, changed_postings={}, deleted_postings={}, removed_ocr_ids={}",
                summary.processed_postings,
                summary.changed_postings,
                summary.deleted_postings,
                summary.removed_ocr_ids
            );
            state.update(|status| {
                status.running = false;
                status.phase = "completed".to_string();
                status.processed = summary.processed_postings;
                status.total = summary.total_postings;
                status.changed_postings = summary.changed_postings;
                status.deleted_postings = summary.deleted_postings;
                status.removed_ocr_ids = summary.removed_ocr_ids;
                status.failed = 0;
                status.last_error = None;
            });
        }
        Err(error) => {
            tracing::error!("[BLIND_INDEX_REPAIR] failed: {error}");
            state.update(|status| {
                status.running = false;
                status.phase = "failed".to_string();
                status.failed = 1;
                status.last_error = Some(error);
            });
        }
    }

    state.running.store(false, Ordering::SeqCst);
    drop(maintenance);
}

#[tauri::command]
pub fn get_blind_index_repair_status(
    state: State<'_, Arc<BlindIndexRepairState>>,
) -> BlindIndexRepairStatus {
    state.snapshot()
}
