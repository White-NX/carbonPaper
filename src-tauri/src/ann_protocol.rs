//! Internal control protocol for the resumable, isolated ANN builder.
// The desktop and the standalone worker intentionally use different subsets.
#![allow(dead_code)]
use serde::{Deserialize, Serialize};
pub const ANN_WORKER_PROTOCOL: u32 = 1;
pub const IO_CHUNK: usize = 256 * 1024;
pub const IO_RATE: u64 = 2 * 1024 * 1024;
pub const CHECKPOINT_ROWS: u64 = 5000;
pub const CHECKPOINT_COMPUTE_MS: f64 = 30_000.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnErrorKind {
    Paused,
    InvalidCheckpoint,
    Failed,
}

#[derive(Debug)]
pub enum AnnError {
    Paused(&'static str),
    InvalidCheckpoint(String),
    Failed(String),
}

impl AnnError {
    pub fn kind(&self) -> AnnErrorKind {
        match self {
            Self::Paused(_) => AnnErrorKind::Paused,
            Self::InvalidCheckpoint(_) => AnnErrorKind::InvalidCheckpoint,
            Self::Failed(_) => AnnErrorKind::Failed,
        }
    }
}

impl std::fmt::Display for AnnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused(reason) => write!(formatter, "background_paused: {reason}"),
            Self::InvalidCheckpoint(message) | Self::Failed(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl From<String> for AnnError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for AnnError {
    fn from(message: &str) -> Self {
        Self::Failed(message.into())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AnnRequest {
    Open {
        request_id: u64,
        protocol: u32,
        flat: String,
        checkpoint: Option<String>,
        checksum: Option<String>,
        checkpoint_rows: u64,
    },
    Step {
        request_id: u64,
        background: bool,
    },
    Cancel {
        request_id: u64,
    },
}
impl AnnRequest {
    pub fn id(&self) -> u64 {
        match self {
            Self::Open { request_id, .. }
            | Self::Step { request_id, .. }
            | Self::Cancel { request_id } => *request_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnCost {
    pub operation: String,
    pub elapsed_ms: f64,
    pub cpu_ms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AnnReply {
    pub request_id: u64,
    pub phase: String,
    pub built: u64,
    pub graph_bytes: u64,
    pub checkpoint: Option<String>,
    pub checksum: Option<String>,
    pub samples: Vec<AnnCost>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<AnnErrorKind>,
}
