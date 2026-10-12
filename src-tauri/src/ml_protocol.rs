//! Versioned framed protocol shared by the desktop process and Rust ML worker.
//!
//! Header and image-size limits are enforced before allocation to keep the local pipe
//! boundary predictable even when a peer is malformed or compromised.
//!
//! Errors retain their kind independently of their diagnostic message. Worker
//! responses keep the same kind/message wire fields for protocol compatibility.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const ML_PROTOCOL_VERSION: u32 = 4;
pub const MAX_ML_HEADER_BYTES: usize = 1024 * 1024;
pub const MAX_ML_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SEMANTIC_BATCH: usize = 32;
pub const MAX_RERANK_DOCUMENTS: usize = 64;
pub const MAX_SEMANTIC_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_SEMANTIC_TEXT_ITEM_BYTES: usize = 64 * 1024;
pub const MAX_ML_TIMEOUT_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlErrorKind {
    InvalidRequest,
    LimitExceeded,
    Timeout,
    Cancelled,
    ModelMissing,
    ModelMismatch,
    ProviderUnavailable,
    Inference,
    Protocol,
    Transport,
    WorkerStopped,
    WorkerMissing,
}

impl MlErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::LimitExceeded => "limit_exceeded",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::ModelMissing => "model_missing",
            Self::ModelMismatch => "model_mismatch",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::Inference => "inference",
            Self::Protocol => "protocol",
            Self::Transport => "transport",
            Self::WorkerStopped => "worker_stopped",
            Self::WorkerMissing => "worker_missing",
        }
    }

    pub fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Transport
                | Self::WorkerStopped
                | Self::ProviderUnavailable
                | Self::Inference
        )
    }

    pub fn is_deterministic(self) -> bool {
        matches!(
            self,
            Self::InvalidRequest
                | Self::LimitExceeded
                | Self::ModelMissing
                | Self::ModelMismatch
                | Self::Protocol
                | Self::WorkerMissing
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MlError {
    pub kind: MlErrorKind,
    pub message: String,
}

impl MlError {
    pub fn new(kind: MlErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Decode only the machine-readable wire field, never the message text.
    pub fn from_response(code: &str, message: String) -> Self {
        let kind = match code {
            "invalid_request" => MlErrorKind::InvalidRequest,
            "limit_exceeded" => MlErrorKind::LimitExceeded,
            "timeout" => MlErrorKind::Timeout,
            "cancelled" => MlErrorKind::Cancelled,
            "model_missing" => MlErrorKind::ModelMissing,
            "model_mismatch" => MlErrorKind::ModelMismatch,
            "provider_unavailable" => MlErrorKind::ProviderUnavailable,
            "inference" => MlErrorKind::Inference,
            "protocol" => MlErrorKind::Protocol,
            "transport" => MlErrorKind::Transport,
            "worker_stopped" => MlErrorKind::WorkerStopped,
            "worker_missing" => MlErrorKind::WorkerMissing,
            _ => {
                return Self::new(
                    MlErrorKind::Protocol,
                    format!("Unknown worker error kind {code}: {message}"),
                )
            }
        };
        Self::new(kind, message)
    }
}

impl std::fmt::Display for MlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for MlError {}

impl From<MlError> for String {
    fn from(error: MlError) -> Self {
        error.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlProvider {
    Cpu,
    DirectMl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlSemanticModel {
    ChineseClip,
    MinilmL12,
    BgeSmallZh,
    BgeRerankerV2M3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlImageInput {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub offset: usize,
    pub body_len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum MlRequest {
    /// Out-of-band control. Produces no response frame; the target request
    /// produces its normal terminal frame with kind `cancelled`.
    Cancel {
        request_id: u64,
    },
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
    EmbedText {
        request_id: u64,
        timeout_ms: u64,
        model: MlSemanticModel,
        texts: Vec<String>,
    },
    EmbedImage {
        request_id: u64,
        timeout_ms: u64,
        model: MlSemanticModel,
        images: Vec<MlImageInput>,
        body_len: usize,
    },
    InspectTokenization {
        request_id: u64,
        model: MlSemanticModel,
        texts: Vec<String>,
        text_pairs: Option<Vec<String>>,
    },
    Rerank {
        request_id: u64,
        timeout_ms: u64,
        model: MlSemanticModel,
        query: String,
        documents: Vec<String>,
    },
    SemanticStatus {
        request_id: u64,
    },
    Unload {
        request_id: u64,
    },
    Shutdown {
        request_id: u64,
    },
}

impl MlRequest {
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Cancel { request_id }
            | Self::Ping { request_id }
            | Self::Ocr { request_id, .. }
            | Self::EmbedText { request_id, .. }
            | Self::EmbedImage { request_id, .. }
            | Self::InspectTokenization { request_id, .. }
            | Self::Rerank { request_id, .. }
            | Self::SemanticStatus { request_id }
            | Self::Unload { request_id }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlSemanticTimings {
    pub model_load_ms: f64,
    pub preprocess_ms: f64,
    pub inference_ms: f64,
    pub request_total_ms: f64,
    #[serde(default)]
    pub cpu_ms: f64,
    #[serde(default)]
    pub model_load_cpu_ms: f64,
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
    SemanticReady {
        protocol_version: u32,
        worker_version: String,
        ort_version: String,
        provider: MlProvider,
        supported_models: Vec<MlSemanticModel>,
    },
    Pong {
        request_id: u64,
    },
    OcrComplete {
        request_id: u64,
        blocks: Vec<MlOcrBlock>,
        timings: MlOcrTimings,
    },
    EmbeddingComplete {
        request_id: u64,
        model: MlSemanticModel,
        dimensions: usize,
        vectors: Vec<Vec<f32>>,
        timings: MlSemanticTimings,
    },
    TokenizationComplete {
        request_id: u64,
        model: MlSemanticModel,
        batch: usize,
        sequence: usize,
        input_ids: Vec<i64>,
        attention_mask: Vec<i64>,
        token_type_ids: Vec<i64>,
    },
    RerankComplete {
        request_id: u64,
        model: MlSemanticModel,
        scores: Vec<f32>,
        timings: MlSemanticTimings,
    },
    SemanticStatus {
        request_id: u64,
        provider: MlProvider,
        loaded_model: Option<MlSemanticModel>,
        model_id: Option<String>,
        model_revision: Option<String>,
        model_fingerprint: Option<String>,
    },
    Unloaded {
        request_id: u64,
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

pub fn read_request<R: Read>(reader: &mut R) -> Result<(MlRequest, Vec<u8>), MlError> {
    let header = read_frame(reader, MAX_ML_HEADER_BYTES)?;
    let request: MlRequest = serde_json::from_slice(&header).map_err(|error| {
        MlError::new(
            MlErrorKind::Protocol,
            format!("invalid ML request JSON: {error}"),
        )
    })?;
    let body_len = match &request {
        MlRequest::Ocr {
            timeout_ms,
            width,
            height,
            stride,
            body_len,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_rgb8_body(*width, *height, *stride, *body_len)?;
            *body_len
        }
        MlRequest::EmbedImage {
            timeout_ms,
            images,
            body_len,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_image_batch(images, *body_len)?;
            *body_len
        }
        MlRequest::EmbedText {
            timeout_ms, texts, ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_text_batch(texts)?;
            0
        }
        MlRequest::InspectTokenization {
            texts, text_pairs, ..
        } => {
            validate_tokenization_request(texts, text_pairs.as_deref())?;
            0
        }
        MlRequest::Rerank {
            timeout_ms,
            query,
            documents,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_rerank(query, documents)?;
            0
        }
        _ => 0,
    };
    let mut body = vec![0u8; body_len];
    reader.read_exact(&mut body).map_err(|error| {
        MlError::new(
            MlErrorKind::Transport,
            format!("failed to read ML request body: {error}"),
        )
    })?;
    Ok((request, body))
}

pub fn write_request<W: Write>(
    writer: &mut W,
    request: &MlRequest,
    body: &[u8],
) -> Result<(), MlError> {
    let expected_body_len = match request {
        MlRequest::Ocr {
            timeout_ms,
            width,
            height,
            stride,
            body_len,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_rgb8_body(*width, *height, *stride, *body_len)?;
            *body_len
        }
        MlRequest::EmbedImage {
            timeout_ms,
            images,
            body_len,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_image_batch(images, *body_len)?;
            *body_len
        }
        MlRequest::EmbedText {
            timeout_ms, texts, ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_text_batch(texts)?;
            0
        }
        MlRequest::InspectTokenization {
            texts, text_pairs, ..
        } => {
            validate_tokenization_request(texts, text_pairs.as_deref())?;
            0
        }
        MlRequest::Rerank {
            timeout_ms,
            query,
            documents,
            ..
        } => {
            validate_timeout(*timeout_ms)?;
            validate_rerank(query, documents)?;
            0
        }
        _ => 0,
    };
    if body.len() != expected_body_len {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!(
                "ML request body length mismatch: header={}, actual={}",
                expected_body_len,
                body.len()
            ),
        ));
    }
    let header = serde_json::to_vec(request).map_err(|error| {
        MlError::new(
            MlErrorKind::Protocol,
            format!("failed to encode ML request: {error}"),
        )
    })?;
    write_frame(writer, &header)?;
    writer
        .write_all(body)
        .and_then(|_| writer.flush())
        .map_err(|error| {
            MlError::new(
                MlErrorKind::Transport,
                format!("failed to write ML request body: {error}"),
            )
        })
}

fn validate_timeout(timeout_ms: u64) -> Result<(), MlError> {
    if timeout_ms == 0 || timeout_ms > MAX_ML_TIMEOUT_MS {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!("ML timeout must be within 1..={MAX_ML_TIMEOUT_MS} ms: {timeout_ms}"),
        ));
    }
    Ok(())
}

fn validate_text_batch(texts: &[String]) -> Result<(), MlError> {
    if texts.is_empty() {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            "semantic text batch must contain at least one item",
        ));
    }
    if texts.len() > MAX_SEMANTIC_BATCH {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("semantic text batch must contain 1..={MAX_SEMANTIC_BATCH} items"),
        ));
    }
    validate_text_limits(texts.iter().map(String::as_str))
}

fn validate_rerank(query: &str, documents: &[String]) -> Result<(), MlError> {
    if documents.is_empty() {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            "rerank request must contain at least one document",
        ));
    }
    if documents.len() > MAX_RERANK_DOCUMENTS {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("rerank request must contain 1..={MAX_RERANK_DOCUMENTS} documents"),
        ));
    }
    validate_text_limits(std::iter::once(query).chain(documents.iter().map(String::as_str)))
}

fn validate_tokenization_request(
    texts: &[String],
    text_pairs: Option<&[String]>,
) -> Result<(), MlError> {
    validate_text_batch(texts)?;
    if let Some(pairs) = text_pairs {
        if pairs.len() != texts.len() {
            return Err(MlError::new(
                MlErrorKind::InvalidRequest,
                "tokenization text_pairs length must match texts",
            ));
        }
        validate_text_limits(pairs.iter().map(String::as_str))?;
    }
    Ok(())
}

fn validate_text_limits<'a>(texts: impl Iterator<Item = &'a str>) -> Result<(), MlError> {
    let mut total = 0usize;
    for text in texts {
        let len = text.len();
        if len > MAX_SEMANTIC_TEXT_ITEM_BYTES {
            return Err(MlError::new(
                MlErrorKind::LimitExceeded,
                format!("semantic text item exceeds limit: {len} > {MAX_SEMANTIC_TEXT_ITEM_BYTES}"),
            ));
        }
        total = total.checked_add(len).ok_or_else(|| {
            MlError::new(
                MlErrorKind::InvalidRequest,
                "semantic text byte count overflow",
            )
        })?;
    }
    if total > MAX_SEMANTIC_TEXT_BYTES {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("semantic text request exceeds limit: {total} > {MAX_SEMANTIC_TEXT_BYTES}"),
        ));
    }
    Ok(())
}

fn validate_image_batch(images: &[MlImageInput], body_len: usize) -> Result<(), MlError> {
    if images.is_empty() {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            "semantic image batch must contain at least one item",
        ));
    }
    if images.len() > MAX_SEMANTIC_BATCH {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("semantic image batch must contain 1..={MAX_SEMANTIC_BATCH} items"),
        ));
    }
    if body_len > MAX_ML_IMAGE_BYTES {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("ML image body exceeds limit: {body_len}"),
        ));
    }
    let mut expected_offset = 0usize;
    for image in images {
        if image.offset != expected_offset {
            return Err(MlError::new(
                MlErrorKind::InvalidRequest,
                format!(
                    "ML image batch offsets must be contiguous: expected={}, actual={}",
                    expected_offset, image.offset
                ),
            ));
        }
        validate_rgb8_body(image.width, image.height, image.stride, image.body_len)?;
        expected_offset = expected_offset.checked_add(image.body_len).ok_or_else(|| {
            MlError::new(
                MlErrorKind::InvalidRequest,
                "ML image batch length overflow",
            )
        })?;
    }
    if expected_offset != body_len {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!(
            "ML image batch body length mismatch: expected={expected_offset}, actual={body_len}"
        ),
        ));
    }
    Ok(())
}

fn validate_rgb8_body(
    width: u32,
    height: u32,
    stride: usize,
    body_len: usize,
) -> Result<(), MlError> {
    if width == 0 || height == 0 {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!("invalid ML RGB dimensions: {width}x{height}"),
        ));
    }
    let expected_stride = usize::try_from(width)
        .map_err(|_| {
            MlError::new(
                MlErrorKind::InvalidRequest,
                "ML RGB width does not fit usize",
            )
        })?
        .checked_mul(3)
        .ok_or_else(|| MlError::new(MlErrorKind::InvalidRequest, "ML RGB stride overflow"))?;
    if stride != expected_stride {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!(
                "ML RGB stride must be tightly packed: expected={expected_stride}, actual={stride}"
            ),
        ));
    }
    let expected_len = stride
        .checked_mul(usize::try_from(height).map_err(|_| {
            MlError::new(
                MlErrorKind::InvalidRequest,
                "ML RGB height does not fit usize",
            )
        })?)
        .ok_or_else(|| MlError::new(MlErrorKind::InvalidRequest, "ML RGB body length overflow"))?;
    if body_len != expected_len {
        return Err(MlError::new(
            MlErrorKind::InvalidRequest,
            format!("ML RGB body length mismatch: expected={expected_len}, actual={body_len}"),
        ));
    }
    if body_len > MAX_ML_IMAGE_BYTES {
        return Err(MlError::new(
            MlErrorKind::LimitExceeded,
            format!("ML image body exceeds limit: {body_len}"),
        ));
    }
    Ok(())
}

pub fn read_response<R: Read>(reader: &mut R) -> Result<MlResponse, MlError> {
    let frame = read_frame(reader, MAX_ML_HEADER_BYTES)?;
    serde_json::from_slice(&frame).map_err(|error| {
        MlError::new(
            MlErrorKind::Protocol,
            format!("invalid ML response JSON: {error}"),
        )
    })
}

pub fn write_response<W: Write>(writer: &mut W, response: &MlResponse) -> Result<(), MlError> {
    let frame = serde_json::to_vec(response).map_err(|error| {
        MlError::new(
            MlErrorKind::Protocol,
            format!("failed to encode ML response: {error}"),
        )
    })?;
    write_frame(writer, &frame)
}

fn read_frame<R: Read>(reader: &mut R, limit: usize) -> Result<Vec<u8>, MlError> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).map_err(|error| {
        MlError::new(
            MlErrorKind::Transport,
            format!("failed to read ML frame length: {error}"),
        )
    })?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len == 0 || len > limit {
        return Err(MlError::new(
            MlErrorKind::Protocol,
            format!("invalid ML frame length: {len}"),
        ));
    }
    let mut frame = vec![0u8; len];
    reader.read_exact(&mut frame).map_err(|error| {
        MlError::new(
            MlErrorKind::Transport,
            format!("failed to read ML frame: {error}"),
        )
    })?;
    Ok(frame)
}

fn write_frame<W: Write>(writer: &mut W, frame: &[u8]) -> Result<(), MlError> {
    let len = u32::try_from(frame.len())
        .map_err(|_| MlError::new(MlErrorKind::LimitExceeded, "ML frame is too large"))?;
    writer
        .write_all(&len.to_le_bytes())
        .and_then(|_| writer.write_all(frame))
        .and_then(|_| writer.flush())
        .map_err(|error| {
            MlError::new(
                MlErrorKind::Transport,
                format!("failed to write ML frame: {error}"),
            )
        })
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
        assert!(read_response(&mut zero).unwrap_err().kind == MlErrorKind::Protocol);

        let oversized = (MAX_ML_HEADER_BYTES as u32 + 1).to_le_bytes();
        assert!(
            read_response(&mut oversized.as_slice()).unwrap_err().kind == MlErrorKind::Protocol
        );
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
        assert!(
            read_request(&mut bytes.as_slice()).unwrap_err().kind == MlErrorKind::LimitExceeded
        );
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
        assert!(error.kind == MlErrorKind::LimitExceeded);
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
        assert!(
            read_request(&mut bytes.as_slice()).unwrap_err().kind == MlErrorKind::InvalidRequest
        );
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
        assert!(
            read_request(&mut bytes.as_slice()).unwrap_err().kind == MlErrorKind::InvalidRequest
        );
    }

    #[test]
    fn semantic_text_batch_round_trip_has_no_binary_body() {
        let request = MlRequest::EmbedText {
            request_id: 21,
            timeout_ms: 5_000,
            model: MlSemanticModel::MinilmL12,
            texts: vec!["Rust 向量索引".to_string(), "second".to_string()],
        };
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request, &[]).unwrap();
        let (decoded, body) = read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id(), 21);
        assert!(body.is_empty());
    }

    #[test]
    fn semantic_image_batch_round_trip_preserves_contiguous_rgb_body() {
        let request = MlRequest::EmbedImage {
            request_id: 22,
            timeout_ms: 5_000,
            model: MlSemanticModel::ChineseClip,
            images: vec![
                MlImageInput {
                    width: 1,
                    height: 1,
                    stride: 3,
                    offset: 0,
                    body_len: 3,
                },
                MlImageInput {
                    width: 2,
                    height: 1,
                    stride: 6,
                    offset: 3,
                    body_len: 6,
                },
            ],
            body_len: 9,
        };
        let body = vec![1u8; 9];
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request, &body).unwrap();
        let (decoded, decoded_body) = read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id(), 22);
        assert_eq!(decoded_body, body);
    }

    #[test]
    fn rejects_non_contiguous_image_offsets_and_oversized_text_batches() {
        let bad_image = MlRequest::EmbedImage {
            request_id: 23,
            timeout_ms: 5_000,
            model: MlSemanticModel::ChineseClip,
            images: vec![MlImageInput {
                width: 1,
                height: 1,
                stride: 3,
                offset: 1,
                body_len: 3,
            }],
            body_len: 3,
        };
        assert!(
            write_request(&mut Vec::new(), &bad_image, &[0; 3])
                .unwrap_err()
                .kind
                == MlErrorKind::InvalidRequest
        );

        let bad_text = MlRequest::EmbedText {
            request_id: 24,
            timeout_ms: 5_000,
            model: MlSemanticModel::MinilmL12,
            texts: vec!["x".to_string(); MAX_SEMANTIC_BATCH + 1],
        };
        assert!(
            write_request(&mut Vec::new(), &bad_text, &[])
                .unwrap_err()
                .kind
                == MlErrorKind::LimitExceeded
        );
    }

    #[test]
    fn rejects_unbounded_semantic_timeout() {
        let request = MlRequest::Rerank {
            request_id: 25,
            timeout_ms: MAX_ML_TIMEOUT_MS + 1,
            model: MlSemanticModel::BgeRerankerV2M3,
            query: "query".to_string(),
            documents: vec!["document".to_string()],
        };
        assert!(
            write_request(&mut Vec::new(), &request, &[])
                .unwrap_err()
                .kind
                == MlErrorKind::InvalidRequest
        );
    }
}
