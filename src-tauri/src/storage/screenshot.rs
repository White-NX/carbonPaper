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
    OcrResultInput, SaveScreenshotRequest, SaveScreenshotResponse, ScreenshotRecord, StorageState,
};

impl StorageState {
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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let image_path = screenshot_dir.join(&filename);

        // Save encrypted image file
        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = self.to_relative_image_path(&image_path);

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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let image_path = screenshot_dir.join(&filename);

        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;
        let file_write_dur = t2.elapsed();

        let image_path_str = self.to_relative_image_path(&image_path);

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
            "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![final_image_path_str, "committed", screenshot_id],
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
                        ls.links_enc, ls.links_key_encrypted
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
                        ls.links_enc, ls.links_key_encrypted
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
}
