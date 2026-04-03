//! Image reading with decryption support.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key, encrypt_with_master_key};
use std::path::{Path, PathBuf};

use super::StorageState;

impl StorageState {
    /// Read an encrypted image file and return Base64-encoded data.
    pub fn read_image(&self, path: &str) -> Result<(String, String), String> {
        let diag_start = std::time::Instant::now();

        // Phase 1: Hold mutex only for DB query to get the encrypted key
        let (key_enc, abs_path) = {
            let guard = self.get_connection_named("read_image")?;
            let conn = guard.as_ref().unwrap();

            if let Some(hash) = path.strip_prefix("memory://") {
                // 旧数据兼容：从 memory:// 中提取 hash 查找
                let result: Option<(Option<Vec<u8>>, String)> = conn
                    .query_row(
                        "SELECT content_key_encrypted, image_path FROM screenshots WHERE image_hash = ?",
                        [hash],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .ok();
                match result {
                    Some((key, real_path)) => (key, self.resolve_image_path(&real_path)),
                    None => return Err(format!("No screenshot found for hash: {}", hash)),
                }
            } else {
                // 正常路径查找（原有逻辑）
                let key: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT content_key_encrypted FROM screenshots WHERE image_path = ?",
                        [path],
                        |row| row.get(0),
                    )
                    .ok();

                let resolved = self.resolve_image_path(path);
                (key, resolved)
            }
            // guard dropped here, mutex released
        };

        let query_elapsed = diag_start.elapsed();

        // Phase 2: CNG decrypt + file read + AES decrypt + base64 — all outside mutex
        let mut row_key = key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(enc).ok())
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let abs_path_str = abs_path.to_string_lossy().to_string();
        let result = read_encrypted_image_as_base64(&abs_path_str, &row_key);
        Self::zeroize_bytes(&mut row_key);
        if diag_start.elapsed().as_secs() >= 5 {
            tracing::warn!(
                "[DIAG:DB] read_image({}) query {:?}, total {:?}",
                path,
                query_elapsed,
                diag_start.elapsed()
            );
        }
        result
    }

    /// Derive the cached thumbnail path from an original image path.
    /// E.g. `screenshots/foo.png.enc` → `screenshots/thumbs/foo.thumb.jpg.enc`
    pub fn thumbnail_path_for(original: &Path) -> PathBuf {
        let parent = original.parent().unwrap_or_else(|| Path::new("."));
        let thumbs_dir = parent.join("thumbs");
        let stem = original
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        // Strip all extensions to get the base stem (e.g. "img.png.enc" → "img")
        let base_stem = if let Some(pos) = stem.find('.') {
            &stem[..pos]
        } else {
            stem
        };
        thumbs_dir.join(format!("{}.thumb.jpg.enc", base_stem))
    }

    /// Read a thumbnail for an image, generating and caching it on first request.
    /// Returns base64-encoded JPEG data with MIME type.
    pub fn read_thumbnail(&self, path: &str) -> Result<(String, String), String> {
        // Phase 1: DB query for the encrypted row key
        let (key_enc, abs_path) = {
            let guard = self.get_connection_named("read_thumbnail")?;
            let conn = guard.as_ref().unwrap();

            if let Some(hash) = path.strip_prefix("memory://") {
                let result: Option<(Option<Vec<u8>>, String)> = conn
                    .query_row(
                        "SELECT content_key_encrypted, image_path FROM screenshots WHERE image_hash = ?",
                        [hash],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .ok();
                match result {
                    Some((key, real_path)) => (key, self.resolve_image_path(&real_path)),
                    None => return Err(format!("No screenshot found for hash: {}", hash)),
                }
            } else {
                let key: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT content_key_encrypted FROM screenshots WHERE image_path = ?",
                        [path],
                        |row| row.get(0),
                    )
                    .ok();
                let resolved = self.resolve_image_path(path);
                (key, resolved)
            }
        };

        // Phase 2: Decrypt row key
        let mut row_key = key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(enc).ok())
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let thumb_path = Self::thumbnail_path_for(&abs_path);

        // Phase 3: Try reading cached thumbnail
        if thumb_path.exists() {
            match self.try_read_cached_thumbnail(&thumb_path, &row_key) {
                Ok(result) => {
                    Self::zeroize_bytes(&mut row_key);
                    return Ok(result);
                }
                Err(_) => {
                    // Corrupt cache — delete and regenerate
                    let _ = std::fs::remove_file(&thumb_path);
                }
            }
        }

        // Phase 4: Generate thumbnail from full image
        let abs_path_str = abs_path.to_string_lossy().to_string();
        let result = self.generate_and_cache_thumbnail(&abs_path_str, &thumb_path, &row_key);
        Self::zeroize_bytes(&mut row_key);
        result
    }

    /// Generate and cache a thumbnail if not already cached.
    /// Returns Ok(true) if generated, Ok(false) if already cached.
    pub fn ensure_thumbnail_cached(&self, path: &str) -> Result<bool, String> {
        // Phase 1: Quick file-existence check (no DB, no mutex)
        // For memory:// paths we must query DB first to resolve the real path,
        // but normal relative paths can be resolved without DB.
        if !path.starts_with("memory://") {
            let abs_path = self.resolve_image_path(path);
            let thumb_path = Self::thumbnail_path_for(&abs_path);
            if thumb_path.exists() {
                return Ok(false);
            }
        }

        // Phase 2: DB query for the encrypted row key (only reached when thumbnail is missing)
        let (key_enc, abs_path) = {
            let guard = self.get_connection_named("ensure_thumbnail_cached")?;
            let conn = guard.as_ref().unwrap();

            if let Some(hash) = path.strip_prefix("memory://") {
                let result: Option<(Option<Vec<u8>>, String)> = conn
                    .query_row(
                        "SELECT content_key_encrypted, image_path FROM screenshots WHERE image_hash = ?",
                        [hash],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .ok();
                match result {
                    Some((key, real_path)) => (key, self.resolve_image_path(&real_path)),
                    None => return Err(format!("No screenshot found for hash: {}", hash)),
                }
            } else {
                let key: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT content_key_encrypted FROM screenshots WHERE image_path = ?",
                        [path],
                        |row| row.get(0),
                    )
                    .ok();
                let resolved = self.resolve_image_path(path);
                (key, resolved)
            }
        };

        // Re-check for memory:// paths (resolved above)
        let thumb_path = Self::thumbnail_path_for(&abs_path);
        if thumb_path.exists() {
            return Ok(false);
        }

        // Phase 3: Decrypt row key and generate thumbnail
        let mut row_key = key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(enc).ok())
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let abs_path_str = abs_path.to_string_lossy().to_string();
        let result = self.generate_and_cache_thumbnail(&abs_path_str, &thumb_path, &row_key);
        Self::zeroize_bytes(&mut row_key);
        result.map(|_| true)
    }

    /// Batch read thumbnails by screenshot IDs.
    /// Does batch id→path lookup and then batch thumbnail reading, all within the storage module.
    /// Returns a map of id (as string) → Result with base64/mime or error.
    pub fn batch_read_thumbnails_by_ids(
        &self,
        ids: &[i64],
    ) -> std::collections::HashMap<String, Result<(String, String), String>> {
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }

        // Batch id → image_path lookup (single DB query)
        let id_path_map: std::collections::HashMap<i64, String> = {
            let guard = match self.get_connection_named("batch_thumbnails_by_ids") {
                Ok(g) => g,
                Err(e) => {
                    return ids
                        .iter()
                        .map(|id| (id.to_string(), Err(format!("DB connection error: {}", e))))
                        .collect();
                }
            };
            let conn = guard.as_ref().unwrap();

            let mut map = std::collections::HashMap::new();
            for chunk in ids.chunks(500) {
                let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
                let sql = format!(
                    "SELECT id, image_path FROM screenshots WHERE id IN ({})",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> = chunk
                    .iter()
                    .map(|id| id as &dyn rusqlite::ToSql)
                    .collect();

                if let Ok(mut stmt) = conn.prepare(&sql) {
                    if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                    }) {
                        for row in rows.filter_map(|r| r.ok()) {
                            map.insert(row.0, row.1);
                        }
                    }
                }
            }
            map
            // guard dropped here
        };

        // Collect paths and batch read thumbnails
        let paths: Vec<String> = ids
            .iter()
            .filter_map(|id| id_path_map.get(id).cloned())
            .collect();

        let thumb_results = self.batch_read_thumbnails(&paths);

        // Build path → result lookup
        let path_result_map: std::collections::HashMap<&str, &Result<(String, String), String>> =
            thumb_results
                .iter()
                .map(|(p, r)| (p.as_str(), r))
                .collect();

        // Build result keyed by id string
        let mut results = std::collections::HashMap::new();
        for id in ids {
            let result = if let Some(path) = id_path_map.get(id) {
                if let Some(r) = path_result_map.get(path.as_str()) {
                    (*r).clone()
                } else {
                    Err("Thumbnail processing failed".to_string())
                }
            } else {
                Err("Screenshot not found".to_string())
            };
            results.insert(id.to_string(), result);
        }

        results
    }

    /// Batch read thumbnails for multiple image paths in a single operation.
    /// Does one DB query to fetch all encrypted keys, then processes each thumbnail outside the lock.
    /// Returns a Vec of (path, Result<(base64, mime_type), error_string>).
    pub fn batch_read_thumbnails(
        &self,
        paths: &[String],
    ) -> Vec<(String, Result<(String, String), String>)> {
        if paths.is_empty() {
            return Vec::new();
        }

        // Single DB query to get all content_key_encrypted values
        let key_map: std::collections::HashMap<String, Option<Vec<u8>>> = {
            let guard = match self.get_connection_named("batch_read_thumbnails") {
                Ok(g) => g,
                Err(e) => {
                    return paths
                        .iter()
                        .map(|p| (p.clone(), Err(format!("DB connection error: {}", e))))
                        .collect();
                }
            };
            let conn = guard.as_ref().unwrap();

            let mut map = std::collections::HashMap::new();
            for chunk in paths.chunks(500) {
                let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
                let sql = format!(
                    "SELECT image_path, content_key_encrypted FROM screenshots WHERE image_path IN ({})",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> = chunk
                    .iter()
                    .map(|p| p as &dyn rusqlite::ToSql)
                    .collect();

                if let Ok(mut stmt) = conn.prepare(&sql) {
                    if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                        ))
                    }) {
                        for row in rows.filter_map(|r| r.ok()) {
                            map.insert(row.0, row.1);
                        }
                    }
                }
            }
            map
            // guard dropped here, mutex released
        };

        // Process each path outside the lock
        paths
            .iter()
            .map(|path| {
                let result = (|| -> Result<(String, String), String> {
                    let key_enc = key_map
                        .get(path)
                        .ok_or_else(|| format!("No screenshot found for path: {}", path))?;

                    let mut row_key = key_enc
                        .as_ref()
                        .and_then(|enc| decrypt_row_key_with_cng(enc).ok())
                        .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

                    let abs_path = self.resolve_image_path(path);
                    let thumb_path = Self::thumbnail_path_for(&abs_path);

                    let result = if thumb_path.exists() {
                        match self.try_read_cached_thumbnail(&thumb_path, &row_key) {
                            Ok(r) => Ok(r),
                            Err(_) => {
                                let _ = std::fs::remove_file(&thumb_path);
                                let abs_path_str = abs_path.to_string_lossy().to_string();
                                self.generate_and_cache_thumbnail(
                                    &abs_path_str,
                                    &thumb_path,
                                    &row_key,
                                )
                            }
                        }
                    } else {
                        let abs_path_str = abs_path.to_string_lossy().to_string();
                        self.generate_and_cache_thumbnail(&abs_path_str, &thumb_path, &row_key)
                    };

                    Self::zeroize_bytes(&mut row_key);
                    result
                })();
                (path.clone(), result)
            })
            .collect()
    }

    // ==================== Thumbnail Warmup Sentinel ====================

    const THUMBNAIL_WARMUP_DONE_KEY: &'static str = "thumbnail_warmup_done";

    /// Check whether thumbnail warmup has already been completed (persistent marker in app_metadata).
    pub fn is_thumbnail_warmup_done(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("thumb_warmup_check")?;
        let conn = guard.as_ref().unwrap();
        let done: bool = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                rusqlite::params![Self::THUMBNAIL_WARMUP_DONE_KEY],
                |_| Ok(true),
            )
            .unwrap_or(false);
        Ok(done)
    }

    /// Write the persistent marker for thumbnail warmup completion.
    pub fn mark_thumbnail_warmup_done(&self) -> Result<(), String> {
        let guard = self.get_connection_named("thumb_warmup_mark")?;
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, '1')",
            rusqlite::params![Self::THUMBNAIL_WARMUP_DONE_KEY],
        )
        .map_err(|e| format!("Failed to mark thumbnail warmup done: {}", e))?;
        Ok(())
    }

    fn try_read_cached_thumbnail(
        &self,
        thumb_path: &Path,
        row_key: &[u8],
    ) -> Result<(String, String), String> {
        let data = std::fs::read(thumb_path)
            .map_err(|e| format!("Failed to read thumbnail cache: {}", e))?;
        let decrypted = decrypt_with_master_key(row_key, &data)
            .map_err(|e| format!("Failed to decrypt thumbnail: {}", e))?;
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &decrypted);
        Ok((b64, "image/jpeg".to_string()))
    }

    /// Generate and cache a thumbnail directly from already-decoded image bytes.
    /// This avoids re-reading and re-decrypting the original file.
    pub fn generate_thumbnail_from_data(
        &self,
        image_data: &[u8],
        image_path: &Path,
        row_key: &[u8],
    ) -> Result<(), String> {
        let thumb_path = Self::thumbnail_path_for(image_path);
        if thumb_path.exists() {
            return Ok(());
        }

        let img = image::load_from_memory(image_data)
            .map_err(|e| format!("Failed to decode image for thumbnail: {}", e))?;
        let thumb = img.thumbnail(192, 192);

        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        thumb
            .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
            .map_err(|e| format!("Failed to encode thumbnail: {}", e))?;
        let jpeg_bytes = jpeg_buf.into_inner();

        let encrypted = encrypt_with_master_key(row_key, &jpeg_bytes)
            .map_err(|e| format!("Failed to encrypt thumbnail: {}", e))?;
        if let Some(parent) = thumb_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&thumb_path, &encrypted);

        Ok(())
    }

    fn generate_and_cache_thumbnail(
        &self,
        original_path: &str,
        thumb_path: &Path,
        row_key: &[u8],
    ) -> Result<(String, String), String> {
        let original = Path::new(original_path);
        if !original.exists() {
            return Err(format!("Image file not found: {}", original_path));
        }

        // Read and decrypt original image
        let raw_data =
            std::fs::read(original).map_err(|e| format!("Failed to read image: {}", e))?;
        let fname = original
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let is_encrypted = fname.contains(".enc");
        let image_data = if is_encrypted {
            decrypt_with_master_key(row_key, &raw_data)
                .map_err(|e| format!("Failed to decrypt image: {}", e))?
        } else {
            raw_data
        };

        // Decode and resize
        let img = image::load_from_memory(&image_data)
            .map_err(|e| format!("Failed to decode image: {}", e))?;
        let thumb = img.thumbnail(192, 192);

        // Encode as JPEG
        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        thumb
            .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
            .map_err(|e| format!("Failed to encode thumbnail: {}", e))?;
        let jpeg_bytes = jpeg_buf.into_inner();

        // Encrypt and write to cache
        let encrypted = encrypt_with_master_key(row_key, &jpeg_bytes)
            .map_err(|e| format!("Failed to encrypt thumbnail: {}", e))?;
        if let Some(parent) = thumb_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(thumb_path, &encrypted);

        // Return base64
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg_bytes);
        Ok((b64, "image/jpeg".to_string()))
    }
}

/// Read an image file and return Base64-encoded data (supports encrypted files).
#[allow(dead_code)]
pub fn read_image_as_base64(path: &str) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;

    // Detect encrypted file: filename contains ".enc" (compatible with .enc.pending)
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // Determine MIME type from base filename (strip .enc/.pending suffixes)
    let base_name = if is_encrypted {
        if let Some(pos) = fname.find(".enc") {
            &fname[..pos]
        } else {
            fname
        }
    } else {
        fname
    };

    let mime_type = if base_name.ends_with(".png") {
        "image/png"
    } else if base_name.ends_with(".jpg") || base_name.ends_with(".jpeg") {
        "image/jpeg"
    } else if base_name.ends_with(".gif") {
        "image/gif"
    } else if base_name.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };

    // Encrypted files need a decryption key; return error for this codepath
    if is_encrypted {
        return Err(
            "Encrypted image requires decryption key. Use read_encrypted_image_as_base64 instead."
                .to_string(),
        );
    }

    let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    Ok((base64_data, mime_type.to_string()))
}

/// Read an encrypted image file and return Base64-encoded data (with decryption).
pub fn read_encrypted_image_as_base64(
    path: &str,
    row_key: &[u8],
) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;
    // Detect encrypted file: filename contains ".enc" (compatible with .enc.pending)
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // Determine MIME type from base filename (strip .enc/.pending suffixes)
    let base_name = if is_encrypted {
        if let Some(pos) = fname.find(".enc") {
            &fname[..pos]
        } else {
            fname
        }
    } else {
        fname
    };

    let mime_type = if base_name.ends_with(".png") {
        "image/png"
    } else if base_name.ends_with(".jpg") || base_name.ends_with(".jpeg") {
        "image/jpeg"
    } else if base_name.ends_with(".gif") {
        "image/gif"
    } else if base_name.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };

    let image_data = if is_encrypted {
        // Decrypt file contents
        decrypt_with_master_key(row_key, &data)
            .map_err(|e| format!("Failed to decrypt image: {}", e))?
    } else {
        data
    };

    let base64_data =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image_data);

    Ok((base64_data, mime_type.to_string()))
}
