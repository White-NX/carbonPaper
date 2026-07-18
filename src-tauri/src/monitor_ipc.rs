//! Authenticated framed IPC helpers for requests sent to the Python monitor.

use rand::Rng;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const IPC_PROTOCOL_VERSION: u32 = 2;
const IPC_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn generate_random_pipe_name() -> String {
    let mut rng = rand::thread_rng();
    let random_suffix: String = (0..32)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect();
    format!("carbon_monitor_{}", random_suffix)
}

pub(crate) fn generate_auth_token() -> String {
    let mut rng = rand::thread_rng();
    (0..64)
        .map(|_| format!("{:02x}", rng.gen::<u8>()))
        .collect()
}

pub(crate) fn inject_ipc_auth(mut req: Value, auth_token: &str, seq_no: u64) -> Value {
    if let Some(obj) = req.as_object_mut() {
        obj.insert(
            "_auth_token".to_string(),
            Value::String(auth_token.to_string()),
        );
        obj.insert("_seq_no".to_string(), Value::Number(seq_no.into()));
        obj.insert(
            "ipc_protocol_version".to_string(),
            Value::Number(IPC_PROTOCOL_VERSION.into()),
        );
    }
    req
}

pub(crate) fn parse_ipc_response(bytes: &[u8]) -> Result<Value, String> {
    let resp_str = String::from_utf8_lossy(bytes);
    serde_json::from_str::<Value>(&resp_str)
        .map_err(|e| format!("Invalid JSON response: {}. Data: {}", e, resp_str))
}

async fn write_ipc_frame<W>(writer: &mut W, body: &[u8]) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    if body.len() > IPC_MAX_MESSAGE_BYTES {
        return Err(format!(
            "IPC request too large: {} bytes (max {})",
            body.len(),
            IPC_MAX_MESSAGE_BYTES
        ));
    }
    let len = body.len() as u32;
    writer
        .write_all(&len.to_le_bytes())
        .await
        .map_err(|e| format!("Write frame length error: {}", e))?;
    writer
        .write_all(body)
        .await
        .map_err(|e| format!("Write frame body error: {}", e))
}

async fn read_ipc_frame<R>(reader: &mut R) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut first = [0u8; 4];
    reader
        .read_exact(&mut first)
        .await
        .map_err(|e| format!("Read frame length error: {}", e))?;

    let len = u32::from_le_bytes(first) as usize;
    if len > 0 && len <= IPC_MAX_MESSAGE_BYTES {
        let mut body = vec![0u8; len];
        reader
            .read_exact(&mut body)
            .await
            .map_err(|e| format!("Read frame body error: {}", e))?;
        return Ok(body);
    }
    Err(format!(
        "Invalid IPC v{} frame length: {} (max {})",
        IPC_PROTOCOL_VERSION, len, IPC_MAX_MESSAGE_BYTES
    ))
}

pub(crate) async fn send_ipc_request_on_client<C>(
    client: &mut C,
    req: &Value,
    ipc_timeout_secs: u64,
) -> Result<Value, String>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    let req_bytes = serde_json::to_vec(req).map_err(|e| format!("Serialize error: {}", e))?;
    if let Err(e) = write_ipc_frame(client, &req_bytes).await {
        return Err(e);
    }

    match tokio::time::timeout(
        std::time::Duration::from_secs(ipc_timeout_secs),
        read_ipc_frame(client),
    )
    .await
    {
        Ok(Ok(buf)) => parse_ipc_response(&buf),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let e = format!("IPC response timed out after {}s", ipc_timeout_secs);
            Err(e)
        }
    }
}
