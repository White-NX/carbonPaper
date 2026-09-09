use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const PROTOCOL_VERSION: u32 = 1;
pub const INPUT_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const RESPONSE_ACK: u8 = 1;
pub const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;
pub const PREPARE_TTL_SECS: i64 = 600;
pub const LEASE_TTL_SECS: i64 = 300;
pub const MAX_ACTIVE_TASKS: u64 = 100_000;
pub const DAY_SECS: i64 = 86_400;
pub const SERVICE_NAME: &str = "CarbonPaperKeyService";
pub const PIPE_NAME: &str = r"\\.\pipe\CarbonPaper.AppBound.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum BrokerError {
    #[error("app_bound_unavailable")]
    Unavailable,
    #[error("app_bound_access_denied")]
    AccessDenied,
    #[error("app_bound_invalid_request")]
    InvalidRequest,
    #[error("app_bound_disabled")]
    Disabled,
    #[error("app_bound_dataset_mismatch")]
    DatasetMismatch,
    #[error("app_bound_task_retired")]
    Retired,
    #[error("app_bound_lease_expired")]
    LeaseExpired,
    #[error("app_bound_task_busy")]
    Busy,
    #[error("app_bound_integrity_failure")]
    Integrity,
    #[error("app_bound_storage_failure")]
    Storage,
    #[error("app_bound_protection_failure")]
    Protection,
    #[error("app_bound_version_mismatch")]
    VersionMismatch,
    #[error("app_bound_limit_exceeded")]
    LimitExceeded,
}

pub type Result<T> = std::result::Result<T, BrokerError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub retention_days: u32,
    pub capacity_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            retention_days: 30,
            capacity_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub fn validate(self) -> Result<Self> {
        if !(1..=30).contains(&self.retention_days)
            || !(256 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&self.capacity_bytes)
        {
            return Err(BrokerError::InvalidRequest);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Consumer {
    Classification,
    MiniLm,
    Clip,
}

impl Consumer {
    pub const ALL: [Self; 3] = [Self::Classification, Self::MiniLm, Self::Clip];
    pub const fn bit(self) -> u8 {
        match self {
            Self::Classification => 1,
            Self::MiniLm => 2,
            Self::Clip => 4,
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Classification => "classification",
            Self::MiniLm => "minilm",
            Self::Clip => "clip",
        }
    }
}

/// Redacted in diagnostics and erased on drop, including decoded IPC replies.
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct TaskKey(pub Vec<u8>);

impl std::fmt::Debug for TaskKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskKey([redacted])")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskBinding {
    pub task_id: String,
    pub dataset_id: String,
    pub screenshot_id: i64,
    pub input_version: u32,
    pub consumers: u8,
    pub payload_bytes: u64,
    pub expires_at: i64,
}

impl TaskBinding {
    pub fn aad(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|_| BrokerError::InvalidRequest)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PreparedTask {
    pub task: TaskBinding,
    pub key: TaskKey,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskLease {
    pub task: TaskBinding,
    pub consumer: Consumer,
    pub lease_id: String,
    pub deadline: i64,
    pub key: TaskKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerStatus {
    pub enabled: bool,
    pub limits: Limits,
    pub dataset_id: Option<String>,
    pub active_tasks: u64,
    pub active_bytes: u64,
    pub runtime_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskState {
    pub task: TaskBinding,
    pub active: bool,
    pub retired: bool,
    pub finished_consumers: u8,
    pub abandoned_consumers: u8,
    pub expires_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status {},
    AttachDataset {
        dataset_id: String,
    },
    SetPolicy {
        enabled: bool,
        limits: Limits,
    },
    RetireDataset {
        dataset_id: String,
    },
    PrepareTask {
        dataset_id: String,
        screenshot_id: i64,
        consumers: u8,
        payload_bytes: u64,
    },
    ActivateTask {
        task_id: String,
        ciphertext_digest: String,
    },
    InspectTask {
        task_id: String,
    },
    AcquireTask {
        task_id: String,
        consumer: Consumer,
        ciphertext_digest: String,
    },
    RenewLease {
        task_id: String,
        consumer: Consumer,
        lease_id: String,
    },
    ReleaseLease {
        task_id: String,
        consumer: Consumer,
        lease_id: String,
    },
    FinishConsumer {
        task_id: String,
        consumer: Consumer,
        lease_id: String,
    },
    AbandonConsumer {
        task_id: String,
        consumer: Consumer,
    },
    RevokeTask {
        task_id: String,
    },
    RevokeTasks {
        task_ids: Vec<String>,
    },
    RevokeScreenshots {
        dataset_id: String,
        screenshot_ids: Vec<i64>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Status(BrokerStatus),
    TaskState(TaskState),
    Prepared(PreparedTask),
    Lease(TaskLease),
    Deadline(i64),
    Error(BrokerError),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub version: u32,
    pub nonce: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFrame {
    pub version: u32,
    pub nonce: String,
    pub sequence: u64,
    pub request: Request,
}

impl RequestFrame {
    pub fn validate(&self, nonce: &str) -> Result<()> {
        if self.version != PROTOCOL_VERSION {
            return Err(BrokerError::VersionMismatch);
        }
        // Connections are deliberately single-request; nonces are freshly minted
        // by the service and never restored or accepted from a previous pipe.
        if self.sequence != 1 || !valid_id(&self.nonce) || self.nonce != nonce {
            return Err(BrokerError::AccessDenied);
        }
        Ok(())
    }
}

pub fn valid_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

pub fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pipe_frames_reject_old_nonces_sequences_versions_and_unknown_fields() {
        let nonce = random_id();
        let mut frame = RequestFrame {
            version: PROTOCOL_VERSION,
            nonce: nonce.clone(),
            sequence: 1,
            request: Request::Status {},
        };
        frame.validate(&nonce).unwrap();
        assert_eq!(
            frame.validate(&random_id()).unwrap_err(),
            BrokerError::AccessDenied
        );
        frame.sequence = 2;
        assert_eq!(
            frame.validate(&nonce).unwrap_err(),
            BrokerError::AccessDenied
        );
        frame.sequence = 1;
        frame.version += 1;
        assert_eq!(
            frame.validate(&nonce).unwrap_err(),
            BrokerError::VersionMismatch
        );
        assert!(serde_json::from_str::<Request>(r#"{"command":"status","pid":123}"#).is_err());
        assert!(
            serde_json::from_str::<Request>(r#"{"command":"unwrap","blob":"old-ciphertext"}"#)
                .is_err()
        );
    }
}
