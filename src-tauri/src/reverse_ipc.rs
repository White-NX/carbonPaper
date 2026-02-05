//! 反向 IPC 服务器 - 接收来自 Python 子服务的存储请求
//!
//! 该模块创建一个命名管道服务器，Python 子服务可以连接到该管道发送存储请求

use crate::storage::{SaveScreenshotRequest, StorageState};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResultInput {
    pub text: String,
    pub confidence: f64,
    #[serde(rename = "box")]
    pub box_coords: Vec<Vec<f64>>,
}

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
    pub fn start(&mut self, storage: Arc<StorageState>) -> Result<(), String> {
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
                            eprintln!("Failed to create pipe server: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    };
                    
                    // 等待客户端连接或关闭信号
                    tokio::select! {
                        _ = shutdown_rx.recv() => {
                            println!("Reverse IPC server shutting down");
                            break;
                        }
                        result = server.connect() => {
                            if let Err(e) = result {
                                eprintln!("Client connection failed: {}", e);
                                continue;
                            }
                            
                            // 处理客户端请求
                            let storage_clone = storage.clone();
                            tokio::spawn(async move {
                                handle_client(server, storage_clone).await;
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
async fn handle_client(mut server: NamedPipeServer, storage: Arc<StorageState>) {
    // 读取请求 - 使用循环读取直到管道关闭或超时
    // 因为图片 Base64 数据可能很大（数 MB），单次 read 可能无法读取完整数据
    let mut buf = Vec::with_capacity(4 * 1024 * 1024); // 预分配 4MB
    let mut temp_buf = vec![0u8; 64 * 1024]; // 64KB 临时缓冲区
    
    // 设置读取超时
    let read_timeout = tokio::time::Duration::from_secs(30);
    let start_time = tokio::time::Instant::now();
    
    loop {
        if start_time.elapsed() > read_timeout {
            eprintln!("Read timeout after {} bytes", buf.len());
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
                    eprintln!("Request too large: {} bytes", buf.len());
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
                    eprintln!("Read error: {}", e);
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
        Ok(req) => process_request(&req, &storage),
        Err(e) => StorageResponse::error(&format!("Invalid JSON: {}", e)),
    };
    
    // 发送响应
    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
    if let Err(e) = server.write_all(&response_bytes).await {
        eprintln!("Write error: {}", e);
    }
}

/// 处理存储请求
fn process_request(req: &serde_json::Value, storage: &StorageState) -> StorageResponse {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    
    match command {
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
        
        "screenshot_exists" => {
            let image_hash = req.get("image_hash").and_then(|h| h.as_str()).unwrap_or("");
            
            match storage.screenshot_exists(image_hash) {
                Ok(exists) => StorageResponse::success(serde_json::json!({
                    "exists": exists
                })),
                Err(e) => StorageResponse::error(&e),
            }
        }
        
        _ => StorageResponse::error(&format!("Unknown command: {}", command)),
    }
}

/// 生成反向 IPC 管道名
pub fn generate_reverse_pipe_name() -> String {
    let mut rng = rand::thread_rng();
    let random_suffix: String = (0..32)
        .map(|_| format!("{:02x}", rand::Rng::gen::<u8>(&mut rng)))
        .collect();
    format!("carbon_storage_{}", random_suffix)
}
