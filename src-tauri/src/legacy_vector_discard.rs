//! Explicit, recoverable discard of the legacy Chroma vector collections.
//!
//! Up to v0.8.5 the two derived indexes were seeded by copying the old Python
//! vector store (`task_vectors` for MiniLM, `screenshots` for Chinese-CLIP)
//! page by page through the monitor process, and the query and repair paths
//! stayed closed until a once-per-revision sentinel in `app_metadata` said the
//! copy had settled. That copy needed Chroma installed, an unlocked vault, and
//! the whole app in maintenance mode, all to move data that is derived from
//! what SQLite already holds.
//!
//! This module is what replaced it. On startup, for each index whose sentinel
//! is missing, it records a run in `derived_migration_runs` whose status is
//! `discarded`, settles the sentinel, and moves on. Nothing is copied. The
//! consequences are bounded and visible:
//!
//! - MiniLM keeps only the last 30 days and re-encodes them from OCR text
//!   during idle time, so nothing is lost.
//! - CLIP re-encodes the last 7 days on its own; anything older waits for the
//!   backfill the user is asked about (`clip_index.rs::get_clip_backfill_offer`),
//!   which is why a discard clears any earlier answer to that question.
//!
//! Two populations reach this path. A fresh installation has no old data and
//! the discard merely opens the indexes. An installation upgrading straight
//! from v0.8.3 or earlier still has a `chroma_db` directory; the run row and
//! its diagnostic record that its vectors were dropped rather than copied, so
//! the state page and the logs can explain a re-encode instead of leaving it
//! to look like a fault. Once both sentinels are settled the directory itself
//! is removed to reclaim the space.

use crate::clip_contract::CLIP_VECTOR_SPACE_REVISION;
use crate::maintenance_support;
use crate::minilm_contract::MINILM_VECTOR_SPACE_REVISION;
use crate::storage::{DerivedIndexKind, DerivedMigrationRunRecord, StorageState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// `derived_migration_runs.mode` for a run that discarded instead of copying.
pub const DISCARD_MODE: &str = "discard_legacy_chroma_v1";
/// `derived_migration_runs.status` and `.phase` of such a run.
pub const DISCARD_STATUS: &str = "discarded";
/// Diagnostic code recorded against the run, so the reason survives alongside
/// the row rather than only in a log file that rotates away.
pub const DISCARD_CODE: &str = "legacy_collection_discarded";
/// Name of the directory the retired Python vector store kept under the data
/// directory.
pub const LEGACY_VECTOR_DIR: &str = "chroma_db";

/// Which of the two populations an installation belongs to, for the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyPresence {
    /// No `chroma_db` directory: a fresh install, or one already cleaned up.
    Absent,
    /// The directory exists: an upgrade from a release that still wrote it.
    Present,
}

fn legacy_presence(data_dir: &Path) -> LegacyPresence {
    if data_dir.join(LEGACY_VECTOR_DIR).is_dir() {
        LegacyPresence::Present
    } else {
        LegacyPresence::Absent
    }
}

/// The two indexes and the revision each sentinel is keyed by.
const INDEXES: [(DerivedIndexKind, &str, &str); 2] = [
    (
        DerivedIndexKind::SemanticText,
        MINILM_VECTOR_SPACE_REVISION,
        "minilm",
    ),
    (
        DerivedIndexKind::ClipImage,
        CLIP_VECTOR_SPACE_REVISION,
        "clip",
    ),
];

/// What one settled index looked like before it was settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardOutcome {
    pub index_kind: DerivedIndexKind,
    /// `true` when this call wrote the sentinel; `false` when it was already
    /// there, which is every launch after the first.
    pub discarded_now: bool,
    pub legacy: LegacyPresence,
}

fn discard_run(
    index_kind: DerivedIndexKind,
    revision: &str,
    prefix: &str,
    legacy: LegacyPresence,
) -> DerivedMigrationRunRecord {
    let now = maintenance_support::now_rfc3339();
    DerivedMigrationRunRecord {
        index_kind,
        run_id: maintenance_support::new_run_id(prefix),
        mode: DISCARD_MODE.to_string(),
        vector_space_revision: revision.to_string(),
        status: DISCARD_STATUS.to_string(),
        phase: DISCARD_STATUS.to_string(),
        export_id: None,
        export_cursor: 0,
        chroma_total: 0,
        chroma_processed: 0,
        migrated: 0,
        legacy_unverified: 0,
        already_current: 0,
        failed: 0,
        // One row per index is the honest count: the collection, whatever it
        // held, was discarded as a unit. Its row count was never read.
        discarded: u64::from(legacy == LegacyPresence::Present),
        unmappable: 0,
        removed_extra: 0,
        publish_current: 0,
        publish_total: 0,
        required_free_bytes: 0,
        available_free_bytes: 0,
        monitor_was_running: false,
        monitor_was_paused: false,
        last_error: None,
        started_at: now.clone(),
        heartbeat_at: now.clone(),
        updated_at: now.clone(),
        finished_at: Some(now),
    }
}

fn discard_reason(legacy: LegacyPresence) -> &'static str {
    match legacy {
        LegacyPresence::Present => {
            "legacy Chroma collection found and discarded without copying; \
             derived vectors are rebuilt from SQLite and the screenshot files"
        }
        LegacyPresence::Absent => {
            "no legacy Chroma collection on this installation; nothing to copy"
        }
    }
}

/// Settle one index's sentinel, recording a discarded run if it was not
/// already settled. Idempotent: a settled index is left exactly as found.
pub fn settle_index(
    storage: &StorageState,
    index_kind: DerivedIndexKind,
    revision: &str,
    prefix: &str,
    legacy: LegacyPresence,
) -> Result<DiscardOutcome, String> {
    if storage.is_auto_migration_done(index_kind, revision)? {
        return Ok(DiscardOutcome {
            index_kind,
            discarded_now: false,
            legacy,
        });
    }

    let run = discard_run(index_kind, revision, prefix, legacy);
    storage.create_derived_migration_run(&run)?;
    storage.record_derived_migration_error(
        &run.run_id,
        None,
        DISCARD_STATUS,
        DISCARD_CODE,
        discard_reason(legacy),
    )?;
    if index_kind == DerivedIndexKind::ClipImage {
        // The backfill question is posed over a different corpus now: whatever
        // an earlier answer covered, it did not cover a discarded collection.
        storage.clear_backfill_decision(index_kind)?;
    }
    // Written last, so a crash between the row and the sentinel repeats the
    // (idempotent) record rather than leaving a settled index with no history.
    storage.mark_auto_migration_done(index_kind, revision)?;
    Ok(DiscardOutcome {
        index_kind,
        discarded_now: true,
        legacy,
    })
}

/// Settle both indexes against one data directory. Returns the outcomes in
/// index order; an error on the first index does not prevent the second from
/// being attempted, since each gates a different feature.
pub fn settle_all(storage: &StorageState, data_dir: &Path) -> Vec<Result<DiscardOutcome, String>> {
    let legacy = legacy_presence(data_dir);
    INDEXES
        .iter()
        .map(|(kind, revision, prefix)| settle_index(storage, *kind, revision, prefix, legacy))
        .collect()
}

/// Remove the retired vector store directory once nothing can want it.
///
/// Only called after every sentinel is settled, so there is no run left that
/// could have read it. Best effort: a locked file (an antivirus scan, say) is
/// retried at the next launch, not treated as a fault.
pub fn remove_legacy_directory(data_dir: &Path) -> Result<bool, String> {
    let directory = data_dir.join(LEGACY_VECTOR_DIR);
    if !directory.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&directory)
        .map(|()| true)
        .map_err(|error| format!("{}: {error}", directory.display()))
}

fn data_dir_of(storage: &StorageState) -> PathBuf {
    storage
        .data_dir
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Startup entry point. Runs on a blocking worker because it touches SQLite
/// and the file system; it needs neither an unlocked vault nor maintenance
/// mode, since nothing here reads encrypted content.
pub fn spawn_legacy_vector_discard(app: AppHandle) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let data_dir = data_dir_of(&storage);
            let outcomes = settle_all(&storage, &data_dir);
            let mut all_settled = true;
            for outcome in outcomes {
                match outcome {
                    Ok(outcome) if outcome.discarded_now => tracing::info!(
                        "[LEGACY_VECTORS] {} sentinel settled by discard (legacy store {:?})",
                        outcome.index_kind.as_str(),
                        outcome.legacy,
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        all_settled = false;
                        tracing::warn!("[LEGACY_VECTORS] could not settle an index: {error}");
                    }
                }
            }
            if all_settled {
                match remove_legacy_directory(&data_dir) {
                    Ok(true) => {
                        tracing::info!(
                            "[LEGACY_VECTORS] removed the retired vector store directory"
                        )
                    }
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        "[LEGACY_VECTORS] could not remove the retired vector store: {error}"
                    ),
                }
            }
        })
        .await;
        if let Err(error) = result {
            tracing::warn!("[LEGACY_VECTORS] discard task failed: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let storage = crate::storage::test_storage_with_schema(temp.path().to_path_buf());
        (temp, storage)
    }

    #[test]
    fn a_fresh_install_settles_both_sentinels_without_a_legacy_store() {
        let (temp, storage) = test_storage();
        let outcomes = settle_all(&storage, temp.path());
        assert_eq!(outcomes.len(), 2);
        for outcome in &outcomes {
            let outcome = outcome.as_ref().unwrap();
            assert!(outcome.discarded_now);
            assert_eq!(outcome.legacy, LegacyPresence::Absent);
        }
        assert!(storage
            .is_auto_migration_done(DerivedIndexKind::SemanticText, MINILM_VECTOR_SPACE_REVISION)
            .unwrap());
        assert!(storage
            .is_auto_migration_done(DerivedIndexKind::ClipImage, CLIP_VECTOR_SPACE_REVISION)
            .unwrap());
        // The record says there was nothing to discard, in the count too.
        let run = storage
            .get_latest_derived_migration_run(DerivedIndexKind::ClipImage)
            .unwrap()
            .unwrap();
        assert_eq!(run.mode, DISCARD_MODE);
        assert_eq!(run.status, DISCARD_STATUS);
        assert_eq!(run.discarded, 0);
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn an_upgrade_from_an_old_release_records_the_discard_and_clears_the_backfill_answer() {
        let (temp, storage) = test_storage();
        std::fs::create_dir_all(temp.path().join(LEGACY_VECTOR_DIR)).unwrap();
        std::fs::write(
            temp.path().join(LEGACY_VECTOR_DIR).join("chroma.sqlite3"),
            b"old",
        )
        .unwrap();
        // An answer given over the old corpus must not be inherited.
        storage
            .set_backfill_decision(DerivedIndexKind::ClipImage, "declined")
            .unwrap();

        let outcomes = settle_all(&storage, temp.path());
        for outcome in &outcomes {
            assert_eq!(outcome.as_ref().unwrap().legacy, LegacyPresence::Present);
        }
        let run = storage
            .get_latest_derived_migration_run(DerivedIndexKind::SemanticText)
            .unwrap()
            .unwrap();
        assert_eq!(run.discarded, 1);
        let census = storage
            .count_derived_migration_errors_by_code(&run.run_id)
            .unwrap();
        assert_eq!(census.get(DISCARD_CODE).copied(), Some(1));
        assert_eq!(
            storage
                .get_backfill_decision(DerivedIndexKind::ClipImage)
                .unwrap(),
            None
        );

        // The directory goes only once both are settled, and then it is gone.
        assert!(remove_legacy_directory(temp.path()).unwrap());
        assert!(!temp.path().join(LEGACY_VECTOR_DIR).exists());
        assert!(!remove_legacy_directory(temp.path()).unwrap());
    }

    #[test]
    fn a_settled_index_is_left_alone() {
        let (temp, storage) = test_storage();
        // What a v0.8.4/v0.8.5 installation looks like after its real copy.
        storage
            .mark_auto_migration_done(DerivedIndexKind::ClipImage, CLIP_VECTOR_SPACE_REVISION)
            .unwrap();
        storage
            .set_backfill_decision(DerivedIndexKind::ClipImage, "approved")
            .unwrap();

        let outcomes = settle_all(&storage, temp.path());
        let minilm = outcomes[0].as_ref().unwrap();
        let clip = outcomes[1].as_ref().unwrap();
        assert!(minilm.discarded_now);
        assert!(!clip.discarded_now);
        // No discard row was written for the settled index, and its answer to
        // the backfill question survives.
        assert!(storage
            .get_latest_derived_migration_run(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());
        assert_eq!(
            storage
                .get_backfill_decision(DerivedIndexKind::ClipImage)
                .unwrap()
                .as_deref(),
            Some("approved")
        );

        // A second pass changes nothing.
        let again = settle_all(&storage, temp.path());
        assert!(again.iter().all(|o| !o.as_ref().unwrap().discarded_now));
    }

    #[test]
    fn a_new_vector_space_revision_is_discarded_afresh() {
        let (temp, storage) = test_storage();
        storage
            .mark_auto_migration_done(DerivedIndexKind::ClipImage, "some-older-revision")
            .unwrap();
        let outcome = settle_index(
            &storage,
            DerivedIndexKind::ClipImage,
            CLIP_VECTOR_SPACE_REVISION,
            "clip",
            legacy_presence(temp.path()),
        )
        .unwrap();
        assert!(outcome.discarded_now);
    }
}
