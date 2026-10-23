//! Screenshot CRUD operations (save, get, delete, commit, abort).

use crate::credential_manager::{
    decrypt_with_master_key, encrypt_with_master_key, CngKeySession, CredentialError,
    CredentialManagerState,
};
use chrono::{DateTime, Utc};
use rand::RngCore;
use roaring::RoaringBitmap;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{
    wire_time, BackgroundReadError, BackgroundScreenshotSummary, DeleteQueueStatus, DensityBucket,
    DerivedIndexKind, IncrementalVacuumResult, IndexStorageStats, OcrDeleteBatchResult,
    OcrResultInput, QueueScreenshotCandidate, SaveScreenshotRequest, SaveScreenshotResponse,
    SoftDeleteResult, SoftDeleteScreenshotsResult, StorageState,
};

const MAX_OCR_POSTPROCESS_ATTEMPTS: i64 = 5;
const MAX_OCR_DELETE_CLEANUP_FAILURES: i64 = 3;
const STARTUP_PENDING_SCREENSHOTS_SQL: &str =
    "SELECT id FROM screenshots WHERE status = 'pending' AND is_deleted = 0 ORDER BY id ASC";

struct OcrDeleteQueueRow {
    id: i64,
    existing_id: Option<i64>,
    failure_count: i64,
    legacy_text: Option<String>,
    text_hash: Option<String>,
    text_enc: Option<Vec<u8>>,
    text_key_encrypted: Option<Vec<u8>>,
}

impl OcrDeleteQueueRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            failure_count: row.get(1)?,
            existing_id: row.get(2)?,
            legacy_text: row.get(3)?,
            text_hash: row.get(4)?,
            text_enc: row.get(5)?,
            text_key_encrypted: row.get(6)?,
        })
    }
}

struct OcrDeleteCleanupFailure {
    id: i64,
    failure_count: i64,
    error: String,
}

struct EncryptedOcrResultRow {
    id: i64,
    screenshot_id: i64,
    text_enc: Option<Vec<u8>>,
    text_key_encrypted: Option<Vec<u8>>,
    confidence: f64,
    box_coords: Vec<Vec<f64>>,
    created_at: String,
}

impl EncryptedOcrResultRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            screenshot_id: row.get(1)?,
            text_enc: row.get(2)?,
            text_key_encrypted: row.get(3)?,
            confidence: row.get(4)?,
            box_coords: vec![
                vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
            ],
            created_at: row.get(13)?,
        })
    }
}

struct EncryptedScreenshotSummaryRow {
    id: i64,
    window_title_plain: Option<String>,
    process_name_plain: Option<String>,
    window_title_enc: Option<Vec<u8>>,
    process_name_enc: Option<Vec<u8>>,
    content_key_enc: Option<Vec<u8>>,
    timestamp: Option<i64>,
    category: Option<String>,
}

impl EncryptedScreenshotSummaryRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let timestamp = row
            .get::<_, Option<String>>(6)?
            .and_then(|value| value.parse::<i64>().ok());
        Ok(Self {
            id: row.get(0)?,
            window_title_plain: row.get(1)?,
            process_name_plain: row.get(2)?,
            window_title_enc: row.get(3)?,
            process_name_enc: row.get(4)?,
            content_key_enc: row.get(5)?,
            timestamp,
            category: row.get(7)?,
        })
    }
}

/// Bound parallel payload decoding so background reads leave cores for the UI.
const CNG_UNWRAP_MAX_THREADS: usize = 8;

fn open_cng_session(
    credential: &CredentialManagerState,
) -> Result<CngKeySession<'_>, BackgroundReadError> {
    CngKeySession::open_silent(credential).map_err(|error| match error {
        CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
        other => BackgroundReadError::Other(format!("Failed to open CNG session: {other}")),
    })
}

fn check_read_deadline(deadline: Option<Instant>) -> Result<(), BackgroundReadError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(BackgroundReadError::Other(
            "query_deadline_exceeded_during_hydrate".to_string(),
        ));
    }
    Ok(())
}

/// Decode row batches in parallel, borrowing the authenticated credential
/// context in each worker. Only the NCrypt calls are serialized; workers can
/// decrypt and decode payloads concurrently without opening unauthenticated
/// native handles. Preserve input order and fail the whole batch on AuthRequired.
pub(super) fn unwrap_batch_parallel<T, R, F>(
    credential: &CredentialManagerState,
    items: Vec<T>,
    process: F,
) -> Result<Vec<R>, BackgroundReadError>
where
    T: Send,
    R: Send,
    F: Fn(&CngKeySession<'_>, T) -> Result<R, BackgroundReadError> + Sync,
{
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(CNG_UNWRAP_MAX_THREADS)
        .min(items.len())
        .max(1);
    if threads == 1 {
        let session = open_cng_session(credential)?;
        return items
            .into_iter()
            .map(|item| process(&session, item))
            .collect();
    }

    let chunk_size = items.len().div_ceil(threads);
    let mut chunks: Vec<Vec<T>> = Vec::with_capacity(threads);
    let mut remaining = items.into_iter();
    loop {
        let chunk: Vec<T> = remaining.by_ref().take(chunk_size).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }

    let results: Vec<Result<Vec<R>, BackgroundReadError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(|| {
                    let session = open_cng_session(credential)?;
                    chunk
                        .into_iter()
                        .map(|item| process(&session, item))
                        .collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    Err(BackgroundReadError::Other(
                        "CNG unwrap worker panicked".to_string(),
                    ))
                })
            })
            .collect()
    });

    let mut merged = Vec::new();
    let mut first_error: Option<BackgroundReadError> = None;
    for result in results {
        match result {
            Ok(mut part) => merged.append(&mut part),
            Err(BackgroundReadError::AuthRequired) => {
                return Err(BackgroundReadError::AuthRequired)
            }
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(merged),
    }
}

/// Builds the batched OCR prefix query for `placeholder_count` screenshot ids.
/// `ORDER BY screenshot_id, id` is load-bearing: `id` is the insertion rowid,
/// and the boxes of one screenshot are inserted in a single transaction in the
/// exact order the OCR engine emitted them — the same order their texts were
/// joined into the MiniLM source string. Ordering by geometry instead would
/// reshuffle boxes that share a visual line (`box_y1` jitters by a few pixels)
/// and rebuild text that no longer matches the stored vector.
fn ocr_text_prefix_query(placeholder_count: usize) -> String {
    let placeholders = vec!["?"; placeholder_count].join(",");
    format!(
        "SELECT screenshot_id, text_enc, text_key_encrypted
         FROM ocr_results
         WHERE is_deleted = 0 AND screenshot_id IN ({})
         ORDER BY screenshot_id, id",
        placeholders
    )
}

/// Joins decrypted OCR box texts with single spaces per screenshot, skipping
/// empty boxes, and stops decrypting a screenshot's remaining boxes once
/// `min_chars` joined characters are collected. Rows of one screenshot must
/// arrive contiguously in insertion order; the produced string is then a join
/// prefix of the full join, so its first `min_chars` characters match the
/// full text exactly (the MiniLM source contract only consumes that prefix).
fn assemble_ocr_text_prefixes(
    rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)>,
    min_chars: usize,
    deadline: Option<Instant>,
    mut decrypt: impl FnMut(&[u8], &[u8]) -> Result<String, BackgroundReadError>,
) -> Result<std::collections::HashMap<i64, String>, BackgroundReadError> {
    let mut texts: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    let mut joined_chars: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for (screenshot_id, text_enc, key_enc) in rows {
        check_read_deadline(deadline)?;
        if joined_chars
            .get(&screenshot_id)
            .is_some_and(|&count| count >= min_chars)
        {
            continue;
        }
        let (Some(data), Some(key)) = (text_enc.as_deref(), key_enc.as_deref()) else {
            continue;
        };
        let text = decrypt(data, key)?;
        if text.is_empty() {
            continue;
        }
        let list = texts.entry(screenshot_id).or_default();
        let count = joined_chars.entry(screenshot_id).or_insert(0);
        *count += text.chars().count() + usize::from(!list.is_empty());
        list.push(text);
    }
    Ok(texts
        .into_iter()
        .map(|(id, list)| (id, list.join(" ")))
        .collect())
}

pub(super) fn ocr_postprocess_retry_decision(
    current_attempts: i64,
) -> (&'static str, Option<i64>, i64) {
    let next_attempts = current_attempts.saturating_add(1);
    if next_attempts >= MAX_OCR_POSTPROCESS_ATTEMPTS {
        ("failed", None, next_attempts)
    } else {
        let delay_secs = match next_attempts {
            1 => 30,
            2 => 60,
            3 => 120,
            _ => 300,
        };
        ("pending", Some(delay_secs), next_attempts)
    }
}

fn validate_ocr_result(result: &OcrResultInput) -> Result<(), String> {
    if result.box_coords.len() != 4 || result.box_coords.iter().any(|point| point.len() != 2) {
        return Err("OCR box must contain exactly four 2D points".to_string());
    }
    if !result.confidence.is_finite()
        || result
            .box_coords
            .iter()
            .flatten()
            .any(|coordinate| !coordinate.is_finite())
    {
        return Err("OCR result contains a non-finite value".to_string());
    }
    if result.text.chars().count() > 16_384 {
        return Err("OCR result text exceeds the storage limit".to_string());
    }
    Ok(())
}

impl StorageState {
    pub fn set_ocr_status(
        &self,
        screenshot_id: i64,
        status: &str,
        engine: Option<&str>,
        model_id: Option<&str>,
        execution_provider: Option<&str>,
        error: Option<&str>,
        elapsed_ms: Option<f64>,
    ) -> Result<(), String> {
        const VALID_STATUSES: &[&str] = &["pending", "running", "completed", "failed"];
        if !VALID_STATUSES.contains(&status) {
            return Err(format!("Invalid OCR status: {status}"));
        }
        let error = error.map(|value| value.chars().take(500).collect::<String>());
        let guard = self.get_connection_named("set_ocr_status")?;
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshot_ocr_status (
                screenshot_id, status, engine, model_id, execution_provider,
                error, elapsed_ms, attempted_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(screenshot_id) DO UPDATE SET
                status = excluded.status,
                engine = excluded.engine,
                model_id = excluded.model_id,
                execution_provider = excluded.execution_provider,
                error = excluded.error,
                elapsed_ms = excluded.elapsed_ms,
                attempted_at = excluded.attempted_at,
                updated_at = CURRENT_TIMESTAMP",
            params![
                screenshot_id,
                status,
                engine,
                model_id,
                execution_provider,
                error,
                elapsed_ms,
            ],
        )
        .map_err(|e| format!("Failed to update OCR status: {e}"))?;
        Ok(())
    }

    pub fn count_failed_ocr(&self) -> Result<i64, String> {
        let guard = self.get_connection_named("count_failed_ocr")?;
        let conn = guard.as_ref().unwrap();
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM screenshot_ocr_status WHERE status = 'failed'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count OCR failures: {e}"))?;
        Ok(count)
    }

    pub fn set_ocr_postprocess_status(
        &self,
        screenshot_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        const VALID_STATUSES: &[&str] = &[
            "none",
            "pending",
            "queued",
            "processing",
            "waiting_for_auth",
            "completed",
            "failed",
            "discarded",
        ];
        if !VALID_STATUSES.contains(&status) {
            return Err(format!("Invalid OCR postprocess status: {status}"));
        }
        let error = error.map(|value| value.chars().take(500).collect::<String>());
        let guard = self.get_connection_named("set_ocr_postprocess_status")?;
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "UPDATE screenshot_ocr_status
             SET postprocess_status = ?2,
                 postprocess_lease = NULL,
                 postprocess_error = ?3,
                 postprocess_attempts = CASE
                    WHEN ?2 IN ('none', 'pending', 'completed') THEN 0
                    ELSE postprocess_attempts
                 END,
                 postprocess_next_retry_at = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE screenshot_id = ?1",
            params![screenshot_id, status, error],
        )
        .map_err(|e| format!("Failed to update OCR postprocess status: {e}"))?;
        Ok(())
    }

    pub fn record_ocr_postprocess_retry(
        &self,
        screenshot_id: i64,
        error: &str,
    ) -> Result<(), String> {
        let error = error.chars().take(500).collect::<String>();
        let guard = self.get_connection_named("record_ocr_postprocess_retry")?;
        let conn = guard.as_ref().unwrap();
        let current_attempts = conn
            .query_row(
                "SELECT postprocess_attempts FROM screenshot_ocr_status WHERE screenshot_id = ?1",
                [screenshot_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| format!("Failed to read OCR postprocess attempts: {e}"))?
            .ok_or_else(|| format!("OCR status row not found for screenshot {screenshot_id}"))?;
        let (status, delay_secs, next_attempts) = ocr_postprocess_retry_decision(current_attempts);
        let retry_modifier = delay_secs.map(|seconds| format!("+{seconds} seconds"));
        conn.execute(
            "UPDATE screenshot_ocr_status
             SET postprocess_attempts = ?2,
                 postprocess_status = ?3,
                 postprocess_error = ?4,
                 postprocess_next_retry_at = CASE
                    WHEN ?5 IS NULL THEN NULL
                    ELSE datetime('now', ?5)
                 END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE screenshot_id = ?1",
            params![screenshot_id, next_attempts, status, error, retry_modifier,],
        )
        .map_err(|e| format!("Failed to schedule OCR postprocess retry: {e}"))?;
        Ok(())
    }

    /// Preserve derived work across restarts. Ordinary retries still wait for
    /// archive authorization; staged work has its own narrow input provider.
    pub(crate) fn recover_incomplete_ocr_postprocess(&self) -> Result<usize, String> {
        let guard = self.get_connection_named("recover_incomplete_ocr_postprocess")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .execute(
                "UPDATE screenshot_ocr_status SET postprocess_status='waiting_for_auth',
             postprocess_error=NULL, postprocess_next_retry_at=NULL, postprocess_lease=NULL
             WHERE postprocess_status IN ('pending','queued','processing')",
                [],
            )
            .map_err(|error| format!("Failed to recover OCR postprocess rows: {error}"))
    }

    pub fn list_pending_ocr_postprocess_ids(&self, limit: i64) -> Result<Vec<i64>, String> {
        let guard = self.get_connection_named("list_pending_ocr_postprocess_ids")?;
        let conn = guard.as_ref().unwrap();
        let mut statement = conn
            .prepare(
                "SELECT screenshot_id FROM screenshot_ocr_status
                 WHERE postprocess_status IN ('pending', 'waiting_for_auth')
                   AND EXISTS(SELECT 1 FROM screenshots WHERE screenshots.id=screenshot_id AND is_deleted=0)
                   AND postprocess_attempts < 5
                   AND (postprocess_next_retry_at IS NULL OR postprocess_next_retry_at <= CURRENT_TIMESTAMP)
                 ORDER BY updated_at ASC LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare OCR postprocess query: {e}"))?;
        let ids = statement
            .query_map([limit.clamp(1, 100)], |row| row.get(0))
            .map_err(|e| format!("Failed to query OCR postprocess backlog: {e}"))?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(|e| format!("Failed to read OCR postprocess backlog: {e}"))?;
        Ok(ids)
    }

    /// Get all screenshot image paths (for thumbnail warmup).
    pub fn get_all_image_paths(&self) -> Result<Vec<String>, String> {
        let conn = self.open_read_connection_named("get_all_image_paths")?;
        let mut stmt = conn
            .prepare(
                "SELECT image_path FROM screenshots WHERE is_deleted = 0 ORDER BY created_at DESC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;
        let paths: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }

    /// Check if a screenshot with the given hash already exists.
    pub fn screenshot_exists(&self, image_hash: &str) -> Result<bool, String> {
        let mut guard = self.get_connection_named("screenshot_exists")?;
        let conn = guard.as_mut().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ? AND is_deleted = 0)",
                [image_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check screenshot: {}", e))?;

        Ok(count > 0)
    }

    /// Batch-lookup screenshot IDs by image hashes.
    /// Returns a map from image_hash to screenshot id.
    pub fn batch_get_screenshot_ids_by_hash(
        &self,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, i64>, String> {
        if hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let guard = self.get_connection_named("batch_get_screenshot_ids_by_hash")?;
        let conn = guard.as_ref().unwrap();

        let placeholders = hashes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT image_hash, id FROM screenshots WHERE is_deleted = 0 AND image_hash IN ({})",
            placeholders
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare batch id lookup: {}", e))?;
        let params: Vec<&dyn rusqlite::types::ToSql> = hashes
            .iter()
            .map(|h| h as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(params.as_slice(), |row| {
                let hash: String = row.get(0)?;
                let id: i64 = row.get(1)?;
                Ok((hash, id))
            })
            .map_err(|e| format!("Failed to batch-query screenshot ids: {}", e))?;

        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (hash, id) = row.map_err(|e| format!("Failed to read row: {}", e))?;
            map.insert(hash, id);
        }
        Ok(map)
    }

    /// Save a screenshot and its OCR results.
    pub fn save_screenshot(
        &self,
        request: &SaveScreenshotRequest,
    ) -> Result<SaveScreenshotResponse, String> {
        // Check for duplicates
        if self.screenshot_exists(&request.image_hash)? {
            return Ok(SaveScreenshotResponse {
                status: "duplicate".to_string(),
                screenshot_id: None,
                image_path: None,
                added: 0,
                skipped: request
                    .ocr_results
                    .as_ref()
                    .map(|v| v.len() as i32)
                    .unwrap_or(0),
            });
        }

        // Decode image data
        let image_data = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &request.image_data,
        )
        .map_err(|e| format!("Failed to decode image data: {}", e))?;

        // Generate row key for image and metadata encryption
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        // Encrypt image data
        let encrypted_image = encrypt_with_master_key(&row_key, &image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = self
            .wrap_row_key_for_storage(&row_key)
            .map_err(|e| format!("Failed to wrap image row key: {}", e))?;

        // Generate filename (use .enc extension to indicate encrypted file)
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        let filename = format!("screenshot_{}.png.enc", timestamp);
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let image_path = screenshot_dir.join(&filename);

        // Save encrypted image file
        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = self.to_relative_image_path(&image_path);

        // Generate thumbnail while we still have image_data and row_key
        if let Err(e) = self.generate_thumbnail_from_data(&image_data, &image_path, &row_key) {
            tracing::warn!("Failed to generate thumbnail during save: {}", e);
        }

        // Save to database (SQLCipher whole-database encryption)
        let mut guard = self.get_connection_named("save_screenshot")?;
        let conn = guard.as_mut().unwrap();

        let metadata_json = request
            .metadata
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok());
        let window_title_enc = match &request.window_title {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt window title: {}", e))?,
            ),
            None => None,
        };
        let process_name_enc = match &request.process_name {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt process name: {}", e))?,
            ),
            None => None,
        };
        let metadata_enc = match &metadata_json {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt metadata: {}", e))?,
            ),
            None => None,
        };

        let page_url_enc_save = match &request.page_url {
            Some(value) if !value.is_empty() => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt page_url: {}", e))?,
            ),
            _ => None,
        };

        // Use dedup tables for page_icon and visible_links
        let page_icon_id: Option<i64> = match &request.page_icon {
            Some(value) if !value.is_empty() => Some(self.get_or_create_page_icon_id(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(self.get_or_create_link_set_id(conn, links)?),
            _ => None,
        };

        Self::zeroize_bytes(&mut row_key);

        conn.execute(
            "INSERT INTO screenshots (
                image_path, image_hash, width, height,
                window_title, process_name, metadata,
                window_title_enc, process_name_enc, metadata_enc,
                content_key_encrypted,
                source, page_url_enc, page_icon_id, link_set_id
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &image_path_str,
                &request.image_hash,
                request.width,
                request.height,
                Option::<String>::None,
                request.process_name.clone(), // plaintext for fast aggregation
                Option::<String>::None,
                window_title_enc,
                process_name_enc,
                metadata_enc,
                encrypted_row_key,
                request.source.as_deref(),
                page_url_enc_save,
                page_icon_id,
                link_set_id,
            ],
        )
        .map_err(|e| format!("Failed to insert screenshot: {}", e))?;

        let screenshot_id = conn.last_insert_rowid();

        // Save OCR results
        let mut added = 0;
        let mut skipped = 0;

        if let Some(ocr_results) = &request.ocr_results {
            for result in ocr_results {
                if let Err(error) = validate_ocr_result(result) {
                    tracing::warn!("Skipping invalid OCR result during save: {}", error);
                    skipped += 1;
                    continue;
                }
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;

                // Check for duplicates
                let box_coords = &result.box_coords;
                if box_coords.len() >= 4 {
                    let existing: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM ocr_results
                             WHERE screenshot_id = ?
                             AND is_deleted = 0
                             AND ABS(box_x1 - ?) < 10 AND ABS(box_y1 - ?) < 10",
                            params![screenshot_id, box_coords[0][0], box_coords[0][1],],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);

                    if existing > 0 {
                        skipped += 1;
                        continue;
                    }

                    // Insert new OCR result with encrypted text (lazy indexing will process this later)
                    conn.execute(
                        "INSERT INTO ocr_results (
                            screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                            box_x1, box_y1, box_x2, box_y2,
                            box_x3, box_y3, box_x4, box_y4
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            screenshot_id,
                            Option::<String>::None,
                            "", // empty text_hash signifies unindexed/backlogged
                            text_enc,
                            text_key_encrypted,
                            result.confidence,
                            box_coords[0][0], box_coords[0][1],
                            box_coords[1][0], box_coords[1][1],
                            box_coords[2][0], box_coords[2][1],
                            box_coords[3][0], box_coords[3][1],
                        ],
                    )
                    .map_err(|e| format!("Failed to insert OCR result: {}", e))?;

                    added += 1;
                    self.ocr_row_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added,
            skipped,
        })
    }

    /// Internal capture path that avoids an unnecessary bytes -> Base64 -> bytes round trip.
    pub fn save_screenshot_temp_bytes(
        &self,
        request: &SaveScreenshotRequest,
        image_data: &[u8],
    ) -> Result<SaveScreenshotResponse, String> {
        let fn_start = std::time::Instant::now();

        // Return duplicate if already exists
        if self.screenshot_exists(&request.image_hash)? {
            return Ok(SaveScreenshotResponse {
                status: "duplicate".to_string(),
                screenshot_id: None,
                image_path: None,
                added: 0,
                skipped: 0,
            });
        }
        let exists_dur = fn_start.elapsed();

        // Generate row key and encrypt image
        let t1 = std::time::Instant::now();
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_image = encrypt_with_master_key(&row_key, image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = self
            .wrap_row_key_for_storage(&row_key)
            .map_err(|e| format!("Failed to wrap image row key: {}", e))?;
        let img_encrypt_dur = t1.elapsed();

        // Use .pending suffix to mark temporary file
        let t2 = std::time::Instant::now();
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        let filename = format!("screenshot_{}.png.enc.pending", timestamp);
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let image_path = screenshot_dir.join(&filename);

        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;
        let file_write_dur = t2.elapsed();

        let image_path_str = self.to_relative_image_path(&image_path);

        // Generate thumbnail while we still have image_data and row_key
        // Use the final path (without .pending) so thumbnail path stays valid after commit rename
        let final_image_path = {
            let fname = image_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if fname.ends_with(".pending") {
                image_path.with_file_name(fname.trim_end_matches(".pending"))
            } else {
                image_path.clone()
            }
        };
        if let Err(e) = self.generate_thumbnail_from_data(image_data, &final_image_path, &row_key) {
            tracing::warn!("Failed to generate thumbnail during temp save: {}", e);
        }

        let mut guard = self.get_connection_named("save_screenshot_temp_bytes")?;
        let conn = guard.as_mut().unwrap();
        let mutex_wait_dur = fn_start.elapsed() - exists_dur - img_encrypt_dur - file_write_dur;

        let t3 = std::time::Instant::now();
        let metadata_json = request
            .metadata
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok());

        let window_title_enc = match &request.window_title {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt window title: {}", e))?,
            ),
            None => None,
        };
        let process_name_enc = match &request.process_name {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt process name: {}", e))?,
            ),
            None => None,
        };
        let metadata_enc = match &metadata_json {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt metadata: {}", e))?,
            ),
            None => None,
        };

        let page_url_enc = match &request.page_url {
            Some(value) if !value.is_empty() => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt page_url: {}", e))?,
            ),
            _ => None,
        };

        // Use dedup tables for page_icon and visible_links
        let page_icon_id: Option<i64> = match &request.page_icon {
            Some(value) if !value.is_empty() => Some(self.get_or_create_page_icon_id(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(self.get_or_create_link_set_id(conn, links)?),
            _ => None,
        };

        Self::zeroize_bytes(&mut row_key);

        conn.execute(
            "INSERT INTO screenshots (
                image_path, image_hash, width, height,
                window_title, process_name, metadata,
                window_title_enc, process_name_enc, metadata_enc,
                content_key_encrypted, status,
                source, page_url_enc, page_icon_id, link_set_id
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &image_path_str,
                &request.image_hash,
                request.width,
                request.height,
                Option::<String>::None,
                request.process_name.clone(), // plaintext for fast aggregation
                Option::<String>::None,
                window_title_enc,
                process_name_enc,
                metadata_enc,
                encrypted_row_key,
                "pending",
                request.source.as_deref(),
                page_url_enc,
                page_icon_id,
                link_set_id,
            ],
        )
        .map_err(|e| format!("Failed to insert screenshot: {}", e))?;

        let screenshot_id = conn.last_insert_rowid();
        let in_lock_dur = t3.elapsed();

        drop(guard);

        let total_dur = fn_start.elapsed();
        if total_dur.as_millis() >= 500 {
            tracing::warn!(
                "[DIAG:DB] save_screenshot_temp_bytes id={} total={:?} (exists_check={:?}, img_encrypt={:?}, file_write={:?}, mutex_wait~={:?}, in_lock={:?})",
                screenshot_id, total_dur, exists_dur, img_encrypt_dur, file_write_dur, mutex_wait_dur, in_lock_dur
            );
        }

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added: 0,
            skipped: 0,
        })
    }

    /// Commit pending screenshot: attach OCR results, update index and mark as committed.
    pub fn commit_screenshot(
        &self,
        screenshot_id: i64,
        ocr_results: Option<&Vec<OcrResultInput>>,
        category: Option<&str>,
        category_confidence: Option<f64>,
    ) -> Result<SaveScreenshotResponse, String> {
        let fn_start = std::time::Instant::now();
        let ocr_count = ocr_results.map(|v| v.len()).unwrap_or(0);

        let db_wait_started = std::time::Instant::now();
        let image_path_str = {
            let guard = self.get_connection_named("commit_screenshot.lookup")?;
            let conn = guard.as_ref().unwrap();
            let image_path: Option<String> = conn
                .query_row(
                    "SELECT image_path FROM screenshots WHERE id = ? AND is_deleted = 0",
                    params![screenshot_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("Failed to query screenshot: {}", e))?;
            drop(guard);
            image_path.ok_or_else(|| "Screenshot not found".to_string())?
        };
        let mutex_wait_dur = db_wait_started.elapsed();

        let image_path = self.resolve_image_path(&image_path_str);

        // If filename ends with .pending, rename to final name before taking the DB lock again.
        let mut final_image_path_str = image_path_str.clone();
        if let Some(fname) = image_path.file_name().and_then(|s| s.to_str()) {
            if fname.ends_with(".pending") {
                let new_name = fname.trim_end_matches(".pending");
                let new_path = image_path.with_file_name(new_name);
                if let Err(e) = std::fs::rename(&image_path, &new_path) {
                    // Log but continue trying to insert OCR results
                    tracing::error!("Failed to rename pending image file: {}", e);
                } else {
                    final_image_path_str = self.to_relative_image_path(&new_path);
                }
            }
        }

        let mut added = 0;
        let mut skipped = 0;

        // Timing accumulators for OCR processing
        let mut total_encrypt_dur = std::time::Duration::ZERO;
        let total_db_insert_dur;
        let total_bitmap_dur = std::time::Duration::ZERO;

        // Encrypt OCR results outside the global DB mutex. This avoids holding the
        // storage lock while generating row keys and wrapping them with CNG/public-key APIs.
        let mut encrypted_results = Vec::new();
        if let Some(results) = ocr_results {
            for result in results {
                if let Err(error) = validate_ocr_result(result) {
                    skipped += 1;
                    tracing::warn!(
                        "Skipping invalid OCR result while committing screenshot {}: {}",
                        screenshot_id,
                        error
                    );
                    continue;
                }
                let te0 = std::time::Instant::now();
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;
                total_encrypt_dur += te0.elapsed();
                encrypted_results.push((result, text_enc, text_key_encrypted));
            }
        }

        let td0 = std::time::Instant::now();
        {
            let mut guard = self.get_connection_named("commit_screenshot.write")?;
            let conn = guard.as_mut().unwrap();
            let tx = conn
                .transaction()
                .map_err(|e| format!("Failed to start commit transaction: {}", e))?;

            for (result, text_enc, text_key_encrypted) in encrypted_results {
                tx.execute(
                    "INSERT INTO ocr_results (
                        screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        screenshot_id,
                        Option::<String>::None,
                        "", // empty text_hash signifies unindexed/backlogged
                        text_enc,
                        text_key_encrypted,
                        result.confidence,
                        result.box_coords[0][0],
                        result.box_coords[0][1],
                        result.box_coords[1][0],
                        result.box_coords[1][1],
                        result.box_coords[2][0],
                        result.box_coords[2][1],
                        result.box_coords[3][0],
                        result.box_coords[3][1],
                    ],
                )
                .map_err(|e| format!("Failed to insert OCR result: {}", e))?;

                added += 1;
            }

            // Mark committed and set committed_at. If classification has already
            // arrived asynchronously, do not overwrite it with NULL.
            let updated = if category.is_some() {
                tx.execute(
                    "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP, category = ?, category_confidence = ? WHERE id = ? AND is_deleted = 0",
                    params![final_image_path_str, "committed", category, category_confidence, screenshot_id],
                )
            } else {
                tx.execute(
                    "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP WHERE id = ? AND is_deleted = 0",
                    params![final_image_path_str, "committed", screenshot_id],
                )
            }
            .map_err(|e| format!("Failed to mark screenshot committed: {}", e))?;

            if updated == 0 {
                return Err("Screenshot not found".to_string());
            }

            tx.commit()
                .map_err(|e| format!("Failed to commit screenshot transaction: {}", e))?;

            drop(guard);
        }
        total_db_insert_dur = td0.elapsed();
        if added > 0 {
            self.ocr_row_count
                .fetch_add(added as u64, Ordering::Relaxed);
        }

        let total_dur = fn_start.elapsed();
        if total_dur.as_secs() >= 5 {
            tracing::warn!(
                "[DIAG:DB] commit_screenshot id={} ocr_count={} total={:?} (mutex_wait={:?}, encrypt={:?}, db_insert={:?}, bitmap={:?})",
                screenshot_id, ocr_count, total_dur, mutex_wait_dur, total_encrypt_dur, total_db_insert_dur, total_bitmap_dur
            );
        }

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(final_image_path_str),
            added,
            skipped,
        })
    }

    /// Update the category of a screenshot.
    #[cfg(test)]
    pub fn update_screenshot_category(
        &self,
        screenshot_id: i64,
        category: &str,
        category_confidence: Option<f64>,
    ) -> Result<bool, String> {
        let mut guard = self.get_connection_named("update_screenshot_category")?;
        let conn = guard.as_mut().unwrap();

        let rows = conn
            .execute(
                "UPDATE screenshots SET category = ?, category_confidence = ? WHERE id = ? AND is_deleted = 0",
                params![category, category_confidence, screenshot_id],
            )
            .map_err(|e| format!("Failed to update category: {}", e))?;

        Ok(rows > 0)
    }

    /// Get distinct categories from the screenshots table (does not require Python).
    ///
    /// Depends on `idx_screenshots_deleted_category` to stay cheap. Without an
    /// index carrying `category` the planner walks
    /// `idx_screenshots_deleted_created_at` and fetches every matching row out
    /// of a multi-gigabyte table just to read one short column, then sorts
    /// through a temp B-tree — 2.8 s against a 54k-screenshot corpus, all of it
    /// under the DB mutex, to return eleven strings. With the index the whole
    /// statement is one covering scan.
    pub fn get_categories_from_db(&self) -> Result<Vec<String>, String> {
        let guard = self.get_connection_named("get_categories_from_db")?;
        let conn = guard.as_ref().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT category FROM screenshots WHERE is_deleted = 0 AND category IS NOT NULL AND category != '' ORDER BY category",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let categories: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(categories)
    }

    /// Batch get categories by image hashes. Returns a map of image_hash -> category.
    pub fn batch_get_categories_by_hash(
        &self,
        image_hashes: &[String],
    ) -> Result<std::collections::HashMap<String, Option<String>>, String> {
        if image_hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let guard = self.get_connection_named("batch_get_categories_by_hash")?;
        let conn = guard.as_ref().unwrap();

        let mut result_map = std::collections::HashMap::new();

        // Process in chunks to avoid SQL param limits
        for chunk in image_hashes.chunks(500) {
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT image_hash, category FROM screenshots WHERE is_deleted = 0 AND image_hash IN ({})",
                placeholders.join(",")
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|h| h as &dyn rusqlite::ToSql).collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare batch query: {}", e))?;

            let rows = stmt
                .query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|e| format!("Failed to execute batch query: {}", e))?;

            for row in rows.filter_map(|r| r.ok()) {
                result_map.insert(row.0, row.1);
            }
        }

        Ok(result_map)
    }

    /// Abort pending screenshot: delete encrypted file and mark DB record as aborted.
    pub fn abort_screenshot(
        &self,
        screenshot_id: i64,
        _reason: Option<&str>,
    ) -> Result<SaveScreenshotResponse, String> {
        let mut guard = self.get_connection_named("abort_screenshot")?;
        let conn = guard.as_mut().unwrap();

        let rec: Option<String> = conn
            .query_row(
                "SELECT image_path FROM screenshots WHERE id = ? AND is_deleted = 0",
                params![screenshot_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to query screenshot: {}", e))?;

        if rec.is_none() {
            return Err("Screenshot not found".to_string());
        }

        let image_path_str = rec.unwrap();
        let image_path = self.resolve_image_path(&image_path_str);

        if image_path.exists() {
            let _ = std::fs::remove_file(&image_path);
        }

        // Also remove the thumbnail (generated at the non-.pending path)
        let final_path = {
            let fname = image_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if fname.ends_with(".pending") {
                image_path.with_file_name(fname.trim_end_matches(".pending"))
            } else {
                image_path.clone()
            }
        };
        let thumb_path = Self::thumbnail_path_for(&final_path);
        if thumb_path.exists() {
            let _ = std::fs::remove_file(&thumb_path);
        }

        // Mark as aborted
        conn.execute(
            "UPDATE screenshots SET status = ? WHERE id = ? AND is_deleted = 0",
            params!["aborted", screenshot_id],
        )
        .map_err(|e| format!("Failed to mark screenshot aborted: {}", e))?;

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added: 0,
            skipped: 0,
        })
    }

    /// Abort stale pending screenshots during monitor startup.
    ///
    /// The caller supplies a cancellation check so startup recovery can stop as
    /// soon as fresh capture work begins.
    /// `idx_screenshots_startup_pending` covers the lookup and its ordering;
    /// scanning historical rows here would hold the shared DB mutex for seconds
    /// even when there are no pending screenshots.
    pub fn abort_startup_pending_screenshots<F>(
        &self,
        mut should_cancel: F,
    ) -> Result<usize, String>
    where
        F: FnMut() -> bool,
    {
        if should_cancel() {
            return Ok(0);
        }
        let pending_ids: Vec<i64> = {
            let guard = self.get_connection_named("list_startup_pending_screenshots")?;
            // Cancellation may have arrived while waiting for the connection.
            if should_cancel() {
                return Ok(0);
            }
            let conn = guard.as_ref().unwrap();
            let mut stmt = conn
                .prepare(STARTUP_PENDING_SCREENSHOTS_SQL)
                .map_err(|e| format!("Failed to prepare pending screenshot query: {}", e))?;
            let ids: Vec<i64> = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| format!("Failed to query pending screenshots: {}", e))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("Failed to read pending screenshot IDs: {}", e))?;
            ids
        };

        let mut aborted = 0usize;
        for screenshot_id in pending_ids {
            if should_cancel() {
                tracing::info!(
                    "[DIAG:STARTUP] pending screenshot cleanup cancelled after {} aborts",
                    aborted
                );
                break;
            }
            match self.abort_screenshot(
                screenshot_id,
                Some("startup recovery: stale pending screenshot"),
            ) {
                Ok(_) => aborted += 1,
                Err(e) => tracing::warn!(
                    "[DIAG:STARTUP] failed to abort pending screenshot {}: {}",
                    screenshot_id,
                    e
                ),
            }
        }

        Ok(aborted)
    }

    /// Rounds a raw bucket width in seconds up to the next standardized step.
    pub(super) fn snap_bucket_seconds(raw_seconds: f64) -> i64 {
        const NICE_SECONDS: [i64; 21] = [
            1, 2, 5, 10, 15, 30, // seconds
            60, 120, 300, 600, 900, 1800, // minutes
            3600, 7200, 10800, 21600, 43200, // hours
            86400, 172800, 604800, 2592000, // days and up
        ];
        let raw = raw_seconds.ceil().max(1.0) as i64;
        NICE_SECONDS
            .iter()
            .copied()
            .find(|&step| step >= raw)
            .unwrap_or(NICE_SECONDS[NICE_SECONDS.len() - 1])
    }

    pub fn count_active_screenshots(&self) -> Result<i64, String> {
        self.with_read_connection_named("count_active_screenshots", |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM screenshots WHERE is_deleted = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| format!("Failed to count active screenshots: {}", e))
        })
    }

    pub fn count_active_ocr_rows(&self) -> Result<i64, String> {
        self.with_read_connection_named("count_active_ocr_rows", |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE is_deleted = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| format!("Failed to count active OCR rows: {}", e))
        })
    }

    /// Estimated expected row count for the derived CLIP image index.
    ///
    /// A vector row is expected for every DISTINCT `image_hash` among
    /// non-deleted screenshots that have at least one active OCR row.
    /// Distinct-hash because the image index is keyed by image hash, so
    /// duplicate captures share one row.
    pub fn count_expected_clip_image_rows(&self) -> Result<i64, String> {
        let guard = self.get_connection_named("count_expected_clip_image_rows")?;
        let conn = guard
            .as_ref()
            .ok_or_else(|| "Database connection is None".to_string())?;
        conn.query_row(
            "SELECT COUNT(DISTINCT s.image_hash) FROM screenshots s
             WHERE s.is_deleted = 0
               AND EXISTS (
                   SELECT 1 FROM ocr_results o
                    WHERE o.screenshot_id = s.id AND o.is_deleted = 0
               )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| format!("Failed to count expected CLIP image rows: {}", e))
    }

    /// Active screenshot ids for each of `image_hashes`.
    ///
    /// The CLIP index is keyed by image hash, while everything that can answer
    /// "does this image carry text?" is keyed by screenshot id. In the shipped
    /// schema `image_hash` is UNIQUE, so each hash resolves to at most one live
    /// screenshot; the return type is a list because the caller should not have
    /// to depend on that constraint, and the rows are ordered newest-first so a
    /// caller that wants one representative gets a defined answer either way.
    pub(crate) fn map_image_hashes_to_screenshot_ids(
        &self,
        image_hashes: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<i64>>, String> {
        use std::collections::HashMap;
        if image_hashes.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_vector_scan_connection("map_image_hashes_to_screenshot_ids", |conn| {
            let mut mapped: HashMap<String, Vec<i64>> = HashMap::new();
            for chunk in image_hashes.chunks(500) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let mut statement = conn
                    .prepare(&format!(
                        "SELECT image_hash, id FROM screenshots
                         WHERE is_deleted = 0 AND image_hash IN ({placeholders})
                         ORDER BY created_at DESC, id DESC"
                    ))
                    .map_err(|e| format!("Failed to prepare the image hash mapping: {}", e))?;
                let rows = statement
                    .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|e| format!("Failed to map image hashes: {}", e))?;
                for row in rows {
                    let (hash, id) =
                        row.map_err(|e| format!("Failed to read a mapped row: {}", e))?;
                    mapped.entry(hash).or_default().push(id);
                }
            }
            Ok(mapped)
        })
    }

    /// Resolve ranking/filter fields without private-key work. Process-filtered
    /// CLIP queries still use decrypted summaries; their encrypted process name
    /// remains authoritative even for legacy rows with inconsistent plaintext.
    pub(crate) fn clip_candidate_metadata(
        &self,
        hashes: &[String],
        deadline: Instant,
    ) -> Result<std::collections::HashMap<String, BackgroundScreenshotSummary>, String> {
        check_read_deadline(Some(deadline)).map_err(|error| error.to_string())?;
        if hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let result = self.with_vector_scan_connection("clip_candidate_metadata", |conn| {
            let _deadline = super::connection::QueryDeadline::install(conn, Some(deadline))
                .map_err(|error| error.to_string())?;
            let mut result = std::collections::HashMap::new();
            for chunk in hashes.chunks(500) {
                check_read_deadline(Some(deadline)).map_err(|error| error.to_string())?;
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT image_hash, id, CAST(strftime('%s', created_at) AS INTEGER), process_name, category
                     FROM screenshots WHERE is_deleted = 0 AND image_hash IN ({placeholders})
                     ORDER BY created_at DESC, id DESC"
                );
                let mut statement = conn.prepare(&sql).map_err(|error| error.to_string())?;
                let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, BackgroundScreenshotSummary {
                        id: row.get(1)?, timestamp: row.get(2)?, process_name: row.get(3)?,
                        category: row.get(4)?, window_title: None,
                    }))
                }).map_err(|error| error.to_string())?;
                for row in rows {
                    let (hash, metadata) = row.map_err(|error| error.to_string())?;
                    result.entry(hash).or_insert(metadata);
                }
            }
            check_read_deadline(Some(deadline)).map_err(|error| error.to_string())?;
            Ok(result)
        });
        check_read_deadline(Some(deadline)).map_err(|error| error.to_string())?;
        result
    }

    pub fn get_index_storage_stats(&self) -> Result<IndexStorageStats, String> {
        Ok(IndexStorageStats {
            screenshots_count: self.count_active_screenshots()?,
            ocr_rows_count: self.count_active_ocr_rows()?,
            semantic_text_rows: self
                .count_query_visible_embeddings(DerivedIndexKind::SemanticText)?,
            clip_image_rows: self.count_query_visible_embeddings(DerivedIndexKind::ClipImage)?,
            semantic_index_backlog: self.derived_index_backlog(
                DerivedIndexKind::SemanticText,
                crate::minilm_index::MAX_ATTEMPTS,
            )?,
            clip_index_backlog: self.derived_index_backlog(
                DerivedIndexKind::ClipImage,
                crate::clip_index::MAX_ATTEMPTS,
            )?,
            smart_cluster_pending_count: self.count_smart_cluster_pending()?,
            delete_queue: self.get_delete_queue_status()?,
        })
    }

    /// Get screenshot density (counts per time bucket) within a time range.
    /// No decryption or joins - extremely fast index-only scan.
    ///
    /// `bucket_offset_seconds` shifts bucket boundaries to align with caller's timezone offset.
    pub fn get_screenshot_density(
        &self,
        start_ts: f64,
        end_ts: f64,
        bucket_seconds: i64,
        bucket_offset_seconds: i64,
    ) -> Result<Vec<DensityBucket>, String> {
        let conn = self.open_read_connection_named("get_screenshot_density")?;

        let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        // Normalize offset within bucket duration
        let offset = bucket_offset_seconds.rem_euclid(bucket_seconds.max(1));

        let sql = format!(
            "SELECT ((CAST(strftime('%s', created_at) AS INTEGER) + {off}) / {bs}) * {bs} - {off} AS bucket, \
                    COUNT(*) AS cnt \
             FROM screenshots \
             WHERE is_deleted = 0 AND created_at BETWEEN '{start}' AND '{end}' \
             GROUP BY bucket \
             ORDER BY bucket",
            bs = bucket_seconds,
            off = offset,
            start = start_dt,
            end = end_dt
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare density query: {}", e))?;

        let rows: Vec<DensityBucket> = stmt
            .query_map([], |row| {
                Ok(DensityBucket {
                    timestamp: row.get(0)?,
                    count: row.get(1)?,
                })
            })
            .map_err(|e| format!("Failed to execute density query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Decrypts one summary row with an injected row-key unwrap so batch
    /// callers borrow the authenticated [`CngKeySession`] across worker threads.
    fn decrypt_screenshot_summary_with_unwrap(
        row: EncryptedScreenshotSummaryRow,
        unwrap_row_key: &dyn Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
        is_authorized: &dyn Fn() -> bool,
    ) -> Result<BackgroundScreenshotSummary, BackgroundReadError> {
        let needs_row_key = row.window_title_enc.is_some() || row.process_name_enc.is_some();
        let mut row_key = if needs_row_key {
            if !is_authorized() {
                return Err(BackgroundReadError::AuthRequired);
            }
            let encrypted_key = row.content_key_enc.as_deref().ok_or_else(|| {
                BackgroundReadError::Other(format!(
                    "Missing content key for screenshot summary id={}",
                    row.id
                ))
            })?;
            Some(unwrap_row_key(encrypted_key).map_err(|error| match error {
                CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
                other => BackgroundReadError::Other(format!(
                    "Failed to unwrap screenshot summary key id={}: {}",
                    row.id, other
                )),
            })?)
        } else {
            None
        };

        let window_title_result = match (row.window_title_enc.as_deref(), row_key.as_deref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .map_err(|error| {
                    BackgroundReadError::Other(format!(
                        "Failed to decrypt window title id={}: {}",
                        row.id, error
                    ))
                })
                .and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|error| {
                        BackgroundReadError::Other(format!(
                            "Invalid window title encoding id={}: {}",
                            row.id, error
                        ))
                    })
                })
                .map(Some),
            _ => Ok(row.window_title_plain),
        };
        let process_name_result = match (row.process_name_enc.as_deref(), row_key.as_deref()) {
            (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                .map_err(|error| {
                    BackgroundReadError::Other(format!(
                        "Failed to decrypt process name id={}: {}",
                        row.id, error
                    ))
                })
                .and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|error| {
                        BackgroundReadError::Other(format!(
                            "Invalid process name encoding id={}: {}",
                            row.id, error
                        ))
                    })
                })
                .map(Some),
            _ => Ok(row.process_name_plain),
        };

        if let Some(ref mut key) = row_key {
            Self::zeroize_bytes(key);
        }

        Ok(BackgroundScreenshotSummary {
            id: row.id,
            window_title: window_title_result?,
            process_name: process_name_result?,
            timestamp: row.timestamp,
            category: row.category,
        })
    }

    /// Decrypts summary rows in parallel with one CNG session per worker
    /// thread, preserving input order.
    fn decrypt_summaries_parallel(
        &self,
        raw_rows: Vec<EncryptedScreenshotSummaryRow>,
        deadline: Option<Instant>,
    ) -> Result<Vec<BackgroundScreenshotSummary>, BackgroundReadError> {
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        check_read_deadline(deadline)?;
        unwrap_batch_parallel(&self.credential_state, raw_rows, |session, row| {
            if !self.is_silent_read_authorized() {
                return Err(BackgroundReadError::AuthRequired);
            }
            check_read_deadline(deadline)?;
            Self::decrypt_screenshot_summary_with_unwrap(
                row,
                &|ciphertext| session.unwrap_row_key(ciphertext),
                &|| self.is_silent_read_authorized(),
            )
        })
    }

    /// Fetch only the metadata required by background semantic processing.
    /// CNG is always called with `NCRYPT_SILENT_FLAG`; a locked session fails
    /// the whole batch with `AuthRequired` instead of displaying system UI.
    pub(crate) fn get_screenshot_summaries_by_ids_silent(
        &self,
        ids: &[i64],
    ) -> Result<Vec<BackgroundScreenshotSummary>, BackgroundReadError> {
        self.get_screenshot_summaries_by_ids_silent_with_deadline(ids, None)
    }

    pub(crate) fn get_screenshot_summaries_by_ids_silent_with_deadline(
        &self,
        ids: &[i64],
        deadline: Option<Instant>,
    ) -> Result<Vec<BackgroundScreenshotSummary>, BackgroundReadError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        check_read_deadline(deadline)?;

        let raw_rows = {
            let conn = self
                .open_read_connection_named("get_screenshot_summaries_by_ids_silent")
                .map_err(BackgroundReadError::Other)?;
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT id, window_title, process_name, window_title_enc,
                        process_name_enc, content_key_encrypted,
                        strftime('%s', created_at) AS timestamp, category
                 FROM screenshots
                 WHERE is_deleted = 0 AND id IN ({})",
                placeholders
            );
            let id_params: Vec<&dyn rusqlite::ToSql> =
                ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            let mut stmt = conn.prepare(&sql).map_err(|error| {
                BackgroundReadError::Other(format!(
                    "Failed to prepare background screenshot query: {}",
                    error
                ))
            })?;
            let rows = stmt
                .query_map(
                    id_params.as_slice(),
                    EncryptedScreenshotSummaryRow::from_row,
                )
                .map_err(|error| {
                    BackgroundReadError::Other(format!(
                        "Failed to execute background screenshot query: {}",
                        error
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    BackgroundReadError::Other(format!(
                        "Failed to read background screenshot row: {}",
                        error
                    ))
                })?;
            rows
        };

        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        self.decrypt_summaries_parallel(raw_rows, deadline)
    }

    /// Get OCR results for a screenshot.
    pub fn get_screenshot_ocr_results(
        &self,
        screenshot_id: i64,
    ) -> Result<Vec<super::OcrResult>, String> {
        let encrypted_rows = {
            let conn = self.open_read_connection_named("get_screenshot_ocr_results")?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, screenshot_id, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4, created_at
                 FROM ocr_results WHERE screenshot_id = ? AND is_deleted = 0
                 ORDER BY box_y1, box_x1, id",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;
            let rows = stmt
                .query_map([screenshot_id], EncryptedOcrResultRow::from_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|row| row.ok())
                .collect::<Vec<_>>();
            rows
        };

        // CNG decryption can be slow or prompt the user. Close the SQLite
        // cursor and read connection before starting it.
        Ok(encrypted_rows
            .into_iter()
            .map(|row| self.decrypt_ocr_row(row))
            .collect())
    }

    fn decrypt_ocr_row(&self, row: EncryptedOcrResultRow) -> super::OcrResult {
        let text = match (row.text_enc.as_deref(), row.text_key_encrypted.as_deref()) {
            (Some(data), Some(key)) => self
                .decrypt_payload_with_row_key(data, key)
                .ok()
                .and_then(|v| String::from_utf8(v).ok()),
            _ => None,
        };

        super::OcrResult {
            id: row.id,
            screenshot_id: row.screenshot_id,
            text: text.unwrap_or_default(),
            confidence: row.confidence,
            box_coords: row.box_coords,
            created_at: wire_time::from_sqlite_utc(&row.created_at),
        }
    }

    /// OCR blocks of several screenshots with their boxes, in the reading order
    /// of [`Self::get_screenshot_ocr_results`]. Used where a filter must drop
    /// single segments rather than a whole joined text.
    pub fn get_ocr_blocks_by_screenshot_ids(
        &self,
        screenshot_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<super::OcrResult>>, String> {
        let mut blocks: std::collections::HashMap<i64, Vec<super::OcrResult>> =
            std::collections::HashMap::new();
        if screenshot_ids.is_empty() {
            return Ok(blocks);
        }
        let encrypted_rows = {
            let conn = self.open_read_connection_named("get_ocr_blocks_by_screenshot_ids")?;
            let mut rows = Vec::new();
            // Chunked to stay under SQLite's bound-parameter limit.
            for chunk in screenshot_ids.chunks(500) {
                let placeholders = vec!["?"; chunk.len()].join(",");
                let sql = format!(
                    "SELECT id, screenshot_id, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4, created_at
                     FROM ocr_results
                     WHERE is_deleted = 0 AND screenshot_id IN ({placeholders})
                     ORDER BY screenshot_id, box_y1, box_x1, id"
                );
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                let mut stmt = conn
                    .prepare(&sql)
                    .map_err(|e| format!("Failed to prepare batch OCR block query: {}", e))?;
                let mapped = stmt
                    .query_map(params.as_slice(), EncryptedOcrResultRow::from_row)
                    .map_err(|e| format!("Failed to execute batch OCR block query: {}", e))?;
                rows.extend(mapped.filter_map(|row| row.ok()));
            }
            rows
        };

        // As above, decrypt only after the read connection is closed.
        for row in encrypted_rows {
            let block = self.decrypt_ocr_row(row);
            blocks.entry(block.screenshot_id).or_default().push(block);
        }
        Ok(blocks)
    }

    /// Get OCR results for unattended recovery without allowing CNG to display UI.
    /// Encrypted rows are loaded while holding the DB lock, then decrypted after
    /// the lock is released so an authentication deferral cannot block storage.
    pub(crate) fn get_screenshot_ocr_results_silent(
        &self,
        screenshot_id: i64,
    ) -> Result<Vec<super::OcrResult>, BackgroundReadError> {
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        let encrypted_rows = {
            let guard = self
                .get_connection_named("get_screenshot_ocr_results_silent")
                .map_err(BackgroundReadError::Other)?;
            let conn = guard.as_ref().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT id, screenshot_id, text_enc, text_key_encrypted, confidence,
                            box_x1, box_y1, box_x2, box_y2,
                            box_x3, box_y3, box_x4, box_y4, created_at
                     FROM ocr_results WHERE screenshot_id = ? AND is_deleted = 0
                     ORDER BY box_y1, box_x1",
                )
                .map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to prepare query: {}", e))
                })?;

            let rows = stmt
                .query_map([screenshot_id], EncryptedOcrResultRow::from_row)
                .map_err(|e| BackgroundReadError::Other(format!("Failed to execute query: {}", e)))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to read OCR row: {}", e))
                })?;
            rows
        };

        encrypted_rows
            .into_iter()
            .map(|row| {
                if !self.is_silent_read_authorized() {
                    return Err(BackgroundReadError::AuthRequired);
                }
                let text = match (row.text_enc.as_deref(), row.text_key_encrypted.as_deref()) {
                    (Some(data), Some(key)) => {
                        String::from_utf8(self.decrypt_payload_with_row_key_silent(data, key)?)
                            .map_err(|e| {
                                BackgroundReadError::Other(format!(
                                    "Invalid OCR text encoding: {}",
                                    e
                                ))
                            })?
                    }
                    _ => String::new(),
                };
                Ok(super::OcrResult {
                    id: row.id,
                    screenshot_id: row.screenshot_id,
                    text,
                    confidence: row.confidence,
                    box_coords: row.box_coords,
                    created_at: wire_time::from_sqlite_utc(&row.created_at),
                })
            })
            .collect()
    }

    /// Get combined OCR texts for a batch of screenshot IDs in a single query.
    /// Returns a map of screenshot_id -> joined OCR text.
    pub fn get_ocr_results_by_screenshot_ids(
        &self,
        screenshot_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, String>, String> {
        self.get_ocr_results_by_screenshot_ids_with_mode(screenshot_ids, false)
            .map_err(|error| error.to_string())
    }

    /// Batch OCR text for MiniLM source fingerprints. Per screenshot only the
    /// first `min_chars` joined characters are guaranteed — the MiniLM source
    /// contract truncates OCR input to a fixed prefix — which lets the
    /// decrypt loop skip the long tail of boxes: most screenshots reach the
    /// budget within a few boxes instead of paying one RSA unwrap per box.
    /// Screenshots are decrypted in parallel with per-thread CNG sessions.
    /// The prefix replays the boxes in insertion order so it reproduces the
    /// text that was actually embedded; see [`ocr_text_prefix_query`].
    /// Every requested id is present in the result (empty string when the
    /// screenshot has no decryptable text).
    pub(crate) fn get_ocr_text_prefixes_by_screenshot_ids_silent(
        &self,
        screenshot_ids: &[i64],
        min_chars: usize,
    ) -> Result<std::collections::HashMap<i64, String>, BackgroundReadError> {
        self.get_ocr_text_prefixes_by_screenshot_ids_silent_with_deadline(
            screenshot_ids,
            min_chars,
            None,
        )
    }

    /// Query reads stop between OCR boxes when their caller's budget expires.
    /// Dropping a spawn_blocking JoinHandle alone cannot stop CNG work; without
    /// this check an expired search continues contending with the next search.
    pub(crate) fn get_ocr_text_prefixes_by_screenshot_ids_silent_with_deadline(
        &self,
        screenshot_ids: &[i64],
        min_chars: usize,
        deadline: Option<Instant>,
    ) -> Result<std::collections::HashMap<i64, String>, BackgroundReadError> {
        if screenshot_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        check_read_deadline(deadline)?;

        // Phase 1: fetch the raw encrypted rows with a read-only connection.
        let raw_rows = {
            let conn = self
                .open_read_connection_named("get_ocr_text_prefixes_by_screenshot_ids_silent")
                .map_err(BackgroundReadError::Other)?;

            let mut raw_rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)> = Vec::new();

            // Process in chunks to avoid SQLite parameter limit (usually 999)
            for chunk in screenshot_ids.chunks(500) {
                check_read_deadline(deadline)?;
                let sql = ocr_text_prefix_query(chunk.len());
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

                let mut stmt = conn.prepare(&sql).map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to prepare OCR prefix query: {}", e))
                })?;

                let rows = stmt
                    .query_map(params.as_slice(), |row| {
                        let screenshot_id: i64 = row.get(0)?;
                        let text_enc: Option<Vec<u8>> = row.get(1)?;
                        let text_key_enc: Option<Vec<u8>> = row.get(2)?;
                        Ok((screenshot_id, text_enc, text_key_enc))
                    })
                    .map_err(|e| {
                        BackgroundReadError::Other(format!(
                            "Failed to execute OCR prefix query: {}",
                            e
                        ))
                    })?;

                for row in rows.filter_map(|r| r.ok()) {
                    check_read_deadline(deadline)?;
                    raw_rows.push(row);
                }
            }
            raw_rows
        };

        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        check_read_deadline(deadline)?;

        // Phase 2: group boxes per screenshot. Each id lives in exactly one
        // IN chunk and every chunk is ordered by screenshot_id, so the rows
        // of one screenshot are contiguous and in insertion order — the exact
        // order the OCR engine emitted the boxes, which is the order their
        // texts were joined into the embedded MiniLM source string.
        let mut groups: Vec<Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)>> = Vec::new();
        for row in raw_rows {
            match groups.last_mut() {
                Some(group) if group.last().map(|(id, _, _)| *id) == Some(row.0) => group.push(row),
                _ => groups.push(vec![row]),
            }
        }

        // Phase 3: decrypt each screenshot's prefix in parallel.
        let maps = unwrap_batch_parallel(&self.credential_state, groups, |session, group| {
            assemble_ocr_text_prefixes(group, min_chars, deadline, |data, key| {
                if !self.is_silent_read_authorized() {
                    return Err(BackgroundReadError::AuthRequired);
                }
                let bytes = Self::decrypt_payload_with_unwrap(data, key, &|ciphertext| {
                    session.unwrap_row_key(ciphertext)
                })?;
                String::from_utf8(bytes).map_err(|error| {
                    BackgroundReadError::Other(format!("Invalid OCR text encoding: {}", error))
                })
            })
        })?;

        check_read_deadline(deadline)?;
        let mut result: std::collections::HashMap<i64, String> = screenshot_ids
            .iter()
            .map(|&id| (id, String::new()))
            .collect();
        for map in maps {
            for (id, text) in map {
                result.insert(id, text);
            }
        }
        Ok(result)
    }

    fn get_ocr_results_by_screenshot_ids_with_mode(
        &self,
        screenshot_ids: &[i64],
        silent: bool,
    ) -> Result<std::collections::HashMap<i64, String>, BackgroundReadError> {
        if screenshot_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        // Phase 1: Use an independent read-only connection to retrieve the raw encrypted rows.
        let raw_rows = {
            let conn = self
                .open_read_connection_named(if silent {
                    "get_ocr_results_by_screenshot_ids_silent"
                } else {
                    "get_ocr_results_by_screenshot_ids"
                })
                .map_err(BackgroundReadError::Other)?;

            let mut raw_rows = Vec::new();

            // Process in chunks to avoid SQLite parameter limit (usually 999)
            for chunk in screenshot_ids.chunks(500) {
                let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
                let sql = format!(
                    "SELECT screenshot_id, text_enc, text_key_encrypted
                     FROM ocr_results
                     WHERE is_deleted = 0 AND screenshot_id IN ({})
                     ORDER BY screenshot_id, box_y1, box_x1",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

                let mut stmt = conn.prepare(&sql).map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to prepare batch OCR query: {}", e))
                })?;

                let rows = stmt
                    .query_map(params.as_slice(), |row| {
                        let screenshot_id: i64 = row.get(0)?;
                        let text_enc: Option<Vec<u8>> = row.get(1)?;
                        let text_key_enc: Option<Vec<u8>> = row.get(2)?;
                        Ok((screenshot_id, text_enc, text_key_enc))
                    })
                    .map_err(|e| {
                        BackgroundReadError::Other(format!(
                            "Failed to execute batch OCR query: {}",
                            e
                        ))
                    })?;

                for row in rows.filter_map(|r| r.ok()) {
                    raw_rows.push(row);
                }
            }
            raw_rows
        };

        // Phase 2: Decrypt payloads and build the final map outside the DB lock.
        let mut result_map = std::collections::HashMap::new();

        // Initialize empty lists for all requested IDs, so they are represented.
        for &id in screenshot_ids {
            result_map.insert(id, Vec::new());
        }

        for (screenshot_id, text_enc, text_key_enc) in raw_rows {
            if silent && !self.is_silent_read_authorized() {
                return Err(BackgroundReadError::AuthRequired);
            }
            let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                (Some(data), Some(key)) if silent => Some(
                    String::from_utf8(self.decrypt_payload_with_row_key_silent(data, key)?)
                        .map_err(|error| {
                            BackgroundReadError::Other(format!(
                                "Invalid OCR text encoding for screenshot id={}: {}",
                                screenshot_id, error
                            ))
                        })?,
                ),
                (Some(data), Some(key)) => self
                    .decrypt_payload_with_row_key(data, key)
                    .ok()
                    .and_then(|v| String::from_utf8(v).ok()),
                _ => None,
            };
            if let Some(text_str) = text {
                if !text_str.is_empty() {
                    if let Some(vec) = result_map.get_mut(&screenshot_id) {
                        vec.push(text_str);
                    }
                }
            }
        }

        // Map the collected vectors of strings to joined strings
        let final_map = result_map
            .into_iter()
            .map(|(id, vec)| (id, vec.join(" ")))
            .collect();

        Ok(final_map)
    }

    /// Queue a screenshot for safe physical deletion by ID.
    pub fn delete_screenshot(&self, id: i64) -> Result<bool, String> {
        let result = self.soft_delete_screenshots(&[id])?;
        Ok(result.screenshots_marked > 0)
    }

    /// Queue screenshots within a time range for safe physical deletion.
    pub fn delete_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<i32, String> {
        // Convert timestamps (milliseconds) to SQLite datetime
        let start_dt = DateTime::<Utc>::from_timestamp((start_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp((end_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        let ids: Vec<i64> = {
            let guard = self.get_connection_named("delete_screenshots_by_time_range")?;
            let conn = guard.as_ref().unwrap();

            let mut stmt = conn
                .prepare(
                    "SELECT id FROM screenshots
                     WHERE is_deleted = 0 AND created_at BETWEEN ?1 AND ?2
                     ORDER BY id ASC",
                )
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows = stmt
                .query_map([&start_dt, &end_dt], |row| row.get(0))
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to decode screenshot id: {}", e))?;
            rows
        };

        let result = self.soft_delete_screenshots(&ids)?;
        Ok(result.screenshots_marked.clamp(0, i32::MAX as i64) as i32)
    }

    /// Soft-delete screenshots by process and optional local month. A month
    /// requires the named time zone returned with its thumbnail page.
    pub fn soft_delete_process_month(
        &self,
        process_name: &str,
        month: Option<&str>,
        time_zone: Option<&str>,
    ) -> Result<SoftDeleteResult, String> {
        let normalized_process = process_name.trim();
        if normalized_process.is_empty() {
            return Err("process_name is required".to_string());
        }

        let normalized_month = month.map(|m| m.trim().to_string());

        let mut filter_plain = String::from("process_name = ?1 AND is_deleted = 0");
        let mut filter_alias = String::from("s.process_name = ?1 AND s.is_deleted = 0");
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(normalized_process.to_string())];

        if let Some(ref month_key) = normalized_month {
            let zone =
                crate::calendar::time_zone(Some(time_zone.ok_or("CALENDAR_TIME_ZONE_REQUIRED")?))?;
            let (start_ms, end_ms) = crate::calendar::month_bounds(month_key, zone)?;
            filter_plain.push_str(" AND created_at >= ?2 AND created_at < ?3");
            filter_alias.push_str(" AND s.created_at >= ?2 AND s.created_at < ?3");
            param_values.push(Box::new(crate::calendar::sqlite_utc(start_ms)?));
            param_values.push(Box::new(crate::calendar::sqlite_utc(end_ms)?));
        }

        let params_ref: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let mut guard = self.get_connection_named("soft_delete_process_month")?;
        let conn = guard.as_mut().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start soft-delete transaction: {}", e))?;

        let queue_screenshots_sql = format!(
            "INSERT OR IGNORE INTO delete_queue_screenshots (id) SELECT id FROM screenshots WHERE {}",
            filter_plain
        );
        let queued_screenshots =
            tx.execute(&queue_screenshots_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to queue screenshots: {}", e))? as i64;

        let queue_ocr_sql = format!(
            "INSERT OR IGNORE INTO delete_queue_ocr (id)
             SELECT o.id
             FROM ocr_results o
             JOIN screenshots s ON s.id = o.screenshot_id
             WHERE o.is_deleted = 0 AND {}",
            filter_alias
        );
        let queued_ocr =
            tx.execute(&queue_ocr_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to queue OCR rows: {}", e))? as i64;

        let mark_ocr_sql = format!(
            "UPDATE ocr_results
             SET is_deleted = 1
             WHERE is_deleted = 0
               AND screenshot_id IN (SELECT id FROM screenshots WHERE {})",
            filter_plain
        );
        let ocr_marked =
            tx.execute(&mark_ocr_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to mark OCR rows deleted: {}", e))? as i64;

        let mark_screenshots_sql = format!(
            "UPDATE screenshots SET is_deleted = 1 WHERE {}",
            filter_plain
        );
        let screenshots_marked = tx
            .execute(&mark_screenshots_sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to mark screenshots deleted: {}", e))?
            as i64;

        tx.commit()
            .map_err(|e| format!("Failed to commit soft-delete transaction: {}", e))?;
        drop(guard);
        self.finish_staged_deletions()?;

        if ocr_marked > 0 {
            let _ = self
                .ocr_row_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(ocr_marked as u64))
                });
        }

        Ok(SoftDeleteResult {
            process_name: normalized_process.to_string(),
            month: normalized_month,
            screenshots_marked,
            ocr_marked,
            queued_screenshots,
            queued_ocr,
        })
    }

    /// Soft-delete selected screenshots by IDs and enqueue physical cleanup.
    pub fn soft_delete_screenshots(
        &self,
        screenshot_ids: &[i64],
    ) -> Result<SoftDeleteScreenshotsResult, String> {
        let mut normalized_ids: Vec<i64> = screenshot_ids
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .collect();
        normalized_ids.sort_unstable();
        normalized_ids.dedup();

        if normalized_ids.is_empty() {
            return Ok(SoftDeleteScreenshotsResult {
                requested: 0,
                screenshots_marked: 0,
                ocr_marked: 0,
                queued_screenshots: 0,
                queued_ocr: 0,
            });
        }

        let mut guard = self.get_connection_named("soft_delete_screenshots")?;
        let conn = guard.as_mut().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start screenshot soft-delete transaction: {}", e))?;

        let mut queued_screenshots = 0i64;
        let mut queued_ocr = 0i64;
        let mut ocr_marked = 0i64;
        let mut screenshots_marked = 0i64;

        for chunk in normalized_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

            let queue_screenshot_sql = format!(
                "INSERT OR IGNORE INTO delete_queue_screenshots (id)
                 SELECT id FROM screenshots WHERE is_deleted = 0 AND id IN ({})",
                placeholders
            );
            queued_screenshots += tx
                .execute(&queue_screenshot_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to queue selected screenshots: {}", e))?
                as i64;

            let queue_ocr_sql = format!(
                "INSERT OR IGNORE INTO delete_queue_ocr (id)
                 SELECT o.id
                 FROM ocr_results o
                 JOIN screenshots s ON s.id = o.screenshot_id
                 WHERE o.is_deleted = 0
                   AND s.is_deleted = 0
                   AND s.id IN ({})",
                placeholders
            );
            queued_ocr += tx
                .execute(&queue_ocr_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to queue selected OCR rows: {}", e))?
                as i64;

            let mark_ocr_sql = format!(
                "UPDATE ocr_results SET is_deleted = 1
                 WHERE is_deleted = 0
                   AND screenshot_id IN ({})",
                placeholders
            );
            ocr_marked += tx
                .execute(&mark_ocr_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to mark selected OCR rows deleted: {}", e))?
                as i64;

            let mark_screenshots_sql = format!(
                "UPDATE screenshots SET is_deleted = 1
                 WHERE is_deleted = 0
                   AND id IN ({})",
                placeholders
            );
            screenshots_marked += tx
                .execute(&mark_screenshots_sql, params_ref.as_slice())
                .map_err(|e| format!("Failed to mark selected screenshots deleted: {}", e))?
                as i64;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit selected screenshot soft-delete: {}", e))?;
        drop(guard);
        self.finish_staged_deletions()?;

        if ocr_marked > 0 {
            let _ = self
                .ocr_row_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(ocr_marked as u64))
                });
        }

        Ok(SoftDeleteScreenshotsResult {
            requested: normalized_ids.len() as i64,
            screenshots_marked,
            ocr_marked,
            queued_screenshots,
            queued_ocr,
        })
    }

    /// Select oldest screenshots until the estimated reclaim size reaches target bytes.
    ///
    /// Returns `(ids, estimated_reclaim_bytes)`.
    pub fn select_oldest_screenshots_for_reclaim(
        &self,
        target_reclaim_bytes: u64,
        max_candidates: i64,
    ) -> Result<(Vec<i64>, u64), String> {
        if target_reclaim_bytes == 0 || max_candidates <= 0 {
            return Ok((Vec::new(), 0));
        }

        let safe_limit = max_candidates.clamp(1, 20_000);

        let rows: Vec<(i64, String)> = {
            let guard = self.get_connection_named("select_oldest_screenshots_for_reclaim")?;
            let conn = guard.as_ref().unwrap();

            let mut stmt = conn
                .prepare(
                    "SELECT id, image_path
                     FROM screenshots
                     WHERE is_deleted = 0
                     ORDER BY created_at ASC
                     LIMIT ?1",
                )
                .map_err(|e| format!("Failed to prepare reclaim candidate query: {}", e))?;

            let rows: Vec<(i64, String)> = stmt
                .query_map([safe_limit], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("Failed to load reclaim candidates: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            rows
        };

        let mut selected_ids = Vec::new();
        let mut estimated_reclaim_bytes = 0u64;
        let mut zombie_ids = Vec::new();

        for (id, image_path) in rows {
            let abs_path = self.resolve_image_path(&image_path);
            let image_bytes = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);
            let thumb_path = Self::thumbnail_path_for(&abs_path);
            let thumb_bytes = std::fs::metadata(&thumb_path).map(|m| m.len()).unwrap_or(0);
            let row_bytes = image_bytes.saturating_add(thumb_bytes);

            if row_bytes == 0 {
                zombie_ids.push(id);
                continue;
            }

            selected_ids.push(id);
            estimated_reclaim_bytes = estimated_reclaim_bytes.saturating_add(row_bytes);
            if estimated_reclaim_bytes >= target_reclaim_bytes {
                break;
            }
        }

        if !zombie_ids.is_empty() {
            match self.soft_delete_screenshots(&zombie_ids) {
                Ok(result) => {
                    tracing::info!(
                        "[POLICY] soft-deleted zombie screenshots: marked={}, queued_screenshots={}, queued_ocr={}",
                        result.screenshots_marked,
                        result.queued_screenshots,
                        result.queued_ocr
                    );
                }
                Err(e) => {
                    tracing::warn!("[POLICY] failed to soft-delete zombie screenshots: {}", e);
                }
            }
        }

        Ok((selected_ids, estimated_reclaim_bytes))
    }

    /// Select up to `max_candidates` non-deleted screenshots created strictly
    /// before `cutoff_dt` (UTC `%Y-%m-%d %H:%M:%S`, matching `created_at`),
    /// oldest first, for age-based retention pruning.
    ///
    /// Returns `(ids, estimated_freed_bytes)`. Unlike the reclaim selector this
    /// has no byte target — every row older than the cutoff is a candidate — and
    /// rows whose files are already gone are still returned so their database
    /// entries get cleared.
    pub fn select_screenshots_created_before(
        &self,
        cutoff_dt: &str,
        max_candidates: i64,
    ) -> Result<(Vec<i64>, u64), String> {
        if max_candidates <= 0 {
            return Ok((Vec::new(), 0));
        }

        let safe_limit = max_candidates.clamp(1, 20_000);

        let rows: Vec<(i64, String)> = {
            let guard = self.get_connection_named("select_screenshots_created_before")?;
            let conn = guard.as_ref().unwrap();

            let mut stmt = conn
                .prepare(
                    "SELECT id, image_path
                     FROM screenshots
                     WHERE is_deleted = 0 AND created_at < ?1
                     ORDER BY created_at ASC
                     LIMIT ?2",
                )
                .map_err(|e| format!("Failed to prepare retention candidate query: {}", e))?;

            let rows: Vec<(i64, String)> = stmt
                .query_map(params![cutoff_dt, safe_limit], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map_err(|e| format!("Failed to load retention candidates: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            rows
        };

        let mut ids = Vec::with_capacity(rows.len());
        let mut estimated_freed_bytes = 0u64;

        for (id, image_path) in rows {
            let abs_path = self.resolve_image_path(&image_path);
            let image_bytes = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);
            let thumb_path = Self::thumbnail_path_for(&abs_path);
            let thumb_bytes = std::fs::metadata(&thumb_path).map(|m| m.len()).unwrap_or(0);
            estimated_freed_bytes = estimated_freed_bytes
                .saturating_add(image_bytes)
                .saturating_add(thumb_bytes);
            ids.push(id);
        }

        Ok((ids, estimated_freed_bytes))
    }

    /// Fetch pending counts in delete queues.
    pub fn get_delete_queue_status(&self) -> Result<DeleteQueueStatus, String> {
        let guard = self.get_connection_named("get_delete_queue_status")?;
        let conn = guard.as_ref().unwrap();

        let pending_screenshots: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_screenshots", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("Failed to count screenshot queue: {}", e))?;
        let pending_ocr: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("Failed to count OCR queue: {}", e))?;

        Ok(DeleteQueueStatus {
            pending_screenshots,
            pending_ocr,
            running: pending_screenshots > 0 || pending_ocr > 0,
        })
    }

    /// Process one OCR delete queue batch and unlink each row from blind bitmap index.
    /// A non-empty batch returns `waiting_for_unlock` without mutation when
    /// protected reads are not silently authorized.
    pub fn process_ocr_delete_queue_batch(
        &self,
        batch_size: i64,
    ) -> Result<OcrDeleteBatchResult, String> {
        let safe_batch_size = batch_size.clamp(1, 2000);

        // Probe only queue metadata before reading any OCR payload columns.
        // Hidden startup must not inspect protected text while locked.
        let queue_has_rows = {
            let guard = self.get_connection_named("process_ocr_delete_queue_batch_probe")?;
            let conn = guard.as_ref().unwrap();
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM delete_queue_ocr LIMIT 1)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|e| format!("Failed to inspect OCR queue: {}", e))?
        };

        if !queue_has_rows {
            return Ok(OcrDeleteBatchResult {
                queue_empty: true,
                ..OcrDeleteBatchResult::default()
            });
        }

        // The maintenance loop also runs during hidden startup. Leave a
        // non-empty batch untouched until protected reads are silently
        // authorized, so this path can never prompt through CNG.
        if !self.is_silent_read_authorized() {
            return Ok(OcrDeleteBatchResult {
                waiting_for_unlock: true,
                ..OcrDeleteBatchResult::default()
            });
        }

        let rows: Vec<OcrDeleteQueueRow> = {
            let guard = self.get_connection_named("process_ocr_delete_queue_batch_read")?;
            let conn = guard.as_ref().unwrap();
            let mut read_stmt = conn
                .prepare(
                    "SELECT q.id, q.failure_count, o.id, o.text, o.text_hash,
                            o.text_enc, o.text_key_encrypted
                     FROM delete_queue_ocr q
                     LEFT JOIN ocr_results o ON o.id = q.id
                     ORDER BY q.id ASC
                     LIMIT ?1",
                )
                .map_err(|e| format!("Failed to prepare OCR queue read: {}", e))?;

            let mapped_rows = read_stmt
                .query_map([safe_batch_size], OcrDeleteQueueRow::from_row)
                .map_err(|e| format!("Failed to read OCR queue rows: {}", e))?;

            mapped_rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("Failed to decode OCR queue row: {}", e))?
        };

        if rows.is_empty() {
            return Ok(OcrDeleteBatchResult {
                queue_empty: true,
                ..OcrDeleteBatchResult::default()
            });
        }

        let mut removals: std::collections::HashMap<String, RoaringBitmap> =
            std::collections::HashMap::new();
        let mut delete_ids: Vec<i64> = Vec::with_capacity(rows.len());
        let mut retry_failures: Vec<OcrDeleteCleanupFailure> = Vec::new();
        let mut fallback_failures: Vec<OcrDeleteCleanupFailure> = Vec::new();
        let mut stale_queue_rows = 0usize;
        let mut hmac_key: Option<Vec<u8>> = None;

        for row in &rows {
            // Authorization can expire after the batch-level check. Abort
            // before touching even legacy plaintext so the batch remains
            // atomic from the caller's perspective.
            if !self.is_silent_read_authorized() {
                return Ok(OcrDeleteBatchResult {
                    waiting_for_unlock: true,
                    ..OcrDeleteBatchResult::default()
                });
            }

            if row.existing_id.is_none() {
                delete_ids.push(row.id);
                stale_queue_rows += 1;
                continue;
            }

            // An empty text hash is the durable marker for a row that never
            // reached the bitmap index, so it has no postings to unlink.
            if row.text_hash.as_deref().unwrap_or_default().is_empty() {
                delete_ids.push(row.id);
                continue;
            }

            let plaintext = match (row.text_enc.as_ref(), row.text_key_encrypted.as_ref()) {
                (Some(enc), Some(key_enc)) => {
                    match self.decrypt_payload_with_row_key_silent(enc, key_enc) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(text) => Ok(text),
                            Err(_) => row.legacy_text.clone().ok_or_else(|| {
                                format!("OCR row {} contains invalid UTF-8", row.id)
                            }),
                        },
                        // Authorization was lost between the batch-level check
                        // and this row. Discard all staged work and leave the
                        // entire batch for the next unlocked pass rather than
                        // counting a failure against any row.
                        Err(BackgroundReadError::AuthRequired) => {
                            return Ok(OcrDeleteBatchResult {
                                waiting_for_unlock: true,
                                ..OcrDeleteBatchResult::default()
                            });
                        }
                        Err(error) => row.legacy_text.clone().ok_or_else(|| {
                            format!(
                                "Failed to decrypt OCR row {} for delete cleanup: {}",
                                row.id, error
                            )
                        }),
                    }
                }
                _ => row
                    .legacy_text
                    .clone()
                    .ok_or_else(|| format!("OCR row {} has no readable text payload", row.id)),
            };

            let cleanup_input = plaintext.and_then(|text| {
                u32::try_from(row.id)
                    .map(|bitmap_id| (text, bitmap_id))
                    .map_err(|_| format!("OCR row {} exceeds bitmap id capacity", row.id))
            });

            let (plaintext, bitmap_id) = match cleanup_input {
                Ok(input) => input,
                Err(error) => {
                    let failure_count = row.failure_count.max(0).saturating_add(1);
                    let failure = OcrDeleteCleanupFailure {
                        id: row.id,
                        failure_count,
                        error,
                    };
                    if failure_count >= MAX_OCR_DELETE_CLEANUP_FAILURES {
                        delete_ids.push(row.id);
                        fallback_failures.push(failure);
                    } else {
                        retry_failures.push(failure);
                    }
                    continue;
                }
            };

            if hmac_key.is_none() {
                hmac_key = match self.credential_state.get_hmac_key() {
                    Ok(key) => Some(key),
                    Err(_error) if !self.is_silent_read_authorized() => {
                        return Ok(OcrDeleteBatchResult {
                            waiting_for_unlock: true,
                            ..OcrDeleteBatchResult::default()
                        });
                    }
                    Err(error) => return Err(error),
                };
            }
            let hmac_key = hmac_key
                .as_deref()
                .expect("OCR posting cleanup requires an HMAC key");
            for token in Self::bigram_tokenize(&plaintext) {
                let token_hash = Self::compute_hmac_hash(&token, hmac_key);
                removals
                    .entry(token_hash)
                    .or_insert_with(RoaringBitmap::new)
                    .insert(bitmap_id);
            }

            // The postings are staged for removal in the same transaction that
            // deletes the row, so the row is now ready for physical deletion.
            // Without this the queue would re-read the same batch forever and
            // no OCR row -- and therefore no screenshot -- would ever be freed.
            delete_ids.push(row.id);
        }

        let mut guard = self.get_connection_named("process_ocr_delete_queue_batch_write")?;
        let conn = guard.as_mut().unwrap();

        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start OCR cleanup transaction: {}", e))?;

        if !removals.is_empty() {
            let index = super::blind_index::BlindIndex::open(&tx)?;
            let mut stats = super::blind_index::MutationStats::default();
            for (token_hash, remove_set) in removals {
                index.remove(&token_hash, &remove_set, &mut stats)?;
            }
        }

        if !retry_failures.is_empty() {
            let mut update_stmt = tx
                .prepare_cached("UPDATE delete_queue_ocr SET failure_count = ?2 WHERE id = ?1")
                .map_err(|e| format!("Failed to prepare OCR retry update: {}", e))?;
            for failure in &retry_failures {
                update_stmt
                    .execute(params![failure.id, failure.failure_count])
                    .map_err(|e| format!("Failed to persist OCR cleanup retry: {}", e))?;
            }
        }

        let mut deleted_rows = 0usize;
        for chunk in delete_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            let sql_delete_ocr = format!("DELETE FROM ocr_results WHERE id IN ({})", placeholders);
            let params_ocr: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            deleted_rows += tx
                .execute(&sql_delete_ocr, params_ocr.as_slice())
                .map_err(|e| format!("Failed to hard-delete OCR rows: {}", e))?;

            let sql_delete_queue = format!(
                "DELETE FROM delete_queue_ocr WHERE id IN ({})",
                placeholders
            );
            let params_queue: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            tx.execute(&sql_delete_queue, params_queue.as_slice())
                .map_err(|e| format!("Failed to clear OCR queue rows: {}", e))?;
        }

        let blind_index_repair_requested = !fallback_failures.is_empty();
        if blind_index_repair_requested {
            Self::mark_blind_index_repair_needed_on_conn(&tx)?;
        }

        let queue_empty: bool = tx
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM delete_queue_ocr LIMIT 1)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to inspect OCR queue after cleanup: {}", e))?;

        tx.commit()
            .map_err(|e| format!("Failed to commit OCR cleanup: {}", e))?;
        // `ocr_row_count` is adjusted on soft-delete when rows become inactive.
        // Hard-delete queue processing is physical cleanup only and must not decrement again.

        for failure in &retry_failures {
            tracing::warn!(
                "[DELETE_QUEUE] OCR row {} cleanup failed ({}/{}); retrying later: {}",
                failure.id,
                failure.failure_count,
                MAX_OCR_DELETE_CLEANUP_FAILURES,
                failure.error
            );
        }
        for failure in &fallback_failures {
            tracing::warn!(
                "[DELETE_QUEUE] OCR row {} cleanup failed {} times; hard-deleted without bitmap unlink and scheduled repair: {}",
                failure.id,
                failure.failure_count,
                failure.error
            );
        }

        Ok(OcrDeleteBatchResult {
            queue_rows: delete_ids.len(),
            deleted_rows,
            stale_queue_rows,
            retry_rows: retry_failures.len(),
            fallback_deleted_rows: fallback_failures.len(),
            blind_index_repair_requested,
            queue_empty,
            waiting_for_unlock: false,
        })
    }

    /// Read screenshot cleanup candidates from queue (batch).
    pub fn fetch_screenshot_delete_candidates(
        &self,
        batch_size: i64,
    ) -> Result<Vec<QueueScreenshotCandidate>, String> {
        let safe_batch_size = batch_size.clamp(1, 2000);
        let guard = self.get_connection_named("fetch_screenshot_delete_candidates")?;
        let conn = guard.as_ref().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT q.id, s.image_hash, s.image_path
                 FROM delete_queue_screenshots q
                 LEFT JOIN screenshots s ON s.id = q.id
                 WHERE s.id IS NULL
                    OR NOT EXISTS (
                        SELECT 1 FROM ocr_results o WHERE o.screenshot_id = q.id
                    )
                 ORDER BY q.id ASC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare screenshot queue read: {}", e))?;

        let rows: Vec<(i64, Option<String>, Option<String>)> = stmt
            .query_map([safe_batch_size], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| format!("Failed to read screenshot queue rows: {}", e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Failed to decode screenshot queue row: {}", e))?;

        let mut stale_ids = Vec::new();
        let mut candidates = Vec::new();
        for (id, image_hash, image_path) in rows {
            match (image_hash, image_path) {
                (Some(hash), Some(path)) => candidates.push(QueueScreenshotCandidate {
                    id,
                    image_hash: hash,
                    image_path: path,
                }),
                _ => stale_ids.push(id),
            }
        }

        if !stale_ids.is_empty() {
            for chunk in stale_ids.chunks(500) {
                let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "DELETE FROM delete_queue_screenshots WHERE id IN ({})",
                    placeholders
                );
                let params_ref: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                conn.execute(&sql, params_ref.as_slice())
                    .map_err(|e| format!("Failed to cleanup stale screenshot queue rows: {}", e))?;
            }
        }

        Ok(candidates)
    }

    /// Finalize screenshot cleanup for a processed batch.
    pub fn finalize_screenshot_delete_batch(&self, ids: &[i64]) -> Result<usize, String> {
        if ids.is_empty() {
            return Ok(0);
        }

        let mut guard = self.get_connection_named("finalize_screenshot_delete_batch")?;
        let conn = guard.as_mut().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start screenshot cleanup transaction: {}", e))?;

        let mut deleted_screenshots = 0usize;
        for chunk in ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            let sql_delete_screenshots =
                format!("DELETE FROM screenshots WHERE id IN ({})", placeholders);
            let params_screenshots: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            let deleted = tx
                .execute(&sql_delete_screenshots, params_screenshots.as_slice())
                .map_err(|e| format!("Failed to hard-delete screenshots: {}", e))?;
            deleted_screenshots += deleted;

            let sql_delete_queue = format!(
                "DELETE FROM delete_queue_screenshots WHERE id IN ({})",
                placeholders
            );
            let params_queue: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            tx.execute(&sql_delete_queue, params_queue.as_slice())
                .map_err(|e| format!("Failed to clear screenshot queue rows: {}", e))?;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit screenshot cleanup: {}", e))?;

        let _ = Self::cleanup_orphaned_dedup_entries(conn);

        Ok(deleted_screenshots)
    }

    /// Run incremental vacuum only when both delete queues are empty.
    pub fn run_incremental_vacuum_if_idle(
        &self,
        freelist_threshold: i64,
        vacuum_pages: i64,
    ) -> Result<IncrementalVacuumResult, String> {
        let Some(_maintenance) = self.try_database_maintenance("run_incremental_vacuum_if_idle")
        else {
            return Ok(IncrementalVacuumResult::default());
        };
        let guard = self.get_connection_named("run_incremental_vacuum_if_idle")?;
        let conn = guard.as_ref().unwrap();

        let queues_busy: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM delete_queue_screenshots LIMIT 1)
                     OR EXISTS(SELECT 1 FROM delete_queue_ocr LIMIT 1)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to inspect delete queues before vacuum: {}", e))?;

        if queues_busy != 0 {
            return Ok(IncrementalVacuumResult::default());
        }

        let freelist_before: i64 = conn
            .query_row("PRAGMA freelist_count;", [], |row| row.get(0))
            .map_err(|e| format!("Failed to read freelist_count: {}", e))?;

        if freelist_before <= freelist_threshold {
            return Ok(IncrementalVacuumResult::default());
        }

        let pages = vacuum_pages.max(1);
        let page_count_before: i64 = conn
            .query_row("PRAGMA page_count;", [], |row| row.get(0))
            .map_err(|e| format!("Failed to read page_count before vacuum: {}", e))?;

        let mut stmt = conn
            .prepare(&format!("PRAGMA incremental_vacuum({});", pages))
            .map_err(|e| format!("Failed to prepare incremental_vacuum: {}", e))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("Failed to start incremental_vacuum: {}", e))?;
        let mut stepped_pages = 0usize;
        while rows
            .next()
            .map_err(|e| format!("Failed while stepping incremental_vacuum: {}", e))?
            .is_some()
        {
            stepped_pages += 1;
        }
        drop(rows);
        drop(stmt);

        let freelist_after: i64 = conn
            .query_row("PRAGMA freelist_count;", [], |row| row.get(0))
            .map_err(|e| format!("Failed to read freelist_count after vacuum: {}", e))?;
        let page_count_after: i64 = conn
            .query_row("PRAGMA page_count;", [], |row| row.get(0))
            .map_err(|e| format!("Failed to read page_count after vacuum: {}", e))?;

        Ok(IncrementalVacuumResult {
            attempted: true,
            requested_pages: pages,
            stepped_pages,
            reclaimed_pages: page_count_before.saturating_sub(page_count_after),
            freelist_before,
            freelist_after,
        })
    }

    /// Canonicalize a list of visible links into a deterministic JSON string.
    /// Links are sorted by (url, text) to ensure the same set always produces the same hash.
    fn canonicalize_links(links: &[super::VisibleLink]) -> String {
        let mut sorted: Vec<(&str, &str)> = links
            .iter()
            .map(|l| (l.url.as_str(), l.text.as_str()))
            .collect();
        sorted.sort();
        serde_json::to_string(&sorted).unwrap_or_default()
    }

    /// Get or create a page_icon dedup entry. Returns the row ID.
    /// Uses INSERT OR IGNORE + SELECT pattern for atomic upsert.
    pub(crate) fn get_or_create_page_icon_id(
        &self,
        conn: &Connection,
        plaintext: &str,
    ) -> Result<i64, String> {
        let content_hash = Self::compute_static_hash(plaintext);

        // Try to find existing entry first (fast path)
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM page_icons WHERE content_hash = ?",
                params![&content_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to query page_icons: {}", e))?;

        if let Some(id) = existing {
            return Ok(id);
        }

        // Encrypt with its own independent row key
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let icon_enc = encrypt_with_master_key(&row_key, plaintext.as_bytes())
            .map_err(|e| format!("Failed to encrypt page_icon: {}", e))?;
        let icon_key_encrypted = self
            .wrap_row_key_for_storage(&row_key)
            .map_err(|e| format!("Failed to wrap page_icon row key: {}", e))?;

        Self::zeroize_bytes(&mut row_key);

        // INSERT OR IGNORE handles race conditions
        conn.execute(
            "INSERT OR IGNORE INTO page_icons (content_hash, icon_enc, icon_key_encrypted) VALUES (?, ?, ?)",
            params![&content_hash, &icon_enc, &icon_key_encrypted],
        )
        .map_err(|e| format!("Failed to insert page_icon: {}", e))?;

        // SELECT the id (whether we just inserted or it was already there)
        let id: i64 = conn
            .query_row(
                "SELECT id FROM page_icons WHERE content_hash = ?",
                params![&content_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get page_icon id: {}", e))?;

        Ok(id)
    }

    /// Get or create a link_set dedup entry. Returns the row ID.
    pub(crate) fn get_or_create_link_set_id(
        &self,
        conn: &Connection,
        links: &[super::VisibleLink],
    ) -> Result<i64, String> {
        let canonical = Self::canonicalize_links(links);
        let content_hash = Self::compute_static_hash(&canonical);

        // Try to find existing entry first (fast path)
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM link_sets WHERE content_hash = ?",
                params![&content_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to query link_sets: {}", e))?;

        if let Some(id) = existing {
            return Ok(id);
        }

        // Encrypt the original JSON (not canonical) for faithful reconstruction
        let json = serde_json::to_string(links)
            .map_err(|e| format!("Failed to serialize visible_links: {}", e))?;

        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let links_enc = encrypt_with_master_key(&row_key, json.as_bytes())
            .map_err(|e| format!("Failed to encrypt link_set: {}", e))?;
        let links_key_encrypted = self
            .wrap_row_key_for_storage(&row_key)
            .map_err(|e| format!("Failed to wrap link_set row key: {}", e))?;

        Self::zeroize_bytes(&mut row_key);

        conn.execute(
            "INSERT OR IGNORE INTO link_sets (content_hash, links_enc, links_key_encrypted, link_count) VALUES (?, ?, ?, ?)",
            params![&content_hash, &links_enc, &links_key_encrypted, links.len() as i64],
        )
        .map_err(|e| format!("Failed to insert link_set: {}", e))?;

        let id: i64 = conn
            .query_row(
                "SELECT id FROM link_sets WHERE content_hash = ?",
                params![&content_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get link_set id: {}", e))?;

        Ok(id)
    }

    /// Clean up orphaned page_icons and link_sets entries after screenshot deletion.
    fn cleanup_orphaned_dedup_entries(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
              "DELETE FROM page_icons WHERE id NOT IN (SELECT DISTINCT page_icon_id FROM screenshots WHERE is_deleted = 0 AND page_icon_id IS NOT NULL);
               DELETE FROM link_sets WHERE id NOT IN (SELECT DISTINCT link_set_id FROM screenshots WHERE is_deleted = 0 AND link_set_id IS NOT NULL);"
        )
        .map_err(|e| format!("Failed to cleanup orphaned dedup entries: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod startup_pending_tests;

#[cfg(test)]
mod ocr_lifecycle_tests;
