//! Cached UI progress and inexpensive estimates from durable scan cursors.
//! MAX(rowid) seeks the end of each table; no content or row-count scan is needed.
use super::{OCR_CURSOR, OCR_DONE};
use crate::storage::{ocr_keys, process_icons, vector_blocks, StorageState};
use rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StorageCompactionProgress {
    pub phase: &'static str,
    pub running: bool,
    pub percent: Option<f64>,
    pub revision: u64,
}

impl Default for StorageCompactionProgress {
    fn default() -> Self {
        Self {
            phase: "idle",
            running: false,
            percent: None,
            revision: 0,
        }
    }
}

#[derive(Default)]
pub(in crate::storage) struct ProgressState {
    generation: u64,
    progress: StorageCompactionProgress,
}

impl ProgressState {
    pub(super) fn snapshot(&self) -> StorageCompactionProgress {
        self.progress.clone()
    }

    pub(super) fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.progress = StorageCompactionProgress {
            revision: self.progress.revision + 1,
            ..StorageCompactionProgress::default()
        };
    }

    pub(super) fn update(
        &mut self,
        generation: u64,
        phase: &'static str,
        percent: Option<f64>,
    ) -> Option<StorageCompactionProgress> {
        if self.generation != generation
            || (self.progress.phase == phase && self.progress.percent == percent)
        {
            return None;
        }
        self.progress.phase = phase;
        self.progress.running = phase == "running";
        self.progress.percent = percent;
        self.progress.revision += 1;
        Some(self.snapshot())
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Checkpoints {
    markers: i64,
    markers_done: bool,
    vectors: i64,
    icons: i64,
    ocr_keys: i64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MaxIds {
    ocr: i64,
    screenshots: i64,
    vectors: i64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Snapshot {
    pub checkpoints: Checkpoints,
    pub max_ids: MaxIds,
    pub free_pages: u64,
}

impl Snapshot {
    pub(super) fn read(conn: &Connection) -> Result<Self, String> {
        let checkpoints = conn
            .query_row(
                "SELECT
                COALESCE((SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?1),0),
                EXISTS(SELECT 1 FROM app_metadata WHERE key=?2 AND value='1'),
                COALESCE((SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?3),0),
                COALESCE((SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?4),0),
                COALESCE((SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?5),0)",
                params![
                    OCR_CURSOR,
                    OCR_DONE,
                    vector_blocks::CURSOR,
                    process_icons::CURSOR,
                    ocr_keys::COMPACTION_CURSOR
                ],
                |row| {
                    Ok(Checkpoints {
                        markers: row.get(0)?,
                        markers_done: row.get(1)?,
                        vectors: row.get(2)?,
                        icons: row.get(3)?,
                        ocr_keys: row.get(4)?,
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        let max_ids = conn
            .query_row(
                "SELECT
                    COALESCE((SELECT MAX(id) FROM ocr_results),0),
                    COALESCE((SELECT MAX(id) FROM screenshots),0),
                    COALESCE((SELECT MAX(rowid) FROM derived_embeddings),0)",
                [],
                |row| {
                    Ok(MaxIds {
                        ocr: row.get::<_, i64>(0)?.max(0),
                        screenshots: row.get::<_, i64>(1)?.max(0),
                        vectors: row.get::<_, i64>(2)?.max(0),
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        let free_pages = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())?
            .max(0) as u64;
        Ok(Self {
            checkpoints,
            max_ids,
            free_pages,
        })
    }

    pub(super) fn unprotected_pending(&self) -> bool {
        (self.max_ids.ocr > 0 && !self.checkpoints.markers_done)
            || (self.max_ids.vectors > 0 && self.checkpoints.vectors >= 0)
    }

    pub(super) fn pending(&self) -> bool {
        self.unprotected_pending()
            || (self.max_ids.screenshots > 0 && self.checkpoints.icons != i64::MAX)
            || (self.max_ids.ocr > 0 && self.checkpoints.ocr_keys != i64::MAX)
    }

    pub(super) fn phase(&self, read_wait_reason: Option<&'static str>) -> &'static str {
        if !self.pending() {
            "idle"
        } else if self.unprotected_pending() {
            "running"
        } else {
            read_wait_reason.unwrap_or("running")
        }
    }

    /// IDs can have gaps and new captures can extend the ranges, so this is an
    /// estimate of scan traversal, not a row count or remaining-time estimate.
    pub(super) fn estimated_percent(&self) -> Option<f64> {
        let total = self.max_ids.ocr as f64 * 2.0
            + self.max_ids.screenshots as f64
            + self.max_ids.vectors as f64;
        if total <= 0.0 {
            return None;
        }
        if !self.pending() {
            return Some(100.0);
        }
        let traversed = |cursor: i64, end: i64| cursor.clamp(0, end) as f64;
        let visited = if self.checkpoints.markers_done {
            self.max_ids.ocr as f64
        } else {
            traversed(self.checkpoints.markers, self.max_ids.ocr)
        } + if self.checkpoints.vectors < 0 {
            self.max_ids.vectors as f64
        } else {
            traversed(self.checkpoints.vectors, self.max_ids.vectors)
        } + traversed(self.checkpoints.icons, self.max_ids.screenshots)
            + traversed(self.checkpoints.ocr_keys, self.max_ids.ocr);
        // Reaching the last ID is not completion: the next committed page must
        // publish the terminal marker, including after authentication waits.
        Some((visited / total * 100.0).clamp(0.0, 99.0))
    }
}

impl StorageState {
    /// Contains no archive content and never waits for the database mutex.
    pub fn storage_compaction_progress(&self) -> StorageCompactionProgress {
        self.storage_compaction_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }

    pub(in crate::storage) fn reset_storage_compaction_progress(&self) {
        self.storage_compaction_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reset(self.db_generation());
    }
}
