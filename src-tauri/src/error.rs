//! Unified error types for CarbonPaper Rust backend.

use thiserror::Error;

/// Top-level application error type.
/// Converts to `String` for Tauri's `Result<T, String>` command convention.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("{0}")]
    Credential(#[from] crate::credential_manager::CredentialError),

    #[error("Monitor error: {0}")]
    Monitor(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Task join error: {0}")]
    TaskJoin(String),
}

impl From<AppError> for String {
    fn from(e: AppError) -> String {
        e.to_string()
    }
}

/// Storage-layer error type.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Image not found")]
    ImageNotFound,

    #[error("Encryption error: {0}")]
    Encryption(String),
}
