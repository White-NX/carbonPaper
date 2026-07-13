use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const ML_PROTOCOL_VERSION: u32 = 1;
pub const MAX_ML_HEADER_BYTES: usize = 1024 * 1024;
pub const MAX_ML_IMAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlProvider {
    Cpu,
    DirectMl,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum MlRequest {
    Ping {
        request_id: u64,
    },
    Ocr {
        request_id: u64,
        timeout_ms: u64,
        body_len: usize,
    },
    Shutdown {
        request_id: u64,
    },
}

impl MlRequest {
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Ping { request_id }
            | Self::Ocr { request_id, .. }
            | Self::Shutdown { request_id } => *request_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlOcrBlock {
    pub text: String,
    pub confidence: f32,
    pub points: [[f32; 2]; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlOcrTimings {
    pub image_decode_ms: f64,
    pub model_total_ms: f64,
    pub request_total_ms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MlResponse {
    Ready {
        protocol_version: u32,
        worker_version: String,
        rapidocr_core_version: String,
        provider: MlProvider,
        model_id: String,
    },
    Pong {
        request_id: u64,
    },
    OcrComplete {
        request_id: u64,
        blocks: Vec<MlOcrBlock>,
        timings: MlOcrTimings,
    },
    Error {
        request_id: u64,
        kind: String,
        message: String,
    },
    ShuttingDown {
        request_id: u64,
    },
}

pub fn read_request<R: Read>(reader: &mut R) -> Result<(MlRequest, Vec<u8>), String> {
    let header = read_frame(reader, MAX_ML_HEADER_BYTES)?;
    let request: MlRequest = serde_json::from_slice(&header)
        .map_err(|error| format!("invalid ML request JSON: {error}"))?;
    let body_len = match &request {
        MlRequest::Ocr { body_len, .. } => *body_len,
        _ => 0,
    };
    if body_len > MAX_ML_IMAGE_BYTES {
        return Err(format!("ML image body exceeds limit: {body_len}"));
    }
    let mut body = vec![0u8; body_len];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("failed to read ML request body: {error}"))?;
    Ok((request, body))
}

pub fn write_request<W: Write>(
    writer: &mut W,
    request: &MlRequest,
    body: &[u8],
) -> Result<(), String> {
    if body.len() > MAX_ML_IMAGE_BYTES {
        return Err(format!("ML image body exceeds limit: {}", body.len()));
    }
    let header = serde_json::to_vec(request)
        .map_err(|error| format!("failed to encode ML request: {error}"))?;
    write_frame(writer, &header)?;
    writer
        .write_all(body)
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write ML request body: {error}"))
}

pub fn read_response<R: Read>(reader: &mut R) -> Result<MlResponse, String> {
    let frame = read_frame(reader, MAX_ML_HEADER_BYTES)?;
    serde_json::from_slice(&frame).map_err(|error| format!("invalid ML response JSON: {error}"))
}

pub fn write_response<W: Write>(writer: &mut W, response: &MlResponse) -> Result<(), String> {
    let frame = serde_json::to_vec(response)
        .map_err(|error| format!("failed to encode ML response: {error}"))?;
    write_frame(writer, &frame)
}

fn read_frame<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>, String> {
    let mut len_bytes = [0u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .map_err(|error| format!("failed to read ML frame length: {error}"))?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len == 0 || len > limit {
        return Err(format!("invalid ML frame length: {len}"));
    }
    let mut frame = vec![0u8; len];
    reader
        .read_exact(&mut frame)
        .map_err(|error| format!("failed to read ML frame: {error}"))?;
    Ok(frame)
}

fn write_frame<W: Write>(writer: &mut W, frame: &[u8]) -> Result<(), String> {
    let len = u32::try_from(frame.len()).map_err(|_| "ML frame is too large".to_string())?;
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|_| writer.write_all(frame))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write ML frame: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame_round_trip_preserves_binary_body() {
        let request = MlRequest::Ocr {
            request_id: 7,
            timeout_ms: 120_000,
            body_len: 4,
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request, &[1, 2, 3, 4]).unwrap();
        let (decoded, body) = read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id(), 7);
        assert_eq!(body, [1, 2, 3, 4]);
    }

    #[test]
    fn rejects_zero_and_oversized_frames_before_allocating_payload() {
        let zero_bytes = 0u32.to_le_bytes();
        let mut zero = zero_bytes.as_slice();
        assert!(read_response(&mut zero)
            .unwrap_err()
            .contains("frame length: 0"));

        let oversized = (MAX_ML_HEADER_BYTES as u32 + 1).to_le_bytes();
        assert!(read_response(&mut oversized.as_slice())
            .unwrap_err()
            .contains("invalid ML frame length"));
    }

    #[test]
    fn rejects_image_body_above_protocol_limit() {
        let request = MlRequest::Ocr {
            request_id: 9,
            timeout_ms: 1,
            body_len: MAX_ML_IMAGE_BYTES + 1,
        };
        let header = serde_json::to_vec(&request).unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &header).unwrap();
        assert!(read_request(&mut bytes.as_slice())
            .unwrap_err()
            .contains("image body exceeds limit"));
    }
}
