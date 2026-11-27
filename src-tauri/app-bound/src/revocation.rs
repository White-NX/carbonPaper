//! An append-only inbox records revocations while the pipe service is offline.
//! Its owner is inferred from the protected file, never from a submitted SID.
use crate::protocol::{valid_id, BrokerError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Revocation {
    RetireOwner,
    Screenshots {
        dataset_id: String,
        screenshot_ids: Vec<i64>,
    },
}

impl Revocation {
    pub fn validate(&self) -> Result<()> {
        if let Self::Screenshots {
            dataset_id,
            screenshot_ids,
        } = self
        {
            if !valid_id(dataset_id)
                || screenshot_ids.is_empty()
                || screenshot_ids.len() > 128
                || screenshot_ids.iter().any(|id| *id <= 0)
            {
                return Err(BrokerError::InvalidRequest);
            }
        }
        Ok(())
    }
}
