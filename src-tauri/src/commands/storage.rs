//! Authenticated Tauri API for screenshot storage, search, policy, and task data.
//!
//! Unless explicitly stated otherwise, commands in this module require a valid
//! credential session and serialize storage-layer DTOs directly to the frontend.
//! Screenshot and search wrappers live in `src/lib/monitor_api.js`; task and cluster
//! wrappers live in `src/lib/task_api.js`.

use super::check_auth_required;
use crate::credential_manager::CredentialManagerState;
use crate::monitor::{self, MonitorState};
use crate::storage::{self, StorageState};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default, Clone)]
struct ThumbnailWarmupProgress {
    running: bool,
    cancel_requested: bool,
    total: u64,
    processed: u64,
    generated: u64,
    skipped: u64,
    errors: u64,
}

static THUMBNAIL_WARMUP_PROGRESS: Lazy<std::sync::Mutex<ThumbnailWarmupProgress>> =
    Lazy::new(|| std::sync::Mutex::new(ThumbnailWarmupProgress::default()));
static THUMBNAIL_WARMUP_RUNNING: AtomicBool = AtomicBool::new(false);
static THUMBNAIL_WARMUP_CANCEL: AtomicBool = AtomicBool::new(false);

fn thumbnail_warmup_progress_json() -> serde_json::Value {
    let progress = THUMBNAIL_WARMUP_PROGRESS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    serde_json::json!({
        "running": progress.running,
        "cancel_requested": progress.cancel_requested,
        "total": progress.total,
        "processed": progress.processed,
        "generated": progress.generated,
        "skipped": progress.skipped,
        "errors": progress.errors,
    })
}

fn merge_policy_update(
    mut existing: serde_json::Value,
    update: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let existing_obj = existing
        .as_object_mut()
        .ok_or_else(|| "Existing policy is not a valid JSON object".to_string())?;
    let update_obj = update
        .as_object()
        .ok_or_else(|| "Policy update is not a valid JSON object".to_string())?;

    for (key, value) in update_obj {
        existing_obj.insert(key.clone(), value.clone());
    }

    Ok(existing)
}

fn redact_policy_for_frontend(policy: &mut serde_json::Value) {
    if let Some(obj) = policy.as_object_mut() {
        obj.remove("mcp_token_encrypted");
    }
}

fn value_as_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    value.and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
    })
}

fn compose_index_health_response(
    storage_stats: storage::IndexStorageStats,
    monitor_health: Result<serde_json::Value, String>,
) -> serde_json::Value {
    let (monitor_available, python, transport_error) = match monitor_health {
        Ok(value) => (true, Some(value), None),
        Err(error) => (false, None, Some(error)),
    };

    let vector_stats = python
        .as_ref()
        .and_then(|value| value.get("stats"))
        .and_then(|stats| stats.get("vector_stats"))
        .cloned();
    let postprocess = python
        .as_ref()
        .and_then(|value| value.get("postprocess"))
        .cloned();
    let storage_ipc = python
        .as_ref()
        .and_then(|value| value.get("storage_ipc"))
        .cloned();
    let worker_storage_ipc = python
        .as_ref()
        .and_then(|value| value.get("worker_storage_ipc"))
        .cloned();
    let actual_clip_image_rows = vector_stats
        .as_ref()
        .and_then(|stats| value_as_i64(stats.get("count")));
    let pending_retry_queue_count = postprocess
        .as_ref()
        .and_then(|stats| value_as_i64(stats.get("vector_retry_backlog_count")));
    let last_indexing_error = postprocess
        .as_ref()
        .and_then(|stats| stats.get("last_indexing_error"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let last_indexing_error_at = postprocess
        .as_ref()
        .and_then(|stats| stats.get("last_indexing_error_at"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let monitor_error = transport_error
        .map(serde_json::Value::String)
        .or_else(|| {
            python
                .as_ref()
                .and_then(|value| value.get("error"))
                .and_then(|value| value.as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
        })
        .unwrap_or(serde_json::Value::Null);

    // Count-level CLIP image-index check (Milestone 1): compares the
    // SQLite-derived eligibility estimate against the live Python Chroma
    // `screenshots` collection count. This is not a per-row or vector-quality
    // check; equal counts can still hide missing/orphan swaps.
    let clip_image_index = match actual_clip_image_rows {
        Some(actual) => {
            let expected = storage_stats.expected_clip_image_rows;
            let missing = (expected - actual).max(0);
            let orphaned = (actual - expected).max(0);
            serde_json::json!({
                "expected_eligible_images": expected,
                "actual_rows": actual,
                "missing_lower_bound": missing,
                "orphaned_lower_bound": orphaned,
                "assessment": if missing == 0 && orphaned == 0 { "count_match" } else { "count_mismatch" },
                "eligibility": "active screenshot with active OCR row",
                "eligibility_is_proxy": true,
            })
        }
        None => serde_json::json!({
            "expected_eligible_images": storage_stats.expected_clip_image_rows,
            "actual_rows": serde_json::Value::Null,
            "assessment": "unknown",
            "eligibility": "active screenshot with active OCR row",
            "eligibility_is_proxy": true,
        }),
    };

    serde_json::json!({
        "status": "success",
        "screenshots_count": storage_stats.screenshots_count,
        "ocr_rows_count": storage_stats.ocr_rows_count,
        "vector_rows_count": actual_clip_image_rows,
        "clip_image_index": clip_image_index,
        "pending_retry_queue_count": pending_retry_queue_count,
        "smart_cluster_pending_count": storage_stats.smart_cluster_pending_count,
        "delete_queue": storage_stats.delete_queue,
        "last_indexing_error": last_indexing_error,
        "last_indexing_error_at": last_indexing_error_at,
        "monitor_available": monitor_available,
        "monitor_error": monitor_error,
        "worker_started": python
            .as_ref()
            .and_then(|value| value.get("worker_started"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "storage_ipc": storage_ipc.unwrap_or(serde_json::Value::Null),
        "worker_storage_ipc": worker_storage_ipc.unwrap_or(serde_json::Value::Null),
        "vector_status": vector_stats.unwrap_or(serde_json::Value::Null),
        "postprocess": postprocess.unwrap_or(serde_json::Value::Null),
        "python": python.unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::{compose_index_health_response, merge_policy_update, redact_policy_for_frontend};
    use crate::storage::{DeleteQueueStatus, IndexStorageStats};
    use serde_json::json;

    #[test]
    fn merge_policy_update_preserves_unmentioned_mcp_fields() {
        let existing = json!({
            "mcp_enabled": true,
            "mcp_port": 23816,
            "mcp_token_encrypted": "secret",
            "sensitive_filter": { "enabled": false },
            "storage_limit": "20"
        });
        let update = json!({
            "storage_limit": "10",
            "retention_period": "6months"
        });

        let merged = merge_policy_update(existing, update).unwrap();

        assert_eq!(merged["storage_limit"], "10");
        assert_eq!(merged["retention_period"], "6months");
        assert_eq!(merged["mcp_enabled"], true);
        assert_eq!(merged["mcp_port"], 23816);
        assert_eq!(merged["mcp_token_encrypted"], "secret");
        assert_eq!(merged["sensitive_filter"]["enabled"], false);
    }

    #[test]
    fn merge_policy_update_rejects_non_object_update() {
        let err = merge_policy_update(json!({}), json!(null)).unwrap_err();
        assert!(err.contains("Policy update"));
    }

    #[test]
    fn redact_policy_for_frontend_removes_encrypted_mcp_token() {
        let mut policy = json!({
            "mcp_enabled": true,
            "mcp_token_encrypted": "secret"
        });

        redact_policy_for_frontend(&mut policy);

        assert_eq!(policy["mcp_enabled"], true);
        assert!(policy.get("mcp_token_encrypted").is_none());
    }

    #[test]
    fn compose_index_health_response_merges_storage_and_monitor_counts() {
        let storage_stats = IndexStorageStats {
            screenshots_count: 10,
            ocr_rows_count: 12,
            expected_clip_image_rows: 9,
            smart_cluster_pending_count: 3,
            delete_queue: DeleteQueueStatus {
                pending_screenshots: 1,
                pending_ocr: 2,
                running: false,
            },
        };
        let monitor_health = Ok(json!({
            "status": "success",
            "worker_started": true,
            "stats": { "vector_stats": { "count": 9 } },
            "postprocess": {
                "vector_retry_backlog_count": 4,
                "last_indexing_error": "chroma down",
                "last_indexing_error_at": 123.0
            }
        }));

        let response = compose_index_health_response(storage_stats, monitor_health);

        assert_eq!(response["screenshots_count"], 10);
        assert_eq!(response["ocr_rows_count"], 12);
        assert_eq!(response["vector_rows_count"], 9);
        assert_eq!(response["clip_image_index"]["actual_rows"], 9);
        assert_eq!(response["clip_image_index"]["expected_eligible_images"], 9);
        assert_eq!(response["clip_image_index"]["assessment"], "count_match");
        assert_eq!(response["clip_image_index"]["missing_lower_bound"], 0);
        assert_eq!(response["pending_retry_queue_count"], 4);
        assert_eq!(response["monitor_available"], true);
        assert_eq!(response["last_indexing_error"], "chroma down");
    }

    #[test]
    fn compose_index_health_response_reports_clip_image_count_mismatch() {
        let storage_stats = IndexStorageStats {
            screenshots_count: 20,
            ocr_rows_count: 40,
            expected_clip_image_rows: 15,
            smart_cluster_pending_count: 0,
            delete_queue: DeleteQueueStatus {
                pending_screenshots: 0,
                pending_ocr: 0,
                running: false,
            },
        };
        let monitor_health = Ok(json!({
            "status": "success",
            "worker_started": true,
            "stats": { "vector_stats": { "count": 9 } },
            "postprocess": {}
        }));

        let response = compose_index_health_response(storage_stats, monitor_health);

        assert_eq!(response["clip_image_index"]["assessment"], "count_mismatch");
        assert_eq!(response["clip_image_index"]["expected_eligible_images"], 15);
        assert_eq!(response["clip_image_index"]["actual_rows"], 9);
        assert_eq!(response["clip_image_index"]["missing_lower_bound"], 6);
        assert_eq!(response["clip_image_index"]["orphaned_lower_bound"], 0);
    }

    #[test]
    fn compose_index_health_response_keeps_storage_counts_when_monitor_unavailable() {
        let storage_stats = IndexStorageStats {
            screenshots_count: 10,
            ocr_rows_count: 12,
            expected_clip_image_rows: 9,
            smart_cluster_pending_count: 3,
            delete_queue: DeleteQueueStatus {
                pending_screenshots: 1,
                pending_ocr: 2,
                running: false,
            },
        };

        let response =
            compose_index_health_response(storage_stats, Err("Monitor not started".to_string()));

        assert_eq!(response["screenshots_count"], 10);
        assert_eq!(response["vector_rows_count"], serde_json::Value::Null);
        assert_eq!(response["clip_image_index"]["assessment"], "unknown");
        assert_eq!(response["clip_image_index"]["expected_eligible_images"], 9);
        assert_eq!(
            response["clip_image_index"]["actual_rows"],
            serde_json::Value::Null
        );
        assert_eq!(response["monitor_available"], false);
        assert_eq!(response["monitor_error"], "Monitor not started");
    }
}

/// Returns timeline records between millisecond timestamps `start_time` and `end_time`.
///
/// Authentication: required. `max_records` caps the result; returns an array of
/// `ScreenshotRecord` objects. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_timeline(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    start_time: f64,
    end_time: f64,
    max_records: Option<i64>,
) -> Result<Vec<storage::ScreenshotRecord>, String> {
    check_auth_required(&credential_state)?;

    let start_ts = if start_time > 10_000_000_000.0 {
        start_time / 1000.0
    } else {
        start_time
    };
    let end_ts = if end_time > 10_000_000_000.0 {
        end_time / 1000.0
    } else {
        end_time
    };

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        state.get_screenshots_by_time_range_limited(start_ts, end_ts, max_records.or(Some(500)))
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Aggregates screenshot counts into `bucket_ms` timeline buckets.
///
/// Authentication: required. Returns an array of `DensityBucket` objects for the
/// requested millisecond range. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_timeline_density(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    start_time: f64,
    end_time: f64,
    bucket_ms: i64,
) -> Result<Vec<storage::DensityBucket>, String> {
    check_auth_required(&credential_state)?;

    let start_ts = if start_time > 10_000_000_000.0 {
        start_time / 1000.0
    } else {
        start_time
    };
    let end_ts = if end_time > 10_000_000_000.0 {
        end_time / 1000.0
    } else {
        end_time
    };

    let bucket_seconds = (bucket_ms / 1000).max(1);

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        state.get_screenshot_density(start_ts, end_ts, bucket_seconds)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Searches OCR records with pagination, fuzzy matching, process, time, and category filters.
///
/// Authentication: required. Returns an array of `SearchResult` objects; optional
/// filters are omitted as JSON `null`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_search(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    query: String,
    limit: Option<i32>,
    offset: Option<i32>,
    fuzzy: Option<bool>,
    process_names: Option<Vec<String>>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    categories: Option<Vec<String>>,
) -> Result<Vec<storage::SearchResult>, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    let limit = limit.unwrap_or(20);
    let offset = offset.unwrap_or(0);
    let fuzzy = fuzzy.unwrap_or(true);
    tokio::task::spawn_blocking(move || {
        state.search_text(
            &query,
            limit,
            offset,
            fuzzy,
            process_names,
            start_time,
            end_time,
            categories,
        )
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Loads and decrypts a full screenshot selected by `id` or legacy `path`.
///
/// Authentication: required. Exactly one selector should be supplied. Returns a status
/// object containing image data and metadata. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_image(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let image_path = if let Some(id) = id {
            let record = state.get_screenshot_by_id(id)?;
            record.map(|r| r.image_path)
        } else {
            path
        };

        match image_path {
            Some(path) => match state.read_image(&path) {
                Ok((data, mime_type)) => Ok(serde_json::json!({
                    "status": "success",
                    "data": data,
                    "mime_type": mime_type
                })),
                Err(e) => Err(e),
            },
            None => Err("Image not found".to_string()),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Loads or creates a thumbnail selected by `id` or legacy `path`.
///
/// Authentication: required. Returns a status object containing the encoded thumbnail.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_thumbnail(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let image_path = if let Some(id) = id {
            let record = state.get_screenshot_by_id(id)?;
            record.map(|r| r.image_path)
        } else {
            path
        };

        match image_path {
            Some(path) => match state.read_thumbnail(&path) {
                Ok((data, mime_type)) => Ok(serde_json::json!({
                    "status": "success",
                    "data": data,
                    "mime_type": mime_type
                })),
                Err(e) => Err(e),
            },
            None => Err("Image not found".to_string()),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Loads thumbnails for multiple screenshot `ids` in one IPC call.
///
/// Authentication: required. Returns an object keyed by screenshot ID with per-item
/// thumbnail results. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_batch_get_thumbnails(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    ids: Vec<i64>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let results_map = state.batch_read_thumbnails_by_ids(&ids);

        let mut results = serde_json::Map::new();
        for (id_str, result) in results_map {
            let entry = match result {
                Ok((data, mime_type)) => serde_json::json!({
                    "status": "success",
                    "data": data,
                    "mime_type": mime_type
                }),
                Err(e) => serde_json::json!({
                    "status": "error",
                    "error": e
                }),
            };
            results.insert(id_str, entry);
        }

        Ok(serde_json::json!({ "results": results }))
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Starts background generation of missing thumbnails.
///
/// Authentication: required. Returns `{ "started", "running", "progress" }`; repeated
/// calls report the active or completed state. Frontend: `hooks/useStartupWizards.js`.
#[tauri::command]
pub async fn storage_warmup_thumbnails(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    if state.thumbnail_warmup_done.load(Ordering::SeqCst) {
        return Ok(serde_json::json!({
            "started": false,
            "cached": true,
            "progress": thumbnail_warmup_progress_json()
        }));
    }

    match state.is_thumbnail_warmup_done() {
        Ok(true) => {
            state.thumbnail_warmup_done.store(true, Ordering::SeqCst);
            return Ok(serde_json::json!({
                "started": false,
                "cached": true,
                "progress": thumbnail_warmup_progress_json()
            }));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                "[Warmup] Failed to check sentinel: {}, proceeding with warmup",
                e
            );
        }
    }

    if THUMBNAIL_WARMUP_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(serde_json::json!({
            "started": false,
            "running": true,
            "progress": thumbnail_warmup_progress_json()
        }));
    }

    THUMBNAIL_WARMUP_CANCEL.store(false, Ordering::SeqCst);
    {
        let mut progress = THUMBNAIL_WARMUP_PROGRESS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *progress = ThumbnailWarmupProgress {
            running: true,
            ..ThumbnailWarmupProgress::default()
        };
    }

    tokio::task::spawn_blocking(move || {
        let paths = match state.get_all_image_paths() {
            Ok(paths) => paths,
            Err(e) => {
                tracing::warn!("[Warmup] Failed to list image paths: {}", e);
                let mut progress = THUMBNAIL_WARMUP_PROGRESS
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                progress.running = false;
                progress.errors += 1;
                THUMBNAIL_WARMUP_RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        };

        {
            let mut progress = THUMBNAIL_WARMUP_PROGRESS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            progress.total = paths.len() as u64;
        }

        const MAX_BATCH_ITEMS: usize = 32;
        const MAX_BATCH_MS: u128 = 200;
        const BATCH_PAUSE_MS: u64 = 250;

        let mut batch_items = 0usize;
        let mut batch_started = Instant::now();

        for path in &paths {
            if THUMBNAIL_WARMUP_CANCEL.load(Ordering::SeqCst) {
                tracing::info!("[Warmup] Thumbnail warmup cancelled");
                break;
            }

            let mut generated_delta = 0;
            let mut skipped_delta = 0;
            let mut errors_delta = 0;
            match state.ensure_thumbnail_cached(path) {
                Ok(true) => generated_delta = 1,
                Ok(false) => skipped_delta = 1,
                Err(e) => {
                    tracing::warn!("[Warmup] Error for {}: {}", path, e);
                    errors_delta = 1;
                }
            }

            {
                let mut progress = THUMBNAIL_WARMUP_PROGRESS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                progress.processed += 1;
                progress.generated += generated_delta;
                progress.skipped += skipped_delta;
                progress.errors += errors_delta;
            }

            batch_items += 1;
            if batch_items >= MAX_BATCH_ITEMS || batch_started.elapsed().as_millis() >= MAX_BATCH_MS
            {
                std::thread::sleep(Duration::from_millis(BATCH_PAUSE_MS));
                batch_items = 0;
                batch_started = Instant::now();
            }
        }

        let cancelled = THUMBNAIL_WARMUP_CANCEL.load(Ordering::SeqCst);
        if !cancelled {
            if let Err(e) = state.mark_thumbnail_warmup_done() {
                tracing::warn!("[Warmup] Failed to write sentinel: {}", e);
            }
            state.thumbnail_warmup_done.store(true, Ordering::SeqCst);
        }

        {
            let mut progress = THUMBNAIL_WARMUP_PROGRESS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            progress.running = false;
            progress.cancel_requested = cancelled;
        }
        THUMBNAIL_WARMUP_RUNNING.store(false, Ordering::SeqCst);
        tracing::info!(
            "[Warmup] Thumbnail warmup finished cancelled={} progress={}",
            cancelled,
            thumbnail_warmup_progress_json()
        );
    });

    Ok(serde_json::json!({
        "started": true,
        "running": true,
        "progress": thumbnail_warmup_progress_json()
    }))
}

/// Returns the current thumbnail warmup progress object.
///
/// Authentication: required. The object includes running, totals, processed counts,
/// failures, and cancellation state.
#[tauri::command]
pub async fn storage_get_thumbnail_warmup_status(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;
    Ok(thumbnail_warmup_progress_json())
}

/// Requests cancellation of thumbnail warmup and returns its updated progress object.
///
/// Authentication: required. Cancellation is cooperative.
#[tauri::command]
pub fn storage_cancel_thumbnail_warmup(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    THUMBNAIL_WARMUP_CANCEL.store(true, Ordering::SeqCst);
    {
        let mut progress = THUMBNAIL_WARMUP_PROGRESS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        progress.cancel_requested = true;
    }
    Ok(thumbnail_warmup_progress_json())
}

/// Returns a screenshot record and all associated OCR rows selected by `id` or `path`.
///
/// Authentication: required. Returns `{ "status": "success" | "not_found",
/// "record", "ocr_results" }`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_screenshot_details(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let record = if let Some(id) = id {
            state.get_screenshot_by_id(id)?
        } else if let Some(ref p) = path {
            state.get_screenshot_by_image_path(p)?
        } else {
            return Err("Either id or path must be provided".into());
        };

        match &record {
            Some(r) => {
                let ocr_results = state.get_screenshot_ocr_results(r.id)?;
                Ok(serde_json::json!({
                    "status": "success",
                    "record": record,
                    "ocr_results": ocr_results
                }))
            }
            None => Ok(serde_json::json!({
                "status": "not_found",
                "record": null,
                "ocr_results": []
            })),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Permanently deletes one screenshot and asks the vector index to remove its embedding.
///
/// Authentication: required. `screenshot_id` identifies the record. Returns
/// `{ "status": "success", "deleted": boolean, "vector_deleted": number | null }`.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_delete_screenshot(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let image_hash = match state.get_screenshot_by_id(screenshot_id)? {
        Some(record) => Some(record.image_hash),
        None => None,
    };

    let deleted = state.delete_screenshot(screenshot_id)?;
    let mut vector_deleted: Option<i64> = None;

    if deleted {
        if let Some(hash) = image_hash {
            let payload = serde_json::json!({
                "command": "delete_screenshot",
                "screenshot_id": screenshot_id,
                "image_hash": hash
            });
            match monitor::forward_command_to_python(&monitor_state, payload).await {
                Ok(resp) => {
                    vector_deleted = resp.get("vector_deleted").and_then(|v| v.as_i64());
                }
                Err(e) => {
                    tracing::error!("Vector delete failed: {}", e);
                }
            }
        }
    }
    Ok(serde_json::json!({
        "status": "success",
        "deleted": deleted,
        "vector_deleted": vector_deleted
    }))
}

/// Permanently deletes screenshots in the requested millisecond time range.
///
/// Authentication: required. Returns `{ "status": "success", "deleted_count": number,
/// "vector_deleted": number | null }`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_delete_by_time_range(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    start_time: f64,
    end_time: f64,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let start_ts = start_time / 1000.0;
    let end_ts = end_time / 1000.0;
    let image_hashes = match state.get_screenshots_by_time_range(start_ts, end_ts) {
        Ok(records) => records
            .into_iter()
            .map(|r| r.image_hash)
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!("Failed to load hashes: {}", e);
            Vec::new()
        }
    };

    let deleted_count = state.delete_screenshots_by_time_range(start_time, end_time)?;
    let mut vector_deleted: Option<i64> = None;

    if !image_hashes.is_empty() {
        let payload = serde_json::json!({
            "command": "delete_by_time_range",
            "start_time": start_time,
            "end_time": end_time,
            "image_hashes": image_hashes
        });

        match monitor::forward_command_to_python(&monitor_state, payload).await {
            Ok(resp) => {
                vector_deleted = resp.get("vector_deleted").and_then(|v| v.as_i64());
            }
            Err(e) => {
                tracing::error!("Vector delete failed: {}", e);
            }
        }
    }
    Ok(serde_json::json!({
        "status": "success",
        "deleted_count": deleted_count,
        "vector_deleted": vector_deleted
    }))
}

/// Lists distinct process names and their screenshot counts.
///
/// Authentication: required. Returns `[{ "process_name": string, "count": number }]`.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_list_processes(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<serde_json::Value>, String> {
    check_auth_required(&credential_state)?;

    let processes = state.list_distinct_processes()?;
    Ok(processes
        .into_iter()
        .map(|(name, count)| {
            serde_json::json!({
                "process_name": name,
                "count": count
            })
        })
        .collect())
}

/// Returns per-process storage usage statistics.
///
/// Authentication: required. Returns an array of `ProcessStorageStat` objects.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_process_stats(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<storage::ProcessStorageStat>, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.get_process_stats())
        .await
        .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Returns a paginated month/thumbnail summary for `process_name`.
///
/// Authentication: required. `page` defaults to 0 and `page_size` to 60; returns a
/// `ProcessMonthlyThumbnailPage`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_process_monthly_thumbnails(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    process_name: String,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<storage::ProcessMonthlyThumbnailPage, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        state.get_process_monthly_thumbnails(
            &process_name,
            page.unwrap_or(0),
            page_size.unwrap_or(60),
        )
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Queues soft deletion for all records from a process and optional `month`.
///
/// Authentication: required. Returns `SoftDeleteResult`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_soft_delete(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    process_name: String,
    month: Option<String>,
) -> Result<storage::SoftDeleteResult, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        state.soft_delete_process_month(&process_name, month.as_deref())
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Queues soft deletion for the supplied screenshot IDs.
///
/// Authentication: required. Returns `SoftDeleteScreenshotsResult`.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_soft_delete_screenshots(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    screenshot_ids: Vec<i64>,
) -> Result<storage::SoftDeleteScreenshotsResult, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.soft_delete_screenshots(&screenshot_ids))
        .await
        .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Returns pending and completed soft-delete queue counts.
///
/// Authentication: required. Returns `DeleteQueueStatus`.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_delete_queue_status(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<storage::DeleteQueueStatus, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || state.get_delete_queue_status())
        .await
        .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Combines encrypted-storage index statistics with Python vector-index health.
///
/// Authentication: required. `refresh_vector` requests a live vector recount. Returns a
/// JSON health object for both stores. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_index_health(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    refresh_vector: Option<bool>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let storage_state = state.inner().clone();
    let storage_stats =
        tokio::task::spawn_blocking(move || storage_state.get_index_storage_stats())
            .await
            .map_err(|e| format!("Task join error: {:?}", e))??;

    let monitor_health = monitor::forward_command_to_python(
        &monitor_state,
        serde_json::json!({
            "command": "index_health",
            "refresh": refresh_vector.unwrap_or(false),
        }),
    )
    .await;

    Ok(compose_index_health_response(storage_stats, monitor_health))
}

/// Retries failed vector indexing through the monitor service.
///
/// Authentication: required. `limit` defaults to 32 and is clamped to 1..=256; returns
/// the monitor's retry result object. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_retry_vector_indexing(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    monitor::forward_command_to_python(
        &monitor_state,
        serde_json::json!({
            "command": "retry_vector_indexing",
            "limit": limit.unwrap_or(32).clamp(1, 256),
        }),
    )
    .await
}

/// Persists a screenshot and its metadata from a trusted native producer.
///
/// Authentication: required. `request` is `SaveScreenshotRequest`; returns
/// `SaveScreenshotResponse`. This command is registered for internal/native callers.
#[tauri::command]
pub async fn storage_save_screenshot(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    request: storage::SaveScreenshotRequest,
) -> Result<storage::SaveScreenshotResponse, String> {
    check_auth_required(&credential_state)?;

    state.save_screenshot(&request)
}

/// Scores visible links using aggregate storage statistics.
///
/// Authentication: not required; input contains link features rather than stored user
/// records. Returns an array of `ScoredLink` objects for the browser integration.
#[tauri::command]
pub async fn storage_compute_link_scores(
    state: tauri::State<'_, Arc<StorageState>>,
    links: Vec<storage::VisibleLink>,
) -> Result<Vec<storage::ScoredLink>, String> {
    state.compute_link_scores(&links)
}

/// Returns the storage encryption public key as standard Base64.
///
/// Authentication: not required because the public key cannot decrypt data. Returns a
/// JSON string for trusted native/browser producers.
#[tauri::command]
pub async fn storage_get_public_key(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    let key = state.get_public_key()?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &key,
    ))
}

/// Merges and persists a partial storage policy update.
///
/// Authentication: required. `policy` must be a JSON object. Returns the merged policy
/// with encrypted secrets redacted. Frontend: settings controllers using `invoke`.
#[tauri::command]
pub async fn storage_set_policy(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    policy: serde_json::Value,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let existing = state
        .load_policy()
        .map_err(|e| format!("Failed to load policy: {}", e))?;
    let merged = merge_policy_update(existing, policy)?;

    state
        .save_policy(&merged)
        .map_err(|e| format!("Failed to save policy: {}", e))?;
    let mut response = merged;
    redact_policy_for_frontend(&mut response);
    Ok(response)
}

/// Returns the current storage policy with encrypted secrets redacted.
///
/// Authentication: required. Returns a JSON object consumed by frontend settings.
#[tauri::command]
pub async fn storage_get_policy(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let mut policy = state
        .load_policy()
        .map_err(|e| format!("Failed to load policy: {}", e))?;
    redact_policy_for_frontend(&mut policy);
    Ok(policy)
}

/// Encrypts plaintext for the legacy ChromaDB bridge.
///
/// Authentication: not required because only the public-key encryption path is used.
/// Returns an encoded ciphertext string for the monitor process.
#[tauri::command]
pub async fn storage_encrypt_for_chromadb(
    state: tauri::State<'_, Arc<StorageState>>,
    plaintext: String,
) -> Result<String, String> {
    state.encrypt_for_chromadb(&plaintext)
}

/// Decrypts a legacy ChromaDB ciphertext.
///
/// Authentication: required because this uses private key material. Returns the
/// plaintext string to the trusted caller.
#[tauri::command]
pub async fn storage_decrypt_from_chromadb(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    encrypted: String,
) -> Result<String, String> {
    check_auth_required(&credential_state)?;

    state.decrypt_from_chromadb(&encrypted)
}

/// Updates a screenshot category and forwards a learning anchor to the monitor.
///
/// Authentication: required. Returns `{ "status": "success", "updated": boolean }`.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_update_category(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
    category: String,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let old_category = state
        .get_screenshot_by_id(screenshot_id)
        .ok()
        .flatten()
        .and_then(|r| r.category.clone());

    let updated = state.update_screenshot_category(screenshot_id, &category, Some(1.0))?;

    if updated {
        if let Ok(Some(record)) = state.get_screenshot_by_id(screenshot_id) {
            let title = record.window_title.clone().unwrap_or_default();
            let process_name = record.process_name.clone().unwrap_or_default();

            let ocr_text = match state.get_screenshot_ocr_results(screenshot_id) {
                Ok(results) => {
                    let texts: Vec<String> = results.iter().map(|r| r.text.clone()).collect();
                    texts.join(" ")
                }
                Err(e) => {
                    tracing::warn!("Failed to get OCR results for learning: {}", e);
                    String::new()
                }
            };

            let payload = serde_json::json!({
                "command": "add_anchor",
                "category": category,
                "title": title,
                "ocr_text": ocr_text,
                "old_category": old_category,
                "process_name": process_name
            });
            if let Err(e) = monitor::forward_command_to_python(&monitor_state, payload).await {
                tracing::error!("Failed to forward add_anchor command to python: {}", e);
            }
        }
    }

    Ok(serde_json::json!({
        "status": "success",
        "updated": updated
    }))
}

/// Returns monitor-defined category metadata.
///
/// Authentication: required. Returns the monitor's `get_categories` JSON object.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_categories(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    monitor_state: tauri::State<'_, MonitorState>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let payload = serde_json::json!({
        "command": "get_categories"
    });
    monitor::forward_command_to_python(&monitor_state, payload).await
}

/// Lists distinct categories currently stored in SQLite.
///
/// Authentication: required. Returns an array of strings.
/// Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_get_categories_from_db(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    check_auth_required(&credential_state)?;

    state.get_categories_from_db()
}

/// Looks up categories for multiple image hashes.
///
/// Authentication: required. Returns a JSON object mapping each hash to a category or
/// `null`. Frontend: `lib/monitor_api.js`.
#[tauri::command]
pub async fn storage_batch_get_categories(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    image_hashes: Vec<String>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    check_auth_required(&credential_state)?;

    state.batch_get_categories_by_hash(&image_hashes)
}

/// Lists task clusters with optional layer, time, and visibility filters.
///
/// Authentication: required. Returns an array of `TaskRecord` objects.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_get_tasks(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    layer: Option<String>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    hide_inactive: Option<bool>,
    hide_entertainment: Option<bool>,
    hide_social: Option<bool>,
) -> Result<Vec<storage::task::TaskRecord>, String> {
    check_auth_required(&credential_state)?;

    state.get_tasks(
        layer.as_deref(),
        start_time,
        end_time,
        hide_inactive,
        hide_entertainment,
        hide_social,
    )
}

/// Finds screenshots related to `screenshot_id` by task/link evidence.
///
/// Authentication: required. `limit` defaults to 8; returns `RelatedScreenshotsResult`.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_get_related_screenshots(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    screenshot_id: i64,
    limit: Option<i64>,
) -> Result<storage::task::RelatedScreenshotsResult, String> {
    check_auth_required(&credential_state)?;

    state.get_related_screenshots(screenshot_id, limit.unwrap_or(8))
}

/// Returns a page of screenshot stubs assigned to `task_id`.
///
/// Authentication: required. `page` defaults to 0 and `page_size` to 50; returns an
/// array of `TaskScreenshotStub`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_get_task_screenshots(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<Vec<storage::task::TaskScreenshotStub>, String> {
    check_auth_required(&credential_state)?;

    state.get_task_screenshots(task_id, page.unwrap_or(0), page_size.unwrap_or(50))
}

/// Replaces the user-visible label for `task_id`.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_update_task_label(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    label: String,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;

    state.update_task_label(task_id, &label)
}

/// Deletes a task and its assignments.
///
/// Authentication: required. Returns JSON `null`. Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_delete_task(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;

    state.delete_task(task_id)
}

/// Removes one screenshot assignment from a task.
///
/// Authentication: required. Returns the remaining assignment count as a JSON integer.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_remove_task_screenshot(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    screenshot_id: i64,
) -> Result<i64, String> {
    check_auth_required(&credential_state)?;

    state.remove_task_screenshot(task_id, screenshot_id)
}

/// Merges `task_ids` and returns the surviving task ID.
///
/// Authentication: required. At least two valid IDs are expected.
/// Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_merge_tasks(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_ids: Vec<i64>,
) -> Result<i64, String> {
    check_auth_required(&credential_state)?;

    state.merge_tasks(&task_ids)
}

/// Persists clustering output supplied by the monitor pipeline.
///
/// Authentication: required. `tasks` contains `SaveTaskRequest` objects; returns the
/// saved task IDs. Frontend: `lib/task_api.js`.
#[tauri::command]
pub async fn storage_save_clustering_results(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    tasks: Vec<storage::task::SaveTaskRequest>,
) -> Result<Vec<i64>, String> {
    check_auth_required(&credential_state)?;

    state.save_clustering_results(&tasks)
}
