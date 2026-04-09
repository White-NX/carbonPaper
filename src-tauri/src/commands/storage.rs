use std::sync::Arc;
use crate::credential_manager::CredentialManagerState;
use crate::storage::{self, StorageState};
use crate::monitor::{self, MonitorState};
use super::check_auth_required;

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
            Some(path) => {
                match state.read_image(&path) {
                    Ok((data, mime_type)) => {
                        Ok(serde_json::json!({
                            "status": "success",
                            "data": data,
                            "mime_type": mime_type
                        }))
                    }
                    Err(e) => Err(e)
                }
            }
            None => Err("Image not found".to_string())
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
    tokio::task::spawn_blocking(move || {
        if state.thumbnail_warmup_done.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(serde_json::json!({
                "generated": 0,
                "skipped": 0,
                "errors": 0,
                "cached": true
            }));
        }

        match state.is_thumbnail_warmup_done() {
            Ok(true) => {
                state.thumbnail_warmup_done.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(serde_json::json!({
                    "generated": 0,
                    "skipped": 0,
                    "errors": 0,
                    "cached": true
                }));
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("[Warmup] Failed to check sentinel: {}, proceeding with warmup", e);
            }
        }

        let paths = state.get_all_image_paths()?;
        let mut generated: u64 = 0;
        let mut skipped: u64 = 0;
        let mut errors: u64 = 0;

        for path in &paths {
            match state.ensure_thumbnail_cached(path) {
                Ok(true) => generated += 1,
                Ok(false) => skipped += 1,
                Err(e) => {
                    tracing::warn!("[Warmup] Error for {}: {}", path, e);
                    errors += 1;
                }
            }
        }

        if let Err(e) = state.mark_thumbnail_warmup_done() {
            tracing::warn!("[Warmup] Failed to write sentinel: {}", e);
        }
        state.thumbnail_warmup_done.store(true, std::sync::atomic::Ordering::SeqCst);

        Ok(serde_json::json!({
            "generated": generated,
            "skipped": skipped,
            "errors": errors
        }))
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
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
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
) -> Result<serde_json::Value, String> {
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
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    start_time: f64,
    end_time: f64,
) -> Result<serde_json::Value, String> {
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
        state.get_process_monthly_thumbnails(&process_name, page.unwrap_or(0), page_size.unwrap_or(60))
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
    tokio::task::spawn_blocking(move || state.soft_delete_process_month(&process_name, month.as_deref()))
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
    state: tauri::State<'_, Arc<StorageState>>,
    request: storage::SaveScreenshotRequest,
) -> Result<storage::SaveScreenshotResponse, String> {
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
    state: tauri::State<'_, Arc<StorageState>>,
    policy: serde_json::Value,
) -> Result<serde_json::Value, String> {
    state
        .save_policy(&policy)
        .map_err(|e| format!("Failed to save policy: {}", e))?;
    Ok(policy)
}

#[tauri::command]
pub async fn storage_get_policy(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    state
        .load_policy()
        .map_err(|e| format!("Failed to load policy: {}", e))
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
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
    category: String,
) -> Result<serde_json::Value, String> {
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
    monitor_state: tauri::State<'_, MonitorState>,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "command": "get_categories"
    });
    monitor::forward_command_to_python(&monitor_state, payload).await
}

#[tauri::command]
pub async fn storage_get_categories_from_db(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    state.get_categories_from_db()
}

#[tauri::command]
pub async fn storage_batch_get_categories(
    state: tauri::State<'_, Arc<StorageState>>,
    image_hashes: Vec<String>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    state.batch_get_categories_by_hash(&image_hashes)
}

#[tauri::command]
pub async fn storage_get_tasks(
    state: tauri::State<'_, Arc<StorageState>>,
    layer: Option<String>,
    start_time: Option<f64>,
    end_time: Option<f64>,
    hide_inactive: Option<bool>,
    hide_entertainment: Option<bool>,
    hide_social: Option<bool>,
) -> Result<Vec<storage::task::TaskRecord>, String> {
    state.get_tasks(layer.as_deref(), start_time, end_time, hide_inactive, hide_entertainment, hide_social)
}

#[tauri::command]
pub async fn storage_get_related_screenshots(
    state: tauri::State<'_, Arc<StorageState>>,
    screenshot_id: i64,
    limit: Option<i64>,
) -> Result<storage::task::RelatedScreenshotsResult, String> {
    state.get_related_screenshots(screenshot_id, limit.unwrap_or(8))
}

#[tauri::command]
pub async fn storage_get_task_screenshots(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<Vec<storage::task::TaskScreenshotStub>, String> {
    state.get_task_screenshots(task_id, page.unwrap_or(0), page_size.unwrap_or(50))
}

#[tauri::command]
pub async fn storage_update_task_label(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    label: String,
) -> Result<(), String> {
    state.update_task_label(task_id, &label)
}

#[tauri::command]
pub async fn storage_delete_task(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
) -> Result<(), String> {
    state.delete_task(task_id)
}

#[tauri::command]
pub async fn storage_merge_tasks(
    state: tauri::State<'_, Arc<StorageState>>,
    task_ids: Vec<i64>,
) -> Result<i64, String> {
    state.merge_tasks(&task_ids)
}

#[tauri::command]
pub async fn storage_save_clustering_results(
    state: tauri::State<'_, Arc<StorageState>>,
    tasks: Vec<storage::task::SaveTaskRequest>,
) -> Result<Vec<i64>, String> {
    state.save_clustering_results(&tasks)
}
