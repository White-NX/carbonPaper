//! Windows named-pipe service for browser native messaging.
//!
//! The shared single-instance pipe negotiates an independent single-instance
//! channel for each OS-verified NMH. Capture/session traffic uses those channels.

use crate::capture::CaptureState;
use crate::nmh_pipe::{self, PeerRole, PeerVerifier, PipeSide};
use crate::nmh_protocol::{self, IPC_PROTOCOL_VERSION, RESPONSE_ACK};
use crate::reverse_ipc_protocol::{read_ipc_frame, write_ipc_frame, StorageResponse};
use crate::storage::{SaveScreenshotRequest, StorageState};
use crate::windows_platform::{self, ProcessIdentity};
use rand::RngCore;
use serde::Serialize;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Win32::Foundation::HANDLE;

/// Return whether `pid` is `expected_ancestor_pid` or a bounded-depth descendant.
fn is_pid_descendant_of(pid: u32, expected_ancestor_pid: u32) -> bool {
    if pid == expected_ancestor_pid {
        return true;
    }
    let Some(processes) = crate::windows_platform::process_tree() else {
        return false;
    };

    let mut current = pid;
    for _ in 0..8 {
        let Some(parent) = processes.get(&current).map(|(parent, _)| *parent) else {
            return false;
        };
        if parent == expected_ancestor_pid {
            return true;
        }
        if parent == 0 || parent == current {
            return false;
        }
        current = parent;
    }
    false
}

/// Compute deterministic NMH pipe name from current user's Windows SID.
/// Both CarbonPaper and carbonpaper-nmh.exe use this same formula.
pub fn compute_nmh_pipe_name() -> Result<String, String> {
    Ok(crate::nmh_protocol::pipe_name(
        &crate::windows_platform::current_user_sid()?,
    ))
}

/// Generate a random 32-byte auth token and write it to the data dir.
pub fn generate_nmh_auth_token(data_dir: &std::path::Path) -> Result<String, String> {
    let mut token_bytes = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut token_bytes);
    let token = hex::encode(&token_bytes);

    let token_path = data_dir.join("nmh_auth_token");
    std::fs::write(&token_path, &token)
        .map_err(|e| format!("Failed to write NMH auth token: {}", e))?;

    tracing::info!("NMH auth token written to {:?}", token_path);
    Ok(token)
}

/// Read the NMH auth token from the data dir.
#[allow(dead_code)]
pub fn read_nmh_auth_token(data_dir: &PathBuf) -> Result<String, String> {
    let token_path = data_dir.join("nmh_auth_token");
    std::fs::read_to_string(&token_path)
        .map_err(|e| format!("Failed to read NMH auth token: {}", e))
        .map(|s| s.trim().to_string())
}

/// NMH pipe server state.
pub struct NmhPipeServer {
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl NmhPipeServer {
    pub fn new() -> Self {
        Self { shutdown_tx: None }
    }

    pub fn start(
        &mut self,
        storage: Arc<StorageState>,
        capture_state: Arc<CaptureState>,
        app: tauri::AppHandle,
        auth_token: String,
    ) -> Result<(), String> {
        let pipe_name = compute_nmh_pipe_name()?;
        let verifier = PeerVerifier::new(PeerRole::NativeHost)?;
        let file = nmh_pipe::create_server_file(&pipe_name, true)
            .map_err(|e| format!("Cannot create NMH bootstrap pipe: {e}"))?;
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);
        let handler: nmh_pipe::RequestHandler = Arc::new(move |peer, request| {
            let storage = storage.clone();
            let capture = capture_state.clone();
            let app = app.clone();
            Box::pin(
                async move { process_nmh_request(&request, &peer, storage, capture, app).await },
            )
        });
        let on_close: nmh_pipe::CloseHandler = Arc::new(|peer| {
            let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
            remove_peer_sessions(&mut sessions, peer, None);
        });
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("Cannot create NMH runtime: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                match nmh_pipe::async_server(file) {
                    Ok(server) => {
                        nmh_pipe::serve_broker(
                            server,
                            verifier,
                            shutdown_rx,
                            auth_token,
                            handler,
                            on_close,
                        )
                        .await
                    }
                    Err(e) => tracing::error!("Cannot attach NMH bootstrap pipe: {e}"),
                }
            });
        });
        Ok(())
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.try_send(());
        }
    }
}

fn registration_claims_match(
    request: &serde_json::Value,
    peer: &ProcessIdentity,
    browser: &ProcessIdentity,
) -> bool {
    let browser_name = browser.image.file_name().map(|name| name.to_string_lossy());
    request["ipc_protocol_version"] == IPC_PROTOCOL_VERSION
        && nmh_protocol::request_pid(request, "nmh_pid") == Some(peer.pid)
        && nmh_protocol::request_pid(request, "browser_pid") == Some(browser.pid)
        && browser.pid != peer.pid
        && browser.created_at <= peer.created_at
        && browser.user_sid == peer.user_sid
        && nmh_protocol::valid_private_pipe_name(
            request["cmd_pipe_name"].as_str().unwrap_or(""),
            nmh_protocol::CMD_PIPE_PREFIX,
        )
        && request["browser_exe_path"]
            .as_str()
            .is_some_and(|path| nmh_pipe::path_matches(std::path::Path::new(path), &browser.image))
        && browser_name.is_some_and(|actual| {
            request["browser_exe_name"]
                .as_str()
                .is_some_and(|claimed| claimed.eq_ignore_ascii_case(&actual))
        })
}

fn remove_peer_sessions(
    sessions: &mut Vec<NmhSession>,
    peer: &ProcessIdentity,
    pipe: Option<&str>,
) {
    sessions.retain(|session| {
        !(session.nmh_peer == *peer && pipe.is_none_or(|pipe| session.cmd_pipe_name == pipe))
    });
}

/// Process an NMH request (save_extension_screenshot)
async fn process_nmh_request(
    req: &serde_json::Value,
    peer: &ProcessIdentity,
    storage: Arc<StorageState>,
    capture_state: Arc<CaptureState>,
    app_handle: tauri::AppHandle,
) -> StorageResponse {
    if req["ipc_protocol_version"] != IPC_PROTOCOL_VERSION
        || nmh_protocol::request_pid(req, "nmh_pid") != Some(peer.pid)
    {
        return StorageResponse::error("Invalid NMH client identity");
    }
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");

    if crate::maintenance::is_active() && !crate::maintenance::reverse_ipc_command_allowed(command)
    {
        return StorageResponse::error(crate::maintenance::MAINTENANCE_IN_PROGRESS);
    }

    match command {
        "register_nmh" => {
            let Some(browser) = windows_platform::browser_ancestor(peer.pid) else {
                return StorageResponse::error("Cannot verify NMH browser ancestor");
            };
            let browser_name = browser
                .image
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cmd_pipe_name = req["cmd_pipe_name"].as_str().unwrap_or("");
            if !registration_claims_match(req, peer, &browser) {
                return StorageResponse::error("Invalid register_nmh request");
            }
            // The declared endpoint must actually belong to this connected NMH.
            // Its pinned single server instance makes the server-PID check meaningful.
            let cmd_pipe = match nmh_pipe::open_async_client(cmd_pipe_name).await {
                Ok(pipe) => pipe,
                Err(e) => {
                    return StorageResponse::error(&format!("Cannot open NMH command pipe: {e}"))
                }
            };
            let verifier = match PeerVerifier::new(PeerRole::NativeHost) {
                Ok(verifier) => verifier,
                Err(e) => return StorageResponse::error(&e),
            };
            if let Err(e) = verifier.verify(
                HANDLE(cmd_pipe.as_raw_handle()),
                PipeSide::Client,
                Some(peer),
            ) {
                return StorageResponse::error(&e);
            }
            drop(cmd_pipe);
            let now_ms = chrono::Utc::now().timestamp_millis();
            let session = NmhSession {
                browser_pid: browser.pid,
                browser_exe_path: browser.image.to_string_lossy().into_owned(),
                browser_exe_name: browser_name,
                nmh_pid: peer.pid,
                cmd_pipe_name: cmd_pipe_name.to_string(),
                registered_at_ms: now_ms,
                last_seen_ms: now_ms,
                nmh_peer: peer.clone(),
                browser_created_at: browser.created_at,
            };
            upsert_session(
                &mut NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner()),
                session,
            );
            StorageResponse::success(serde_json::json!({"registered": true}))
        }
        "unregister_nmh" => {
            let cmd_pipe_name = req["cmd_pipe_name"].as_str().unwrap_or("");
            let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
            remove_peer_sessions(&mut sessions, peer, Some(cmd_pipe_name));
            StorageResponse::success(serde_json::json!({"unregistered": true}))
        }
        "save_extension_screenshot" => {
            // Only the OS-verified owner can refresh this session.
            let now_ms = chrono::Utc::now().timestamp_millis();
            {
                let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
                for session in sessions
                    .iter_mut()
                    .filter(|session| session.nmh_peer == *peer)
                {
                    session.last_seen_ms = now_ms;
                }
            }

            // A delayed extension response must not restart capture after a
            // stop or bypass power/game-mode suppression.
            if !extension_capture_allowed(&app_handle, &capture_state) {
                return StorageResponse::error("Capture is paused or stopped");
            }

            let image_data = match req.get("image_data").and_then(|v| v.as_str()) {
                Some(d) => d,
                None => return StorageResponse::error("Missing image_data"),
            };
            if req.get("image_hash").and_then(|v| v.as_str()).is_none() {
                return StorageResponse::error("Missing image_hash");
            }
            let page_url = req
                .get("page_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let page_title = req
                .get("page_title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let page_icon = req
                .get("page_icon")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let visible_links: Option<Vec<crate::storage::VisibleLink>> = req
                .get("visible_links")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let browser_name = req
                .get("browser_name")
                .and_then(|v| v.as_str())
                .unwrap_or("browser-extension")
                .to_string();

            // Check if extension enhancement is enabled for this browser
            if !is_extension_enhanced_browser(&browser_name) {
                return StorageResponse::error(
                    "Extension enhancement not enabled for this browser",
                );
            }

            let Some(ocr_slot) = capture_state.try_reserve_ocr_slot() else {
                return StorageResponse::error("OCR is busy (single-flight mode)");
            };

            let encoded_image = match base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                image_data,
            ) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return StorageResponse::error(&format!(
                        "Invalid extension image base64: {error}"
                    ));
                }
            };
            match image::guess_format(&encoded_image) {
                Ok(image::ImageFormat::Png) => {}
                Ok(format) => {
                    return StorageResponse::error(&format!(
                        "Extension OCR requires lossless PNG input, received {format:?}"
                    ));
                }
                Err(error) => {
                    return StorageResponse::error(&format!(
                        "Cannot determine extension image format: {error}"
                    ));
                }
            }
            let decoded_rgb_image = match image::load_from_memory_with_format(
                &encoded_image,
                image::ImageFormat::Png,
            ) {
                Ok(image) => Arc::new(image.to_rgb8()),
                Err(error) => {
                    return StorageResponse::error(&format!(
                        "Cannot decode extension PNG: {error}"
                    ));
                }
            };
            drop(encoded_image);
            let ocr_rgb_image = resize_extension_ocr_image(decoded_rgb_image.clone());
            let jpeg_quality = capture_state
                .config
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .jpeg_quality;
            let jpeg_bytes = match crate::capture::encode_rgb_jpeg(&decoded_rgb_image, jpeg_quality)
            {
                Ok(bytes) => Arc::<[u8]>::from(bytes),
                Err(error) => return StorageResponse::error(&error),
            };
            let image_hash = crate::capture::md5_hash(&jpeg_bytes);
            let width = decoded_rgb_image.width() as i32;
            let height = decoded_rgb_image.height() as i32;

            // Build metadata with process_icon (same mechanism as capture loop)
            let metadata = Some(serde_json::json!({
                "process_icon": page_icon,
            }));

            let request = SaveScreenshotRequest {
                image_data: String::new(),
                image_hash: image_hash.clone(),
                width,
                height,
                window_title: page_title.clone(),
                process_name: Some(browser_name.clone()),
                metadata,
                ocr_results: None,
                source: Some("extension".to_string()),
                page_url,
                page_icon,
                visible_links,
            };

            if !extension_capture_allowed(&app_handle, &capture_state) {
                return StorageResponse::error("Capture is paused or stopped");
            }
            let recap_generation = storage.db_generation();
            match storage.save_screenshot_temp_bytes(&request, &jpeg_bytes) {
                Ok(result) => {
                    if result.status == "duplicate" {
                        return StorageResponse::success(serde_json::to_value(result).unwrap());
                    }

                    // Dispatch to OCR pipeline if we have a screenshot_id
                    if let Some(screenshot_id) = result.screenshot_id {
                        if let Err(error) = storage.processing_stage.put_recap_metadata(
                            &storage,
                            screenshot_id,
                            recap_generation,
                            request.page_url.as_deref(),
                            None,
                        ) {
                            tracing::warn!("[RECAP] Browser context staging deferred: {}", error);
                        }
                        // Spawn async OCR task
                        let storage_arc = storage.clone();
                        let app_clone = app_handle.clone();
                        let window_title = page_title.unwrap_or_default();
                        let timestamp_ms = chrono::Utc::now().timestamp_millis();

                        let ocr_guard = ocr_slot.into_task_guard();
                        tokio::spawn(async move {
                            let _ocr_guard = ocr_guard;
                            let route = crate::capture::OcrRouteConfig::from_app(&app_clone);
                            let result = process_extension_ocr(
                                &app_clone,
                                &storage_arc,
                                screenshot_id,
                                ocr_rgb_image,
                                &image_hash,
                                &window_title,
                                &browser_name,
                                timestamp_ms,
                                route,
                            )
                            .await;

                            if let Err(e) = result {
                                if e == crate::power::POWER_SAVING_ACTIVE {
                                    tracing::info!("Extension OCR interrupted by power saving for screenshot {screenshot_id}");
                                } else {
                                    crate::ml_runtime::schedule_ocr_model_health_notification(
                                        app_clone.clone(),
                                    );
                                    tracing::error!(
                                        "Extension OCR failed for screenshot {}: {}",
                                        screenshot_id,
                                        e
                                    );
                                }
                                if let Err(commit_err) =
                                    storage_arc.commit_screenshot(screenshot_id, None, None, None)
                                {
                                    tracing::error!(
                                        "Failed to preserve extension screenshot: {}",
                                        commit_err
                                    );
                                }
                                let _ = storage_arc.set_ocr_status(
                                    screenshot_id,
                                    "failed",
                                    Some("rust"),
                                    Some("ppocrv5-ch-mobile"),
                                    None,
                                    Some(&e),
                                    None,
                                );
                            }
                        });
                    }

                    StorageResponse::success(serde_json::to_value(result).unwrap())
                }
                Err(e) => StorageResponse::error(&e),
            }
        }
        _ => StorageResponse::error(&format!("Unknown NMH command: {}", command)),
    }
}

const EXTENSION_OCR_MAX_SIDE: u32 = 1600;

fn extension_capture_allowed(app: &tauri::AppHandle, capture: &CaptureState) -> bool {
    use tauri::Manager;
    let monitor = app.state::<crate::monitor::MonitorState>();
    capture.accepts_frames(
        monitor.is_running() && !monitor.stopping.load(std::sync::atomic::Ordering::SeqCst),
        crate::power::is_saving_power(app),
    )
}

fn resize_extension_ocr_image(image: Arc<image::RgbImage>) -> Arc<image::RgbImage> {
    let max_dim = image.width().max(image.height());
    if max_dim <= EXTENSION_OCR_MAX_SIDE {
        return image;
    }

    let ratio = EXTENSION_OCR_MAX_SIDE as f64 / max_dim as f64;
    let new_width = ((image.width() as f64 * ratio).round() as u32).max(1);
    let new_height = ((image.height() as f64 * ratio).round() as u32).max(1);
    Arc::new(image::imageops::resize(
        image.as_ref(),
        new_width,
        new_height,
        image::imageops::FilterType::Lanczos3,
    ))
}

/// Check if extension enhancement is enabled (single global toggle).
fn is_extension_enhanced_browser(_browser_name: &str) -> bool {
    extension_enhancement_enabled()
}

/// The single global "browser extension enhancement" toggle.
fn extension_enhancement_enabled() -> bool {
    crate::registry_config::get_bool("extension_enhanced_global").unwrap_or(false)
}

// ==================== NMH session table ====================
//
// Each NMH instance registers itself at runtime with the browser main
// process it belongs to (PID + exe path) and a random-suffix command pipe.
// The capture loop routes capture requests by matching the foreground
// window's PID against this table — no browser-name lists anywhere, so any
// Chromium-based browser works without per-browser support code.

/// A live NMH registration: one browser instance with the extension connected.
#[derive(Debug, Clone, Serialize)]
pub struct NmhSession {
    pub browser_pid: u32,
    pub browser_exe_path: String,
    pub browser_exe_name: String,
    pub nmh_pid: u32,
    pub cmd_pipe_name: String,
    pub registered_at_ms: i64,
    pub last_seen_ms: i64,
    #[serde(skip)]
    nmh_peer: ProcessIdentity,
    #[serde(skip)]
    browser_created_at: u64,
}

static NMH_SESSIONS: once_cell::sync::Lazy<std::sync::Mutex<Vec<NmhSession>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(Vec::new()));

/// Insert or replace a session, keyed by nmh_pid (an NMH re-registering —
/// e.g. after an app restart rotated the token — replaces its old entry).
fn upsert_session(sessions: &mut Vec<NmhSession>, session: NmhSession) {
    sessions.retain(|s| s.nmh_pid != session.nmh_pid);
    sessions.push(session);
}

/// Pick the session serving a foreground window: exact browser-PID match
/// first; otherwise same exe path + ancestor check (covers windows owned by
/// a child process of the browser main process). Ties (multi-profile: several
/// NMHs on one browser process) go to the most recently seen session.
fn select_session<'a>(
    sessions: &'a [NmhSession],
    window_pid: u32,
    process_path: &str,
    is_descendant: impl Fn(u32, u32) -> bool,
) -> Option<&'a NmhSession> {
    let exact = sessions
        .iter()
        .filter(|s| s.browser_pid == window_pid)
        .max_by_key(|s| s.last_seen_ms);
    if exact.is_some() {
        return exact;
    }
    if process_path.is_empty() {
        return None;
    }
    sessions
        .iter()
        .filter(|s| {
            s.browser_exe_path.eq_ignore_ascii_case(process_path)
                && is_descendant(window_pid, s.browser_pid)
        })
        .max_by_key(|s| s.last_seen_ms)
}

/// Drop sessions whose browser process is gone, or whose PID now belongs to
/// a different executable (PID-reuse guard).
pub fn prune_dead_sessions() {
    let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions.retain(|session| {
        windows_platform::process_is_same(&session.nmh_peer)
            && windows_platform::inspect_process(session.browser_pid).is_ok_and(|browser| {
                browser.created_at == session.browser_created_at
                    && nmh_pipe::path_matches(
                        &browser.image,
                        std::path::Path::new(&session.browser_exe_path),
                    )
            })
    });
}

/// Snapshot of live sessions (pruned first) for the settings UI.
pub fn nmh_sessions_snapshot() -> Vec<NmhSession> {
    prune_dead_sessions();
    NMH_SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Find the NMH session that should capture for the given foreground window,
/// if extension enhancement is enabled. Used by the capture loop.
pub fn find_nmh_session_for_pid(window_pid: u32, process_path: &str) -> Option<NmhSession> {
    if !extension_enhancement_enabled() {
        return None;
    }
    {
        let sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.is_empty() {
            return None;
        }
    }
    prune_dead_sessions();
    let sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    select_session(&sessions, window_pid, process_path, is_pid_descendant_of).cloned()
}

/// Whether any registered NMH session belongs to a process with this
/// executable name. The game-mode fullscreen exemption uses this so
/// Chromium forks absent from the hardcoded browser list are still
/// treated as browsers while an extension session is live. Deliberately
/// a plain table lookup — no pruning, no per-call syscalls.
pub fn has_nmh_session_for_exe(process_name: &str) -> bool {
    if process_name.is_empty() {
        return false;
    }
    let sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions
        .iter()
        .any(|s| s.browser_exe_name.eq_ignore_ascii_case(process_name))
}

/// Request the browser extension behind `session` to capture its current tab.
/// Opens the session's command pipe and sends a `request_capture` command.
/// The whole round-trip is bounded by a timeout so a wedged NMH (e.g. its
/// browser is suspended with a full Native Messaging pipe) cannot stall the
/// capture loop. On failure the session is dropped from the table (dead pipe)
/// so the capture loop falls back to normal screen capture immediately.
pub async fn request_extension_capture_session(session: &NmhSession) -> Result<(), String> {
    const CMD_ROUNDTRIP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

    let pipe_name = session.cmd_pipe_name.clone();

    tracing::debug!(
        "request_extension_capture: browser={} pid={} pipe={}",
        session.browser_exe_name,
        session.browser_pid,
        pipe_name
    );

    let round_trip = async {
        use tokio::io::AsyncWriteExt;

        let mut pipe = nmh_pipe::open_async_client(&pipe_name)
            .await
            .map_err(|e| format!("Cannot open NMH cmd pipe: {e}"))?;
        let verifier = PeerVerifier::new(PeerRole::NativeHost)?;
        verifier.verify(
            HANDLE(pipe.as_raw_handle()),
            PipeSide::Client,
            Some(&session.nmh_peer),
        )?;
        let request = serde_json::json!({
            "command": "request_capture", "ipc_protocol_version": IPC_PROTOCOL_VERSION,
        });
        let data = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        write_ipc_frame(&mut pipe, &data).await?;
        let response: serde_json::Value = serde_json::from_slice(&read_ipc_frame(&mut pipe).await?)
            .map_err(|e| format!("Invalid NMH cmd response: {e}"))?;
        pipe.write_all(&[RESPONSE_ACK])
            .await
            .map_err(|e| e.to_string())?;
        if response.get("status").and_then(|s| s.as_str()) == Some("ok") {
            Ok(())
        } else {
            Err(response
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("NMH reported failure")
                .to_string())
        }
    };

    let result: Result<(), String> =
        match tokio::time::timeout(CMD_ROUNDTRIP_TIMEOUT, round_trip).await {
            Ok(result) => result,
            Err(_) => Err(format!(
                "NMH cmd pipe round-trip timed out after {:?}",
                CMD_ROUNDTRIP_TIMEOUT
            )),
        };

    if result.is_err() {
        let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        remove_peer_sessions(
            &mut sessions,
            &session.nmh_peer,
            Some(&session.cmd_pipe_name),
        );
    }
    result
}

/// Send losslessly decoded extension pixels to the Rust OCR pipeline and commit results.
async fn process_extension_ocr(
    app: &tauri::AppHandle,
    storage: &StorageState,
    screenshot_id: i64,
    rgb_image: Arc<image::RgbImage>,
    image_hash: &str,
    window_title: &str,
    process_name: &str,
    timestamp_ms: i64,
    route: crate::capture::OcrRouteConfig,
) -> Result<(), String> {
    let provider = if route.use_directml {
        "directml"
    } else {
        "cpu"
    };
    storage.set_ocr_status(
        screenshot_id,
        "running",
        Some("rust"),
        Some("ppocrv5-ch-mobile"),
        Some(provider),
        None,
        None,
    )?;
    crate::capture::process_ocr_inner(
        app,
        storage,
        screenshot_id,
        rgb_image,
        image_hash,
        window_title,
        process_name,
        timestamp_ms,
        crate::registry_config::get_u32("ocr_timeout_secs").unwrap_or(120),
        route,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_responses_serialize_success_and_error_without_opposite_fields() {
        for (response, expected) in [
            (
                StorageResponse::success(serde_json::json!({"id": 1})),
                serde_json::json!({"status": "success", "data": {"id": 1}}),
            ),
            (
                StorageResponse::error("bad request"),
                serde_json::json!({"status": "error", "error": "bad request"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(response).unwrap(), expected);
        }
    }

    fn make_session(browser_pid: u32, nmh_pid: u32, last_seen_ms: i64) -> NmhSession {
        NmhSession {
            browser_pid,
            browser_exe_path: format!(r"C:\browsers\b{}.exe", browser_pid),
            browser_exe_name: format!("b{}.exe", browser_pid),
            nmh_pid,
            cmd_pipe_name: format!(r"\\.\pipe\carbon_nmh_cmd_r_{:032x}", nmh_pid),
            registered_at_ms: last_seen_ms,
            last_seen_ms,
            nmh_peer: ProcessIdentity {
                pid: nmh_pid,
                created_at: 1,
                user_sid: String::new(),
                image: PathBuf::new(),
            },
            browser_created_at: 1,
        }
    }

    #[test]
    fn test_upsert_session_replaces_same_nmh_pid() {
        let mut sessions = vec![make_session(100, 1, 1000)];
        upsert_session(&mut sessions, make_session(200, 1, 2000));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].browser_pid, 200);
        assert_eq!(sessions[0].last_seen_ms, 2000);
    }

    #[test]
    fn test_upsert_session_keeps_other_sessions() {
        let mut sessions = vec![make_session(100, 1, 1000)];
        upsert_session(&mut sessions, make_session(200, 2, 2000));
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_remove_session_matches_both_keys() {
        let mut sessions = vec![make_session(100, 1, 1000), make_session(200, 2, 2000)];
        let pipe = sessions[0].cmd_pipe_name.clone();
        let first = sessions[0].nmh_peer.clone();
        let second = sessions[1].nmh_peer.clone();
        remove_peer_sessions(&mut sessions, &first, Some(&pipe));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].nmh_pid, 2);
        // Wrong pipe name doesn't remove
        remove_peer_sessions(&mut sessions, &second, Some(r"\\.\pipe\other"));
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_select_session_exact_pid_match() {
        let sessions = vec![make_session(100, 1, 1000), make_session(200, 2, 2000)];
        let s = select_session(&sessions, 200, "", |_, _| false).unwrap();
        assert_eq!(s.nmh_pid, 2);
    }

    #[test]
    fn test_select_session_no_match() {
        let sessions = vec![make_session(100, 1, 1000)];
        assert!(select_session(&sessions, 999, "", |_, _| false).is_none());
    }

    #[test]
    fn test_select_session_same_pid_picks_most_recent() {
        // Multi-profile: two NMHs registered against the same browser process
        let sessions = vec![make_session(100, 1, 1000), {
            let mut s = make_session(100, 2, 5000);
            s.browser_exe_path = sessions_path_of(100);
            s
        }];
        let s = select_session(&sessions, 100, "", |_, _| false).unwrap();
        assert_eq!(s.nmh_pid, 2);
    }

    fn sessions_path_of(pid: u32) -> String {
        format!(r"C:\browsers\b{}.exe", pid)
    }

    #[test]
    fn test_select_session_descendant_fallback() {
        let sessions = vec![make_session(100, 1, 1000)];
        // Window owned by a child process (pid 555) of the browser (pid 100),
        // same exe path (case-insensitive)
        let path = r"c:\BROWSERS\B100.EXE";
        let s = select_session(&sessions, 555, path, |pid, ancestor| {
            pid == 555 && ancestor == 100
        });
        assert!(s.is_some());
        // Different exe path → no fallback even if descendant
        let s2 = select_session(&sessions, 555, r"C:\other\b.exe", |_, _| true);
        assert!(s2.is_none());
    }

    #[test]
    fn test_select_session_empty_path_no_fallback() {
        let sessions = vec![make_session(100, 1, 1000)];
        assert!(select_session(&sessions, 555, "", |_, _| true).is_none());
    }

    #[test]
    fn extension_ocr_image_is_scaled_to_maximum_side() {
        let image = Arc::new(image::RgbImage::from_pixel(
            5120,
            2880,
            image::Rgb([0, 0, 0]),
        ));
        let resized = resize_extension_ocr_image(image);
        assert_eq!(resized.dimensions(), (1600, 900));
        assert!(resized.as_raw().len() <= 32 * 1024 * 1024);
    }

    #[test]
    fn extension_ocr_image_keeps_small_input_dimensions() {
        let image = Arc::new(image::RgbImage::from_pixel(
            1200,
            800,
            image::Rgb([0, 0, 0]),
        ));
        let resized = resize_extension_ocr_image(image);
        assert_eq!(resized.dimensions(), (1200, 800));
    }
    #[test]
    fn registration_binds_claims_to_the_os_nmh_and_browser() {
        let peer = ProcessIdentity {
            pid: 200,
            created_at: 20,
            user_sid: "fixture-user".into(),
            image: PathBuf::from(r"C:\runtime\carbonpaper-nmh.exe"),
        };
        let browser = ProcessIdentity {
            pid: 100,
            created_at: 10,
            user_sid: peer.user_sid.clone(),
            image: PathBuf::from(r"C:\browsers\360chromex.exe"),
        };
        let valid = serde_json::json!({
            "ipc_protocol_version": IPC_PROTOCOL_VERSION, "nmh_pid": peer.pid,
            "browser_pid": browser.pid,
            "browser_exe_path": browser.image.to_string_lossy(),
            "browser_exe_name": "360chromex.exe",
            "cmd_pipe_name": format!("{}{}", nmh_protocol::CMD_PIPE_PREFIX, "ab".repeat(16)),
        });
        assert!(registration_claims_match(&valid, &peer, &browser));
        for (field, forged) in [
            ("nmh_pid", serde_json::json!(201)),
            ("nmh_pid", serde_json::json!((u32::MAX as u64) + 201)),
            ("browser_pid", serde_json::json!(peer.pid)),
            (
                "browser_exe_path",
                serde_json::json!(r"C:\browsers\other.exe"),
            ),
            ("browser_exe_name", serde_json::json!("chrome.exe")),
            (
                "cmd_pipe_name",
                serde_json::json!(r"\\.\pipe\carbon_nmh_cmd_r_abc"),
            ),
        ] {
            let mut request = valid.clone();
            request[field] = forged;
            assert!(
                !registration_claims_match(&request, &peer, &browser),
                "{field}"
            );
        }
        let mut reused_browser = browser.clone();
        reused_browser.created_at = peer.created_at + 1;
        assert!(!registration_claims_match(&valid, &peer, &reused_browser));
    }

    #[test]
    fn peer_cleanup_preserves_other_nmh_and_reused_pid_sessions() {
        let old = make_session(100, 1, 1000);
        let mut replacement = old.clone();
        replacement.nmh_peer.created_at += 1;
        let other = make_session(100, 2, 2000);
        let mut sessions = vec![replacement.clone(), other.clone()];
        remove_peer_sessions(&mut sessions, &old.nmh_peer, None);
        assert_eq!(sessions.len(), 2);
        remove_peer_sessions(&mut sessions, &replacement.nmh_peer, Some("wrong-pipe"));
        assert_eq!(sessions.len(), 2);
        remove_peer_sessions(
            &mut sessions,
            &replacement.nmh_peer,
            Some(&replacement.cmd_pipe_name),
        );
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].nmh_pid, other.nmh_pid);
    }
}
