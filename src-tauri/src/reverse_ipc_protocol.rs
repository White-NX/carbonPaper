//! Framing and validation for the browser native-messaging named pipe.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeServer;

pub async fn read_ipc_frame(server: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>, String> {
    read_frame_inner(server, false).await
}

/// Mio keeps one read-ahead operation pending after an ACK. Disconnecting and
/// reconnecting the same server handle can deliver that old operation's EOF or
/// ERROR_PIPE_NOT_CONNECTED on the first read of the new connection. Consume it once,
/// before any prefix byte has been read, so the next read belongs to this peer.
pub async fn read_server_ipc_frame(server: &mut NamedPipeServer) -> Result<Vec<u8>, String> {
    read_frame_inner(server, true).await
}

async fn read_frame_inner(
    server: &mut (impl AsyncRead + Unpin),
    mut previous_disconnect: bool,
) -> Result<Vec<u8>, String> {
    let mut first = [0u8; 4];
    match tokio::time::timeout(tokio::time::Duration::from_secs(30), async {
        let mut offset = 0;
        while offset < first.len() {
            match server.read(&mut first[offset..]).await {
                Ok(0) if previous_disconnect && offset == 0 => previous_disconnect = false,
                Err(e)
                    if previous_disconnect
                        && offset == 0
                        && matches!(e.raw_os_error(), Some(109 | 233)) =>
                {
                    previous_disconnect = false
                }
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
                Ok(read) => {
                    offset += read;
                    previous_disconnect = false;
                }
                Err(e) => return Err(e),
            }
        }
        Ok::<(), std::io::Error>(())
    })
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

pub async fn write_ipc_frame(
    server: &mut (impl AsyncWrite + Unpin),
    body: &[u8],
) -> Result<(), String> {
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
