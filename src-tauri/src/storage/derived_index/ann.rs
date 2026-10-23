//! SQLite manifests, retry state, publication and change tails for ANN sidecars.

use super::{
    decode_vector, read_derived_data_epoch, validate_required_text, DerivedAnnBuildFailureUpdate,
    DerivedAnnBuildState, DerivedAnnGeneration, DerivedAnnTailRow, DerivedIndexKind, StorageState,
    MAX_METADATA_BYTES, VISIBLE_EMBEDDING_SOURCE,
};
#[cfg(test)]
use super::{visible_embedding_page_sql, DerivedAnnSnapshotRow};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

pub(super) const ANN_TAIL_UPDATES_SQL: &str = r#"
    SELECT c.subject_key,
           CASE WHEN j.status = 'completed'
                  AND j.model_id = e.model_id AND j.model_revision = e.model_revision
                  AND j.embedding_version = e.embedding_version
                  AND j.source_fingerprint = e.source_fingerprint
                THEN e.dimensions END,
           CASE WHEN j.status = 'completed'
                  AND j.model_id = e.model_id AND j.model_revision = e.model_revision
                  AND j.embedding_version = e.embedding_version
                  AND j.source_fingerprint = e.source_fingerprint
                THEN e.vector_f32 END
    FROM derived_ann_changes c INDEXED BY idx_derived_ann_changes_epoch
    LEFT JOIN derived_embeddings e
      ON e.index_kind = c.index_kind AND e.subject_key = c.subject_key
    LEFT JOIN derived_index_jobs j
      ON j.index_kind = c.index_kind AND j.subject_key = c.subject_key
    WHERE c.index_kind = ?1 AND c.change_epoch > ?2
    ORDER BY c.change_epoch, c.subject_key
    LIMIT ?3
"#;

fn sqlite_i64(value: u64, field: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{field} exceeds SQLite INTEGER range: {value}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedModelContract {
    model_id: String,
    model_revision: String,
    embedding_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedIndexSnapshotMetadata {
    data_epoch: u64,
    row_count: u64,
    subject_key_bytes: u64,
    dimensions: Option<u32>,
    model_contract: Option<DerivedModelContract>,
}

impl StorageState {
    /// Startup runs before derived-index readers are exposed, so it is the safe
    /// point to remove immutable generations that are no longer referenced by
    /// SQLite. Runtime publication never deletes finalized sidecars.
    pub(in crate::storage) fn cleanup_derived_index_sidecars_at_startup(
        &self,
        conn: &rusqlite::Connection,
        data_dir: &Path,
    ) -> Result<(), String> {
        let mut statement = conn
            .prepare(
                r#"
                SELECT file_name FROM derived_index_generations
                UNION ALL
                SELECT flat_file_name FROM derived_ann_generations WHERE status = 'ready'
                UNION ALL
                SELECT ann_file_name FROM derived_ann_generations WHERE status = 'ready'
                "#,
            )
            .map_err(|error| format!("Failed to prepare derived sidecar cleanup: {error}"))?;
        let referenced = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to query referenced derived sidecars: {error}"))?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| format!("Failed to read referenced derived sidecar: {error}"))?;

        let sidecar_dir = data_dir.join("derived-indexes");
        let entries = match fs::read_dir(&sidecar_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                // Sidecars are rebuildable acceleration data. A malformed path,
                // transient sharing violation, or restrictive ACL must not make
                // the authoritative SQLite store unavailable at startup.
                tracing::warn!(
                    "Failed to scan derived sidecars during startup at {}: {}",
                    sidecar_dir.display(),
                    error
                );
                return Ok(());
            }
        };
        for entry in entries.filter_map(Result::ok) {
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let is_finalized = [DerivedIndexKind::SemanticText, DerivedIndexKind::ClipImage]
                .iter()
                .any(|kind| {
                    file_name.starts_with(&format!("{}-", kind.as_str()))
                        && (file_name.ends_with(".cpdvec") || file_name.ends_with(".cpdann"))
                });
            let is_temp = file_name.starts_with('.')
                && (file_name.ends_with(".cpdvec.tmp") || file_name.ends_with(".cpdann.tmp"));
            if (!is_finalized || referenced.contains(&file_name)) && !is_temp {
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    "Failed to remove unreferenced derived sidecar {}: {}",
                    file_name,
                    error
                ),
            }
        }
        Ok(())
    }

    pub fn get_derived_ann_generation(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<DerivedAnnGeneration>, String> {
        let guard = self.get_connection_named("get_derived_ann_generation")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            r#"
            SELECT generation, covered_epoch, flat_file_name, flat_checksum_sha256,
                   ann_file_name, ann_checksum_sha256, row_count, dimensions,
                   model_id, model_revision, embedding_version,
                   sidecar_format_version, ann_format_version, algorithm,
                   implementation_version, metric, quantization, connectivity,
                   expansion_add, expansion_search, created_at
            FROM derived_ann_generations
            WHERE index_kind = ?1 AND status = 'ready'
            "#,
            [index_kind.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, String>(20)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Failed to read ANN generation: {error}"))?
        .map(|raw| decode_ann_generation(index_kind, raw))
        .transpose()
    }

    pub fn get_derived_ann_build_state(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<DerivedAnnBuildState>, String> {
        let guard = self.get_connection_named("get_derived_ann_build_state")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            r#"
            SELECT consecutive_failures, last_failure_at, next_retry_at,
                   last_error_code, last_error, circuit_open, notification_sent
            FROM derived_ann_build_state
            WHERE index_kind = ?1
            "#,
            [index_kind.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Failed to read ANN build state: {error}"))?
        .map(
            |(
                consecutive_failures,
                last_failure_at,
                next_retry_at,
                last_error_code,
                last_error,
                circuit_open,
                notification_sent,
            )| {
                Ok(DerivedAnnBuildState {
                    index_kind,
                    consecutive_failures: u32::try_from(consecutive_failures).map_err(|_| {
                        format!("Invalid ANN consecutive failure count: {consecutive_failures}")
                    })?,
                    last_failure_at,
                    next_retry_at,
                    last_error_code,
                    last_error,
                    circuit_open: circuit_open != 0,
                    notification_sent: notification_sent >= 2,
                })
            },
        )
        .transpose()
    }

    pub fn record_derived_ann_build_failure(
        &self,
        index_kind: DerivedIndexKind,
        consecutive_failures: u32,
        failed_at: &str,
        next_retry_at: &str,
        error_code: &str,
        error: &str,
        circuit_open: bool,
        notify: bool,
    ) -> Result<DerivedAnnBuildFailureUpdate, String> {
        validate_required_text("failed_at", failed_at, MAX_METADATA_BYTES)?;
        validate_required_text("next_retry_at", next_retry_at, MAX_METADATA_BYTES)?;
        validate_required_text("ANN error_code", error_code, MAX_METADATA_BYTES)?;
        validate_required_text("ANN error", error, MAX_METADATA_BYTES)?;
        let mut guard = self.get_connection_named("record_derived_ann_build_failure")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|db_error| format!("Failed to start ANN failure transaction: {db_error}"))?;
        // 0 = no notification, 1 = pending delivery/ack, 2 = delivered. The
        // pending state prevents a second failure from emitting again while
        // still allowing the frontend to recover a startup event it missed.
        let notification_status = tx
            .query_row(
                r#"
                SELECT notification_sent
                FROM derived_ann_build_state
                WHERE index_kind = ?1
                "#,
                [index_kind.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|db_error| format!("Failed to read previous ANN failure: {db_error}"))?;
        let notification_status = notification_status.unwrap_or(0);
        let should_notify = notify && notification_status == 0;
        let next_notification_status = if should_notify {
            1
        } else {
            notification_status
        };
        tx.execute(
            r#"
            INSERT INTO derived_ann_build_state (
                index_kind, consecutive_failures, last_failure_at,
                next_retry_at, last_error_code, last_error,
                circuit_open, notification_sent, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CURRENT_TIMESTAMP)
            ON CONFLICT(index_kind) DO UPDATE SET
                consecutive_failures = excluded.consecutive_failures,
                last_failure_at = excluded.last_failure_at,
                next_retry_at = excluded.next_retry_at,
                last_error_code = excluded.last_error_code,
                last_error = excluded.last_error,
                circuit_open = excluded.circuit_open,
                notification_sent = excluded.notification_sent,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                index_kind.as_str(),
                i64::from(consecutive_failures),
                failed_at,
                next_retry_at,
                error_code,
                error,
                circuit_open,
                next_notification_status,
            ],
        )
        .map_err(|db_error| format!("Failed to record ANN build failure: {db_error}"))?;
        tx.commit()
            .map_err(|db_error| format!("Failed to commit ANN build failure: {db_error}"))?;
        Ok(DerivedAnnBuildFailureUpdate {
            state: DerivedAnnBuildState {
                index_kind,
                consecutive_failures,
                last_failure_at: failed_at.to_string(),
                next_retry_at: next_retry_at.to_string(),
                last_error_code: error_code.to_string(),
                last_error: error.to_string(),
                circuit_open,
                notification_sent: next_notification_status >= 2,
            },
            should_notify,
        })
    }

    pub fn mark_derived_ann_build_notification_sent(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<(), String> {
        let mut guard = self.get_connection_named("mark_derived_ann_build_notification_sent")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        conn.execute(
            r#"
            UPDATE derived_ann_build_state
            SET notification_sent = 2, updated_at = CURRENT_TIMESTAMP
            WHERE index_kind = ?1 AND notification_sent = 1
            "#,
            [index_kind.as_str()],
        )
        .map_err(|error| format!("Failed to acknowledge ANN build notification: {error}"))?;
        Ok(())
    }

    pub fn take_derived_ann_build_notification(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<DerivedAnnBuildState>, String> {
        let mut guard = self.get_connection_named("take_derived_ann_build_notification")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start ANN notification transaction: {error}"))?;
        let raw = tx
            .query_row(
                r#"
                SELECT consecutive_failures, last_failure_at, next_retry_at,
                       last_error_code, last_error, circuit_open
                FROM derived_ann_build_state
                WHERE index_kind = ?1 AND notification_sent = 1 AND circuit_open != 0
                "#,
                [index_kind.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Failed to take ANN build notification: {error}"))?;
        let Some((
            consecutive_failures,
            last_failure_at,
            next_retry_at,
            last_error_code,
            last_error,
            circuit_open,
        )) = raw
        else {
            tx.commit().map_err(|error| {
                format!("Failed to finish empty ANN notification transaction: {error}")
            })?;
            return Ok(None);
        };
        tx.execute(
            r#"
            UPDATE derived_ann_build_state
            SET notification_sent = 2, updated_at = CURRENT_TIMESTAMP
            WHERE index_kind = ?1 AND notification_sent = 1
            "#,
            [index_kind.as_str()],
        )
        .map_err(|error| format!("Failed to mark ANN build notification taken: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit ANN notification transaction: {error}"))?;
        Ok(Some(DerivedAnnBuildState {
            index_kind,
            consecutive_failures: u32::try_from(consecutive_failures).map_err(|_| {
                format!("Invalid ANN consecutive failure count: {consecutive_failures}")
            })?,
            last_failure_at,
            next_retry_at,
            last_error_code,
            last_error,
            circuit_open: circuit_open != 0,
            notification_sent: true,
        }))
    }

    /// Record a ready ANN generation and prune the changes covered by it.
    /// Callers replacing a live reader must hold that reader slot's publication
    /// write boundary through this transaction and the in-memory replacement.
    pub fn record_derived_ann_generation(
        &self,
        generation: &DerivedAnnGeneration,
    ) -> Result<(), String> {
        let mut guard = self.get_connection_named("record_derived_ann_generation")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start ANN manifest transaction: {error}"))?;
        let current_epoch: i64 = tx
            .query_row(
                "SELECT COALESCE((SELECT data_epoch FROM derived_index_state WHERE index_kind = ?1), 0)",
                [generation.index_kind.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| format!("Failed to read ANN publication epoch: {error}"))?;
        let current_epoch = u64::try_from(current_epoch)
            .map_err(|_| format!("Invalid ANN publication epoch: {current_epoch}"))?;
        if current_epoch < generation.covered_epoch {
            return Err("ANN generation covers an epoch newer than SQLite".to_string());
        }
        let generation_id = sqlite_i64(generation.generation, "ANN generation")?;
        let covered_epoch = sqlite_i64(generation.covered_epoch, "ANN covered epoch")?;
        let row_count = sqlite_i64(generation.row_count, "ANN row count")?;
        tx.execute(
            r#"
            INSERT INTO derived_ann_generations (
                index_kind, generation, covered_epoch, flat_file_name,
                flat_checksum_sha256, ann_file_name, ann_checksum_sha256,
                row_count, dimensions, model_id, model_revision,
                embedding_version, sidecar_format_version, ann_format_version,
                algorithm, implementation_version, metric, quantization,
                connectivity, expansion_add, expansion_search, status, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, 'ready', CURRENT_TIMESTAMP
            )
            ON CONFLICT(index_kind) DO UPDATE SET
                generation = excluded.generation,
                covered_epoch = excluded.covered_epoch,
                flat_file_name = excluded.flat_file_name,
                flat_checksum_sha256 = excluded.flat_checksum_sha256,
                ann_file_name = excluded.ann_file_name,
                ann_checksum_sha256 = excluded.ann_checksum_sha256,
                row_count = excluded.row_count,
                dimensions = excluded.dimensions,
                model_id = excluded.model_id,
                model_revision = excluded.model_revision,
                embedding_version = excluded.embedding_version,
                sidecar_format_version = excluded.sidecar_format_version,
                ann_format_version = excluded.ann_format_version,
                algorithm = excluded.algorithm,
                implementation_version = excluded.implementation_version,
                metric = excluded.metric,
                quantization = excluded.quantization,
                connectivity = excluded.connectivity,
                expansion_add = excluded.expansion_add,
                expansion_search = excluded.expansion_search,
                status = 'ready',
                created_at = CURRENT_TIMESTAMP
            "#,
            params![
                generation.index_kind.as_str(),
                generation_id,
                covered_epoch,
                generation.flat_file_name,
                generation.flat_checksum_sha256,
                generation.ann_file_name,
                generation.ann_checksum_sha256,
                row_count,
                generation.dimensions,
                generation.model_id,
                generation.model_revision,
                generation.embedding_version,
                generation.sidecar_format_version,
                generation.ann_format_version,
                generation.algorithm,
                generation.implementation_version,
                generation.metric,
                generation.quantization,
                generation.connectivity,
                generation.expansion_add,
                generation.expansion_search,
            ],
        )
        .map_err(|error| format!("Failed to record ANN generation: {error}"))?;
        tx.execute(
            r#"
            DELETE FROM derived_ann_changes
            WHERE index_kind = ?1 AND change_epoch <= ?2
            "#,
            params![generation.index_kind.as_str(), covered_epoch],
        )
        .map_err(|error| format!("Failed to prune covered ANN changes: {error}"))?;
        tx.execute(
            "DELETE FROM derived_ann_build_state WHERE index_kind = ?1",
            [generation.index_kind.as_str()],
        )
        .map_err(|error| format!("Failed to clear ANN build failure state: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit ANN manifest: {error}"))?;
        Ok(())
    }

    pub fn derived_ann_tail_count(
        &self,
        index_kind: DerivedIndexKind,
        covered_epoch: u64,
    ) -> Result<u64, String> {
        let guard = self.get_connection_named("derived_ann_tail_count")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let count: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*) FROM derived_ann_changes
                WHERE index_kind = ?1 AND change_epoch > ?2
                "#,
                params![
                    index_kind.as_str(),
                    sqlite_i64(covered_epoch, "ANN covered epoch")?
                ],
                |row| row.get(0),
            )
            .map_err(|error| format!("Failed to count ANN tail: {error}"))?;
        u64::try_from(count).map_err(|_| "Invalid ANN tail count".to_string())
    }

    /// Latest visible vectors and tombstones after an immutable ANN base.
    /// `None` vectors are deliberate: the subject changed after the base and
    /// no longer belongs to the current visible set, so base candidates bearing
    /// that key must be suppressed.
    pub fn list_derived_ann_tail(
        &self,
        index_kind: DerivedIndexKind,
        covered_epoch: u64,
        hard_limit: u64,
    ) -> Result<Vec<DerivedAnnTailRow>, String> {
        let count = self.derived_ann_tail_count(index_kind, covered_epoch)?;
        if count > hard_limit {
            return Err(format!("ann_tail_too_large:{count}"));
        }
        self.with_vector_scan_connection("list_derived_ann_tail", |conn| {
            let mut statement = conn
                .prepare(
                    r#"
                    SELECT
                        c.subject_key,
                        CASE WHEN
                               j.status = 'completed'
                           AND j.model_id = e.model_id
                           AND j.model_revision = e.model_revision
                           AND j.embedding_version = e.embedding_version
                           AND j.source_fingerprint = e.source_fingerprint
                        THEN e.dimensions END,
                        CASE WHEN
                               j.status = 'completed'
                           AND j.model_id = e.model_id
                           AND j.model_revision = e.model_revision
                           AND j.embedding_version = e.embedding_version
                           AND j.source_fingerprint = e.source_fingerprint
                        THEN e.vector_f32 END
                    FROM derived_ann_changes c
                    LEFT JOIN derived_embeddings e
                      ON e.index_kind = c.index_kind AND e.subject_key = c.subject_key
                    LEFT JOIN derived_index_jobs j
                      ON j.index_kind = c.index_kind AND j.subject_key = c.subject_key
                    WHERE c.index_kind = ?1 AND c.change_epoch > ?2
                    ORDER BY c.subject_key
                    "#,
                )
                .map_err(|error| format!("Failed to prepare ANN tail query: {error}"))?;
            let rows = statement
                .query_map(
                    params![
                        index_kind.as_str(),
                        sqlite_i64(covered_epoch, "ANN covered epoch")?
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                        ))
                    },
                )
                .map_err(|error| format!("Failed to query ANN tail: {error}"))?;
            let mut out = Vec::with_capacity(count as usize);
            for row in rows {
                let (subject_key, dimensions, blob) =
                    row.map_err(|error| format!("Failed to read ANN tail: {error}"))?;
                let vector = match (dimensions, blob) {
                    (Some(dimensions), Some(blob)) => {
                        let dimensions = usize::try_from(dimensions)
                            .map_err(|_| "Invalid ANN tail dimensions".to_string())?;
                        Some(decode_vector(&blob, dimensions)?)
                    }
                    _ => None,
                };
                out.push(DerivedAnnTailRow {
                    subject_key,
                    vector,
                });
            }
            Ok(out)
        })
    }

    /// Read the data epoch and its latest changes in one SQLite snapshot.
    /// A cached epoch makes unchanged queries constant-size; the epoch index
    /// limits a refresh to subjects changed since the preceding query.
    pub(crate) fn derived_ann_tail_updates(
        &self,
        index_kind: DerivedIndexKind,
        covered_epoch: u64,
        cached_epoch: Option<u64>,
        hard_limit: u64,
        deadline: Option<Instant>,
    ) -> Result<(u64, Vec<DerivedAnnTailRow>), String> {
        let expired = || deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if expired() {
            return Err("query_deadline_exceeded_during_ann_tail".to_string());
        }
        let result = self.with_vector_scan_connection("derived_ann_tail_updates", |conn| {
            let _deadline = crate::storage::connection::QueryDeadline::install(conn, deadline)
                .map_err(|error| error.to_string())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|error| error.to_string())?;
            let epoch = u64::try_from(read_derived_data_epoch(&tx, index_kind)?)
                .map_err(|_| "Invalid ANN data epoch")?;
            if epoch < covered_epoch || cached_epoch.is_some_and(|cached| epoch < cached) {
                return Err("ann_data_epoch_regressed".to_string());
            }
            if cached_epoch == Some(epoch) {
                return Ok((epoch, Vec::new()));
            }
            let after = cached_epoch.unwrap_or(covered_epoch).max(covered_epoch);
            let mut statement = tx
                .prepare(ANN_TAIL_UPDATES_SQL)
                .map_err(|error| error.to_string())?;
            let mut rows = statement
                .query(params![
                    index_kind.as_str(),
                    sqlite_i64(after, "ANN cached epoch")?,
                    sqlite_i64(hard_limit.saturating_add(1), "ANN tail limit")?,
                ])
                .map_err(|error| error.to_string())?;
            let mut changes = Vec::new();
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                if expired() {
                    return Err("query_deadline_exceeded_during_ann_tail".to_string());
                }
                let dimensions: Option<i64> = row.get(1).map_err(|error| error.to_string())?;
                let blob: Option<Vec<u8>> = row.get(2).map_err(|error| error.to_string())?;
                let vector = match (dimensions, blob) {
                    (Some(dimensions), Some(blob)) => Some(decode_vector(
                        &blob,
                        usize::try_from(dimensions).map_err(|_| "Invalid ANN tail dimensions")?,
                    )?),
                    _ => None,
                };
                changes.push(DerivedAnnTailRow {
                    subject_key: row.get(0).map_err(|error| error.to_string())?,
                    vector,
                });
                if changes.len() as u64 > hard_limit {
                    return Err(format!("ann_tail_too_large:{}", changes.len()));
                }
            }
            Ok((epoch, changes))
        });
        if expired() {
            Err("query_deadline_exceeded_during_ann_tail".to_string())
        } else {
            result
        }
    }

    pub(crate) fn derived_index_snapshot_for_ann(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<(u64, u64, u64, u32, String, String, u32), String> {
        self.with_vector_scan_connection("derived_index_snapshot_for_ann", |conn| {
            let snapshot = derived_index_snapshot_metadata_from_conn(conn, index_kind)?;
            let contract = snapshot
                .model_contract
                .ok_or_else(|| "Cannot build ANN from an empty derived index".to_string())?;
            Ok((
                snapshot.data_epoch,
                snapshot.row_count,
                snapshot.subject_key_bytes,
                snapshot.dimensions.ok_or("ANN dimensions are missing")?,
                contract.model_id,
                contract.model_revision,
                contract.embedding_version,
            ))
        })
    }

    /// Test oracle for paged ANN input visibility. Production builds freeze
    /// each page and its cursor in the resumable builder's short transaction.
    #[cfg(test)]
    pub(crate) fn for_each_query_visible_embedding_page_for_ann(
        &self,
        index_kind: DerivedIndexKind,
        limit: u32,
        mut on_page: impl FnMut(Vec<DerivedAnnSnapshotRow>) -> Result<(), String>,
    ) -> Result<(), String> {
        if limit == 0 {
            return Err("ANN snapshot page limit must be greater than zero".to_string());
        }
        self.with_vector_scan_connection("for_each_query_visible_embedding_page_for_ann", |conn| {
            let mut after_subject_key: Option<String> = None;
            loop {
                let page = list_query_visible_ann_snapshot_page_from_conn(
                    conn,
                    index_kind,
                    after_subject_key.as_deref(),
                    limit,
                )?;
                if page.is_empty() {
                    break;
                }
                after_subject_key = page.last().map(|row| row.subject_key.clone());
                on_page(page)?;
            }
            Ok(())
        })
    }
}

type RawSnapshotMetadata = (
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);
type RawAnnGeneration = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    i64,
    i64,
    i64,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    String,
);

fn derived_index_snapshot_metadata_from_conn(
    conn: &rusqlite::Connection,
    index_kind: DerivedIndexKind,
) -> Result<DerivedIndexSnapshotMetadata, String> {
    let data_epoch: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT data_epoch FROM derived_index_state WHERE index_kind = ?1), 0)",
            [index_kind.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| format!("Failed to read derived index epoch: {error}"))?;
    let aggregate_sql = visible_embedding_aggregate_sql();
    let raw = conn
        .query_row(&aggregate_sql, [index_kind.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })
        .map_err(|error| format!("Failed to inspect derived generation rows: {error}"))?;
    decode_snapshot_metadata(data_epoch, raw)
}

#[cfg(test)]
fn list_query_visible_ann_snapshot_page_from_conn(
    conn: &rusqlite::Connection,
    index_kind: DerivedIndexKind,
    after_subject_key: Option<&str>,
    limit: u32,
) -> Result<Vec<DerivedAnnSnapshotRow>, String> {
    let sql = visible_embedding_page_sql();
    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| format!("Failed to prepare ANN snapshot page: {error}"))?;
    let rows = statement
        .query_map(
            params![index_kind.as_str(), after_subject_key.unwrap_or(""), limit],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .map_err(|error| format!("Failed to query ANN snapshot page: {error}"))?;
    rows.map(|row| {
        let (subject_key, dimensions, vector_f32) =
            row.map_err(|error| format!("Failed to read ANN snapshot row: {error}"))?;
        let dimensions = u32::try_from(dimensions)
            .map_err(|_| format!("Invalid ANN snapshot dimensions: {dimensions}"))?;
        let expected_bytes = usize::try_from(dimensions)
            .ok()
            .and_then(|value| value.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| "ANN snapshot vector byte length overflow".to_string())?;
        if vector_f32.len() != expected_bytes {
            return Err(format!(
                "ANN snapshot vector length mismatch: expected {expected_bytes}, got {}",
                vector_f32.len()
            ));
        }
        Ok(DerivedAnnSnapshotRow {
            subject_key,
            dimensions,
            vector_f32,
        })
    })
    .collect()
}

fn decode_ann_generation(
    index_kind: DerivedIndexKind,
    raw: RawAnnGeneration,
) -> Result<DerivedAnnGeneration, String> {
    let (
        generation,
        covered_epoch,
        flat_file_name,
        flat_checksum_sha256,
        ann_file_name,
        ann_checksum_sha256,
        row_count,
        dimensions,
        model_id,
        model_revision,
        embedding_version,
        sidecar_format_version,
        ann_format_version,
        algorithm,
        implementation_version,
        metric,
        quantization,
        connectivity,
        expansion_add,
        expansion_search,
        created_at,
    ) = raw;
    let positive_u64 = |name: &str, value: i64| {
        u64::try_from(value).map_err(|_| format!("Invalid ANN {name}: {value}"))
    };
    let positive_u32 = |name: &str, value: i64| {
        u32::try_from(value).map_err(|_| format!("Invalid ANN {name}: {value}"))
    };
    Ok(DerivedAnnGeneration {
        index_kind,
        generation: positive_u64("generation", generation)?,
        covered_epoch: positive_u64("covered epoch", covered_epoch)?,
        flat_file_name,
        flat_checksum_sha256,
        ann_file_name,
        ann_checksum_sha256,
        row_count: positive_u64("row count", row_count)?,
        dimensions: positive_u32("dimensions", dimensions)?,
        model_id,
        model_revision,
        embedding_version: positive_u32("embedding version", embedding_version)?,
        sidecar_format_version: positive_u32("sidecar format", sidecar_format_version)?,
        ann_format_version: positive_u32("ANN format", ann_format_version)?,
        algorithm,
        implementation_version,
        metric,
        quantization,
        connectivity: positive_u32("connectivity", connectivity)?,
        expansion_add: positive_u32("expansion add", expansion_add)?,
        expansion_search: positive_u32("expansion search", expansion_search)?,
        created_at,
    })
}

fn visible_embedding_aggregate_sql() -> String {
    format!(
        r#"
        SELECT COUNT(*),
               COALESCE(SUM(length(CAST(e.subject_key AS BLOB))), 0),
               MIN(e.dimensions), MAX(e.dimensions),
               MIN(e.model_id), MAX(e.model_id),
               MIN(e.model_revision), MAX(e.model_revision),
               MIN(e.embedding_version), MAX(e.embedding_version)
        {VISIBLE_EMBEDDING_SOURCE}
        "#
    )
}

fn decode_snapshot_metadata(
    data_epoch: i64,
    (
        row_count,
        subject_key_bytes,
        min_dimensions,
        max_dimensions,
        min_model_id,
        max_model_id,
        min_model_revision,
        max_model_revision,
        min_embedding_version,
        max_embedding_version,
    ): RawSnapshotMetadata,
) -> Result<DerivedIndexSnapshotMetadata, String> {
    let data_epoch = u64::try_from(data_epoch)
        .map_err(|_| format!("Invalid derived index epoch: {data_epoch}"))?;
    let row_count = u64::try_from(row_count)
        .map_err(|_| format!("Invalid derived generation row count: {row_count}"))?;
    let subject_key_bytes = u64::try_from(subject_key_bytes)
        .map_err(|_| format!("Invalid derived generation key byte count: {subject_key_bytes}"))?;
    if row_count == 0 {
        return Ok(DerivedIndexSnapshotMetadata {
            data_epoch,
            row_count,
            subject_key_bytes,
            dimensions: None,
            model_contract: None,
        });
    }
    if min_dimensions != max_dimensions {
        return Err("Cannot publish a derived index generation with mixed dimensions".to_string());
    }
    if min_model_id != max_model_id
        || min_model_revision != max_model_revision
        || min_embedding_version != max_embedding_version
    {
        return Err(
            "Cannot publish a derived index generation with mixed model contracts".to_string(),
        );
    }
    let dimensions =
        min_dimensions.ok_or_else(|| "Derived generation dimensions are missing".to_string())?;
    let dimensions = u32::try_from(dimensions)
        .map_err(|_| format!("Invalid derived generation dimensions: {dimensions}"))?;
    let embedding_version = min_embedding_version
        .ok_or_else(|| "Derived generation embedding version is missing".to_string())?;
    let embedding_version = u32::try_from(embedding_version).map_err(|_| {
        format!("Invalid derived generation embedding version: {embedding_version}")
    })?;
    Ok(DerivedIndexSnapshotMetadata {
        data_epoch,
        row_count,
        subject_key_bytes,
        dimensions: Some(dimensions),
        model_contract: Some(DerivedModelContract {
            model_id: min_model_id
                .ok_or_else(|| "Derived generation model id is missing".to_string())?,
            model_revision: min_model_revision
                .ok_or_else(|| "Derived generation model revision is missing".to_string())?,
            embedding_version,
        }),
    })
}
