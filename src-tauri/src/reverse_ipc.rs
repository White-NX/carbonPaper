//! Restful API Server for Storage - Windows Named Pipe Implementation
//!
//! This module implements a reverse IPC server using Windows Named Pipes
//! to allow external processes (like Python scripts) to send storage-related
//! commands to the Rust backend. This is used for scenarios like browser
//! extensions or other integrations that need to save screenshots and OCR
//! results without going through the full capture pipeline.
//!
use crate::capture::CaptureState;
use crate::capture::OcrImageCache;
use crate::monitor::MonitorState;
use crate::reverse_ipc_protocol::{
    read_ipc_frame, write_ipc_binary_frame, write_ipc_frame, StorageResponse,
};
#[cfg(test)]
use crate::storage::ScreenshotRecord;
use crate::storage::{
    BackgroundReadError, BackgroundScreenshotSummary, OcrResultInput, SaveScreenshotRequest,
    StorageState,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use tauri::Manager;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, Semaphore};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

/// Commands that Python can send to Rust via the reverse IPC named pipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command")]
#[allow(dead_code)]
pub enum StorageCommand {
    /// Save a screenshot with image data, metadata, and optional OCR results.
    #[serde(rename = "save_screenshot")]
    SaveScreenshot {
        image_data: String,
        image_hash: String,
        width: i32,
        height: i32,
        window_title: Option<String>,
        process_name: Option<String>,
        metadata: Option<serde_json::Value>,
        ocr_results: Option<Vec<OcrResultInput>>,
    },
    /// Save a screenshot as pending, awaiting a subsequent commit or abort.
    #[serde(rename = "save_screenshot_temp")]
    SaveScreenshotTemp {
        image_data: String,
        image_hash: String,
        width: i32,
        height: i32,
        window_title: Option<String>,
        process_name: Option<String>,
        metadata: Option<serde_json::Value>,
    },
    /// Commit a pending screenshot and write its OCR results.
    #[serde(rename = "commit_screenshot")]
    CommitScreenshot {
        screenshot_id: String,
        ocr_results: Option<Vec<OcrResultInput>>,
    },
    /// Abort a pending screenshot (delete temp files and roll back the DB record).
    #[serde(rename = "abort_screenshot")]
    AbortScreenshot {
        screenshot_id: String,
        reason: Option<String>,
    },
    /// Retrieve the RSA public key for encryption.
    #[serde(rename = "get_public_key")]
    GetPublicKey,
    /// Encrypt plaintext data for storage in ChromaDB.
    #[serde(rename = "encrypt_for_chromadb")]
    EncryptForChromaDb { plaintext: String },
    /// Decrypt data previously encrypted for ChromaDB.
    #[serde(rename = "decrypt_from_chromadb")]
    DecryptFromChromaDb { encrypted: String },
    /// Check whether a screenshot with the given hash already exists.
    #[serde(rename = "screenshot_exists")]
    ScreenshotExists { image_hash: String },
    #[serde(rename = "set_ocr_postprocess_status")]
    SetOcrPostprocessStatus {
        screenshot_id: i64,
        status: String,
        error: Option<String>,
    },
    #[serde(rename = "record_ocr_postprocess_retry")]
    RecordOcrPostprocessRetry { screenshot_id: i64, error: String },
}

// Use OcrResultInput from crate::storage to keep a single canonical type

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
        ocr_cache: OcrImageCache,
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

                    // 应用安全描述符和 PIPE_REJECT_REMOTE_CLIENTS。
                    // Use byte-mode pipes for robust streaming of large JSON payloads.
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
                        tracing::error!("Failed to create pipe via Win32 API: {:?}", unsafe { windows::Win32::Foundation::GetLastError() });
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    // 将 Raw Handle 转换为 tokio NamedPipeServer
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

                    // 等待客户端连接或关闭信号
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
                            let ocr_cache_clone = ocr_cache.clone();
                            let app_clone = app_handle.clone();
                            let auth_token_clone = auth_token.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                handle_client(server, storage_clone, ocr_cache_clone, app_clone, auth_token_clone).await;
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

/// 处理单个客户端连接
async fn handle_client(
    mut server: NamedPipeServer,
    storage: Arc<StorageState>,
    ocr_cache: OcrImageCache,
    app_handle: tauri::AppHandle,
    expected_auth_token: String,
) {
    // 安全校验：验证客户端 PID
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

        if req.get("command").and_then(|c| c.as_str()) == Some("get_temp_image") {
            match get_temp_image_bytes(&req, &storage, &ocr_cache) {
                Ok((image_bytes, mime_type)) => {
                    let metadata = StorageResponse::success(serde_json::json!({
                        "mime_type": mime_type,
                        "binary_body_len": image_bytes.len(),
                        "binary_frame": true,
                    }));
                    let metadata_bytes = serde_json::to_vec(&metadata).unwrap_or_default();
                    if let Err(e) = write_ipc_frame(&mut server, &metadata_bytes).await {
                        tracing::error!("Write binary metadata error: {}", e);
                        return;
                    }
                    if let Err(e) = write_ipc_binary_frame(&mut server, &image_bytes).await {
                        tracing::error!("Write binary body error: {}", e);
                        return;
                    }
                }
                Err(e) => {
                    let response = StorageResponse::error(&e);
                    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                    if let Err(e) = write_ipc_frame(&mut server, &response_bytes).await {
                        tracing::error!("Write error: {}", e);
                        return;
                    }
                }
            }
        } else {
            let response = process_request(&req, &storage, &app_handle);

            // 发送响应
            let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
            if let Err(e) = write_ipc_frame(&mut server, &response_bytes).await {
                tracing::error!("Write error: {}", e);
                return;
            }
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
fn process_request(
    req: &serde_json::Value,
    storage: &StorageState,
    app_handle: &tauri::AppHandle,
) -> StorageResponse {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let diag_start = std::time::Instant::now();

    let response = match command {
        "save_screenshot" => {
            // 解析保存截图请求
            let request = match serde_json::from_value::<SaveScreenshotRequest>(req.clone()) {
                Ok(r) => r,
                Err(e) => return StorageResponse::error(&format!("Invalid request: {}", e)),
            };

            match storage.save_screenshot(&request) {
                Ok(result) => StorageResponse::success(serde_json::to_value(result).unwrap()),
                Err(e) => StorageResponse::error(&e),
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

        "screenshot_exists" => {
            let image_hash = req.get("image_hash").and_then(|h| h.as_str()).unwrap_or("");

            match storage.screenshot_exists(image_hash) {
                Ok(exists) => StorageResponse::success(serde_json::json!({
                    "exists": exists
                })),
                Err(e) => StorageResponse::error(&e),
            }
        }
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
        "save_screenshot_temp" => {
            let request = match serde_json::from_value::<SaveScreenshotRequest>(req.clone()) {
                Ok(r) => r,
                Err(e) => return StorageResponse::error(&format!("Invalid request: {}", e)),
            };

            match storage.save_screenshot_temp(&request) {
                Ok(result) => StorageResponse::success(serde_json::to_value(result).unwrap()),
                Err(e) => StorageResponse::error(&e),
            }
        }
        "commit_screenshot" => {
            // Accept screenshot_id as number or string
            let screenshot_id_val = req.get("screenshot_id").cloned();
            let screenshot_id = match screenshot_id_val {
                Some(v) => {
                    if v.is_i64() {
                        v.as_i64().unwrap_or(-1)
                    } else if v.is_u64() {
                        v.as_u64().map(|x| x as i64).unwrap_or(-1)
                    } else if v.is_string() {
                        v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1)
                    } else {
                        -1
                    }
                }
                None => -1,
            };

            if screenshot_id < 0 {
                return StorageResponse::error("Invalid screenshot_id");
            }

            // Parse ocr_results strictly: fail the whole request if any entry is invalid
            // If parsing fails, ensure we abort the pending screenshot to avoid leaking .pending files
            let ocr_results = match req.get("ocr_results") {
                Some(v) => {
                    let arr = match v.as_array() {
                        Some(arr) => arr,
                        None => {
                            let msg = "ocr_results must be an array when provided";
                            if let Err(e) = storage.abort_screenshot(screenshot_id, Some(msg)) {
                                tracing::error!(
                                    "Failed to abort screenshot {}: {}",
                                    screenshot_id,
                                    e
                                );
                            }
                            return StorageResponse::error(msg);
                        }
                    };

                    let mut results = Vec::with_capacity(arr.len());
                    for (idx, item) in arr.iter().enumerate() {
                        match serde_json::from_value::<OcrResultInput>(item.clone()) {
                            Ok(parsed) => results.push(parsed),
                            Err(e) => {
                                let msg = format!("Invalid ocr_results[{}]: {}", idx, e);
                                if let Err(abort_err) =
                                    storage.abort_screenshot(screenshot_id, Some(&msg))
                                {
                                    tracing::error!(
                                        "Failed to abort screenshot {}: {}",
                                        screenshot_id,
                                        abort_err
                                    );
                                }
                                return StorageResponse::error(&msg);
                            }
                        }
                    }

                    Some(results)
                }
                None => None,
            };

            // Extract category from request (may be provided by Python classification)
            let category = req
                .get("category")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let category_confidence = req.get("category_confidence").and_then(|v| v.as_f64());

            match storage.commit_screenshot(
                screenshot_id,
                ocr_results.as_ref(),
                category.as_deref(),
                category_confidence,
            ) {
                Ok(result) => StorageResponse::success(serde_json::to_value(result).unwrap()),
                Err(e) => StorageResponse::error(&e),
            }
        }
        "abort_screenshot" => {
            let screenshot_id_val = req.get("screenshot_id").cloned();
            let screenshot_id = match screenshot_id_val {
                Some(v) => {
                    if v.is_i64() {
                        v.as_i64().unwrap_or(-1)
                    } else if v.is_u64() {
                        v.as_u64().map(|x| x as i64).unwrap_or(-1)
                    } else if v.is_string() {
                        v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1)
                    } else {
                        -1
                    }
                }
                None => -1,
            };

            if screenshot_id < 0 {
                return StorageResponse::error("Invalid screenshot_id");
            }

            let reason = req
                .get("reason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            match storage.abort_screenshot(screenshot_id, reason.as_deref()) {
                Ok(result) => StorageResponse::success(serde_json::to_value(result).unwrap()),
                Err(e) => StorageResponse::error(&e),
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

        "get_screenshots_with_ocr_by_ids" => {
            if !storage.is_session_valid() {
                return StorageResponse::error("AUTH_REQUIRED");
            }

            let ids: Vec<i64> = req
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default();

            if ids.is_empty() {
                return StorageResponse::success(serde_json::json!({ "screenshots": [] }));
            }

            // Cap the batch size to bound work for a single IPC request.
            const MAX_BATCH: usize = 500;
            let ids: Vec<i64> = ids.into_iter().take(MAX_BATCH).collect();

            // Fetch OCR results in silent mode. Authentication loss aborts the
            // entire batch instead of retrying CNG once per OCR row.
            let ocr_map = match storage.get_ocr_results_by_screenshot_ids_silent(&ids) {
                Ok(map) => map,
                Err(error) => return background_read_error_response(error),
            };

            match storage.get_screenshot_summaries_by_ids_silent(&ids) {
                Ok(records) => {
                    let out: Vec<serde_json::Value> = records
                        .into_iter()
                        .map(|rec| background_screenshot_with_ocr_json(rec, &ocr_map))
                        .collect();
                    StorageResponse::success(serde_json::json!({ "screenshots": out }))
                }
                Err(error) => background_read_error_response(error),
            }
        }

        // ============ Smart Cluster reverse IPC ============
        "get_idle_state" => {
            use std::sync::atomic::Ordering;
            use tauri::Manager;
            match app_handle.try_state::<std::sync::Arc<crate::idle::IdleState>>() {
                Some(s) => StorageResponse::success(serde_json::json!({
                    "is_idle": s.is_idle.load(Ordering::SeqCst),
                    "idle_secs": s.idle_secs.load(Ordering::SeqCst),
                    "fullscreen_exclusive": s.fullscreen_exclusive.load(Ordering::SeqCst),
                })),
                None => StorageResponse::error("IdleState not initialised"),
            }
        }

        "smart_cluster_list_enabled" => match storage.list_smart_clusters() {
            Ok(clusters) => {
                let enabled: Vec<serde_json::Value> = clusters
                    .into_iter()
                    .filter(|c| c.enabled)
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "anchor_text": c.anchor_text,
                            "threshold": c.threshold,
                        })
                    })
                    .collect();
                StorageResponse::success(serde_json::json!({ "clusters": enabled }))
            }
            Err(e) => StorageResponse::error(&e),
        },

        "smart_cluster_enqueue_pending" => {
            let id = req
                .get("screenshot_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if id <= 0 {
                return StorageResponse::error("missing screenshot_id");
            }
            match storage.enqueue_smart_cluster_pending(id) {
                Ok(()) => StorageResponse::success(serde_json::json!({ "ok": true })),
                Err(e) => StorageResponse::error(&e),
            }
        }

        "smart_cluster_peek_pending" => {
            let limit = req
                .get("limit")
                .and_then(|v| v.as_i64())
                .unwrap_or(32)
                .clamp(1, 256);
            match storage.peek_smart_cluster_pending_batch(limit) {
                Ok(ids) => StorageResponse::success(serde_json::json!({ "ids": ids })),
                Err(e) => StorageResponse::error(&e),
            }
        }

        "smart_cluster_delete_pending" => {
            let ids: Vec<i64> = req
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default();
            if ids.is_empty() {
                return StorageResponse::success(serde_json::json!({ "ok": true, "deleted": 0 }));
            }
            // Cap to bound per-IPC work; the worker batch is ≤256 so this is
            // a defence-in-depth check, not a normal-case limit.
            const MAX_BATCH: usize = 1000;
            let ids: Vec<i64> = ids.into_iter().take(MAX_BATCH).collect();
            let count = ids.len() as i64;
            match storage.delete_smart_cluster_pending_ids(&ids) {
                Ok(()) => {
                    StorageResponse::success(serde_json::json!({ "ok": true, "deleted": count }))
                }
                Err(e) => StorageResponse::error(&e),
            }
        }

        "smart_cluster_count_pending" => match storage.count_smart_cluster_pending() {
            Ok(n) => StorageResponse::success(serde_json::json!({ "count": n })),
            Err(e) => StorageResponse::error(&e),
        },

        "smart_cluster_record_assignment" => {
            let cluster_id = req
                .get("smart_cluster_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let screenshot_id = req
                .get("screenshot_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let score = req
                .get("rerank_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if cluster_id <= 0 || screenshot_id <= 0 {
                return StorageResponse::error("missing smart_cluster_id or screenshot_id");
            }
            match storage.record_smart_cluster_assignment(cluster_id, screenshot_id, score) {
                Ok(()) => StorageResponse::success(serde_json::json!({ "ok": true })),
                Err(e) => StorageResponse::error(&e),
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

fn parse_screenshot_id(req: &serde_json::Value) -> Result<i64, String> {
    let Some(v) = req.get("screenshot_id") else {
        return Err("Invalid screenshot_id".to_string());
    };
    let screenshot_id = if v.is_i64() {
        v.as_i64().unwrap_or(-1)
    } else if v.is_u64() {
        v.as_u64().map(|x| x as i64).unwrap_or(-1)
    } else if v.is_string() {
        v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1)
    } else {
        -1
    };
    if screenshot_id < 0 {
        Err("Invalid screenshot_id".to_string())
    } else {
        Ok(screenshot_id)
    }
}

fn get_temp_image_bytes(
    req: &serde_json::Value,
    storage: &StorageState,
    ocr_cache: &OcrImageCache,
) -> Result<(Vec<u8>, String), String> {
    let screenshot_id = parse_screenshot_id(req)?;

    let cached = {
        let cache = ocr_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.get(&screenshot_id).cloned()
    };

    if let Some(jpeg_bytes) = cached {
        return Ok((jpeg_bytes, "image/jpeg".to_string()));
    }

    match storage.get_screenshot_by_id(screenshot_id) {
        Ok(Some(record)) => {
            let (bytes, mime) = storage
                .read_image_bytes(&record.image_path)
                .map_err(|e| format!("Failed to read image: {}", e))?;
            Ok((bytes, mime))
        }
        Ok(None) => Err("Screenshot not found".to_string()),
        Err(e) => Err(e),
    }
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
                loop {
                    let server = match ServerOptions::new()
                        .first_pipe_instance(false)
                        .create(&pipe_name)
                    {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("Failed to create NMH pipe server: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
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
            if provided_token != expected_token {
                tracing::warn!("NMH auth token mismatch");
                StorageResponse::error("Authentication failed")
            } else {
                process_nmh_request(
                    &req,
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
    storage: Arc<StorageState>,
    capture_state: Arc<CaptureState>,
    app_handle: tauri::AppHandle,
) -> StorageResponse {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");

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
                Some(d) => d.to_string(),
                None => return StorageResponse::error("Missing image_data"),
            };
            let image_hash = match req.get("image_hash").and_then(|v| v.as_str()) {
                Some(h) => h.to_string(),
                None => return StorageResponse::error("Missing image_hash"),
            };
            let width = req.get("width").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let height = req.get("height").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
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

            // OCR queue backpressure check (same logic as capture loop)
            let in_flight = capture_state
                .in_flight_ocr_count
                .load(std::sync::atomic::Ordering::SeqCst);
            let capture_on_busy = capture_state
                .capture_on_ocr_busy
                .load(std::sync::atomic::Ordering::SeqCst);
            let max_queue = capture_state
                .ocr_queue_max_size
                .load(std::sync::atomic::Ordering::SeqCst);

            if !capture_on_busy && in_flight > 0 {
                return StorageResponse::error("OCR queue busy (conservative mode)");
            } else if in_flight > max_queue {
                return StorageResponse::error("OCR queue full");
            }

            // Build metadata with process_icon (same mechanism as capture loop)
            let metadata = Some(serde_json::json!({
                "process_icon": page_icon,
            }));

            let request = SaveScreenshotRequest {
                image_data: image_data.clone(),
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

            match storage.save_screenshot_temp(&request) {
                Ok(result) => {
                    if result.status == "duplicate" {
                        return StorageResponse::success(serde_json::to_value(result).unwrap());
                    }

                    // Dispatch to OCR pipeline if we have a screenshot_id
                    if let Some(screenshot_id) = result.screenshot_id {
                        // Decode image bytes for OCR cache
                        if let Ok(jpeg_bytes) = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            &image_data,
                        ) {
                            // Store in OCR cache so Python can fetch via get_temp_image
                            {
                                let mut cache = capture_state
                                    .ocr_image_cache
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                cache.insert(screenshot_id, jpeg_bytes.clone());
                            }

                            // Spawn async OCR task
                            let storage_arc = storage.clone();
                            let capture_arc = capture_state.clone();
                            let app_clone = app_handle.clone();
                            let window_title = page_title.unwrap_or_default();
                            let timestamp_ms = chrono::Utc::now().timestamp_millis();

                            // Increment in-flight OCR counter
                            capture_state
                                .in_flight_ocr_count
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                            tokio::spawn(async move {
                                let route = crate::capture::OcrRouteConfig::from_registry();
                                let result = process_extension_ocr(
                                    &app_clone,
                                    &storage_arc,
                                    screenshot_id,
                                    &jpeg_bytes,
                                    &image_hash,
                                    &window_title,
                                    &browser_name,
                                    timestamp_ms,
                                    route,
                                )
                                .await;

                                // Remove from OCR cache
                                {
                                    let mut cache = capture_arc
                                        .ocr_image_cache
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    cache.remove(&screenshot_id);
                                }

                                // Decrement in-flight OCR counter
                                capture_arc
                                    .in_flight_ocr_count
                                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

                                if let Err(e) = result {
                                    tracing::error!(
                                        "Extension OCR failed for screenshot {}: {}",
                                        screenshot_id,
                                        e
                                    );
                                    if let Err(commit_err) = storage_arc.commit_screenshot(
                                        screenshot_id,
                                        None,
                                        None,
                                        None,
                                    ) {
                                        tracing::error!(
                                            "Failed to preserve extension screenshot: {}",
                                            commit_err
                                        );
                                    }
                                    let _ = storage_arc.set_ocr_status(
                                        screenshot_id,
                                        "failed",
                                        Some(if route.use_rust { "rust" } else { "python" }),
                                        Some("ppocrv5-ch-mobile"),
                                        None,
                                        Some(&e),
                                        None,
                                    );
                                }
                            });
                        }
                    }

                    StorageResponse::success(serde_json::to_value(result).unwrap())
                }
                Err(e) => StorageResponse::error(&e),
            }
        }
        _ => StorageResponse::error(&format!("Unknown NMH command: {}", command)),
    }
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

/// Request the browser extension behind `session` to capture its current tab.
/// Opens the session's command pipe and sends a `request_capture` command.
/// On failure the session is dropped from the table (dead pipe) so the
/// capture loop falls back to normal screen capture immediately.
pub async fn request_extension_capture_session(session: &NmhSession) -> Result<(), String> {
    let pipe_name = session.cmd_pipe_name.clone();

    tracing::debug!(
        "request_extension_capture: browser={} pid={} pipe={}",
        session.browser_exe_name,
        session.browser_pid,
        pipe_name
    );

    // Run blocking pipe I/O on a separate thread
    let result: Result<(), String> = tokio::task::spawn_blocking(move || {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};

        let mut pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe_name)
            .map_err(|e| format!("Cannot open NMH cmd pipe: {}", e))?;

        let request = serde_json::json!({"command": "request_capture"});
        let data =
            serde_json::to_vec(&request).map_err(|e| format!("Serialization failed: {}", e))?;

        pipe.write_all(&data)
            .map_err(|e| format!("Pipe write failed: {}", e))?;
        pipe.flush()
            .map_err(|e| format!("Pipe flush failed: {}", e))?;

        // The NMH replies ok only after successfully forwarding the request
        // to the extension over its Native Messaging port.
        let mut response_buf = vec![0u8; 1024];
        let n = pipe
            .read(&mut response_buf)
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
    })
    .await
    .map_err(|e| format!("Task join failed: {}", e))?;

    if result.is_err() {
        let mut sessions = NMH_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        remove_session(&mut sessions, session.nmh_pid, &session.cmd_pipe_name);
    }
    result
}

/// Send extension screenshot to Python OCR pipeline and commit results
async fn process_extension_ocr(
    app: &tauri::AppHandle,
    storage: &StorageState,
    screenshot_id: i64,
    jpeg_bytes: &[u8],
    image_hash: &str,
    window_title: &str,
    process_name: &str,
    timestamp_ms: i64,
    route: crate::capture::OcrRouteConfig,
) -> Result<(), String> {
    let provider = if route.use_rust && route.use_directml_beta {
        "directml_beta"
    } else if route.use_rust {
        "cpu"
    } else {
        "legacy_python"
    };
    storage.set_ocr_status(
        screenshot_id,
        "running",
        Some(if route.use_rust { "rust" } else { "python" }),
        Some("ppocrv5-ch-mobile"),
        Some(provider),
        None,
        None,
    )?;
    crate::capture::process_ocr_inner(
        app,
        storage,
        screenshot_id,
        jpeg_bytes,
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
    fn test_storage_command_deserialize_save_screenshot_temp() {
        let payload = serde_json::json!({
            "command": "save_screenshot_temp",
            "image_data": "base64",
            "image_hash": "h123",
            "width": 1920,
            "height": 1080,
            "window_title": "Editor",
            "process_name": "code.exe",
            "metadata": {"k": "v"}
        });

        let cmd: StorageCommand = serde_json::from_value(payload).unwrap();
        match cmd {
            StorageCommand::SaveScreenshotTemp {
                image_hash,
                width,
                height,
                ..
            } => {
                assert_eq!(image_hash, "h123");
                assert_eq!(width, 1920);
                assert_eq!(height, 1080);
            }
            _ => panic!("expected SaveScreenshotTemp"),
        }
    }

    #[test]
    fn test_storage_command_deserialize_commit_screenshot() {
        let payload = serde_json::json!({
            "command": "commit_screenshot",
            "screenshot_id": "42",
            "ocr_results": []
        });

        let cmd: StorageCommand = serde_json::from_value(payload).unwrap();
        match cmd {
            StorageCommand::CommitScreenshot {
                screenshot_id,
                ocr_results,
            } => {
                assert_eq!(screenshot_id, "42");
                assert!(ocr_results.is_some());
            }
            _ => panic!("expected CommitScreenshot"),
        }
    }

    #[test]
    fn test_storage_command_unknown_tag_rejected() {
        let payload = serde_json::json!({
            "command": "list_screenshots_for_clustering",
            "start_ts": 0,
            "end_ts": 1
        });

        let result: Result<StorageCommand, _> = serde_json::from_value(payload);
        assert!(result.is_err());
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
