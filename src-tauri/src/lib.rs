mod analysis;
mod autostart;
mod capture;
mod credential_manager;
pub mod error;
mod error_window;
mod logging;
mod mcp_server;
mod model_management;
mod monitor;
mod native_messaging;
mod python;
mod registry_config;
mod resource_utils;
mod reverse_ipc;
mod sensitive_filter;
mod storage;
mod updater;

use analysis::AnalysisState;
use autostart::{get_autostart_status, set_autostart};
use capture::CaptureState;
use credential_manager::CredentialManagerState;
use monitor::MonitorState;
use sensitive_filter::SensitiveFilterState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage::StorageState;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Emitter;
use tauri::Manager;
use window_vibrancy::apply_acrylic;

const MENU_ID_OPEN: &str = "open";

fn policy_as_object_mut(policy: &mut serde_json::Value) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    policy.as_object_mut().ok_or_else(|| "Policy is not a valid JSON object".to_string())
}

#[tauri::command]
fn frontend_log(level: String, message: String) {
    match level.as_str() {
        "info" => tracing::info!("Frontend: {}", message),
        "warn" => tracing::warn!("Frontend: {}", message),
        "error" => tracing::error!("Frontend: {}", message),
        "debug" => tracing::debug!("Frontend: {}", message),
        "trace" => tracing::trace!("Frontend: {}", message),
        _ => tracing::info!("Frontend: {}", message),
    }
}

#[tauri::command]
fn get_log_dir() -> String {
    let data_dir = get_data_dir();
    data_dir.join("logs").to_string_lossy().to_string()
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    tauri::process::restart(&app.env());
}

#[tauri::command]
async fn trigger_test_error() {
    // Spawn a blocking task so the panic happens inside Tokio's executor,
    // which wraps it in catch_unwind. Our global panic hook fires first
    // (showing the error overlay), then Tokio catches the unwind — the
    // thread and process survive.
    let _ = tokio::task::spawn_blocking(|| {
        panic!("This is a test panic triggered from Rust!");
    }).await;
}

#[tauri::command]
fn exit_app() {
    std::process::exit(1);
}

const MENU_ID_PAUSE: &str = "pause";
const MENU_ID_RESUME: &str = "resume";
const MENU_ID_RESTART: &str = "restart";
const MENU_ID_QUIT: &str = "quit";

static IS_UPDATING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
fn set_updating_flag(updating: bool) {
    IS_UPDATING.store(updating, Ordering::Relaxed);
}
fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let app_handle = app.handle().clone();

    let menu = MenuBuilder::new(&app_handle)
        .item(&MenuItemBuilder::with_id(MENU_ID_OPEN, "打开界面").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_PAUSE, "暂停截图").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_RESUME, "恢复截图").build(&app_handle)?)
        .item(&MenuItemBuilder::with_id(MENU_ID_RESTART, "重启截图").build(&app_handle)?)
        .separator()
        .item(&MenuItemBuilder::with_id(MENU_ID_QUIT, "彻底退出").build(&app_handle)?)
        .build()?;

    let mut tray_builder = TrayIconBuilder::new().menu(&menu);
    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    }
    tray_builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_ID_OPEN => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            MENU_ID_PAUSE => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::pause_monitor(state, cs).await;
                });
            }
            MENU_ID_RESUME => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::resume_monitor(state, cs).await;
                });
            }
            MENU_ID_RESTART => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::stop_monitor(state, cs).await;
                    let start_state = app_handle.state::<MonitorState>();
                    let _ = monitor::start_monitor(start_state, app_handle.clone()).await;
                });
            }
            MENU_ID_QUIT => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app_handle.state::<MonitorState>();
                    let cs = app_handle.state::<Arc<CaptureState>>();
                    let _ = monitor::stop_monitor(state, cs).await;
                    app_handle.exit(0);
                });
            }
            _ => {}
        })
        .build(&app_handle)?;

    Ok(())
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn close_process() {
    std::process::exit(0);
}

// ==================== 存储相关命令 ====================

/// Checks whether the current session requires re-authentication.
fn check_auth_required(credential_state: &CredentialManagerState) -> Result<(), String> {
    if !credential_state.is_session_valid() {
        return Err("AUTH_REQUIRED".to_string());
    }
    Ok(())
}

/// Retrieves a paginated timeline of screenshots within a time range.
#[tauri::command]
async fn storage_get_timeline(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    start_time: f64,
    end_time: f64,
    max_records: Option<i64>,
) -> Result<Vec<storage::ScreenshotRecord>, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    // 如果传入的是毫秒级时间戳，转换为秒
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

/// Get screenshot density (counts per time bucket) for timeline visualization.
/// Ultra-fast: no decryption, no joins, just COUNT(*) GROUP BY time bucket.
#[tauri::command]
async fn storage_get_timeline_density(
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

/// Full-text search across screenshot OCR text with optional filters.
#[tauri::command]
async fn storage_search(
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
    // 检查认证状态
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

/// Retrieves the full-resolution image for a screenshot by ID or path.
#[tauri::command]
async fn storage_get_image(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    tracing::debug!("id={:?}, path={:?}", id, path);

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let image_path = if let Some(id) = id {
            let record = state.get_screenshot_by_id(id)?;
            tracing::debug!("Found record: {:?}", record.as_ref().map(|r| &r.image_path));
            record.map(|r| r.image_path)
        } else {
            path
        };

        tracing::debug!("Final image_path={:?}", image_path);

        match image_path {
            Some(path) => {
                // 使用 StorageManager::read_image 读取加密图片
                match state.read_image(&path) {
                    Ok((data, mime_type)) => {
                        tracing::debug!("Successfully read image, mime={}", mime_type);
                        Ok(serde_json::json!({
                            "status": "success",
                            "data": data,
                            "mime_type": mime_type
                        }))
                    }
                    Err(e) => {
                        tracing::error!("Failed to read image: {}", e);
                        Err(e)
                    }
                }
            }
            None => {
                tracing::warn!("Image not found - no path available");
                Err("Image not found".to_string())
            }
        }
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
}

/// Retrieves a thumbnail image for a screenshot by ID or path.
#[tauri::command]
async fn storage_get_thumbnail(
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

/// Retrieves multiple thumbnails in a single batch request by screenshot IDs.
#[tauri::command]
async fn storage_batch_get_thumbnails(
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
async fn storage_warmup_thumbnails(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        // Session-level guard: skip if already done this session
        if state.thumbnail_warmup_done.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::info!("[Warmup] Thumbnail warmup already done this session, skipping");
            return Ok(serde_json::json!({
                "generated": 0,
                "skipped": 0,
                "errors": 0,
                "cached": true
            }));
        }

        // Persistent guard: check sentinel in DB
        match state.is_thumbnail_warmup_done() {
            Ok(true) => {
                tracing::info!("[Warmup] Thumbnail warmup already completed (sentinel found), skipping");
                state.thumbnail_warmup_done.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(serde_json::json!({
                    "generated": 0,
                    "skipped": 0,
                    "errors": 0,
                    "cached": true
                }));
            }
            Ok(false) => {} // proceed
            Err(e) => {
                tracing::warn!("[Warmup] Failed to check sentinel: {}, proceeding with warmup", e);
            }
        }

        let paths = state.get_all_image_paths()?;
        let total = paths.len();
        tracing::info!("[Warmup] Starting thumbnail warmup for {} screenshots", total);
        let start = std::time::Instant::now();
        let mut generated: u64 = 0;
        let mut skipped: u64 = 0;
        let mut errors: u64 = 0;
        let mut last_progress = std::time::Instant::now();

        for (i, path) in paths.iter().enumerate() {
            match state.ensure_thumbnail_cached(path) {
                Ok(true) => generated += 1,
                Ok(false) => skipped += 1,
                Err(e) => {
                    tracing::warn!("[Warmup] [{}/{}] Error for {}: {}", i + 1, total, path, e);
                    errors += 1;
                }
            }
            // Periodic progress every 5 seconds
            if last_progress.elapsed().as_secs() >= 5 {
                tracing::info!(
                    "[Warmup] Progress: {}/{} (generated: {}, skipped: {}, errors: {})",
                    i + 1, total, generated, skipped, errors
                );
                last_progress = std::time::Instant::now();
            }
        }

        let elapsed = start.elapsed();
        tracing::info!(
            "[Warmup] Done in {:.1}s — generated: {}, skipped: {}, errors: {} (total: {})",
            elapsed.as_secs_f64(), generated, skipped, errors, total
        );

        // Mark as done: write persistent sentinel and set session flag
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

/// Retrieves full metadata and OCR results for a specific screenshot.
#[tauri::command]
async fn storage_get_screenshot_details(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    id: Option<i64>,
    path: Option<String>,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    let state = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        // 优先按 id 查找，其次按 path 查找
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

/// Deletes a screenshot and its associated image data and vector embeddings.
#[tauri::command]
async fn storage_delete_screenshot(
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

/// Deletes all screenshots within a specified time range.
#[tauri::command]
async fn storage_delete_by_time_range(
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
async fn storage_list_processes(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<serde_json::Value>, String> {
    // 检查认证状态
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

/// Saves a new screenshot with OCR text and metadata.
#[tauri::command]
async fn storage_save_screenshot(
    state: tauri::State<'_, Arc<StorageState>>,
    request: storage::SaveScreenshotRequest,
) -> Result<storage::SaveScreenshotResponse, String> {
    state.save_screenshot(&request)
}

#[tauri::command]
async fn storage_compute_link_scores(
    state: tauri::State<'_, Arc<StorageState>>,
    links: Vec<storage::VisibleLink>,
) -> Result<Vec<storage::ScoredLink>, String> {
    state.compute_link_scores(&links)
}

#[tauri::command]
async fn storage_get_public_key(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    let key = state.get_public_key()?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &key,
    ))
}

// ==================== 存储策略（policy）命令 ====================

/// Saves application policy/configuration as JSON.
#[tauri::command]
async fn storage_set_policy(
    state: tauri::State<'_, Arc<StorageState>>,
    policy: serde_json::Value,
) -> Result<serde_json::Value, String> {
    state
        .save_policy(&policy)
        .map_err(|e| format!("Failed to save policy: {}", e))?;
    Ok(policy)
}

#[derive(serde::Serialize)]
struct HmacMigrationStatus {
    needs_migration: bool,
    is_running: bool,
}

#[tauri::command]
async fn storage_check_hmac_migration_status(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<HmacMigrationStatus, String> {
    let needs_migration = state.check_hmac_migration_status()?;
    let is_running = state.is_migration_in_progress();
    
    Ok(HmacMigrationStatus {
        needs_migration,
        is_running,
    })
}

#[tauri::command]
async fn storage_run_hmac_migration(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    let state = state.inner().clone();
    
    // If already running, don't start a new task, but don't return "success" either
    // This prevents the frontend from thinking it's finished
    if state.is_migration_in_progress() {
        return Err("ALREADY_RUNNING".to_string());
    }

    tokio::task::spawn_blocking(move || {
        state.run_hmac_migration(move |phase, processed, total| {
            let _ = app_handle.emit(
                "hmac-migration-progress",
                serde_json::json!({
                    "phase": phase,
                    "processed": processed,
                    "total": total
                }),
            );
        })
    })
    .await
    .map_err(|e| format!("Migration task panicked: {}", e))?
}

/// Retrieves the current application policy/configuration.
#[tauri::command]
async fn storage_get_policy(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    state
        .load_policy()
        .map_err(|e| format!("Failed to load policy: {}", e))
}

#[tauri::command]
async fn storage_encrypt_for_chromadb(
    state: tauri::State<'_, Arc<StorageState>>,
    plaintext: String,
) -> Result<String, String> {
    state.encrypt_for_chromadb(&plaintext)
}

#[tauri::command]
async fn storage_decrypt_from_chromadb(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
    encrypted: String,
) -> Result<String, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    state.decrypt_from_chromadb(&encrypted)
}

// ==================== 分类命令 ====================

#[tauri::command]
async fn storage_update_category(
    state: tauri::State<'_, Arc<StorageState>>,
    monitor_state: tauri::State<'_, MonitorState>,
    screenshot_id: i64,
    category: String,
) -> Result<serde_json::Value, String> {
    // Read old category before updating
    let old_category = state
        .get_screenshot_by_id(screenshot_id)
        .ok()
        .flatten()
        .and_then(|r| r.category.clone());

    // Update category in DB with confidence 1.0 (user-set)
    let updated = state.update_screenshot_category(screenshot_id, &category, Some(1.0))?;

    // Also tell Python to learn from this user classification
    if updated {
        if let Ok(Some(record)) = state.get_screenshot_by_id(screenshot_id) {
            let title = record.window_title.clone().unwrap_or_default();
            let process_name = record.process_name.clone().unwrap_or_default();

            // Collect OCR text for this screenshot
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
            // Fire-and-forget: don't fail the command if Python is unavailable
            match monitor::forward_command_to_python(&monitor_state, payload).await {
                Ok(resp) => {
                    tracing::info!(
                        "Anchor learning result for screenshot {}: {:?}",
                        screenshot_id, resp
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to add anchor to classifier: {}", e);
                }
            }
        }
    }

    Ok(serde_json::json!({
        "status": "success",
        "updated": updated
    }))
}

#[tauri::command]
async fn storage_get_categories(
    monitor_state: tauri::State<'_, MonitorState>,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "command": "get_categories"
    });
    monitor::forward_command_to_python(&monitor_state, payload).await
}

/// 从数据库获取所有已使用的分类（纯 Rust，不依赖 Python）
#[tauri::command]
async fn storage_get_categories_from_db(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    state.get_categories_from_db()
}

/// 批量通过 image_hash 获取分类信息
#[tauri::command]
async fn storage_batch_get_categories(
    state: tauri::State<'_, Arc<StorageState>>,
    image_hashes: Vec<String>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    state.batch_get_categories_by_hash(&image_hashes)
}

// ==================== 任务聚类命令 ====================

/// 获取任务列表
#[tauri::command]
async fn storage_get_tasks(
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

/// 获取与当前快照同任务簇的相关快照
#[tauri::command]
async fn storage_get_related_screenshots(
    state: tauri::State<'_, Arc<StorageState>>,
    screenshot_id: i64,
    limit: Option<i64>,
) -> Result<storage::task::RelatedScreenshotsResult, String> {
    state.get_related_screenshots(screenshot_id, limit.unwrap_or(8))
}

/// 获取任务关联的快照
#[tauri::command]
async fn storage_get_task_screenshots(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<Vec<storage::task::TaskScreenshotStub>, String> {
    state.get_task_screenshots(task_id, page.unwrap_or(0), page_size.unwrap_or(50))
}

/// 更新任务标签
#[tauri::command]
async fn storage_update_task_label(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
    label: String,
) -> Result<(), String> {
    state.update_task_label(task_id, &label)
}

/// 删除任务（保留快照）
#[tauri::command]
async fn storage_delete_task(
    state: tauri::State<'_, Arc<StorageState>>,
    task_id: i64,
) -> Result<(), String> {
    state.delete_task(task_id)
}

/// 合并多个任务
#[tauri::command]
async fn storage_merge_tasks(
    state: tauri::State<'_, Arc<StorageState>>,
    task_ids: Vec<i64>,
) -> Result<i64, String> {
    state.merge_tasks(&task_ids)
}

/// 保存聚类结果
#[tauri::command]
async fn storage_save_clustering_results(
    state: tauri::State<'_, Arc<StorageState>>,
    tasks: Vec<storage::task::SaveTaskRequest>,
) -> Result<Vec<i64>, String> {
    state.save_clustering_results(&tasks)
}

// ==================== MCP 服务命令 ====================

/// Enables or disables the MCP (Model Context Protocol) server.
#[tauri::command]
async fn mcp_set_enabled(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
    enabled: bool,
) -> Result<serde_json::Value, String> {
    if enabled {
        // Load existing policy
        let mut policy = storage_state.load_policy()?;

        // Check if token already exists
        let existing_token = policy.get("mcp_token_encrypted").and_then(|v| v.as_str());
        let (token_plaintext, is_new_token) = if existing_token.is_some() {
            // Token exists, decrypt for hash
            let encrypted_b64 = existing_token.unwrap();
            let token = mcp_server::decrypt_token(&credential_state, encrypted_b64)?;
            (token, false)
        } else {
            // Generate new token
            let token = mcp_server::generate_token();
            let encrypted_b64 = mcp_server::encrypt_token(&credential_state, &token)?;
            policy_as_object_mut(&mut policy)?.insert("mcp_token_encrypted".into(), serde_json::json!(encrypted_b64));
            (token, true)
        };

        let port = policy.get("mcp_port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .unwrap_or(mcp_server::get_port(&storage_state));

        // Save enabled state
        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(true));
        if policy.get("mcp_port").is_none() {
            policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
        }
        storage_state.save_policy(&policy)?;

        // Compute hash and start server
        let token_hash = mcp_server::hash_token(&token_plaintext);
        mcp_state.set_token_hash(token_hash);
        mcp_server::start_server(app, port, token_hash).await?;

        if is_new_token {
            Ok(serde_json::json!({ "status": "ok", "token": token_plaintext, "port": port }))
        } else {
            Ok(serde_json::json!({ "status": "ok", "port": port }))
        }
    } else {
        // Disable: stop server and save policy
        mcp_server::stop_server(&mcp_state).await;

        let mut policy = storage_state.load_policy()?;
        policy_as_object_mut(&mut policy)?.insert("mcp_enabled".into(), serde_json::json!(false));
        storage_state.save_policy(&policy)?;

        Ok(serde_json::json!({ "status": "ok" }))
    }
}

/// Returns the current MCP server status (enabled, port, running).
#[tauri::command]
async fn mcp_get_status(
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    let policy = storage_state.load_policy()?;
    let enabled = policy.get("mcp_enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let port = mcp_server::get_port(&storage_state);
    let running = mcp_state.is_running();

    Ok(serde_json::json!({
        "enabled": enabled,
        "port": port,
        "running": running
    }))
}

#[tauri::command]
async fn mcp_reset_token(
    app: tauri::AppHandle,
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    mcp_state: tauri::State<'_, mcp_server::McpRuntimeState>,
) -> Result<serde_json::Value, String> {
    let token = mcp_server::generate_token();
    let encrypted_b64 = mcp_server::encrypt_token(&credential_state, &token)?;

    // Update policy
    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert("mcp_token_encrypted".into(), serde_json::json!(encrypted_b64));
    storage_state.save_policy(&policy)?;

    // Update runtime hash
    let token_hash = mcp_server::hash_token(&token);
    mcp_state.set_token_hash(token_hash);

    // Restart server if running
    let was_running = mcp_state.is_running();
    if was_running {
        mcp_server::stop_server(&mcp_state).await;
        let port = mcp_server::get_port(&storage_state);
        mcp_server::start_server(app, port, token_hash).await?;
    }

    Ok(serde_json::json!({ "status": "ok", "token": token }))
}

#[tauri::command]
async fn mcp_get_port(
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<u16, String> {
    Ok(mcp_server::get_port(&storage_state))
}

#[tauri::command]
async fn mcp_set_port(
    storage_state: tauri::State<'_, Arc<StorageState>>,
    port: u16,
) -> Result<(), String> {
    let mut policy = storage_state.load_policy()?;
    policy_as_object_mut(&mut policy)?.insert("mcp_port".into(), serde_json::json!(port));
    storage_state.save_policy(&policy)
}

#[tauri::command]
async fn mcp_get_sensitive_filter_config(
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
) -> Result<sensitive_filter::SensitiveFilterConfig, String> {
    Ok(filter_state.get_config())
}

#[tauri::command]
async fn mcp_set_sensitive_filter_config(
    filter_state: tauri::State<'_, Arc<SensitiveFilterState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
    config: sensitive_filter::SensitiveFilterConfig,
) -> Result<(), String> {
    filter_state.update_config(config.clone());

    // Persist to policy JSON
    let mut policy = storage_state.load_policy()?;
    let config_value = serde_json::to_value(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    policy_as_object_mut(&mut policy)?.insert("sensitive_filter".into(), config_value);
    storage_state.save_policy(&policy)
}

// ==================== 数据迁移命令 ====================

/// 列出所有未加密的明文截图文件
#[tauri::command]
async fn storage_list_plaintext_files(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<Vec<String>, String> {
    state.list_plaintext_screenshots()
}

/// 迁移所有明文截图文件（加密并删除原文件）
/// 需要认证
#[tauri::command]
async fn storage_migrate_plaintext(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<storage::MigrationResult, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    state.migrate_plaintext_screenshots()
}

/// 新：整体迁移 data 目录（copy + remove 实现将在 storage.rs 中完成）
#[tauri::command]
async fn storage_migrate_data_dir(
    state: tauri::State<'_, Arc<StorageState>>,
    app_handle: tauri::AppHandle,
    target: String,
    migrate_data_files: Option<bool>,
) -> Result<serde_json::Value, String> {
    // 调用 storage impl 的阻塞迁移方法，使用 spawn_blocking 避免阻塞 async runtime
    let state_clone = state.inner().clone();
    let app = app_handle.clone();
    let t = target.clone();
    let should_migrate = migrate_data_files.unwrap_or(true);
    let join_handle = tauri::async_runtime::spawn_blocking(move || {
        state_clone.migrate_data_dir_blocking(app, t, should_migrate)
    });

    match join_handle.await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Migration task join failed: {:?}", e)),
    }
}

/// 取消正在进行的迁移
#[tauri::command]
async fn storage_migration_cancel(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    let in_progress = state.request_migration_cancel();
    Ok(serde_json::json!({
        "status": if in_progress { "cancel_requested" } else { "idle" },
        "in_progress": in_progress
    }))
}

/// 删除所有明文截图文件（不迁移，直接删除）
/// 需要认证
#[tauri::command]
async fn storage_delete_plaintext(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<serde_json::Value, String> {
    // 检查认证状态
    check_auth_required(&credential_state)?;

    let deleted = state.delete_plaintext_screenshots()?;
    Ok(serde_json::json!({
        "status": "success",
        "deleted_count": deleted
    }))
}

#[tauri::command]
fn get_advanced_config() -> Result<serde_json::Value, String> {
    let cpu_limit_enabled = registry_config::get_bool("cpu_limit_enabled").unwrap_or(true);
    let cpu_limit_percent = registry_config::get_u32("cpu_limit_percent").unwrap_or(10);
    let capture_on_ocr_busy = registry_config::get_bool("capture_on_ocr_busy").unwrap_or(false);
    let ocr_queue_limit_enabled =
        registry_config::get_bool("ocr_queue_limit_enabled").unwrap_or(true);
    let ocr_queue_max_size = registry_config::get_u32("ocr_queue_max_size").unwrap_or(1);
    let use_dml = registry_config::get_bool("use_dml").unwrap_or(false);
    let dml_device_id = registry_config::get_u32("dml_device_id").unwrap_or(0);
    let game_mode_enabled = registry_config::get_bool("game_mode_enabled").unwrap_or(true);
    let clustering_interval = registry_config::get_string("clustering_interval").unwrap_or_else(|| "1w".to_string());

    Ok(serde_json::json!({
        "cpu_limit_enabled": cpu_limit_enabled,
        "cpu_limit_percent": cpu_limit_percent,
        "capture_on_ocr_busy": capture_on_ocr_busy,
        "ocr_queue_limit_enabled": ocr_queue_limit_enabled,
        "ocr_queue_max_size": ocr_queue_max_size,
        "use_dml": use_dml,
        "dml_device_id": dml_device_id,
        "game_mode_enabled": game_mode_enabled,
        "clustering_interval": clustering_interval,
    }))
}

#[tauri::command]
fn set_advanced_config(config: serde_json::Value) -> Result<(), String> {
    if let Some(v) = config.get("cpu_limit_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("cpu_limit_enabled", v)?;
    }
    if let Some(v) = config.get("cpu_limit_percent").and_then(|v| v.as_u64()) {
        registry_config::set_u32("cpu_limit_percent", v as u32)?;
    }
    if let Some(v) = config.get("capture_on_ocr_busy").and_then(|v| v.as_bool()) {
        registry_config::set_bool("capture_on_ocr_busy", v)?;
    }
    if let Some(v) = config
        .get("ocr_queue_limit_enabled")
        .and_then(|v| v.as_bool())
    {
        registry_config::set_bool("ocr_queue_limit_enabled", v)?;
    }
    if let Some(v) = config.get("ocr_queue_max_size").and_then(|v| v.as_u64()) {
        registry_config::set_u32("ocr_queue_max_size", v as u32)?;
    }
    if let Some(v) = config.get("use_dml").and_then(|v| v.as_bool()) {
        registry_config::set_bool("use_dml", v)?;
    }
    if let Some(v) = config.get("dml_device_id").and_then(|v| v.as_u64()) {
        registry_config::set_u32("dml_device_id", v as u32)?;
    }
    if let Some(v) = config.get("game_mode_enabled").and_then(|v| v.as_bool()) {
        registry_config::set_bool("game_mode_enabled", v)?;
    }
    if let Some(v) = config.get("clustering_interval").and_then(|v| v.as_str()) {
        registry_config::set_string("clustering_interval", v)?;
    }
    Ok(())
}

/// Initializes the credential manager and Windows Hello key pair.
#[tauri::command]
async fn credential_initialize(
    credential_state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        // 尝试从文件加载公钥（已有安装，不触发任何弹窗）
        // load_public_key_from_file 内部会自动缓存公钥
        match credential_manager::load_public_key_from_file(&credential_state) {
            Ok(_) => {}
            Err(_) => {
                // 公钥文件不存在 → 首次安装，从 CNG 导出公钥
                let pk = credential_manager::export_or_get_public_key(&credential_state)
                    .map_err(|e| format!("Failed to initialize credentials: {}", e))?;

                credential_manager::save_public_key_to_file(&credential_state, &pk)
                    .map_err(|e| format!("Failed to save public key: {}", e))?;
            }
        };

        // 仅在首次使用时生成主密钥（不触发任何弹窗）
        // 已有主密钥文件时跳过，解锁由 credential_verify_user 负责
        credential_manager::ensure_master_key_created(&credential_state)
            .map_err(|e| format!("Failed to create master key: {}", e))?;

        // 初始化存储
        storage_state.initialize()?;

        Ok("Credentials initialized successfully".to_string())
    }

    #[cfg(not(windows))]
    {
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// Prompts Windows Hello authentication and establishes a session.
#[tauri::command]
async fn credential_verify_user(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    storage_state: tauri::State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    #[cfg(windows)]
    {
        // 始终强制执行 CNG 私钥解密操作，触发"Windows 安全中心"对话框
        // 不使用缓存，确保每次解锁都需要用户输入凭据
        credential_manager::force_verify_and_unlock_master_key(&state)
            .map_err(|e| format!("Verification failed: {}", e))?;

        // 认证成功，更新会话时间
        state.update_auth_time();

        // 认证成功后尝试执行去重迁移（仅首次）
        storage_state.try_dedup_migration();

        // 认证成功后尝试执行 bitmap 索引优化迁移（去标点 bigram）
        storage_state.try_bitmap_index_migration();

        Ok(true)
    }

    #[cfg(not(windows))]
    {
        let _ = &storage_state; // suppress unused warning
        Err("Windows Hello is only available on Windows".to_string())
    }
}

/// 检查当前认证会话是否有效
#[tauri::command]
async fn credential_check_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<bool, String> {
    Ok(state.is_session_valid())
}

/// 使认证会话失效（手动锁定）
#[tauri::command]
async fn credential_lock_session(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<(), String> {
    state.invalidate_session();
    Ok(())
}

/// 通知应用进入前台/后台（由前端调用）
#[tauri::command]
async fn credential_set_foreground(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    in_foreground: bool,
) -> Result<(), String> {
    state.set_foreground_state(in_foreground);
    Ok(())
}

/// 设置会话超时时间（秒），并尝试持久化到注册表
#[tauri::command]
async fn credential_set_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
    timeout: i64,
) -> Result<(), String> {
    state.set_session_timeout(timeout);
    // 尝试写入注册表作为持久化机制
    if let Err(e) = crate::registry_config::set_string("session_timeout_secs", &timeout.to_string())
    {
        tracing::error!("Failed to persist session_timeout_secs: {}", e);
        // 不要因为持久化失败而使设置失败——仍将会话超时应用于内存状态
    }
    Ok(())
}

/// 获取当前会话超时时间（秒）
#[tauri::command]
async fn credential_get_session_timeout(
    state: tauri::State<'_, Arc<CredentialManagerState>>,
) -> Result<i64, String> {
    Ok(state.get_session_timeout())
}

#[tauri::command]
async fn toggle_game_mode(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    registry_config::set_bool("game_mode_enabled", enabled)?;
    if enabled {
        monitor::start_game_mode_monitor(app);
    } else {
        // If DirectML was suppressed due to game mode, restart monitor to reapply DirectML settings
        let state = app.state::<MonitorState>();
        let was_suppressed = state
            .game_mode_dml_suppressed
            .load(std::sync::atomic::Ordering::SeqCst);
        monitor::stop_game_mode_monitor(&app);
        if was_suppressed {
            let _ = monitor::stop_monitor(
                app.state::<MonitorState>(),
                app.state::<Arc<CaptureState>>(),
            )
            .await;
            let _ = monitor::start_monitor(app.state::<MonitorState>(), app.clone()).await;
        }
    }
    Ok(())
}


#[tauri::command]
fn check_extension_setup_needed() -> Result<bool, String> {
    Ok(!registry_config::get_bool("extension_setup_done").unwrap_or(false))
}

#[tauri::command]
fn mark_extension_setup_done() -> Result<(), String> {
    registry_config::set_bool("extension_setup_done", true)
}

#[tauri::command]
async fn check_clustering_setup_needed(
    state: tauri::State<'_, Arc<StorageState>>,
) -> Result<bool, String> {
    // Already completed → not needed
    if registry_config::get_bool("clustering_setup_done").unwrap_or(false) {
        return Ok(false);
    }
    // No screenshots at all → new install, skip wizard
    let count = state.count_screenshots_by_time_range(0.0, 9_999_999_999.0)?;
    Ok(count > 0)
}

#[tauri::command]
fn mark_clustering_setup_done() -> Result<(), String> {
    registry_config::set_bool("clustering_setup_done", true)
}

#[tauri::command]
fn get_extension_enhancement_config() -> Result<serde_json::Value, String> {
    let chrome = registry_config::get_bool("extension_enhanced_chrome").unwrap_or(false);
    let edge = registry_config::get_bool("extension_enhanced_edge").unwrap_or(false);
    Ok(serde_json::json!({
        "chrome": chrome,
        "edge": edge,
    }))
}

#[tauri::command]
fn set_extension_enhancement(browser: String, enabled: bool) -> Result<(), String> {
    match browser.as_str() {
        "chrome" => registry_config::set_bool("extension_enhanced_chrome", enabled),
        "edge" => registry_config::set_bool("extension_enhanced_edge", enabled),
        _ => Err(format!("Unknown browser: {}", browser)),
    }
}

#[tauri::command]
fn get_game_mode_status(app: tauri::AppHandle) -> serde_json::Value {
    let state = app.state::<MonitorState>();
    let active = state
        .game_mode_dml_suppressed
        .load(std::sync::atomic::Ordering::SeqCst);
    let permanent = state
        .game_mode_permanently_suppressed
        .load(std::sync::atomic::Ordering::SeqCst);
    let capture_state = app.state::<Arc<CaptureState>>();
    let fullscreen_paused = capture_state
        .game_mode_capture_paused
        .load(std::sync::atomic::Ordering::SeqCst);
    serde_json::json!({
        "active": active,
        "permanent": permanent,
        "fullscreen_paused": fullscreen_paused,
    })
}

fn get_data_dir() -> std::path::PathBuf {
    // 优先从注册表读取 data_dir（HKCU\Software\CarbonPaper）
    if let Some(dir) = registry_config::get_string("data_dir") {
        return std::path::PathBuf::from(dir);
    }

    // 兼容旧版：尝试从 config.json 迁移到注册表
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        dirs::data_local_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    });

    let cfg_path = std::path::PathBuf::from(&local_appdata)
        .join("CarbonPaper")
        .join("config.json");
    if cfg_path.exists() {
        if let Ok(s) = std::fs::read_to_string(&cfg_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if let Some(dd) = v.get("data_dir").and_then(|d| d.as_str()) {
                    // 写入注册表并删除旧配置文件
                    let _ = registry_config::set_string("data_dir", dd);
                    let _ = std::fs::remove_file(&cfg_path);
                    return std::path::PathBuf::from(dd);
                }
            }
        }
    }

    std::path::PathBuf::from(local_appdata)
        .join("CarbonPaper")
        .join("data")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 创建凭证管理器状态
    let data_dir = get_data_dir();
    let _log_guard = logging::init_logging(&data_dir); // 最早初始化日志系统

    let credential_state = Arc::new(CredentialManagerState::new(data_dir.clone()));
    let storage_state = Arc::new(StorageState::new(
        data_dir.clone(),
        credential_state.clone(),
    ));

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(MonitorState::new())
        .manage(Arc::new(CaptureState::default()))
        .manage(AnalysisState::default())
        .manage(updater::UpdaterState::new())
        .manage(mcp_server::McpRuntimeState::new())
        .manage(Arc::new(SensitiveFilterState::default()))
        .manage(credential_state)
        .manage(storage_state)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    // If a critical error has occurred, close = exit
                    if error_window::HAS_CRITICAL_ERROR.load(Ordering::Relaxed) {
                        std::process::exit(1);
                    }
                    // Normal: hide instead of close (unless updating)
                    if !IS_UPDATING.load(Ordering::Relaxed) {
                        api.prevent_close();
                        let _ = window.hide();
                        let _ = window.app_handle().emit("app-hidden", ());
                    }
                }
            }
        })
        .setup({
            let data_dir = data_dir.clone();
            move |app| {
                // Install global panic hook (before anything else that might panic)
                error_window::set_app_handle(app.handle().clone());
                error_window::install_panic_hook();

                build_tray(app)?;

                // 应用亚克力磨砂透明效果
                if let Some(window) = app.get_webview_window("main") {
                    let _ = apply_acrylic(&window, Some((0, 0, 0, 0)));
                }

                analysis::start_memory_sampler(app.handle().clone());
                logging::spawn_maintenance_task(data_dir.clone());

                tracing::info!(
                    r#"
  _____               _                    _____
 / ____|             | |                  |  __ \
| |       __ _  _ __ | |__    ___   _ __  | |__) |  __ _  _ __    ___  _ __
| |      / _` || '__|| '_ \  / _ \ | '_ \ |  ___/  / _` || '_ \  / _ \| '__|
| |____ | (_| || |   | |_) || (_) || | | || |     | (_| || |_) ||  __/| |
 \_____| \__,_||_|   |_.__/  \___/ |_| |_||_|      \__,_|| .__/  \___||_|
                                                         | |
                                                         |_|
    "#
                );

                // 初始化凭据管理器（加载公钥或首次创建）
                let credential_state = app.state::<Arc<CredentialManagerState>>();

                // 公钥用于弱数据库加密与行级封装
                let public_key_ready =
                    match credential_manager::load_public_key_from_file(&credential_state) {
                        Ok(public_key) => {
                            tracing::info!(
                                "Public key loaded from file, length: {}",
                                public_key.len()
                            );
                            true
                        }
                        Err(credential_manager::CredentialError::KeyNotFound) => {
                            tracing::info!("Public key file missing, exporting from CNG...");

                            match credential_manager::export_or_get_public_key(&credential_state) {
                                Ok(public_key) => {
                                    tracing::info!(
                                        "CNG public key exported, length: {}",
                                        public_key.len()
                                    );
                                    if let Err(e) = credential_manager::save_public_key_to_file(
                                        &credential_state,
                                        &public_key,
                                    ) {
                                        tracing::error!("Failed to save public key: {}", e);
                                    }
                                    true
                                }
                                Err(e) => {
                                    tracing::error!("Failed to export CNG public key: {:?}", e);
                                    false
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to load public key: {:?}", e);
                            false
                        }
                    };

                // 初始化存储（弱加密，不需要认证）
                let storage = app.state::<Arc<StorageState>>();
                if public_key_ready {
                    if let Err(e) = storage.initialize() {
                        tracing::error!("Failed to initialize storage: {}", e);
                    } else {
                        // Start background migration to backfill plaintext process_name
                        let storage_clone = storage.inner().clone();
                        std::thread::spawn(move || {
                            StorageState::backfill_plaintext_process_names(storage_clone);
                        });
                    }
                } else {
                    tracing::error!("Storage initialization deferred: public key unavailable");
                }

                // 如果游戏模式已启用，启动游戏模式监控（GPU 负载 + 全屏暂停）
                if registry_config::get_bool("game_mode_enabled").unwrap_or(false) {
                    tracing::info!("Restoring game mode monitor on startup");
                    monitor::start_game_mode_monitor(app.handle().clone());
                }

                // Sync installed browser extension if source was updated
                match native_messaging::sync_installed_extension() {
                    Ok(true) => tracing::info!("Browser extension synced to latest version"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("Extension sync check failed: {}", e),
                }

                // Start NMH pipe server for browser extension communication
                {
                    let data_dir_clone = data_dir.clone();
                    let storage_for_nmh = storage.inner().clone();
                    let capture_for_nmh = app.state::<Arc<CaptureState>>().inner().clone();
                    let app_handle_for_nmh = app.handle().clone();
                    std::thread::spawn(move || {
                        match reverse_ipc::generate_nmh_auth_token(&data_dir_clone) {
                            Ok(token) => {
                                let mut nmh_server = reverse_ipc::NmhPipeServer::new();
                                if let Err(e) = nmh_server.start(
                                    storage_for_nmh,
                                    capture_for_nmh,
                                    app_handle_for_nmh,
                                    token,
                                ) {
                                    tracing::error!("Failed to start NMH pipe server: {}", e);
                                }
                                // Keep the server alive by not dropping it
                                // (it runs in its own thread internally)
                                std::mem::forget(nmh_server);
                            }
                            Err(e) => {
                                tracing::error!("Failed to generate NMH auth token: {}", e);
                            }
                        }
                    });
                }

                // Load sensitive filter dictionaries and persisted config
                {
                    let filter_state = app.state::<Arc<SensitiveFilterState>>();
                    filter_state.load_dicts(app.handle());
                    // Load persisted config from policy
                    if let Ok(policy) = storage.load_policy() {
                        if let Some(filter_config) = policy.get("sensitive_filter") {
                            if let Ok(config) = serde_json::from_value::<sensitive_filter::SensitiveFilterConfig>(filter_config.clone()) {
                                filter_state.update_config(config);
                            }
                        }
                    }
                }

                // Auto-start MCP server if enabled in policy
                {
                    let app_handle_mcp = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        use tauri::Manager;
                        let storage = app_handle_mcp.state::<Arc<StorageState>>();
                        let credential = app_handle_mcp.state::<Arc<CredentialManagerState>>();
                        let mcp_runtime = app_handle_mcp.state::<mcp_server::McpRuntimeState>();

                        if let Ok(policy) = storage.load_policy() {
                            if policy.get("mcp_enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                                match mcp_server::auto_start(
                                    app_handle_mcp.clone(),
                                    &credential,
                                    &storage,
                                    &mcp_runtime,
                                ).await {
                                    Ok(()) => tracing::info!("MCP server auto-started"),
                                    Err(e) => tracing::error!("MCP auto-start failed: {}", e),
                                }
                            }
                        }
                    });
                }

                // Auto-install missing spaCy models in background
                python::auto_install_spacy_models(app.handle().clone());

                Ok(())
            }
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            close_process,
            set_updating_flag,
            monitor::start_monitor,
            monitor::stop_monitor,
            monitor::pause_monitor,
            monitor::resume_monitor,
            monitor::get_monitor_status,
            monitor::execute_monitor_command,
            // 存储相关命令
            storage_get_timeline,
            storage_get_timeline_density,
            storage_search,
            storage_get_image,
            storage_get_thumbnail,
            storage_batch_get_thumbnails,
            storage_warmup_thumbnails,
            storage_get_screenshot_details,
            storage_delete_screenshot,
            storage_delete_by_time_range,
            storage_list_processes,
            storage_save_screenshot,
            storage_set_policy,
            storage_get_policy,
            storage_get_public_key,
            storage_compute_link_scores,
            storage_encrypt_for_chromadb,
            storage_decrypt_from_chromadb,
            storage_update_category,
            storage_get_categories,
            storage_get_categories_from_db,
            storage_batch_get_categories,
            storage_check_hmac_migration_status,
            storage_run_hmac_migration,
            // 任务聚类命令
            storage_get_tasks,
            storage_get_related_screenshots,
            storage_get_task_screenshots,
            storage_update_task_label,
            storage_delete_task,
            storage_merge_tasks,
            storage_save_clustering_results,
            analysis::get_analysis_overview,
            // MCP 服务命令
            mcp_set_enabled,
            mcp_get_status,
            mcp_reset_token,
            mcp_get_port,
            mcp_set_port,
            mcp_get_sensitive_filter_config,
            mcp_set_sensitive_filter_config,
            // 高级配置命令
            get_advanced_config,
            set_advanced_config,
            monitor::enumerate_gpus,
            toggle_game_mode,
            get_game_mode_status,
            // 数据迁移命令
            storage_list_plaintext_files,
            storage_migrate_plaintext,
            storage_migrate_data_dir,
            storage_migration_cancel,
            storage_delete_plaintext,
            // 凭证管理相关命令
            credential_initialize,
            credential_verify_user,
            credential_check_session,
            credential_lock_session,
            credential_set_foreground,
            credential_set_session_timeout,
            credential_get_session_timeout,
            get_autostart_status,
            set_autostart,
            python::check_python_status,
            python::check_python_venv,
            python::request_install_python,
            python::install_python_venv,
            python::check_deps_freshness,
            python::sync_python_deps,
            python::install_spacy_model,
            python::check_spacy_models,
            python::force_recheck_spacy_models,
            model_management::download_model,
            model_management::check_model_files,
            // Updater commands
            updater::updater_check,
            updater::updater_download,
            updater::updater_extract,
            updater::updater_apply,
            // Native messaging commands
            native_messaging::get_nm_host_status,
            native_messaging::register_nm_host_chrome,
            native_messaging::register_nm_host_edge,
            native_messaging::install_browser_extension,
            native_messaging::sync_extension_if_needed,
            check_extension_setup_needed,
            mark_extension_setup_done,
            check_clustering_setup_needed,
            mark_clustering_setup_done,
            get_extension_enhancement_config,
            set_extension_enhancement,
            // Error window commands
            get_log_dir,
            restart_app,
            trigger_test_error,
            exit_app,
            frontend_log,
        ]);

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = app
                .get_webview_window("main")
                .expect("no main window")
                .set_focus();
        }));
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub fn run_silent_install() {
    python::run_silent_install();
}

/// 子进程入口：执行 CNG 解密并将结果输出到 stdout
/// 协议：
///   成功: exit 0, stdout = hex(master_key)
///   失败: exit 1, stderr = 错误信息
///   用户取消: exit 2, stderr = "UserCancelled"
pub fn run_cng_unlock(key_file_path: &str) {
    use std::process::exit;

    let file_data = match std::fs::read(key_file_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to read master key file: {}", e);
            exit(1);
        }
    };

    let ciphertext = match credential_manager::decode_master_key_file(&file_data) {
        Ok(ct) => ct,
        Err(e) => {
            eprintln!("Failed to decode master key file: {}", e);
            exit(1);
        }
    };

    match credential_manager::decrypt_master_key_with_cng(&ciphertext) {
        Ok(master_key) => {
            print!("{}", hex::encode(&master_key));
            exit(0);
        }
        Err(credential_manager::CredentialError::UserCancelled) => {
            eprintln!("UserCancelled");
            exit(2);
        }
        Err(e) => {
            eprintln!("{}", e);
            exit(1);
        }
    }
}
