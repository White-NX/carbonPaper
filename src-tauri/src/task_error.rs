//! Internal scheduling failures. Messages are for diagnostics; variants decide
//! whether work yields, waits for authorization, retries, or opens its circuit.
use crate::ml_protocol::{MlError, MlErrorKind};

pub type TaskResult<T> = Result<T, TaskError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    #[error("background_paused: {0}")]
    Paused(&'static str),
    #[error("AUTH_REQUIRED")]
    AuthenticationRequired,
    #[error(transparent)]
    Worker(#[from] MlError),
    #[error("{0}")]
    InvalidCheckpoint(String),
    #[error("{0}")]
    Failed(String),
}

impl TaskError {
    pub fn worker(kind: MlErrorKind, message: impl Into<String>) -> Self {
        Self::Worker(MlError::new(kind, message))
    }

    pub fn is_pause(&self) -> bool {
        matches!(self, Self::Paused(_)) || self.worker_kind() == Some(MlErrorKind::Cancelled)
    }

    pub fn worker_kind(&self) -> Option<MlErrorKind> {
        match self {
            Self::Worker(error) => Some(error.kind),
            _ => None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Paused(_) => "background_paused",
            Self::AuthenticationRequired => "auth_required",
            Self::Worker(error) => error.kind.as_str(),
            Self::InvalidCheckpoint(_) => "invalid_checkpoint",
            Self::Failed(_) => "task_failed",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Paused(reason) => reason,
            Self::AuthenticationRequired => "Authentication required",
            Self::Worker(error) => &error.message,
            Self::InvalidCheckpoint(message) | Self::Failed(message) => message,
        }
    }

    pub fn is_transient_worker_failure(&self) -> bool {
        self.worker_kind().is_some_and(MlErrorKind::is_transient)
    }

    pub fn is_deterministic_worker_failure(&self) -> bool {
        self.worker_kind()
            .is_some_and(MlErrorKind::is_deterministic)
    }
}

impl From<String> for TaskError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for TaskError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

impl From<TaskError> for String {
    fn from(error: TaskError) -> Self {
        error.to_string()
    }
}

impl From<carbonpaper_app_bound::protocol::BrokerError> for TaskError {
    fn from(error: carbonpaper_app_bound::protocol::BrokerError) -> Self {
        use carbonpaper_app_bound::protocol::BrokerError;
        match error {
            BrokerError::Disabled
            | BrokerError::LeaseExpired
            | BrokerError::Retired
            | BrokerError::DatasetMismatch => Self::Paused(error.code()),
            _ => Self::Failed(error.to_string()),
        }
    }
}

impl From<crate::storage::BackgroundReadError> for TaskError {
    fn from(error: crate::storage::BackgroundReadError) -> Self {
        match error {
            crate::storage::BackgroundReadError::AuthRequired => Self::AuthenticationRequired,
            error => Self::Failed(error.to_string()),
        }
    }
}
