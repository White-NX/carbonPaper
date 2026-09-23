//! Durable records left by derived-index migrations, and the sentinels that
//! gate the indexes on them.
//!
//! The page-by-page Chroma copy that once filled these tables is gone. What
//! remains is what the live code still reads: the run history (so an
//! installation that copied its vectors in v0.8.4 or v0.8.5 can still explain
//! what it did, and so `clip_index.rs::get_clip_backfill_offer` can split that
//! run's diagnostics into "skipped" and "failed"), the once-per-revision
//! sentinel that the query and repair paths check, and the backfill decision.
//! `legacy_vector_discard.rs` is the only remaining writer of a run row.

use super::derived_index::MAX_METADATA_BYTES;
use super::{DerivedIndexKind, StorageState};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedMigrationRunRecord {
    pub run_id: String,
    /// Which derived index this run is filling. Runs of different kinds
    /// coexist; each resumes from its own cursor and settles its own sentinel.
    pub index_kind: DerivedIndexKind,
    pub mode: String,
    pub vector_space_revision: String,
    pub status: String,
    pub phase: String,
    pub export_id: Option<String>,
    pub export_cursor: u64,
    pub chroma_total: u64,
    pub chroma_processed: u64,
    pub migrated: u64,
    pub legacy_unverified: u64,
    pub already_current: u64,
    pub failed: u64,
    pub discarded: u64,
    pub unmappable: u64,
    pub removed_extra: u64,
    pub publish_current: u64,
    pub publish_total: u64,
    pub required_free_bytes: u64,
    pub available_free_bytes: u64,
    pub monitor_was_running: bool,
    pub monitor_was_paused: bool,
    pub last_error: Option<String>,
    pub started_at: String,
    pub heartbeat_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

impl StorageState {
    pub fn create_derived_migration_run(
        &self,
        run: &DerivedMigrationRunRecord,
    ) -> Result<(), String> {
        validate_run_record(run)?;
        let guard = self.get_connection_named("create_derived_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO derived_migration_runs (
                run_id, index_kind, mode, vector_space_revision, status, phase,
                export_id, export_cursor, chroma_total, chroma_processed,
                migrated, legacy_unverified, already_current, failed, discarded,
                unmappable, removed_extra, publish_current, publish_total,
                required_free_bytes, available_free_bytes,
                monitor_was_running, monitor_was_paused, last_error,
                started_at, heartbeat_at, updated_at, finished_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27, ?28
            )
            "#,
            run_params(run),
        )
        .map_err(|error| format!("Failed to create derived migration run: {error}"))?;
        Ok(())
    }

    pub fn update_derived_migration_run(
        &self,
        run: &DerivedMigrationRunRecord,
    ) -> Result<(), String> {
        validate_run_record(run)?;
        let guard = self.get_connection_named("update_derived_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                r#"
                UPDATE derived_migration_runs SET
                    index_kind = ?2, mode = ?3, vector_space_revision = ?4,
                    status = ?5, phase = ?6, export_id = ?7, export_cursor = ?8,
                    chroma_total = ?9, chroma_processed = ?10,
                    migrated = ?11, legacy_unverified = ?12,
                    already_current = ?13, failed = ?14, discarded = ?15,
                    unmappable = ?16, removed_extra = ?17,
                    publish_current = ?18, publish_total = ?19,
                    required_free_bytes = ?20, available_free_bytes = ?21,
                    monitor_was_running = ?22, monitor_was_paused = ?23,
                    last_error = ?24, started_at = ?25, heartbeat_at = ?26,
                    updated_at = ?27, finished_at = ?28
                WHERE run_id = ?1
                "#,
                run_params(run),
            )
            .map_err(|error| format!("Failed to update derived migration run: {error}"))?;
        if changed == 0 {
            return Err("Derived migration run no longer exists".to_string());
        }
        Ok(())
    }

    /// Most recently updated run **of one kind**. A CLIP run must not be
    /// mistaken for a MiniLM one when either decides whether to resume.
    pub fn get_latest_derived_migration_run(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<DerivedMigrationRunRecord>, String> {
        let guard = self.get_connection_named("get_latest_derived_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &format!(
                "{} AND index_kind = ?1 ORDER BY datetime(updated_at) DESC, rowid DESC LIMIT 1",
                migration_run_select_sql()
            ),
            [index_kind.as_str()],
            map_migration_run,
        )
        .optional()
        .map_err(|error| format!("Failed to read latest derived migration run: {error}"))?
        .map(decode_migration_run)
        .transpose()
    }

    pub fn get_derived_migration_run(
        &self,
        run_id: &str,
    ) -> Result<Option<DerivedMigrationRunRecord>, String> {
        let guard = self.get_connection_named("get_derived_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &format!("{} AND run_id = ?1", migration_run_select_sql()),
            [run_id],
            map_migration_run,
        )
        .optional()
        .map_err(|error| format!("Failed to read derived migration run: {error}"))?
        .map(decode_migration_run)
        .transpose()
    }

    /// Sentinel row in `app_metadata` (same design as the auto-vacuum and
    /// plaintext-backfill markers) recording that the automatic copy is settled
    /// for one index kind and vector-space revision, so startup does not
    /// re-trigger it.
    ///
    /// `semantic_text` keeps the key M2.4 shipped. Deriving a new one from the
    /// kind name would read as unset on every machine that already migrated and
    /// re-run the whole copy under maintenance mode — the one outcome a sentinel
    /// exists to prevent.
    fn auto_migration_sentinel_key(
        index_kind: DerivedIndexKind,
        vector_space_revision: &str,
    ) -> String {
        match index_kind {
            DerivedIndexKind::SemanticText => {
                format!("minilm_auto_migration_done_{vector_space_revision}")
            }
            DerivedIndexKind::ClipImage => {
                format!("clip_auto_migration_done_{vector_space_revision}")
            }
        }
    }

    pub fn is_auto_migration_done(
        &self,
        index_kind: DerivedIndexKind,
        vector_space_revision: &str,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("auto_migration_sentinel")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            "SELECT 1 FROM app_metadata WHERE key = ?1",
            params![Self::auto_migration_sentinel_key(
                index_kind,
                vector_space_revision
            )],
            |_| Ok(true),
        )
        .optional()
        .map(|found| found.unwrap_or(false))
        .map_err(|error| format!("Failed to check the auto-migration sentinel: {error}"))
    }

    pub fn mark_auto_migration_done(
        &self,
        index_kind: DerivedIndexKind,
        vector_space_revision: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("auto_migration_sentinel")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, '1')",
            params![Self::auto_migration_sentinel_key(
                index_kind,
                vector_space_revision
            )],
        )
        .map_err(|error| format!("Failed to write the auto-migration sentinel: {error}"))?;
        Ok(())
    }

    fn backfill_decision_key(index_kind: DerivedIndexKind) -> String {
        format!("derived_backfill_decision_{}", index_kind.as_str())
    }

    /// What the user answered when offered a backfill of everything the
    /// migration could not deliver, or `None` if they have not been asked.
    ///
    /// A separate row from the migration sentinel because it answers a separate
    /// question. The sentinel records that the copy finished; this records
    /// whether the user wants their own processor spent re-encoding the images
    /// the copy had no vector for. Nothing infers one from the other: a settled
    /// migration on a machine whose Chroma collection was complete leaves
    /// nothing to ask about, and asking anyway would be a dialog with no
    /// content.
    ///
    /// Not keyed by vector-space revision, unlike the sentinel. A revision bump
    /// invalidates every stored vector and therefore poses the question afresh,
    /// so the answer is deliberately cleared at that point rather than silently
    /// inherited — see [`Self::clear_backfill_decision`].
    pub fn get_backfill_decision(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<String>, String> {
        let guard = self.get_connection_named("backfill_decision")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            "SELECT value FROM app_metadata WHERE key = ?1",
            params![Self::backfill_decision_key(index_kind)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("Failed to read the backfill decision: {error}"))
    }

    pub fn set_backfill_decision(
        &self,
        index_kind: DerivedIndexKind,
        decision: &str,
    ) -> Result<(), String> {
        if !matches!(decision, "approved" | "declined") {
            return Err(format!("Unknown backfill decision: {decision}"));
        }
        let guard = self.get_connection_named("backfill_decision")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, ?2)",
            params![Self::backfill_decision_key(index_kind), decision],
        )
        .map_err(|error| format!("Failed to write the backfill decision: {error}"))?;
        Ok(())
    }

    pub fn clear_backfill_decision(&self, index_kind: DerivedIndexKind) -> Result<(), String> {
        let guard = self.get_connection_named("backfill_decision")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "DELETE FROM app_metadata WHERE key = ?1",
            params![Self::backfill_decision_key(index_kind)],
        )
        .map_err(|error| format!("Failed to clear the backfill decision: {error}"))?;
        Ok(())
    }

    /// Record one diagnostic against a run, with or without a subject.
    pub fn record_derived_migration_error(
        &self,
        run_id: &str,
        subject_key: Option<&str>,
        phase: &str,
        code: &str,
        error: &str,
    ) -> Result<(), String> {
        if phase.len() > MAX_METADATA_BYTES
            || code.len() > MAX_METADATA_BYTES
            || error.len() > MAX_METADATA_BYTES
        {
            return Err("Derived migration diagnostic exceeds the metadata size limit".to_string());
        }
        let guard = self.get_connection_named("record_derived_migration_error")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO derived_migration_run_errors (run_id, subject_key, phase, code, error)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![run_id, subject_key, phase, code, error],
        )
        .map_err(|db_error| format!("Failed to record derived migration error: {db_error}"))?;
        Ok(())
    }

    /// How many diagnostics of each `error_code` one run recorded.
    ///
    /// The run row carries a single `unmappable` total, and reporting that to a
    /// user would be misleading: a Chroma id no live screenshot reproduces is
    /// the ordinary consequence of having ever deleted a screenshot, so any
    /// collection with deletion history has a large one and nothing is wrong.
    /// Splitting by code is what separates "skipped, as expected" from "could
    /// not be read", which are the two halves of an honest report.
    pub fn count_derived_migration_errors_by_code(
        &self,
        run_id: &str,
    ) -> Result<std::collections::HashMap<String, u64>, String> {
        let guard = self.get_connection_named("count_derived_migration_errors_by_code")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                "SELECT code, COUNT(*) FROM derived_migration_run_errors
                  WHERE run_id = ?1 GROUP BY code",
            )
            .map_err(|error| format!("Failed to prepare the migration error census: {error}"))?;
        let rows = statement
            .query_map(params![run_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })
            .map_err(|error| format!("Failed to census migration errors: {error}"))?;
        rows.collect::<rusqlite::Result<std::collections::HashMap<String, u64>>>()
            .map_err(|error| format!("Failed to read a migration error census row: {error}"))
    }
}

fn validate_run_record(run: &DerivedMigrationRunRecord) -> Result<(), String> {
    if run.run_id.trim().is_empty()
        || run.mode.trim().is_empty()
        || run.vector_space_revision.trim().is_empty()
        || run.status.trim().is_empty()
        || run.phase.trim().is_empty()
    {
        return Err("Derived migration run contains an empty required field".to_string());
    }
    Ok(())
}

type RawMigrationRun = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    // `index_kind` is projected last so the twenty-seven positional reads that
    // predate it keep their indices.
    String,
);

fn migration_run_select_sql() -> &'static str {
    r#"
    SELECT run_id, mode, vector_space_revision, status, phase,
           export_id, export_cursor, chroma_total, chroma_processed,
           migrated, legacy_unverified, already_current, failed, discarded,
           unmappable, removed_extra, publish_current, publish_total,
           required_free_bytes, available_free_bytes,
           monitor_was_running, monitor_was_paused, last_error,
           started_at, heartbeat_at, updated_at, finished_at,
           index_kind
      FROM derived_migration_runs WHERE 1 = 1
    "#
}

fn map_migration_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMigrationRun> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
        row.get(20)?,
        row.get(21)?,
        row.get(22)?,
        row.get(23)?,
        row.get(24)?,
        row.get(25)?,
        row.get(26)?,
        row.get(27)?,
    ))
}

fn nonnegative(value: i64, name: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Stored migration {name} is invalid: {value}"))
}

fn decode_migration_run(raw: RawMigrationRun) -> Result<DerivedMigrationRunRecord, String> {
    Ok(DerivedMigrationRunRecord {
        run_id: raw.0,
        index_kind: DerivedIndexKind::from_db(&raw.27)?,
        mode: raw.1,
        vector_space_revision: raw.2,
        status: raw.3,
        phase: raw.4,
        export_id: raw.5,
        export_cursor: nonnegative(raw.6, "export cursor")?,
        chroma_total: nonnegative(raw.7, "Chroma total")?,
        chroma_processed: nonnegative(raw.8, "Chroma processed count")?,
        migrated: nonnegative(raw.9, "migrated count")?,
        legacy_unverified: nonnegative(raw.10, "legacy-unverified count")?,
        already_current: nonnegative(raw.11, "already-current count")?,
        failed: nonnegative(raw.12, "failed count")?,
        discarded: nonnegative(raw.13, "discarded count")?,
        unmappable: nonnegative(raw.14, "unmappable count")?,
        removed_extra: nonnegative(raw.15, "removed-extra count")?,
        publish_current: nonnegative(raw.16, "publish progress")?,
        publish_total: nonnegative(raw.17, "publish total")?,
        required_free_bytes: nonnegative(raw.18, "required free bytes")?,
        available_free_bytes: nonnegative(raw.19, "available free bytes")?,
        monitor_was_running: raw.20 != 0,
        monitor_was_paused: raw.21 != 0,
        last_error: raw.22,
        started_at: raw.23,
        heartbeat_at: raw.24,
        updated_at: raw.25,
        finished_at: raw.26,
    })
}

fn run_params(
    run: &DerivedMigrationRunRecord,
) -> rusqlite::ParamsFromIter<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;
    let values = vec![
        Value::Text(run.run_id.clone()),
        Value::Text(run.index_kind.as_str().to_string()),
        Value::Text(run.mode.clone()),
        Value::Text(run.vector_space_revision.clone()),
        Value::Text(run.status.clone()),
        Value::Text(run.phase.clone()),
        run.export_id
            .clone()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        Value::Integer(run.export_cursor as i64),
        Value::Integer(run.chroma_total as i64),
        Value::Integer(run.chroma_processed as i64),
        Value::Integer(run.migrated as i64),
        Value::Integer(run.legacy_unverified as i64),
        Value::Integer(run.already_current as i64),
        Value::Integer(run.failed as i64),
        Value::Integer(run.discarded as i64),
        Value::Integer(run.unmappable as i64),
        Value::Integer(run.removed_extra as i64),
        Value::Integer(run.publish_current as i64),
        Value::Integer(run.publish_total as i64),
        Value::Integer(run.required_free_bytes as i64),
        Value::Integer(run.available_free_bytes as i64),
        Value::Integer(i64::from(run.monitor_was_running)),
        Value::Integer(i64::from(run.monitor_was_paused)),
        run.last_error
            .clone()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        Value::Text(run.started_at.clone()),
        Value::Text(run.heartbeat_at.clone()),
        Value::Text(run.updated_at.clone()),
        run.finished_at
            .clone()
            .map(Value::Text)
            .unwrap_or(Value::Null),
    ];
    rusqlite::params_from_iter(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_storage() -> StorageState {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let path = temp.keep();
        let credential = Arc::new(CredentialManagerState::new(path.clone()));
        let storage = StorageState::new(path, credential);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        storage
    }

    fn run_record() -> DerivedMigrationRunRecord {
        DerivedMigrationRunRecord {
            run_id: "run-1".to_string(),
            index_kind: DerivedIndexKind::SemanticText,
            mode: "copy_chroma_hot_layer".to_string(),
            vector_space_revision: "revision-1".to_string(),
            status: "running".to_string(),
            phase: "copying_chroma".to_string(),
            export_id: Some("export-1".to_string()),
            export_cursor: 0,
            chroma_total: 1,
            chroma_processed: 0,
            migrated: 0,
            legacy_unverified: 0,
            already_current: 0,
            failed: 0,
            discarded: 0,
            unmappable: 0,
            removed_extra: 0,
            publish_current: 0,
            publish_total: 0,
            required_free_bytes: 0,
            available_free_bytes: 0,
            monitor_was_running: true,
            monitor_was_paused: false,
            last_error: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            heartbeat_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: None,
        }
    }

    #[test]
    fn auto_migration_sentinel_round_trips_per_revision_and_kind() {
        use DerivedIndexKind::{ClipImage, SemanticText};
        let storage = test_storage();
        assert!(!storage
            .is_auto_migration_done(SemanticText, "revision-1")
            .unwrap());
        storage
            .mark_auto_migration_done(SemanticText, "revision-1")
            .unwrap();
        assert!(storage
            .is_auto_migration_done(SemanticText, "revision-1")
            .unwrap());
        // A new vector-space revision must trigger its own migration.
        assert!(!storage
            .is_auto_migration_done(SemanticText, "revision-2")
            .unwrap());
        // And so must a different index kind at the same revision: the two
        // copies are unrelated, so one settling must not silence the other.
        assert!(!storage
            .is_auto_migration_done(ClipImage, "revision-1")
            .unwrap());
        // Re-marking stays idempotent.
        storage
            .mark_auto_migration_done(SemanticText, "revision-1")
            .unwrap();
        assert!(storage
            .is_auto_migration_done(SemanticText, "revision-1")
            .unwrap());
    }

    #[test]
    fn the_backfill_decision_is_per_kind_and_survives_only_until_a_fresh_run() {
        use DerivedIndexKind::{ClipImage, SemanticText};
        let storage = test_storage();
        // Unasked is a third state, distinct from both answers: it is what
        // makes the dialog appear exactly once.
        assert_eq!(storage.get_backfill_decision(ClipImage).unwrap(), None);

        storage
            .set_backfill_decision(ClipImage, "declined")
            .unwrap();
        assert_eq!(
            storage.get_backfill_decision(ClipImage).unwrap().as_deref(),
            Some("declined")
        );
        // Declining a CLIP backfill says nothing about any other index.
        assert_eq!(storage.get_backfill_decision(SemanticText).unwrap(), None);

        storage
            .set_backfill_decision(ClipImage, "approved")
            .unwrap();
        assert_eq!(
            storage.get_backfill_decision(ClipImage).unwrap().as_deref(),
            Some("approved")
        );

        // A vector-space revision bump starts a fresh run, which clears the
        // answer: consenting to re-encode a few images is not consent to
        // re-encode a whole history.
        storage.clear_backfill_decision(ClipImage).unwrap();
        assert_eq!(storage.get_backfill_decision(ClipImage).unwrap(), None);

        // Only the two known answers are storable, so a typo cannot become a
        // value the scope check silently reads as "not approved" forever.
        assert!(storage.set_backfill_decision(ClipImage, "yes").is_err());
    }

    #[test]
    fn the_error_census_separates_expected_skips_from_failures() {
        let storage = test_storage();
        storage.create_derived_migration_run(&run_record()).unwrap();
        for (subject, code) in [
            (Some("a"), "orphan_document_id"),
            (Some("b"), "orphan_document_id"),
            (Some("c"), "invalid_vector"),
            (None, "run_failed"),
        ] {
            storage
                .record_derived_migration_error(&run_record().run_id, subject, "copying", code, "x")
                .unwrap();
        }
        let census = storage
            .count_derived_migration_errors_by_code(&run_record().run_id)
            .unwrap();
        // Two orphans is the ordinary result of having deleted two screenshots;
        // reporting three "failures" here would be the misleading total.
        assert_eq!(census.get("orphan_document_id").copied(), Some(2));
        assert_eq!(census.get("invalid_vector").copied(), Some(1));
        assert_eq!(census.get("run_failed").copied(), Some(1));
    }

    #[test]
    fn a_run_of_one_kind_is_not_returned_as_the_latest_of_another() {
        let storage = test_storage();
        storage.create_derived_migration_run(&run_record()).unwrap();
        assert!(storage
            .get_latest_derived_migration_run(DerivedIndexKind::SemanticText)
            .unwrap()
            .is_some());
        // Resuming CLIP from MiniLM's cursor would replay a foreign snapshot
        // against the wrong collection.
        assert!(storage
            .get_latest_derived_migration_run(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());
    }
}
