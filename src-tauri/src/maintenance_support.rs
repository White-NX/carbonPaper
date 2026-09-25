//! Pieces shared by the startup maintenance tasks and the derived-index
//! validators.
//!
//! What is left after the Chroma copies went away: vector validation that the
//! capture encoders and the Smart Cluster scorer share, run ids and timestamps
//! for `derived_migration_runs` rows, and the pause-and-exactly-restore dance
//! the blind-index repair does around the monitor.

use crate::monitor::MonitorState;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// A vector must be usable for cosine scoring the moment it lands.
///
/// Shared because the failure modes are the same for every index: a width that
/// does not match the model contract, a NaN or infinity that would poison every
/// comparison it touches, and a zero vector whose cosine similarity is
/// undefined. Quarantined as a diagnostic rather than written — a broken row
/// in the index would be worse than admitting the row is missing.
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

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// A run id that is unique per process and per instant, prefixed so a log line
/// or a database row says which index produced it without a join.
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

/// Saved monitor/capture state so a maintenance task can restore exactly what
/// the user had, instead of unconditionally resuming.
pub struct MonitorRestore {
    was_running: bool,
    was_paused: bool,
}

/// Pause capture for a maintenance task. Starting a stopped monitor just to
/// pause it would briefly change a user's explicit stopped state.
pub async fn pause_capture_for_maintenance(app: &AppHandle) -> Result<MonitorRestore, String> {
    let monitor = app.state::<MonitorState>();
    let capture = app.state::<Arc<crate::capture::CaptureState>>();
    let was_running = monitor.is_running();
    let was_paused = capture.paused.load(Ordering::SeqCst);
    if was_running && !was_paused {
        let _ = crate::monitor::pause_monitor_impl(capture, app.clone()).await;
    }
    Ok(MonitorRestore {
        was_running,
        was_paused,
    })
}

pub async fn restore_monitor_after_maintenance(app: &AppHandle, restore: &MonitorRestore) {
    if !restore.was_paused && restore.was_running {
        let _ = crate::monitor::resume_monitor_impl(
            app.state::<Arc<crate::capture::CaptureState>>(),
            app.clone(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
