//! Built-in AI features backed by a model endpoint the user configures.
//!
//! [`config`] stores endpoints and their encrypted API keys in the policy file,
//! [`provider`] talks to those endpoints, [`tools`] adapts the MCP contract
//! tools, and [`agent`] runs the search loop that ties them together.
//! [`unattended`] is the entry point for runs nobody is watching, such as
//! future scheduled tasks.

pub mod agent;
pub mod config;
mod context;
pub mod provider;
pub mod recap;
mod tokenizer;
pub mod tools;
pub mod unattended;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use provider::Cancellation;

/// Running AI requests, keyed by the id the frontend chose, so they can be
/// cancelled from a separate command.
#[derive(Default)]
pub struct AiRuntimeState {
    running: Mutex<HashMap<String, Arc<Cancellation>>>,
}

impl AiRuntimeState {
    /// Registers a request. Returns `None` when the id is already running.
    pub fn register(&self, request_id: &str) -> Option<Arc<Cancellation>> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains_key(request_id) {
            return None;
        }
        let cancel = Cancellation::new();
        running.insert(request_id.to_string(), cancel.clone());
        Some(cancel)
    }

    pub fn finish(&self, request_id: &str) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
    }

    pub fn cancel(&self, request_id: &str) -> bool {
        match self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(request_id)
        {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }
}
