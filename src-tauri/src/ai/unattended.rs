//! Runs of the AI loop that nobody is watching.
//!
//! Interactive search has the user in front of it: they unlocked the app,
//! they see each step, and they can press stop. A scheduled task has none of
//! that, so this module adds the checks that stand in for the user:
//!
//! - The session must be unlocked. The API key and every tool need it, and a
//!   task never prompts for Windows Hello on its own.
//! - The computer must be idle. Tool calls such as `search_nl` run local
//!   model inference, which must not compete with what the user is doing.
//! - An endpoint outside this computer needs the same consent as interactive
//!   search.
//! - Tools that change stored data are offered only when the task says so.
//!
//! The run's events are collected into [`UnattendedRun`] so the caller can
//! store them and show them later. Nothing triggers these runs yet; the
//! background scheduler is expected to call [`run`] once task definitions
//! exist.

#![allow(dead_code)] // No trigger calls `run` until scheduled tasks exist.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::Manager;

use super::agent::{self, AgentEvent, AgentLimits, AgentOutcome};
use super::config::{is_local_url, AiSettings};
use super::provider::Cancellation;
use super::tools::ToolScope;
use crate::credential_manager::CredentialManagerState;
use crate::idle::IdleState;
use crate::storage::StorageState;

/// A saved instruction for the AI to carry out without the user present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnattendedTask {
    pub id: String,
    pub prompt: String,
    /// `None` uses the default provider at the time of the run.
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub tool_scope: ToolScope,
}

/// Why a task was not started. Every reason is expected to clear up on its
/// own or after a user action, so the caller should try again later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Deferral {
    Locked,
    NotIdle,
    NoProvider,
    RemoteConsentMissing,
    Maintenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnattendedRun {
    pub task_id: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub outcome: Result<AgentOutcome, String>,
    pub events: Vec<AgentEvent>,
}

/// The admission rules, kept free of Tauri state so they can be tested.
pub fn admit(
    session_valid: bool,
    idle: bool,
    maintenance: bool,
    settings: &AiSettings,
    provider_id: Option<&str>,
) -> Result<String, Deferral> {
    if !session_valid {
        return Err(Deferral::Locked);
    }
    if maintenance {
        return Err(Deferral::Maintenance);
    }
    if !idle {
        return Err(Deferral::NotIdle);
    }
    let id = provider_id
        .or_else(|| settings.effective_default_id())
        .ok_or(Deferral::NoProvider)?;
    let provider = settings.find(id).ok_or(Deferral::NoProvider)?;
    if !settings.remote_consent && !is_local_url(&provider.base_url) {
        return Err(Deferral::RemoteConsentMissing);
    }
    Ok(id.to_string())
}

/// Runs `task` if the admission rules allow it. The outer error is a
/// [`Deferral`]; failures during the run are recorded in the returned
/// [`UnattendedRun`] instead.
pub async fn run(
    app_handle: &tauri::AppHandle,
    task: &UnattendedTask,
    cancel: &Cancellation,
) -> Result<UnattendedRun, Deferral> {
    let credential_state = app_handle.state::<Arc<CredentialManagerState>>();
    let storage = app_handle.state::<Arc<StorageState>>();
    let idle = app_handle
        .state::<Arc<IdleState>>()
        .is_idle
        .load(std::sync::atomic::Ordering::Relaxed);
    let settings = AiSettings::load(&storage).map_err(|_| Deferral::NoProvider)?;
    let provider_id = admit(
        credential_state.is_session_valid(),
        idle,
        crate::maintenance::is_active(),
        &settings,
        task.provider_id.as_deref(),
    )?;
    let resolved = settings
        .resolve(&credential_state, &provider_id)
        .map_err(|e| {
            if e.contains("AUTH_REQUIRED") {
                Deferral::Locked
            } else {
                Deferral::NoProvider
            }
        })?;

    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let mut events = Vec::new();
    let mut sink = |event: AgentEvent| events.push(event);
    let limits = AgentLimits {
        tool_scope: task.tool_scope,
        ..AgentLimits::default()
    };
    let outcome = agent::run(
        app_handle,
        &resolved,
        &task.prompt,
        &[],
        limits,
        &mut sink,
        cancel,
    )
    .await
    .map_err(|e| e.to_string());

    Ok(UnattendedRun {
        task_id: task.id.clone(),
        started_at_ms,
        finished_at_ms: chrono::Utc::now().timestamp_millis(),
        outcome,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::config::{ProviderKind, StoredProvider, ToolCalling};

    fn settings(base_url: &str, remote_consent: bool) -> AiSettings {
        AiSettings {
            providers: vec![StoredProvider {
                id: "p".into(),
                name: "P".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: base_url.into(),
                model: "m".into(),
                api_key_encrypted: None,
                tool_calling: ToolCalling::Supported,
                context_tokens: super::super::config::DEFAULT_CONTEXT_TOKENS,
            }],
            default_provider_id: None,
            remote_consent,
        }
    }

    #[test]
    fn locked_busy_or_maintaining_computers_defer_the_task() {
        let s = settings("http://localhost:11434/v1", false);
        assert_eq!(admit(false, true, false, &s, None), Err(Deferral::Locked));
        assert_eq!(admit(true, false, false, &s, None), Err(Deferral::NotIdle));
        assert_eq!(
            admit(true, true, true, &s, None),
            Err(Deferral::Maintenance)
        );
        assert_eq!(admit(true, true, false, &s, None), Ok("p".into()));
    }

    #[test]
    fn remote_endpoints_need_the_same_consent_as_interactive_search() {
        let s = settings("https://api.example.com/v1", false);
        assert_eq!(
            admit(true, true, false, &s, None),
            Err(Deferral::RemoteConsentMissing)
        );
        let s = settings("https://api.example.com/v1", true);
        assert_eq!(admit(true, true, false, &s, Some("p")), Ok("p".into()));
    }

    #[test]
    fn a_missing_provider_defers_instead_of_failing() {
        let s = AiSettings::default();
        assert_eq!(
            admit(true, true, false, &s, None),
            Err(Deferral::NoProvider)
        );
        let s = settings("http://localhost", false);
        assert_eq!(
            admit(true, true, false, &s, Some("gone")),
            Err(Deferral::NoProvider)
        );
    }

    #[test]
    fn tasks_default_to_read_only_tools() {
        let task: UnattendedTask =
            serde_json::from_str(r#"{"id":"t","prompt":"summarize today"}"#).unwrap();
        assert_eq!(task.tool_scope, ToolScope::ReadOnly);
    }
}
