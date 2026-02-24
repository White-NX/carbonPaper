//! Plaintext screenshot file encryption, backfill, and cleanup.

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_with_master_key, encrypt_row_key_with_cng,
    encrypt_with_master_key,
};
use rand::RngCore;
use rusqlite::params;
use std::path::Path;
use std::sync::Arc;

use super::{MigrationResult, StorageState};

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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
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
        let encrypted_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap row key: {}", e))?;
        Self::zeroize_bytes(&mut row_key);

        // Generate new filename
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let new_file_name = format!("{}.enc", file_name);
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
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

    /// Background migration: batch-decrypt old records and backfill plaintext process_name.
    /// Waits for user authentication before starting CNG decryption.
    pub fn backfill_plaintext_process_names(storage: Arc<Self>) {
        // Wait for user to authenticate before attempting CNG decryption
        tracing::info!(
            "[BACKFILL] waiting for user authentication before starting migration..."
        );
        loop {
            if storage.credential_state.is_session_valid() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        tracing::info!("[BACKFILL] user authenticated, starting migration");

        let migrate_start = std::time::Instant::now();
        let mut total_updated = 0u64;
        let mut batch_num = 0u64;

        loop {
            batch_num += 1;

            // Re-check auth: if session expired, pause until user re-authenticates
            if !storage.credential_state.is_session_valid() {
                tracing::info!(
                    "[BACKFILL] session expired at batch #{}, pausing...",
                    batch_num
                );
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if storage.credential_state.is_session_valid() {
                        tracing::info!("[BACKFILL] session restored, resuming migration");
                        break;
                    }
                }
            }

            // Phase 1: extract a batch of encrypted-only rows (hold mutex)
            let batch: Vec<(i64, Option<Vec<u8>>, Option<Vec<u8>>)> = match storage
                .get_connection_named("backfill_process_names")
            {
                Ok(guard) => {
                    let conn = guard.as_ref().unwrap();
                    let mut stmt = match conn.prepare(
                        "SELECT id, process_name_enc, content_key_encrypted FROM screenshots
                         WHERE process_name IS NULL AND process_name_enc IS NOT NULL
                         LIMIT 100",
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("[BACKFILL] Failed to prepare query: {}", e);
                            break;
                        }
                    };
                    let rows = match stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                        ))
                    }) {
                        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                        Err(e) => {
                            tracing::error!("[BACKFILL] Failed to execute query: {}", e);
                            break;
                        }
                    };
                    rows
                    // guard dropped — mutex released
                }
                Err(e) => {
                    tracing::error!("[BACKFILL] Failed to get connection: {}", e);
                    break;
                }
            };

            if batch.is_empty() {
                break;
            }

            // Phase 2: decrypt outside mutex
            let mut decrypted: Vec<(i64, String)> = Vec::new();
            for (id, process_enc, key_enc) in &batch {
                let mut row_key = key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(enc).ok());
                let name = match (process_enc.as_ref(), row_key.as_ref()) {
                    (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => None,
                };
                if let Some(ref mut key) = row_key {
                    Self::zeroize_bytes(key);
                }
                if let Some(n) = name {
                    decrypted.push((*id, n));
                }
            }

            // Phase 3: write back plaintext (hold mutex)
            if !decrypted.is_empty() {
                match storage.get_connection_named("backfill_process_names_update") {
                    Ok(guard) => {
                        let conn = guard.as_ref().unwrap();
                        for (id, name) in &decrypted {
                            if let Err(e) = conn.execute(
                                "UPDATE screenshots SET process_name = ? WHERE id = ?",
                                params![name, id],
                            ) {
                                tracing::error!(
                                    "[BACKFILL] Failed to update id={}: {}",
                                    id,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "[BACKFILL] Failed to get connection for update: {}",
                            e
                        );
                        break;
                    }
                }
            }

            total_updated += decrypted.len() as u64;
            tracing::info!(
                "[BACKFILL] batch #{}: decrypted {}/{} rows (cumulative: {})",
                batch_num,
                decrypted.len(),
                batch.len(),
                total_updated
            );

            // Yield to let normal queries acquire the mutex
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let total_dur = migrate_start.elapsed();
        tracing::info!(
            "[BACKFILL] completed: {} rows backfilled in {:?} ({} batches)",
            total_updated,
            total_dur,
            batch_num
        );
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
