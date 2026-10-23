//! Rust-owned persistence for rebuildable semantic embeddings.
//!
//! SQLite stores the durable derived cache and the per-subject job ledger. A
//! generation-versioned sidecar can be published from completed rows without
//! becoming authoritative; consumers must be able to rebuild it from SQLite.

mod ann;

use super::semantic_cache::TopKOptions;
use super::StorageState;
use crate::task_error::TaskError;
use chrono::{DateTime, NaiveDateTime, Utc};
use rand::RngCore;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SUBJECT_KEY_BYTES: usize = 1024;
pub(super) const MAX_METADATA_BYTES: usize = 4096;
const MAX_VECTOR_DIMENSIONS: usize = 65_536;
const LEASE_TOKEN_BYTES: usize = 16;

#[derive(Default)]
struct EmbeddingCommitContext<'a> {
    receipt: Option<&'a crate::processing_stage::TaskReceipt>,
    generation: Option<u64>,
    revisions: &'a [(i64, Option<i64>)],
    execution: Option<&'a crate::background_policy::ExecutionLease>,
}

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
    /// Rows that are still outstanding, including rows whose per-subject retry
    /// timestamp has not elapsed yet. This is the user-visible backlog.
    pub claimable: u64,
    /// Rows a worker can claim at this instant. The scheduler uses this value;
    /// using `claimable` there would repeatedly admit a task whose rows are all
    /// still in their own backoff window.
    pub ready: u64,
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

struct DerivedWorkerJobUpdate<'a> {
    status: DerivedIndexJobStatus,
    error_code: Option<&'a str>,
    error: Option<&'a str>,
    next_retry_at: Option<&'a str>,
    increment_attempts: bool,
}

impl StorageState {
    pub(crate) fn clip_input_pixels(&self, hash: &str) -> Result<Option<u64>, String> {
        let guard = self.get_connection_named("clip_input_pixels")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT CAST(width AS INTEGER)*CAST(height AS INTEGER) FROM screenshots
             WHERE image_hash=?1 AND is_deleted=0 AND width>0 AND height>0 LIMIT 1",
                [hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|pixels| pixels.and_then(|p| u64::try_from(p).ok()))
            .map_err(|e| e.to_string())
    }
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
    ) -> Result<String, TaskError> {
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
            return Err(TaskError::Paused("derived_job_busy"));
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

    /// Make delayed/failed rows eligible for an explicit user retry and reset
    /// their per-subject budget. A manual action is a new recovery decision,
    /// not another automatic attempt against the old failure window.
    pub fn reset_derived_index_retries(&self, index_kind: DerivedIndexKind) -> Result<u64, String> {
        let guard = self.get_connection_named("reset_derived_index_retries")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let changed = conn
            .execute(
                "UPDATE derived_index_jobs
                 SET status = 'pending', attempts = 0,
                     error_code = NULL, error = NULL,
                     next_retry_at = NULL, lease_token = NULL,
                     updated_at = CURRENT_TIMESTAMP
                 WHERE index_kind = ?1 AND status = 'failed'",
                [index_kind.as_str()],
            )
            .map_err(|error| format!("Failed to reset derived index retries: {error}"))?;
        u64::try_from(changed).map_err(|_| "Invalid reset derived index row count".to_string())
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
        self.commit_derived_embedding_with_receipt(write, EmbeddingCommitContext::default())
            .map_err(Into::into)
    }

    pub(crate) fn commit_archive_embedding(
        &self,
        write: &DerivedEmbeddingWrite,
        generation: u64,
        revisions: &[(i64, Option<i64>)],
        execution: Option<&crate::background_policy::ExecutionLease>,
    ) -> Result<(), TaskError> {
        self.commit_derived_embedding_with_receipt(
            write,
            EmbeddingCommitContext {
                generation: Some(generation),
                revisions,
                execution,
                ..Default::default()
            },
        )
    }

    pub(crate) fn commit_staged_embedding(
        &self,
        write: &DerivedEmbeddingWrite,
        receipt: &crate::processing_stage::TaskReceipt,
    ) -> Result<(), TaskError> {
        self.processing_stage.check_receipt(receipt)?;
        use carbonpaper_app_bound::protocol::Consumer;
        if !matches!(
            (receipt.consumer, write.job.index_kind),
            (Consumer::MiniLm, DerivedIndexKind::SemanticText)
                | (Consumer::Clip, DerivedIndexKind::ClipImage)
        ) || (receipt.consumer == Consumer::MiniLm
            && write.job.subject_key != receipt.screenshot_id.to_string())
        {
            return Err("Staged embedding scope mismatch".into());
        }
        let execution = crate::background_policy::current_execution();
        self.commit_derived_embedding_with_receipt(
            write,
            EmbeddingCommitContext {
                receipt: Some(receipt),
                execution: execution.as_deref(),
                ..Default::default()
            },
        )
    }

    fn commit_derived_embedding_with_receipt(
        &self,
        write: &DerivedEmbeddingWrite,
        context: EmbeddingCommitContext<'_>,
    ) -> Result<(), TaskError> {
        let receipt = context.receipt;
        validate_job_spec(&write.job)?;
        validate_required_text("lease_token", &write.lease_token, MAX_METADATA_BYTES)?;
        let vector_blob = encode_vector(&write.vector)?;
        let dimensions = i64::try_from(write.vector.len())
            .map_err(|_| "Derived embedding dimensions exceed SQLite range".to_string())?;
        let mut guard = self.get_connection_named("commit_derived_embedding")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        if let Some(execution) = context.execution {
            execution.check()?;
        }
        if context
            .generation
            .is_some_and(|g| g != self.db_generation())
        {
            return Err(TaskError::Paused("database_changed"));
        }
        for (id, expected) in context.revisions {
            let current: Option<i64> = conn
                .query_row(
                    "SELECT COALESCE(r.revision,0) FROM screenshots s
                 LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
                 WHERE s.id=?1 AND s.is_deleted=0",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if current != *expected {
                return Err(TaskError::Paused("embedding_source_changed"));
            }
        }
        // Sampled before the write so the resident cache can tell "I was
        // current and this is my delta" from "I already missed something".
        let epoch_before = Some(read_derived_data_epoch(conn, write.job.index_kind)?);
        if let Some(receipt) = receipt {
            if !self.processing_stage.available()
                || !self.staged_source_current_on_conn(conn, receipt)?
            {
                return Err(TaskError::Paused("staged_source_changed"));
            }
            if write.job.index_kind == DerivedIndexKind::ClipImage {
                let matches: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND image_hash=?2)",
                        params![receipt.screenshot_id, write.job.subject_key],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                if !matches {
                    return Err("Staged image scope mismatch".into());
                }
            }
        }
        let tx = conn
            .transaction()
            .map_err(|error| format!("Failed to start derived embedding transaction: {error}"))?;
        if !derived_subject_is_active(&tx, write.job.index_kind, &write.job.subject_key)? {
            return Err(TaskError::Paused("inactive_subject"));
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
            return Err(TaskError::Paused("derived_lease_stale"));
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
        if let Some(receipt) = receipt {
            if Self::record_staged_receipt_on_conn(&tx, receipt)? {
                Self::enqueue_staged_semantic_completion_on_conn(&tx, receipt)?;
            }
        }
        if let Some(execution) = context.execution {
            execution.check()?;
        }
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
    /// resident matrix serves the ~30-day MiniLM hot layer. CLIP image search
    /// has its own persistent ANN path in `clip_ann.rs`.
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
        self.semantic_topk_resident(
            DerivedIndexKind::SemanticText,
            query,
            k,
            TopKOptions::default(),
        )
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
        self.semantic_topk_resident(
            DerivedIndexKind::ClipImage,
            query,
            k,
            TopKOptions::default(),
        )
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
        self.semantic_topk_resident(
            DerivedIndexKind::ClipImage,
            query,
            k,
            TopKOptions {
                deadline: Some(deadline),
                ..TopKOptions::default()
            },
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
                    COALESCE(SUM(CASE WHEN (
                        (status IN ('pending', 'waiting_for_auth')
                         OR (status = 'failed' AND attempts < ?2))
                        AND (next_retry_at IS NULL OR next_retry_at <= CURRENT_TIMESTAMP)
                    ) THEN 1 ELSE 0 END), 0),
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
                    ready: row.get::<_, i64>(1)?.max(0) as u64,
                    exhausted: row.get::<_, i64>(2)?.max(0) as u64,
                    oldest_claimable_age_secs: row.get::<_, Option<i64>>(3)?,
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
}

type RawEmbeddingRow = (String, i64, Vec<u8>, String, String, i64, String, String);
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
pub(super) const VISIBLE_EMBEDDING_SOURCE: &str = r#"
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

#[cfg(test)]
mod tests;
