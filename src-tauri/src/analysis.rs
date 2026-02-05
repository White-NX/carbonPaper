use crate::monitor::MonitorState;
use crate::resource_utils::file_in_local_appdata;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};
use walkdir::WalkDir;
use sysinfo::{Pid, System};

const MEMORY_WINDOW: Duration = Duration::from_secs(30 * 60);
const STORAGE_CACHE_TTL: Duration = Duration::from_secs(5 * 60 * 60);
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
pub struct MemoryPoint {
    pub timestamp_ms: u64,
    pub rss_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStats {
    pub root_path: String,
    pub total_bytes: u64,
    pub models_bytes: u64,
    pub images_bytes: u64,
    pub database_bytes: u64,
    pub other_bytes: u64,
    pub cached_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisOverview {
    pub memory: Vec<MemoryPoint>,
    pub storage: StorageStats,
}

struct StorageCache {
    cached_at: Instant,
    stats: StorageStats,
}

pub struct AnalysisState {
    pub memory_history: Mutex<VecDeque<MemoryPoint>>,
    pub storage_cache: Mutex<Option<StorageCache>>,
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self {
            memory_history: Mutex::new(VecDeque::new()),
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

fn prune_history(history: &mut VecDeque<MemoryPoint>, cutoff_ms: u64) {
    while let Some(front) = history.front() {
        if front.timestamp_ms < cutoff_ms {
            history.pop_front();
        } else {
            break;
        }
    }
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

fn compute_storage_stats() -> Result<StorageStats, String> {
    let root = file_in_local_appdata()
        .ok_or_else(|| "Unable to resolve LOCALAPPDATA/CarbonPaper".to_string())?;

    let root_path = root.to_string_lossy().to_string();

    let data_dir = root.join("data");
    let models_dir = root.join("models");

    let screenshots_dir = data_dir.join("screenshots");
    let chroma_dir = data_dir.join("chroma_db");
    let ocr_db = data_dir.join("ocr_data.db");

    let total_bytes = if root.exists() { directory_size(&root) } else { 0 };
    let models_bytes = if models_dir.exists() { directory_size(&models_dir) } else { 0 };
    let images_bytes = if screenshots_dir.exists() { directory_size(&screenshots_dir) } else { 0 };
    let database_bytes = {
        let chroma_size = if chroma_dir.exists() { directory_size(&chroma_dir) } else { 0 };
        let ocr_size = if ocr_db.exists() { file_size(&ocr_db) } else { 0 };
        chroma_size + ocr_size
    };

    let accounted = models_bytes.saturating_add(images_bytes).saturating_add(database_bytes);
    let other_bytes = total_bytes.saturating_sub(accounted);

    Ok(StorageStats {
        root_path,
        total_bytes,
        models_bytes,
        images_bytes,
        database_bytes,
        other_bytes,
        cached_at_ms: now_ms(),
    })
}

fn get_cached_storage_stats(state: &AnalysisState, force: bool) -> Result<StorageStats, String> {
    let mut cache_guard = state
        .storage_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let is_valid = cache_guard
        .as_ref()
        .map(|cache| cache.cached_at.elapsed() < STORAGE_CACHE_TTL)
        .unwrap_or(false);

    if !force {
        if let Some(cache) = cache_guard.as_ref() {
            if is_valid {
                return Ok(cache.stats.clone());
            }
        }
    }

    let stats = compute_storage_stats()?;
    *cache_guard = Some(StorageCache {
        cached_at: Instant::now(),
        stats: stats.clone(),
    });

    Ok(stats)
}

fn sample_python_memory(pid: u32) -> Option<u64> {
    let mut system = System::new();
    system.refresh_process(Pid::from_u32(pid));
    system
        .process(Pid::from_u32(pid))
        .map(|process| process.memory() * 1024)
}

pub fn start_memory_sampler(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(MEMORY_SAMPLE_INTERVAL);

        loop {
            interval.tick().await;

            let pid_opt = {
                let monitor_state = app.state::<MonitorState>();
                let mut guard = monitor_state.process.lock().unwrap();
                if let Some(child) = guard.as_mut() {
                    if let Ok(Some(_)) = child.try_wait() {
                        *guard = None;
                        None
                    } else {
                        Some(child.id())
                    }
                } else {
                    None
                }
            };

            if let Some(pid) = pid_opt {
                if let Some(rss_bytes) = sample_python_memory(pid) {
                    let timestamp_ms = now_ms();
                    let analysis_state = app.state::<AnalysisState>();
                    let mut history = analysis_state
                        .memory_history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    history.push_back(MemoryPoint {
                        timestamp_ms,
                        rss_bytes,
                    });

                    let cutoff_ms = timestamp_ms.saturating_sub(MEMORY_WINDOW.as_millis() as u64);
                    prune_history(&mut history, cutoff_ms);
                }
            }
        }
    });
}

#[tauri::command]
pub fn get_analysis_overview(
    state: State<'_, AnalysisState>,
    force_storage: bool,
) -> Result<AnalysisOverview, String> {
    let memory = {
        let history = state
            .memory_history
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        history.iter().cloned().collect::<Vec<_>>()
    };

    let storage = get_cached_storage_stats(&state, force_storage)?;

    Ok(AnalysisOverview { memory, storage })
}
