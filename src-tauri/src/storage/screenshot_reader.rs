//! Foreground reads of screenshot records. SQL rows are read under the database
//! guard and decrypted only after it is released.

use super::types::{RawRecentCaptureRow, RawScreenshotRow};
use super::{RecentCapture, ScreenshotRecord, StorageState};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

pub(crate) struct ScreenshotReader<'a> {
    storage: &'a StorageState,
}

impl<'a> ScreenshotReader<'a> {
    pub(super) fn new(storage: &'a StorageState) -> Self {
        Self { storage }
    }

    /// Returns the earliest records in the range, with an optional SQL limit.
    /// Timestamps at this boundary are Unix seconds.
    pub(crate) fn in_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
        max_records: Option<i64>,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        let start_dt = sqlite_datetime(start_ts);
        let end_dt = sqlite_datetime(end_ts);
        let raw_rows = {
            let guard = self
                .storage
                .get_connection_named("screenshots_in_time_range")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let sql = format!(
                "{} WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2
                 ORDER BY s.created_at ASC LIMIT ?3",
                RawScreenshotRow::SELECT,
            );
            let mut statement = conn
                .prepare(&sql)
                .map_err(|error| format!("Failed to prepare screenshot range query: {error}"))?;
            let rows = statement
                .query_map(
                    params![start_dt, end_dt, max_records.unwrap_or(-1)],
                    RawScreenshotRow::from_row,
                )
                .map_err(|error| format!("Failed to query screenshot range: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Failed to read screenshot range row: {error}"))?;
            rows
        };
        Ok(raw_rows
            .into_iter()
            .map(|row| row.into_record(&self.storage.credential_state))
            .collect())
    }

    /// Returns one deterministic page, with Unix-second bounds and SQL pagination.
    /// Only the requested records are read and decrypted.
    pub(crate) fn in_time_range_paged(
        &self,
        start_ts: f64,
        end_ts: f64,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        let start_dt = sqlite_datetime(start_ts);
        let end_dt = sqlite_datetime(end_ts);
        let raw_rows =
            self.storage
                .with_read_connection_named("screenshots_in_time_range_paged", |conn| {
                    let sql = format!(
                        "{} WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2
                     ORDER BY s.created_at ASC, s.id ASC LIMIT ?3 OFFSET ?4",
                        RawScreenshotRow::SELECT,
                    );
                    let mut statement = conn.prepare(&sql).map_err(|error| {
                        format!("Failed to prepare screenshot range query: {error}")
                    })?;
                    let rows = statement
                        .query_map(
                            params![start_dt, end_dt, limit, offset],
                            RawScreenshotRow::from_row,
                        )
                        .map_err(|error| format!("Failed to query screenshot range: {error}"))?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| format!("Failed to read screenshot range row: {error}"))?;
                    Ok(rows)
                })?;
        Ok(raw_rows
            .into_iter()
            .map(|row| row.into_record(&self.storage.credential_state))
            .collect())
    }

    /// Lists recent captures without joining OCR rows, so each screenshot
    /// appears once and SQLite can stop at the indexed limit.
    pub(crate) fn recent(&self, limit: i64) -> Result<Vec<RecentCapture>, String> {
        let raw_rows = {
            let guard = self.storage.get_connection_named("recent_screenshots")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let mut statement = conn
                .prepare(
                    "SELECT id, image_path, window_title, process_name,
                            window_title_enc, process_name_enc, content_key_encrypted,
                            category, created_at
                     FROM screenshots
                     WHERE is_deleted = 0 AND (status IS NULL OR status != 'aborted')
                     ORDER BY created_at DESC, id DESC LIMIT ?1",
                )
                .map_err(|error| format!("Failed to prepare recent captures query: {error}"))?;
            let rows = statement
                .query_map([limit.clamp(1, 500)], |row| {
                    Ok(RawRecentCaptureRow {
                        id: row.get(0)?,
                        image_path: row.get(1)?,
                        window_title_plain: row.get(2)?,
                        process_name_plain: row.get(3)?,
                        window_title_enc: row.get(4)?,
                        process_name_enc: row.get(5)?,
                        content_key_enc: row.get(6)?,
                        category: row.get(7)?,
                        created_at: row.get(8)?,
                    })
                })
                .map_err(|error| format!("Failed to query recent captures: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Failed to read recent capture row: {error}"))?;
            rows
        };
        Ok(raw_rows
            .into_iter()
            .map(|row| row.into_capture(&self.storage.credential_state))
            .collect())
    }

    pub(crate) fn by_id(&self, id: i64) -> Result<Option<ScreenshotRecord>, String> {
        let raw_row = {
            let guard = self.storage.get_connection_named("screenshot_by_id")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            conn.query_row(
                &format!(
                    "{} WHERE s.id = ?1 AND s.is_deleted = 0",
                    RawScreenshotRow::SELECT
                ),
                [id],
                RawScreenshotRow::from_row,
            )
            .optional()
            .map_err(|error| format!("Failed to get screenshot: {error}"))?
        };
        Ok(raw_row.map(|row| row.into_record(&self.storage.credential_state)))
    }

    /// Legacy `memory://` references identify a screenshot by its image hash.
    pub(crate) fn by_image_path(&self, path: &str) -> Result<Option<ScreenshotRecord>, String> {
        let (column, value) = match path.strip_prefix("memory://") {
            Some(hash) => ("s.image_hash", hash),
            None => ("s.image_path", path),
        };
        let raw_row = {
            let guard = self
                .storage
                .get_connection_named("screenshot_by_image_path")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            conn.query_row(
                &format!(
                    "{} WHERE {column} = ?1 AND s.is_deleted = 0",
                    RawScreenshotRow::SELECT,
                ),
                [value],
                RawScreenshotRow::from_row,
            )
            .optional()
            .map_err(|error| format!("Failed to get screenshot by path: {error}"))?
        };
        Ok(raw_row.map(|row| row.into_record(&self.storage.credential_state)))
    }
}

fn sqlite_datetime(timestamp: f64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp as i64, 0)
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
