//! Durable image publication, invalidation of old evidence and OCR enqueueing.

use super::derived_index::read_derived_data_epoch;
use super::{DerivedIndexKind, EditedScreenshotResponse, StorageState};
use crate::credential_manager::{decrypt_row_key_with_cng_silent, encrypt_with_master_key};
use image::RgbImage;
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;
use zeroize::Zeroizing;

struct Publication<'a> {
    id: i64,
    expected_hash: &'a str,
    image_hash: &'a str,
    image_path: &'a str,
}

fn publish(conn: &mut Connection, edit: &Publication<'_>) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    let updated = tx
        .execute(
            "UPDATE screenshots SET image_path=?1, image_hash=?2, committed_at=CURRENT_TIMESTAMP
         WHERE id=?3 AND image_hash=?4 AND is_deleted=0 AND status='committed'",
            params![
                edit.image_path,
                edit.image_hash,
                edit.id,
                edit.expected_hash
            ],
        )
        .map_err(|error| error.to_string())?;
    if updated != 1 {
        return Err("The snapshot changed. Select it again before editing".into());
    }

    // Old OCR disappears from queries at the same commit. The existing delete
    // queue then unlinks its blind postings and retires its encrypted row keys.
    tx.execute(
        "INSERT OR IGNORE INTO delete_queue_ocr(id)
        SELECT id FROM ocr_results WHERE screenshot_id=?1 AND is_deleted=0",
        [edit.id],
    )
    .map_err(|error| error.to_string())?;
    let removed = tx
        .execute(
            "UPDATE ocr_results SET is_deleted=1 WHERE screenshot_id=?1 AND is_deleted=0",
            [edit.id],
        )
        .map_err(|error| error.to_string())?;

    for table in ["derived_embeddings", "derived_index_jobs"] {
        tx.execute(&format!("DELETE FROM {table} WHERE (index_kind='semantic_text' AND subject_key=CAST(?1 AS TEXT))
            OR (index_kind='clip_image' AND subject_key=?2 AND NOT EXISTS(
                SELECT 1 FROM screenshots WHERE image_hash=?2 AND is_deleted=0))"),
            params![edit.id, edit.expected_hash]).map_err(|error| error.to_string())?;
    }
    tx.execute(
        "DELETE FROM smart_cluster_assignments WHERE screenshot_id=?1",
        [edit.id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT OR IGNORE INTO smart_cluster_pending(screenshot_id) VALUES(?1)",
        [edit.id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute("INSERT INTO screenshot_ocr_status(screenshot_id,status,postprocess_status,postprocess_attempts)
        VALUES(?1,'pending','none',0) ON CONFLICT(screenshot_id) DO UPDATE SET status='pending',
        engine=NULL,model_id=NULL,execution_provider=NULL,elapsed_ms=NULL,error=NULL,
        postprocess_status='none',postprocess_error=NULL,postprocess_attempts=0,postprocess_next_retry_at=NULL,postprocess_lease=NULL",
        [edit.id]).map_err(|error| error.to_string())?;
    tx.execute("INSERT INTO screenshot_edit_ocr_jobs(screenshot_id,image_hash) VALUES(?1,?2)
        ON CONFLICT(screenshot_id) DO UPDATE SET image_hash=excluded.image_hash,status='pending',
        attempts=0,next_retry_at=0,lease=NULL,error=NULL,queued_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP",
        params![edit.id,edit.image_hash]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(removed)
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Failed to create edited snapshot file: {error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Failed to write edited snapshot file: {error}"))
}

impl StorageState {
    pub(crate) fn screenshot_status(&self, id: i64) -> Result<String, String> {
        self.with_read_connection_named("screenshot_status", |conn| {
            conn.query_row(
                "SELECT status FROM screenshots WHERE id=?1 AND is_deleted=0",
                [id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn check_screenshot_edit(&self, id: i64, expected_hash: &str) -> Result<(), String> {
        let guard = self.get_connection_named("check_screenshot_edit")?;
        let valid: bool = guard.as_ref().ok_or("Database not initialized")?.query_row(
            "SELECT EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND image_hash=?2 AND is_deleted=0 AND status='committed')",
            params![id, expected_hash], |row| row.get(0),
        ).map_err(|error| error.to_string())?;
        if valid {
            Ok(())
        } else {
            Err(
                "The snapshot changed or is still being processed. Select it again before editing"
                    .into(),
            )
        }
    }

    pub(crate) fn save_edited_screenshot_image(
        &self,
        id: i64,
        expected_hash: &str,
        generation: u64,
        bytes: &[u8],
        pixels: &RgbImage,
    ) -> Result<EditedScreenshotResponse, String> {
        let _activity = self.foreground_db_read();
        let (old_path, wrapped_key) = {
            let guard = self.get_connection_named("replace_edited_screenshot.lookup")?;
            if self.db_generation() != generation {
                return Err("Database changed while editing the snapshot".into());
            }
            guard
                .as_ref()
                .ok_or("Database not initialized")?
                .query_row(
                    "SELECT image_path,content_key_encrypted FROM screenshots
                 WHERE id=?1 AND image_hash=?2 AND is_deleted=0 AND status='committed'",
                    params![id, expected_hash],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .ok_or("The snapshot changed. Select it again before editing")?
        };
        let row_key = Zeroizing::new(
            decrypt_row_key_with_cng_silent(&self.credential_state, &wrapped_key)
                .map_err(|error| error.to_string())?,
        );
        let mut hash = Md5::new();
        hash.update(b"carbonpaper/edited-snapshot/v1\0");
        hash.update(id.to_le_bytes());
        hash.update(expected_hash.as_bytes());
        hash.update(bytes);
        let image_hash = hex::encode(hash.finalize());
        let name = format!(
            "screenshot_edit_{id}_{}.png.enc",
            carbonpaper_app_bound::protocol::random_id()
        );
        let image_path = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .join(name);
        let relative_path = self.to_relative_image_path(&image_path);
        let mut remove_old_files = false;
        let result = (|| -> Result<(), String> {
            let encrypted =
                encrypt_with_master_key(&row_key, bytes).map_err(|error| error.to_string())?;
            write_new_file(&image_path, &encrypted)?;
            let thumb =
                image::DynamicImage::ImageRgb8(image::imageops::thumbnail(pixels, 192, 192));
            let mut jpeg = std::io::Cursor::new(Vec::new());
            thumb
                .write_to(&mut jpeg, image::ImageFormat::Jpeg)
                .map_err(|error| error.to_string())?;
            let encrypted_thumb = encrypt_with_master_key(&row_key, &jpeg.into_inner())
                .map_err(|error| error.to_string())?;
            let thumb_path = Self::thumbnail_path_for(&image_path);
            std::fs::create_dir_all(
                thumb_path
                    .parent()
                    .ok_or("Thumbnail has no parent directory")?,
            )
            .map_err(|error| error.to_string())?;
            write_new_file(&thumb_path, &encrypted_thumb)?;
            let mut guard = self.get_connection_named("replace_edited_screenshot.publish")?;
            if self.db_generation() != generation {
                return Err("Database changed while editing the snapshot".into());
            }
            if !self.credential_state.is_session_valid() {
                return Err("AUTH_REQUIRED".into());
            }
            let conn = guard.as_mut().ok_or("Database not initialized")?;
            let text_epoch = read_derived_data_epoch(conn, DerivedIndexKind::SemanticText)?;
            let image_epoch = read_derived_data_epoch(conn, DerivedIndexKind::ClipImage)?;
            let removed = publish(
                conn,
                &Publication {
                    id,
                    expected_hash,
                    image_hash: &image_hash,
                    image_path: &relative_path,
                },
            )?;
            // Everything after publish is best effort: propagating an error
            // here would incorrectly remove a file already named by the DB.
            match (
                read_derived_data_epoch(conn, DerivedIndexKind::SemanticText),
                read_derived_data_epoch(conn, DerivedIndexKind::ClipImage),
            ) {
                (Ok(text_after), Ok(image_after)) => {
                    self.note_semantic_cache_removal(
                        DerivedIndexKind::SemanticText,
                        &id.to_string(),
                        text_epoch,
                        text_after,
                    );
                    self.note_semantic_cache_removal(
                        DerivedIndexKind::ClipImage,
                        expected_hash,
                        image_epoch,
                        image_after,
                    );
                }
                _ => self.reset_semantic_vector_cache(),
            }
            remove_old_files = conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM screenshots WHERE image_path=?1 AND is_deleted=0)",
                [&old_path], |row| row.get(0)).unwrap_or(false);
            let _ = self
                .ocr_row_count
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_sub(removed as u64))
                });
            self.processing_stage.invalidate_screenshot_receipts(&[id]);
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(cleanup) = self.remove_screenshot_files(&relative_path) {
                tracing::warn!("Failed to remove unpublished screenshot edit id={id}: {cleanup}");
            }
            return Err(error);
        }
        self.clear_search_order_cache();
        // Keep the old files through the commit. A cleanup failure cannot turn
        // a completed save into a reported failure or remove the new image.
        if remove_old_files {
            if let Err(error) = self.remove_screenshot_files(&old_path) {
                tracing::warn!("Failed to remove superseded screenshot files id={id}: {error}");
            }
        }
        Ok(EditedScreenshotResponse {
            status: "success".into(),
            screenshot_id: id,
            image_path: relative_path,
            image_hash,
            ocr_status: "pending".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;

    fn fixture() -> (tempfile::TempDir, StorageState, Connection) {
        let directory = tempfile::tempdir().unwrap();
        let storage = StorageState::new(
            directory.path().to_path_buf(),
            std::sync::Arc::new(CredentialManagerState::new(directory.path().to_path_buf())),
        );
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        conn.execute_batch("INSERT INTO screenshots(id,image_path,image_hash,width,height,created_at,status) VALUES
            (1,'original.enc','old',100,80,'2026-11-13 12:00:00','committed'),
            (2,'other.enc','other',100,80,'2026-11-13 13:00:00','committed');
            INSERT INTO ocr_results(id,screenshot_id,text,text_hash) VALUES(10,1,'old private text','old-text'),(11,2,'other text','other-text');
            INSERT INTO derived_index_jobs(index_kind,subject_key,status,model_id,model_revision,embedding_version,source_fingerprint,lease_token) VALUES
            ('semantic_text','1','processing','test','r1',1,'old','old-worker'),('clip_image','old','completed','test','r1',1,'old',NULL),
            ('semantic_text','2','completed','test','r1',1,'other',NULL);").unwrap();
        (directory, storage, conn)
    }

    fn edit(expected_hash: &str) -> Publication<'_> {
        Publication {
            id: 1,
            expected_hash,
            image_hash: "new",
            image_path: "new.enc",
        }
    }

    #[test]
    fn edit_publishes_image_and_invalidates_evidence_before_recognition() {
        let (_directory, _storage, mut conn) = fixture();
        assert_eq!(publish(&mut conn, &edit("old")).unwrap(), 1);
        let identity: (String, String, String, String) = conn
            .query_row(
                "SELECT image_path,image_hash,created_at,status FROM screenshots WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            identity,
            (
                "new.enc".into(),
                "new".into(),
                "2026-11-13 12:00:00".into(),
                "committed".into()
            )
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id=1 AND is_deleted=0",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id=2 AND is_deleted=0",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM delete_queue_ocr WHERE id=10",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM derived_index_jobs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let job: (String, String, String) = conn
            .query_row(
                "SELECT j.image_hash,j.status,o.postprocess_status FROM screenshot_edit_ocr_jobs j
             JOIN screenshot_ocr_status o USING(screenshot_id) WHERE j.screenshot_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(job, ("new".into(), "pending".into(), "none".into()));

        // Physically removing obsolete evidence must not invalidate a later
        // recognition/classification lease for the edited image.
        let revision: i64 = conn
            .query_row(
                "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute("DELETE FROM ocr_results WHERE id=10", [])
            .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT revision FROM screenshot_processing_revisions WHERE screenshot_id=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            revision
        );
    }

    #[test]
    fn stale_edits_and_queue_write_failure_leave_the_original_image_and_text() {
        let (_directory, _storage, mut conn) = fixture();
        assert!(publish(&mut conn, &edit("stale")).is_err());
        conn.execute_batch(
            "CREATE TRIGGER reject_edit_queue BEFORE INSERT ON screenshot_edit_ocr_jobs
            BEGIN SELECT RAISE(ABORT,'test queue publication failure'); END;",
        )
        .unwrap();
        assert!(publish(&mut conn, &edit("old")).is_err());
        assert_eq!(
            conn.query_row("SELECT image_path FROM screenshots WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "original.enc"
        );
        assert_eq!(
            conn.query_row(
                "SELECT is_deleted FROM ocr_results WHERE id=10",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM screenshot_edit_ocr_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM derived_index_jobs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
}
