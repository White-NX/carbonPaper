//! Durable state and transactional page import for the MiniLM migration.

use super::derived_index::{
    derived_subject_is_active, encode_vector, validate_job_spec, MAX_METADATA_BYTES,
};
use super::policy::disk_totals_for_path;
use super::{DerivedIndexJobSpec, DerivedIndexKind, StorageState};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const GIB: u64 = 1024 * 1024 * 1024;
const MIGRATION_TRANSIENT_ALLOWANCE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinilmMigrationRunRecord {
    pub run_id: String,
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

#[derive(Debug, Clone)]
pub struct MinilmMigrationPageRow {
    pub job: DerivedIndexJobSpec,
    pub vector: Vec<f32>,
    pub outcome: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MinilmMigrationPageCommit {
    pub migrated: u64,
    pub legacy_unverified: u64,
    pub already_current: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinilmMigrationDiskPreflight {
    pub expected_rows: u64,
    pub estimated_database_bytes: u64,
    pub estimated_generation_bytes: u64,
    pub estimated_snapshot_bytes: u64,
    pub estimated_peak_bytes: u64,
    pub required_free_bytes: u64,
    pub available_free_bytes: u64,
    pub sufficient: bool,
}

impl StorageState {
    pub fn create_minilm_migration_run(
        &self,
        run: &MinilmMigrationRunRecord,
    ) -> Result<(), String> {
        validate_run_record(run)?;
        let guard = self.get_connection_named("create_minilm_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO minilm_migration_runs (
                run_id, mode, vector_space_revision, status, phase,
                export_id, export_cursor, chroma_total, chroma_processed,
                migrated, legacy_unverified, already_current, failed, discarded,
                unmappable, removed_extra, publish_current, publish_total,
                required_free_bytes, available_free_bytes,
                monitor_was_running, monitor_was_paused, last_error,
                started_at, heartbeat_at, updated_at, finished_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,
                ?24, ?25, ?26, ?27
            )
            "#,
            run_params(run),
        )
        .map_err(|error| format!("Failed to create MiniLM migration run: {error}"))?;
        Ok(())
    }

    pub fn update_minilm_migration_run(
        &self,
        run: &MinilmMigrationRunRecord,
    ) -> Result<(), String> {
        validate_run_record(run)?;
        let guard = self.get_connection_named("update_minilm_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                r#"
                UPDATE minilm_migration_runs SET
                    mode = ?2, vector_space_revision = ?3, status = ?4,
                    phase = ?5, export_id = ?6, export_cursor = ?7,
                    chroma_total = ?8, chroma_processed = ?9,
                    migrated = ?10, legacy_unverified = ?11,
                    already_current = ?12, failed = ?13, discarded = ?14,
                    unmappable = ?15, removed_extra = ?16,
                    publish_current = ?17, publish_total = ?18,
                    required_free_bytes = ?19, available_free_bytes = ?20,
                    monitor_was_running = ?21, monitor_was_paused = ?22,
                    last_error = ?23, started_at = ?24, heartbeat_at = ?25,
                    updated_at = ?26, finished_at = ?27
                WHERE run_id = ?1
                "#,
                run_params(run),
            )
            .map_err(|error| format!("Failed to update MiniLM migration run: {error}"))?;
        if changed == 0 {
            return Err("MiniLM migration run no longer exists".to_string());
        }
        Ok(())
    }

    pub fn get_latest_minilm_migration_run(
        &self,
    ) -> Result<Option<MinilmMigrationRunRecord>, String> {
        let guard = self.get_connection_named("get_latest_minilm_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &format!(
                "{} ORDER BY datetime(updated_at) DESC, rowid DESC LIMIT 1",
                minilm_run_select_sql()
            ),
            [],
            map_minilm_run,
        )
        .optional()
        .map_err(|error| format!("Failed to read latest MiniLM migration run: {error}"))?
        .map(decode_minilm_run)
        .transpose()
    }

    pub fn get_minilm_migration_run(
        &self,
        run_id: &str,
    ) -> Result<Option<MinilmMigrationRunRecord>, String> {
        let guard = self.get_connection_named("get_minilm_migration_run")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &format!("{} AND run_id = ?1", minilm_run_select_sql()),
            [run_id],
            map_minilm_run,
        )
        .optional()
        .map_err(|error| format!("Failed to read MiniLM migration run: {error}"))?
        .map(decode_minilm_run)
        .transpose()
    }

    pub fn reset_minilm_migration_export(&self, run_id: &str) -> Result<(), String> {
        let mut guard = self.get_connection_named("reset_minilm_migration_export")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start MiniLM reset transaction: {error}"))?;
        tx.execute(
            "DELETE FROM minilm_migration_subjects WHERE run_id = ?1",
            [run_id],
        )
        .map_err(|error| format!("Failed to clear MiniLM migration subjects: {error}"))?;
        tx.execute(
            r#"
            UPDATE minilm_migration_runs
               SET export_id = NULL, export_cursor = 0, chroma_total = 0,
                   chroma_processed = 0, migrated = 0,
                   legacy_unverified = 0, already_current = 0,
                   unmappable = 0, failed = 0, last_error = NULL,
                   heartbeat_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?1
            "#,
            [run_id],
        )
        .map_err(|error| format!("Failed to reset MiniLM migration run: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit MiniLM reset transaction: {error}"))
    }

    pub fn commit_minilm_migration_page(
        &self,
        run_id: &str,
        expected_cursor: u64,
        next_cursor: u64,
        rows: &[MinilmMigrationPageRow],
    ) -> Result<MinilmMigrationPageCommit, String> {
        if next_cursor < expected_cursor {
            return Err("MiniLM migration cursor moved backwards".to_string());
        }
        for row in rows {
            validate_job_spec(&row.job)?;
            if row.job.index_kind != DerivedIndexKind::SemanticText {
                return Err("MiniLM migration can only import semantic text rows".to_string());
            }
            validate_outcome(&row.outcome)?;
            encode_vector(&row.vector)?;
        }

        let mut guard = self.get_connection_named("commit_minilm_migration_page")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start MiniLM page transaction: {error}"))?;
        let stored_cursor: i64 = tx
            .query_row(
                "SELECT export_cursor FROM minilm_migration_runs WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Failed to read MiniLM page cursor: {error}"))?;
        let stored_cursor = u64::try_from(stored_cursor)
            .map_err(|_| "Stored MiniLM migration cursor is invalid".to_string())?;
        if stored_cursor != expected_cursor {
            return Err(format!(
                "MiniLM migration cursor mismatch: expected {expected_cursor}, stored {stored_cursor}"
            ));
        }

        let mut result = MinilmMigrationPageCommit::default();
        for row in rows {
            if !derived_subject_is_active(&tx, row.job.index_kind, &row.job.subject_key)? {
                return Err(format!(
                    "Cannot import inactive MiniLM subject {}",
                    row.job.subject_key
                ));
            }
            let already_current = query_job_is_current(&tx, &row.job)?;
            write_completed_embedding(&tx, row)?;
            tx.execute(
                r#"
                INSERT INTO minilm_migration_subjects (
                    run_id, subject_key, outcome, source_fingerprint, updated_at
                ) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
                ON CONFLICT(run_id, subject_key) DO UPDATE SET
                    outcome = excluded.outcome,
                    source_fingerprint = excluded.source_fingerprint,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    run_id,
                    row.job.subject_key,
                    row.outcome,
                    row.job.source_fingerprint,
                ],
            )
            .map_err(|error| format!("Failed to record MiniLM migration subject: {error}"))?;

            if row.outcome == "legacy_chroma_unverified" {
                result.legacy_unverified = result.legacy_unverified.saturating_add(1);
            } else if already_current {
                result.already_current = result.already_current.saturating_add(1);
            } else {
                result.migrated = result.migrated.saturating_add(1);
            }
        }

        let consumed = next_cursor.saturating_sub(expected_cursor);
        tx.execute(
            r#"
            UPDATE minilm_migration_runs SET
                export_cursor = ?2,
                chroma_processed = chroma_processed + ?3,
                migrated = migrated + ?4,
                legacy_unverified = legacy_unverified + ?5,
                already_current = already_current + ?6,
                heartbeat_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE run_id = ?1
            "#,
            params![
                run_id,
                i64::try_from(next_cursor).map_err(|_| "MiniLM cursor overflow")?,
                i64::try_from(consumed).map_err(|_| "MiniLM page size overflow")?,
                i64::try_from(result.migrated).map_err(|_| "MiniLM migrated count overflow")?,
                i64::try_from(result.legacy_unverified)
                    .map_err(|_| "MiniLM unverified count overflow")?,
                i64::try_from(result.already_current)
                    .map_err(|_| "MiniLM current count overflow")?,
            ],
        )
        .map_err(|error| format!("Failed to advance MiniLM migration cursor: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit MiniLM page transaction: {error}"))?;
        Ok(result)
    }

    pub fn record_minilm_migration_subject(
        &self,
        run_id: &str,
        spec: &DerivedIndexJobSpec,
        outcome: &str,
    ) -> Result<(), String> {
        validate_job_spec(spec)?;
        validate_outcome(outcome)?;
        let guard = self.get_connection_named("record_minilm_migration_subject")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO minilm_migration_subjects (
                run_id, subject_key, outcome, source_fingerprint, updated_at
            ) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
            ON CONFLICT(run_id, subject_key) DO UPDATE SET
                outcome = excluded.outcome,
                source_fingerprint = excluded.source_fingerprint,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![run_id, spec.subject_key, outcome, spec.source_fingerprint],
        )
        .map_err(|error| format!("Failed to record MiniLM migration subject: {error}"))?;
        Ok(())
    }

    pub fn list_minilm_migration_subject_ids(&self, run_id: &str) -> Result<HashSet<i64>, String> {
        let guard = self.get_connection_named("list_minilm_migration_subject_ids")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare("SELECT subject_key FROM minilm_migration_subjects WHERE run_id = ?1")
            .map_err(|error| format!("Failed to prepare MiniLM subject query: {error}"))?;
        let rows = statement
            .query_map([run_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to query MiniLM migration subjects: {error}"))?;
        let mut ids = HashSet::new();
        for row in rows {
            let subject =
                row.map_err(|error| format!("Failed to read MiniLM migration subject: {error}"))?;
            if let Ok(id) = subject.parse::<i64>() {
                if id > 0 && subject == id.to_string() {
                    ids.insert(id);
                }
            }
        }
        Ok(ids)
    }

    pub fn record_minilm_migration_error(
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
            return Err("MiniLM migration diagnostic exceeds storage limit".to_string());
        }
        let guard = self.get_connection_named("record_minilm_migration_error")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            INSERT INTO minilm_migration_run_errors (
                run_id, subject_key, phase, code, error
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![run_id, subject_key, phase, code, error],
        )
        .map_err(|db_error| format!("Failed to record MiniLM migration error: {db_error}"))?;
        Ok(())
    }

    pub fn list_minilm_migration_errors(
        &self,
        run_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let guard = self.get_connection_named("list_minilm_migration_errors")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT subject_key, phase, code, error, created_at
                  FROM minilm_migration_run_errors
                 WHERE run_id = ?1
                 ORDER BY id
                 LIMIT ?2 OFFSET ?3
                "#,
            )
            .map_err(|error| format!("Failed to prepare MiniLM error query: {error}"))?;
        let rows = statement
            .query_map(params![run_id, limit.clamp(1, 500), offset], |row| {
                Ok(serde_json::json!({
                    "subject_key": row.get::<_, Option<String>>(0)?,
                    "phase": row.get::<_, String>(1)?,
                    "code": row.get::<_, String>(2)?,
                    "error": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|error| format!("Failed to query MiniLM migration errors: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Failed to read MiniLM migration error: {error}"))
    }

    pub fn reconcile_minilm_migration_scope(&self, run_id: &str) -> Result<u64, String> {
        let mut guard = self.get_connection_named("reconcile_minilm_migration_scope")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start MiniLM reconcile transaction: {error}"))?;
        let vectors = tx
            .execute(
                r#"
                DELETE FROM derived_embeddings
                 WHERE index_kind = 'semantic_text'
                   AND subject_key NOT IN (
                       SELECT subject_key FROM minilm_migration_subjects WHERE run_id = ?1
                   )
                "#,
                [run_id],
            )
            .map_err(|error| format!("Failed to reconcile MiniLM embeddings: {error}"))?;
        let jobs = tx
            .execute(
                r#"
                DELETE FROM derived_index_jobs
                 WHERE index_kind = 'semantic_text'
                   AND subject_key NOT IN (
                       SELECT subject_key FROM minilm_migration_subjects WHERE run_id = ?1
                   )
                "#,
                [run_id],
            )
            .map_err(|error| format!("Failed to reconcile MiniLM jobs: {error}"))?;
        let removed = u64::try_from(vectors.max(jobs))
            .map_err(|_| "Invalid MiniLM reconcile count".to_string())?;
        tx.execute(
            r#"
            UPDATE minilm_migration_runs
               SET removed_extra = ?2, heartbeat_at = CURRENT_TIMESTAMP,
                   updated_at = CURRENT_TIMESTAMP
             WHERE run_id = ?1
            "#,
            params![
                run_id,
                i64::try_from(removed).map_err(|_| "MiniLM count overflow")?
            ],
        )
        .map_err(|error| format!("Failed to record MiniLM reconcile count: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit MiniLM reconcile transaction: {error}"))?;
        Ok(removed)
    }

    pub fn minilm_migration_disk_preflight(
        &self,
        expected_rows: u64,
    ) -> Result<MinilmMigrationDiskPreflight, String> {
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let (_, available_free_bytes) = disk_totals_for_path(&data_dir).ok_or_else(|| {
            format!(
                "Cannot determine free space for the CarbonPaper data directory ({})",
                data_dir.display()
            )
        })?;
        let estimated_database_bytes = expected_rows.saturating_mul(4 * 1024);
        let estimated_generation_bytes = 256_u64.saturating_add(
            expected_rows.saturating_mul((4 + 24 + 384 * std::mem::size_of::<f32>()) as u64),
        );
        let estimated_snapshot_bytes =
            (1024 * 1024_u64).saturating_add(expected_rows.saturating_mul(24));
        let estimated_peak_bytes = estimated_database_bytes
            .saturating_add(estimated_generation_bytes)
            .saturating_add(estimated_snapshot_bytes)
            .saturating_add(MIGRATION_TRANSIENT_ALLOWANCE);
        let required_free_bytes = estimated_peak_bytes
            .saturating_add(estimated_peak_bytes / 4)
            .saturating_add(GIB);
        Ok(MinilmMigrationDiskPreflight {
            expected_rows,
            estimated_database_bytes,
            estimated_generation_bytes,
            estimated_snapshot_bytes,
            estimated_peak_bytes,
            required_free_bytes,
            available_free_bytes,
            sufficient: available_free_bytes >= required_free_bytes,
        })
    }

    /// Sentinel row in `app_metadata` (same design as the auto-vacuum and
    /// plaintext-backfill markers) recording that the automatic M2.4a copy is
    /// settled for one vector-space revision, so startup does not re-trigger it.
    fn minilm_auto_migration_sentinel_key(vector_space_revision: &str) -> String {
        format!("minilm_auto_migration_done_{vector_space_revision}")
    }

    pub fn is_minilm_auto_migration_done(
        &self,
        vector_space_revision: &str,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("minilm_auto_migration_sentinel")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            "SELECT 1 FROM app_metadata WHERE key = ?1",
            params![Self::minilm_auto_migration_sentinel_key(
                vector_space_revision
            )],
            |_| Ok(true),
        )
        .optional()
        .map(|found| found.unwrap_or(false))
        .map_err(|error| format!("Failed to check MiniLM auto-migration sentinel: {error}"))
    }

    pub fn mark_minilm_auto_migration_done(
        &self,
        vector_space_revision: &str,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("minilm_auto_migration_sentinel")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, '1')",
            params![Self::minilm_auto_migration_sentinel_key(
                vector_space_revision
            )],
        )
        .map_err(|error| format!("Failed to write MiniLM auto-migration sentinel: {error}"))?;
        Ok(())
    }
}

fn query_job_is_current(tx: &Transaction<'_>, spec: &DerivedIndexJobSpec) -> Result<bool, String> {
    tx.query_row(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM derived_index_jobs j
            INNER JOIN derived_embeddings e
              ON e.index_kind = j.index_kind AND e.subject_key = j.subject_key
            WHERE j.index_kind = ?1 AND j.subject_key = ?2
              AND j.status = 'completed'
              AND j.model_id = ?3 AND j.model_revision = ?4
              AND j.embedding_version = ?5 AND j.source_fingerprint = ?6
              AND e.model_id = j.model_id AND e.model_revision = j.model_revision
              AND e.embedding_version = j.embedding_version
              AND e.source_fingerprint = j.source_fingerprint
        )
        "#,
        params![
            spec.index_kind.as_str(),
            spec.subject_key,
            spec.model_id,
            spec.model_revision,
            spec.embedding_version,
            spec.source_fingerprint,
        ],
        |row| row.get(0),
    )
    .map_err(|error| format!("Failed to inspect current MiniLM row: {error}"))
}

fn write_completed_embedding(
    tx: &Transaction<'_>,
    row: &MinilmMigrationPageRow,
) -> Result<(), String> {
    let vector_blob = encode_vector(&row.vector)?;
    let dimensions = i64::try_from(row.vector.len())
        .map_err(|_| "MiniLM embedding dimensions exceed SQLite range".to_string())?;
    tx.execute(
        r#"
        INSERT INTO derived_index_jobs (
            index_kind, subject_key, status, error_code, error, attempts,
            next_retry_at, lease_token, model_id, model_revision,
            embedding_version, source_fingerprint, updated_at
        ) VALUES (?1, ?2, 'completed', NULL, NULL, 0, NULL, NULL,
                  ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
        ON CONFLICT(index_kind, subject_key) DO UPDATE SET
            status = 'completed', error_code = NULL, error = NULL,
            attempts = 0, next_retry_at = NULL, lease_token = NULL,
            model_id = excluded.model_id,
            model_revision = excluded.model_revision,
            embedding_version = excluded.embedding_version,
            source_fingerprint = excluded.source_fingerprint,
            updated_at = CURRENT_TIMESTAMP
        "#,
        params![
            row.job.index_kind.as_str(),
            row.job.subject_key,
            row.job.model_id,
            row.job.model_revision,
            row.job.embedding_version,
            row.job.source_fingerprint,
        ],
    )
    .map_err(|error| format!("Failed to write MiniLM migration ledger: {error}"))?;
    tx.execute(
        r#"
        INSERT INTO derived_embeddings (
            index_kind, subject_key, dimensions, vector_f32, model_id,
            model_revision, embedding_version, source_fingerprint, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
        ON CONFLICT(index_kind, subject_key) DO UPDATE SET
            dimensions = excluded.dimensions,
            vector_f32 = excluded.vector_f32,
            model_id = excluded.model_id,
            model_revision = excluded.model_revision,
            embedding_version = excluded.embedding_version,
            source_fingerprint = excluded.source_fingerprint,
            updated_at = CURRENT_TIMESTAMP
        "#,
        params![
            row.job.index_kind.as_str(),
            row.job.subject_key,
            dimensions,
            vector_blob,
            row.job.model_id,
            row.job.model_revision,
            row.job.embedding_version,
            row.job.source_fingerprint,
        ],
    )
    .map_err(|error| format!("Failed to write MiniLM migration embedding: {error}"))?;
    Ok(())
}

fn validate_run_record(run: &MinilmMigrationRunRecord) -> Result<(), String> {
    if run.run_id.trim().is_empty()
        || run.mode.trim().is_empty()
        || run.vector_space_revision.trim().is_empty()
        || run.status.trim().is_empty()
        || run.phase.trim().is_empty()
    {
        return Err("MiniLM migration run contains an empty required field".to_string());
    }
    Ok(())
}

fn validate_outcome(outcome: &str) -> Result<(), String> {
    match outcome {
        "migrated" | "already_current" | "legacy_chroma_unverified" => Ok(()),
        _ => Err(format!(
            "Unknown MiniLM migration subject outcome: {outcome}"
        )),
    }
}

type RawMinilmRun = (
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
);

fn minilm_run_select_sql() -> &'static str {
    r#"
    SELECT run_id, mode, vector_space_revision, status, phase,
           export_id, export_cursor, chroma_total, chroma_processed,
           migrated, legacy_unverified, already_current, failed, discarded,
           unmappable, removed_extra, publish_current, publish_total,
           required_free_bytes, available_free_bytes,
           monitor_was_running, monitor_was_paused, last_error,
           started_at, heartbeat_at, updated_at, finished_at
      FROM minilm_migration_runs WHERE 1 = 1
    "#
}

fn map_minilm_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMinilmRun> {
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
    ))
}

fn nonnegative(value: i64, name: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Stored MiniLM {name} is invalid: {value}"))
}

fn decode_minilm_run(raw: RawMinilmRun) -> Result<MinilmMigrationRunRecord, String> {
    Ok(MinilmMigrationRunRecord {
        run_id: raw.0,
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
    run: &MinilmMigrationRunRecord,
) -> rusqlite::ParamsFromIter<Vec<rusqlite::types::Value>> {
    use rusqlite::types::Value;
    let values = vec![
        Value::Text(run.run_id.clone()),
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

    fn run_record() -> MinilmMigrationRunRecord {
        MinilmMigrationRunRecord {
            run_id: "run-1".to_string(),
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
    fn page_import_advances_cursor_with_vectors_in_one_transaction() {
        let storage = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (7, 'x', 'h7')",
                    [],
                )
                .unwrap();
        }
        storage.create_minilm_migration_run(&run_record()).unwrap();
        let spec = DerivedIndexJobSpec {
            index_kind: DerivedIndexKind::SemanticText,
            subject_key: "7".to_string(),
            model_id: "model".to_string(),
            model_revision: "revision".to_string(),
            embedding_version: 1,
            source_fingerprint: "source".to_string(),
        };
        let committed = storage
            .commit_minilm_migration_page(
                "run-1",
                0,
                1,
                &[MinilmMigrationPageRow {
                    job: spec.clone(),
                    vector: vec![0.25; 384],
                    outcome: "migrated".to_string(),
                }],
            )
            .unwrap();
        assert_eq!(committed.migrated, 1);
        let run = storage.get_minilm_migration_run("run-1").unwrap().unwrap();
        assert_eq!(run.export_cursor, 1);
        assert_eq!(run.migrated, 1);
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "7")
            .unwrap()
            .is_some());
    }

    #[test]
    fn auto_migration_sentinel_round_trips_per_revision() {
        let storage = test_storage();
        assert!(!storage.is_minilm_auto_migration_done("revision-1").unwrap());
        storage
            .mark_minilm_auto_migration_done("revision-1")
            .unwrap();
        assert!(storage.is_minilm_auto_migration_done("revision-1").unwrap());
        // A new vector-space revision must trigger its own migration.
        assert!(!storage.is_minilm_auto_migration_done("revision-2").unwrap());
        // Re-marking stays idempotent.
        storage
            .mark_minilm_auto_migration_done("revision-1")
            .unwrap();
        assert!(storage.is_minilm_auto_migration_done("revision-1").unwrap());
    }

    // The two tests below cover `crate::minilm_migration`'s dual-write entry
    // point rather than this module's. They live here because that is where the
    // `StorageState` test fixture is: the type's connection field is private to
    // the storage module, and a permanently-vs-transiently rejected mirror is
    // exactly a storage-level distinction.

    #[test]
    fn a_malformed_dual_write_row_is_rejected_permanently_not_queued_forever() {
        // Python queues every failed mirror durably, and the Rust read path
        // stands down while that queue is non-empty. A row whose payload can
        // never become valid must therefore be reported as permanent, or it
        // would hold semantic retrieval on Python for the life of the queue.
        use crate::minilm_migration::{import_minilm_vector_from_python, MINILM_DIMENSIONS};
        let storage = test_storage();

        let rejection = import_minilm_vector_from_python(&storage, 0, vec![0.25; MINILM_DIMENSIONS])
            .err()
            .expect("a non-positive screenshot id is rejected");
        assert!(rejection.permanent, "{}", rejection.message);

        let rejection = import_minilm_vector_from_python(&storage, 5, vec![0.25; 10])
            .err()
            .expect("a wrong-width vector is rejected");
        assert!(rejection.permanent, "{}", rejection.message);
    }

    #[test]
    fn a_locked_session_is_transient_so_the_mirror_is_retried_after_unlock() {
        // The opposite mistake: treating a locked vault as permanent would make
        // Python drop vectors it will never write again, leaving the index
        // silently short with nothing recording the loss.
        use crate::minilm_migration::{import_minilm_vector_from_python, MINILM_DIMENSIONS};
        let storage = test_storage();
        assert!(!storage.is_session_valid());

        let rejection =
            import_minilm_vector_from_python(&storage, 5, vec![0.25; MINILM_DIMENSIONS])
                .err()
                .expect("a locked session cannot read the source row");
        assert!(!rejection.permanent, "{}", rejection.message);
        assert_eq!(rejection.message, "AUTH_REQUIRED");
    }
}
