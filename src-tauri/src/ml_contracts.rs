//! Stable application-facing boundaries for the staged Python-to-Rust ML migration.
//!
//! These contracts deliberately describe inference only. Derived-vector persistence,
//! ANN ownership, dual-write, and consumer cutover belong to later M2 branches.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticModel {
    ChineseClip,
    MinilmL12,
    BgeSmallZh,
    BgeRerankerV2M3,
}

impl SemanticModel {
    pub const fn id(self) -> &'static str {
        match self {
            Self::ChineseClip => "chinese-clip-vit-base-patch16",
            Self::MinilmL12 => "paraphrase-multilingual-MiniLM-L12-v2",
            Self::BgeSmallZh => "bge-small-zh-v1.5",
            Self::BgeRerankerV2M3 => "bge-reranker-v2-m3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlErrorKind {
    InvalidRequest,
    LimitExceeded,
    ModelMissing,
    ModelMismatch,
    ProviderUnavailable,
    Timeout,
    Cancelled,
    Inference,
    Protocol,
    WorkerStopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl std::fmt::Display for MlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for MlError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub model: SemanticModel,
    pub id: String,
    pub revision: String,
    pub runtime: String,
    pub files: Vec<String>,
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
    pub max_length: Option<usize>,
    pub dimensions: Option<usize>,
    pub pooling: Option<String>,
    pub normalization: Option<String>,
    pub installed: bool,
    pub loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageEmbeddingInput {
    pub width: u32,
    pub height: u32,
    pub rgb8: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatch {
    pub model: SemanticModel,
    pub dimensions: usize,
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankBatch {
    pub model: SemanticModel,
    pub scores: Vec<f32>,
}

pub trait OcrEngine: Send {
    type Input;
    type Output;

    fn engine_id(&self) -> &'static str;
    fn recognize(&mut self, input: Self::Input) -> Result<Self::Output, String>;
}

pub trait TextEmbedder: Send {
    fn descriptor(&self) -> &ModelDescriptor;
    fn embed_text(&mut self, texts: &[String]) -> Result<EmbeddingBatch, MlError>;
}

pub trait ImageEmbedder: Send {
    fn descriptor(&self) -> &ModelDescriptor;
    fn embed_image(&mut self, images: &[ImageEmbeddingInput]) -> Result<EmbeddingBatch, MlError>;
}

pub trait Reranker: Send {
    fn descriptor(&self) -> &ModelDescriptor;
    fn rerank(&mut self, query: &str, documents: &[String]) -> Result<RerankBatch, MlError>;
}

pub trait VectorIndex: Send + Sync {
    fn index_id(&self) -> &str;
}

pub trait ModelRegistry: Send + Sync {
    fn models(&self) -> Vec<ModelDescriptor>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_model_ids_are_stable() {
        assert_eq!(
            SemanticModel::MinilmL12.id(),
            "paraphrase-multilingual-MiniLM-L12-v2"
        );
        assert_eq!(SemanticModel::BgeRerankerV2M3.id(), "bge-reranker-v2-m3");
    }
}
