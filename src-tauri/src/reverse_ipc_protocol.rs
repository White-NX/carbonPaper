//! Framing and validation for the browser native-messaging named pipe.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;

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

    let len = crate::nmh_protocol::frame_length(first).map_err(|error| error.to_string())?;
    let mut body = vec![0u8; len];
    tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        server.read_exact(&mut body),
    )
    .await
    .map_err(|_| "overall_timeout".to_string())?
    .map_err(|e| format!("read_body_error:{}", e))?;
    Ok(body)
}

pub async fn write_ipc_frame(server: &mut NamedPipeServer, body: &[u8]) -> Result<(), String> {
    let prefix =
        crate::nmh_protocol::frame_prefix(body.len()).map_err(|error| error.to_string())?;
    server
        .write_all(&prefix)
        .await
        .map_err(|e| format!("write_prefix_error:{}", e))?;
    server
        .write_all(body)
        .await
        .map_err(|e| format!("write_body_error:{}", e))
}

/// Response sent back to the native-messaging host after processing a command.
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
