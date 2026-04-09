//! Screenshot CRUD operations (save, get, delete, commit, abort).

use crate::credential_manager::{
    encrypt_row_key_with_cng,
    encrypt_with_master_key,
};
use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use roaring::RoaringBitmap;
use std::sync::atomic::Ordering;

use super::types::RawScreenshotRow;
use super::{
    DeleteQueueStatus, DensityBucket, OcrResultInput, QueueScreenshotCandidate,
    SaveScreenshotRequest, SaveScreenshotResponse, ScreenshotRecord, SoftDeleteResult,
    SoftDeleteScreenshotsResult, StorageState,
};

impl StorageState {
    /// Get all screenshot image paths (for thumbnail warmup).
    pub fn get_all_image_paths(&self) -> Result<Vec<String>, String> {
        let guard = self.get_connection_named("get_all_image_paths")?;
        let conn = guard.as_ref().unwrap();
        let mut stmt = conn
            .prepare("SELECT image_path FROM screenshots WHERE is_deleted = 0 ORDER BY created_at DESC")
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
        let encrypted_row_key = encrypt_row_key_with_cng(&row_key)
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
            Some(value) if !value.is_empty() => Some(Self::get_or_create_page_icon_id(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(Self::get_or_create_link_set_id(conn, links)?),
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

    /// Save a pending screenshot: encrypt and write file, insert DB record with 'pending' status.
    pub fn save_screenshot_temp(
        &self,
        request: &SaveScreenshotRequest,
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

        // Decode image
        let t0 = std::time::Instant::now();
        let image_data = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &request.image_data,
        )
        .map_err(|e| format!("Failed to decode image data: {}", e))?;
        let decode_dur = t0.elapsed();

        // Generate row key and encrypt image
        let t1 = std::time::Instant::now();
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_image = encrypt_with_master_key(&row_key, &image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = encrypt_row_key_with_cng(&row_key)
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
        if let Err(e) = self.generate_thumbnail_from_data(&image_data, &final_image_path, &row_key)
        {
            tracing::warn!("Failed to generate thumbnail during temp save: {}", e);
        }

        let mut guard = self.get_connection_named("save_screenshot_temp")?;
        let conn = guard.as_mut().unwrap();
        let mutex_wait_dur =
            fn_start.elapsed() - exists_dur - decode_dur - img_encrypt_dur - file_write_dur;

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
            Some(value) if !value.is_empty() => Some(Self::get_or_create_page_icon_id(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(Self::get_or_create_link_set_id(conn, links)?),
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

        // Clear lock holder on drop
        if let Ok(mut h) = self.lock_holder.lock() {
            *h = "";
        }
        drop(guard);

        let total_dur = fn_start.elapsed();
        if total_dur.as_millis() >= 500 {
            tracing::warn!(
                "[DIAG:DB] save_screenshot_temp id={} total={:?} (exists_check={:?}, b64_decode={:?}, img_encrypt={:?}, file_write={:?}, mutex_wait~={:?}, in_lock={:?})",
                screenshot_id, total_dur, exists_dur, decode_dur, img_encrypt_dur, file_write_dur, mutex_wait_dur, in_lock_dur
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

        let mut guard = self.get_connection_named("commit_screenshot")?;
        let conn = guard.as_mut().unwrap();
        let mutex_wait_dur = fn_start.elapsed();

        // Look up the screenshot record
        let rec: Option<(String, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT image_path, content_key_encrypted FROM screenshots WHERE id = ? AND is_deleted = 0",
                params![screenshot_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("Failed to query screenshot: {}", e))?;

        if rec.is_none() {
            if let Ok(mut h) = self.lock_holder.lock() {
                *h = "";
            }
            return Err("Screenshot not found".to_string());
        }

        let (image_path_str, _content_key_enc) = rec.unwrap();
        let image_path = self.resolve_image_path(&image_path_str);

        // If filename ends with .pending, rename to final name and update DB image_path
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
        let skipped = 0;

        // Timing accumulators for OCR processing
        let mut total_encrypt_dur = std::time::Duration::ZERO;
        let mut total_db_insert_dur = std::time::Duration::ZERO;
        let total_bitmap_dur = std::time::Duration::ZERO;

        // Insert OCR results (if any)
        if let Some(results) = ocr_results {
            for result in results {
                let te0 = std::time::Instant::now();
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;
                total_encrypt_dur += te0.elapsed();

                // Insert OCR result (lazy indexing will process this later)
                let td0 = std::time::Instant::now();
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
                total_db_insert_dur += td0.elapsed();

                added += 1;
                self.ocr_row_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Mark committed and set committed_at, update image_path to renamed path
        conn.execute(
            "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP, category = ?, category_confidence = ? WHERE id = ? AND is_deleted = 0",
            params![final_image_path_str, "committed", category, category_confidence, screenshot_id],
        )
        .map_err(|e| format!("Failed to mark screenshot committed: {}", e))?;

        // Clear lock holder on drop
        if let Ok(mut h) = self.lock_holder.lock() {
            *h = "";
        }
        drop(guard);

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

    /// Get screenshots within a time range.
    pub fn get_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        self.get_screenshots_by_time_range_limited(start_ts, end_ts, None)
    }

    pub fn get_screenshots_by_time_range_limited(
        &self,
        start_ts: f64,
        end_ts: f64,
        max_records: Option<i64>,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        let diag_start = std::time::Instant::now();

        // Phase 1: Hold mutex only for SQL query, extract raw data without decryption
        let raw_rows = {
            let mut guard = self.get_connection_named("get_screenshots_by_time_range")?;
            let conn = guard.as_mut().unwrap();

            let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();

            let limit_clause = match max_records {
                Some(n) => format!(" LIMIT {}", n),
                None => String::new(),
            };

            let sql = format!(
                "SELECT s.id, s.image_path, s.image_hash, s.width, s.height,
                        s.window_title, s.process_name, s.metadata,
                        s.window_title_enc, s.process_name_enc, s.metadata_enc,
                        s.content_key_encrypted,
                        strftime('%s', s.created_at) as timestamp, s.created_at,
                        s.source, s.page_url_enc, s.page_icon_enc, s.visible_links_enc,
                        pi.icon_enc, pi.icon_key_encrypted,
                        ls.links_enc, ls.links_key_encrypted,
                        s.category, s.category_confidence
                 FROM screenshots s
                 LEFT JOIN page_icons pi ON s.page_icon_id = pi.id
                 LEFT JOIN link_sets ls ON s.link_set_id = ls.id
                 WHERE s.is_deleted = 0 AND s.created_at BETWEEN '{}' AND '{}'
                 ORDER BY s.created_at ASC{}",
                start_dt, end_dt, limit_clause
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows: Vec<RawScreenshotRow> = stmt
                .query_map([], RawScreenshotRow::from_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            rows
            // guard is dropped here, mutex released
        };

        let query_elapsed = diag_start.elapsed();

        // Phase 2: Decrypt outside mutex
        let records: Vec<ScreenshotRecord> =
            raw_rows.into_iter().map(|raw| raw.into_record()).collect();

        if diag_start.elapsed().as_secs() >= 5 {
            tracing::warn!(
                "[DIAG:DB] get_screenshots_by_time_range({} ~ {}) returned {} records, query {:?}, total {:?}",
                start_ts,
                end_ts,
                records.len(),
                query_elapsed,
                diag_start.elapsed()
            );
        }
        Ok(records)
    }

    /// Count screenshots within a time range (no decryption, very fast).
    pub fn count_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<i64, String> {
        let guard = self.get_connection_named("count_screenshots_by_time_range")?;
        let conn = guard.as_ref().unwrap();

        let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        let sql = format!(
            "SELECT COUNT(*) FROM screenshots WHERE is_deleted = 0 AND created_at BETWEEN '{}' AND '{}'",
            start_dt, end_dt
        );

        conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
            .map_err(|e| format!("Failed to count screenshots: {}", e))
    }

    /// Get screenshot density (counts per time bucket) within a time range.
    /// No decryption or joins - extremely fast index-only scan.
    pub fn get_screenshot_density(
        &self,
        start_ts: f64,
        end_ts: f64,
        bucket_seconds: i64,
    ) -> Result<Vec<DensityBucket>, String> {
        let guard = self.get_connection_named("get_screenshot_density")?;
        let conn = guard.as_ref().unwrap();

        let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        let sql = format!(
            "SELECT (CAST(strftime('%s', created_at) AS INTEGER) / {bs}) * {bs} AS bucket, \
                    COUNT(*) AS cnt \
             FROM screenshots \
             WHERE is_deleted = 0 AND created_at BETWEEN '{start}' AND '{end}' \
             GROUP BY bucket \
             ORDER BY bucket",
            bs = bucket_seconds,
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

    /// Get screenshots within a time range with SQL-level LIMIT/OFFSET.
    /// Only the requested page is fetched and decrypted.
    pub fn get_screenshots_by_time_range_paged(
        &self,
        start_ts: f64,
        end_ts: f64,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        let diag_start = std::time::Instant::now();

        // Phase 1: Hold mutex only for SQL query, extract raw data without decryption
        let raw_rows = {
            let mut guard = self.get_connection_named("get_screenshots_by_time_range_paged")?;
            let conn = guard.as_mut().unwrap();

            let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();

            let sql = format!(
                "SELECT s.id, s.image_path, s.image_hash, s.width, s.height,
                        s.window_title, s.process_name, s.metadata,
                        s.window_title_enc, s.process_name_enc, s.metadata_enc,
                        s.content_key_encrypted,
                        strftime('%s', s.created_at) as timestamp, s.created_at,
                        s.source, s.page_url_enc, s.page_icon_enc, s.visible_links_enc,
                        pi.icon_enc, pi.icon_key_encrypted,
                        ls.links_enc, ls.links_key_encrypted,
                        s.category, s.category_confidence
                 FROM screenshots s
                 LEFT JOIN page_icons pi ON s.page_icon_id = pi.id
                 LEFT JOIN link_sets ls ON s.link_set_id = ls.id
                 WHERE s.is_deleted = 0 AND s.created_at BETWEEN '{}' AND '{}'
                 ORDER BY s.created_at ASC
                 LIMIT {} OFFSET {}",
                start_dt, end_dt, limit, offset
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows: Vec<RawScreenshotRow> = stmt
                .query_map([], RawScreenshotRow::from_row)
                .map_err(|e| format!("Failed to execute query: {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            rows
        };

        let query_elapsed = diag_start.elapsed();

        // Phase 2: Decrypt only the page rows
        let records: Vec<ScreenshotRecord> =
            raw_rows.into_iter().map(|raw| raw.into_record()).collect();

        if diag_start.elapsed().as_secs() >= 2 {
            tracing::warn!(
                "[DIAG:DB] get_screenshots_by_time_range_paged({} ~ {}, offset={}, limit={}) returned {} records, query {:?}, total {:?}",
                start_ts, end_ts, offset, limit,
                records.len(),
                query_elapsed,
                diag_start.elapsed()
            );
        }
        Ok(records)
    }

    /// Get screenshot details by ID.
    pub fn get_screenshot_by_id(&self, id: i64) -> Result<Option<ScreenshotRecord>, String> {
        tracing::debug!("get_screenshot_by_id called with id={}", id);

        // Phase 1: Hold mutex only for SQL query, extract raw data
        let raw_row = {
            let guard = self.get_connection_named("get_screenshot_by_id")?;
            let conn = guard.as_ref().unwrap();

            let sql = format!(
                "SELECT s.id, s.image_path, s.image_hash, s.width, s.height,
                        s.window_title, s.process_name, s.metadata,
                        s.window_title_enc, s.process_name_enc, s.metadata_enc,
                        s.content_key_encrypted,
                        strftime('%s', s.created_at) as timestamp, s.created_at,
                        s.source, s.page_url_enc, s.page_icon_enc, s.visible_links_enc,
                        pi.icon_enc, pi.icon_key_encrypted,
                        ls.links_enc, ls.links_key_encrypted,
                        s.category, s.category_confidence
                 FROM screenshots s
                 LEFT JOIN page_icons pi ON s.page_icon_id = pi.id
                 LEFT JOIN link_sets ls ON s.link_set_id = ls.id
                 WHERE s.id = {} AND s.is_deleted = 0",
                id
            );

            let result = conn.query_row(&sql, [], |row| RawScreenshotRow::from_row(row));

            match result {
                Ok(raw) => Some(raw),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    tracing::debug!("No record found for id={}", id);
                    return Ok(None);
                }
                Err(e) => {
                    tracing::error!("Query error for id={}: {}", id, e);
                    return Err(format!("Failed to get screenshot: {}", e));
                }
            }
            // guard dropped here, mutex released
        };

        // Phase 2: Decrypt outside mutex
        match raw_row {
            Some(raw) => {
                let record = raw.into_record();
                tracing::debug!(
                    "Found record id={}, image_path={}",
                    record.id,
                    record.image_path
                );
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// Get a screenshot by image_path (or by image_hash if path starts with "memory://").
    pub fn get_screenshot_by_image_path(
        &self,
        path: &str,
    ) -> Result<Option<ScreenshotRecord>, String> {
        tracing::debug!("get_screenshot_by_image_path called with path={}", path);

        let raw_row = {
            let guard = self.get_connection_named("get_screenshot_by_image_path")?;
            let conn = guard.as_ref().unwrap();

            let (where_clause, param_value): (&str, String) = if let Some(hash) = path.strip_prefix("memory://") {
                ("WHERE s.image_hash = ? AND s.is_deleted = 0", hash.to_string())
            } else {
                ("WHERE s.image_path = ? AND s.is_deleted = 0", path.to_string())
            };

            let sql = format!(
                "SELECT s.id, s.image_path, s.image_hash, s.width, s.height,
                        s.window_title, s.process_name, s.metadata,
                        s.window_title_enc, s.process_name_enc, s.metadata_enc,
                        s.content_key_encrypted,
                        strftime('%s', s.created_at) as timestamp, s.created_at,
                        s.source, s.page_url_enc, s.page_icon_enc, s.visible_links_enc,
                        pi.icon_enc, pi.icon_key_encrypted,
                        ls.links_enc, ls.links_key_encrypted,
                        s.category, s.category_confidence
                 FROM screenshots s
                 LEFT JOIN page_icons pi ON s.page_icon_id = pi.id
                 LEFT JOIN link_sets ls ON s.link_set_id = ls.id
                 {}",
                where_clause
            );

            let result =
                conn.query_row(&sql, [&param_value], |row| RawScreenshotRow::from_row(row));

            match result {
                Ok(raw) => Some(raw),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    tracing::debug!("No record found for path={}", path);
                    return Ok(None);
                }
                Err(e) => {
                    tracing::error!("Query error for path={}: {}", path, e);
                    return Err(format!("Failed to get screenshot by path: {}", e));
                }
            }
        };

        match raw_row {
            Some(raw) => {
                let record = raw.into_record();
                tracing::debug!(
                    "Found record id={}, image_path={}",
                    record.id,
                    record.image_path
                );
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// Get OCR results for a screenshot.
    pub fn get_screenshot_ocr_results(
        &self,
        screenshot_id: i64,
    ) -> Result<Vec<super::OcrResult>, String> {
        let guard = self.get_connection_named("get_screenshot_ocr_results")?;
        let conn = guard.as_ref().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT id, screenshot_id, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4, created_at
                 FROM ocr_results WHERE screenshot_id = ? AND is_deleted = 0
                 ORDER BY box_y1, box_x1",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map([screenshot_id], |row| {
                let text_enc: Option<Vec<u8>> = row.get(2)?;
                let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                    (Some(data), Some(key)) => self
                        .decrypt_payload_with_row_key(data, key)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => None,
                };

                Ok(super::OcrResult {
                    id: row.get(0)?,
                    screenshot_id: row.get(1)?,
                    text: text.unwrap_or_default(),
                    confidence: row.get(4)?,
                    box_coords: vec![
                        vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                        vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                        vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                        vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                    ],
                    created_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Delete a screenshot by ID.
    pub fn delete_screenshot(&self, id: i64) -> Result<bool, String> {
        let guard = self.get_connection_named("delete_screenshot")?;
        let conn = guard.as_ref().unwrap();

        // Get image path first
        let image_path: Option<String> = conn
            .query_row(
                "SELECT image_path FROM screenshots WHERE id = ? AND is_deleted = 0",
                [id],
                |row| row.get(0),
            )
            .ok();

        // Count OCR rows that will be cascade-deleted
        let ocr_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id = ? AND is_deleted = 0",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Delete database record
        let deleted = conn
            .execute("DELETE FROM screenshots WHERE id = ? AND is_deleted = 0", [id])
            .map_err(|e| format!("Failed to delete screenshot: {}", e))?;

        // Try to delete image file
        if deleted > 0 {
            if let Some(path) = image_path {
                let abs_path = self.resolve_image_path(&path);
                let _ = std::fs::remove_file(&abs_path);
                let thumb = Self::thumbnail_path_for(&abs_path);
                let _ = std::fs::remove_file(&thumb);
            }
            let _ = self
                .ocr_row_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(ocr_count as u64))
                });
            // Clean up orphaned dedup entries
            let _ = Self::cleanup_orphaned_dedup_entries(conn);
        }

        Ok(deleted > 0)
    }

    /// Delete screenshots within a time range.
    pub fn delete_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<i32, String> {
        let guard = self.get_connection_named("delete_screenshots_by_time_range")?;
        let conn = guard.as_ref().unwrap();

        // Convert timestamps (milliseconds) to SQLite datetime
        let start_dt = DateTime::<Utc>::from_timestamp((start_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp((end_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        // Get all image paths to delete
        let mut stmt = conn
            .prepare("SELECT image_path FROM screenshots WHERE is_deleted = 0 AND created_at BETWEEN ? AND ?")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let paths: Vec<String> = stmt
            .query_map([&start_dt, &end_dt], |row| row.get(0))
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Count OCR rows that will be cascade-deleted
        let ocr_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE is_deleted = 0 AND screenshot_id IN (SELECT id FROM screenshots WHERE is_deleted = 0 AND created_at BETWEEN ? AND ?)",
                [&start_dt, &end_dt],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Delete database records
        let deleted = conn
            .execute(
                "DELETE FROM screenshots WHERE is_deleted = 0 AND created_at BETWEEN ? AND ?",
                [&start_dt, &end_dt],
            )
            .map_err(|e| format!("Failed to delete screenshots: {}", e))?;

        if deleted > 0 {
            let _ = self
                .ocr_row_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(ocr_count as u64))
                });
            // Clean up orphaned dedup entries
            let _ = Self::cleanup_orphaned_dedup_entries(conn);
        }

        // Try to delete image files
        for path in paths {
            let abs_path = self.resolve_image_path(&path);
            let _ = std::fs::remove_file(&abs_path);
            let thumb = Self::thumbnail_path_for(&abs_path);
            let _ = std::fs::remove_file(&thumb);
        }

        Ok(deleted as i32)
    }

    /// Soft-delete screenshots by process (and optional month key `YYYY-MM`) and enqueue IDs.
    pub fn soft_delete_process_month(
        &self,
        process_name: &str,
        month: Option<&str>,
    ) -> Result<SoftDeleteResult, String> {
        let normalized_process = process_name.trim();
        if normalized_process.is_empty() {
            return Err("process_name is required".to_string());
        }

        let normalized_month = month
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        if let Some(ref m) = normalized_month {
            let valid = m.len() == 7
                && m.as_bytes().get(4) == Some(&b'-')
                && m.chars().enumerate().all(|(idx, ch)| {
                    if idx == 4 {
                        ch == '-'
                    } else {
                        ch.is_ascii_digit()
                    }
                });
            if !valid {
                return Err("month must be in YYYY-MM format".to_string());
            }
        }

        let mut filter_plain = String::from("process_name = ?1 AND is_deleted = 0");
        let mut filter_alias = String::from("s.process_name = ?1 AND s.is_deleted = 0");
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(normalized_process.to_string())];

        if let Some(ref month_key) = normalized_month {
            filter_plain.push_str(" AND strftime('%Y-%m', created_at) = ?2");
            filter_alias.push_str(" AND strftime('%Y-%m', s.created_at) = ?2");
            param_values.push(Box::new(month_key.clone()));
        }

        let params_ref: Vec<&dyn rusqlite::ToSql> = param_values.iter().map(|v| v.as_ref()).collect();

        let mut guard = self.get_connection_named("soft_delete_process_month")?;
        let conn = guard.as_mut().unwrap();
        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start soft-delete transaction: {}", e))?;

        let queue_screenshots_sql = format!(
            "INSERT OR IGNORE INTO delete_queue_screenshots (id) SELECT id FROM screenshots WHERE {}",
            filter_plain
        );
        let queued_screenshots = tx
            .execute(&queue_screenshots_sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to queue screenshots: {}", e))? as i64;

        let queue_ocr_sql = format!(
            "INSERT OR IGNORE INTO delete_queue_ocr (id)
             SELECT o.id
             FROM ocr_results o
             JOIN screenshots s ON s.id = o.screenshot_id
             WHERE o.is_deleted = 0 AND {}",
            filter_alias
        );
        let queued_ocr = tx
            .execute(&queue_ocr_sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to queue OCR rows: {}", e))? as i64;

        let mark_ocr_sql = format!(
            "UPDATE ocr_results
             SET is_deleted = 1
             WHERE is_deleted = 0
               AND screenshot_id IN (SELECT id FROM screenshots WHERE {})",
            filter_plain
        );
        let ocr_marked = tx
            .execute(&mark_ocr_sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to mark OCR rows deleted: {}", e))? as i64;

        let mark_screenshots_sql = format!(
            "UPDATE screenshots SET is_deleted = 1 WHERE {}",
            filter_plain
        );
        let screenshots_marked = tx
            .execute(&mark_screenshots_sql, params_ref.as_slice())
            .map_err(|e| format!("Failed to mark screenshots deleted: {}", e))? as i64;

        tx.commit()
            .map_err(|e| format!("Failed to commit soft-delete transaction: {}", e))?;

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
        let mut normalized_ids: Vec<i64> = screenshot_ids.iter().copied().filter(|id| *id > 0).collect();
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

    /// Fetch pending counts in delete queues.
    pub fn get_delete_queue_status(&self) -> Result<DeleteQueueStatus, String> {
        let guard = self.get_connection_named("get_delete_queue_status")?;
        let conn = guard.as_ref().unwrap();

        let pending_screenshots: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_screenshots", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count screenshot queue: {}", e))?;
        let pending_ocr: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count OCR queue: {}", e))?;

        Ok(DeleteQueueStatus {
            pending_screenshots,
            pending_ocr,
            running: pending_screenshots > 0 || pending_ocr > 0,
        })
    }

    /// Process one OCR delete queue batch and unlink each row from blind bitmap index.
    pub fn process_ocr_delete_queue_batch(&self, batch_size: i64) -> Result<usize, String> {
        let safe_batch_size = batch_size.clamp(1, 2000);
        let hmac_key = self.credential_state.get_hmac_key()?;

        let mut guard = self.get_connection_named("process_ocr_delete_queue_batch")?;
        let conn = guard.as_mut().unwrap();

        let rows: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)> = {
            let mut read_stmt = conn
                .prepare(
                    "SELECT q.id, o.text_enc, o.text_key_encrypted
                     FROM delete_queue_ocr q
                     LEFT JOIN ocr_results o ON o.id = q.id
                     ORDER BY q.id ASC
                     LIMIT ?1",
                )
                .map_err(|e| format!("Failed to prepare OCR queue read: {}", e))?;

            let mapped_rows = read_stmt
                .query_map([safe_batch_size], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|e| format!("Failed to read OCR queue rows: {}", e))?;

            let collected: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)> =
                mapped_rows.filter_map(|r| r.ok()).collect();
            collected
        };

        if rows.is_empty() {
            return Ok(0);
        }

        let mut removals: std::collections::HashMap<String, RoaringBitmap> =
            std::collections::HashMap::new();
        let mut queue_ids: Vec<i64> = Vec::with_capacity(rows.len());

        for (ocr_id, text_enc, text_key_enc) in &rows {
            queue_ids.push(*ocr_id);
            let plaintext = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                (Some(enc), Some(key_enc)) => self
                    .decrypt_payload_with_row_key(enc, key_enc)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok()),
                _ => None,
            };

            if let Some(text) = plaintext {
                for token in Self::bigram_tokenize(&text) {
                    let token_hash = Self::compute_hmac_hash(&token, &hmac_key);
                    removals
                        .entry(token_hash)
                        .or_insert_with(RoaringBitmap::new)
                        .insert(*ocr_id as u32);
                }
            }
        }

        let tx = conn
            .transaction()
            .map_err(|e| format!("Failed to start OCR cleanup transaction: {}", e))?;

        if !removals.is_empty() {
            let mut get_stmt = tx
                .prepare_cached("SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1")
                .map_err(|e| format!("Failed to prepare bitmap read: {}", e))?;
            let mut put_stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)",
                )
                .map_err(|e| format!("Failed to prepare bitmap write: {}", e))?;
            let mut del_stmt = tx
                .prepare_cached("DELETE FROM blind_bitmap_index WHERE token_hash = ?1")
                .map_err(|e| format!("Failed to prepare bitmap delete: {}", e))?;

            for (token_hash, remove_set) in removals {
                let existing_blob: Option<Vec<u8>> = get_stmt
                    .query_row(params![&token_hash], |row| row.get(0))
                    .optional()
                    .map_err(|e| format!("Failed to load bitmap row: {}", e))?;

                let Some(blob) = existing_blob else {
                    continue;
                };

                let mut bitmap = RoaringBitmap::deserialize_from(&blob[..])
                    .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;

                for id in remove_set.iter() {
                    bitmap.remove(id);
                }

                if bitmap.is_empty() {
                    del_stmt
                        .execute(params![&token_hash])
                        .map_err(|e| format!("Failed to delete empty bitmap row: {}", e))?;
                } else {
                    let mut buf = Vec::new();
                    bitmap
                        .serialize_into(&mut buf)
                        .map_err(|e| format!("Failed to serialize bitmap: {}", e))?;
                    put_stmt
                        .execute(params![&token_hash, &buf])
                        .map_err(|e| format!("Failed to write bitmap row: {}", e))?;
                }
            }
        }

        let mut hard_deleted = 0usize;
        for chunk in queue_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");

            let sql_delete_ocr = format!("DELETE FROM ocr_results WHERE id IN ({})", placeholders);
            let params_ocr: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            let deleted = tx
                .execute(&sql_delete_ocr, params_ocr.as_slice())
                .map_err(|e| format!("Failed to hard-delete OCR rows: {}", e))?;
            hard_deleted += deleted;

            let sql_delete_queue =
                format!("DELETE FROM delete_queue_ocr WHERE id IN ({})", placeholders);
            let params_queue: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            tx.execute(&sql_delete_queue, params_queue.as_slice())
                .map_err(|e| format!("Failed to clear OCR queue rows: {}", e))?;
        }

        tx.commit()
            .map_err(|e| format!("Failed to commit OCR cleanup: {}", e))?;

        if hard_deleted > 0 {
            let _ = self
                .ocr_row_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(hard_deleted as u64))
                });
        }

        Ok(queue_ids.len())
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
                 ORDER BY q.id ASC
                 LIMIT ?1",
            )
            .map_err(|e| format!("Failed to prepare screenshot queue read: {}", e))?;

        let rows: Vec<(i64, Option<String>, Option<String>)> = stmt
            .query_map([safe_batch_size], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|e| format!("Failed to read screenshot queue rows: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

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

            let sql_delete_queue =
                format!("DELETE FROM delete_queue_screenshots WHERE id IN ({})", placeholders);
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
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("run_incremental_vacuum_if_idle")?;
        let conn = guard.as_ref().unwrap();

        let pending_screenshots: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_screenshots", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count screenshot queue: {}", e))?;
        let pending_ocr: i64 = conn
            .query_row("SELECT COUNT(*) FROM delete_queue_ocr", [], |row| row.get(0))
            .map_err(|e| format!("Failed to count OCR queue: {}", e))?;

        if pending_screenshots > 0 || pending_ocr > 0 {
            return Ok(false);
        }

        let freelist_count: i64 = conn
            .query_row("PRAGMA freelist_count;", [], |row| row.get(0))
            .map_err(|e| format!("Failed to read freelist_count: {}", e))?;

        if freelist_count <= freelist_threshold {
            return Ok(false);
        }

        let pages = vacuum_pages.max(1);
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({});", pages))
            .map_err(|e| format!("Failed to run incremental_vacuum: {}", e))?;
        Ok(true)
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
    pub(crate) fn get_or_create_page_icon_id(conn: &Connection, plaintext: &str) -> Result<i64, String> {
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
        let icon_key_encrypted = encrypt_row_key_with_cng(&row_key)
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
        let links_key_encrypted = encrypt_row_key_with_cng(&row_key)
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
