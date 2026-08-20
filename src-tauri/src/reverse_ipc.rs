//! Windows named-pipe services for retained Python features and browser native
//! messaging.
//!
//! The authenticated reverse pipe exposes the narrow storage and inference
//! operations needed by classification and task clustering. Browser-extension
//! screenshot ingestion uses the separate NMH pipe in this module.
//!
use crate::capture::CaptureState;
use crate::monitor::MonitorState;
use crate::reverse_ipc_protocol::{read_ipc_frame, write_ipc_frame, StorageResponse};
#[cfg(test)]
use crate::storage::ScreenshotRecord;
use crate::storage::{
    BackgroundReadError, BackgroundScreenshotSummary, SaveScreenshotRequest, StorageState,
};
use rand::RngCore;
use serde::Serialize;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use tauri::Manager;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer};
use tokio::sync::{mpsc, Semaphore};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

#[cfg(test)]
fn screenshot_record_with_ocr_json(
    rec: ScreenshotRecord,
    ocr_map: &std::collections::HashMap<i64, String>,
) -> serde_json::Value {
    let ocr_text = ocr_map.get(&rec.id).cloned().unwrap_or_default();
    serde_json::json!({
        "id": rec.id,
        "process_name": rec.process_name.unwrap_or_default(),
        "window_title": rec.window_title.unwrap_or_default(),
        "ocr_text": ocr_text,
        "timestamp": rec.timestamp.unwrap_or(0) as f64,
        "category": rec.category.unwrap_or_default(),
    })
}

fn background_screenshot_with_ocr_json(
    rec: BackgroundScreenshotSummary,
    ocr_map: &std::collections::HashMap<i64, String>,
) -> serde_json::Value {
    let ocr_text = ocr_map.get(&rec.id).cloned().unwrap_or_default();
    serde_json::json!({
        "id": rec.id,
        "process_name": rec.process_name.unwrap_or_default(),
        "window_title": rec.window_title.unwrap_or_default(),
        "ocr_text": ocr_text,
        "timestamp": rec.timestamp.unwrap_or(0) as f64,
        "category": rec.category.unwrap_or_default(),
    })
}

fn background_read_error_response(error: BackgroundReadError) -> StorageResponse {
    match error {
        BackgroundReadError::AuthRequired => StorageResponse::error("AUTH_REQUIRED"),
        BackgroundReadError::Other(message) => StorageResponse::error(&message),
    }
}

use windows::Win32::Security::GetTokenInformation;
use windows::Win32::Security::{
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, TokenUser, ACL, ACL_REVISION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Holds pre-built security descriptor and ACL buffers whose addresses are referenced
/// by the SECURITY_ATTRIBUTES pointer fields. Bundling them in a struct guarantees
/// the backing memory lives as long as the SA is in use.
struct PipeSecurityContext {
    sa: SECURITY_ATTRIBUTES,
    _sd: Box<SECURITY_DESCRIPTOR>,
    _acl_buffer: Vec<u8>,
}

/// Create SECURITY_ATTRIBUTES that only allow the current user to access the pipe.
fn get_security_context() -> Result<PipeSecurityContext, String> {
    // SAFETY: `token_handle` is writable output storage; on success this function owns
    // the process-token handle and closes it exactly once after the inner helper returns.
    unsafe {
        let mut token_handle = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle)
            .map_err(|e| format!("OpenProcessToken failed: {}", e))?;

        let result = get_security_context_inner(token_handle);
        let _ = windows::Win32::Foundation::CloseHandle(token_handle);
        result
    }
}

/// Inner helper so that `?` returns to `get_security_context` which always closes token_handle.
fn get_security_context_inner(token_handle: HANDLE) -> Result<PipeSecurityContext, String> {
    // SAFETY: `token_handle` is live for this call; queried buffer sizes determine every
    // allocation, SID/ACL pointers refer into owned buffers retained by the returned
    // context, and Windows does not retain temporary Rust pointers beyond each call.
    unsafe {
        let mut return_length = 0u32;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut return_length);

        let mut token_buffer = vec![0u8; return_length as usize];
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(token_buffer.as_mut_ptr() as *mut _),
            return_length,
            &mut return_length,
        )
        .map_err(|e| format!("GetTokenInformation failed: {}", e))?;

        let token_user = &*(token_buffer.as_ptr() as *const windows::Win32::Security::TOKEN_USER);
        let user_sid = token_user.User.Sid;

        let sid_len = windows::Win32::Security::GetLengthSid(user_sid);
        let acl_size = std::mem::size_of::<ACL>()
            + std::mem::size_of::<windows::Win32::Security::ACCESS_ALLOWED_ACE>()
            + sid_len as usize
            - 4;
        let mut acl_buffer = vec![0u8; acl_size];
        let p_acl = acl_buffer.as_mut_ptr() as *mut ACL;

        windows::Win32::Security::InitializeAcl(
            p_acl,
            acl_size as u32,
            windows::Win32::Security::ACE_REVISION(ACL_REVISION.0),
        )
        .map_err(|e| format!("InitializeAcl failed: {}", e))?;

        windows::Win32::Security::AddAccessAllowedAce(
            p_acl,
            windows::Win32::Security::ACE_REVISION(ACL_REVISION.0),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            user_sid,
        )
        .map_err(|e| format!("AddAccessAllowedAce failed: {}", e))?;

        let mut sd = Box::new(SECURITY_DESCRIPTOR::default());
        InitializeSecurityDescriptor(
            PSECURITY_DESCRIPTOR(sd.as_mut() as *mut _ as *mut _),
            windows::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION,
        )
        .map_err(|e| format!("InitializeSecurityDescriptor failed: {}", e))?;

        SetSecurityDescriptorDacl(
            PSECURITY_DESCRIPTOR(sd.as_mut() as *mut _ as *mut _),
            true,
            Some(p_acl),
            false,
        )
        .map_err(|e| format!("SetSecurityDescriptorDacl failed: {}", e))?;

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.as_mut() as *mut _ as *mut _,
            bInheritHandle: false.into(),
        };

        Ok(PipeSecurityContext {
            sa,
            _sd: sd,
            _acl_buffer: acl_buffer,
        })
    }
}

/// Return whether `pid` is `expected_ancestor_pid` or a bounded-depth descendant.
fn is_pid_descendant_of(pid: u32, expected_ancestor_pid: u32) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };
    if pid == expected_ancestor_pid {
        return true;
    }

    let mut parent_by_pid = std::collections::HashMap::<u32, u32>::new();
    // SAFETY: the snapshot handle is checked before use, `PROCESSENTRY32.dwSize` is set as
    // required, and the owned snapshot is closed after synchronous enumeration.
    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(snapshot) => snapshot,
            Err(_) => return false,
        };
        let mut entry = PROCESSENTRY32 {
            dwSize: std::mem::size_of::<PROCESSENTRY32>() as u32,
            ..std::mem::zeroed()
        };
        if Process32First(snapshot, &mut entry).is_ok() {
            loop {
                parent_by_pid.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32Next(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }

    let mut current = pid;
    for _ in 0..8 {
        let Some(parent) = parent_by_pid.get(&current).copied() else {
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

/// Named pipe server for Python-to-Rust reverse IPC (storage requests).
pub struct ReverseIpcServer {
    pipe_name: String,
    auth_token: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl ReverseIpcServer {
    pub fn new(pipe_name: &str, auth_token: String) -> Self {
        Self {
            pipe_name: pipe_name.to_string(),
            auth_token,
            shutdown_tx: None,
        }
    }

    /// Start the named pipe server that listens for Python storage requests.
    pub fn start(
        &mut self,
        storage: Arc<StorageState>,
        app_handle: tauri::AppHandle,
    ) -> Result<(), String> {
        let pipe_name = self.pipe_name.clone();
        let auth_token = self.auth_token.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // 在新线程中运行 tokio runtime
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("Failed to create runtime: {}", e);
                    return;
                }
            };

            rt.block_on(async move {
                let full_pipe_name = format!(r"\\.\pipe\{}", pipe_name);
                let wide_pipe_name: Vec<u16> = full_pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
                let handler_semaphore = Arc::new(Semaphore::new(8));

                loop {
                    // 创建安全描述符
                    let sec_ctx = match get_security_context() {
                        Ok(ctx) => ctx,
                        Err(e) => {
                            tracing::error!("Failed to get security attributes: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    };

                    // Apply the current-user ACL and reject remote clients.
                    // Use byte-mode pipes for robust streaming of large JSON payloads.
                    // SAFETY: the pipe name is NUL-terminated; `sec_ctx` retains the
                    // security descriptor and ACL backing memory for the entire call.
                    let handle = unsafe {
                        windows::Win32::System::Pipes::CreateNamedPipeW(
                            windows::core::PCWSTR(wide_pipe_name.as_ptr()),
                            PIPE_ACCESS_DUPLEX | windows::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED,
                            windows::Win32::System::Pipes::PIPE_TYPE_BYTE | windows::Win32::System::Pipes::PIPE_READMODE_BYTE | windows::Win32::System::Pipes::PIPE_WAIT | windows::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS,
                            windows::Win32::System::Pipes::PIPE_UNLIMITED_INSTANCES,
                            1024 * 1024,
                            1024 * 1024,
                            0,
                            Some(&sec_ctx.sa),
                        )
                    };

                    if handle.is_invalid() {
                        // SAFETY: `GetLastError` reads thread-local Win32 state and takes
                        // no pointers or handles.
                        tracing::error!("Failed to create pipe via Win32 API: {:?}", unsafe { windows::Win32::Foundation::GetLastError() });
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    // Transfer the raw pipe handle into Tokio's owning server wrapper.
                    // SAFETY: `handle` is valid and uniquely owned. On success ownership
                    // transfers to `NamedPipeServer`; on failure it is closed below.
                    let server = unsafe {
                        match NamedPipeServer::from_raw_handle(handle.0) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!("Failed to convert raw handle to NamedPipeServer: {}", e);
                                let _ = windows::Win32::Foundation::CloseHandle(handle);
                                continue;
                            }
                        }
                    };

                    // Wait for either a client connection or the shutdown signal.
                    tokio::select! {
                        _ = shutdown_rx.recv() => {
                            tracing::info!("Reverse IPC server shutting down, goodbye");
                            break;
                        }
                        result = server.connect() => {
                            if let Err(e) = result {
                                tracing::error!("Client connection failed: {}", e);
                                continue;
                            }

                            let permit = match handler_semaphore.clone().try_acquire_owned() {
                                Ok(permit) => permit,
                                Err(_) => {
                                    tracing::warn!("[DIAG:REVERSE_IPC] handler pool busy; rejecting connection");
                                    let mut server = server;
                                    let err_resp = StorageResponse::error("Reverse IPC server busy");
                                    let response_bytes = serde_json::to_vec(&err_resp).unwrap_or_default();
                                    let _ = write_ipc_frame(&mut server, &response_bytes).await;
                                    continue;
                                }
                            };

                            // 处理客户端请求
                            let storage_clone = storage.clone();
                            let app_clone = app_handle.clone();
                            let auth_token_clone = auth_token.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                handle_client(server, storage_clone, app_clone, auth_token_clone).await;
                            });
                        }
                    }
                }
            });
        });

        Ok(())
    }

    /// 停止服务器
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.try_send(());
        }
    }

    /// 获取管道名
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }
}

/// Handles one authenticated reverse-IPC client connection.
async fn handle_client(
    mut server: NamedPipeServer,
    storage: Arc<StorageState>,
    app_handle: tauri::AppHandle,
    expected_auth_token: String,
) {
    // Validate the client PID before reading any application request.
    // SAFETY: Tokio owns a live pipe handle for the duration of the call and `pid` points
    // to writable stack storage that Windows fills synchronously.
    let client_pid_raw = unsafe {
        let mut pid: u32 = 0;
        let handle = HANDLE(server.as_raw_handle() as *mut _);
        if GetNamedPipeClientProcessId(handle, &mut pid).is_ok() {
            Some(pid)
        } else {
            None
        }
    };

    match client_pid_raw {
        Some(client_pid) => {
            let monitor_state = app_handle.state::<MonitorState>();
            let expected_pid_raw = {
                let guard = monitor_state
                    .process
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                guard.as_ref().map(|child| child.id())
            };

            if let Some(expected_pid) = expected_pid_raw {
                let is_valid = is_pid_descendant_of(client_pid, expected_pid);

                if !is_valid {
                    tracing::warn!(
                        "Illegal access attempt to Reverse IPC from PID {} (monitor root PID {} not in parent chain)",
                        client_pid,
                        expected_pid
                    );
                    let err_resp = serde_json::json!({"error": format!("Access denied: PID {} is not authorized", client_pid)});
                    let _ = write_ipc_frame(&mut server, err_resp.to_string().as_bytes()).await;
                    return;
                }
            } else {
                tracing::warn!(
                    "Reverse IPC connection received but monitor process is not registered"
                );
                return;
            }
        }
        None => {
            tracing::error!("Failed to get client PID from reverse IPC pipe");
            return;
        }
    }

    let mut keepalive = true;
    let mut requests_handled: u64 = 0;
    let mut last_seq_no: Option<u64> = None;
    while keepalive {
        let buf = match read_ipc_frame(&mut server).await {
            Ok(result) => result,
            Err(e) => {
                if keepalive && requests_handled > 0 && e == "overall_timeout" {
                    tracing::debug!(
                        "[DIAG:REVERSE_IPC] persistent connection idle timeout after {} requests",
                        requests_handled
                    );
                    return;
                }
                tracing::error!("Reverse IPC read error: {}", e);
                let response = StorageResponse::error(&e);
                let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                let _ = write_ipc_frame(&mut server, &response_bytes).await;
                return;
            }
        };

        if buf.is_empty() {
            return;
        }

        // 解析请求
        let req = match serde_json::from_slice::<serde_json::Value>(&buf) {
            Ok(req) => req,
            Err(e) => {
                let response =
                    StorageResponse::error(&format!("Invalid JSON: {} (bytes={})", e, buf.len()));
                let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                if let Err(e) = write_ipc_frame(&mut server, &response_bytes).await {
                    tracing::error!("Write error: {}", e);
                }
                return;
            }
        };

        if let Err(e) = validate_reverse_ipc_request(&req, &expected_auth_token, &mut last_seq_no) {
            tracing::warn!("[SECURITY] Reverse IPC auth rejected: {}", e);
            let response = StorageResponse::error(&e);
            let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
            let _ = write_ipc_frame(&mut server, &response_bytes).await;
            return;
        }

        keepalive = req
            .get("_ipc_keepalive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        requests_handled = requests_handled.saturating_add(1);

        let response = process_request(&req, &storage, &app_handle).await;

        // 发送响应
        let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
        if let Err(e) = write_ipc_frame(&mut server, &response_bytes).await {
            tracing::error!("Write error: {}", e);
            return;
        }

        if keepalive && requests_handled % 100 == 0 {
            tracing::debug!(
                "[DIAG:REVERSE_IPC] persistent connection handled {} requests",
                requests_handled
            );
        }
    }
}

/// 处理存储请求
async fn process_request(
    req: &serde_json::Value,
    storage: &StorageState,
    app_handle: &tauri::AppHandle,
) -> StorageResponse {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let diag_start = std::time::Instant::now();

    if crate::maintenance::is_active() && !crate::maintenance::reverse_ipc_command_allowed(command)
    {
        return StorageResponse::error(crate::maintenance::MAINTENANCE_IN_PROGRESS);
    }

    let response = match command {
        "bge_embed_texts" => {
            let texts = match req.get("texts").and_then(|value| value.as_array()) {
                Some(values) => {
                    let mut texts = Vec::with_capacity(values.len());
                    for (index, value) in values.iter().enumerate() {
                        let Some(text) = value.as_str() else {
                            return StorageResponse::error(&format!(
                                "texts[{index}] must be a string"
                            ));
                        };
                        texts.push(text.to_string());
                    }
                    texts
                }
                None => return StorageResponse::error("texts must be an array"),
            };
            match crate::classification_runtime::embed_bge_texts(app_handle.clone(), texts).await {
                Ok(result) => StorageResponse::success(
                    serde_json::to_value(result).unwrap_or_else(|_| serde_json::json!({})),
                ),
                Err(error) => StorageResponse::error(&error),
            }
        }

        "get_public_key" => match storage.get_public_key() {
            Ok(key) => {
                let encoded =
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &key);
                StorageResponse::success(serde_json::json!({
                    "public_key": encoded
                }))
            }
            Err(e) => StorageResponse::error(&e),
        },

        "encrypt_for_chromadb" => {
            let plaintext = req.get("plaintext").and_then(|p| p.as_str()).unwrap_or("");

            match storage.encrypt_for_chromadb(plaintext) {
                Ok(encrypted) => StorageResponse::success(serde_json::json!({
                    "encrypted": encrypted
                })),
                Err(e) => StorageResponse::error(&e),
            }
        }

        "decrypt_from_chromadb" => {
            let encrypted = req.get("encrypted").and_then(|p| p.as_str()).unwrap_or("");

            match storage.decrypt_from_chromadb(encrypted) {
                Ok(decrypted) => StorageResponse::success(serde_json::json!({
                    "decrypted": decrypted
                })),
                Err(e) => StorageResponse::error(&e),
            }
        }

        "decrypt_many_from_chromadb" => {
            let list_value = req.get("encrypted_list");
            let mut decrypted_list: Vec<String> = Vec::new();
            let mut error_count = 0;

            if let Some(values) = list_value.and_then(|v| v.as_array()) {
                for value in values {
                    let encrypted = value.as_str().unwrap_or("");
                    match storage.decrypt_from_chromadb(encrypted) {
                        Ok(decrypted) => decrypted_list.push(decrypted),
                        Err(_) => {
                            error_count += 1;
                            decrypted_list.push(encrypted.to_string());
                        }
                    }
                }
            }

            StorageResponse::success(serde_json::json!({
                "decrypted_list": decrypted_list,
                "error_count": error_count
            }))
        }

        "get_auth_status" => StorageResponse::success(serde_json::json!({
            "session_valid": storage.is_session_valid()
        })),

        "set_ocr_postprocess_status" => {
            let screenshot_id = req
                .get("screenshot_id")
                .and_then(|value| value.as_i64())
                .unwrap_or(-1);
            let status = req
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let error = req.get("error").and_then(|value| value.as_str());
            if screenshot_id < 0 {
                return StorageResponse::error("Invalid screenshot_id");
            }
            match storage.set_ocr_postprocess_status(screenshot_id, status, error) {
                Ok(()) => StorageResponse::success(serde_json::json!({ "updated": true })),
                Err(error) => StorageResponse::error(&error),
            }
        }
        "record_ocr_postprocess_retry" => {
            let screenshot_id = req
                .get("screenshot_id")
                .and_then(|value| value.as_i64())
                .unwrap_or(-1);
            let error = req
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("OCR postprocess failed");
            if screenshot_id < 0 {
                return StorageResponse::error("Invalid screenshot_id");
            }
            match storage.record_ocr_postprocess_retry(screenshot_id, error) {
                Ok(()) => StorageResponse::success(serde_json::json!({ "updated": true })),
                Err(error) => StorageResponse::error(&error),
            }
        }
        "update_screenshot_category" => {
            let screenshot_id = req
                .get("screenshot_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1);
            let category = req.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let category_confidence = req.get("category_confidence").and_then(|v| v.as_f64());

            if screenshot_id < 0 {
                return StorageResponse::error("Invalid screenshot_id");
            }
            if category.trim().is_empty() {
                return StorageResponse::error("category is required");
            }

            match storage.update_screenshot_category(screenshot_id, category, category_confidence) {
                Ok(updated) => StorageResponse::success(serde_json::json!({"updated": updated})),
                Err(e) => StorageResponse::error(&e),
            }
        }
        "list_screenshots_for_clustering" => {
            if !storage.is_session_valid() {
                return StorageResponse::error("AUTH_REQUIRED");
            }

            let start_ts = req.get("start_ts").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let end_ts = req.get("end_ts").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let offset = req.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = req
                .get("limit")
                .and_then(|v| v.as_i64())
                .unwrap_or(500)
                .min(1000);

            // If no time range given, use full range
            let (s, e) = if end_ts <= start_ts {
                (0.0_f64, 4102444800.0_f64) // epoch 0 to 2100-01-01
            } else {
                (start_ts, end_ts)
            };

            // Fast COUNT query (no decryption)
            let total = match storage.count_screenshots_by_time_range(s, e) {
                Ok(n) => n,
                Err(err) => return StorageResponse::error(&err),
            };

            // Paged unattended query: decrypt only clustering metadata and
            // force CNG silent mode so a state race can never display UI.
            match storage.get_screenshot_summaries_by_time_range_paged_silent(s, e, offset, limit) {
                Ok(records) => {
                    let ids: Vec<i64> = records.iter().map(|rec| rec.id).collect();
                    let ocr_batch_started = std::time::Instant::now();
                    let ocr_map = match storage.get_ocr_results_by_screenshot_ids_silent(&ids) {
                        Ok(map) => map,
                        Err(error) => return background_read_error_response(error),
                    };
                    tracing::debug!(
                        "[DIAG:CLUSTERING] batch OCR fetch ids={} elapsed={}ms",
                        ids.len(),
                        ocr_batch_started.elapsed().as_millis()
                    );
                    let page: Vec<serde_json::Value> = records
                        .into_iter()
                        .map(|rec| background_screenshot_with_ocr_json(rec, &ocr_map))
                        .collect();
                    StorageResponse::success(serde_json::json!({
                        "screenshots": page,
                        "total": total,
                    }))
                }
                Err(error) => background_read_error_response(error),
            }
        }

        // M2.5 step 5 retired three MiniLM mirror commands that lived here:
        // `upsert_minilm_derived_embeddings`, `report_minilm_import_debt`, and
        // `delete_minilm_derived_embeddings`. All three existed because Python
        // owned MiniLM inference and Rust held a copy that could fall behind.
        // Rust is now the only encoder, mirrors *to* Chroma through
        // `upsert_task_vectors`, and expires its own rows against SQLite
        // timestamps, so there is nothing left for Python to write or report
        // here. Removing them rather than leaving them inert matters: a handler
        // that still accepts vectors is a second writer for a store that is
        // supposed to have exactly one.
        "get_idle_state" => {
            use std::sync::atomic::Ordering;
            use tauri::Manager;
            match app_handle.try_state::<std::sync::Arc<crate::idle::IdleState>>() {
                Some(s) => StorageResponse::success(serde_json::json!({
                    "is_idle": s.is_idle.load(Ordering::SeqCst),
                    "idle_secs": s.idle_secs.load(Ordering::SeqCst),
                    "fullscreen_exclusive": s.fullscreen_exclusive.load(Ordering::SeqCst),
                    // Additive. The retained Python task-clustering scheduler
                    // gates on `is_idle` alone, which already accounts for
                    // battery; this lets diagnostics identify the gate signal.
                    "ac_connected": s.ac_connected.load(Ordering::SeqCst),
                })),
                None => StorageResponse::error("IdleState not initialised"),
            }
        }

        _ => StorageResponse::error(&format!("Unknown command: {}", command)),
    };

    if diag_start.elapsed().as_secs() >= 10 {
        tracing::warn!(
            "[DIAG:RIPC] command='{}' completed in {:?}",
            command,
            diag_start.elapsed()
        );
    }
    response
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn validate_reverse_ipc_request(
    req: &serde_json::Value,
    expected_auth_token: &str,
    last_seq_no: &mut Option<u64>,
) -> Result<(), String> {
    let provided = req
        .get("_auth_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Authentication failed".to_string())?;
    if !constant_time_eq(provided, expected_auth_token) {
        return Err("Authentication failed".to_string());
    }

    let seq_no = req
        .get("_seq_no")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "Invalid sequence number".to_string())?;
    if last_seq_no.map(|last| seq_no <= last).unwrap_or(false) {
        return Err("Replay detected".to_string());
    }
    *last_seq_no = Some(seq_no);
    Ok(())
}

/// 生成反向 IPC 管道名
pub fn generate_reverse_pipe_name() -> String {
    let mut rng = rand::thread_rng();
    let random_suffix: String = (0..32)
        .map(|_| format!("{:02x}", rand::Rng::gen::<u8>(&mut rng)))
        .collect();
    format!("carbon_storage_{}", random_suffix)
}

pub fn generate_reverse_ipc_auth_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// NMH Pipe Server

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Compute deterministic NMH pipe name from current user's Windows SID.
/// Both CarbonPaper and carbonpaper-nmh.exe use this same formula.
pub fn compute_nmh_pipe_name() -> Result<String, String> {
    let sid = get_current_user_sid()?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{}carbonpaper_nmh_salt", sid));
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);
    Ok(format!(r"\\.\pipe\carbon_nmh_{}", &hex_hash[..16]))
}

/// Get the current user's SID string via Windows API
fn get_current_user_sid() -> Result<String, String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: token and SID buffers are allocated from sizes returned by Windows; all
    // handles and `LocalAlloc` strings are released once, and every output pointer refers
    // to live writable storage for the duration of its synchronous call.
    unsafe {
        let mut token_handle = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle)
            .map_err(|e| format!("OpenProcessToken failed: {}", e))?;

        // Get required buffer size
        let mut return_length = 0u32;
        let _ = GetTokenInformation(token_handle, TokenUser, None, 0, &mut return_length);

        let mut buffer = vec![0u8; return_length as usize];
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buffer.as_mut_ptr() as *mut _),
            return_length,
            &mut return_length,
        )
        .map_err(|e| format!("GetTokenInformation failed: {}", e))?;

        let token_user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_string = PWSTR::null();
        ConvertSidToStringSidW(token_user.User.Sid, &mut sid_string)
            .map_err(|e| format!("ConvertSidToStringSid failed: {}", e))?;

        let result = sid_string
            .to_string()
            .map_err(|e| format!("SID string conversion failed: {}", e))?;

        // Free the allocated string
        windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(
            sid_string.0 as *mut _,
        ));
        let _ = windows::Win32::Foundation::CloseHandle(token_handle);

        Ok(result)
    }
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

/// NMH pipe server state
pub struct NmhPipeServer {
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl NmhPipeServer {
    pub fn new() -> Self {
        Self { shutdown_tx: None }
    }

    /// Start the NMH pipe server with auth token validation
    pub fn start(
        &mut self,
        storage: Arc<StorageState>,
        capture_state: Arc<CaptureState>,
        app_handle: tauri::AppHandle,
        auth_token: String,
    ) -> Result<(), String> {
        let pipe_name = compute_nmh_pipe_name()?;
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        tracing::info!("Starting NMH pipe server on {}", pipe_name);

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("Failed to create NMH runtime: {}", e);
                    return;
                }
            };

            rt.block_on(async move {
                let wide_pipe_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
                let mut first_pipe_instance = true;

                loop {
                    // Apply the same current-user ACL as the storage pipe and
                    // reject remote clients. Claim first-instance on the
                    // initial create so a squatted pipe name fails loudly.
                    let sec_ctx = match get_security_context() {
                        Ok(ctx) => ctx,
                        Err(e) => {
                            tracing::error!("Failed to get NMH pipe security attributes: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    };

                    let open_mode = if first_pipe_instance {
                        PIPE_ACCESS_DUPLEX
                            | windows::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED
                            | windows::Win32::Storage::FileSystem::FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        PIPE_ACCESS_DUPLEX
                            | windows::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED
                    };

                    // SAFETY: the pipe name is NUL-terminated; `sec_ctx` retains the
                    // security descriptor and ACL backing memory for the entire call.
                    let handle = unsafe {
                        windows::Win32::System::Pipes::CreateNamedPipeW(
                            windows::core::PCWSTR(wide_pipe_name.as_ptr()),
                            open_mode,
                            windows::Win32::System::Pipes::PIPE_TYPE_BYTE | windows::Win32::System::Pipes::PIPE_READMODE_BYTE | windows::Win32::System::Pipes::PIPE_WAIT | windows::Win32::System::Pipes::PIPE_REJECT_REMOTE_CLIENTS,
                            windows::Win32::System::Pipes::PIPE_UNLIMITED_INSTANCES,
                            1024 * 1024,
                            1024 * 1024,
                            0,
                            Some(&sec_ctx.sa),
                        )
                    };

                    if handle.is_invalid() {
                        // SAFETY: `GetLastError` reads thread-local Win32 state and takes
                        // no pointers or handles.
                        tracing::error!("Failed to create NMH pipe server: {:?}", unsafe { windows::Win32::Foundation::GetLastError() });
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }
                    first_pipe_instance = false;

                    // SAFETY: `handle` is valid and uniquely owned. On success ownership
                    // transfers to `NamedPipeServer`; on failure it is closed below.
                    let server = unsafe {
                        match NamedPipeServer::from_raw_handle(handle.0) {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!("Failed to convert raw handle to NMH pipe server: {}", e);
                                let _ = windows::Win32::Foundation::CloseHandle(handle);
                                continue;
                            }
                        }
                    };

                    tokio::select! {
                        _ = shutdown_rx.recv() => {
                            tracing::info!("NMH pipe server shutting down");
                            break;
                        }
                        result = server.connect() => {
                            if let Err(e) = result {
                                tracing::error!("NMH client connection failed: {}", e);
                                continue;
                            }

                            let storage_clone = storage.clone();
                            let capture_clone = capture_state.clone();
                            let app_clone = app_handle.clone();
                            let token_clone = auth_token.clone();
                            tokio::spawn(async move {
                                handle_nmh_client(server, storage_clone, capture_clone, app_clone, token_clone).await;
                            });
                        }
                    }
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

/// Handle a single NMH client connection
async fn handle_nmh_client(
    mut server: NamedPipeServer,
    storage: Arc<StorageState>,
    capture_state: Arc<CaptureState>,
    app_handle: tauri::AppHandle,
    expected_token: String,
) {
    // SAFETY: Tokio owns a live pipe handle for the duration of the call and `pid` points
    // to writable stack storage that Windows fills synchronously.
    let client_pid = unsafe {
        let mut pid: u32 = 0;
        let handle = HANDLE(server.as_raw_handle() as *mut _);
        if GetNamedPipeClientProcessId(handle, &mut pid).is_ok() {
            Some(pid)
        } else {
            None
        }
    };

    let buf = match read_ipc_frame(&mut server).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("NMH read error: {}", e);
            let response = StorageResponse::error(&e);
            let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
            let _ = write_ipc_frame(&mut server, &response_bytes).await;
            return;
        }
    };

    if buf.is_empty() {
        return;
    }

    let response = match serde_json::from_slice::<serde_json::Value>(&buf) {
        Ok(req) => {
            // Validate auth token
            let provided_token = req.get("auth_token").and_then(|t| t.as_str()).unwrap_or("");
            if !constant_time_eq(provided_token, &expected_token) {
                tracing::warn!("NMH auth token mismatch");
                StorageResponse::error("Authentication failed")
            } else {
                process_nmh_request(
                    &req,
                    client_pid,
                    storage.clone(),
                    capture_state.clone(),
                    app_handle.clone(),
                )
                .await
            }
        }
        Err(e) => StorageResponse::error(&format!("Invalid JSON: {} (bytes={})", e, buf.len())),
    };

    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
    if let Err(e) = write_ipc_frame(&mut server, &response_bytes).await {
        tracing::error!("NMH write error: {}", e);
    }
}

/// Process an NMH request (save_extension_screenshot)
async fn process_nmh_request(
    req: &serde_json::Value,
    client_pid: Option<u32>,
    storage: Arc<StorageState>,
    capture_state: Arc<CaptureState>,
    app_handle: tauri::AppHandle,
) -> StorageResponse {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");

    if crate::maintenance::is_active() && !crate::maintenance::reverse_ipc_command_allowed(command)
    {
        return StorageResponse::error(crate::maintenance::MAINTENANCE_IN_PROGRESS);
    }

    match command {
        "register_nmh" => {
            let browser_pid = req.get("browser_pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let nmh_pid = req.get("nmh_pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let cmd_pipe_name = req
                .get("cmd_pipe_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let browser_exe_path = req
                .get("browser_exe_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let browser_exe_name = req
                .get("browser_exe_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if browser_pid == 0 || nmh_pid == 0 || !cmd_pipe_name.starts_with(NMH_CMD_PIPE_PREFIX) {
                return StorageResponse::error("Invalid register_nmh request");
            }

            // The NMH connects to this pipe itself and is spawned by the
            // browser it claims (directly or via the cmd.exe NM launcher
            // wrapper), so the caller must sit below the claimed browser PID.
            let Some(client_pid) = client_pid else {
                return StorageResponse::error("Cannot verify NMH client process");
            };
            if !is_pid_descendant_of(client_pid, browser_pid) {
                tracing::warn!(
                    "[SECURITY] register_nmh rejected: client PID {} is not a descendant of claimed browser PID {}",
                    client_pid,
                    browser_pid
                );
                return StorageResponse::error("Invalid register_nmh request");
            }

            let now_ms = chrono::Utc::now().timestamp_millis();
            {
                let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
                upsert_session(
                    &mut sessions,
                    NmhSession {
                        browser_pid,
                        browser_exe_path,
                        browser_exe_name: browser_exe_name.clone(),
                        nmh_pid,
                        cmd_pipe_name,
                        registered_at_ms: now_ms,
                        last_seen_ms: now_ms,
                    },
                );
            }
            tracing::info!(
                "NMH session registered: browser={} pid={} nmh_pid={}",
                browser_exe_name,
                browser_pid,
                nmh_pid
            );
            StorageResponse::success(serde_json::json!({"registered": true}))
        }
        "unregister_nmh" => {
            let nmh_pid = req.get("nmh_pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let cmd_pipe_name = req
                .get("cmd_pipe_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            {
                let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
                remove_session(&mut sessions, nmh_pid, cmd_pipe_name);
            }
            tracing::info!("NMH session unregistered: nmh_pid={}", nmh_pid);
            StorageResponse::success(serde_json::json!({"unregistered": true}))
        }
        "save_extension_screenshot" => {
            // Keep the sender's session fresh (liveness signal)
            if let Some(nmh_pid) = req.get("nmh_pid").and_then(|v| v.as_u64()) {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
                for s in sessions.iter_mut() {
                    if s.nmh_pid == nmh_pid as u32 {
                        s.last_seen_ms = now_ms;
                    }
                }
            }

            // Check if capture is paused
            if capture_state
                .paused
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return StorageResponse::error("Capture is paused");
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

            match storage.save_screenshot_temp_bytes(&request, &jpeg_bytes) {
                Ok(result) => {
                    if result.status == "duplicate" {
                        return StorageResponse::success(serde_json::to_value(result).unwrap());
                    }

                    // Dispatch to OCR pipeline if we have a screenshot_id
                    if let Some(screenshot_id) = result.screenshot_id {
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
                                crate::ml_runtime::schedule_ocr_model_health_notification(
                                    app_clone.clone(),
                                );
                                tracing::error!(
                                    "Extension OCR failed for screenshot {}: {}",
                                    screenshot_id,
                                    e
                                );
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
}

static NMH_SESSIONS: once_cell::sync::Lazy<std::sync::Mutex<Vec<NmhSession>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(Vec::new()));

/// Only accept command pipes created by our NMH (random-suffix namespace).
const NMH_CMD_PIPE_PREFIX: &str = r"\\.\pipe\carbon_nmh_cmd_r_";

/// Insert or replace a session, keyed by nmh_pid (an NMH re-registering —
/// e.g. after an app restart rotated the token — replaces its old entry).
fn upsert_session(sessions: &mut Vec<NmhSession>, session: NmhSession) {
    sessions.retain(|s| s.nmh_pid != session.nmh_pid);
    sessions.push(session);
}

/// Remove a session by nmh_pid + cmd_pipe_name.
fn remove_session(sessions: &mut Vec<NmhSession>, nmh_pid: u32, cmd_pipe_name: &str) {
    sessions.retain(|s| !(s.nmh_pid == nmh_pid && s.cmd_pipe_name == cmd_pipe_name));
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

/// Query the full executable path of a process by PID (empty on failure).
fn query_process_image_path(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the process handle is checked and owned locally; the UTF-16 buffer and size
    // pointer are valid for the query, and the handle is closed exactly once.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if result.is_ok() && size > 0 {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

/// Drop sessions whose browser process is gone, or whose PID now belongs to
/// a different executable (PID-reuse guard).
pub fn prune_dead_sessions() {
    let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions.retain(|s| match query_process_image_path(s.browser_pid) {
        Some(path) => {
            s.browser_exe_path.is_empty() || path.eq_ignore_ascii_case(&s.browser_exe_path)
        }
        None => false,
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
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut pipe = ClientOptions::new()
            .open(&pipe_name)
            .map_err(|e| format!("Cannot open NMH cmd pipe: {}", e))?;

        let request = serde_json::json!({"command": "request_capture"});
        let data =
            serde_json::to_vec(&request).map_err(|e| format!("Serialization failed: {}", e))?;

        pipe.write_all(&data)
            .await
            .map_err(|e| format!("Pipe write failed: {}", e))?;

        // The NMH replies ok only after successfully forwarding the request
        // to the extension over its Native Messaging port.
        let mut response_buf = vec![0u8; 1024];
        let n = pipe
            .read(&mut response_buf)
            .await
            .map_err(|e| format!("Pipe read failed: {}", e))?;
        let response: serde_json::Value = serde_json::from_slice(&response_buf[..n])
            .map_err(|e| format!("Invalid NMH cmd response: {}", e))?;
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
        remove_session(&mut sessions, session.nmh_pid, &session.cmd_pipe_name);
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
    use std::collections::HashMap;

    #[test]
    fn test_storage_response_success() {
        let resp = StorageResponse::success(serde_json::json!({"id": 1}));
        assert_eq!(resp.status, "success");
        assert!(resp.error.is_none());
        assert!(resp.data.is_some());
        assert_eq!(resp.data.unwrap()["id"], 1);
    }

    #[test]
    fn test_storage_response_error() {
        let resp = StorageResponse::error("something failed");
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap(), "something failed");
        assert!(resp.data.is_none());
    }

    #[test]
    fn test_storage_response_success_serialization() {
        let resp = StorageResponse::success(serde_json::json!({"key": "value"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"success\""));
        assert!(json.contains("\"key\":\"value\""));
        // error field should be skipped (skip_serializing_if = "Option::is_none")
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_storage_response_error_serialization() {
        let resp = StorageResponse::error("bad request");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("\"error\":\"bad request\""));
        // data field should be skipped
        assert!(!json.contains("\"data\""));
    }

    #[test]
    fn background_auth_error_uses_stable_auth_required_code() {
        let response = background_read_error_response(BackgroundReadError::AuthRequired);
        assert_eq!(response.status, "error");
        assert_eq!(response.error.as_deref(), Some("AUTH_REQUIRED"));
        assert!(response.data.is_none());
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
        remove_session(&mut sessions, 1, &pipe);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].nmh_pid, 2);
        // Wrong pipe name doesn't remove
        remove_session(&mut sessions, 2, r"\\.\pipe\other");
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
    fn test_generate_reverse_pipe_name_format() {
        let name = generate_reverse_pipe_name();
        assert!(
            name.starts_with("carbon_storage_"),
            "pipe name should start with 'carbon_storage_': {}",
            name
        );
        // The random suffix is 32 bytes * 2 hex chars = 64 chars
        assert_eq!(name.len(), "carbon_storage_".len() + 64);
    }

    #[test]
    fn test_generate_reverse_pipe_name_unique() {
        let name1 = generate_reverse_pipe_name();
        let name2 = generate_reverse_pipe_name();
        assert_ne!(name1, name2, "Two generated pipe names should be different");
    }

    #[test]
    fn test_reverse_ipc_auth_rejects_missing_token() {
        let req = serde_json::json!({
            "command": "get_auth_status",
            "_seq_no": 1
        });
        let mut last = None;
        let result = validate_reverse_ipc_request(&req, "secret", &mut last);
        assert!(result.is_err());
    }

    #[test]
    fn test_reverse_ipc_auth_rejects_wrong_token() {
        let req = serde_json::json!({
            "command": "get_auth_status",
            "_auth_token": "wrong",
            "_seq_no": 1
        });
        let mut last = None;
        let result = validate_reverse_ipc_request(&req, "secret", &mut last);
        assert!(result.is_err());
    }

    #[test]
    fn test_reverse_ipc_auth_rejects_replayed_sequence() {
        let mut last = None;
        let first = serde_json::json!({
            "command": "get_auth_status",
            "_auth_token": "secret",
            "_seq_no": 2
        });
        validate_reverse_ipc_request(&first, "secret", &mut last).unwrap();

        let replay = serde_json::json!({
            "command": "get_auth_status",
            "_auth_token": "secret",
            "_seq_no": 2
        });
        let result = validate_reverse_ipc_request(&replay, "secret", &mut last);
        assert_eq!(result.unwrap_err(), "Replay detected");
    }

    #[test]
    fn test_reverse_ipc_auth_accepts_monotonic_sequence() {
        let mut last = None;
        for seq_no in [1, 2] {
            let req = serde_json::json!({
                "command": "get_auth_status",
                "_auth_token": "secret",
                "_seq_no": seq_no
            });
            validate_reverse_ipc_request(&req, "secret", &mut last).unwrap();
        }
        assert_eq!(last, Some(2));
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
    fn test_decrypt_many_response_contract_shape() {
        let resp = StorageResponse::success(serde_json::json!({
            "decrypted_list": ["plain-1", "plain-2"],
            "error_count": 0
        }));
        let as_value = serde_json::to_value(resp).unwrap();

        assert_eq!(as_value["status"], "success");
        assert!(as_value["data"]["decrypted_list"].is_array());
        assert!(as_value["data"]["error_count"].is_number());
    }

    #[test]
    fn test_list_screenshots_response_contract_shape() {
        let resp = StorageResponse::success(serde_json::json!({
            "screenshots": [
                {
                    "id": 1,
                    "process_name": "code.exe",
                    "window_title": "Editor",
                    "ocr_text": "hello",
                    "timestamp": 123.0,
                    "category": "Development"
                }
            ],
            "total": 1
        }));
        let as_value = serde_json::to_value(resp).unwrap();

        assert_eq!(as_value["status"], "success");
        assert!(as_value["data"]["screenshots"].is_array());
        assert!(as_value["data"]["total"].is_number());
        let first = &as_value["data"]["screenshots"][0];
        assert!(first.get("process_name").is_some());
        assert!(first.get("window_title").is_some());
        assert!(first.get("ocr_text").is_some());
    }

    #[test]
    fn test_screenshot_record_with_ocr_json_uses_batch_map() {
        let mut ocr_map = HashMap::new();
        ocr_map.insert(42, "alpha beta".to_string());

        let rec = ScreenshotRecord {
            id: 42,
            image_path: "screenshots/42.jpg.enc".to_string(),
            image_hash: "h42".to_string(),
            width: Some(100),
            height: Some(80),
            window_title: Some("Editor".to_string()),
            process_name: Some("code.exe".to_string()),
            created_at: "2026-06-16 12:00:00".to_string(),
            metadata: None,
            timestamp: Some(1_797_331_200_000),
            source: None,
            page_url: None,
            page_icon: None,
            visible_links: None,
            category: Some("Development".to_string()),
            category_confidence: Some(0.9),
        };

        let value = screenshot_record_with_ocr_json(rec, &ocr_map);

        assert_eq!(value["id"], 42);
        assert_eq!(value["ocr_text"], "alpha beta");
        assert_eq!(value["process_name"], "code.exe");
        assert_eq!(value["window_title"], "Editor");
        assert_eq!(value["category"], "Development");
    }

    #[test]
    fn test_screenshot_record_with_ocr_json_missing_ocr_is_empty() {
        let rec = ScreenshotRecord {
            id: 7,
            image_path: "screenshots/7.jpg.enc".to_string(),
            image_hash: "h7".to_string(),
            width: None,
            height: None,
            window_title: None,
            process_name: None,
            created_at: "2026-06-16 12:00:00".to_string(),
            metadata: None,
            timestamp: None,
            source: None,
            page_url: None,
            page_icon: None,
            visible_links: None,
            category: None,
            category_confidence: None,
        };

        let value = screenshot_record_with_ocr_json(rec, &HashMap::new());

        assert_eq!(value["ocr_text"], "");
        assert_eq!(value["process_name"], "");
        assert_eq!(value["window_title"], "");
        assert_eq!(value["timestamp"], 0.0);
    }
}
