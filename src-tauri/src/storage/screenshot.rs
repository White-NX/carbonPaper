//! Screenshot CRUD operations (save, get, delete, commit, abort).

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_with_master_key, encrypt_row_key_with_cng,
    encrypt_with_master_key,
};
use chrono::{DateTime, Utc};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;

use super::types::RawScreenshotRow;
use super::{
    DensityBucket, OcrResultInput, SaveScreenshotRequest, SaveScreenshotResponse,
    ScreenshotRecord, StorageState,
};

impl StorageState {
    /// Get all screenshot image paths (for thumbnail warmup).
    pub fn get_all_image_paths(&self) -> Result<Vec<String>, String> {
        let guard = self.get_connection_named("get_all_image_paths")?;
        let conn = guard.as_ref().unwrap();
        let mut stmt = conn
            .prepare("SELECT image_path FROM screenshots ORDER BY created_at DESC")
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
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ?)",
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
            "SELECT image_hash, id FROM screenshots WHERE image_hash IN ({})",
            placeholders
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare batch id lookup: {}", e))?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            hashes.iter().map(|h| h as &dyn rusqlite::types::ToSql).collect();

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
        let screenshot_dir = self.screenshot_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
            .map(|m| serde_json::to_string(m).ok())
            .flatten();
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
            Some(value) if !value.is_empty() => Some(Self::get_or_create_page_icon(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(Self::get_or_create_link_set(conn, links)?),
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
                let text_hash = Self::compute_hmac_hash(&result.text);
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;

                // Check for duplicates
                let box_coords = &result.box_coords;
                if box_coords.len() >= 4 {
                    let existing: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM ocr_results
                             WHERE screenshot_id = ? AND text_hash = ?
                             AND ABS(box_x1 - ?) < 10 AND ABS(box_y1 - ?) < 10",
                            params![
                                screenshot_id,
                                &text_hash,
                                box_coords[0][0],
                                box_coords[0][1],
                            ],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);

                    if existing > 0 {
                        skipped += 1;
                        continue;
                    }

                    // Insert new OCR result with encrypted text and blind index update
                    conn.execute(
                        "INSERT INTO ocr_results (
                            screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                            box_x1, box_y1, box_x2, box_y2,
                            box_x3, box_y3, box_x4, box_y4
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            screenshot_id,
                            Option::<String>::None,
                            &text_hash,
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

                    // Update blind bigram bitmap index
                    let ocr_id = conn.last_insert_rowid();
                    let triple_tokens = Self::bigram_tokenize(&result.text);
                    let tx = conn
                        .transaction()
                        .map_err(|e| format!("Failed to start transaction: {}", e))?;

                    let mut get_stmt = tx
                        .prepare_cached(
                            "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                        )
                        .map_err(|e| format!("Failed to prepare get statement: {}", e))?;
                    let mut put_stmt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)"
                    ).map_err(|e| format!("Failed to prepare put statement: {}", e))?;

                    for token in triple_tokens {
                        let token_hash = Self::compute_hmac_hash(&token);
                        let existing_blob: Option<Vec<u8>> = get_stmt
                            .query_row(params![&token_hash], |row| row.get(0))
                            .optional()
                            .map_err(|e| format!("Failed to query postings_blob: {}", e))?;
                        let mut bitmap = if let Some(blob) = existing_blob {
                            roaring::RoaringBitmap::deserialize_from(&blob[..])
                                .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?
                        } else {
                            roaring::RoaringBitmap::new()
                        };

                        bitmap.insert(ocr_id as u32);

                        let mut serialized_blob = Vec::new();
                        bitmap
                            .serialize_into(&mut serialized_blob)
                            .map_err(|e| format!("Failed to serialize bitmap: {}", e))?;

                        put_stmt
                            .execute(params![&token_hash, &serialized_blob])
                            .map_err(|e| {
                                format!("Failed to update blind bitmap index: {}", e)
                            })?;
                    }

                    drop(put_stmt);
                    drop(get_stmt);

                    tx.commit()
                        .map_err(|e| format!("Failed to commit bitmap index: {}", e))?;

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
        let screenshot_dir = self.screenshot_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let image_path = screenshot_dir.join(&filename);

        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;
        let file_write_dur = t2.elapsed();

        let image_path_str = self.to_relative_image_path(&image_path);

        // Generate thumbnail while we still have image_data and row_key
        // Use the final path (without .pending) so thumbnail path stays valid after commit rename
        let final_image_path = {
            let fname = image_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if fname.ends_with(".pending") {
                image_path.with_file_name(fname.trim_end_matches(".pending"))
            } else {
                image_path.clone()
            }
        };
        if let Err(e) = self.generate_thumbnail_from_data(&image_data, &final_image_path, &row_key) {
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
            .map(|m| serde_json::to_string(m).ok())
            .flatten();

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
            Some(value) if !value.is_empty() => Some(Self::get_or_create_page_icon(conn, value)?),
            _ => None,
        };
        let link_set_id: Option<i64> = match &request.visible_links {
            Some(links) if !links.is_empty() => Some(Self::get_or_create_link_set(conn, links)?),
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
                "SELECT image_path, content_key_encrypted FROM screenshots WHERE id = ?",
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
        let mut total_bitmap_dur = std::time::Duration::ZERO;

        // Insert OCR results (if any) and update blind bigram bitmap index
        if let Some(results) = ocr_results {
            for result in results {
                let te0 = std::time::Instant::now();
                let text_hash = Self::compute_hmac_hash(&result.text);
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;
                total_encrypt_dur += te0.elapsed();

                // Insert OCR result
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
                        &text_hash,
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

                let tb0 = std::time::Instant::now();
                let ocr_id = conn.last_insert_rowid();
                let triple_tokens = Self::bigram_tokenize(&result.text);
                let tx = conn
                    .transaction()
                    .map_err(|e| format!("Failed to start transaction: {}", e))?;

                let mut get_stmt = tx
                    .prepare_cached(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                    )
                    .map_err(|e| format!("Failed to prepare get statement: {}", e))?;
                let mut put_stmt = tx.prepare_cached(
                    "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)"
                ).map_err(|e| format!("Failed to prepare put statement: {}", e))?;

                for token in triple_tokens {
                    let token_hash = Self::compute_hmac_hash(&token);
                    let existing_blob: Option<Vec<u8>> = get_stmt
                        .query_row(params![&token_hash], |row| row.get(0))
                        .optional()
                        .map_err(|e| format!("Failed to query postings_blob: {}", e))?;
                    let mut bitmap = if let Some(blob) = existing_blob {
                        roaring::RoaringBitmap::deserialize_from(&blob[..])
                            .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?
                    } else {
                        roaring::RoaringBitmap::new()
                    };

                    bitmap.insert(ocr_id as u32);

                    let mut serialized_blob = Vec::new();
                    bitmap
                        .serialize_into(&mut serialized_blob)
                        .map_err(|e| format!("Failed to serialize bitmap: {}", e))?;

                    put_stmt
                        .execute(params![&token_hash, &serialized_blob])
                        .map_err(|e| format!("Failed to update blind bitmap index: {}", e))?;
                }

                drop(put_stmt);
                drop(get_stmt);

                tx.commit()
                    .map_err(|e| format!("Failed to commit bitmap index: {}", e))?;
                total_bitmap_dur += tb0.elapsed();

                added += 1;
                self.ocr_row_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Mark committed and set committed_at, update image_path to renamed path
        conn.execute(
            "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP, category = ?, category_confidence = ? WHERE id = ?",
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
                "UPDATE screenshots SET category = ?, category_confidence = ? WHERE id = ?",
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
                "SELECT DISTINCT category FROM screenshots WHERE category IS NOT NULL AND category != '' ORDER BY category",
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
                "SELECT image_hash, category FROM screenshots WHERE image_hash IN ({})",
                placeholders.join(",")
            );
            let params: Vec<&dyn rusqlite::ToSql> = chunk
                .iter()
                .map(|h| h as &dyn rusqlite::ToSql)
                .collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare batch query: {}", e))?;

            let rows = stmt
                .query_map(params.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
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
                "SELECT image_path FROM screenshots WHERE id = ?",
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
            let fname = image_path.file_name().and_then(|s| s.to_str()).unwrap_or("");
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
            "UPDATE screenshots SET status = ? WHERE id = ?",
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
                 WHERE s.created_at BETWEEN '{}' AND '{}'
                 ORDER BY s.created_at ASC{}",
                start_dt, end_dt, limit_clause
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows: Vec<RawScreenshotRow> = stmt
                .query_map([], |row| RawScreenshotRow::from_row(row))
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
            "SELECT COUNT(*) FROM screenshots WHERE created_at BETWEEN '{}' AND '{}'",
            start_dt, end_dt
        );

        conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
            .map_err(|e| format!("Failed to count screenshots: {}", e))
    }

    /// Get screenshot density (counts per time bucket) within a time range.
    /// No decryption or joins — extremely fast index-only scan.
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
             WHERE created_at BETWEEN '{start}' AND '{end}' \
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
                 WHERE s.created_at BETWEEN '{}' AND '{}'
                 ORDER BY s.created_at ASC
                 LIMIT {} OFFSET {}",
                start_dt, end_dt, limit, offset
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Failed to prepare query: {}", e))?;

            let rows: Vec<RawScreenshotRow> = stmt
                .query_map([], |row| RawScreenshotRow::from_row(row))
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
                 WHERE s.id = {}",
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

            let (where_clause, param_value): (&str, String) = if path.starts_with("memory://") {
                let hash = &path["memory://".len()..];
                ("WHERE s.image_hash = ?", hash.to_string())
            } else {
                ("WHERE s.image_path = ?", path.to_string())
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
                 FROM ocr_results WHERE screenshot_id = ?
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
                "SELECT image_path FROM screenshots WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .ok();

        // Count OCR rows that will be cascade-deleted
        let ocr_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id = ?",
                [id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Delete database record
        let deleted = conn
            .execute("DELETE FROM screenshots WHERE id = ?", [id])
            .map_err(|e| format!("Failed to delete screenshot: {}", e))?;

        // Try to delete image file
        if deleted > 0 {
            if let Some(path) = image_path {
                let abs_path = self.resolve_image_path(&path);
                let _ = std::fs::remove_file(&abs_path);
                let thumb = Self::thumbnail_path_for(&abs_path);
                let _ = std::fs::remove_file(&thumb);
            }
            let _ = self.ocr_row_count.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |v| Some(v.saturating_sub(ocr_count as u64)),
            );
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
            .prepare("SELECT image_path FROM screenshots WHERE created_at BETWEEN ? AND ?")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let paths: Vec<String> = stmt
            .query_map([&start_dt, &end_dt], |row| row.get(0))
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // Count OCR rows that will be cascade-deleted
        let ocr_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE screenshot_id IN (SELECT id FROM screenshots WHERE created_at BETWEEN ? AND ?)",
                [&start_dt, &end_dt],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Delete database records
        let deleted = conn
            .execute(
                "DELETE FROM screenshots WHERE created_at BETWEEN ? AND ?",
                [&start_dt, &end_dt],
            )
            .map_err(|e| format!("Failed to delete screenshots: {}", e))?;

        if deleted > 0 {
            let _ = self.ocr_row_count.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |v| Some(v.saturating_sub(ocr_count as u64)),
            );
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

    /// Canonicalize a list of visible links into a deterministic JSON string.
    /// Links are sorted by (url, text) to ensure the same set always produces the same hash.
    fn canonicalize_links(links: &[super::VisibleLink]) -> String {
        let mut sorted: Vec<(&str, &str)> = links.iter().map(|l| (l.url.as_str(), l.text.as_str())).collect();
        sorted.sort();
        serde_json::to_string(&sorted).unwrap_or_default()
    }

    /// Get or create a page_icon dedup entry. Returns the row ID.
    /// Uses INSERT OR IGNORE + SELECT pattern for atomic upsert.
    fn get_or_create_page_icon(
        conn: &Connection,
        plaintext: &str,
    ) -> Result<i64, String> {
        let content_hash = Self::compute_hmac_hash(plaintext);

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
    fn get_or_create_link_set(
        conn: &Connection,
        links: &[super::VisibleLink],
    ) -> Result<i64, String> {
        let canonical = Self::canonicalize_links(links);
        let content_hash = Self::compute_hmac_hash(&canonical);

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
            "DELETE FROM page_icons WHERE id NOT IN (SELECT DISTINCT page_icon_id FROM screenshots WHERE page_icon_id IS NOT NULL);
             DELETE FROM link_sets WHERE id NOT IN (SELECT DISTINCT link_set_id FROM screenshots WHERE link_set_id IS NOT NULL);"
        )
        .map_err(|e| format!("Failed to cleanup orphaned dedup entries: {}", e))?;
        Ok(())
    }

    /// Migrate existing inline page_icon_enc / visible_links_enc data into dedup tables.
    ///
    /// Processes rows in batches: for each row with inline data but no dedup reference,
    /// decrypts the inline blob, creates/reuses a dedup entry, sets the FK, and NULLs
    /// the inline column. Safe to call multiple times (idempotent).
    pub fn migrate_inline_to_dedup(&self) -> Result<(usize, usize), String> {
        const BATCH_SIZE: i64 = 100;
        let mut migrated_icons: usize = 0;
        let mut migrated_links: usize = 0;

        loop {
            let mut guard = self.get_connection_named("migrate_inline_to_dedup")?;
            let conn = guard.as_mut().unwrap();

            // Fetch a batch of rows that still have inline page_icon_enc but no dedup ref
            let mut stmt = conn
                .prepare(
                    "SELECT id, page_icon_enc, content_key_encrypted
                     FROM screenshots
                     WHERE page_icon_enc IS NOT NULL AND page_icon_id IS NULL
                     LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare migration query (icons): {}", e))?;

            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
                .query_map(params![BATCH_SIZE], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| format!("Failed to query migration rows (icons): {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            if rows.is_empty() {
                break;
            }

            for (id, icon_enc, content_key_enc) in &rows {
                // Decrypt inline data using the screenshot's row key
                let row_key = match decrypt_row_key_with_cng(content_key_enc) {
                    Ok(k) => k,
                    Err(_) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt row key for screenshot id={}", id);
                        continue;
                    }
                };
                let plaintext = match decrypt_with_master_key(&row_key, icon_enc) {
                    Ok(d) => match String::from_utf8(d) {
                        Ok(s) => s,
                        Err(_) => continue,
                    },
                    Err(_) => continue,
                };

                if plaintext.is_empty() {
                    // Just NULL the inline column for empty values
                    let _ = conn.execute(
                        "UPDATE screenshots SET page_icon_enc = NULL WHERE id = ?",
                        params![id],
                    );
                    continue;
                }

                // Create/reuse dedup entry and update screenshot
                match Self::get_or_create_page_icon(conn, &plaintext) {
                    Ok(icon_id) => {
                        conn.execute(
                            "UPDATE screenshots SET page_icon_id = ?, page_icon_enc = NULL WHERE id = ?",
                            params![icon_id, id],
                        )
                        .map_err(|e| format!("Failed to update screenshot {}: {}", id, e))?;
                        migrated_icons += 1;
                    }
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to create page_icon for screenshot id={}: {}", id, e);
                    }
                }
            }

            // If we got fewer than BATCH_SIZE, we're done
            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }

        // Now migrate visible_links_enc
        loop {
            let mut guard = self.get_connection_named("migrate_inline_to_dedup")?;
            let conn = guard.as_mut().unwrap();

            let mut stmt = conn
                .prepare(
                    "SELECT id, visible_links_enc, content_key_encrypted
                     FROM screenshots
                     WHERE visible_links_enc IS NOT NULL AND link_set_id IS NULL
                     LIMIT ?",
                )
                .map_err(|e| format!("Failed to prepare migration query (links): {}", e))?;

            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = stmt
                .query_map(params![BATCH_SIZE], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| format!("Failed to query migration rows (links): {}", e))?
                .filter_map(|r| r.ok())
                .collect();

            if rows.is_empty() {
                break;
            }

            for (id, links_enc, content_key_enc) in &rows {
                let row_key = match decrypt_row_key_with_cng(content_key_enc) {
                    Ok(k) => k,
                    Err(_) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to decrypt row key for screenshot id={}", id);
                        continue;
                    }
                };
                let plaintext = match decrypt_with_master_key(&row_key, links_enc) {
                    Ok(d) => match String::from_utf8(d) {
                        Ok(s) => s,
                        Err(_) => continue,
                    },
                    Err(_) => continue,
                };

                if plaintext.is_empty() {
                    let _ = conn.execute(
                        "UPDATE screenshots SET visible_links_enc = NULL WHERE id = ?",
                        params![id],
                    );
                    continue;
                }

                // Parse links JSON
                let links: Vec<super::VisibleLink> = match serde_json::from_str(&plaintext) {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to parse visible_links JSON for screenshot id={}", id);
                        continue;
                    }
                };

                if links.is_empty() {
                    let _ = conn.execute(
                        "UPDATE screenshots SET visible_links_enc = NULL WHERE id = ?",
                        params![id],
                    );
                    continue;
                }

                match Self::get_or_create_link_set(conn, &links) {
                    Ok(set_id) => {
                        conn.execute(
                            "UPDATE screenshots SET link_set_id = ?, visible_links_enc = NULL WHERE id = ?",
                            params![set_id, id],
                        )
                        .map_err(|e| format!("Failed to update screenshot {}: {}", id, e))?;
                        migrated_links += 1;
                    }
                    Err(e) => {
                        tracing::warn!("migrate_inline_to_dedup: failed to create link_set for screenshot id={}: {}", id, e);
                    }
                }
            }

            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }

        if migrated_icons > 0 || migrated_links > 0 {
            tracing::info!(
                "[DEDUP:MIGRATE] Migrated {} page_icons, {} link_sets from inline to dedup tables",
                migrated_icons,
                migrated_links
            );
        }

        Ok((migrated_icons, migrated_links))
    }

    /// Attempt dedup migration if not already done this session.
    /// Should be called after user authentication succeeds.
    /// Uses compare_exchange to ensure it only runs once.
    pub fn try_dedup_migration(&self) {
        // Only run once per session
        if self.dedup_migrated.compare_exchange(
            false, true,
            Ordering::SeqCst, Ordering::SeqCst,
        ).is_err() {
            return;
        }

        let t0 = std::time::Instant::now();
        match self.migrate_inline_to_dedup() {
            Ok((icons, links)) => {
                if icons > 0 || links > 0 {
                    tracing::info!(
                        "[DEDUP:MIGRATE] Completed in {:?} ({} icons, {} link_sets)",
                        t0.elapsed(), icons, links
                    );
                }
            }
            Err(e) => {
                tracing::warn!("[DEDUP:MIGRATE] Migration failed (non-fatal): {}", e);
                // Reset flag so it can be retried on next auth
                self.dedup_migrated.store(false, Ordering::SeqCst);
            }
        }
    }

    // ==================== Bitmap Index Migration ====================

    /// Marker key stored in the live table after atomic swap to signal completion (legacy location).
    const BITMAP_MIGRATION_DONE_KEY_LEGACY: &'static str = "__bitmap_migration_done__";
    /// Marker key in app_metadata for bitmap migration completion.
    const BITMAP_MIGRATION_DONE_KEY: &'static str = "bitmap_migration_done";
    /// Progress key stored in the staging table (blob = 8-byte little-endian i64).
    const STAGING_LAST_ID_KEY: &'static str = "__staging_last_id__";
    /// Number of OCR rows to process per batch.
    const BITMAP_MIGRATION_BATCH: i64 = 500;

    /// Attempt bitmap index migration (punctuation cleanup) if not already done.
    /// Called after user authentication succeeds.
    pub fn try_bitmap_index_migration(&self) {
        // Session-level guard: only run once per session
        if self
            .bitmap_index_migrated
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let t0 = std::time::Instant::now();
        match self.run_bitmap_index_migration() {
            Ok(migrated) => {
                if migrated > 0 {
                    tracing::info!(
                        "[BITMAP:MIGRATE] Completed in {:?} ({} OCR rows re-indexed)",
                        t0.elapsed(),
                        migrated
                    );
                }
            }
            Err(e) => {
                tracing::warn!("[BITMAP:MIGRATE] Migration failed (non-fatal): {}", e);
                // Reset so it can be retried next session/auth
                self.bitmap_index_migrated.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Rebuild bitmap index with punctuation-free bigrams.
    fn run_bitmap_index_migration(&self) -> Result<i64, String> {
        // Check persistent completion marker — new location (app_metadata) first, then legacy
        {
            let guard = self.get_connection_named("bitmap_migrate_check")?;
            let conn = guard.as_ref().unwrap();

            let done_new: bool = conn
                .query_row(
                    "SELECT 1 FROM app_metadata WHERE key = ?1",
                    params![Self::BITMAP_MIGRATION_DONE_KEY],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if done_new {
                return Ok(0);
            }

            let done_legacy: bool = conn
                .query_row(
                    "SELECT 1 FROM blind_bitmap_index WHERE token_hash = ?1",
                    params![Self::BITMAP_MIGRATION_DONE_KEY_LEGACY],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if done_legacy {
                // Migrate marker to app_metadata, then clean up legacy sentinel
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, '1')",
                    params![Self::BITMAP_MIGRATION_DONE_KEY],
                );
                let _ = conn.execute(
                    "DELETE FROM blind_bitmap_index WHERE token_hash = ?1",
                    params![Self::BITMAP_MIGRATION_DONE_KEY_LEGACY],
                );
                return Ok(0);
            }
        }

        tracing::info!("[BITMAP:MIGRATE] Processing database optimization...");

        // Read persisted progress from staging table
        let mut last_id: i64 = {
            let guard = self.get_connection_named("bitmap_migrate_progress")?;
            let conn = guard.as_ref().unwrap();
            Self::read_staging_last_id(conn)
        };

        let mut total_processed: i64 = 0;
        let report_interval = std::time::Duration::from_secs(5);
        let mut last_report = std::time::Instant::now();

        // Incremental batch loop
        loop {
            // Read OCR rows (hold DB mutex briefly)
            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = {
                let guard = self.get_connection_named("bitmap_migrate_read")?;
                let conn = guard.as_ref().unwrap();
                let mut stmt = conn
                    .prepare(
                        "SELECT id, text_enc, text_key_encrypted \
                         FROM ocr_results \
                         WHERE id > ?1 AND text_enc IS NOT NULL AND text_key_encrypted IS NOT NULL \
                         ORDER BY id ASC LIMIT ?2",
                    )
                    .map_err(|e| format!("bitmap migrate prepare: {}", e))?;
                let mapped = stmt
                    .query_map(params![last_id, Self::BITMAP_MIGRATION_BATCH], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    })
                    .map_err(|e| format!("bitmap migrate query: {}", e))?;
                mapped.filter_map(|r| r.ok()).collect()
            };

            // all rows processed
            if rows.is_empty() {
                break;
            }

            // Decrypt and tokenize
            let mut batch_tokens: std::collections::HashMap<String, roaring::RoaringBitmap> =
                std::collections::HashMap::new();
            let mut batch_max_id: i64 = last_id;

            for (ocr_id, text_enc, text_key_enc) in &rows {
                let plaintext = match self.decrypt_payload_with_row_key(text_enc, text_key_enc) {
                    Ok(bytes) => match String::from_utf8(bytes) {
                        Ok(s) => s,
                        Err(_) => continue, // skip non-utf8
                    },
                    Err(_) => continue, // skip rows that fail to decrypt
                };

                let bigrams = Self::bigram_tokenize(&plaintext);
                for token in bigrams {
                    let hash = Self::compute_hmac_hash(&token);
                    batch_tokens
                        .entry(hash)
                        .or_insert_with(roaring::RoaringBitmap::new)
                        .insert(*ocr_id as u32);
                }
                if *ocr_id > batch_max_id {
                    batch_max_id = *ocr_id;
                }
            }

            // Merge into staging table (single transaction)
            {
                let mut guard = self.get_connection_named("bitmap_migrate_write")?;
                let conn = guard.as_mut().unwrap();
                let tx = conn
                    .transaction()
                    .map_err(|e| format!("bitmap migrate tx: {}", e))?;

                {
                    let mut get_stmt = tx
                        .prepare_cached(
                            "SELECT postings_blob FROM blind_bitmap_index_staging WHERE token_hash = ?1",
                        )
                        .map_err(|e| format!("bitmap migrate prepare get: {}", e))?;
                    let mut put_stmt = tx
                        .prepare_cached(
                            "INSERT OR REPLACE INTO blind_bitmap_index_staging (token_hash, postings_blob) VALUES (?1, ?2)",
                        )
                        .map_err(|e| format!("bitmap migrate prepare put: {}", e))?;

                    for (hash, new_bitmap) in &batch_tokens {
                        let existing_blob: Option<Vec<u8>> = get_stmt
                            .query_row(params![hash], |row| row.get(0))
                            .optional()
                            .map_err(|e| format!("bitmap migrate get: {}", e))?;

                        let merged = if let Some(blob) = existing_blob {
                            let mut existing =
                                roaring::RoaringBitmap::deserialize_from(&blob[..])
                                    .map_err(|e| format!("bitmap migrate deser: {}", e))?;
                            existing |= new_bitmap;
                            existing
                        } else {
                            new_bitmap.clone()
                        };

                        let mut buf = Vec::new();
                        merged
                            .serialize_into(&mut buf)
                            .map_err(|e| format!("bitmap migrate ser: {}", e))?;
                        put_stmt
                            .execute(params![hash, buf])
                            .map_err(|e| format!("bitmap migrate put: {}", e))?;
                    }
                    drop(get_stmt);
                    drop(put_stmt);
                }

                // Update progress marker
                Self::write_staging_last_id(&tx, batch_max_id)?;

                tx.commit()
                    .map_err(|e| format!("bitmap migrate commit: {}", e))?;
            }

            total_processed += rows.len() as i64;
            last_id = batch_max_id;

            // Periodic progress report
            if last_report.elapsed() >= report_interval {
                tracing::info!(
                    "[BITMAP:MIGRATE] Progress: {} rows optimized so far (last_id={})",
                    total_processed,
                    last_id
                );
                last_report = std::time::Instant::now();
            }
        }

        // Process any rows added during migration, then atomically swap tables.
        // IMPORTANT: The final "no more rows" check and table swap MUST happen
        // under a single mutex acquisition to prevent a race where new bitmap
        // entries are inserted between the empty check and DROP TABLE.
        loop {
            // Step 1: Acquire mutex, read remaining rows, release mutex
            let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = {
                let guard = self.get_connection_named("bitmap_migrate_catchup")?;
                let conn = guard.as_ref().unwrap();
                let mut stmt = conn
                    .prepare(
                        "SELECT id, text_enc, text_key_encrypted \
                         FROM ocr_results \
                         WHERE id > ?1 AND text_enc IS NOT NULL AND text_key_encrypted IS NOT NULL \
                         ORDER BY id ASC LIMIT ?2",
                    )
                    .map_err(|e| format!("bitmap catchup prepare: {}", e))?;
                let mapped = stmt
                    .query_map(params![last_id, Self::BITMAP_MIGRATION_BATCH], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    })
                    .map_err(|e| format!("bitmap catchup query: {}", e))?;
                mapped.filter_map(|r| r.ok()).collect()
            };

            if !rows.is_empty() {
                // Step 2: Decrypt + tokenize OUTSIDE mutex (CPU-intensive, no DB needed)
                let mut batch_tokens: std::collections::HashMap<String, roaring::RoaringBitmap> =
                    std::collections::HashMap::new();
                let mut batch_max_id = last_id;

                for (ocr_id, text_enc, text_key_enc) in &rows {
                    let plaintext =
                        match self.decrypt_payload_with_row_key(text_enc, text_key_enc) {
                            Ok(bytes) => match String::from_utf8(bytes) {
                                Ok(s) => s,
                                Err(_) => continue,
                            },
                            Err(_) => continue,
                        };

                    let bigrams = Self::bigram_tokenize(&plaintext);
                    for token in bigrams {
                        let hash = Self::compute_hmac_hash(&token);
                        batch_tokens
                            .entry(hash)
                            .or_insert_with(roaring::RoaringBitmap::new)
                            .insert(*ocr_id as u32);
                    }
                    if *ocr_id > batch_max_id {
                        batch_max_id = *ocr_id;
                    }
                }

                // Step 3: Write to staging (acquire mutex briefly)
                {
                    let mut guard =
                        self.get_connection_named("bitmap_migrate_catchup_write")?;
                    let conn = guard.as_mut().unwrap();
                    let tx = conn
                        .transaction()
                        .map_err(|e| format!("bitmap catchup tx: {}", e))?;

                    {
                        let mut get_stmt = tx
                            .prepare_cached(
                                "SELECT postings_blob FROM blind_bitmap_index_staging WHERE token_hash = ?1",
                            )
                            .map_err(|e| format!("bitmap catchup prepare get: {}", e))?;
                        let mut put_stmt = tx
                            .prepare_cached(
                                "INSERT OR REPLACE INTO blind_bitmap_index_staging (token_hash, postings_blob) VALUES (?1, ?2)",
                            )
                            .map_err(|e| format!("bitmap catchup prepare put: {}", e))?;

                        for (hash, new_bitmap) in &batch_tokens {
                            let existing_blob: Option<Vec<u8>> = get_stmt
                                .query_row(params![hash], |row| row.get(0))
                                .optional()
                                .map_err(|e| format!("bitmap catchup get: {}", e))?;

                            let merged = if let Some(blob) = existing_blob {
                                let mut existing =
                                    roaring::RoaringBitmap::deserialize_from(&blob[..])
                                        .map_err(|e| format!("bitmap catchup deser: {}", e))?;
                                existing |= new_bitmap;
                                existing
                            } else {
                                new_bitmap.clone()
                            };

                            let mut buf = Vec::new();
                            merged
                                .serialize_into(&mut buf)
                                .map_err(|e| format!("bitmap catchup ser: {}", e))?;
                            put_stmt
                                .execute(params![hash, buf])
                                .map_err(|e| format!("bitmap catchup put: {}", e))?;
                        }
                        drop(get_stmt);
                        drop(put_stmt);
                    }

                    Self::write_staging_last_id(&tx, batch_max_id)?;

                    tx.commit()
                        .map_err(|e| format!("bitmap catchup commit: {}", e))?;
                }

                total_processed += rows.len() as i64;
                last_id = batch_max_id;
                continue;
            }

            // rows was empty — but mutex was already released after step 1,
            // so a concurrent insert could have added rows in the gap.
            // Step 4: FINAL — acquire mutex ONCE for re-check + swap atomically.
            // This guarantees no rows can be inserted between the check and DROP.
            {
                let mut guard = self.get_connection_named("bitmap_migrate_swap")?;
                let conn = guard.as_mut().unwrap();

                // Re-check for any rows inserted after step 1 released the mutex
                let late_rows: Vec<(i64, Vec<u8>, Vec<u8>)> = {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, text_enc, text_key_encrypted \
                             FROM ocr_results \
                             WHERE id > ?1 AND text_enc IS NOT NULL \
                             AND text_key_encrypted IS NOT NULL \
                             ORDER BY id ASC",
                        )
                        .map_err(|e| format!("bitmap swap recheck prepare: {}", e))?;
                    let mapped = stmt
                        .query_map(params![last_id], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, Vec<u8>>(1)?,
                                row.get::<_, Vec<u8>>(2)?,
                            ))
                        })
                        .map_err(|e| format!("bitmap swap recheck query: {}", e))?;
                    mapped.filter_map(|r| r.ok()).collect()
                };

                // Process any late arrivals in-place under mutex.
                // Typically 0 rows; at most 1-2 in a race scenario.
                // decrypt_payload_with_row_key uses CNG API, not DB mutex — safe.
                if !late_rows.is_empty() {
                    tracing::info!(
                        "[BITMAP:MIGRATE] Processing {} late row(s) during final swap",
                        late_rows.len()
                    );

                    let mut batch_tokens: std::collections::HashMap<
                        String,
                        roaring::RoaringBitmap,
                    > = std::collections::HashMap::new();

                    for (ocr_id, text_enc, text_key_enc) in &late_rows {
                        let plaintext =
                            match self.decrypt_payload_with_row_key(text_enc, text_key_enc)
                            {
                                Ok(bytes) => match String::from_utf8(bytes) {
                                    Ok(s) => s,
                                    Err(_) => continue,
                                },
                                Err(_) => continue,
                            };

                        let bigrams = Self::bigram_tokenize(&plaintext);
                        for token in bigrams {
                            let hash = Self::compute_hmac_hash(&token);
                            batch_tokens
                                .entry(hash)
                                .or_insert_with(roaring::RoaringBitmap::new)
                                .insert(*ocr_id as u32);
                        }
                    }

                    let tx = conn
                        .transaction()
                        .map_err(|e| format!("bitmap swap late tx: {}", e))?;
                    {
                        let mut get_stmt = tx
                            .prepare_cached(
                                "SELECT postings_blob FROM blind_bitmap_index_staging WHERE token_hash = ?1",
                            )
                            .map_err(|e| format!("bitmap swap late prepare get: {}", e))?;
                        let mut put_stmt = tx
                            .prepare_cached(
                                "INSERT OR REPLACE INTO blind_bitmap_index_staging (token_hash, postings_blob) VALUES (?1, ?2)",
                            )
                            .map_err(|e| format!("bitmap swap late prepare put: {}", e))?;

                        for (hash, new_bitmap) in &batch_tokens {
                            let existing_blob: Option<Vec<u8>> = get_stmt
                                .query_row(params![hash], |row| row.get(0))
                                .optional()
                                .map_err(|e| format!("bitmap swap late get: {}", e))?;

                            let merged = if let Some(blob) = existing_blob {
                                let mut existing =
                                    roaring::RoaringBitmap::deserialize_from(&blob[..])
                                        .map_err(|e| {
                                            format!("bitmap swap late deser: {}", e)
                                        })?;
                                existing |= new_bitmap;
                                existing
                            } else {
                                new_bitmap.clone()
                            };

                            let mut buf = Vec::new();
                            merged
                                .serialize_into(&mut buf)
                                .map_err(|e| format!("bitmap swap late ser: {}", e))?;
                            put_stmt
                                .execute(params![hash, buf])
                                .map_err(|e| format!("bitmap swap late put: {}", e))?;
                        }
                        drop(get_stmt);
                        drop(put_stmt);
                    }
                    tx.commit()
                        .map_err(|e| format!("bitmap swap late commit: {}", e))?;

                    total_processed += late_rows.len() as i64;
                }

                // Remove progress marker from staging before swap
                conn.execute(
                    "DELETE FROM blind_bitmap_index_staging WHERE token_hash = ?1",
                    params![Self::STAGING_LAST_ID_KEY],
                )
                .map_err(|e| format!("bitmap swap cleanup: {}", e))?;

                // Atomic swap — guaranteed no concurrent inserts (mutex held)
                conn.execute_batch(
                    "BEGIN TRANSACTION;
                     DROP TABLE blind_bitmap_index;
                     ALTER TABLE blind_bitmap_index_staging RENAME TO blind_bitmap_index;
                     COMMIT;",
                )
                .map_err(|e| format!("bitmap swap failed: {}", e))?;

                // Insert completion marker into app_metadata
                conn.execute(
                    "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, '1')",
                    params![Self::BITMAP_MIGRATION_DONE_KEY],
                )
                .map_err(|e| format!("bitmap done marker insert: {}", e))?;
            } // mutex released — swap complete
            break;
        }

        if total_processed > 0 {
            tracing::info!(
                "[BITMAP:MIGRATE] Atomic swap complete. {} OCR rows re-indexed with clean bigrams.",
                total_processed
            );
        }

        Ok(total_processed)
    }

    /// Read the last-processed OCR id from the staging table.
    fn read_staging_last_id(conn: &Connection) -> i64 {
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT postings_blob FROM blind_bitmap_index_staging WHERE token_hash = ?1",
                params![Self::STAGING_LAST_ID_KEY],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();

        match blob {
            Some(b) if b.len() == 8 => i64::from_le_bytes(b.try_into().unwrap()),
            _ => 0,
        }
    }

    /// Write the last-processed OCR id into the staging table.
    fn write_staging_last_id(tx: &rusqlite::Transaction, id: i64) -> Result<(), String> {
        tx.execute(
            "INSERT OR REPLACE INTO blind_bitmap_index_staging (token_hash, postings_blob) VALUES (?1, ?2)",
            params![Self::STAGING_LAST_ID_KEY, id.to_le_bytes().to_vec()],
        )
        .map_err(|e| format!("bitmap write progress: {}", e))?;
        Ok(())
    }
}
