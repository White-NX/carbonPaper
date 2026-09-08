//! Cached storage analysis summaries assembled from encrypted-storage state.

use crate::resource_utils::file_in_local_appdata;
use crate::storage::disk_totals_for_path;
use crate::storage::StorageState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::State;
use walkdir::WalkDir;

const STORAGE_CACHE_TTL: Duration = Duration::from_secs(5 * 60 * 60);

/// Database and disk storage statistics (images, models, database sizes).
///
/// `disk_total_bytes` / `disk_available_bytes` describe the volume hosting the
/// data directory, not CarbonPaper's own footprint. They are `None` when the
/// hosting disk cannot be resolved, which the UI renders as "unknown" — a zero
/// would otherwise be drawn as a full or empty disk.
#[derive(Debug, Clone, Serialize)]
pub struct StorageStats {
    pub root_path: String,
    pub total_bytes: u64,
    pub models_bytes: u64,
    pub images_bytes: u64,
    pub database_bytes: u64,
    pub other_bytes: u64,
    pub disk_total_bytes: Option<u64>,
    pub disk_available_bytes: Option<u64>,
    pub cached_at_ms: u64,
}

/// Storage analysis data.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisOverview {
    pub storage: StorageStats,
}

pub struct StorageCache {
    cached_at: Instant,
    stats: StorageStats,
}

/// Shared state for the analysis subsystem (storage cache).
pub struct AnalysisState {
    pub storage_cache: Mutex<Option<StorageCache>>,
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self {
            storage_cache: Mutex::new(None),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn directory_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().map(|meta| meta.len()).ok())
        .sum()
}

fn file_size(path: &Path) -> u64 {
    path.metadata().map(|m| m.len()).unwrap_or(0)
}

fn compute_storage_stats(data_dir: PathBuf) -> Result<StorageStats, String> {
    let root_path = data_dir.to_string_lossy().to_string();

    // models 目录始终位于 %LOCALAPPDATA%/CarbonPaper/models
    let models_dir = file_in_local_appdata()
        .map(|p| p.join("models"))
        .unwrap_or_else(|| data_dir.join("models"));

    let screenshots_dir = data_dir.join("screenshots");
    let chroma_dir = data_dir.join("chroma_db");
    let ocr_db = data_dir.join("screenshots.db");

    let models_bytes = if models_dir.exists() {
        directory_size(&models_dir)
    } else {
        0
    };
    let images_bytes = if screenshots_dir.exists() {
        directory_size(&screenshots_dir)
    } else {
        0
    };
    let database_bytes = {
        let chroma_size = if chroma_dir.exists() {
            directory_size(&chroma_dir)
        } else {
            0
        };
        let ocr_size = if ocr_db.exists() {
            file_size(&ocr_db)
        } else {
            0
        };
        chroma_size + ocr_size
    };
    let data_dir_bytes = if data_dir.exists() {
        directory_size(&data_dir)
    } else {
        0
    };

    let total_bytes = data_dir_bytes.saturating_add(models_bytes);
    let accounted = models_bytes
        .saturating_add(images_bytes)
        .saturating_add(database_bytes);
    let other_bytes = total_bytes.saturating_sub(accounted);

    Ok(StorageStats {
        root_path,
        total_bytes,
        models_bytes,
        images_bytes,
        database_bytes,
        other_bytes,
        // Filled in by `attach_disk_totals` on every request so the figure
        // never ages with the directory-scan cache.
        disk_total_bytes: None,
        disk_available_bytes: None,
        cached_at_ms: now_ms(),
    })
}

/// Overwrite the disk fields of `stats` with a fresh reading for the volume
/// hosting `data_dir`.
///
/// Directory sizes are cached for `STORAGE_CACHE_TTL` because scanning them
/// walks the whole tree, but querying the volume is cheap and its result goes
/// stale as soon as anything else on the machine writes a file. So this runs on
/// every request, including cache hits. A failed lookup degrades to `None`
/// rather than failing the whole overview, since the figure is informational.
async fn attach_disk_totals(stats: &mut StorageStats, data_dir: PathBuf) {
    let totals = tokio::task::spawn_blocking(move || disk_totals_for_path(&data_dir))
        .await
        .unwrap_or(None);

    stats.disk_total_bytes = totals.map(|(total, _)| total);
    stats.disk_available_bytes = totals.map(|(_, available)| available);
}

fn get_cached_storage_stats(state: &AnalysisState) -> Option<StorageStats> {
    let cache_guard = state
        .storage_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let is_valid = cache_guard
        .as_ref()
        .map(|cache| cache.cached_at.elapsed() < STORAGE_CACHE_TTL)
        .unwrap_or(false);

    if is_valid {
        return cache_guard.as_ref().map(|c| c.stats.clone());
    }

    None
}

#[tauri::command]
pub async fn get_analysis_overview(
    credential_state: State<'_, Arc<crate::credential_manager::CredentialManagerState>>,
    state: State<'_, AnalysisState>,
    storage_state: State<'_, Arc<StorageState>>,
    force_storage: bool,
) -> Result<AnalysisOverview, String> {
    crate::commands::check_auth_required(&credential_state)?;

    // 从 StorageState 获取实际的 data_dir
    let data_dir = storage_state
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();

    // If not forcing, try to return cached stats quickly.
    if !force_storage {
        if let Some(mut stats) = get_cached_storage_stats(&state) {
            attach_disk_totals(&mut stats, data_dir).await;
            return Ok(AnalysisOverview { storage: stats });
        }
    }

    // Perform expensive storage computation on a blocking thread.
    let mut stats = tokio::task::spawn_blocking({
        let data_dir = data_dir.clone();
        move || compute_storage_stats(data_dir)
    })
    .await
    .map_err(|e| format!("Storage task join error: {}", e))??;

    // Update cache under lock.
    {
        let mut cache_guard = state
            .storage_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cache_guard = Some(StorageCache {
            cached_at: Instant::now(),
            stats: stats.clone(),
        });
    }

    attach_disk_totals(&mut stats, data_dir).await;

    Ok(AnalysisOverview { storage: stats })
}
