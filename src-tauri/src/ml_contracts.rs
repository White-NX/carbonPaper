//! Stable application-facing boundaries for the staged Python-to-Rust ML migration.
//!
//! OCR is implemented in v0.8.3. The remaining traits deliberately stay small
//! until their Rust runtimes are introduced in later 0.8.x releases.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub runtime: String,
    pub files: Vec<String>,
    pub installed: bool,
    pub loaded: bool,
}

pub trait OcrEngine: Send {
    type Input;
    type Output;

    fn engine_id(&self) -> &'static str;
    fn recognize(&mut self, input: Self::Input) -> Result<Self::Output, String>;
}

pub trait TextEmbedder: Send {
    fn model_id(&self) -> &str;
}

pub trait Reranker: Send {
    fn model_id(&self) -> &str;
}

pub trait VectorIndex: Send + Sync {
    fn index_id(&self) -> &str;
}

pub trait ModelRegistry: Send + Sync {
    fn models(&self) -> Vec<ModelDescriptor>;
}
