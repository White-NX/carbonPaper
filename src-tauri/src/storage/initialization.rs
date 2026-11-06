//! Startup progress never borrows the database or its maintenance locks.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InitializationPhase {
    Credentials,
    Opening,
    Schema,
    Metadata,
    Vectors,
    Processes,
    Cleanup,
    Compacting,
    Finalizing,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InitializationProgress {
    pub phase: InitializationPhase,
    pub processed: u64,
    pub total: u64,
}

impl InitializationProgress {
    pub fn new(phase: InitializationPhase, processed: u64, total: u64) -> Self {
        Self {
            phase,
            processed,
            total,
        }
    }

    pub fn phase(phase: InitializationPhase) -> Self {
        Self::new(phase, 0, 0)
    }
}
