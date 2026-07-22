//! Rust-owned persistence for rebuildable semantic embeddings.
//!
//! SQLite stores the durable derived cache and the per-subject job ledger. A
//! generation-versioned sidecar can be published from completed rows without
//! becoming authoritative; consumers must be able to rebuild it from SQLite.

use super::StorageState;
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SIDECAR_MAGIC: &[u8; 8] = b"CPDVEC01";
const SIDECAR_FORMAT_VERSION: u32 = 3;
const MAX_SUBJECT_KEY_BYTES: usize = 1024;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_VECTOR_DIMENSIONS: usize = 65_536;
const SIDECAR_PAGE_SIZE: u32 = 512;
const LEASE_TOKEN_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedIndexKind {
    SemanticText,
    ClipImage,
}

impl DerivedIndexKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SemanticText => "semantic_text",
            Self::ClipImage => "clip_image",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedIndexJobStatus {
    Pending,
    Processing,
    WaitingForAuth,
    Completed,
    Failed,
    Discarded,
}

impl DerivedIndexJobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::WaitingForAuth => "waiting_for_auth",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Discarded => "discarded",
        }
    }

    fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "processing" => Ok(Self::Processing),
            "waiting_for_auth" => Ok(Self::WaitingForAuth),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "discarded" => Ok(Self::Discarded),
            other => Err(format!("Unknown derived index job status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedIndexJobSpec {
    pub index_kind: DerivedIndexKind,
    pub subject_key: String,
    pub model_id: String,
    pub model_revision: String,
    pub embedding_version: u32,
    pub source_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedEmbeddingWrite {
    pub job: DerivedIndexJobSpec,
    pub lease_token: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedEmbeddingRecord {
    pub job: DerivedIndexJobSpec,
    pub vector: Vec<f32>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedIndexJobRecord {
    pub spec: DerivedIndexJobSpec,
    pub status: DerivedIndexJobStatus,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub attempts: u32,
    pub next_retry_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedIndexGeneration {
    pub index_kind: DerivedIndexKind,
    pub generation: u64,
    pub data_epoch: u64,
    pub file_name: String,
    pub checksum_sha256: String,
    pub row_count: u64,
    pub dimensions: Option<u32>,
    pub model_id: Option<String>,
    pub model_revision: Option<String>,
    pub embedding_version: Option<u32>,
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
    dimensions: Option<u32>,
    model_contract: Option<DerivedModelContract>,
}

struct DerivedWorkerJobUpdate<'a> {
    status: DerivedIndexJobStatus,
    error_code: Option<&'a str>,
    error: Option<&'a str>,
    next_retry_at: Option<&'a str>,
    increment_attempts: bool,
}

impl StorageState {
    /// Queue or re-queue one derived subject. A changed source/model contract
    /// resets its retry budget and immediately hides any stale vector.
    pub fn upsert_derived_index_job(&self, spec: &DerivedIndexJobSpec) -> Result<(), String> {
        validate_job_spec(spec)?;
        let mut guard = self.get_connection_named("upsert_derived_index_job")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived job transaction: {error}"))?;
        if !derived_subject_is_active(&tx, spec.index_kind, &spec.subject_key)? {
            return Err("Cannot queue a derived index job for an inactive subject".to_string());
        }
        tx.execute(
            r#"
            INSERT INTO derived_index_jobs (
                index_kind, subject_key, status, attempts, model_id,
                model_revision, embedding_version, source_fingerprint, updated_at
            ) VALUES (?1, ?2, 'pending', 0, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
            ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                status = 'pending',
                error_code = NULL,
                error = NULL,
                attempts = CASE
                    WHEN derived_index_jobs.model_id != excluded.model_id
                      OR derived_index_jobs.model_revision != excluded.model_revision
                      OR derived_index_jobs.embedding_version != excluded.embedding_version
                      OR derived_index_jobs.source_fingerprint != excluded.source_fingerprint
                    THEN 0 ELSE derived_index_jobs.attempts END,
                next_retry_at = NULL,
                lease_token = NULL,
                model_id = excluded.model_id,
                model_revision = excluded.model_revision,
                embedding_version = excluded.embedding_version,
                source_fingerprint = excluded.source_fingerprint,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                spec.index_kind.as_str(),
                spec.subject_key,
                spec.model_id,
                spec.model_revision,
                spec.embedding_version,
                spec.source_fingerprint,
            ],
        )
        .map_err(|error| format!("Failed to queue derived index job: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit derived job transaction: {error}"))?;
        Ok(())
    }

    pub fn mark_derived_index_job_processing(
        &self,
        spec: &DerivedIndexJobSpec,
    ) -> Result<String, String> {
        validate_job_spec(spec)?;
        let lease_token = new_lease_token();
        let guard = self.get_connection_named("mark_derived_index_job_processing")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                r#"
                UPDATE derived_index_jobs
                SET status = 'processing', error_code = NULL, error = NULL,
                    next_retry_at = NULL, lease_token = ?7,
                    updated_at = CURRENT_TIMESTAMP
                WHERE index_kind = ?1 AND subject_key = ?2
                  AND model_id = ?3 AND model_revision = ?4
                  AND embedding_version = ?5 AND source_fingerprint = ?6
                  AND status IN ('pending', 'failed', 'waiting_for_auth')
                  AND (next_retry_at IS NULL OR next_retry_at <= CURRENT_TIMESTAMP)
                "#,
                params![
                    spec.index_kind.as_str(),
                    spec.subject_key,
                    spec.model_id,
                    spec.model_revision,
                    spec.embedding_version,
                    spec.source_fingerprint,
                    lease_token,
                ],
            )
            .map_err(|error| format!("Failed to claim derived index job: {error}"))?;
        if changed == 0 {
            return Err(
                "Derived index job is missing, already claimed, or no longer queueable".to_string(),
            );
        }
        Ok(lease_token)
    }

    pub fn mark_derived_index_job_waiting_for_auth(
        &self,
        spec: &DerivedIndexJobSpec,
        lease_token: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.set_derived_worker_job_state(
            spec,
            lease_token,
            DerivedWorkerJobUpdate {
                status: DerivedIndexJobStatus::WaitingForAuth,
                error_code: Some("authentication_required"),
                error,
                next_retry_at: None,
                increment_attempts: false,
            },
        )
    }

    /// Records a worker failure. `next_retry_at` accepts RFC3339 or a UTC
    /// `YYYY-MM-DD HH:MM:SS` value and is normalized to SQLite's UTC format.
    pub fn mark_derived_index_job_failed(
        &self,
        spec: &DerivedIndexJobSpec,
        lease_token: &str,
        error_code: &str,
        error: &str,
        next_retry_at: Option<&str>,
    ) -> Result<(), String> {
        self.set_derived_worker_job_state(
            spec,
            lease_token,
            DerivedWorkerJobUpdate {
                status: DerivedIndexJobStatus::Failed,
                error_code: Some(error_code),
                error: Some(error),
                next_retry_at,
                increment_attempts: true,
            },
        )
    }

    pub fn mark_derived_index_job_discarded(
        &self,
        spec: &DerivedIndexJobSpec,
        lease_token: &str,
        error_code: &str,
        error: &str,
    ) -> Result<(), String> {
        validate_job_spec(spec)?;
        validate_required_text("lease_token", lease_token, MAX_METADATA_BYTES)?;
        validate_required_text("error_code", error_code, MAX_METADATA_BYTES)?;
        validate_required_text("error", error, MAX_METADATA_BYTES)?;
        let guard = self.get_connection_named("mark_derived_index_job_discarded")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                r#"
                UPDATE derived_index_jobs
                SET status = 'discarded', error_code = ?7, error = ?8,
                    next_retry_at = NULL, lease_token = NULL,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE index_kind = ?1 AND subject_key = ?2
                   AND model_id = ?3 AND model_revision = ?4
                   AND embedding_version = ?5 AND source_fingerprint = ?6
                   AND status = 'processing' AND lease_token = ?9
                "#,
                params![
                    spec.index_kind.as_str(),
                    spec.subject_key,
                    spec.model_id,
                    spec.model_revision,
                    spec.embedding_version,
                    spec.source_fingerprint,
                    error_code,
                    error,
                    lease_token,
                ],
            )
            .map_err(|db_error| format!("Failed to discard derived index job: {db_error}"))?;
        if changed == 0 {
            return Err(
                "Derived index worker lease is stale or the job is no longer processing"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn set_derived_worker_job_state(
        &self,
        spec: &DerivedIndexJobSpec,
        lease_token: &str,
        update: DerivedWorkerJobUpdate<'_>,
    ) -> Result<(), String> {
        validate_job_spec(spec)?;
        validate_required_text("lease_token", lease_token, MAX_METADATA_BYTES)?;
        validate_optional_text("error_code", update.error_code, MAX_METADATA_BYTES)?;
        validate_optional_text("error", update.error, MAX_METADATA_BYTES)?;
        let next_retry_at = normalize_retry_timestamp(update.next_retry_at)?;
        let guard = self.get_connection_named("set_derived_worker_job_state")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                r#"
            UPDATE derived_index_jobs
            SET status = ?3, error_code = ?4, error = ?5,
                attempts = attempts + ?6, next_retry_at = ?7,
                lease_token = NULL, updated_at = CURRENT_TIMESTAMP
            WHERE index_kind = ?1 AND subject_key = ?2
              AND model_id = ?8 AND model_revision = ?9
              AND embedding_version = ?10 AND source_fingerprint = ?11
              AND status = 'processing' AND lease_token = ?12
            "#,
                params![
                    spec.index_kind.as_str(),
                    spec.subject_key,
                    update.status.as_str(),
                    update.error_code,
                    update.error,
                    i64::from(update.increment_attempts),
                    next_retry_at,
                    spec.model_id,
                    spec.model_revision,
                    spec.embedding_version,
                    spec.source_fingerprint,
                    lease_token,
                ],
            )
            .map_err(|db_error| format!("Failed to update derived index job: {db_error}"))?;
        if changed == 0 {
            return Err(
                "Derived index worker lease is stale or the job is no longer processing"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Startup is the only point where no derived-index workers can still be
    /// alive. Requeue leases left behind by a crash so resumable migrations do
    /// not strand subjects in `processing` forever.
    pub(super) fn recover_interrupted_derived_index_jobs_at_startup(
        &self,
        conn: &rusqlite::Connection,
    ) -> Result<u64, String> {
        let changed = conn
            .execute(
                r#"
                UPDATE derived_index_jobs
                SET status = 'pending', error_code = 'worker_interrupted',
                    error = 'Derived index worker was interrupted before completion',
                    next_retry_at = NULL, lease_token = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE status = 'processing'
                "#,
                [],
            )
            .map_err(|error| format!("Failed to recover interrupted derived jobs: {error}"))?;
        u64::try_from(changed).map_err(|_| "Invalid recovered derived job count".to_string())
    }

    /// Atomically commits the vector and the matching completed ledger state.
    /// If either write fails, the transaction rolls back and no partial vector
    /// becomes query-visible.
    pub fn commit_derived_embedding(&self, write: &DerivedEmbeddingWrite) -> Result<(), String> {
        validate_job_spec(&write.job)?;
        validate_required_text("lease_token", &write.lease_token, MAX_METADATA_BYTES)?;
        let vector_blob = encode_vector(&write.vector)?;
        let dimensions = i64::try_from(write.vector.len())
            .map_err(|_| "Derived embedding dimensions exceed SQLite range".to_string())?;
        let mut guard = self.get_connection_named("commit_derived_embedding")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived embedding transaction: {error}"))?;
        if !derived_subject_is_active(&tx, write.job.index_kind, &write.job.subject_key)? {
            return Err("Cannot commit a derived embedding for an inactive subject".to_string());
        }
        let completed = tx
            .execute(
                r#"
                UPDATE derived_index_jobs
                SET status = 'completed', error_code = NULL, error = NULL,
                    next_retry_at = NULL, lease_token = NULL,
                    updated_at = CURRENT_TIMESTAMP
                WHERE index_kind = ?1 AND subject_key = ?2
                  AND model_id = ?3 AND model_revision = ?4
                  AND embedding_version = ?5 AND source_fingerprint = ?6
                  AND status = 'processing' AND lease_token = ?7
                "#,
                params![
                    write.job.index_kind.as_str(),
                    write.job.subject_key,
                    write.job.model_id,
                    write.job.model_revision,
                    write.job.embedding_version,
                    write.job.source_fingerprint,
                    write.lease_token,
                ],
            )
            .map_err(|error| format!("Failed to complete derived index job: {error}"))?;
        if completed == 0 {
            return Err(
                "Derived index worker lease is stale or the job is no longer processing"
                    .to_string(),
            );
        }
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
                write.job.index_kind.as_str(),
                write.job.subject_key,
                dimensions,
                vector_blob,
                write.job.model_id,
                write.job.model_revision,
                write.job.embedding_version,
                write.job.source_fingerprint,
            ],
        )
        .map_err(|error| format!("Failed to write derived embedding: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit derived embedding: {error}"))?;
        Ok(())
    }

    pub fn get_query_visible_embedding(
        &self,
        index_kind: DerivedIndexKind,
        subject_key: &str,
    ) -> Result<Option<DerivedEmbeddingRecord>, String> {
        validate_required_text("subject_key", subject_key, MAX_SUBJECT_KEY_BYTES)?;
        let guard = self.get_connection_named("get_query_visible_embedding")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &visible_embedding_sql("AND e.subject_key = ?2", ""),
            params![index_kind.as_str(), subject_key],
            map_embedding_row(index_kind),
        )
        .optional()
        .map_err(|error| format!("Failed to read derived embedding: {error}"))?
        .map(|row| decode_embedding_row(index_kind, row))
        .transpose()
    }

    pub fn list_query_visible_embeddings(
        &self,
        index_kind: DerivedIndexKind,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<DerivedEmbeddingRecord>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("list_query_visible_embeddings")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let sql = visible_embedding_sql("", "ORDER BY e.subject_key LIMIT ?2 OFFSET ?3");
        let mut statement = conn
            .prepare(&sql)
            .map_err(|error| format!("Failed to prepare derived embedding query: {error}"))?;
        let rows = statement
            .query_map(
                params![index_kind.as_str(), limit, offset],
                map_embedding_row(index_kind),
            )
            .map_err(|error| format!("Failed to query derived embeddings: {error}"))?;
        rows.map(|row| {
            row.map_err(|error| format!("Failed to read derived embedding row: {error}"))
                .and_then(|row| decode_embedding_row(index_kind, row))
        })
        .collect()
    }

    pub fn get_derived_index_job(
        &self,
        index_kind: DerivedIndexKind,
        subject_key: &str,
    ) -> Result<Option<DerivedIndexJobRecord>, String> {
        validate_required_text("subject_key", subject_key, MAX_SUBJECT_KEY_BYTES)?;
        let guard = self.get_connection_named("get_derived_index_job")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let raw = conn
            .query_row(
                r#"
                SELECT status, error_code, error, attempts, next_retry_at,
                       model_id, model_revision, embedding_version,
                       source_fingerprint, updated_at
                FROM derived_index_jobs
                WHERE index_kind = ?1 AND subject_key = ?2
                "#,
                params![index_kind.as_str(), subject_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Failed to read derived index job: {error}"))?;
        raw.map(
            |(
                status,
                error_code,
                error,
                attempts,
                next_retry_at,
                model_id,
                model_revision,
                embedding_version,
                source_fingerprint,
                updated_at,
            )| {
                Ok(DerivedIndexJobRecord {
                    spec: DerivedIndexJobSpec {
                        index_kind,
                        subject_key: subject_key.to_string(),
                        model_id,
                        model_revision,
                        embedding_version: u32::try_from(embedding_version).map_err(|_| {
                            format!("Invalid stored embedding version: {embedding_version}")
                        })?,
                        source_fingerprint,
                    },
                    status: DerivedIndexJobStatus::from_db(&status)?,
                    error_code,
                    error,
                    attempts: u32::try_from(attempts)
                        .map_err(|_| format!("Invalid stored attempts: {attempts}"))?,
                    next_retry_at,
                    updated_at,
                })
            },
        )
        .transpose()
    }

    pub fn list_derived_index_jobs(
        &self,
        index_kind: DerivedIndexKind,
        status: Option<DerivedIndexJobStatus>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<DerivedIndexJobRecord>, String> {
        let limit = limit.clamp(1, 10_000);
        let status_text = status.map(DerivedIndexJobStatus::as_str);
        let guard = self.get_connection_named("list_derived_index_jobs")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT subject_key, status, error_code, error, attempts,
                       next_retry_at, model_id, model_revision,
                       embedding_version, source_fingerprint, updated_at
                FROM derived_index_jobs
                WHERE index_kind = ?1 AND (?2 IS NULL OR status = ?2)
                ORDER BY updated_at, subject_key
                LIMIT ?3 OFFSET ?4
                "#,
            )
            .map_err(|error| format!("Failed to prepare derived job query: {error}"))?;
        let rows = statement
            .query_map(
                params![index_kind.as_str(), status_text, limit, offset],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .map_err(|error| format!("Failed to query derived jobs: {error}"))?;
        rows.map(|row| {
            let (
                subject_key,
                status,
                error_code,
                error,
                attempts,
                next_retry_at,
                model_id,
                model_revision,
                embedding_version,
                source_fingerprint,
                updated_at,
            ) = row.map_err(|db_error| format!("Failed to read derived job row: {db_error}"))?;
            Ok(DerivedIndexJobRecord {
                spec: DerivedIndexJobSpec {
                    index_kind,
                    subject_key,
                    model_id,
                    model_revision,
                    embedding_version: u32::try_from(embedding_version).map_err(|_| {
                        format!("Invalid stored embedding version: {embedding_version}")
                    })?,
                    source_fingerprint,
                },
                status: DerivedIndexJobStatus::from_db(&status)?,
                error_code,
                error,
                attempts: u32::try_from(attempts)
                    .map_err(|_| format!("Invalid stored attempts: {attempts}"))?,
                next_retry_at,
                updated_at,
            })
        })
        .collect()
    }

    /// Deletes both the cached vector and its ledger row in one transaction.
    pub fn delete_derived_index_subject(
        &self,
        index_kind: DerivedIndexKind,
        subject_key: &str,
    ) -> Result<bool, String> {
        validate_required_text("subject_key", subject_key, MAX_SUBJECT_KEY_BYTES)?;
        let mut guard = self.get_connection_named("delete_derived_index_subject")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived deletion transaction: {error}"))?;
        let vectors = tx
            .execute(
                "DELETE FROM derived_embeddings WHERE index_kind = ?1 AND subject_key = ?2",
                params![index_kind.as_str(), subject_key],
            )
            .map_err(|error| format!("Failed to delete derived embedding: {error}"))?;
        let jobs = tx
            .execute(
                "DELETE FROM derived_index_jobs WHERE index_kind = ?1 AND subject_key = ?2",
                params![index_kind.as_str(), subject_key],
            )
            .map_err(|error| format!("Failed to delete derived index job: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit derived deletion: {error}"))?;
        Ok(vectors > 0 || jobs > 0)
    }

    /// Invalidates rows that do not match the selected model contract. Stale
    /// vectors are deleted and their ledger rows become explicit pending work.
    pub fn invalidate_derived_index_model(
        &self,
        index_kind: DerivedIndexKind,
        model_id: &str,
        model_revision: &str,
        embedding_version: u32,
    ) -> Result<u64, String> {
        validate_required_text("model_id", model_id, MAX_METADATA_BYTES)?;
        validate_required_text("model_revision", model_revision, MAX_METADATA_BYTES)?;
        if embedding_version == 0 {
            return Err("embedding_version must be greater than zero".to_string());
        }
        let mut guard = self.get_connection_named("invalidate_derived_index_model")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|error| {
            format!("Failed to start derived invalidation transaction: {error}")
        })?;
        let changed = tx
            .execute(
                r#"
                UPDATE derived_index_jobs
                SET status = 'pending', error_code = 'model_version_changed',
                    error = 'Derived embedding model contract changed', attempts = 0,
                    next_retry_at = NULL, lease_token = NULL,
                    model_id = ?2, model_revision = ?3,
                    embedding_version = ?4, updated_at = CURRENT_TIMESTAMP
                WHERE index_kind = ?1 AND status != 'discarded' AND (
                    model_id != ?2 OR model_revision != ?3 OR embedding_version != ?4
                    OR EXISTS (
                        SELECT 1 FROM derived_embeddings e
                        WHERE e.index_kind = derived_index_jobs.index_kind
                          AND e.subject_key = derived_index_jobs.subject_key
                          AND (e.model_id != ?2 OR e.model_revision != ?3
                               OR e.embedding_version != ?4)
                    )
                )
                "#,
                params![
                    index_kind.as_str(),
                    model_id,
                    model_revision,
                    embedding_version
                ],
            )
            .map_err(|error| format!("Failed to invalidate derived jobs: {error}"))?;
        tx.execute(
            r#"
            DELETE FROM derived_embeddings
            WHERE index_kind = ?1 AND (
                model_id != ?2 OR model_revision != ?3 OR embedding_version != ?4
            )
            "#,
            params![
                index_kind.as_str(),
                model_id,
                model_revision,
                embedding_version
            ],
        )
        .map_err(|error| format!("Failed to delete stale derived embeddings: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit derived invalidation: {error}"))?;
        u64::try_from(changed).map_err(|_| "Invalid derived invalidation count".to_string())
    }

    /// Writes a checksummed, immutable flat-vector generation through a temp
    /// file and atomic rename. A later ANN implementation can replace the
    /// payload format without changing SQLite ownership or generation safety.
    pub fn publish_derived_index_generation(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<DerivedIndexGeneration, String> {
        if self.is_migration_in_progress() {
            return Err("Cannot publish a derived index during data migration".to_string());
        }
        let _publish_guard = self
            .derived_generation_publish_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Recheck after acquiring the shared publication/migration boundary so
        // a migration that won the race cannot overlap filesystem publication.
        if self.is_migration_in_progress() {
            return Err("Cannot publish a derived index during data migration".to_string());
        }
        // Copy the path without holding data_dir while acquiring the database
        // mutex. Existing image reads acquire those locks in the opposite order.
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let snapshot = self.get_derived_index_snapshot_metadata(index_kind)?;
        let generation = next_generation_id()?;
        let sidecar_dir = data_dir.join("derived-indexes");
        fs::create_dir_all(&sidecar_dir)
            .map_err(|error| format!("Failed to create derived index directory: {error}"))?;
        let file_name = format!("{}-{generation}.cpdvec", index_kind.as_str());
        let final_path = sidecar_dir.join(&file_name);
        let temp_path = sidecar_dir.join(format!(".{file_name}.tmp"));
        let checksum_sha256 =
            match self.write_sidecar_streaming(&temp_path, index_kind, generation, &snapshot) {
                Ok(checksum) => checksum,
                Err(error) => {
                    let _ = fs::remove_file(&temp_path);
                    return Err(error);
                }
            };
        if let Err(error) = verify_sidecar(&temp_path, &checksum_sha256) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        fs::rename(&temp_path, &final_path).map_err(|error| {
            let _ = fs::remove_file(&temp_path);
            format!("Failed to publish derived index generation: {error}")
        })?;

        let record = DerivedIndexGeneration {
            index_kind,
            generation,
            data_epoch: snapshot.data_epoch,
            file_name,
            checksum_sha256,
            row_count: snapshot.row_count,
            dimensions: snapshot.dimensions,
            model_id: snapshot
                .model_contract
                .as_ref()
                .map(|value| value.model_id.clone()),
            model_revision: snapshot
                .model_contract
                .as_ref()
                .map(|value| value.model_revision.clone()),
            embedding_version: snapshot
                .model_contract
                .as_ref()
                .map(|value| value.embedding_version),
        };
        if let Err(error) = self.record_derived_index_generation(&record) {
            let _ = fs::remove_file(&final_path);
            return Err(error);
        }
        Ok(record)
    }

    /// Startup runs before derived-index readers are exposed, so it is the safe
    /// point to remove immutable generations that are no longer referenced by
    /// SQLite. Runtime publication never deletes finalized sidecars.
    pub(super) fn cleanup_derived_index_sidecars_at_startup(
        &self,
        conn: &rusqlite::Connection,
        data_dir: &Path,
    ) -> Result<(), String> {
        let mut statement = conn
            .prepare(
                r#"
                SELECT g.file_name
                FROM derived_index_generations g
                LEFT JOIN derived_index_state s ON s.index_kind = g.index_kind
                WHERE g.data_epoch = COALESCE(s.data_epoch, 0)
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
                        && file_name.ends_with(".cpdvec")
                });
            let is_temp = file_name.starts_with('.') && file_name.ends_with(".cpdvec.tmp");
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

    pub fn get_derived_index_generation(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<Option<DerivedIndexGeneration>, String> {
        let guard = self.get_connection_named("get_derived_index_generation")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            r#"
            SELECT g.generation, g.data_epoch, g.file_name, g.checksum_sha256,
                   g.row_count, g.dimensions, g.model_id, g.model_revision,
                   g.embedding_version
            FROM derived_index_generations g
            LEFT JOIN derived_index_state s ON s.index_kind = g.index_kind
            WHERE g.index_kind = ?1
              AND g.data_epoch = COALESCE(s.data_epoch, 0)
            "#,
            [index_kind.as_str()],
            |row| {
                Ok(DerivedIndexGeneration {
                    index_kind,
                    generation: row.get(0)?,
                    data_epoch: row.get(1)?,
                    file_name: row.get(2)?,
                    checksum_sha256: row.get(3)?,
                    row_count: row.get(4)?,
                    dimensions: row.get(5)?,
                    model_id: row.get(6)?,
                    model_revision: row.get(7)?,
                    embedding_version: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Failed to read derived index generation: {error}"))
    }

    fn record_derived_index_generation(
        &self,
        generation: &DerivedIndexGeneration,
    ) -> Result<(), String> {
        let mut guard = self.get_connection_named("record_derived_index_generation")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived generation transaction: {error}"))?;
        let current_epoch: i64 = tx
            .query_row(
                "SELECT COALESCE((SELECT data_epoch FROM derived_index_state WHERE index_kind = ?1), 0)",
                [generation.index_kind.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| format!("Failed to read derived index epoch: {error}"))?;
        let current_epoch = u64::try_from(current_epoch)
            .map_err(|_| format!("Invalid derived index epoch: {current_epoch}"))?;
        if current_epoch != generation.data_epoch {
            return Err("Derived index changed while generation was being published".to_string());
        }
        tx.execute(
            r#"
            INSERT INTO derived_index_generations (
                index_kind, generation, data_epoch, file_name, checksum_sha256,
                row_count, dimensions, model_id, model_revision, embedding_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(index_kind) DO UPDATE SET
                generation = excluded.generation,
                data_epoch = excluded.data_epoch,
                file_name = excluded.file_name,
                checksum_sha256 = excluded.checksum_sha256,
                row_count = excluded.row_count,
                dimensions = excluded.dimensions,
                model_id = excluded.model_id,
                model_revision = excluded.model_revision,
                embedding_version = excluded.embedding_version,
                created_at = CURRENT_TIMESTAMP
            "#,
            params![
                generation.index_kind.as_str(),
                generation.generation,
                generation.data_epoch,
                generation.file_name,
                generation.checksum_sha256,
                generation.row_count,
                generation.dimensions,
                generation.model_id,
                generation.model_revision,
                generation.embedding_version,
            ],
        )
        .map_err(|error| format!("Failed to record derived index generation: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit derived index generation: {error}"))?;
        Ok(())
    }

    fn get_derived_index_snapshot_metadata(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<DerivedIndexSnapshotMetadata, String> {
        let guard = self.get_connection_named("get_derived_index_snapshot_metadata")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
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
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            })
            .map_err(|error| format!("Failed to inspect derived generation rows: {error}"))?;
        decode_snapshot_metadata(data_epoch, raw)
    }

    fn list_query_visible_embedding_page(
        &self,
        index_kind: DerivedIndexKind,
        after_subject_key: Option<&str>,
        limit: u32,
    ) -> Result<Vec<DerivedEmbeddingRecord>, String> {
        let guard = self.get_connection_named("list_query_visible_embedding_page")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let sql = visible_embedding_sql(
            "AND (?2 IS NULL OR e.subject_key > ?2)",
            "ORDER BY e.subject_key LIMIT ?3",
        );
        let mut statement = conn
            .prepare(&sql)
            .map_err(|error| format!("Failed to prepare derived generation page: {error}"))?;
        let rows = statement
            .query_map(
                params![index_kind.as_str(), after_subject_key, limit],
                map_embedding_row(index_kind),
            )
            .map_err(|error| format!("Failed to query derived generation page: {error}"))?;
        rows.map(|row| {
            row.map_err(|error| format!("Failed to read derived generation row: {error}"))
                .and_then(|row| decode_embedding_row(index_kind, row))
        })
        .collect()
    }

    fn write_sidecar_streaming(
        &self,
        path: &Path,
        index_kind: DerivedIndexKind,
        generation: u64,
        snapshot: &DerivedIndexSnapshotMetadata,
    ) -> Result<String, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Failed to create derived index temp file: {error}"))?;
        let mut writer = BufWriter::new(file);
        let mut hasher = Sha256::new();
        write_sidecar_header(&mut writer, &mut hasher, index_kind, generation, snapshot)?;

        let mut after_subject_key: Option<String> = None;
        let mut written_rows = 0u64;
        loop {
            let page = self.list_query_visible_embedding_page(
                index_kind,
                after_subject_key.as_deref(),
                SIDECAR_PAGE_SIZE,
            )?;
            if page.is_empty() {
                break;
            }
            for row in &page {
                write_sidecar_row(&mut writer, &mut hasher, row)?;
            }
            written_rows = written_rows
                .checked_add(page.len() as u64)
                .ok_or_else(|| "Derived generation row count overflow".to_string())?;
            after_subject_key = page.last().map(|row| row.job.subject_key.clone());
        }
        if written_rows != snapshot.row_count {
            return Err(format!(
                "Derived index changed while generation was being streamed: expected {} rows, wrote {written_rows}",
                snapshot.row_count
            ));
        }

        writer
            .flush()
            .map_err(|error| format!("Failed to flush derived index temp file: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Failed to sync derived index temp file: {error}"))?;
        Ok(hex::encode(hasher.finalize()))
    }
}

type RawEmbeddingRow = (String, i64, Vec<u8>, String, String, i64, String, String);
type RawSnapshotMetadata = (
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

fn derived_subject_is_active(
    conn: &rusqlite::Connection,
    index_kind: DerivedIndexKind,
    subject_key: &str,
) -> Result<bool, String> {
    match index_kind {
        DerivedIndexKind::SemanticText => {
            let screenshot_id = subject_key.parse::<i64>().map_err(|error| {
                format!("Invalid semantic derived subject key '{subject_key}': {error}")
            })?;
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id = ?1 AND is_deleted = 0)",
                [screenshot_id],
                |row| row.get(0),
            )
        }
        DerivedIndexKind::ClipImage => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ?1 AND is_deleted = 0)",
            [subject_key],
            |row| row.get(0),
        ),
    }
    .map_err(|error| format!("Failed to validate derived index subject: {error}"))
}

fn visible_embedding_sql(predicate: &str, suffix: &str) -> String {
    format!(
        r#"
        SELECT e.subject_key, e.dimensions, e.vector_f32, e.model_id,
               e.model_revision, e.embedding_version, e.source_fingerprint,
               e.updated_at
        FROM derived_embeddings e
        INNER JOIN derived_index_jobs j
          ON j.index_kind = e.index_kind AND j.subject_key = e.subject_key
        WHERE e.index_kind = ?1 AND j.status = 'completed'
          AND j.model_id = e.model_id
          AND j.model_revision = e.model_revision
          AND j.embedding_version = e.embedding_version
          AND j.source_fingerprint = e.source_fingerprint
          {predicate} {suffix}
        "#
    )
}

fn visible_embedding_aggregate_sql() -> String {
    r#"
        SELECT COUNT(*), MIN(e.dimensions), MAX(e.dimensions),
               MIN(e.model_id), MAX(e.model_id),
               MIN(e.model_revision), MAX(e.model_revision),
               MIN(e.embedding_version), MAX(e.embedding_version)
        FROM derived_embeddings e
        INNER JOIN derived_index_jobs j
          ON j.index_kind = e.index_kind AND j.subject_key = e.subject_key
        WHERE e.index_kind = ?1 AND j.status = 'completed'
          AND j.model_id = e.model_id
          AND j.model_revision = e.model_revision
          AND j.embedding_version = e.embedding_version
          AND j.source_fingerprint = e.source_fingerprint
    "#
    .to_string()
}

fn decode_snapshot_metadata(
    data_epoch: i64,
    (
        row_count,
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
    if row_count == 0 {
        return Ok(DerivedIndexSnapshotMetadata {
            data_epoch,
            row_count,
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

fn map_embedding_row(
    _index_kind: DerivedIndexKind,
) -> impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<RawEmbeddingRow> {
    |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
        ))
    }
}

fn decode_embedding_row(
    index_kind: DerivedIndexKind,
    (
        subject_key,
        dimensions,
        vector_blob,
        model_id,
        model_revision,
        embedding_version,
        source_fingerprint,
        updated_at,
    ): RawEmbeddingRow,
) -> Result<DerivedEmbeddingRecord, String> {
    let dimensions = usize::try_from(dimensions)
        .map_err(|_| format!("Invalid stored embedding dimensions: {dimensions}"))?;
    let vector = decode_vector(&vector_blob, dimensions)?;
    let embedding_version = u32::try_from(embedding_version)
        .map_err(|_| format!("Invalid stored embedding version: {embedding_version}"))?;
    Ok(DerivedEmbeddingRecord {
        job: DerivedIndexJobSpec {
            index_kind,
            subject_key,
            model_id,
            model_revision,
            embedding_version,
            source_fingerprint,
        },
        vector,
        updated_at,
    })
}

fn validate_job_spec(spec: &DerivedIndexJobSpec) -> Result<(), String> {
    validate_required_text("subject_key", &spec.subject_key, MAX_SUBJECT_KEY_BYTES)?;
    validate_required_text("model_id", &spec.model_id, MAX_METADATA_BYTES)?;
    validate_required_text("model_revision", &spec.model_revision, MAX_METADATA_BYTES)?;
    validate_required_text(
        "source_fingerprint",
        &spec.source_fingerprint,
        MAX_METADATA_BYTES,
    )?;
    if spec.embedding_version == 0 {
        return Err("embedding_version must be greater than zero".to_string());
    }
    if spec.index_kind == DerivedIndexKind::SemanticText {
        let screenshot_id = spec.subject_key.parse::<i64>().map_err(|error| {
            format!("Semantic derived subject key must be a canonical screenshot id: {error}")
        })?;
        if screenshot_id <= 0 || spec.subject_key != screenshot_id.to_string() {
            return Err(
                "Semantic derived subject key must be a canonical positive screenshot id"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn validate_required_text(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!("{name} exceeds {max_bytes} bytes"));
    }
    Ok(())
}

fn validate_optional_text(name: &str, value: Option<&str>, max_bytes: usize) -> Result<(), String> {
    if let Some(value) = value {
        if value.len() > max_bytes {
            return Err(format!("{name} exceeds {max_bytes} bytes"));
        }
    }
    Ok(())
}

fn normalize_retry_timestamp(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    validate_required_text("next_retry_at", value, MAX_METADATA_BYTES)?;

    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Ok(Some(
            value
                .with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ));
    }

    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|value| Some(value.format("%Y-%m-%d %H:%M:%S").to_string()))
        .map_err(|_| "next_retry_at must be RFC3339 or UTC YYYY-MM-DD HH:MM:SS".to_string())
}

fn encode_vector(vector: &[f32]) -> Result<Vec<u8>, String> {
    if vector.is_empty() {
        return Err("Derived embedding vector must not be empty".to_string());
    }
    if vector.len() > MAX_VECTOR_DIMENSIONS {
        return Err(format!(
            "Derived embedding dimensions exceed limit: {} > {MAX_VECTOR_DIMENSIONS}",
            vector.len()
        ));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err("Derived embedding vector contains a non-finite value".to_string());
    }
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

fn decode_vector(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>, String> {
    let expected = dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| "Stored embedding byte length overflow".to_string())?;
    if bytes.len() != expected {
        return Err(format!(
            "Stored embedding length mismatch: expected {expected}, got {}",
            bytes.len()
        ));
    }
    let mut vector = Vec::with_capacity(dimensions);
    for chunk in bytes.chunks_exact(4) {
        let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !value.is_finite() {
            return Err("Stored embedding contains a non-finite value".to_string());
        }
        vector.push(value);
    }
    Ok(vector)
}

fn next_generation_id() -> Result<u64, String> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock is before UNIX epoch: {error}"))?
        .as_micros();
    u64::try_from(micros).map_err(|_| "Derived generation timestamp overflow".to_string())
}

fn new_lease_token() -> String {
    let mut bytes = [0u8; LEASE_TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn write_sidecar_header(
    writer: &mut impl Write,
    hasher: &mut Sha256,
    index_kind: DerivedIndexKind,
    generation: u64,
    snapshot: &DerivedIndexSnapshotMetadata,
) -> Result<(), String> {
    write_hashed(writer, hasher, SIDECAR_MAGIC)?;
    write_hashed(writer, hasher, &SIDECAR_FORMAT_VERSION.to_le_bytes())?;
    write_hashed(writer, hasher, &generation.to_le_bytes())?;
    write_hashed(writer, hasher, &snapshot.data_epoch.to_le_bytes())?;
    let kind = index_kind.as_str().as_bytes();
    let kind_len = u16::try_from(kind.len()).map_err(|_| "Index kind is too long".to_string())?;
    write_hashed(writer, hasher, &kind_len.to_le_bytes())?;
    write_hashed(writer, hasher, kind)?;
    let (model_id, model_revision, embedding_version) = snapshot
        .model_contract
        .as_ref()
        .map(|contract| {
            (
                contract.model_id.as_bytes(),
                contract.model_revision.as_bytes(),
                contract.embedding_version,
            )
        })
        .unwrap_or((&[], &[], 0));
    let model_id_len =
        u16::try_from(model_id.len()).map_err(|_| "Model id is too long".to_string())?;
    let model_revision_len = u16::try_from(model_revision.len())
        .map_err(|_| "Model revision is too long".to_string())?;
    write_hashed(writer, hasher, &model_id_len.to_le_bytes())?;
    write_hashed(writer, hasher, model_id)?;
    write_hashed(writer, hasher, &model_revision_len.to_le_bytes())?;
    write_hashed(writer, hasher, model_revision)?;
    write_hashed(writer, hasher, &embedding_version.to_le_bytes())?;
    write_hashed(writer, hasher, &snapshot.row_count.to_le_bytes())?;
    write_hashed(
        writer,
        hasher,
        &snapshot.dimensions.unwrap_or(0).to_le_bytes(),
    )
}

fn write_sidecar_row(
    writer: &mut impl Write,
    hasher: &mut Sha256,
    row: &DerivedEmbeddingRecord,
) -> Result<(), String> {
    let key = row.job.subject_key.as_bytes();
    let key_len = u32::try_from(key.len()).map_err(|_| "Subject key is too long".to_string())?;
    write_hashed(writer, hasher, &key_len.to_le_bytes())?;
    write_hashed(writer, hasher, key)?;
    let vector = encode_vector(&row.vector)?;
    write_hashed(writer, hasher, &vector)
}

fn write_hashed(writer: &mut impl Write, hasher: &mut Sha256, bytes: &[u8]) -> Result<(), String> {
    writer
        .write_all(bytes)
        .map_err(|error| format!("Failed to write derived index temp file: {error}"))?;
    hasher.update(bytes);
    Ok(())
}

fn verify_sidecar(path: &Path, expected_checksum: &str) -> Result<(), String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Failed to open derived index sidecar: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut header = [0u8; SIDECAR_MAGIC.len()];
    reader
        .read_exact(&mut header)
        .map_err(|error| format!("Failed to read derived index sidecar header: {error}"))?;
    if &header != SIDECAR_MAGIC {
        return Err("Derived index sidecar has an invalid header".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(header);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read derived index sidecar: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let checksum = hex::encode(hasher.finalize());
    if checksum != expected_checksum {
        return Err("Derived index sidecar checksum mismatch".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    fn job(kind: DerivedIndexKind, subject_key: &str) -> DerivedIndexJobSpec {
        DerivedIndexJobSpec {
            index_kind: kind,
            subject_key: subject_key.to_string(),
            model_id: "model-a".to_string(),
            model_revision: "revision-1".to_string(),
            embedding_version: 1,
            source_fingerprint: format!("source-{subject_key}"),
        }
    }

    fn ensure_active_subject(storage: &StorageState, spec: &DerivedIndexJobSpec) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        match spec.index_kind {
            DerivedIndexKind::SemanticText => {
                let id = spec
                    .subject_key
                    .parse::<i64>()
                    .expect("semantic subject key must be a screenshot id");
                conn.execute(
                    "INSERT OR IGNORE INTO screenshots (id, image_path, image_hash) VALUES (?1, ?2, ?3)",
                    params![id, format!("{id}.enc"), format!("semantic-hash-{id}")],
                )
                .unwrap();
            }
            DerivedIndexKind::ClipImage => {
                conn.execute(
                    "INSERT OR IGNORE INTO screenshots (image_path, image_hash) VALUES (?1, ?2)",
                    params![format!("{}.enc", spec.subject_key), spec.subject_key],
                )
                .unwrap();
            }
        }
    }

    fn queue_and_claim(storage: &StorageState, spec: &DerivedIndexJobSpec) -> String {
        ensure_active_subject(storage, spec);
        storage.upsert_derived_index_job(spec).unwrap();
        storage.mark_derived_index_job_processing(spec).unwrap()
    }

    fn claimed_write(
        storage: &StorageState,
        spec: DerivedIndexJobSpec,
        vector: Vec<f32>,
    ) -> DerivedEmbeddingWrite {
        let lease_token = queue_and_claim(storage, &spec);
        DerivedEmbeddingWrite {
            job: spec,
            lease_token,
            vector,
        }
    }

    fn commit_vector(
        storage: &StorageState,
        spec: DerivedIndexJobSpec,
        vector: Vec<f32>,
    ) -> Result<(), String> {
        storage.commit_derived_embedding(&claimed_write(storage, spec, vector))
    }

    #[test]
    fn completed_vector_and_ledger_commit_atomically() {
        let (_temp, storage) = test_storage();
        let write = claimed_write(
            &storage,
            job(DerivedIndexKind::SemanticText, "42"),
            vec![0.25, -0.5, 0.75],
        );
        storage
            .commit_derived_embedding(&write)
            .expect("commit embedding");

        let visible = storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "42")
            .expect("read embedding")
            .expect("visible embedding");
        assert_eq!(visible.vector, write.vector);
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
                .unwrap()
                .unwrap()
                .status,
            DerivedIndexJobStatus::Completed
        );
    }

    #[test]
    fn ledger_failure_rolls_back_vector_write() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "9");
        let write = claimed_write(&storage, spec, vec![1.0, 0.0]);
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER reject_completed_job BEFORE UPDATE OF status ON derived_index_jobs
                     WHEN NEW.status = 'completed' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
                )
                .unwrap();
        }
        let result = storage.commit_derived_embedding(&write);
        assert!(result.is_err());
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let count: i64 = guard
            .as_ref()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM derived_embeddings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn pending_rebuild_hides_stale_vector() {
        let (_temp, storage) = test_storage();
        let write = claimed_write(
            &storage,
            job(DerivedIndexKind::SemanticText, "17"),
            vec![0.1, 0.2],
        );
        storage.commit_derived_embedding(&write).unwrap();
        let mut changed = write.job.clone();
        changed.source_fingerprint = "changed-source".to_string();
        storage.upsert_derived_index_job(&changed).unwrap();
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "17")
            .unwrap()
            .is_none());
    }

    #[test]
    fn inactive_subject_cannot_be_requeued_after_screenshot_deletion() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "18");
        ensure_active_subject(&storage, &spec);
        storage.upsert_derived_index_job(&spec).unwrap();

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 18", [])
                .unwrap();
        }

        assert!(storage.upsert_derived_index_job(&spec).is_err());
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "18")
            .unwrap()
            .is_none());
    }

    #[test]
    fn semantic_subject_keys_must_be_canonical_ids() {
        let (_temp, storage) = test_storage();
        let canonical = job(DerivedIndexKind::SemanticText, "42");
        ensure_active_subject(&storage, &canonical);

        for alias in ["042", "+42"] {
            let aliased = job(DerivedIndexKind::SemanticText, alias);
            assert!(storage.upsert_derived_index_job(&aliased).is_err());
        }
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
            .unwrap()
            .is_none());
    }

    #[test]
    fn interrupted_processing_jobs_are_requeued_at_startup() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "53");
        let old_lease = queue_and_claim(&storage, &spec);

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            storage
                .recover_interrupted_derived_index_jobs_at_startup(guard.as_ref().unwrap())
                .unwrap();
        }

        let recovered = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "53")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status, DerivedIndexJobStatus::Pending);
        assert_eq!(recovered.error_code.as_deref(), Some("worker_interrupted"));
        let new_lease = storage.mark_derived_index_job_processing(&spec).unwrap();
        assert_ne!(new_lease, old_lease);
        assert!(storage
            .mark_derived_index_job_failed(&spec, &old_lease, "late", "stale", None)
            .is_err());
    }

    #[test]
    fn late_worker_cannot_complete_or_fail_requeued_contract() {
        let (_temp, storage) = test_storage();
        let old = job(DerivedIndexKind::SemanticText, "19");
        ensure_active_subject(&storage, &old);
        storage.upsert_derived_index_job(&old).unwrap();
        let old_lease = storage.mark_derived_index_job_processing(&old).unwrap();

        let mut current = old.clone();
        current.source_fingerprint = "new-source".to_string();
        storage.upsert_derived_index_job(&current).unwrap();

        assert!(storage
            .commit_derived_embedding(&DerivedEmbeddingWrite {
                job: old.clone(),
                lease_token: old_lease.clone(),
                vector: vec![1.0, 0.0],
            })
            .is_err());
        assert!(storage
            .mark_derived_index_job_failed(&old, &old_lease, "late", "stale worker", None)
            .is_err());

        let queued = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "19")
            .unwrap()
            .unwrap();
        assert_eq!(queued.spec.source_fingerprint, "new-source");
        assert_eq!(queued.status, DerivedIndexJobStatus::Pending);
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "19")
            .unwrap()
            .is_none());
    }

    #[test]
    fn worker_lease_prevents_duplicate_claims_and_late_terminal_updates() {
        let (_temp, storage) = test_storage();
        let completed_spec = job(DerivedIndexKind::SemanticText, "20");
        let completed_write = claimed_write(&storage, completed_spec.clone(), vec![1.0, 0.0]);
        assert!(storage
            .mark_derived_index_job_processing(&completed_spec)
            .is_err());
        storage.commit_derived_embedding(&completed_write).unwrap();
        assert!(storage
            .mark_derived_index_job_failed(
                &completed_spec,
                &completed_write.lease_token,
                "late_failure",
                "a slower worker failed after completion",
                None,
            )
            .is_err());
        assert!(storage
            .mark_derived_index_job_discarded(
                &completed_spec,
                &completed_write.lease_token,
                "late_discard",
                "a cancellation arrived after completion",
            )
            .is_err());
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "20")
                .unwrap()
                .unwrap()
                .status,
            DerivedIndexJobStatus::Completed
        );

        let discarded_spec = job(DerivedIndexKind::SemanticText, "21");
        let discarded_write = claimed_write(&storage, discarded_spec.clone(), vec![0.0, 1.0]);
        storage
            .mark_derived_index_job_discarded(
                &discarded_spec,
                &discarded_write.lease_token,
                "cancelled",
                "discarded while inference was running",
            )
            .unwrap();
        assert!(storage.commit_derived_embedding(&discarded_write).is_err());
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "21")
                .unwrap()
                .unwrap()
                .status,
            DerivedIndexJobStatus::Discarded
        );
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "21")
            .unwrap()
            .is_none());
    }

    #[test]
    fn duplicate_clip_subject_replaces_one_image_hash_row() {
        let (_temp, storage) = test_storage();
        let first = claimed_write(
            &storage,
            job(DerivedIndexKind::ClipImage, "same-image-hash"),
            vec![1.0, 0.0],
        );
        storage.commit_derived_embedding(&first).unwrap();
        let second = claimed_write(&storage, first.job.clone(), vec![0.0, 1.0]);
        storage.commit_derived_embedding(&second).unwrap();
        let rows = storage
            .list_query_visible_embeddings(DerivedIndexKind::ClipImage, 0, 100)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].vector, second.vector);
    }

    #[test]
    fn deletion_removes_vector_and_ledger_together() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "81"),
            vec![0.3, 0.4],
        )
        .unwrap();
        assert!(storage
            .delete_derived_index_subject(DerivedIndexKind::SemanticText, "81")
            .unwrap());
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "81")
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "81")
            .unwrap()
            .is_none());
    }

    #[test]
    fn screenshot_lifecycle_removes_text_and_image_derived_rows() {
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (42, '42.enc', 'hash-42')",
                    [],
                )
                .unwrap();
        }
        for (spec, vector) in [
            (job(DerivedIndexKind::SemanticText, "42"), vec![1.0, 0.0]),
            (job(DerivedIndexKind::ClipImage, "hash-42"), vec![0.0, 1.0]),
        ] {
            commit_vector(&storage, spec, vector).unwrap();
        }
        storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();
        storage
            .publish_derived_index_generation(DerivedIndexKind::ClipImage)
            .unwrap();

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 42", [])
                .unwrap();
        }
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "42")
            .unwrap()
            .is_none());
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::ClipImage, "hash-42")
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "42")
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::ClipImage, "hash-42")
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap()
            .is_none());
        assert!(storage
            .get_derived_index_generation(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());
    }

    #[test]
    fn late_workers_cannot_resurrect_soft_deleted_screenshot_subjects() {
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (91, '91.enc', 'hash-91')",
                    [],
                )
                .unwrap();
        }
        let writes = [
            claimed_write(
                &storage,
                job(DerivedIndexKind::SemanticText, "91"),
                vec![1.0, 0.0],
            ),
            claimed_write(
                &storage,
                job(DerivedIndexKind::ClipImage, "hash-91"),
                vec![0.0, 1.0],
            ),
        ];

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 91", [])
                .unwrap();
        }

        for write in writes {
            assert!(storage.commit_derived_embedding(&write).is_err());
            assert!(storage
                .get_query_visible_embedding(write.job.index_kind, &write.job.subject_key)
                .unwrap()
                .is_none());
            assert!(storage
                .get_derived_index_job(write.job.index_kind, &write.job.subject_key)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn model_invalidation_deletes_stale_vector_and_requeues_job() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "23"),
            vec![0.3, 0.4],
        )
        .unwrap();
        assert_eq!(
            storage
                .invalidate_derived_index_model(
                    DerivedIndexKind::SemanticText,
                    "model-a",
                    "revision-2",
                    2,
                )
                .unwrap(),
            1
        );
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "23")
            .unwrap()
            .is_none());
        let queued = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "23")
            .unwrap()
            .unwrap();
        assert_eq!(queued.status, DerivedIndexJobStatus::Pending);
        assert_eq!(queued.spec.model_revision, "revision-2");
        assert_eq!(queued.spec.embedding_version, 2);
    }

    #[test]
    fn model_invalidation_does_not_resurrect_discarded_jobs() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "24");
        let lease_token = queue_and_claim(&storage, &spec);
        storage
            .mark_derived_index_job_discarded(
                &spec,
                &lease_token,
                "cancelled",
                "explicitly discarded",
            )
            .unwrap();

        assert_eq!(
            storage
                .invalidate_derived_index_model(
                    DerivedIndexKind::SemanticText,
                    "model-a",
                    "revision-2",
                    2,
                )
                .unwrap(),
            0
        );
        let discarded = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "24")
            .unwrap()
            .unwrap();
        assert_eq!(discarded.status, DerivedIndexJobStatus::Discarded);
        assert_eq!(discarded.spec.model_revision, "revision-1");
        assert_eq!(discarded.spec.embedding_version, 1);
    }

    #[test]
    fn failure_attempts_increment_and_auth_wait_does_not_consume_budget() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "51");
        let auth_lease = queue_and_claim(&storage, &spec);
        storage
            .mark_derived_index_job_waiting_for_auth(&spec, &auth_lease, Some("locked"))
            .unwrap();
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "51")
                .unwrap()
                .unwrap()
                .attempts,
            0
        );
        storage.upsert_derived_index_job(&spec).unwrap();
        let failure_lease = storage.mark_derived_index_job_processing(&spec).unwrap();
        storage
            .mark_derived_index_job_failed(&spec, &failure_lease, "inference", "failed", None)
            .unwrap();
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "51")
                .unwrap()
                .unwrap()
                .attempts,
            1
        );

        let failed = storage
            .list_derived_index_jobs(
                DerivedIndexKind::SemanticText,
                Some(DerivedIndexJobStatus::Failed),
                0,
                10,
            )
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].spec.subject_key, "51");
    }

    #[test]
    fn retry_backoff_blocks_early_worker_claims() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "52");
        let lease_token = queue_and_claim(&storage, &spec);
        storage
            .mark_derived_index_job_failed(
                &spec,
                &lease_token,
                "inference",
                "temporary failure",
                Some("9999-12-31 23:59:59"),
            )
            .unwrap();

        assert!(storage.mark_derived_index_job_processing(&spec).is_err());
    }

    #[test]
    fn retry_timestamps_are_normalized_to_sqlite_utc() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "54");
        let lease_token = queue_and_claim(&storage, &spec);
        storage
            .mark_derived_index_job_failed(
                &spec,
                &lease_token,
                "inference",
                "temporary failure",
                Some("2000-01-01T08:00:00+08:00"),
            )
            .unwrap();

        let failed = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "54")
            .unwrap()
            .unwrap();
        assert_eq!(failed.next_retry_at.as_deref(), Some("2000-01-01 00:00:00"));
        storage.mark_derived_index_job_processing(&spec).unwrap();
    }

    #[test]
    fn invalid_retry_timestamp_does_not_mutate_processing_job() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "55");
        let lease_token = queue_and_claim(&storage, &spec);
        assert!(storage
            .mark_derived_index_job_failed(
                &spec,
                &lease_token,
                "inference",
                "temporary failure",
                Some("tomorrow"),
            )
            .is_err());

        let processing = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "55")
            .unwrap()
            .unwrap();
        assert_eq!(processing.status, DerivedIndexJobStatus::Processing);
        assert_eq!(processing.attempts, 0);
    }

    #[test]
    fn generation_is_checksummed_and_published_after_completed_rows_only() {
        let (temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let pending = job(DerivedIndexKind::SemanticText, "2");
        ensure_active_subject(&storage, &pending);
        storage.upsert_derived_index_job(&pending).unwrap();

        let generation = storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .expect("publish generation");
        assert_eq!(generation.row_count, 1);
        assert_eq!(generation.dimensions, Some(2));
        assert_eq!(generation.model_id.as_deref(), Some("model-a"));
        assert_eq!(generation.model_revision.as_deref(), Some("revision-1"));
        assert_eq!(generation.embedding_version, Some(1));
        let path = temp
            .path()
            .join("derived-indexes")
            .join(&generation.file_name);
        verify_sidecar(&path, &generation.checksum_sha256).unwrap();
        assert_eq!(
            storage
                .get_derived_index_generation(DerivedIndexKind::SemanticText)
                .unwrap()
                .unwrap(),
            generation
        );
    }

    #[test]
    fn publishing_retains_replaced_sidecar_until_safe_startup_cleanup() {
        let (temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let first = storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();
        let first_path = temp.path().join("derived-indexes").join(&first.file_name);
        assert!(first_path.exists());

        let second = storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();
        let second_path = temp.path().join("derived-indexes").join(&second.file_name);
        assert_ne!(first.file_name, second.file_name);
        assert!(first_path.exists());
        assert!(second_path.exists());

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            storage
                .cleanup_derived_index_sidecars_at_startup(guard.as_ref().unwrap(), temp.path())
                .unwrap();
        }
        assert!(!first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn startup_cleanup_does_not_fail_when_sidecar_cache_path_is_not_a_directory() {
        let (temp, storage) = test_storage();
        let sidecar_path = temp.path().join("derived-indexes");
        std::fs::write(&sidecar_path, b"not a directory").unwrap();

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            storage
                .cleanup_derived_index_sidecars_at_startup(guard.as_ref().unwrap(), temp.path())
                .unwrap();
        }

        assert!(sidecar_path.is_file());
    }

    #[test]
    fn derived_mutation_invalidates_published_generation() {
        let (_temp, storage) = test_storage();
        let write = claimed_write(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        );
        storage.commit_derived_embedding(&write).unwrap();
        storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();
        assert!(storage
            .get_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap()
            .is_some());

        let mut changed = write.job.clone();
        changed.source_fingerprint = "changed-source".to_string();
        storage.upsert_derived_index_job(&changed).unwrap();

        assert!(storage
            .get_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap()
            .is_none());
    }

    #[test]
    fn non_visible_job_churn_and_unindexed_deletion_preserve_generation() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let pending = job(DerivedIndexKind::SemanticText, "2");
        ensure_active_subject(&storage, &pending);
        storage.upsert_derived_index_job(&pending).unwrap();
        let generation = storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();

        let lease_token = storage.mark_derived_index_job_processing(&pending).unwrap();
        storage
            .mark_derived_index_job_failed(
                &pending,
                &lease_token,
                "inference",
                "temporary failure",
                None,
            )
            .unwrap();
        assert_eq!(
            storage
                .get_derived_index_generation(DerivedIndexKind::SemanticText)
                .unwrap(),
            Some(generation.clone())
        );

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().unwrap();
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash) VALUES (3, '3.enc', 'hash-3')",
                [],
            )
            .unwrap();
            conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 3", [])
                .unwrap();
        }
        assert_eq!(
            storage
                .get_derived_index_generation(DerivedIndexKind::SemanticText)
                .unwrap(),
            Some(generation)
        );
    }

    #[test]
    fn stale_snapshot_cannot_be_recorded_as_current_generation() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let snapshot = storage
            .get_derived_index_snapshot_metadata(DerivedIndexKind::SemanticText)
            .unwrap();

        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "2"),
            vec![0.0, 1.0],
        )
        .unwrap();

        let stale = DerivedIndexGeneration {
            index_kind: DerivedIndexKind::SemanticText,
            generation: 1,
            data_epoch: snapshot.data_epoch,
            file_name: "stale.cpdvec".to_string(),
            checksum_sha256: "00".repeat(32),
            row_count: snapshot.row_count,
            dimensions: snapshot.dimensions,
            model_id: snapshot
                .model_contract
                .as_ref()
                .map(|contract| contract.model_id.clone()),
            model_revision: snapshot
                .model_contract
                .as_ref()
                .map(|contract| contract.model_revision.clone()),
            embedding_version: snapshot
                .model_contract
                .as_ref()
                .map(|contract| contract.embedding_version),
        };
        assert!(storage.record_derived_index_generation(&stale).is_err());
    }

    #[test]
    fn generation_streams_across_multiple_database_pages() {
        let (_temp, storage) = test_storage();
        let row_count = SIDECAR_PAGE_SIZE + 17;
        for id in 0..row_count {
            commit_vector(
                &storage,
                job(DerivedIndexKind::SemanticText, &(id + 1).to_string()),
                vec![id as f32, 1.0],
            )
            .unwrap();
        }

        let generation = storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap();
        assert_eq!(generation.row_count, u64::from(row_count));
        assert_eq!(generation.dimensions, Some(2));
        assert!(storage
            .get_derived_index_generation(DerivedIndexKind::SemanticText)
            .unwrap()
            .is_some());
    }

    #[test]
    fn rejects_non_finite_and_mixed_dimension_generations() {
        let (_temp, storage) = test_storage();
        let invalid = storage.commit_derived_embedding(&claimed_write(
            &storage,
            job(DerivedIndexKind::SemanticText, "90"),
            vec![f32::NAN],
        ));
        assert!(invalid.is_err());

        for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![1.0, 0.0, 0.0])] {
            commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
        }
        assert!(storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .is_err());
    }

    #[test]
    fn rejects_mixed_model_contract_generations() {
        let (_temp, storage) = test_storage();
        let first = job(DerivedIndexKind::SemanticText, "1");
        let mut second = job(DerivedIndexKind::SemanticText, "2");
        second.model_revision = "revision-2".to_string();
        for spec in [first, second] {
            commit_vector(&storage, spec, vec![1.0, 0.0]).unwrap();
        }
        assert!(storage
            .publish_derived_index_generation(DerivedIndexKind::SemanticText)
            .is_err());
    }
}
