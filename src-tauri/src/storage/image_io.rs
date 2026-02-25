//! Image reading with decryption support.

use crate::credential_manager::{decrypt_row_key_with_cng, decrypt_with_master_key};
use std::path::Path;

use super::StorageState;

impl StorageState {
    /// Read an encrypted image file and return Base64-encoded data.
    pub fn read_image(&self, path: &str) -> Result<(String, String), String> {
        let diag_start = std::time::Instant::now();

        // Phase 1: Hold mutex only for DB query to get the encrypted key
        let (key_enc, abs_path) = {
            let guard = self.get_connection_named("read_image")?;
            let conn = guard.as_ref().unwrap();

            if path.starts_with("memory://") {
                // 旧数据兼容：从 memory:// 中提取 hash 查找
                let hash = &path["memory://".len()..];
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
