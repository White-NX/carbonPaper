//! Restful API Server for Storage - Windows Named Pipe Implementation
//!
//! This module implements a reverse IPC server using Windows Named Pipes 
//! to allow external processes (like Python scripts) to send storage-related 
//! commands to the Rust backend. This is used for scenarios like browser 
//! extensions or other integrations that need to save screenshots and OCR 
//! results without going through the full capture pipeline.
//! 
use crate::capture::OcrImageCache;
use crate::capture::CaptureState;
use crate::monitor::MonitorState;
use crate::storage::{SaveScreenshotRequest, StorageState, OcrResultInput};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;

/// 来自 Python 的存储请求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command")]
pub enum StorageCommand {
    /// 保存截图
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
    /// 临时保存截图（pending），等待后续 commit/abort
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
    /// 提交之前临时保存的截图并写入 OCR 结果
    #[serde(rename = "commit_screenshot")]
    CommitScreenshot {
        screenshot_id: String,
        ocr_results: Option<Vec<OcrResultInput>>,
    },
    /// 中止之前临时保存的截图（删除临时文件并回滚记录）
    #[serde(rename = "abort_screenshot")]
    AbortScreenshot {
        screenshot_id: String,
        reason: Option<String>,
    },
    /// 获取公钥
    #[serde(rename = "get_public_key")]
    GetPublicKey,
    /// 加密数据（用于 ChromaDB）
    #[serde(rename = "encrypt_for_chromadb")]
    EncryptForChromaDb {
        plaintext: String,
    },
    /// 解密数据
    #[serde(rename = "decrypt_from_chromadb")]
    DecryptFromChromaDb {
        encrypted: String,
    },
    /// 检查截图是否存在
    #[serde(rename = "screenshot_exists")]
    ScreenshotExists {
        image_hash: String,
    },
}

// Use OcrResultInput from crate::storage to keep a single canonical type

/// 存储响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl StorageResponse {
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            status: "success".to_string(),
            error: None,
            data: Some(data),
        }
    }
    
    pub fn error(msg: &str) -> Self {
        Self {
            status: "error".to_string(),
            error: Some(msg.to_string()),
            data: None,
        }
    }
}

/// 反向 IPC 服务器状态
pub struct ReverseIpcServer {
    pipe_name: String,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl ReverseIpcServer {
    pub fn new(pipe_name: &str) -> Self {
        Self {
            pipe_name: pipe_name.to_string(),
            shutdown_tx: None,
        }
    }
    
    /// 启动服务器
    pub fn start(&mut self, storage: Arc<StorageState>, ocr_cache: OcrImageCache) -> Result<(), String> {
        let pipe_name = self.pipe_name.clone();
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // 在新线程中运行 tokio runtime
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");

            rt.block_on(async move {
                let full_pipe_name = format!(r"\\.\pipe\{}", pipe_name);

                loop {
                    // 创建管道服务器
                    let server = match ServerOptions::new()
                        .first_pipe_instance(false)
                        .create(&full_pipe_name)
                    {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("Failed to create pipe server: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
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

                            // 处理客户端请求
                            let storage_clone = storage.clone();
                            let ocr_cache_clone = ocr_cache.clone();
                            tokio::spawn(async move {
                                handle_client(server, storage_clone, ocr_cache_clone).await;
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
async fn handle_client(mut server: NamedPipeServer, storage: Arc<StorageState>, ocr_cache: OcrImageCache) {
    // 读取请求 - 使用循环读取直到管道关闭或超时
    // 因为图片 Base64 数据可能很大（数 MB），单次 read 可能无法读取完整数据
    let mut buf = Vec::with_capacity(4 * 1024 * 1024); // 预分配 4MB
    let mut temp_buf = vec![0u8; 64 * 1024]; // 64KB 临时缓冲区
    
    // 设置读取超时
    let read_timeout = tokio::time::Duration::from_secs(30);
    let start_time = tokio::time::Instant::now();
    
    loop {
        if start_time.elapsed() > read_timeout {
            tracing::warn!("Read timeout after {} bytes", buf.len());
            break;
        }
        
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            server.read(&mut temp_buf)
        ).await {
            Ok(Ok(0)) => {
                // 连接已关闭，数据读取完成
                break;
            }
            Ok(Ok(n)) => {
                buf.extend_from_slice(&temp_buf[..n]);
                // 如果读取的数据小于缓冲区，可能已经读完
                if n < temp_buf.len() {
                    // 再等一小会看看是否还有数据
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    match tokio::time::timeout(
                        tokio::time::Duration::from_millis(100),
                        server.read(&mut temp_buf)
                    ).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(more)) => {
                            buf.extend_from_slice(&temp_buf[..more]);
                        }
                        _ => break,
                    }
                }
                // 限制最大数据量为 16MB
                if buf.len() > 16 * 1024 * 1024 {
                    tracing::error!("Request too large: {} bytes", buf.len());
                    let response = StorageResponse::error("Request too large (max 16MB)");
                    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let _ = server.write_all(&response_bytes).await;
                    return;
                }
            }
            Ok(Err(e)) => {
                // 检查是否是 "管道已结束" 错误（正常情况，客户端已发送完数据）
                let is_pipe_ended = e.raw_os_error() == Some(109); // ERROR_BROKEN_PIPE
                if !is_pipe_ended {
                    tracing::error!("Read error: {}", e);
                }
                break;
            }
            Err(_) => {
                // 超时，可能数据已读取完成
                break;
            }
        }
    }
    
    if buf.is_empty() {
        return;
    }
    
    let request_str = String::from_utf8_lossy(&buf);
    
    // 解析请求
    let response = match serde_json::from_str::<serde_json::Value>(&request_str) {
        Ok(req) => process_request(&req, &storage, &ocr_cache),
        Err(e) => StorageResponse::error(&format!("Invalid JSON: {}", e)),
    };
    
    // 发送响应
    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
    if let Err(e) = server.write_all(&response_bytes).await {
        tracing::error!("Write error: {}", e);
    }
}

/// 处理存储请求
fn process_request(req: &serde_json::Value, storage: &StorageState, ocr_cache: &OcrImageCache) -> StorageResponse {
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
        
        "get_public_key" => {
            match storage.get_public_key() {
                Ok(key) => {
                    let encoded = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &key,
                    );
                    StorageResponse::success(serde_json::json!({
                        "public_key": encoded
                    }))
                }
                Err(e) => StorageResponse::error(&e),
            }
        }
        
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
        
        "screenshot_exists" => {
            let image_hash = req.get("image_hash").and_then(|h| h.as_str()).unwrap_or("");
            
            match storage.screenshot_exists(image_hash) {
                Ok(exists) => StorageResponse::success(serde_json::json!({
                    "exists": exists
                })),
                Err(e) => StorageResponse::error(&e),
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
                                tracing::error!("Failed to abort screenshot {}: {}", screenshot_id, e);
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
                                if let Err(abort_err) = storage.abort_screenshot(screenshot_id, Some(&msg)) {
                                    tracing::error!("Failed to abort screenshot {}: {}", screenshot_id, abort_err);
                                }
                                return StorageResponse::error(&msg);
                            }
                        }
                    }

                    Some(results)
                }
                None => None,
            };

            match storage.commit_screenshot(screenshot_id, ocr_results.as_ref()) {
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

            let reason = req.get("reason").and_then(|v| v.as_str()).map(|s| s.to_string());

            match storage.abort_screenshot(screenshot_id, reason.as_deref()) {
                Ok(result) => StorageResponse::success(serde_json::to_value(result).unwrap()),
                Err(e) => StorageResponse::error(&e),
            }
        }
        "get_temp_image" => {
            // Return image data from in-memory OCR cache (avoids CNG decryption / Windows Hello)
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

            // Look up the in-memory cache first (no CNG decryption needed)
            let cached = {
                let cache = ocr_cache.lock().unwrap();
                cache.get(&screenshot_id).cloned()
            };

            match cached {
                Some(jpeg_bytes) => {
                    let b64_data = base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &jpeg_bytes,
                    );
                    StorageResponse::success(serde_json::json!({
                        "image_data": b64_data,
                        "mime_type": "image/jpeg",
                    }))
                }
                None => {
                    // Fallback to storage (may trigger CNG) for non-capture callers
                    match storage.get_screenshot_by_id(screenshot_id) {
                        Ok(Some(record)) => {
                            match storage.read_image(&record.image_path) {
                                Ok((b64_data, mime)) => {
                                    StorageResponse::success(serde_json::json!({
                                        "image_data": b64_data,
                                        "mime_type": mime,
                                    }))
                                }
                                Err(e) => StorageResponse::error(&format!("Failed to read image: {}", e)),
                            }
                        }
                        Ok(None) => StorageResponse::error("Screenshot not found"),
                        Err(e) => StorageResponse::error(&e),
                    }
                }
            }
        }

        _ => StorageResponse::error(&format!("Unknown command: {}", command)),
    };

    if diag_start.elapsed().as_secs() >= 10 {
        tracing::warn!("[DIAG:RIPC] command='{}' completed in {:?}", command, diag_start.elapsed());
    }
    response
}

/// 生成反向 IPC 管道名
pub fn generate_reverse_pipe_name() -> String {
    let mut rng = rand::thread_rng();
    let random_suffix: String = (0..32)
        .map(|_| format!("{:02x}", rand::Rng::gen::<u8>(&mut rng)))
        .collect();
    format!("carbon_storage_{}", random_suffix)
}

// NMH Pipe Server

use sha2::{Sha256, Digest};
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
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::core::PWSTR;

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

        let result = sid_string.to_string()
            .map_err(|e| format!("SID string conversion failed: {}", e))?;

        // Free the allocated string
        windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(sid_string.0 as *mut _));
        let _ = windows::Win32::Foundation::CloseHandle(token_handle);

        Ok(result)
    }
}

/// Generate a random 32-byte auth token and write it to the data dir.
pub fn generate_nmh_auth_token(data_dir: &PathBuf) -> Result<String, String> {
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
        Self {
            shutdown_tx: None,
        }
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
            let rt = tokio::runtime::Runtime::new().expect("Failed to create NMH runtime");

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
    // Read the full request (same pattern as handle_client)
    let mut buf = Vec::with_capacity(4 * 1024 * 1024);
    let mut temp_buf = vec![0u8; 64 * 1024];
    let read_timeout = tokio::time::Duration::from_secs(30);
    let start_time = tokio::time::Instant::now();

    loop {
        if start_time.elapsed() > read_timeout {
            tracing::warn!("NMH read timeout after {} bytes", buf.len());
            break;
        }

        match tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            server.read(&mut temp_buf)
        ).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&temp_buf[..n]);
                if n < temp_buf.len() {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    match tokio::time::timeout(
                        tokio::time::Duration::from_millis(100),
                        server.read(&mut temp_buf)
                    ).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(more)) => buf.extend_from_slice(&temp_buf[..more]),
                        _ => break,
                    }
                }
                if buf.len() > 16 * 1024 * 1024 {
                    let response = StorageResponse::error("Request too large (max 16MB)");
                    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let _ = server.write_all(&response_bytes).await;
                    return;
                }
            }
            Ok(Err(e)) => {
                if e.raw_os_error() != Some(109) {
                    tracing::error!("NMH read error: {}", e);
                }
                break;
            }
            Err(_) => break,
        }
    }

    if buf.is_empty() {
        return;
    }

    let request_str = String::from_utf8_lossy(&buf);

    let response = match serde_json::from_str::<serde_json::Value>(&request_str) {
        Ok(req) => {
            // Validate auth token
            let provided_token = req.get("auth_token").and_then(|t| t.as_str()).unwrap_or("");
            if provided_token != expected_token {
                tracing::warn!("NMH auth token mismatch");
                StorageResponse::error("Authentication failed")
            } else {
                process_nmh_request(&req, storage.clone(), capture_state.clone(), app_handle.clone()).await
            }
        }
        Err(e) => StorageResponse::error(&format!("Invalid JSON: {}", e)),
    };

    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
    if let Err(e) = server.write_all(&response_bytes).await {
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
        "save_extension_screenshot" => {
            // Check if capture is paused
            if capture_state.paused.load(std::sync::atomic::Ordering::SeqCst) {
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
            let page_url = req.get("page_url").and_then(|v| v.as_str()).map(|s| s.to_string());
            let page_title = req.get("page_title").and_then(|v| v.as_str()).map(|s| s.to_string());
            let page_icon = req.get("page_icon").and_then(|v| v.as_str()).map(|s| s.to_string());
            let visible_links: Option<Vec<crate::storage::VisibleLink>> = req.get("visible_links")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let browser_name = req.get("browser_name").and_then(|v| v.as_str())
                .unwrap_or("browser-extension").to_string();

            // Check if extension enhancement is enabled for this browser
            if !is_extension_enhanced_browser(&browser_name) {
                return StorageResponse::error("Extension enhancement not enabled for this browser");
            }

            // OCR queue backpressure check (same logic as capture loop)
            let in_flight = capture_state.in_flight_ocr_count.load(std::sync::atomic::Ordering::SeqCst);
            let capture_on_busy = capture_state.capture_on_ocr_busy.load(std::sync::atomic::Ordering::SeqCst);
            let max_queue = capture_state.ocr_queue_max_size.load(std::sync::atomic::Ordering::SeqCst);

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
                                let mut cache = capture_state.ocr_image_cache.lock().unwrap();
                                cache.insert(screenshot_id, jpeg_bytes);
                            }

                            // Spawn async OCR task
                            let storage_arc = storage.clone();
                            let capture_arc = capture_state.clone();
                            let app_clone = app_handle.clone();
                            let window_title = page_title.unwrap_or_default();
                            let timestamp_ms = chrono::Utc::now().timestamp_millis();

                            // Increment in-flight OCR counter
                            capture_state.in_flight_ocr_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                            tokio::spawn(async move {
                                let monitor_state = app_clone.state::<MonitorState>();
                                let result = process_extension_ocr(
                                    &monitor_state,
                                    &storage_arc,
                                    screenshot_id,
                                    &image_hash,
                                    &window_title,
                                    &browser_name,
                                    timestamp_ms,
                                ).await;

                                // Remove from OCR cache
                                {
                                    let mut cache = capture_arc.ocr_image_cache.lock().unwrap();
                                    cache.remove(&screenshot_id);
                                }

                                // Decrement in-flight OCR counter
                                capture_arc.in_flight_ocr_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

                                if let Err(e) = result {
                                    tracing::error!("Extension OCR failed for screenshot {}: {}", screenshot_id, e);
                                    if let Err(abort_err) = storage_arc.abort_screenshot(screenshot_id, Some(&e)) {
                                        tracing::error!("abort_screenshot also failed: {}", abort_err);
                                    }
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

/// Check if extension enhancement is enabled for a given browser process name.
/// Reads from Windows registry settings.
fn is_extension_enhanced_browser(browser_name: &str) -> bool {
    let lower = browser_name.to_lowercase();
    if lower.contains("chrome") {
        crate::registry_config::get_bool("extension_enhanced_chrome").unwrap_or(false)
    } else if lower.contains("edge") || lower.contains("msedge") {
        crate::registry_config::get_bool("extension_enhanced_edge").unwrap_or(false)
    } else {
        false
    }
}

/// Compute deterministic NMH command pipe name for a given browser type.
/// Must match the formula in nmh.rs `compute_nmh_cmd_pipe_name`.
pub fn compute_nmh_cmd_pipe_name(browser_type: &str) -> Result<String, String> {
    let sid = get_current_user_sid()?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{}carbonpaper_nmh_cmd_salt_{}", sid, browser_type));
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);
    Ok(format!(r"\\.\pipe\carbon_nmh_cmd_{}", &hex_hash[..16]))
}

/// Request the browser extension to capture the current tab.
/// Opens the NMH command pipe and sends a `request_capture` command.
/// Fails silently (returns Err) if the NMH is not running — this is expected.
pub async fn request_extension_capture(process_name: &str) -> Result<(), String> {
    let browser_type = process_name_to_browser_type(process_name);
    let pipe_name = compute_nmh_cmd_pipe_name(&browser_type)?;

    tracing::info!("request_extension_capture: process={} browser_type={} pipe={}", process_name, browser_type, pipe_name);

    // Run blocking pipe I/O on a separate thread
    tokio::task::spawn_blocking(move || {
        use std::fs::OpenOptions;
        use std::io::{Read, Write};

        let mut pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe_name)
            .map_err(|e| format!("Cannot open NMH cmd pipe: {}", e))?;

        let request = serde_json::json!({"command": "request_capture"});
        let data = serde_json::to_vec(&request)
            .map_err(|e| format!("Serialization failed: {}", e))?;

        pipe.write_all(&data)
            .map_err(|e| format!("Pipe write failed: {}", e))?;
        pipe.flush()
            .map_err(|e| format!("Pipe flush failed: {}", e))?;

        // Read response (best-effort, don't care much about content)
        let mut response_buf = vec![0u8; 1024];
        let _ = pipe.read(&mut response_buf);

        Ok(())
    })
    .await
    .map_err(|e| format!("Task join failed: {}", e))?
}

/// Map process name (e.g. "chrome.exe") to browser type string for pipe name computation.
fn process_name_to_browser_type(process_name: &str) -> String {
    let lower = process_name.to_lowercase();
    if lower.contains("msedge") || lower.contains("edge") {
        "edge".to_string()
    } else {
        "chrome".to_string()
    }
}

/// Check if a process name belongs to a browser with extension enhancement enabled.
/// Used by the capture loop to skip OCR for extension-enhanced browsers.
pub fn is_process_extension_enhanced(process_name: &str) -> bool {
    let lower = process_name.to_lowercase();
    if lower == "chrome.exe" || lower == "chrome" {
        crate::registry_config::get_bool("extension_enhanced_chrome").unwrap_or(false)
    } else if lower == "msedge.exe" || lower == "msedge" {
        crate::registry_config::get_bool("extension_enhanced_edge").unwrap_or(false)
    } else {
        false
    }
}

/// Send extension screenshot to Python OCR pipeline and commit results
async fn process_extension_ocr(
    monitor_state: &MonitorState,
    storage: &StorageState,
    screenshot_id: i64,
    image_hash: &str,
    window_title: &str,
    process_name: &str,
    timestamp_ms: i64,
) -> Result<(), String> {
    let pipe_name = {
        let guard = monitor_state.pipe_name.lock().unwrap();
        guard.clone().ok_or_else(|| "Monitor pipe not available".to_string())?
    };
    let auth_token = {
        let guard = monitor_state.auth_token.lock().unwrap();
        guard.clone().ok_or_else(|| "Auth token not available".to_string())?
    };
    let seq_no = monitor_state.request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let req = serde_json::json!({
        "command": "process_ocr",
        "screenshot_id": screenshot_id,
        "image_hash": image_hash,
        "window_title": window_title,
        "process_name": process_name,
        "timestamp": timestamp_ms,
    });

    let response = crate::monitor::send_ipc_request(&pipe_name, &auth_token, seq_no, req).await?;

    if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
        return Err(format!("Python OCR error: {}", error));
    }

    let ocr_results: Option<Vec<OcrResultInput>> = response
        .get("ocr_results")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    storage.commit_screenshot(screenshot_id, ocr_results.as_ref())?;

    tracing::debug!(
        "Extension screenshot {} committed with {} OCR results",
        screenshot_id,
        ocr_results.as_ref().map(|r| r.len()).unwrap_or(0)
    );

    Ok(())
}
