//! Restartable recognition of published image edits, fenced by image and lease.

use super::ocr_keys::{self, WriteKey};
use super::{OcrResultInput, StorageState};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug)]
pub(crate) struct EditOcrJob {
    pub screenshot_id: i64,
    pub image_hash: String,
    pub image_path: String,
    pub generation: u64,
    pub lease: String,
    pub attempts: i64,
    pub timestamp_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EditOcrStatus {
    pub screenshot_id: i64,
    pub image_hash: String,
    pub status: String,
}

fn current(conn: &Connection, job: &EditOcrJob) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM screenshot_edit_ocr_jobs j
         JOIN screenshots s ON s.id=j.screenshot_id AND s.image_hash=j.image_hash
         WHERE j.screenshot_id=?1 AND j.image_hash=?2 AND j.lease=?3
         AND j.status='processing' AND s.is_deleted=0 AND s.status='committed')",
        params![job.screenshot_id, job.image_hash, job.lease],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn insert_results(
    conn: &Connection,
    id: i64,
    results: &[OcrResultInput],
    key: Option<&WriteKey>,
) -> Result<(), String> {
    let dataset = ocr_keys::dataset(conn)?;
    let mut row_id = ocr_keys::next_row_id(conn)?;
    if let Some(key) = key {
        key.insert(conn, &dataset, id)?;
    }
    for result in results {
        super::screenshot::validate_ocr_result(result)?;
        let key = key.ok_or("Missing fresh OCR encryption key")?;
        let text = key.encrypt(&dataset, id, row_id, result.text.as_bytes())?;
        conn.execute(
            "INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,ocr_key_id,confidence,
             box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
             VALUES(?1,?2,'',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                row_id,
                id,
                text,
                key.id,
                result.confidence,
                result.box_coords[0][0],
                result.box_coords[0][1],
                result.box_coords[1][0],
                result.box_coords[1][1],
                result.box_coords[2][0],
                result.box_coords[2][1],
                result.box_coords[3][0],
                result.box_coords[3][1]
            ],
        )
        .map_err(|error| error.to_string())?;
        row_id = row_id
            .checked_add(1)
            .ok_or("OCR row ID capacity exhausted")?;
    }
    Ok(())
}

fn publish_results(
    conn: &mut Connection,
    job: &EditOcrJob,
    results: &[OcrResultInput],
    key: Option<&WriteKey>,
    elapsed_ms: f64,
    provider: &str,
) -> Result<bool, String> {
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    if !current(&tx, job)? {
        return Ok(false);
    }
    insert_results(&tx, job.screenshot_id, results, key)?;
    tx.execute(
        "UPDATE screenshot_edit_ocr_jobs SET status='completed',lease=NULL,error=NULL,
        updated_at=CURRENT_TIMESTAMP WHERE screenshot_id=?1",
        [job.screenshot_id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute("UPDATE screenshot_ocr_status SET status='completed',engine='rust',model_id='ppocrv5-ch-mobile',
        execution_provider=?2,elapsed_ms=?3,error=NULL,attempted_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP,
        postprocess_status='pending',postprocess_attempts=0,postprocess_error=NULL,
        postprocess_next_retry_at=NULL,postprocess_lease=NULL WHERE screenshot_id=?1",
        params![job.screenshot_id,provider,elapsed_ms]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(true)
}

impl StorageState {
    pub(crate) fn edited_ocr_status(&self, id: i64) -> Result<Option<EditOcrStatus>, String> {
        self.with_read_connection_named("edited_ocr_status", |conn| {
            conn.query_row(
                "SELECT j.image_hash,j.status FROM screenshot_edit_ocr_jobs j
                 JOIN screenshots s ON s.id=j.screenshot_id AND s.image_hash=j.image_hash
                 WHERE j.screenshot_id=?1 AND s.is_deleted=0",
                [id],
                |row| {
                    Ok(EditOcrStatus {
                        screenshot_id: id,
                        image_hash: row.get(0)?,
                        status: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn screenshot_image_is_current(&self, id: i64, hash: &str) -> Result<bool, String> {
        self.with_read_connection_named("screenshot_image_is_current", |conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND image_hash=?2
                 AND is_deleted=0 AND status='committed')",
                params![id, hash],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn recover_edited_ocr_jobs(&self, generation: u64) -> Result<(), String> {
        let _activity = self.background_db_read();
        let mut guard = self.get_connection_named("recover_edited_ocr_jobs")?;
        if self.db_generation() != generation {
            return Ok(());
        }
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE screenshot_edit_ocr_jobs SET status='pending',lease=NULL,next_retry_at=0,
            updated_at=CURRENT_TIMESTAMP WHERE status='processing'",
            [],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE screenshot_ocr_status SET status='pending' WHERE screenshot_id IN
            (SELECT screenshot_id FROM screenshot_edit_ocr_jobs WHERE status='pending')",
            [],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn claim_edited_ocr_job(&self) -> Result<Option<EditOcrJob>, String> {
        if !self.is_silent_read_authorized() {
            return Ok(None);
        }
        let _activity = self.background_db_read();
        let mut guard = self.get_connection_named("claim_edited_ocr_job")?;
        let generation = self.db_generation();
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let row = tx.query_row(
            "SELECT j.screenshot_id,j.image_hash,s.image_path,j.attempts,
                    CAST(strftime('%s',s.created_at) AS INTEGER)*1000
             FROM screenshot_edit_ocr_jobs j JOIN screenshots s
               ON s.id=j.screenshot_id AND s.image_hash=j.image_hash
             WHERE j.status IN ('pending','waiting_for_auth','failed') AND j.attempts<5 AND j.next_retry_at<=?1
               AND s.is_deleted=0 AND s.status='committed'
             ORDER BY j.queued_at,j.screenshot_id LIMIT 1",
            [chrono::Utc::now().timestamp()],
            |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,
                row.get::<_,String>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?)),
        ).optional().map_err(|error| error.to_string())?;
        let Some((screenshot_id, image_hash, image_path, attempts, timestamp_ms)) = row else {
            return Ok(None);
        };
        let lease = carbonpaper_app_bound::protocol::random_id();
        tx.execute(
            "UPDATE screenshot_edit_ocr_jobs SET status='processing',lease=?2,error=NULL,
            updated_at=CURRENT_TIMESTAMP WHERE screenshot_id=?1",
            params![screenshot_id, lease],
        )
        .map_err(|error| error.to_string())?;
        tx.execute("UPDATE screenshot_ocr_status SET status='processing',error=NULL,updated_at=CURRENT_TIMESTAMP
            WHERE screenshot_id=?1", [screenshot_id]).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(Some(EditOcrJob {
            screenshot_id,
            image_hash,
            image_path,
            generation,
            lease,
            attempts,
            timestamp_ms,
        }))
    }

    pub(crate) fn complete_edited_ocr_job(
        &self,
        job: &EditOcrJob,
        results: &[OcrResultInput],
        elapsed_ms: f64,
        provider: &str,
    ) -> Result<bool, String> {
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        if self.db_generation() != job.generation
            || !self
                .with_read_connection_named("check_edited_ocr_job", |conn| current(conn, job))?
        {
            return Ok(false);
        }
        let key = if results.is_empty() {
            None
        } else {
            Some(self.new_ocr_write_key()?)
        };
        let _activity = self.background_db_read();
        let mut guard = self.get_connection_named("complete_edited_ocr_job")?;
        if self.db_generation() != job.generation {
            return Ok(false);
        }
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let published = publish_results(conn, job, results, key.as_ref(), elapsed_ms, provider)?;
        if published {
            self.ocr_row_count
                .fetch_add(results.len() as u64, Ordering::Relaxed);
        }
        drop(guard);
        if published {
            self.clear_search_order_cache();
        }
        Ok(published)
    }

    pub(crate) fn defer_edited_ocr_job(
        &self,
        job: &EditOcrJob,
        error: &str,
        waiting_for_auth: bool,
    ) -> Result<Option<EditOcrStatus>, String> {
        let _activity = self.background_db_read();
        let mut guard = self.get_connection_named("defer_edited_ocr_job")?;
        if self.db_generation() != job.generation {
            return Ok(None);
        }
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        if !current(&tx, job)? {
            return Ok(None);
        }
        let attempts = if waiting_for_auth {
            job.attempts
        } else {
            job.attempts.saturating_add(1)
        };
        let status = if waiting_for_auth {
            "waiting_for_auth"
        } else {
            "failed"
        };
        let delay = match attempts {
            0 => 0,
            1 => 30,
            2 => 60,
            3 => 120,
            _ => 300,
        };
        let next_retry_at = if waiting_for_auth {
            0
        } else {
            chrono::Utc::now().timestamp().saturating_add(delay)
        };
        let error = error.chars().take(500).collect::<String>();
        tx.execute(
            "UPDATE screenshot_edit_ocr_jobs SET status=?2,attempts=?3,next_retry_at=?4,
            lease=NULL,error=?5,updated_at=CURRENT_TIMESTAMP WHERE screenshot_id=?1",
            params![job.screenshot_id, status, attempts, next_retry_at, error],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE screenshot_ocr_status SET status=?2,error=?3,updated_at=CURRENT_TIMESTAMP
            WHERE screenshot_id=?1",
            params![job.screenshot_id, status, error],
        )
        .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(Some(EditOcrStatus {
            screenshot_id: job.screenshot_id,
            image_hash: job.image_hash.clone(),
            status: status.into(),
        }))
    }

    pub(crate) fn retry_edited_ocr_job(
        &self,
        id: i64,
        hash: &str,
    ) -> Result<EditOcrStatus, String> {
        let _activity = self.foreground_db_read();
        let mut guard = self.get_connection_named("retry_edited_ocr_job")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let changed = tx.execute("UPDATE screenshot_edit_ocr_jobs SET status='pending',attempts=0,
            next_retry_at=0,lease=NULL,error=NULL,queued_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP
            WHERE screenshot_id=?1 AND image_hash=?2 AND status IN ('failed','waiting_for_auth','pending')
            AND EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND image_hash=?2 AND is_deleted=0 AND status='committed')",
            params![id,hash]).map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err("The snapshot changed or recognition is already running".into());
        }
        tx.execute("UPDATE screenshot_ocr_status SET status='pending',error=NULL,updated_at=CURRENT_TIMESTAMP
            WHERE screenshot_id=?1", [id]).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(EditOcrStatus {
            screenshot_id: id,
            image_hash: hash.into(),
            status: "pending".into(),
        })
    }
}

#[cfg(test)]
mod tests;
