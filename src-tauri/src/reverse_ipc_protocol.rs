use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;

pub const IPC_PROTOCOL_VERSION: u32 = 2;
pub const IPC_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const IPC_MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;

pub async fn read_ipc_frame(server: &mut NamedPipeServer) -> Result<Vec<u8>, String> {
    let mut first = [0u8; 4];
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        server.read_exact(&mut first),
    )
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(e)) if e.raw_os_error() == Some(109) => return Ok(Vec::new()),
        Ok(Err(e)) => return Err(format!("read_prefix_error:{}", e)),
        Err(_) => return Err("overall_timeout".to_string()),
    }

    let len = u32::from_le_bytes(first) as usize;
    if len > 0 && len <= IPC_MAX_MESSAGE_BYTES {
        let mut body = vec![0u8; len];
        tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            server.read_exact(&mut body),
        )
        .await
        .map_err(|_| "overall_timeout".to_string())?
        .map_err(|e| format!("read_body_error:{}", e))?;
        return Ok(body);
    }

    Err(format!(
        "Invalid IPC v{} frame length: {} (max {})",
        IPC_PROTOCOL_VERSION, len, IPC_MAX_MESSAGE_BYTES
    ))
}

pub async fn write_ipc_frame(server: &mut NamedPipeServer, body: &[u8]) -> Result<(), String> {
    if body.len() > IPC_MAX_MESSAGE_BYTES {
        return Err(format!(
            "Response too large: {} bytes (max {})",
            body.len(),
            IPC_MAX_MESSAGE_BYTES
        ));
    }
    let len = body.len() as u32;
    server
        .write_all(&len.to_le_bytes())
        .await
        .map_err(|e| format!("write_prefix_error:{}", e))?;
    server
        .write_all(body)
        .await
        .map_err(|e| format!("write_body_error:{}", e))
}

pub async fn write_ipc_binary_frame(
    server: &mut NamedPipeServer,
    body: &[u8],
) -> Result<(), String> {
    if body.len() > IPC_MAX_BINARY_BYTES {
        return Err(format!(
            "Binary response too large: {} bytes (max {})",
            body.len(),
            IPC_MAX_BINARY_BYTES
        ));
    }
    let len = body.len() as u32;
    server
        .write_all(&len.to_le_bytes())
        .await
        .map_err(|e| format!("write_binary_prefix_error:{}", e))?;
    server
        .write_all(body)
        .await
        .map_err(|e| format!("write_binary_body_error:{}", e))
}

/// Response sent back to Python after processing a storage command.
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
