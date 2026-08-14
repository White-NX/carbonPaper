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
pub(super) const MAX_METADATA_BYTES: usize = 4096;
const MAX_VECTOR_DIMENSIONS: usize = 65_536;
const SIDECAR_PAGE_SIZE: u32 = 512;
const LEASE_TOKEN_BYTES: usize = 16;
pub const DERIVED_GENERATION_CANCELLED: &str = "DERIVED_GENERATION_CANCELLED";

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

    pub fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "semantic_text" => Ok(Self::SemanticText),
            "clip_image" => Ok(Self::ClipImage),
            other => Err(format!("Unknown derived index kind: {other}")),
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

/// One scored subject from an exact-scan semantic retrieval. `score` is cosine
/// similarity (dot product of L2-normalized vectors).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredSubject {
    pub subject_key: String,
    pub score: f32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnsureDerivedIndexJobResult {
    Queued,
    Requeued,
    AlreadyCurrent,
    AlreadyProcessing,
}

/// Ledger depth for one index kind.
///
/// Under idle gating a non-zero `claimable` is ordinary operation — captures
/// waiting for the next idle window — so the read path reports it instead of
/// acting on it. `exhausted` is the number that cannot clear itself: those
/// subjects stay missing from search until someone intervenes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedIndexBacklog {
    pub claimable: u64,
    pub exhausted: u64,
    /// Age of the oldest claimable row by ledger update time. `None` when the
    /// queue is empty.
    pub oldest_claimable_age_secs: Option<i64>,
}

/// What a full CLIP backfill would have to encode.
///
/// Two numbers rather than one because the cost is not per row: a CLIP encode
/// is dominated by decode and resize, so a 4K screenshot costs roughly six
/// times a 720p one. Reporting the megapixel sum alongside the count is what
/// lets a caller turn a backlog into a duration honestly instead of multiplying
/// by an average nobody measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipBackfillWork {
    pub images: u64,
    pub megapixels: f64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedAnnGeneration {
    pub index_kind: DerivedIndexKind,
    pub generation: u64,
    pub covered_epoch: u64,
    pub flat_file_name: String,
    pub flat_checksum_sha256: String,
    pub ann_file_name: String,
    pub ann_checksum_sha256: String,
    pub row_count: u64,
    pub dimensions: u32,
    pub model_id: String,
    pub model_revision: String,
    pub embedding_version: u32,
    pub sidecar_format_version: u32,
    pub ann_format_version: u32,
    pub algorithm: String,
    pub implementation_version: String,
    pub metric: String,
    pub quantization: String,
    pub connectivity: u32,
    pub expansion_add: u32,
    pub expansion_search: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedAnnBuildState {
    pub index_kind: DerivedIndexKind,
    pub consecutive_failures: u32,
    pub last_failure_at: String,
    pub next_retry_at: String,
    pub last_error_code: String,
    pub last_error: String,
    pub circuit_open: bool,
    /// True once the UI has atomically consumed or acknowledged the Toast.
    /// A pending startup Toast is deliberately reported as false.
    pub notification_sent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedAnnBuildFailureUpdate {
    pub state: DerivedAnnBuildState,
    pub should_notify: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DerivedAnnTailRow {
    pub subject_key: String,
    pub vector: Option<Vec<f32>>,
}

/// Column-minimal row used while freezing an ANN base. `vector_f32` is already
/// the little-endian byte representation required by CPDVEC04, so the
/// bootstrap can copy it directly instead of decoding and re-encoding every
/// scalar in a large corpus.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DerivedAnnSnapshotRow {
    pub subject_key: String,
    pub dimensions: u32,
    pub vector_f32: Vec<u8>,
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

struct DerivedWorkerJobUpdate<'a> {
    status: DerivedIndexJobStatus,
    error_code: Option<&'a str>,
    error: Option<&'a str>,
    next_retry_at: Option<&'a str>,
    increment_attempts: bool,
}

impl StorageState {
    /// Ensure one subject has a ledger entry without reviving a current result.
    /// A changed model/source contract queues fresh work; a matching completed
    /// result is always reused so migration cannot accidentally recompute it.
    pub fn ensure_derived_index_job(
        &self,
        spec: &DerivedIndexJobSpec,
    ) -> Result<EnsureDerivedIndexJobResult, String> {
        validate_job_spec(spec)?;
        let mut guard = self.get_connection_named("ensure_derived_index_job")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived job transaction: {error}"))?;
        if !derived_subject_is_active(&tx, spec.index_kind, &spec.subject_key)? {
            return Err("Cannot queue a derived index job for an inactive subject".to_string());
        }

        let existing = tx
            .query_row(
                r#"
                SELECT status, model_id, model_revision, embedding_version,
                       source_fingerprint
                FROM derived_index_jobs
                WHERE index_kind = ?1 AND subject_key = ?2
                "#,
                params![spec.index_kind.as_str(), spec.subject_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Failed to inspect derived index job: {error}"))?;

        let result = match existing {
            None => {
                tx.execute(
                    r#"
                    INSERT INTO derived_index_jobs (
                        index_kind, subject_key, status, attempts, model_id,
                        model_revision, embedding_version, source_fingerprint, updated_at
                    ) VALUES (?1, ?2, 'pending', 0, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
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
                .map_err(|error| format!("Failed to create derived index job: {error}"))?;
                EnsureDerivedIndexJobResult::Queued
            }
            Some((status, model_id, model_revision, embedding_version, source_fingerprint)) => {
                let contract_matches = model_id == spec.model_id
                    && model_revision == spec.model_revision
                    && embedding_version == i64::from(spec.embedding_version)
                    && source_fingerprint == spec.source_fingerprint;
                if contract_matches {
                    match DerivedIndexJobStatus::from_db(&status)? {
                        DerivedIndexJobStatus::Completed => {
                            EnsureDerivedIndexJobResult::AlreadyCurrent
                        }
                        DerivedIndexJobStatus::Processing => {
                            EnsureDerivedIndexJobResult::AlreadyProcessing
                        }
                        DerivedIndexJobStatus::Pending => EnsureDerivedIndexJobResult::Queued,
                        DerivedIndexJobStatus::Failed
                        | DerivedIndexJobStatus::WaitingForAuth
                        | DerivedIndexJobStatus::Discarded => {
                            tx.execute(
                                r#"
                                UPDATE derived_index_jobs
                                SET status = 'pending', error_code = NULL, error = NULL,
                                    next_retry_at = NULL, lease_token = NULL,
                                    updated_at = CURRENT_TIMESTAMP
                                WHERE index_kind = ?1 AND subject_key = ?2
                                "#,
                                params![spec.index_kind.as_str(), spec.subject_key],
                            )
                            .map_err(|error| {
                                format!("Failed to resume derived index job: {error}")
                            })?;
                            EnsureDerivedIndexJobResult::Requeued
                        }
                    }
                } else {
                    tx.execute(
                        r#"
                        UPDATE derived_index_jobs
                        SET status = 'pending', error_code = NULL, error = NULL,
                            attempts = 0,
                            next_retry_at = NULL, lease_token = NULL,
                            model_id = ?3, model_revision = ?4,
                            embedding_version = ?5, source_fingerprint = ?6,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE index_kind = ?1 AND subject_key = ?2
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
                    .map_err(|error| format!("Failed to requeue derived index job: {error}"))?;
                    EnsureDerivedIndexJobResult::Requeued
                }
            }
        };
        tx.commit()
            .map_err(|error| format!("Failed to commit derived job transaction: {error}"))?;
        Ok(result)
    }

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

    /// Release a processing lease without charging the retry budget. This is
    /// used by explicit cancellation so the next run resumes.
    pub fn requeue_derived_index_job(
        &self,
        spec: &DerivedIndexJobSpec,
        lease_token: &str,
        reason_code: &str,
        reason: &str,
    ) -> Result<(), String> {
        self.set_derived_worker_job_state(
            spec,
            lease_token,
            DerivedWorkerJobUpdate {
                status: DerivedIndexJobStatus::Pending,
                error_code: Some(reason_code),
                error: Some(reason),
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
        // Sampled before the write so the resident cache can tell "I was
        // current and this is my delta" from "I already missed something".
        let epoch_before = Some(read_derived_data_epoch(conn, write.job.index_kind)?);
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
        if let Some(epoch_before) = epoch_before {
            // The triggers in this transaction advanced the epoch. Read it back
            // on the connection already held and fold the row into the resident
            // matrix, so continuous dual-write capture does not invalidate the
            // whole cache every few seconds.
            let epoch_after = read_derived_data_epoch(conn, write.job.index_kind)?;
            self.note_semantic_cache_write(
                write.job.index_kind,
                &write.job.subject_key,
                &write.vector,
                epoch_before,
                epoch_after,
            );
        }
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

    /// Fetch the query-visible vectors for a set of subjects in one statement.
    ///
    /// The Smart Cluster prefilter needs a whole peeked batch of vectors at
    /// once, and calling [`Self::get_query_visible_embedding`] per subject would
    /// take and release the process-wide database mutex once per screenshot
    /// while a foreground query waits behind it.
    ///
    /// Subjects with no visible row are simply absent from the map. That is the
    /// normal case, not an error: a screenshot whose semantic vector has not
    /// been encoded yet is queued in the ledger and will be scored on a later
    /// pass, which is exactly what the caller does with a missing entry.
    pub fn get_query_visible_embeddings_by_subjects(
        &self,
        index_kind: DerivedIndexKind,
        subject_keys: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<f32>>, String> {
        let mut out = std::collections::HashMap::new();
        if subject_keys.is_empty() {
            return Ok(out);
        }
        for subject_key in subject_keys {
            validate_required_text("subject_key", subject_key, MAX_SUBJECT_KEY_BYTES)?;
        }
        let guard = self.get_connection_named("get_query_visible_embeddings_by_subjects")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        // Chunked to stay under SQLite's default 999-parameter ceiling, with
        // one slot reserved for `index_kind`.
        for chunk in subject_keys.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = visible_embedding_sql(&format!("AND e.subject_key IN ({placeholders})"), "");
            let mut statement = conn
                .prepare(&sql)
                .map_err(|error| format!("Failed to prepare derived embedding batch: {error}"))?;
            let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 1);
            let kind = index_kind.as_str();
            params.push(&kind);
            for subject_key in chunk {
                params.push(subject_key);
            }
            let rows = statement
                .query_map(params.as_slice(), map_embedding_row(index_kind))
                .map_err(|error| format!("Failed to query derived embeddings: {error}"))?;
            for row in rows {
                let row =
                    row.map_err(|error| format!("Failed to read derived embedding row: {error}"))?;
                let record = decode_embedding_row(index_kind, row)?;
                out.insert(record.job.subject_key.clone(), record.vector);
            }
        }
        Ok(out)
    }

    /// Count query-visible embeddings for one index kind. Reported for
    /// `semantic_text` in the Settings → Advanced backend diagnostic, where it
    /// is the local half of "how much of the corpus can Rust actually rank".
    pub fn count_query_visible_embeddings(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<u64, String> {
        let guard = self.get_connection_named("count_query_visible_embeddings")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let count: i64 = conn
            .query_row(
                &visible_embedding_count_sql(),
                [index_kind.as_str()],
                |row| row.get(0),
            )
            .map_err(|error| format!("Failed to count query-visible embeddings: {error}"))?;
        u64::try_from(count).map_err(|_| "Invalid query-visible embedding count".to_string())
    }

    /// Whether at least one embedding is query-visible for an index kind.
    ///
    /// Capability checks only need existence, not an exact corpus size. Keep
    /// this separate from [`Self::count_query_visible_embeddings`] so callers
    /// do not pay for an unbounded aggregate when a single matching row is
    /// enough to answer the question.
    pub fn has_query_visible_embeddings(
        &self,
        index_kind: DerivedIndexKind,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("has_query_visible_embeddings")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &visible_embedding_exists_sql(),
            [index_kind.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(|error| format!("Failed to check query-visible embeddings: {error}"))
    }

    /// Exact-scan cosine top-K over the query-visible `semantic_text`
    /// embeddings. Stored and query vectors are both L2-normalized, so cosine
    /// similarity equals the dot product. This is the production semantic read
    /// path (`semantic_query.rs`): SQLite remains authoritative and the
    /// `.cpdvec` ANN sidecar is not consulted until a performance gate requires
    /// it. The scan is bounded by the ~30-day MiniLM hot layer that M2.4
    /// migrated.
    ///
    /// Scoring runs against the resident matrix rather than re-reading the
    /// store per query — see `semantic_cache` for the freshness and lifetime
    /// rules. The result is identical to a fresh SQL scan; only the summation
    /// order of the dot product differs, which can reorder exact ties.
    pub fn semantic_text_topk(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredSubject>, String> {
        if query.is_empty() {
            return Err("Semantic query vector must not be empty".to_string());
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        self.semantic_topk_resident(DerivedIndexKind::SemanticText, query, k)
    }

    /// Cosine top-K over the migrated Chinese-CLIP image vectors.
    ///
    /// The image-side counterpart of [`Self::semantic_text_topk`], and the same
    /// exact scan over the same resident-matrix machinery. What differs is only
    /// the subject: a key here is an `image_hash`, so the caller resolves rows
    /// to screenshots rather than parsing the key as one.
    pub fn clip_image_topk(&self, query: &[f32], k: usize) -> Result<Vec<ScoredSubject>, String> {
        if query.is_empty() {
            return Err("CLIP query vector must not be empty".to_string());
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        self.semantic_topk_resident(DerivedIndexKind::ClipImage, query, k)
    }

    pub(crate) fn clip_image_topk_with_deadline(
        &self,
        query: &[f32],
        k: usize,
        deadline: std::time::Instant,
    ) -> Result<Vec<ScoredSubject>, String> {
        if query.is_empty() {
            return Err("CLIP query vector must not be empty".to_string());
        }
        if k == 0 {
            return Ok(Vec::new());
        }
        self.semantic_topk_resident_with_deadline(
            DerivedIndexKind::ClipImage,
            query,
            k,
            Some(deadline),
        )
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
            .prepare(&format!(
                r#"
                SELECT {JOB_ROW_COLUMNS}
                FROM derived_index_jobs
                WHERE index_kind = ?1 AND (?2 IS NULL OR status = ?2)
                ORDER BY updated_at, subject_key
                LIMIT ?3 OFFSET ?4
                "#
            ))
            .map_err(|error| format!("Failed to prepare derived job query: {error}"))?;
        let rows = statement
            .query_map(
                params![index_kind.as_str(), status_text, limit, offset],
                read_job_row,
            )
            .map_err(|error| format!("Failed to query derived jobs: {error}"))?;
        rows.map(|row| {
            let row =
                row.map_err(|db_error| format!("Failed to read derived job row: {db_error}"))?;
            job_record_from_row(index_kind, row)
        })
        .collect()
    }

    /// Ledger rows a worker may claim right now: never started, parked on a
    /// locked session, or failed with the backoff elapsed and retry budget
    /// intact. Deliberately narrower than [`Self::list_derived_index_jobs`],
    /// which reports every row whether or not anything can be done with it.
    ///
    /// Oldest first, so a backlog drains in capture order rather than starving
    /// whatever was queued while the machine was busy.
    pub fn claimable_derived_index_jobs(
        &self,
        index_kind: DerivedIndexKind,
        max_attempts: u32,
        limit: u32,
    ) -> Result<Vec<DerivedIndexJobRecord>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("claimable_derived_index_jobs")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(&format!(
                r#"
                SELECT {JOB_ROW_COLUMNS}
                FROM derived_index_jobs
                WHERE index_kind = ?1
                  AND {CLAIMABLE_JOB_PREDICATE}
                  AND (next_retry_at IS NULL OR next_retry_at <= CURRENT_TIMESTAMP)
                ORDER BY updated_at, subject_key
                LIMIT ?3
                "#
            ))
            .map_err(|error| format!("Failed to prepare claimable job query: {error}"))?;
        let rows = statement
            .query_map(
                params![index_kind.as_str(), max_attempts, limit],
                read_job_row,
            )
            .map_err(|error| format!("Failed to query claimable derived jobs: {error}"))?;
        rows.map(|row| {
            let row =
                row.map_err(|db_error| format!("Failed to read claimable job row: {db_error}"))?;
            job_record_from_row(index_kind, row)
        })
        .collect()
    }

    /// Ledger depth, for the read path's backend diagnostic.
    ///
    /// `claimable` is ordinary operation under idle gating and is reported, not
    /// acted on. `exhausted` is the number that cannot clear itself: a job whose
    /// retry budget is spent stays missing from search until someone looks.
    pub fn derived_index_backlog(
        &self,
        index_kind: DerivedIndexKind,
        max_attempts: u32,
    ) -> Result<DerivedIndexBacklog, String> {
        let guard = self.get_connection_named("derived_index_backlog")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            &format!(
                r#"
                SELECT
                    COALESCE(SUM(CASE WHEN {CLAIMABLE_JOB_PREDICATE} THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' AND attempts >= ?2
                                      THEN 1 ELSE 0 END), 0),
                    CAST((julianday('now') - julianday(
                        MIN(CASE WHEN {CLAIMABLE_JOB_PREDICATE} THEN updated_at END)
                    )) * 86400 AS INTEGER)
                FROM derived_index_jobs
                WHERE index_kind = ?1
                "#
            ),
            params![index_kind.as_str(), max_attempts],
            |row| {
                Ok(DerivedIndexBacklog {
                    claimable: row.get::<_, i64>(0)?.max(0) as u64,
                    exhausted: row.get::<_, i64>(1)?.max(0) as u64,
                    oldest_claimable_age_secs: row.get::<_, Option<i64>>(2)?,
                })
            },
        )
        .map_err(|error| format!("Failed to read derived index backlog: {error}"))
    }

    /// Screenshots that should have a `semantic_text` vector but have no ledger
    /// row at all, newest first.
    ///
    /// This is the repair path for an enqueue that never happened — the capture
    /// ran while the session was locked, or the process died between the OCR
    /// commit and the enqueue. It deliberately does not look for rows whose
    /// *fingerprint* went stale, because deciding that requires decrypting and
    /// rebuilding the source text of every screenshot; a model or contract
    /// change is handled by [`Self::invalidate_derived_index_model`] instead.
    ///
    /// The `EXISTS` clause is an over-approximation of corpus membership, and
    /// knowingly so: whether a screenshot has any text to encode depends on the
    /// contents of `ocr_results.text_enc`, `screenshots.process_name_enc`, and
    /// `screenshots.window_title_enc`, and an empty string is indistinguishable
    /// from a full one in ciphertext. `minilm_sources` applies the real rule
    /// after decryption and records what it rules out through
    /// [`Self::exclude_derived_index_subject`], which is what keeps a screenshot
    /// with nothing to encode from being handed back here on every pass.
    ///
    /// `retention_modifier` is a SQLite datetime modifier such as `-30 days`,
    /// bound as a parameter. Screenshots older than it are outside the window
    /// this index keeps at all, so they are not candidates.
    pub fn list_semantic_text_index_candidates(
        &self,
        retention_modifier: &str,
        limit: u32,
    ) -> Result<Vec<i64>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("list_semantic_text_index_candidates")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT s.id
                FROM screenshots s
                WHERE s.is_deleted = 0
                  AND s.created_at >= datetime('now', ?1)
                  AND EXISTS (
                      SELECT 1 FROM ocr_results o
                      WHERE o.screenshot_id = s.id AND o.is_deleted = 0
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM derived_index_jobs j
                      WHERE j.index_kind = 'semantic_text'
                        AND j.subject_key = CAST(s.id AS TEXT)
                  )
                ORDER BY s.created_at DESC, s.id DESC
                LIMIT ?2
                "#,
            )
            .map_err(|error| format!("Failed to prepare index candidate query: {error}"))?;
        let rows = statement
            .query_map(params![retention_modifier, limit], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| format!("Failed to query index candidates: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<i64>>>()
            .map_err(|error| format!("Failed to read index candidate row: {error}"))
    }

    /// Subjects the semantic index should no longer hold: aged out of the
    /// retention window, or with no live screenshot behind them.
    ///
    /// Ageing is the part that needs a query. Deletion is already handled
    /// transactionally by `cleanup_derived_index_on_screenshot_soft_delete` and
    /// its hard-delete twin, and [`Self::ensure_derived_index_job`] refuses to
    /// queue work for a subject that is not live, so the `s.id IS NULL` branch
    /// cannot be produced through the normal APIs. It is kept because a row that
    /// arrived some other way — a database written before those triggers — has
    /// no `created_at` to age against and would otherwise never be reclaimed.
    pub fn list_expired_semantic_text_subjects(
        &self,
        retention_modifier: &str,
        limit: u32,
    ) -> Result<Vec<String>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("list_expired_semantic_text_subjects")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT t.subject_key
                FROM (
                    SELECT subject_key FROM derived_index_jobs
                    WHERE index_kind = 'semantic_text'
                    UNION
                    SELECT subject_key FROM derived_embeddings
                    WHERE index_kind = 'semantic_text'
                ) t
                LEFT JOIN screenshots s
                    ON s.id = CAST(t.subject_key AS INTEGER) AND s.is_deleted = 0
                WHERE s.id IS NULL OR s.created_at < datetime('now', ?1)
                LIMIT ?2
                "#,
            )
            .map_err(|error| format!("Failed to prepare expired subject query: {error}"))?;
        let rows = statement
            .query_map(params![retention_modifier, limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("Failed to query expired subjects: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .map_err(|error| format!("Failed to read expired subject row: {error}"))
    }

    /// Image hashes the CLIP index should hold but has no ledger row for.
    ///
    /// `created_after` is an optional SQLite datetime modifier (`-7 days`) to bound the scan.
    pub fn list_clip_image_index_candidates(
        &self,
        created_after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("list_clip_image_index_candidates")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT s.image_hash
                FROM screenshots s
                WHERE s.is_deleted = 0
                  AND (?1 IS NULL OR s.created_at >= datetime('now', ?1))
                  AND EXISTS (
                      SELECT 1 FROM ocr_results o
                      WHERE o.screenshot_id = s.id AND o.is_deleted = 0
                  )
                  AND NOT EXISTS (
                      SELECT 1 FROM derived_index_jobs j
                      WHERE j.index_kind = 'clip_image'
                        AND j.subject_key = s.image_hash
                  )
                ORDER BY s.created_at DESC, s.id DESC
                LIMIT ?2
                "#,
            )
            .map_err(|error| format!("Failed to prepare CLIP candidate query: {error}"))?;
        let rows = statement
            .query_map(params![created_after, limit], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to query CLIP candidates: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .map_err(|error| format!("Failed to read CLIP candidate row: {error}"))
    }

    /// How much work a full CLIP backfill would be, over the whole history.
    ///
    /// The same population [`Self::list_clip_image_index_candidates`] returns
    /// with no age bound, counted rather than listed, plus the one property that
    /// decides what encoding it costs. Measured 2026-08-04, a CLIP encode is
    /// dominated by decode and resize and is therefore very nearly linear in the
    /// *source* pixel count rather than constant at the 224² the model sees — so
    /// the megapixel sum, not the row count, is what turns a backlog into a
    /// duration. The caller owns the coefficients; storage has no business
    /// knowing how fast a vision transformer runs.
    ///
    /// `width` and `height` are nullable, and a row that predates them is
    /// counted at `assumed_megapixels` rather than at zero: guessing 1080p is
    /// wrong by at most a factor of four, while treating an unknown screenshot
    /// as free would understate a whole-history estimate by however many old
    /// rows the database has.
    pub fn clip_image_backfill_work(
        &self,
        assumed_megapixels: f64,
    ) -> Result<ClipBackfillWork, String> {
        let guard = self.get_connection_named("clip_image_backfill_work")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row(
            r#"
            SELECT COUNT(*), COALESCE(SUM(
                CASE WHEN s.width > 0 AND s.height > 0
                     THEN (s.width * 1.0 * s.height) / 1000000.0
                     ELSE ?1 END
            ), 0.0)
            FROM screenshots s
            WHERE s.is_deleted = 0
              AND EXISTS (
                  SELECT 1 FROM ocr_results o
                  WHERE o.screenshot_id = s.id AND o.is_deleted = 0
              )
              AND NOT EXISTS (
                  SELECT 1 FROM derived_index_jobs j
                  WHERE j.index_kind = 'clip_image'
                    AND j.subject_key = s.image_hash
              )
            "#,
            params![assumed_megapixels],
            |row| {
                Ok(ClipBackfillWork {
                    images: row.get::<_, i64>(0)?.max(0) as u64,
                    megapixels: row.get::<_, f64>(1)?.max(0.0),
                })
            },
        )
        .map_err(|error| format!("Failed to size the CLIP backfill: {error}"))
    }

    /// CLIP subjects with no live screenshot behind them.
    ///
    /// Deliberately not an age query. The delete triggers already remove a row
    /// when its screenshot goes, so this is the safety net for rows written
    /// before those triggers existed — including every row the M2.5 step-7
    /// migration imported for a screenshot deleted between the snapshot and the
    /// commit.
    pub fn list_orphaned_clip_image_subjects(&self, limit: u32) -> Result<Vec<String>, String> {
        let limit = limit.clamp(1, 10_000);
        let guard = self.get_connection_named("list_orphaned_clip_image_subjects")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let mut statement = conn
            .prepare(
                r#"
                SELECT t.subject_key
                FROM (
                    SELECT subject_key FROM derived_index_jobs
                    WHERE index_kind = 'clip_image'
                    UNION
                    SELECT subject_key FROM derived_embeddings
                    WHERE index_kind = 'clip_image'
                ) t
                WHERE NOT EXISTS (
                    SELECT 1 FROM screenshots s
                     WHERE s.image_hash = t.subject_key AND s.is_deleted = 0
                )
                LIMIT ?1
                "#,
            )
            .map_err(|error| format!("Failed to prepare orphaned CLIP subject query: {error}"))?;
        let rows = statement
            .query_map(params![limit], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to query orphaned CLIP subjects: {error}"))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .map_err(|error| format!("Failed to read orphaned CLIP subject row: {error}"))
    }

    /// Records that a subject has nothing to index, so the repair scan stops
    /// selecting it.
    ///
    /// [`Self::list_semantic_text_index_candidates`] decides corpus membership
    /// from SQL alone — a live screenshot inside the retention window with at
    /// least one OCR row — because the text that really decides it lives in
    /// encrypted columns no predicate can read. `minilm_sources` applies the
    /// real rule and drops screenshots whose process name, window title, and
    /// OCR text are all empty. Nothing used to record that decision, so such a
    /// screenshot was re-selected, re-decrypted, and re-dropped on every pass,
    /// and enough of them would crowd genuinely missing screenshots out of the
    /// scan's `LIMIT`.
    ///
    /// The row is terminal in the same sense as `discarded`: not claimable, not
    /// counted as backlog, and not revived by a model contract change. It is
    /// fingerprinted against the empty source, so a screenshot that later gains
    /// text no longer matches it and [`Self::ensure_derived_index_job`] queues
    /// fresh work for it like it would for any other source change. Expiry
    /// reclaims the row with its screenshot.
    ///
    /// Returns `false` when the subject is no longer active: the lifecycle
    /// triggers have already removed its rows and there is nothing to record.
    pub fn exclude_derived_index_subject(
        &self,
        spec: &DerivedIndexJobSpec,
        error_code: &str,
        error: &str,
    ) -> Result<bool, String> {
        validate_job_spec(spec)?;
        validate_required_text("error_code", error_code, MAX_METADATA_BYTES)?;
        validate_required_text("error", error, MAX_METADATA_BYTES)?;
        let mut guard = self.get_connection_named("exclude_derived_index_subject")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let epoch_before = Some(read_derived_data_epoch(conn, spec.index_kind)?);
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived exclusion transaction: {error}"))?;
        if !derived_subject_is_active(&tx, spec.index_kind, &spec.subject_key)? {
            return Ok(false);
        }
        // A row that already describes this exact empty source is left alone.
        // The M2.4 migration deliberately copies a legacy Chroma vector for a
        // screenshot whose current SQLite text is empty — it cannot be
        // recomputed from nothing — and stamps it with this same fingerprint.
        let contract_matches: Option<bool> = tx
            .query_row(
                r#"
                SELECT model_id = ?3 AND model_revision = ?4
                       AND embedding_version = ?5 AND source_fingerprint = ?6
                FROM derived_index_jobs
                WHERE index_kind = ?1 AND subject_key = ?2
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
            .optional()
            .map_err(|error| format!("Failed to inspect derived index job: {error}"))?;
        if contract_matches == Some(true) {
            return Ok(true);
        }
        // Any vector still held describes a source that no longer exists, so it
        // must not stay query-visible.
        tx.execute(
            "DELETE FROM derived_embeddings WHERE index_kind = ?1 AND subject_key = ?2",
            params![spec.index_kind.as_str(), spec.subject_key],
        )
        .map_err(|error| format!("Failed to drop an excluded subject's vector: {error}"))?;
        tx.execute(
            r#"
            INSERT INTO derived_index_jobs (
                index_kind, subject_key, status, attempts, error_code, error,
                model_id, model_revision, embedding_version, source_fingerprint,
                updated_at
            ) VALUES (?1, ?2, 'discarded', 0, ?7, ?8, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
            ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                status = 'discarded', attempts = 0, error_code = ?7, error = ?8,
                next_retry_at = NULL, lease_token = NULL,
                model_id = ?3, model_revision = ?4,
                embedding_version = ?5, source_fingerprint = ?6,
                updated_at = CURRENT_TIMESTAMP
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
            ],
        )
        .map_err(|error| format!("Failed to record a derived index exclusion: {error}"))?;
        tx.commit()
            .map_err(|error| format!("Failed to commit a derived exclusion: {error}"))?;
        if let Some(epoch_before) = epoch_before {
            let epoch_after = read_derived_data_epoch(conn, spec.index_kind)?;
            self.note_semantic_cache_removal(
                spec.index_kind,
                &spec.subject_key,
                epoch_before,
                epoch_after,
            );
        }
        Ok(true)
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
        let epoch_before = Some(read_derived_data_epoch(conn, index_kind)?);
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
        if let Some(epoch_before) = epoch_before {
            let epoch_after = read_derived_data_epoch(conn, index_kind)?;
            self.note_semantic_cache_removal(index_kind, subject_key, epoch_before, epoch_after);
        }
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
        self.publish_derived_index_generation_with_progress(
            index_kind,
            |_phase, _current, _total| {},
            || false,
        )
    }

    /// Publish a generation on a blocking worker with cooperative cancellation
    /// and bounded progress callbacks. `sync_all` itself cannot be interrupted;
    /// callers should present that phase as a short safe-write interval.
    pub fn publish_derived_index_generation_with_progress<P, C>(
        &self,
        index_kind: DerivedIndexKind,
        mut progress: P,
        cancelled: C,
    ) -> Result<DerivedIndexGeneration, String>
    where
        P: FnMut(&str, u64, u64),
        C: Fn() -> bool,
    {
        if self.is_migration_in_progress() {
            return Err("Cannot publish a derived index during data migration".to_string());
        }
        let _publish_guard = self.derived_generation_publish_guard();
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
        if cancelled() {
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        let checksum_sha256 = match self.write_sidecar_streaming(
            &temp_path,
            index_kind,
            generation,
            &snapshot,
            &mut progress,
            &cancelled,
        ) {
            Ok(checksum) => checksum,
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
        };
        if cancelled() {
            let _ = fs::remove_file(&temp_path);
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        if let Err(error) =
            verify_sidecar_with_progress(&temp_path, &checksum_sha256, &mut progress, &cancelled)
        {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        if cancelled() {
            let _ = fs::remove_file(&temp_path);
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        progress("publishing_commit", 0, 1);
        fs::rename(&temp_path, &final_path).map_err(|error| {
            let _ = fs::remove_file(&temp_path);
            format!("Failed to publish derived index generation: {error}")
        })?;

        if cancelled() {
            let _ = fs::remove_file(&final_path);
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }

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
        progress("publishing_commit", 1, 1);
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
                generation.generation,
                generation.covered_epoch,
                generation.flat_file_name,
                generation.flat_checksum_sha256,
                generation.ann_file_name,
                generation.ann_checksum_sha256,
                generation.row_count,
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
            params![generation.index_kind.as_str(), generation.covered_epoch],
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
                params![index_kind.as_str(), covered_epoch],
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
                .query_map(params![index_kind.as_str(), covered_epoch], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                })
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

    /// Read one raw-vector page using a short-lived independent connection.
    /// Background ANN rebuilds use this form so the rollback-journal SHARED
    /// lock is released between pages; maintenance bootstrap uses the
    /// single-connection stream below because capture and derived writes are
    /// paused for that window.
    pub(crate) fn list_query_visible_ann_snapshot_page_for_ann(
        &self,
        index_kind: DerivedIndexKind,
        after_subject_key: Option<&str>,
        limit: u32,
    ) -> Result<Vec<DerivedAnnSnapshotRow>, String> {
        self.with_vector_scan_connection("list_query_visible_ann_snapshot_page_for_ann", |conn| {
            list_query_visible_ann_snapshot_page_from_conn(
                conn,
                index_kind,
                after_subject_key,
                limit,
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn list_query_visible_embedding_page_for_ann(
        &self,
        index_kind: DerivedIndexKind,
        after_subject_key: Option<&str>,
        limit: u32,
    ) -> Result<Vec<DerivedEmbeddingRecord>, String> {
        self.with_vector_scan_connection("list_query_visible_embedding_page_for_ann", |conn| {
            list_query_visible_embedding_page_from_conn(conn, index_kind, after_subject_key, limit)
        })
    }

    /// Stream the query-visible embedding snapshot through one independent
    /// read connection. The callback is invoked once per keyset-paginated
    /// page, so callers can write/process a page before the next one is
    /// materialized without repeatedly opening and keying SQLCipher.
    ///
    /// SQLite remains authoritative; this streams the same query-visible set
    /// used by the exact scorer. Callers that need a
    /// stable point-in-time view must hold their own maintenance boundary (the
    /// ANN bootstrap does); ordinary background rebuilds still verify the row
    /// count and publication epoch before committing the generation.
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
        derived_index_snapshot_metadata_from_conn(conn, index_kind)
    }

    fn list_query_visible_embedding_page(
        &self,
        index_kind: DerivedIndexKind,
        after_subject_key: Option<&str>,
        limit: u32,
    ) -> Result<Vec<DerivedEmbeddingRecord>, String> {
        let guard = self.get_connection_named("list_query_visible_embedding_page")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        list_query_visible_embedding_page_from_conn(conn, index_kind, after_subject_key, limit)
    }

    fn write_sidecar_streaming(
        &self,
        path: &Path,
        index_kind: DerivedIndexKind,
        generation: u64,
        snapshot: &DerivedIndexSnapshotMetadata,
        progress: &mut impl FnMut(&str, u64, u64),
        cancelled: &impl Fn() -> bool,
    ) -> Result<String, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("Failed to create derived index temp file: {error}"))?;
        let mut writer = BufWriter::new(file);
        let mut hasher = Sha256::new();
        write_sidecar_header(&mut writer, &mut hasher, index_kind, generation, snapshot)?;
        progress("publishing_write", 0, snapshot.row_count);

        let mut after_subject_key: Option<String> = None;
        let mut written_rows = 0u64;
        loop {
            if cancelled() {
                return Err(DERIVED_GENERATION_CANCELLED.to_string());
            }
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
            progress("publishing_write", written_rows, snapshot.row_count);
        }
        if written_rows != snapshot.row_count {
            return Err(format!(
                "Derived index changed while generation was being streamed: expected {} rows, wrote {written_rows}",
                snapshot.row_count
            ));
        }

        if cancelled() {
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        writer
            .flush()
            .map_err(|error| format!("Failed to flush derived index temp file: {error}"))?;
        progress("publishing_sync", 0, 0);
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("Failed to sync derived index temp file: {error}"))?;
        if cancelled() {
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        Ok(hex::encode(hasher.finalize()))
    }
}

type RawEmbeddingRow = (String, i64, Vec<u8>, String, String, i64, String, String);
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

fn list_query_visible_embedding_page_from_conn(
    conn: &rusqlite::Connection,
    index_kind: DerivedIndexKind,
    after_subject_key: Option<&str>,
    limit: u32,
) -> Result<Vec<DerivedEmbeddingRecord>, String> {
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

/// The `derived_index_jobs` projection shared by every ledger read, so the
/// column list and [`read_job_row`] cannot drift apart.
const JOB_ROW_COLUMNS: &str = "subject_key, status, error_code, error, attempts, \
     next_retry_at, model_id, model_revision, embedding_version, source_fingerprint, \
     updated_at";

/// One definition of "a worker could still pick this up", with `?2` bound to
/// the retry budget. `waiting_for_auth` counts because the session it stalled
/// on may have been unlocked since.
const CLAIMABLE_JOB_PREDICATE: &str = "(status IN ('pending', 'waiting_for_auth') \
     OR (status = 'failed' AND attempts < ?2))";

type JobRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    String,
    String,
    i64,
    String,
    String,
);

fn read_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
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
    ))
}

fn job_record_from_row(
    index_kind: DerivedIndexKind,
    row: JobRow,
) -> Result<DerivedIndexJobRecord, String> {
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
    ) = row;
    Ok(DerivedIndexJobRecord {
        spec: DerivedIndexJobSpec {
            index_kind,
            subject_key,
            model_id,
            model_revision,
            embedding_version: u32::try_from(embedding_version)
                .map_err(|_| format!("Invalid stored embedding version: {embedding_version}"))?,
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
}

pub(super) fn derived_subject_is_active(
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

/// The query-visible content epoch. The `derived_index_state` triggers advance
/// it on every mutation that can change the visible join, which makes it the
/// authority the resident vector cache checks itself against.
pub(super) fn read_derived_data_epoch(
    conn: &rusqlite::Connection,
    index_kind: DerivedIndexKind,
) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE((SELECT data_epoch FROM derived_index_state WHERE index_kind = ?1), 0)",
        [index_kind.as_str()],
        |row| row.get(0),
    )
    .map_err(|error| format!("Failed to read derived index epoch: {error}"))
}

/// Single definition of "query-visible": a completed ledger row whose model
/// contract and fingerprint still match the stored vector. Every projection
/// below shares it so the resident cache cannot drift from the SQL readers.
const VISIBLE_EMBEDDING_SOURCE: &str = r#"
        FROM derived_embeddings e
        INNER JOIN derived_index_jobs j
          ON j.index_kind = e.index_kind AND j.subject_key = e.subject_key
        WHERE e.index_kind = ?1 AND j.status = 'completed'
          AND j.model_id = e.model_id
          AND j.model_revision = e.model_revision
          AND j.embedding_version = e.embedding_version
          AND j.source_fingerprint = e.source_fingerprint
"#;

fn visible_embedding_sql(predicate: &str, suffix: &str) -> String {
    format!(
        r#"
        SELECT e.subject_key, e.dimensions, e.vector_f32, e.model_id,
               e.model_revision, e.embedding_version, e.source_fingerprint,
               e.updated_at
        {VISIBLE_EMBEDDING_SOURCE}
          {predicate} {suffix}
        "#
    )
}

/// Column-minimal projection for the resident cache load, one page at a time.
///
/// The scan needs only an identity and the vector; the four text columns the
/// full projection carries would be decoded and thrown away once per row.
///
/// Paginated rather than whole because the load is the one read here that no
/// retention window bounds — `clip_image` covers the entire history — and a
/// single statement holds a SQLite SHARED lock until it finishes, which in this
/// database's rollback journal mode stalls every capture commit for that long.
/// `semantic_cache.rs::SCAN_PAGE_ROWS` records the measurements behind the page
/// size and the reasoning behind the cursor.
///
/// Bind parameters: `?1` index kind, `?2` the last subject key of the previous
/// page (empty string to start), `?3` the page size. Ordering by `subject_key`
/// walks the `(index_kind, subject_key)` primary-key index directly, so no page
/// costs a temporary sort.
pub(super) fn visible_embedding_page_sql() -> String {
    format!(
        r#"
        SELECT e.subject_key, e.dimensions, e.vector_f32
        {VISIBLE_EMBEDDING_SOURCE}
          AND e.subject_key > ?2
        ORDER BY e.subject_key
        LIMIT ?3
        "#
    )
}

/// Allocation metadata for deciding whether a query-visible vector set may be
/// admitted to the resident exact-scan cache.
///
/// This deliberately avoids reading `vector_f32`: the aggregate walks row and
/// ledger metadata only, so it is much cheaper than materialising the matrix.
/// It runs on the independent read connection used by the paged scan, and its
/// statement is dropped before the vector pages are opened so a writer is not
/// held behind one long read transaction.
pub(super) fn visible_embedding_cache_stats_sql() -> String {
    format!(
        r#"
        SELECT COUNT(*), MIN(e.dimensions), MAX(e.dimensions),
               COALESCE(SUM(LENGTH(CAST(e.subject_key AS BLOB))), 0)
        {VISIBLE_EMBEDDING_SOURCE}
        "#
    )
}

fn visible_embedding_exists_sql() -> String {
    format!(
        r#"
        SELECT 1
        {VISIBLE_EMBEDDING_SOURCE}
        LIMIT 1
        "#
    )
}

fn visible_embedding_count_sql() -> String {
    format!(
        r#"
        SELECT COUNT(*)
        {VISIBLE_EMBEDDING_SOURCE}
        "#
    )
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

pub(super) fn validate_job_spec(spec: &DerivedIndexJobSpec) -> Result<(), String> {
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

pub(super) fn encode_vector(vector: &[f32]) -> Result<Vec<u8>, String> {
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
    if vector.iter().all(|value| value.abs() <= f32::EPSILON) {
        return Err("Derived embedding vector must not be a zero vector".to_string());
    }
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

pub(super) fn decode_vector(bytes: &[u8], dimensions: usize) -> Result<Vec<f32>, String> {
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

pub(crate) fn next_generation_id() -> Result<u64, String> {
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
    verify_sidecar_with_progress(
        path,
        expected_checksum,
        &mut |_phase, _current, _total| {},
        &|| false,
    )
}

fn verify_sidecar_with_progress(
    path: &Path,
    expected_checksum: &str,
    progress: &mut impl FnMut(&str, u64, u64),
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Failed to open derived index sidecar: {error}"))?;
    let total = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
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
    let mut verified = header.len() as u64;
    let mut last_reported = 0_u64;
    progress("publishing_verify", verified, total);
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        if cancelled() {
            return Err(DERIVED_GENERATION_CANCELLED.to_string());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Failed to read derived index sidecar: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        verified = verified.saturating_add(read as u64);
        if verified.saturating_sub(last_reported) >= 8 * 1024 * 1024 || verified >= total {
            progress("publishing_verify", verified, total);
            last_reported = verified;
        }
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

    fn current_epoch(storage: &StorageState) -> u64 {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        u64::try_from(
            read_derived_data_epoch(guard.as_ref().unwrap(), DerivedIndexKind::ClipImage).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn ensure_job_is_idempotent_until_source_change() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "41");
        ensure_active_subject(&storage, &spec);
        assert_eq!(
            storage.ensure_derived_index_job(&spec).unwrap(),
            EnsureDerivedIndexJobResult::Queued
        );
        let lease = storage.mark_derived_index_job_processing(&spec).unwrap();
        storage
            .commit_derived_embedding(&DerivedEmbeddingWrite {
                job: spec.clone(),
                lease_token: lease,
                vector: vec![0.5, 0.25],
            })
            .unwrap();
        assert_eq!(
            storage.ensure_derived_index_job(&spec).unwrap(),
            EnsureDerivedIndexJobResult::AlreadyCurrent
        );
        assert_eq!(
            storage
                .get_derived_index_job(DerivedIndexKind::SemanticText, "41")
                .unwrap()
                .unwrap()
                .status,
            DerivedIndexJobStatus::Completed
        );

        let mut changed = spec.clone();
        changed.source_fingerprint = "new-source".to_string();
        assert_eq!(
            storage.ensure_derived_index_job(&changed).unwrap(),
            EnsureDerivedIndexJobResult::Requeued
        );
    }

    #[test]
    fn cancellation_requeue_preserves_retry_budget() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "43");
        ensure_active_subject(&storage, &spec);
        storage.ensure_derived_index_job(&spec).unwrap();
        let lease = storage.mark_derived_index_job_processing(&spec).unwrap();
        storage
            .requeue_derived_index_job(&spec, &lease, "cancelled", "cancelled by user")
            .unwrap();
        let record = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "43")
            .unwrap()
            .unwrap();
        assert_eq!(record.status, DerivedIndexJobStatus::Pending);
        assert_eq!(record.attempts, 0);
        assert_eq!(record.error_code.as_deref(), Some("cancelled"));
    }

    #[test]
    fn explicit_ensure_resumes_terminal_and_delayed_jobs() {
        let (_temp, storage) = test_storage();
        let discarded = job(DerivedIndexKind::SemanticText, "44");
        let lease = queue_and_claim(&storage, &discarded);
        storage
            .mark_derived_index_job_discarded(&discarded, &lease, "empty_model_input", "empty")
            .unwrap();
        assert_eq!(
            storage.ensure_derived_index_job(&discarded).unwrap(),
            EnsureDerivedIndexJobResult::Requeued
        );

        let failed = job(DerivedIndexKind::SemanticText, "45");
        let lease = queue_and_claim(&storage, &failed);
        storage
            .mark_derived_index_job_failed(
                &failed,
                &lease,
                "temporary",
                "temporary",
                Some("2100-01-01 00:00:00"),
            )
            .unwrap();
        assert_eq!(
            storage.ensure_derived_index_job(&failed).unwrap(),
            EnsureDerivedIndexJobResult::Requeued
        );
        storage
            .mark_derived_index_job_processing(&failed)
            .expect("explicit ensure clears delayed retry timestamp");
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
    fn clip_scan_ranks_by_cosine_and_hides_a_deleted_image() {
        // The step-9 read path in miniature: a top-K over image vectors keyed by
        // hash, and a hit whose last live screenshot is gone must not surface.
        let (_temp, storage) = test_storage();
        for (hash, vector) in [
            ("hash-near", vec![1.0_f32, 0.0]),
            ("hash-far", vec![0.0_f32, 1.0]),
        ] {
            commit_vector(&storage, job(DerivedIndexKind::ClipImage, hash), vector).unwrap();
        }

        let ranked = storage.clip_image_topk(&[1.0, 0.0], 2).unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].subject_key, "hash-near");
        assert!(ranked[0].score > ranked[1].score);

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "UPDATE screenshots SET is_deleted = 1 WHERE image_hash = 'hash-near'",
                    [],
                )
                .unwrap();
        }
        let ranked = storage.clip_image_topk(&[1.0, 0.0], 2).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].subject_key, "hash-far");
    }

    #[test]
    fn clip_candidates_need_ocr_and_stop_once_a_ledger_row_exists() {
        // The scan is the SQL half of Python's `ocr_text.strip()` gate: an image
        // with no OCR row at all was never in this corpus, and an image that
        // already has a ledger row is not offered again.
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().unwrap();
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash, width, height, created_at)
                 VALUES (1, 'a', 'with-ocr', 1920, 1080, datetime('now'))",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash, created_at)
                 VALUES (2, 'b', 'no-ocr', datetime('now'))",
                [],
            )
            .unwrap();
            // Older than the repair window: only a full backfill reaches it.
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash, width, height, created_at)
                 VALUES (3, 'c', 'old-with-ocr', 1920, 1080, datetime('now', '-30 days'))",
                [],
            )
            .unwrap();
            for id in [1, 3] {
                conn.execute(
                    "INSERT INTO ocr_results (screenshot_id, text, text_hash)
                     VALUES (?1, 'hello', 'hash-of-hello-' || ?1)",
                    params![id],
                )
                .unwrap();
            }
        }
        let all = storage.list_clip_image_index_candidates(None, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&"with-ocr".to_string()));
        assert!(all.contains(&"old-with-ocr".to_string()));

        // The bounded scan is the one the automatic repair pass uses. Its whole
        // purpose is to leave the old screenshot alone: before the step-7 copy
        // settles, an image with no ledger row is one the copy has not reached,
        // and re-encoding it costs hours to reproduce a vector Chroma holds.
        let recent = storage
            .list_clip_image_index_candidates(Some("-7 days"), 10)
            .unwrap();
        assert_eq!(recent, vec!["with-ocr".to_string()]);

        storage
            .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "with-ocr"))
            .unwrap();
        assert!(storage
            .list_clip_image_index_candidates(Some("-7 days"), 10)
            .unwrap()
            .is_empty());
        // A ledger row removes a subject from the unbounded scan too, which is
        // what stops a settled migration's own rows from being offered back.
        assert_eq!(
            storage.list_clip_image_index_candidates(None, 10).unwrap(),
            vec!["old-with-ocr".to_string()]
        );
    }

    #[test]
    fn backfill_work_counts_megapixels_and_assumes_a_size_for_rows_without_one() {
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().unwrap();
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash, width, height)
                 VALUES (1, 'a', 'uhd', 3840, 2160)",
                [],
            )
            .unwrap();
            // Written before the dimension columns existed.
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash) VALUES (2, 'b', 'unknown')",
                [],
            )
            .unwrap();
            for id in [1, 2] {
                conn.execute(
                    "INSERT INTO ocr_results (screenshot_id, text, text_hash)
                     VALUES (?1, 'hello', 'hash-' || ?1)",
                    params![id],
                )
                .unwrap();
            }
        }
        let work = storage.clip_image_backfill_work(2.0736).unwrap();
        assert_eq!(work.images, 2);
        // 8.2944 for the 4K row plus the assumed 1080p for the unsized one. A
        // count alone would have called these two images equal work; they are
        // not, by a factor of four.
        assert!((work.megapixels - (8.2944 + 2.0736)).abs() < 1e-6);

        storage
            .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "uhd"))
            .unwrap();
        let work = storage.clip_image_backfill_work(2.0736).unwrap();
        assert_eq!(work.images, 1);
        assert!((work.megapixels - 2.0736).abs() < 1e-6);
    }

    #[test]
    fn clip_orphan_scan_reclaims_a_row_the_delete_triggers_never_saw() {
        // `screenshots.image_hash` is UNIQUE and the capture path skips a hash
        // it already holds, so an image maps to exactly one screenshot. When
        // that screenshot is soft-deleted through the ordinary path the trigger
        // removes the derived rows inside the deleting transaction and this
        // scan finds nothing — which is what the middle assertion pins.
        //
        // The scan exists for rows that arrived some other way: a database
        // written before those triggers, or a step-7 import that committed for a
        // screenshot deleted after the snapshot was taken. Both are modelled
        // here by writing the ledger row directly, after the delete, which is
        // the one thing `upsert_derived_index_job` refuses to do.
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (1, 'a', 'only')",
                    [],
                )
                .unwrap();
        }
        storage
            .upsert_derived_index_job(&job(DerivedIndexKind::ClipImage, "only"))
            .unwrap();
        assert!(storage
            .list_orphaned_clip_image_subjects(10)
            .unwrap()
            .is_empty());

        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            let conn = guard.as_ref().unwrap();
            conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
                .unwrap();
            let remaining: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM derived_index_jobs WHERE index_kind = 'clip_image'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(remaining, 0, "the delete trigger owns the ordinary path");

            conn.execute(
                "INSERT INTO derived_index_jobs (index_kind, subject_key, status, model_id,                  model_revision, embedding_version, source_fingerprint)                  VALUES ('clip_image', 'only', 'completed', 'm', 'r', 1, 'f')",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            storage.list_orphaned_clip_image_subjects(10).unwrap(),
            vec!["only".to_string()]
        );
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

    /// Give a screenshot a `created_at` the retention queries can reason about.
    fn backdate_screenshot(storage: &StorageState, id: i64, modifier: &str) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        guard
            .as_ref()
            .unwrap()
            .execute(
                "UPDATE screenshots SET created_at = datetime('now', ?2) WHERE id = ?1",
                params![id, modifier],
            )
            .unwrap();
    }

    fn insert_screenshot_with_ocr(storage: &StorageState, id: i64) {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO screenshots (id, image_path, image_hash) VALUES (?1, ?2, ?3)",
            params![id, format!("{id}.enc"), format!("hash-{id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ocr_results (screenshot_id, text, text_hash) VALUES (?1, ?2, ?3)",
            params![id, format!("text-{id}"), format!("text-hash-{id}")],
        )
        .unwrap();
    }

    #[test]
    fn only_jobs_a_worker_could_actually_pick_up_are_claimable() {
        let (_temp, storage) = test_storage();

        // Never started.
        let pending = job(DerivedIndexKind::SemanticText, "1");
        ensure_active_subject(&storage, &pending);
        storage.upsert_derived_index_job(&pending).unwrap();

        // Already leased by someone else.
        let processing = job(DerivedIndexKind::SemanticText, "2");
        queue_and_claim(&storage, &processing);

        // Failed with the budget intact and the backoff elapsed.
        let retryable = job(DerivedIndexKind::SemanticText, "3");
        let lease = queue_and_claim(&storage, &retryable);
        storage
            .mark_derived_index_job_failed(&retryable, &lease, "embed_failed", "boom", None)
            .unwrap();

        // Failed with the backoff still running.
        let backing_off = job(DerivedIndexKind::SemanticText, "4");
        let lease = queue_and_claim(&storage, &backing_off);
        storage
            .mark_derived_index_job_failed(
                &backing_off,
                &lease,
                "embed_failed",
                "boom",
                Some("9999-12-31 23:59:59"),
            )
            .unwrap();

        let claimable = storage
            .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
            .unwrap();
        let subjects: Vec<&str> = claimable
            .iter()
            .map(|record| record.spec.subject_key.as_str())
            .collect();
        assert_eq!(subjects, vec!["1", "3"]);

        // A spent retry budget stops the job being claimable: nothing picks it
        // up again on its own, which is what the diagnostic calls stalled.
        let starved: Vec<String> = storage
            .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 1, 100)
            .unwrap()
            .into_iter()
            .map(|record| record.spec.subject_key)
            .collect();
        assert_eq!(starved, vec!["1".to_string()]);

        let backlog = storage
            .derived_index_backlog(DerivedIndexKind::SemanticText, 1)
            .unwrap();
        // Only the never-started job is still claimable at a budget of one; both
        // failures have spent it, and a spent budget is what `exhausted` counts.
        // The leased job is neither: someone is holding it right now.
        assert_eq!(backlog.claimable, 1);
        assert_eq!(backlog.exhausted, 2);
        assert!(backlog.oldest_claimable_age_secs.is_some());

        // With the real budget the retryable failure is claimable again, and so
        // is the one still inside its backoff: this figure is queue depth, not
        // what a worker could lease this instant. Nothing is stalled.
        let generous = storage
            .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
            .unwrap();
        assert_eq!(generous.claimable, 3);
        assert_eq!(generous.exhausted, 0);

        // A different index kind must not leak into either number.
        let clip = storage
            .derived_index_backlog(DerivedIndexKind::ClipImage, 5)
            .unwrap();
        assert_eq!(clip, DerivedIndexBacklog::default());
    }

    #[test]
    fn index_candidates_are_screenshots_with_text_no_ledger_row_and_inside_retention() {
        let (_temp, storage) = test_storage();
        for id in [10, 11, 12, 13] {
            insert_screenshot_with_ocr(&storage, id);
        }
        // 11 already has a ledger row, 12 aged out, 13 was deleted.
        let queued = job(DerivedIndexKind::SemanticText, "11");
        storage.upsert_derived_index_job(&queued).unwrap();
        backdate_screenshot(&storage, 12, "-40 days");
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 13", [])
                .unwrap();
        }
        // A screenshot with no OCR row at all is not part of the corpus.
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO screenshots (id, image_path, image_hash) VALUES (14, '14.enc', 'hash-14')",
                    [],
                )
                .unwrap();
        }

        let candidates = storage
            .list_semantic_text_index_candidates("-30 days", 100)
            .unwrap();
        assert_eq!(candidates, vec![10]);
    }

    /// The candidate scan can only over-approximate the corpus, so the ledger has
    /// to remember what the decrypting builder ruled out. Without this the same
    /// screenshot is handed back, re-decrypted, and re-dropped on every pass, and
    /// once enough of them accumulate they fill the scan's `LIMIT` and the repair
    /// path stops reaching screenshots that really are missing a vector.
    #[test]
    fn an_excluded_subject_leaves_the_repair_scan_for_good() {
        let (_temp, storage) = test_storage();
        insert_screenshot_with_ocr(&storage, 30);
        insert_screenshot_with_ocr(&storage, 31);
        assert_eq!(
            storage
                .list_semantic_text_index_candidates("-30 days", 100)
                .unwrap(),
            vec![31, 30]
        );

        let empty = job(DerivedIndexKind::SemanticText, "30");
        assert!(storage
            .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
            .unwrap());

        // Out of the scan, and out of both worker-facing projections: nothing
        // will lease it, and it is neither queue depth nor a stalled job.
        assert_eq!(
            storage
                .list_semantic_text_index_candidates("-30 days", 100)
                .unwrap(),
            vec![31]
        );
        assert!(storage
            .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
            .unwrap()
            .is_empty());
        let backlog = storage
            .derived_index_backlog(DerivedIndexKind::SemanticText, 5)
            .unwrap();
        assert_eq!(backlog.claimable, 0);
        assert_eq!(backlog.exhausted, 0);

        // Repeating the decision is a no-op rather than a second row.
        assert!(storage
            .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
            .unwrap());
        let record = storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "30")
            .unwrap()
            .unwrap();
        assert_eq!(record.status, DerivedIndexJobStatus::Discarded);
        assert_eq!(record.attempts, 0);
        assert_eq!(record.error_code.as_deref(), Some("empty_source"));
    }

    #[test]
    fn regaining_text_reclaims_an_excluded_subject() {
        let (_temp, storage) = test_storage();
        let empty = job(DerivedIndexKind::SemanticText, "32");
        ensure_active_subject(&storage, &empty);
        assert!(storage
            .exclude_derived_index_subject(&empty, "empty_source", "nothing to encode")
            .unwrap());

        // The exclusion is fingerprinted against the empty source, so a
        // re-OCR that produces text is an ordinary source change and not
        // something the ledger has already settled.
        let mut with_text = empty.clone();
        with_text.source_fingerprint = "source-32-with-text".to_string();
        assert_eq!(
            storage.ensure_derived_index_job(&with_text).unwrap(),
            EnsureDerivedIndexJobResult::Requeued
        );
        let claimable: Vec<String> = storage
            .claimable_derived_index_jobs(DerivedIndexKind::SemanticText, 5, 100)
            .unwrap()
            .into_iter()
            .map(|record| record.spec.subject_key)
            .collect();
        assert_eq!(claimable, vec!["32".to_string()]);
    }

    #[test]
    fn an_exclusion_drops_a_vector_whose_source_is_gone_but_spares_a_matching_one() {
        let (_temp, storage) = test_storage();
        // A vector encoded from text that has since disappeared cannot stay
        // query-visible: it would keep ranking against a source nothing holds.
        let stale = job(DerivedIndexKind::SemanticText, "33");
        commit_vector(&storage, stale.clone(), vec![1.0, 0.0]).unwrap();
        let mut now_empty = stale.clone();
        now_empty.source_fingerprint = "source-33-empty".to_string();
        assert!(storage
            .exclude_derived_index_subject(&now_empty, "empty_source", "nothing to encode")
            .unwrap());
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "33")
            .unwrap()
            .is_none());

        // The M2.4 migration deliberately copies a legacy Chroma vector for a
        // screenshot whose current text is empty, stamped with exactly this
        // fingerprint. It cannot be recomputed, so excluding must not touch it.
        let legacy = job(DerivedIndexKind::SemanticText, "34");
        commit_vector(&storage, legacy.clone(), vec![0.0, 1.0]).unwrap();
        assert!(storage
            .exclude_derived_index_subject(&legacy, "empty_source", "nothing to encode")
            .unwrap());
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "34")
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_deleted_screenshot_is_reported_rather_than_recorded_as_excluded() {
        let (_temp, storage) = test_storage();
        // Nothing to record: the lifecycle triggers already removed the rows,
        // and inserting one would resurrect a subject the schema retired.
        let gone = job(DerivedIndexKind::SemanticText, "35");
        assert!(!storage
            .exclude_derived_index_subject(&gone, "empty_source", "nothing to encode")
            .unwrap());
        assert!(storage
            .get_derived_index_job(DerivedIndexKind::SemanticText, "35")
            .unwrap()
            .is_none());
    }

    #[test]
    fn expiry_reaps_aged_subjects_and_leaves_the_rest_alone() {
        let (_temp, storage) = test_storage();
        for id in [20, 21] {
            let spec = job(DerivedIndexKind::SemanticText, &id.to_string());
            commit_vector(&storage, spec, vec![1.0, 0.0]).unwrap();
        }
        backdate_screenshot(&storage, 20, "-40 days");

        let expired = storage
            .list_expired_semantic_text_subjects("-30 days", 100)
            .unwrap();
        assert_eq!(expired, vec!["20".to_string()]);

        assert!(storage
            .delete_derived_index_subject(DerivedIndexKind::SemanticText, "20")
            .unwrap());
        assert!(storage
            .list_expired_semantic_text_subjects("-30 days", 100)
            .unwrap()
            .is_empty());
        // The row still inside the window survives the sweep.
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "21")
            .unwrap()
            .is_some());
    }

    #[test]
    fn expiry_also_sweeps_a_ledger_row_whose_screenshot_is_gone() {
        // The schema triggers already delete derived rows with their screenshot,
        // and `ensure_derived_index_job` refuses to queue work for a subject that
        // is not live, so this branch cannot be reached through the normal APIs
        // at all. It is a safety net for a row that arrived some other way — a
        // database written before those triggers existed — which is otherwise
        // invisible and never ages out, because ageing is decided against a
        // screenshot row that no longer exists.
        let (_temp, storage) = test_storage();
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO derived_index_jobs
                       (index_kind, subject_key, model_id, model_revision,
                        embedding_version, source_fingerprint, status)
                     VALUES ('semantic_text', '999', 'model-a', 'revision-1', 1,
                             'source-999', 'pending')",
                    [],
                )
                .unwrap();
        }

        let expired = storage
            .list_expired_semantic_text_subjects("-30 days", 100)
            .unwrap();
        assert_eq!(expired, vec!["999".to_string()]);
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

    fn ann_manifest(covered_epoch: u64) -> DerivedAnnGeneration {
        DerivedAnnGeneration {
            index_kind: DerivedIndexKind::ClipImage,
            generation: 1,
            covered_epoch,
            flat_file_name: "clip_image-1.cpdvec".to_string(),
            flat_checksum_sha256: "00".repeat(32),
            ann_file_name: "clip_image-1.cpdann".to_string(),
            ann_checksum_sha256: "11".repeat(32),
            row_count: 1,
            dimensions: 2,
            model_id: "model-a".to_string(),
            model_revision: "revision-1".to_string(),
            embedding_version: 1,
            sidecar_format_version: 4,
            ann_format_version: 1,
            algorithm: "hnsw".to_string(),
            implementation_version: "usearch-2.26.0".to_string(),
            metric: "ip".to_string(),
            quantization: "i8".to_string(),
            connectivity: 16,
            expansion_add: 160,
            expansion_search: 256,
            created_at: "2026-08-13T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn ann_build_failure_state_persists_deduplicates_notification_and_clears_on_publish() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-base"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let covered_epoch = current_epoch(&storage);

        let first = storage
            .record_derived_ann_build_failure(
                DerivedIndexKind::ClipImage,
                1,
                "2026-08-14T00:00:00Z",
                "2026-08-14T00:15:00Z",
                "builder_missing",
                "builder missing",
                true,
                true,
            )
            .unwrap();
        assert!(first.should_notify);
        assert!(!first.state.notification_sent);

        let taken = storage
            .take_derived_ann_build_notification(DerivedIndexKind::ClipImage)
            .unwrap()
            .unwrap();
        assert!(taken.notification_sent);
        assert!(storage
            .take_derived_ann_build_notification(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());

        let second = storage
            .record_derived_ann_build_failure(
                DerivedIndexKind::ClipImage,
                2,
                "2026-08-14T00:01:00Z",
                "2026-08-15T00:01:00Z",
                "builder_missing",
                "builder still missing",
                true,
                true,
            )
            .unwrap();
        assert!(!second.should_notify);
        assert_eq!(second.state.consecutive_failures, 2);
        assert_eq!(
            storage
                .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
                .unwrap()
                .unwrap(),
            second.state
        );

        storage
            .record_derived_ann_generation(&ann_manifest(covered_epoch))
            .unwrap();
        assert!(storage
            .get_derived_ann_build_state(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ann_generation_survives_new_capture_and_tail_tracks_latest_value() {
        let (_temp, storage) = test_storage();
        let first = job(DerivedIndexKind::ClipImage, "hash-a");
        commit_vector(&storage, first.clone(), vec![1.0, 0.0]).unwrap();
        let covered_epoch = current_epoch(&storage);
        storage
            .record_derived_ann_generation(&ann_manifest(covered_epoch))
            .unwrap();

        let second = job(DerivedIndexKind::ClipImage, "hash-b");
        commit_vector(&storage, second, vec![0.0, 1.0]).unwrap();
        assert!(storage
            .get_derived_ann_generation(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_some());
        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "hash-b");
        assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));

        let mut changed = first;
        changed.source_fingerprint = "changed-hash-a".to_string();
        let lease = queue_and_claim(&storage, &changed);
        storage
            .commit_derived_embedding(&DerivedEmbeddingWrite {
                job: changed,
                lease_token: lease,
                vector: vec![0.5, 0.5],
            })
            .unwrap();
        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(
            tail.iter()
                .find(|row| row.subject_key == "hash-a")
                .and_then(|row| row.vector.as_deref()),
            Some(&[0.5, 0.5][..])
        );
    }

    #[test]
    fn ann_tail_returns_tombstone_after_subject_deletion() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-delete"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let covered_epoch = current_epoch(&storage);
        storage
            .delete_derived_index_subject(DerivedIndexKind::ClipImage, "hash-delete")
            .unwrap();
        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "hash-delete");
        assert!(tail[0].vector.is_none());
    }

    #[test]
    fn ann_tail_returns_tombstone_while_existing_embedding_is_not_query_visible() {
        let (_temp, storage) = test_storage();
        let original = job(DerivedIndexKind::ClipImage, "hash-pending");
        commit_vector(&storage, original.clone(), vec![1.0, 0.0]).unwrap();
        let covered_epoch = current_epoch(&storage);

        let mut replacement = original;
        replacement.source_fingerprint = "replacement-source".to_string();
        storage.upsert_derived_index_job(&replacement).unwrap();

        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "hash-pending");
        assert!(tail[0].vector.is_none());
    }

    #[test]
    fn ann_tail_keeps_only_the_latest_epoch_for_repeated_subject_changes() {
        let (_temp, storage) = test_storage();
        let mut spec = job(DerivedIndexKind::ClipImage, "hash-repeat");
        commit_vector(&storage, spec.clone(), vec![1.0, 0.0]).unwrap();
        let covered_epoch = current_epoch(&storage);

        spec.source_fingerprint = "second".to_string();
        let lease = queue_and_claim(&storage, &spec);
        storage
            .commit_derived_embedding(&DerivedEmbeddingWrite {
                job: spec.clone(),
                lease_token: lease,
                vector: vec![0.5, 0.5],
            })
            .unwrap();
        let epoch_after_second = current_epoch(&storage);

        spec.source_fingerprint = "third".to_string();
        let lease = queue_and_claim(&storage, &spec);
        storage
            .commit_derived_embedding(&DerivedEmbeddingWrite {
                job: spec,
                lease_token: lease,
                vector: vec![0.0, 1.0],
            })
            .unwrap();
        let epoch_after_third = current_epoch(&storage);
        assert!(epoch_after_third > epoch_after_second);

        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "hash-repeat");
        assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));

        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        let conn = guard.as_ref().unwrap();
        let stored_epoch: i64 = conn
            .query_row(
                "SELECT change_epoch FROM derived_ann_changes WHERE index_kind = 'clip_image' AND subject_key = 'hash-repeat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_epoch as u64, epoch_after_third);
    }

    #[test]
    fn recording_new_ann_generation_preserves_changes_that_landed_during_build() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-base"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let covered_epoch = current_epoch(&storage);

        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-during-build"),
            vec![0.0, 1.0],
        )
        .unwrap();
        let current = current_epoch(&storage);
        assert!(current > covered_epoch);

        storage
            .record_derived_ann_generation(&ann_manifest(covered_epoch))
            .unwrap();
        let tail = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 100)
            .unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].subject_key, "hash-during-build");
        assert_eq!(tail[0].vector.as_deref(), Some(&[0.0, 1.0][..]));
    }

    #[test]
    fn ann_manifest_cannot_claim_an_epoch_newer_than_sqlite() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-base"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let future_epoch = current_epoch(&storage) + 1;
        assert!(storage
            .record_derived_ann_generation(&ann_manifest(future_epoch))
            .is_err());
        assert!(storage
            .get_derived_ann_generation(DerivedIndexKind::ClipImage)
            .unwrap()
            .is_none());
    }

    #[test]
    fn oversized_ann_tail_refuses_partial_overlay() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-base"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let covered_epoch = current_epoch(&storage);
        for key in ["hash-tail-a", "hash-tail-b"] {
            commit_vector(
                &storage,
                job(DerivedIndexKind::ClipImage, key),
                vec![0.0, 1.0],
            )
            .unwrap();
        }
        let error = storage
            .list_derived_ann_tail(DerivedIndexKind::ClipImage, covered_epoch, 1)
            .unwrap_err();
        assert_eq!(error, "ann_tail_too_large:2");
    }

    #[test]
    fn ann_page_stream_keeps_pending_jobs_out_without_waiting_for_them() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-a"),
            vec![1.0, 0.0],
        )
        .unwrap();
        commit_vector(
            &storage,
            job(DerivedIndexKind::ClipImage, "hash-c"),
            vec![0.0, 1.0],
        )
        .unwrap();
        let pending = job(DerivedIndexKind::ClipImage, "hash-b");
        ensure_active_subject(&storage, &pending);
        storage.ensure_derived_index_job(&pending).unwrap();

        let mut pages = Vec::new();
        storage
            .for_each_query_visible_embedding_page_for_ann(DerivedIndexKind::ClipImage, 1, |page| {
                pages.push(
                    page.into_iter()
                        .map(|row| row.subject_key)
                        .collect::<Vec<_>>(),
                );
                Ok(())
            })
            .unwrap();

        assert_eq!(
            pages,
            vec![vec!["hash-a".to_string()], vec!["hash-c".to_string()]]
        );
        assert_eq!(
            storage
                .derived_index_backlog(DerivedIndexKind::ClipImage, 3)
                .unwrap()
                .claimable,
            1
        );
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

    #[test]
    fn semantic_text_topk_ranks_visible_rows_by_cosine() {
        let (_temp, storage) = test_storage();
        // Three L2-normalized 2-D vectors at increasing angle from the query.
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "2"),
            vec![0.8, 0.6],
        )
        .unwrap();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "3"),
            vec![0.0, 1.0],
        )
        .unwrap();

        // A pending rebuild must hide its row from retrieval.
        let hidden = job(DerivedIndexKind::SemanticText, "4");
        ensure_active_subject(&storage, &hidden);
        storage.upsert_derived_index_job(&hidden).unwrap();

        let query = vec![1.0f32, 0.0];
        let ranked = storage.semantic_text_topk(&query, 2).unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].subject_key, "1");
        assert_eq!(ranked[1].subject_key, "2");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert!(ranked[0].score >= ranked[1].score);

        // The pending subject (id 4) never appears; only completed rows scan.
        assert_eq!(storage.semantic_text_topk(&query, 10).unwrap().len(), 3);
        assert_eq!(
            storage
                .count_query_visible_embeddings(DerivedIndexKind::SemanticText)
                .unwrap(),
            3
        );
        assert!(storage
            .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap());
        assert!(!storage
            .has_query_visible_embeddings(DerivedIndexKind::ClipImage)
            .unwrap());
    }

    #[test]
    fn query_visible_embedding_existence_tracks_model_contract() {
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "7");
        assert!(!storage
            .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap());

        commit_vector(&storage, spec.clone(), vec![0.6, 0.8]).unwrap();
        assert!(storage
            .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap());

        storage
            .invalidate_derived_index_model(
                DerivedIndexKind::SemanticText,
                &spec.model_id,
                "revision-2",
                spec.embedding_version,
            )
            .unwrap();
        assert!(!storage
            .has_query_visible_embeddings(DerivedIndexKind::SemanticText)
            .unwrap());
    }

    #[test]
    fn semantic_text_topk_rejects_empty_query_and_zero_k() {
        let (_temp, storage) = test_storage();
        assert!(storage.semantic_text_topk(&[], 5).is_err());
        assert!(storage
            .semantic_text_topk(&[1.0, 0.0], 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn batch_vector_reads_return_only_query_visible_subjects() {
        // The Smart Cluster prefilter reads a whole peeked batch at once. Two
        // properties matter and neither is obvious from the signature: a
        // subject whose rebuild is pending must not come back (its vector is
        // stale), and a subject that was never indexed must be silently absent
        // rather than an error, because that is the ordinary case for a
        // screenshot still queued in the ledger.
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "2"),
            vec![0.0, 1.0],
        )
        .unwrap();
        let pending = job(DerivedIndexKind::SemanticText, "3");
        ensure_active_subject(&storage, &pending);
        storage.upsert_derived_index_job(&pending).unwrap();

        let subjects: Vec<String> = ["1", "2", "3", "999"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let vectors = storage
            .get_query_visible_embeddings_by_subjects(DerivedIndexKind::SemanticText, &subjects)
            .unwrap();
        assert_eq!(vectors.len(), 2);
        assert_eq!(
            vectors.get("1").map(Vec::as_slice),
            Some([1.0, 0.0].as_slice())
        );
        assert_eq!(
            vectors.get("2").map(Vec::as_slice),
            Some([0.0, 1.0].as_slice())
        );
        assert!(
            !vectors.contains_key("3"),
            "a pending rebuild is not visible"
        );
        assert!(!vectors.contains_key("999"), "an unknown subject is absent");

        // And the empty batch is not a query at all.
        assert!(storage
            .get_query_visible_embeddings_by_subjects(DerivedIndexKind::SemanticText, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_batch_vector_read_agrees_with_the_single_subject_read() {
        // The batch path duplicates `visible_embedding_sql`'s predicate, so it
        // could drift from the single-subject reader that defines what
        // "query-visible" means. This pins them together.
        let (_temp, storage) = test_storage();
        let spec = job(DerivedIndexKind::SemanticText, "7");
        commit_vector(&storage, spec.clone(), vec![0.6, 0.8]).unwrap();

        let single = storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "7")
            .unwrap()
            .expect("committed vector is visible");
        let batch = storage
            .get_query_visible_embeddings_by_subjects(
                DerivedIndexKind::SemanticText,
                &["7".to_string()],
            )
            .unwrap();
        assert_eq!(batch.get("7"), Some(&single.vector));

        // Invalidating the model hides the row from both readers together.
        storage
            .invalidate_derived_index_model(
                DerivedIndexKind::SemanticText,
                &spec.model_id,
                "revision-2",
                spec.embedding_version,
            )
            .unwrap();
        assert!(storage
            .get_query_visible_embedding(DerivedIndexKind::SemanticText, "7")
            .unwrap()
            .is_none());
        assert!(storage
            .get_query_visible_embeddings_by_subjects(
                DerivedIndexKind::SemanticText,
                &["7".to_string()]
            )
            .unwrap()
            .is_empty());
    }

    fn data_epoch(storage: &StorageState) -> i64 {
        let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
        read_derived_data_epoch(guard.as_ref().unwrap(), DerivedIndexKind::SemanticText).unwrap()
    }

    const VECTOR_BYTES: usize = std::mem::size_of::<f32>();

    #[test]
    fn resident_cache_absorbs_write_path_updates() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        let query = vec![1.0f32, 0.0];
        // The first query loads the matrix; nothing is resident before it.
        assert_eq!(storage.semantic_vector_cache_bytes(), 0);
        assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 1);
        assert_eq!(
            storage.semantic_vector_cache_matrix_bytes(),
            2 * VECTOR_BYTES
        );
        assert!(
            storage.semantic_vector_cache_bytes() > storage.semantic_vector_cache_matrix_bytes()
        );

        // A dual-write landing while the cache is resident must be visible to
        // the next query without a reload.
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "2"),
            vec![0.8, 0.6],
        )
        .unwrap();
        let ranked = storage.semantic_text_topk(&query, 5).unwrap();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].subject_key, "1");
        assert_eq!(ranked[1].subject_key, "2");
        assert_eq!(
            storage.semantic_vector_cache_matrix_bytes(),
            4 * VECTOR_BYTES
        );

        // Re-encoding an existing subject replaces its row in place.
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "2"),
            vec![0.0, 1.0],
        )
        .unwrap();
        let ranked = storage.semantic_text_topk(&[0.0, 1.0], 1).unwrap();
        assert_eq!(ranked[0].subject_key, "2");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert_eq!(
            storage.semantic_vector_cache_matrix_bytes(),
            4 * VECTOR_BYTES
        );
    }

    #[test]
    fn resident_cache_does_not_survive_a_trigger_driven_delete() {
        let (_temp, storage) = test_storage();
        for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![0.9, 0.1])] {
            commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
        }
        let query = vec![1.0f32, 0.0];
        assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 2);

        // Soft-deleting the screenshot removes the embedding from inside
        // SQLite, which the write-path hooks cannot observe.
        let before = data_epoch(&storage);
        {
            let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
            guard
                .as_ref()
                .unwrap()
                .execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
                .unwrap();
        }
        // The nested epoch trigger is what makes the resident matrix notice.
        // If this ever stops holding, the visibility re-check below is the only
        // thing standing between the cache and a deleted screenshot.
        assert!(data_epoch(&storage) > before);

        let ranked = storage.semantic_text_topk(&query, 5).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].subject_key, "2");
        assert_eq!(
            storage.semantic_vector_cache_matrix_bytes(),
            2 * VECTOR_BYTES
        );
    }

    #[test]
    fn resident_cache_drops_a_subject_deleted_through_the_api() {
        let (_temp, storage) = test_storage();
        for (key, vector) in [("1", vec![1.0, 0.0]), ("2", vec![0.9, 0.1])] {
            commit_vector(&storage, job(DerivedIndexKind::SemanticText, key), vector).unwrap();
        }
        let query = vec![1.0f32, 0.0];
        assert_eq!(storage.semantic_text_topk(&query, 5).unwrap().len(), 2);

        assert!(storage
            .delete_derived_index_subject(DerivedIndexKind::SemanticText, "1")
            .unwrap());
        let ranked = storage.semantic_text_topk(&query, 5).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].subject_key, "2");
        assert_eq!(
            storage.semantic_vector_cache_matrix_bytes(),
            2 * VECTOR_BYTES
        );
    }

    #[test]
    fn tiny_resident_budget_keeps_the_full_clip_history_searchable() {
        let (_temp, storage) = test_storage();
        for (key, vector) in [
            ("image-a", vec![1.0, 0.0]),
            ("image-b", vec![0.8, 0.6]),
            ("image-c", vec![0.0, 1.0]),
        ] {
            commit_vector(&storage, job(DerivedIndexKind::ClipImage, key), vector).unwrap();
        }

        // A budget this small cannot admit even one resident row. The exact
        // fallback must still rank the complete durable index, and it must not
        // leave the temporary scan matrix behind for the next query.
        let ranked = storage
            .semantic_topk_resident_with_budget(DerivedIndexKind::ClipImage, &[1.0, 0.0], 3, 1)
            .unwrap();
        assert_eq!(
            ranked
                .iter()
                .map(|row| row.subject_key.as_str())
                .collect::<Vec<_>>(),
            vec!["image-a", "image-b", "image-c"]
        );
        assert_eq!(storage.semantic_vector_cache_bytes(), 0);
        assert_eq!(storage.semantic_vector_cache_matrix_bytes(), 0);

        // A second query takes the same bounded-memory path rather than
        // accidentally becoming a resident hit.
        assert_eq!(
            storage
                .semantic_topk_resident_with_budget(DerivedIndexKind::ClipImage, &[0.0, 1.0], 1, 1,)
                .unwrap()[0]
                .subject_key,
            "image-c"
        );
        assert_eq!(storage.semantic_vector_cache_bytes(), 0);
    }

    #[test]
    fn idle_eviction_releases_the_resident_matrix() {
        let (_temp, storage) = test_storage();
        commit_vector(
            &storage,
            job(DerivedIndexKind::SemanticText, "1"),
            vec![1.0, 0.0],
        )
        .unwrap();
        // Nothing resident yet, so there is nothing to evict.
        assert!(!storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));

        assert_eq!(storage.semantic_text_topk(&[1.0, 0.0], 1).unwrap().len(), 1);
        assert!(storage.semantic_vector_cache_bytes() > 0);
        // A live TTL keeps a just-used cache.
        assert!(!storage.evict_semantic_vector_cache_if_idle(super::super::SEMANTIC_CACHE_IDLE_TTL));
        assert!(storage.semantic_vector_cache_bytes() > 0);

        assert!(storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));
        assert_eq!(storage.semantic_vector_cache_bytes(), 0);
        assert!(!storage.evict_semantic_vector_cache_if_idle(std::time::Duration::ZERO));

        // The next query reloads transparently.
        assert_eq!(storage.semantic_text_topk(&[1.0, 0.0], 1).unwrap().len(), 1);
        assert!(storage.semantic_vector_cache_bytes() > 0);
    }
}
