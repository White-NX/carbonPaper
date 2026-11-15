//! Transactional publication of an edited screenshot and its new OCR evidence.

use super::derived_index::read_derived_data_epoch;
use super::ocr_keys::{self, WriteKey};
use super::{DerivedIndexKind, EditedScreenshotResponse, OcrResultInput, StorageState};
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
    results: &'a [OcrResultInput],
    key: Option<&'a WriteKey>,
}

fn publish(conn: &mut Connection, edit: &Publication<'_>) -> Result<usize, String> {
    for result in edit.results {
        super::screenshot::validate_ocr_result(result)?;
    }
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
    let dataset = ocr_keys::dataset(&tx)?;
    let mut row_id = ocr_keys::next_row_id(&tx)?;
    if let Some(key) = edit.key {
        key.insert(&tx, &dataset, edit.id)?;
    }
    for result in edit.results {
        let key = edit.key.ok_or("Missing fresh OCR encryption key")?;
        let text = key.encrypt(&dataset, edit.id, row_id, result.text.as_bytes())?;
        tx.execute(
            "INSERT INTO ocr_results(id,screenshot_id,text_hash,text_enc,ocr_key_id,confidence,
            box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4)
            VALUES(?1,?2,'',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                row_id,
                edit.id,
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
        VALUES(?1,'completed','pending',0) ON CONFLICT(screenshot_id) DO UPDATE SET status='completed',
        error=NULL,postprocess_status='pending',postprocess_error=NULL,postprocess_attempts=0,postprocess_next_retry_at=NULL,postprocess_lease=NULL",
        [edit.id]).map_err(|error| error.to_string())?;
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
        let guard = self.get_connection_named("screenshot_status")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT status FROM screenshots WHERE id=?1 AND is_deleted=0",
                [id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
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

    pub(crate) fn replace_edited_screenshot(
        &self,
        id: i64,
        expected_hash: &str,
        generation: u64,
        bytes: &[u8],
        pixels: &RgbImage,
        results: &[OcrResultInput],
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
            let key = if results.is_empty() {
                None
            } else {
                Some(self.new_ocr_write_key()?)
            };
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
                    results,
                    key: key.as_ref(),
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
                    Some(
                        value
                            .saturating_sub(removed as u64)
                            .saturating_add(results.len() as u64),
                    )
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
            Arc::new(CredentialManagerState::new(directory.path().to_path_buf())),
        );
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        conn.execute_batch("INSERT INTO screenshots(id,image_path,image_hash,width,height,created_at,status) VALUES
            (1,'original.enc','old',100,80,'2026-11-13 12:00:00','committed'),
            (2,'other.enc','other',100,80,'2026-11-13 13:00:00','committed');
            INSERT INTO ocr_results(id,screenshot_id,text,text_hash) VALUES(10,1,'old private text','old-text'),(11,2,'other text','other-text');
            INSERT INTO derived_index_jobs(index_kind,subject_key,status,model_id,model_revision,embedding_version,source_fingerprint,lease_token) VALUES
            ('semantic_text','1','processing','test','r1',1,'old','old-worker'),('clip_image','old','completed','test','r1',1,'old',NULL),
            ('semantic_text','2','completed','test','r1',1,'other',NULL);") .unwrap();
        (directory, storage, conn)
    }

    use std::sync::Arc;

    #[test]
    fn edit_replaces_ocr_and_invalidates_only_its_derived_jobs_without_changing_capture_identity() {
        let (_directory, _storage, mut conn) = fixture();
        let key = WriteKey::new(|bytes| Ok(bytes.to_vec())).unwrap();
        let results = vec![OcrResultInput {
            text: "new redacted text".into(),
            confidence: 0.9,
            box_coords: vec![
                vec![1.0, 2.0],
                vec![3.0, 2.0],
                vec![3.0, 4.0],
                vec![1.0, 4.0],
            ],
        }];
        let removed = publish(
            &mut conn,
            &Publication {
                id: 1,
                expected_hash: "old",
                image_hash: "new",
                image_path: "new.enc",
                results: &results,
                key: Some(&key),
            },
        )
        .unwrap();
        assert_eq!(removed, 1);
        let identity: (String, String, String, i64) = conn
            .query_row(
                "SELECT image_path,image_hash,created_at,is_deleted FROM screenshots WHERE id=1",
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
                0
            )
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id=1 AND is_deleted=0",
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
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id=2 AND is_deleted=0",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let (plain, encrypted): (Option<String>, Vec<u8>) = conn
            .query_row(
                "SELECT text,text_enc FROM ocr_results WHERE screenshot_id=1 AND is_deleted=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(plain.is_none());
        assert!(!encrypted
            .windows(results[0].text.len())
            .any(|bytes| bytes == results[0].text.as_bytes()));
    }

    #[test]
    fn stale_edits_and_ocr_insert_failure_leave_original_image_and_text_published() {
        let (_directory, _storage, mut conn) = fixture();
        assert!(publish(
            &mut conn,
            &Publication {
                id: 1,
                expected_hash: "stale",
                image_hash: "new",
                image_path: "new.enc",
                results: &[],
                key: None
            }
        )
        .is_err());
        conn.execute_batch("CREATE TRIGGER reject_new_ocr BEFORE INSERT ON ocr_results BEGIN SELECT RAISE(ABORT,'test publication failure'); END;").unwrap();
        let key = WriteKey::new(|bytes| Ok(bytes.to_vec())).unwrap();
        let results = vec![OcrResultInput {
            text: "replacement".into(),
            confidence: 1.0,
            box_coords: vec![vec![0.0, 0.0]; 4],
        }];
        assert!(publish(
            &mut conn,
            &Publication {
                id: 1,
                expected_hash: "old",
                image_hash: "new",
                image_path: "new.enc",
                results: &results,
                key: Some(&key)
            }
        )
        .is_err());
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
    }
}
