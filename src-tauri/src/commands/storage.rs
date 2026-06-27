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

#[cfg(test)]
mod tests {
    use super::{merge_policy_update, redact_policy_for_frontend};
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
}

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

#[tauri::command]
pub async fn storage_get_thumbnail_warmup_status(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;
    Ok(thumbnail_warmup_progress_json())
}

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

#[tauri::command]
pub async fn storage_save_screenshot(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    request: storage::SaveScreenshotRequest,
) -> Result<storage::SaveScreenshotResponse, String> {
    check_auth_required(&credential_state)?;

    state.save_screenshot(&request)
}

#[tauri::command]
pub async fn storage_compute_link_scores(
    state: tauri::State<'_, Arc<StorageState>>,
    links: Vec<storage::VisibleLink>,
) -> Result<Vec<storage::ScoredLink>, String> {
    state.compute_link_scores(&links)
}

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

#[tauri::command]
pub async fn storage_encrypt_for_chromadb(
    state: tauri::State<'_, Arc<StorageState>>,
    plaintext: String,
) -> Result<String, String> {
    state.encrypt_for_chromadb(&plaintext)
}

#[tauri::command]
pub async fn storage_decrypt_from_chromadb(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    encrypted: String,
) -> Result<String, String> {
    check_auth_required(&credential_state)?;

    state.decrypt_from_chromadb(&encrypted)
}

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

#[tauri::command]
pub async fn storage_get_categories_from_db(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    check_auth_required(&credential_state)?;

    state.get_categories_from_db()
}

#[tauri::command]
pub async fn storage_batch_get_categories(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    image_hashes: Vec<String>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    check_auth_required(&credential_state)?;

    state.batch_get_categories_by_hash(&image_hashes)
}

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

#[tauri::command]
pub async fn storage_delete_task(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
) -> Result<(), String> {
    check_auth_required(&credential_state)?;

    state.delete_task(task_id)
}

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

#[tauri::command]
pub async fn storage_merge_tasks(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    task_ids: Vec<i64>,
) -> Result<i64, String> {
    check_auth_required(&credential_state)?;

    state.merge_tasks(&task_ids)
}

#[tauri::command]
pub async fn storage_save_clustering_results(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    tasks: Vec<storage::task::SaveTaskRequest>,
) -> Result<Vec<i64>, String> {
    check_auth_required(&credential_state)?;

    state.save_clustering_results(&tasks)
}
