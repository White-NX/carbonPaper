//! Pieces every derived-index migration needs, regardless of which index it
//! fills.
//!
//! MiniLM (M2.4) and Chinese-CLIP (M2.5 step 7) copy from different Chroma
//! collections, map their ids differently, and validate different vector
//! widths. What they share is the awkward part: waiting out a locked vault
//! without losing the run, pausing and then exactly restoring the monitor,
//! polling an asynchronous Python snapshot to readiness, decoding a Base64
//! float32 page, and publishing a generation on a blocking worker.
//!
//! Everything here is deliberately free of index-specific knowledge. A caller
//! supplies its own command names, dimensions, and subject mapping.

use crate::credential_manager::CredentialManagerState;
use crate::monitor::{authenticated_monitor_command, MonitorState};
use crate::storage::{BackgroundReadError, DerivedIndexKind, StorageState};
use base64::Engine as _;
use chrono::Utc;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const AUTH_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Python enforces a 10 minute logical snapshot deadline; allow a little slack
/// before the Rust side also gives up.
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(11 * 60);

/// How a migration reports the phase it is currently blocked in.
///
/// The helpers below can sit for minutes — waiting for Windows Hello, or for
/// Python to finish walking a collection — and a run that shows nothing during
/// those minutes is indistinguishable from a hung one. Each migration owns its
/// own status struct, so this is the one thing they hand in.
pub trait MigrationPhaseSink: Send + Sync {
    fn set_phase(&self, phase: &str);
}

/// The command names one migration uses to drive a Python snapshot.
///
/// Four names rather than one prefix: the existing MiniLM commands do not
/// follow a single naming rule (`export_task_vectors_page` against
/// `start_task_vectors_export`), and inventing a rule now would mean renaming
/// a shipped protocol for tidiness.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotCommands {
    pub start: &'static str,
    pub status: &'static str,
    pub page: &'static str,
    pub finish: &'static str,
}

#[derive(Deserialize)]
struct ExportStart {
    export_id: String,
}

#[derive(Deserialize)]
struct ExportStatus {
    state: String,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExportPageError {
    pub id: Option<String>,
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct ExportPage {
    pub ids: Vec<String>,
    pub dimensions: usize,
    #[serde(default)]
    pub embeddings_f32_le_b64: String,
    #[serde(default)]
    pub missing_ids: Vec<String>,
    #[serde(default)]
    pub errors: Vec<ExportPageError>,
    pub next_cursor: u64,
    pub done: bool,
    pub total: u64,
}

/// Decode the little-endian float32 page payload into per-id vectors.
///
/// `expected_dimensions` is checked against what Python declares rather than
/// inferred from the payload length, so a collection whose width silently
/// changed is rejected instead of being reshaped into plausible nonsense.
pub fn decode_export_page_vectors(
    page: &ExportPage,
    expected_dimensions: usize,
) -> Result<Vec<Vec<f32>>, String> {
    if page.dimensions != expected_dimensions {
        return Err(format!(
            "Chroma export page has {} dimensions, expected {expected_dimensions}",
            page.dimensions
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(page.embeddings_f32_le_b64.as_bytes())
        .map_err(|error| format!("Invalid Base64 embedding payload: {error}"))?;
    let expected = page
        .ids
        .len()
        .checked_mul(page.dimensions)
        .and_then(|floats| floats.checked_mul(4))
        .ok_or_else(|| "Chroma export page size overflow".to_string())?;
    if bytes.len() != expected {
        return Err(format!(
            "Chroma export payload is {} bytes, expected {expected} for {} ids",
            bytes.len(),
            page.ids.len()
        ));
    }
    let mut vectors = Vec::with_capacity(page.ids.len());
    for row in bytes.chunks_exact(page.dimensions * 4) {
        vectors.push(
            row.chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect(),
        );
    }
    Ok(vectors)
}

/// A migrated vector must be usable for cosine scoring the moment it lands.
///
/// Shared because the failure modes are the same for every index: a width that
/// does not match the model contract, a NaN or infinity that would poison every
/// comparison it touches, and a zero vector whose cosine similarity is
/// undefined. Quarantined as a diagnostic rather than imported — the roadmap's
/// rule is that a migration never manufactures a vector, and importing a broken
/// one would be worse than admitting it is missing.
pub fn validate_migrated_vector(
    vector: &[f32],
    dimensions: usize,
    min_l2_norm: f32,
) -> Result<(), String> {
    if vector.len() != dimensions {
        return Err(format!(
            "Expected {dimensions} dimensions, got {}",
            vector.len()
        ));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err("Embedding contains a non-finite value".to_string());
    }
    let norm_squared: f32 = vector.iter().map(|value| value * value).sum();
    if norm_squared.sqrt() <= min_l2_norm {
        return Err("Embedding is a zero vector".to_string());
    }
    Ok(())
}

pub fn parse_monitor_success(response: serde_json::Value) -> Result<serde_json::Value, String> {
    if let Some(error) = response.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_string());
    }
    if response.get("status").and_then(|value| value.as_str()) == Some("error") {
        return Err(response
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("Monitor command failed")
            .to_string());
    }
    Ok(response)
}

pub fn is_auth_required_error(error: &str) -> bool {
    error.contains("AUTH_REQUIRED")
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// A run id that is unique per process and per instant, prefixed so a log line
/// or a database row says which migration produced it without a join.
pub fn new_run_id(prefix: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(
        Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_le_bytes(),
    );
    digest.update(std::process::id().to_le_bytes());
    let hex = format!("{:x}", digest.finalize());
    format!("{prefix}-{}", &hex[..32])
}

/// Block until the vault is unlocked.
///
/// Cancellation does not exist: a mandatory copy waits for the user rather than
/// abandoning itself. Closing the app is the only interruption, and the run
/// resumes from its persisted cursor on the next launch.
pub async fn wait_for_auth(app: &AppHandle, phase: &dyn MigrationPhaseSink) {
    loop {
        let authenticated = app
            .try_state::<Arc<CredentialManagerState>>()
            .map(|value| value.is_session_valid())
            .unwrap_or(false);
        if authenticated {
            return;
        }
        phase.set_phase("waiting_for_auth");
        tokio::time::sleep(AUTH_POLL_INTERVAL).await;
    }
}

/// One monitor command, retried across a session lock that happens mid-call.
pub async fn monitor_command_with_auth_retry(
    app: &AppHandle,
    phase: &dyn MigrationPhaseSink,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    loop {
        wait_for_auth(app, phase).await;
        let credential = app.state::<Arc<CredentialManagerState>>();
        let monitor = app.state::<MonitorState>();
        let result = authenticated_monitor_command(&credential, &monitor, payload.clone())
            .await
            .and_then(parse_monitor_success);
        match result {
            Err(error) if is_auth_required_error(&error) => continue,
            other => return other,
        }
    }
}

/// One encrypted-content read, retried across a session lock.
pub async fn background_read_with_auth_retry<T>(
    app: &AppHandle,
    phase: &dyn MigrationPhaseSink,
    mut read: impl FnMut(&StorageState) -> Result<T, BackgroundReadError>,
) -> Result<T, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    loop {
        wait_for_auth(app, phase).await;
        match read(&storage) {
            Ok(value) => return Ok(value),
            Err(BackgroundReadError::AuthRequired) => continue,
            Err(BackgroundReadError::Other(error)) => return Err(error),
        }
    }
}

/// Saved monitor/capture state so the migration can restore exactly what the
/// user had, instead of unconditionally resuming.
pub struct MonitorRestore {
    was_running: bool,
    was_paused: bool,
    started_by_migration: bool,
}

/// Start the monitor if it is down and pause capture for the run's duration.
///
/// Both halves are needed: the Chroma collections only speak through the Python
/// monitor, and screenshots taken during the run would race a rewrite of the
/// store they are being written into.
pub async fn prepare_monitor_for_migration(app: &AppHandle) -> Result<MonitorRestore, String> {
    let monitor = app.state::<MonitorState>();
    let capture = app.state::<Arc<crate::capture::CaptureState>>();
    let was_running = monitor
        .process
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    let was_paused = capture.paused.load(Ordering::SeqCst);
    let mut started_by_migration = false;
    if !was_running {
        crate::monitor::start_monitor_impl(app.state::<MonitorState>(), app.clone())
            .await
            .map_err(|error| format!("Cannot start the monitor for the migration: {error}"))?;
        started_by_migration = true;
    }
    if !was_paused {
        let _ = crate::monitor::pause_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
    }
    Ok(MonitorRestore {
        was_running,
        was_paused,
        started_by_migration,
    })
}

/// Pause capture for a maintenance task that does not need Python to be
/// started. ANN bootstrap reads SQLite and launches the Rust ML sidecar only;
/// starting a stopped monitor just to pause it would add avoidable work and
/// briefly change a user's explicit stopped state.
pub async fn pause_capture_for_maintenance(app: &AppHandle) -> Result<MonitorRestore, String> {
    let monitor = app.state::<MonitorState>();
    let capture = app.state::<Arc<crate::capture::CaptureState>>();
    let was_running = monitor
        .process
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    let was_paused = capture.paused.load(Ordering::SeqCst);
    if was_running && !was_paused {
        let _ = crate::monitor::pause_monitor_impl(monitor, capture, app.clone()).await;
    }
    Ok(MonitorRestore {
        was_running,
        was_paused,
        started_by_migration: false,
    })
}

pub async fn restore_monitor_after_migration(app: &AppHandle, restore: &MonitorRestore) {
    if restore.started_by_migration && !restore.was_running {
        let _ = crate::monitor::stop_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
        return;
    }
    if !restore.was_paused && (restore.was_running || restore.started_by_migration) {
        let _ = crate::monitor::resume_monitor_impl(
            app.state::<MonitorState>(),
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
    }
}

impl MonitorRestore {
    pub fn was_running(&self) -> bool {
        self.was_running
    }

    pub fn was_paused(&self) -> bool {
        self.was_paused
    }
}

/// Query a persisted snapshot's readiness without creating a new one.
///
/// `None` when Python no longer knows the export, in memory or on disk, which
/// is the signal to reset the cursor rather than page on against a snapshot
/// that may have been rebuilt in a different order.
pub async fn attach_snapshot(
    app: &AppHandle,
    phase: &dyn MigrationPhaseSink,
    commands: SnapshotCommands,
    export_id: &str,
) -> Result<Option<u64>, String> {
    let response = monitor_command_with_auth_retry(
        app,
        phase,
        serde_json::json!({
            "command": commands.status,
            "export_id": export_id,
        }),
    )
    .await?;
    let status: ExportStatus = serde_json::from_value(response)
        .map_err(|error| format!("Invalid Chroma export status: {error}"))?;
    match status.state.as_str() {
        "ready" => Ok(Some(status.total)),
        _ => Ok(None),
    }
}

/// Start the asynchronous Python ID snapshot and wait until it is ready.
///
/// The start call returns immediately; readiness is polled so a slow Chroma
/// `get` over tens of thousands of rows cannot exhaust one IPC window.
pub async fn wait_for_snapshot(
    app: &AppHandle,
    phase: &dyn MigrationPhaseSink,
    commands: SnapshotCommands,
    export_id: &str,
) -> Result<u64, String> {
    let response = monitor_command_with_auth_retry(
        app,
        phase,
        serde_json::json!({
            "command": commands.start,
            "export_id": export_id,
        }),
    )
    .await?;
    let started: ExportStart = serde_json::from_value(response)
        .map_err(|error| format!("Invalid Chroma export start response: {error}"))?;
    if started.export_id != export_id {
        return Err("Chroma export id mismatch".to_string());
    }

    let deadline = tokio::time::Instant::now() + SNAPSHOT_DEADLINE;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "Chroma ID snapshot did not become ready within its deadline; restart the monitor and retry"
                    .to_string(),
            );
        }
        let response = monitor_command_with_auth_retry(
            app,
            phase,
            serde_json::json!({
                "command": commands.status,
                "export_id": export_id,
            }),
        )
        .await?;
        let status: ExportStatus = serde_json::from_value(response)
            .map_err(|error| format!("Invalid Chroma export status: {error}"))?;
        match status.state.as_str() {
            "ready" => return Ok(status.total),
            "preparing" => tokio::time::sleep(SNAPSHOT_POLL_INTERVAL).await,
            other => {
                return Err(format!(
                    "Chroma ID snapshot is {other}: {}",
                    status.error.unwrap_or_else(|| "no detail".to_string())
                ))
            }
        }
    }
}

/// Best-effort release of the Python-side snapshot after a completed copy.
/// Python's own TTLs cover a failure here.
pub async fn release_snapshot(app: &AppHandle, commands: SnapshotCommands, export_id: &str) {
    let credential = app.state::<Arc<CredentialManagerState>>();
    let monitor = app.state::<MonitorState>();
    let _ = authenticated_monitor_command(
        &credential,
        &monitor,
        serde_json::json!({
            "command": commands.finish,
            "export_id": export_id,
        }),
    )
    .await;
}

/// Publish the derived-index generation on a blocking worker.
///
/// The final `sync_all` cannot be interrupted; callers present that window to
/// the user as a safe write rather than a hang. `progress` receives the phase
/// name and a current/total pair after each step.
pub async fn publish_generation(
    app: &AppHandle,
    index_kind: DerivedIndexKind,
    progress: impl Fn(&str, u64, u64) + Send + 'static,
) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    tokio::task::spawn_blocking(move || {
        storage.publish_derived_index_generation_with_progress(
            index_kind,
            move |phase, current, total| progress(phase, current, total),
            // A sentinel-triggered migration is not cancellable.
            || false,
        )
    })
    .await
    .map_err(|error| format!("Generation publication worker crashed: {error:?}"))?
    .map(|_generation| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(dimensions: usize, ids: &[&str], payload: &[f32]) -> ExportPage {
        let mut bytes = Vec::new();
        for value in payload {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        ExportPage {
            ids: ids.iter().map(|id| id.to_string()).collect(),
            dimensions,
            embeddings_f32_le_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            missing_ids: Vec::new(),
            errors: Vec::new(),
            next_cursor: ids.len() as u64,
            done: true,
            total: ids.len() as u64,
        }
    }

    #[test]
    fn a_page_decodes_into_one_vector_per_id() {
        let decoded =
            decode_export_page_vectors(&page(2, &["a", "b"], &[1.0, 2.0, 3.0, 4.0]), 2).unwrap();
        assert_eq!(decoded, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    }

    #[test]
    fn a_page_of_the_wrong_width_is_rejected_rather_than_reshaped() {
        // 512 CLIP floats reshaped as 384 MiniLM ones would decode into
        // plausible garbage, so the declared width is checked first.
        let error = decode_export_page_vectors(&page(2, &["a"], &[1.0, 2.0]), 3).unwrap_err();
        assert!(error.contains("expected 3"), "unexpected error: {error}");
    }

    #[test]
    fn a_payload_that_does_not_match_its_id_count_is_rejected() {
        let mut short = page(2, &["a", "b"], &[1.0, 2.0]);
        short.total = 2;
        let error = decode_export_page_vectors(&short, 2).unwrap_err();
        assert!(error.contains("bytes"), "unexpected error: {error}");
    }

    #[test]
    fn vector_validation_quarantines_the_three_unusable_shapes() {
        assert!(validate_migrated_vector(&[0.6, 0.8], 2, 1e-6).is_ok());
        assert!(validate_migrated_vector(&[0.6, 0.8, 0.0], 2, 1e-6).is_err());
        assert!(validate_migrated_vector(&[f32::NAN, 1.0], 2, 1e-6).is_err());
        assert!(validate_migrated_vector(&[0.0, 0.0], 2, 1e-6).is_err());
    }

    #[test]
    fn run_ids_carry_their_prefix_and_do_not_repeat() {
        let first = new_run_id("clip");
        let second = new_run_id("clip");
        assert!(first.starts_with("clip-"));
        assert_ne!(first, second);
    }
}
