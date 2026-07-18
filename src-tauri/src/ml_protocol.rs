//! Versioned framed protocol shared by the desktop process and Rust ML worker.
//!
//! Header and image-size limits are enforced before allocation to keep the local pipe
//! boundary predictable even when a peer is malformed or compromised.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const ML_PROTOCOL_VERSION: u32 = 2;
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
        width: u32,
        height: u32,
        stride: usize,
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
    pub image_prepare_ms: f64,
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
        MlRequest::Ocr {
            width,
            height,
            stride,
            body_len,
            ..
        } => {
            validate_rgb8_body(*width, *height, *stride, *body_len)?;
            *body_len
        }
        _ => 0,
    };
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
    match request {
        MlRequest::Ocr {
            width,
            height,
            stride,
            body_len,
            ..
        } => {
            validate_rgb8_body(*width, *height, *stride, *body_len)?;
            if body.len() != *body_len {
                return Err(format!(
                    "ML request body length mismatch: header={}, actual={}",
                    body_len,
                    body.len()
                ));
            }
        }
        _ if !body.is_empty() => {
            return Err("non-OCR ML request must not include a body".to_string());
        }
        _ => {}
    }
    let header = serde_json::to_vec(request)
        .map_err(|error| format!("failed to encode ML request: {error}"))?;
    write_frame(writer, &header)?;
    writer
        .write_all(body)
        .and_then(|_| writer.flush())
        .map_err(|error| format!("failed to write ML request body: {error}"))
}

fn validate_rgb8_body(
    width: u32,
    height: u32,
    stride: usize,
    body_len: usize,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("invalid ML RGB dimensions: {width}x{height}"));
    }
    let expected_stride = usize::try_from(width)
        .map_err(|_| "ML RGB width does not fit usize".to_string())?
        .checked_mul(3)
        .ok_or_else(|| "ML RGB stride overflow".to_string())?;
    if stride != expected_stride {
        return Err(format!(
            "ML RGB stride must be tightly packed: expected={expected_stride}, actual={stride}"
        ));
    }
    let expected_len = stride
        .checked_mul(
            usize::try_from(height).map_err(|_| "ML RGB height does not fit usize".to_string())?,
        )
        .ok_or_else(|| "ML RGB body length overflow".to_string())?;
    if body_len != expected_len {
        return Err(format!(
            "ML RGB body length mismatch: expected={expected_len}, actual={body_len}"
        ));
    }
    if body_len > MAX_ML_IMAGE_BYTES {
        return Err(format!("ML image body exceeds limit: {body_len}"));
    }
    Ok(())
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
            width: 2,
            height: 1,
            stride: 6,
            body_len: 6,
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request, &[1, 2, 3, 4, 5, 6]).unwrap();
        let (decoded, body) = read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id(), 7);
        assert_eq!(body, [1, 2, 3, 4, 5, 6]);
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
            width: 4096,
            height: 4096,
            stride: 4096 * 3,
            body_len: 4096 * 4096 * 3,
        };
        let header = serde_json::to_vec(&request).unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &header).unwrap();
        assert!(read_request(&mut bytes.as_slice())
            .unwrap_err()
            .contains("image body exceeds limit"));
    }

    #[test]
    fn rejects_5k_rgb_body_before_writing_payload() {
        let request = MlRequest::Ocr {
            request_id: 12,
            timeout_ms: 1,
            width: 5120,
            height: 2880,
            stride: 5120 * 3,
            body_len: 5120 * 2880 * 3,
        };
        let mut bytes = Vec::new();
        let error = write_request(&mut bytes, &request, &[]).unwrap_err();
        assert!(error.contains("image body exceeds limit"));
        assert!(bytes.is_empty());
    }

    #[test]
    fn rejects_non_tightly_packed_rgb_before_reading_body() {
        let request = MlRequest::Ocr {
            request_id: 10,
            timeout_ms: 1,
            width: 2,
            height: 1,
            stride: 8,
            body_len: 8,
        };
        let header = serde_json::to_vec(&request).unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &header).unwrap();
        assert!(read_request(&mut bytes.as_slice())
            .unwrap_err()
            .contains("stride must be tightly packed"));
    }

    #[test]
    fn rejects_rgb_body_length_mismatch() {
        let request = MlRequest::Ocr {
            request_id: 11,
            timeout_ms: 1,
            width: 2,
            height: 2,
            stride: 6,
            body_len: 6,
        };
        let header = serde_json::to_vec(&request).unwrap();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &header).unwrap();
        assert!(read_request(&mut bytes.as_slice())
            .unwrap_err()
            .contains("body length mismatch"));
    }
}
