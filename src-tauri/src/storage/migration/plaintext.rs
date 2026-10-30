//! Plaintext screenshot file encryption and cleanup.

use crate::credential_manager::encrypt_with_master_key;
use rand::RngCore;
use rusqlite::params;
use std::path::Path;

use super::super::{MigrationResult, StorageState};

impl StorageState {
    /// Scan and encrypt all plaintext screenshot files.
    ///
    /// 1. Scans the screenshots directory for non-.enc files
    /// 2. Encrypts each file and saves as .enc format
    /// 3. Updates the path in the database
    /// 4. Deletes the original plaintext file
    pub fn migrate_plaintext_screenshots(&self) -> Result<MigrationResult, String> {
        let mut result = MigrationResult {
            total_files: 0,
            migrated: 0,
            skipped: 0,
            errors: Vec::new(),
        };

        // Scan screenshots directory
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let entries = std::fs::read_dir(&screenshot_dir)
            .map_err(|e| format!("Failed to read screenshot directory: {}", e))?;

        let plaintext_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                let ext = path.extension().and_then(|e| e.to_str());
                // Only process non-encrypted image files
                matches!(
                    ext,
                    Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp")
                )
            })
            .collect();

        result.total_files = plaintext_files.len();

        for entry in plaintext_files {
            let path = entry.path();
            let path_str = self.to_relative_image_path(&path);

            match self.encrypt_single_file(&path) {
                Ok((new_path, encrypted_key)) => {
                    // Update screenshot path in database
                    if let Err(e) =
                        self.update_screenshot_path(&path_str, &new_path, &encrypted_key)
                    {
                        result
                            .errors
                            .push(format!("Failed to update DB for {}: {}", path_str, e));
                    }

                    // Delete original file
                    if let Err(e) = std::fs::remove_file(&path) {
                        result
                            .errors
                            .push(format!("Failed to delete {}: {}", path_str, e));
                    } else {
                        result.migrated += 1;
                        tracing::info!("Migrated: {} -> {}", path_str, new_path);
                    }
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to encrypt {}: {}", path_str, e));
                }
            }
        }

        result.skipped = result.total_files - result.migrated - result.errors.len();

        Ok(result)
    }

    /// Encrypt a single file using row-level key encryption.
    fn encrypt_single_file(&self, path: &Path) -> Result<(String, Vec<u8>), String> {
        let data = std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;

        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);
        let encrypted = encrypt_with_master_key(&row_key, &data)
            .map_err(|e| format!("Failed to encrypt: {}", e))?;
        let encrypted_key = self.wrap_row_key_for_storage(&row_key)?;
        Self::zeroize_bytes(&mut row_key);

        // Generate new filename
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let new_file_name = format!("{}.enc", file_name);
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let new_path = screenshot_dir.join(&new_file_name);

        // Save encrypted file
        std::fs::write(&new_path, &encrypted)
            .map_err(|e| format!("Failed to write encrypted file: {}", e))?;

        Ok((self.to_relative_image_path(&new_path), encrypted_key))
    }

    /// Update a screenshot's image path in the database.
    fn update_screenshot_path(
        &self,
        old_path: &str,
        new_path: &str,
        encrypted_key: &[u8],
    ) -> Result<(), String> {
        let guard = self.get_connection_named("update_screenshot_path")?;
        let conn = guard.as_ref().unwrap();

        conn.execute(
            "UPDATE screenshots SET image_path = ?, content_key_encrypted = ? WHERE image_path = ?",
            params![new_path, encrypted_key, old_path],
        )
        .map_err(|e| format!("Failed to update screenshot path: {}", e))?;

        Ok(())
    }

    /// List all plaintext (non-encrypted) screenshot files.
    pub fn list_plaintext_screenshots(&self) -> Result<Vec<String>, String> {
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let entries = std::fs::read_dir(&screenshot_dir)
            .map_err(|e| format!("Failed to read screenshot directory: {}", e))?;

        let files: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                let ext = path.extension().and_then(|e| e.to_str());
                matches!(
                    ext,
                    Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp")
                )
            })
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        Ok(files)
    }

    /// Delete all plaintext screenshot files (without migration, direct deletion).
    pub fn delete_plaintext_screenshots(&self) -> Result<usize, String> {
        let files = self.list_plaintext_screenshots()?;
        let mut deleted = 0;

        for file in &files {
            if let Err(e) = std::fs::remove_file(file) {
                tracing::error!("Failed to delete {}: {}", file, e);
            } else {
                deleted += 1;
            }
        }

        Ok(deleted)
    }
}
