//! Image reading with decryption support.

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_row_key_with_cng_measured, decrypt_row_key_with_cng_silent,
    decrypt_with_master_key, encrypt_with_master_key, CredentialError, RowKeyDecryptTimings,
};
use rusqlite::{Connection, OptionalExtension, ToSql};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{BackgroundReadError, StorageState};

const IMAGE_BY_ID_SQL: &str = "SELECT content_key_encrypted, image_path FROM screenshots
    WHERE id = ?1 AND is_deleted = 0";
const IMAGE_BY_PATH_SQL: &str = "SELECT content_key_encrypted, image_path FROM screenshots
    WHERE image_path = ?1 AND is_deleted = 0";
const IMAGE_BY_HASH_SQL: &str = "SELECT content_key_encrypted, image_path FROM screenshots
    WHERE image_hash = ?1 AND is_deleted = 0";

#[derive(Clone, Copy)]
enum ImageLookup<'a> {
    Id(i64),
    Path(&'a str),
    Hash(&'a str),
}

#[derive(Debug)]
struct ImageLocation {
    key_enc: Option<Vec<u8>>,
    path: String,
}

impl ImageLookup<'_> {
    fn from_path(path: &str) -> ImageLookup<'_> {
        match path.strip_prefix("memory://") {
            Some(hash) => ImageLookup::Hash(hash),
            None => ImageLookup::Path(path),
        }
    }

    fn sql(&self) -> &'static str {
        match self {
            Self::Id(_) => IMAGE_BY_ID_SQL,
            Self::Path(_) => IMAGE_BY_PATH_SQL,
            Self::Hash(_) => IMAGE_BY_HASH_SQL,
        }
    }

    fn parameter(&self) -> &dyn ToSql {
        match self {
            Self::Id(id) => id,
            Self::Path(path) | Self::Hash(path) => path,
        }
    }

    /// Fetch only the file location and opaque wrapped key, without decrypting metadata.
    fn query(&self, conn: &Connection) -> Result<ImageLocation, String> {
        let mut statement = conn
            .prepare_cached(self.sql())
            .map_err(|error| format!("Failed to prepare image key query: {error}"))?;
        statement
            .query_row([self.parameter()], |row| {
                Ok(ImageLocation {
                    key_enc: row.get(0)?,
                    path: row.get(1)?,
                })
            })
            .optional()
            .map_err(|error| format!("Failed to query image key: {error}"))?
            .ok_or_else(|| match self {
                Self::Id(_) => "Image not found".to_string(),
                Self::Hash(hash) => format!("No screenshot found for hash: {hash}"),
                Self::Path(_) => "Failed to unwrap image row key".to_string(),
            })
    }
}

#[derive(Default)]
struct ImageFileReadTimings {
    file_read: Duration,
    decrypt: Duration,
}

fn image_locations_by_ids(
    conn: &Connection,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, ImageLocation>, String> {
    let mut locations = std::collections::HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let ids = ids
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect::<Vec<_>>();
    for chunk in ids.chunks(500) {
        // NOT INDEXED still permits INTEGER PRIMARY KEY seeks. It keeps
        // competing live-row indexes from turning an ID list into a scan.
        let sql = format!(
            "SELECT id, content_key_encrypted, image_path FROM screenshots NOT INDEXED
             WHERE is_deleted = 0 AND id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let mut statement = conn.prepare(&sql).map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ImageLocation {
                        key_enc: row.get(1)?,
                        path: row.get(2)?,
                    },
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (id, location) = row.map_err(|error| error.to_string())?;
            locations.insert(id, location);
        }
    }
    Ok(locations)
}

impl StorageState {
    /// Read an encrypted image file and return Base64-encoded data.
    pub fn read_image(&self, path: &str) -> Result<(String, String), String> {
        self.read_image_with_lookup(ImageLookup::from_path(path))
    }

    /// Load a full screenshot by its primary key without hydrating its metadata.
    pub fn read_image_by_id(&self, id: i64) -> Result<(String, String), String> {
        self.read_image_with_lookup(ImageLookup::Id(id))
    }

    fn read_image_with_lookup(&self, lookup: ImageLookup<'_>) -> Result<(String, String), String> {
        let started = Instant::now();
        // Keep the database and directory stable while using the fetched file location.
        // This shared maintenance permit does not serialize other image readers.
        let _activity = self.foreground_db_read();
        let maintenance_wait = started.elapsed();
        let acquiring = Instant::now();
        let primary = self.try_get_connection_named("read_image")?;
        let db_wait = acquiring.elapsed();
        let (location, read_open, db_query, db_connection) = if let Some(guard) = primary {
            let querying = Instant::now();
            let location = lookup.query(guard.as_ref().unwrap())?;
            (location, Duration::ZERO, querying.elapsed(), "primary")
        } else {
            let opening = Instant::now();
            let conn = self.open_read_connection_named("read_image")?;
            let read_open = opening.elapsed();
            let querying = Instant::now();
            let location = lookup.query(&conn)?;
            (location, read_open, querying.elapsed(), "independent")
        };

        // Path resolution, CNG, file I/O and decoding all run after releasing the DB mutex.
        let resolve_started = Instant::now();
        let abs_path = self.resolve_image_path(&location.path);
        let path_resolve = resolve_started.elapsed();
        let encrypted_key = location
            .key_enc
            .as_ref()
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;
        let mut cng = RowKeyDecryptTimings::default();
        let mut row_key = decrypt_row_key_with_cng_measured(
            &self.credential_state,
            encrypted_key,
            &mut cng,
            &|| false,
        )
        .ok()
        .flatten()
        .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let abs_path_str = abs_path.to_string_lossy().to_string();
        let mut file_timings = ImageFileReadTimings::default();
        let mut base64_elapsed = Duration::ZERO;
        let result = (|| -> Result<(String, String), String> {
            let (data, mime_type) =
                read_encrypted_image_bytes_measured(&abs_path_str, &row_key, &mut file_timings)?;
            let base64_started = Instant::now();
            let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
            base64_elapsed = base64_started.elapsed();
            Ok((encoded, mime_type))
        })();
        Self::zeroize_bytes(&mut row_key);
        let total = started.elapsed();
        if total >= Duration::from_millis(250) {
            tracing::warn!(
                ?maintenance_wait,
                ?db_wait,
                db_connection,
                ?read_open,
                ?db_query,
                ?path_resolve,
                cng_calls = cng.calls,
                cng_wait = ?cng.lock_wait,
                cng_decrypt = ?cng.decrypt,
                file_read = ?file_timings.file_read,
                file_decrypt = ?file_timings.decrypt,
                base64 = ?base64_elapsed,
                ?total,
                "[DIAG:IMAGE] read_image timing"
            );
        }
        result
    }

    /// Read image bytes for unattended work without allowing CNG to display UI.
    pub(crate) fn read_image_bytes_silent(
        &self,
        path: &str,
    ) -> Result<(Vec<u8>, String), BackgroundReadError> {
        // Keep the authorization check at the helper boundary as well as at
        // scheduler call sites. The preference/lease can be revoked between
        // a caller's admission check and the first database read.
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        let diag_start = std::time::Instant::now();

        let (location, abs_path) = {
            let guard = self
                .get_connection_named("read_image_bytes_silent")
                .map_err(BackgroundReadError::Other)?;
            let location = ImageLookup::from_path(path)
                .query(guard.as_ref().unwrap())
                .map_err(BackgroundReadError::Other)?;
            let abs_path = self.resolve_image_path(&location.path);
            (location, abs_path)
        };

        let query_elapsed = diag_start.elapsed();
        let encrypted_key = location.key_enc.as_ref().ok_or_else(|| {
            BackgroundReadError::Other("Failed to unwrap image row key".to_string())
        })?;
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        let mut row_key = decrypt_row_key_with_cng_silent(&self.credential_state, encrypted_key)
            .map_err(|error| match error {
                CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
                other => {
                    BackgroundReadError::Other(format!("Failed to unwrap image row key: {}", other))
                }
            })?;

        let abs_path_str = abs_path.to_string_lossy().to_string();
        let result =
            read_encrypted_image_bytes(&abs_path_str, &row_key).map_err(BackgroundReadError::Other);
        Self::zeroize_bytes(&mut row_key);
        if diag_start.elapsed().as_secs() >= 5 {
            tracing::warn!(
                "[DIAG:DB] read_image_bytes_silent({}) query {:?}, total {:?}",
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
        self.read_thumbnail_with_lookup(ImageLookup::from_path(path))
    }

    /// Load a thumbnail with a single primary-key query for its path and wrapped key.
    pub fn read_thumbnail_by_id(&self, id: i64) -> Result<(String, String), String> {
        self.read_thumbnail_with_lookup(ImageLookup::Id(id))
    }

    fn read_thumbnail_with_lookup(
        &self,
        lookup: ImageLookup<'_>,
    ) -> Result<(String, String), String> {
        let _activity = self.foreground_db_read();
        // Phase 1: DB query for the encrypted row key
        let (location, abs_path) = {
            let conn = self.open_read_connection_named("read_thumbnail")?;
            let location = lookup.query(&conn)?;
            let abs_path = self.resolve_image_path(&location.path);
            (location, abs_path)
        };
        self.read_thumbnail_at(
            &abs_path,
            location.key_enc.as_deref(),
            &mut RowKeyDecryptTimings::default(),
            false,
        )
    }

    fn read_thumbnail_at(
        &self,
        abs_path: &Path,
        key_enc: Option<&[u8]>,
        cng: &mut RowKeyDecryptTimings,
        cached_only: bool,
    ) -> Result<(String, String), String> {
        let thumb_path = Self::thumbnail_path_for(abs_path);
        // Speculative reads must not open CNG or decode the original on a miss.
        if cached_only && !thumb_path.exists() {
            return Err("Thumbnail not cached".into());
        }
        let mut row_key = key_enc
            .and_then(|enc| {
                decrypt_row_key_with_cng_measured(&self.credential_state, enc, cng, &|| false)
                    .ok()
                    .flatten()
            })
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let result = self.read_thumbnail_with_key(abs_path, &thumb_path, &row_key, cached_only);
        Self::zeroize_bytes(&mut row_key);
        result
    }

    fn read_thumbnail_with_key(
        &self,
        abs_path: &Path,
        thumb_path: &Path,
        row_key: &[u8],
        cached_only: bool,
    ) -> Result<(String, String), String> {
        // Phase 3: Try reading cached thumbnail
        if thumb_path.exists() {
            match self.try_read_cached_thumbnail(thumb_path, row_key) {
                Ok(result) => return Ok(result),
                Err(error) => {
                    if cached_only {
                        return Err(error);
                    }
                    // Corrupt cache — delete and regenerate
                    let _ = std::fs::remove_file(thumb_path);
                }
            }
        }

        // The file may have disappeared while CNG was unwrapping its key.
        if cached_only {
            return Err("Thumbnail not cached".into());
        }

        // Phase 4: Generate thumbnail from full image
        let abs_path_str = abs_path.to_string_lossy().to_string();
        self.generate_and_cache_thumbnail(&abs_path_str, thumb_path, row_key)
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
        let (location, abs_path) = {
            let conn = self.open_read_connection_named("ensure_thumbnail_cached")?;
            let location = ImageLookup::from_path(path).query(&conn)?;
            let abs_path = self.resolve_image_path(&location.path);
            (location, abs_path)
        };

        // Re-check for memory:// paths (resolved above)
        let thumb_path = Self::thumbnail_path_for(&abs_path);
        if thumb_path.exists() {
            return Ok(false);
        }

        // Phase 3: Decrypt row key and generate thumbnail
        let mut row_key = location
            .key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(&self.credential_state, enc).ok())
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let abs_path_str = abs_path.to_string_lossy().to_string();
        let result = self.generate_and_cache_thumbnail(&abs_path_str, &thumb_path, &row_key);
        Self::zeroize_bytes(&mut row_key);
        result.map(|_| true)
    }

    /// Batch read thumbnails by screenshot IDs.
    /// Fetches paths and opaque wrapped keys together, without hydrating metadata.
    /// Returns a map of id (as string) → Result with base64/mime or error.
    pub fn batch_read_thumbnails_by_ids(
        &self,
        ids: &[i64],
        cached_only: bool,
    ) -> std::collections::HashMap<String, Result<(String, String), String>> {
        if ids.is_empty() {
            return std::collections::HashMap::new();
        }

        let started = Instant::now();
        // Keep maintenance from replacing files or row keys during the batch;
        // this shared permit never holds the primary database mutex.
        let _activity = self.foreground_db_read();
        let lookup = (|| -> Result<_, String> {
            let conn = self.open_read_connection_named("batch_thumbnails_by_ids")?;
            let mut locations = image_locations_by_ids(&conn, ids)?;
            // Resolve paths while the independent-read maintenance permit still
            // pins this database and data directory, without the primary mutex.
            for location in locations.values_mut() {
                location.path = self
                    .resolve_image_path(&location.path)
                    .to_string_lossy()
                    .into_owned();
            }
            Ok(locations)
        })();
        let query = started.elapsed();
        let locations = match lookup {
            Ok(locations) => locations,
            Err(error) => {
                return ids
                    .iter()
                    .map(|id| {
                        (
                            id.to_string(),
                            Err(format!("DB image lookup error: {error}")),
                        )
                    })
                    .collect()
            }
        };
        let mut cng = RowKeyDecryptTimings::default();
        let mut results = std::collections::HashMap::new();
        for id in ids {
            if results.contains_key(&id.to_string()) {
                continue;
            }
            let result = if let Some(location) = locations.get(id) {
                self.read_thumbnail_at(
                    Path::new(&location.path),
                    location.key_enc.as_deref(),
                    &mut cng,
                    cached_only,
                )
            } else {
                Err("Screenshot not found".to_string())
            };
            results.insert(id.to_string(), result);
        }
        let total = started.elapsed();
        if total >= Duration::from_millis(250) {
            tracing::warn!(requested = ids.len(), ?query, cng_calls = cng.calls,
                cng_wait = ?cng.lock_wait, cng_decrypt = ?cng.decrypt, ?total,
                "[DIAG:IMAGE] batch thumbnail timing");
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
            let conn = match self.open_read_connection_named("batch_read_thumbnails") {
                Ok(conn) => conn,
                Err(e) => {
                    return paths
                        .iter()
                        .map(|p| (p.clone(), Err(format!("DB read connection error: {}", e))))
                        .collect();
                }
            };

            let mut map = std::collections::HashMap::new();
            for chunk in paths.chunks(500) {
                let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
                let sql = format!(
                    "SELECT image_path, content_key_encrypted FROM screenshots WHERE is_deleted = 0 AND image_path IN ({})",
                    placeholders.join(",")
                );
                let params: Vec<&dyn rusqlite::ToSql> =
                    chunk.iter().map(|p| p as &dyn rusqlite::ToSql).collect();

                if let Ok(mut stmt) = conn.prepare(&sql) {
                    if let Ok(rows) = stmt.query_map(params.as_slice(), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
                    }) {
                        for row in rows.filter_map(|r| r.ok()) {
                            map.insert(row.0, row.1);
                        }
                    }
                }
            }
            map
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
                        .and_then(|enc| decrypt_row_key_with_cng(&self.credential_state, enc).ok())
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
        let fname = original.file_name().and_then(|s| s.to_str()).unwrap_or("");
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
/// Standalone file adapter; foreground screenshot loads use the measured storage path.
#[allow(dead_code)]
pub fn read_encrypted_image_as_base64(
    path: &str,
    row_key: &[u8],
) -> Result<(String, String), String> {
    let (image_data, mime_type) = read_encrypted_image_bytes(path, row_key)?;
    let base64_data =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image_data);

    Ok((base64_data, mime_type))
}

/// Read an encrypted image file and return raw image bytes (with decryption).
pub fn read_encrypted_image_bytes(path: &str, row_key: &[u8]) -> Result<(Vec<u8>, String), String> {
    read_encrypted_image_bytes_measured(path, row_key, &mut ImageFileReadTimings::default())
}

fn read_encrypted_image_bytes_measured(
    path: &str,
    row_key: &[u8],
    timings: &mut ImageFileReadTimings,
) -> Result<(Vec<u8>, String), String> {
    let path = Path::new(path);

    let read_started = Instant::now();
    let data = if path.exists() {
        std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))
    } else {
        Err(format!("Image file not found: {}", path.display()))
    };
    timings.file_read = read_started.elapsed();
    let data = data?;
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
        let decrypt_started = Instant::now();
        let decrypted = decrypt_with_master_key(row_key, &data)
            .map_err(|e| format!("Failed to decrypt image: {}", e));
        timings.decrypt = decrypt_started.elapsed();
        decrypted?
    } else {
        data
    };

    Ok((image_data, mime_type.to_string()))
}

#[cfg(test)]
mod tests;
