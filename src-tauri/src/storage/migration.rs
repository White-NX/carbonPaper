//! Data directory migration with rollback support.

use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::AppHandle;
use tauri::Emitter;
use walkdir::WalkDir;

use super::StorageState;

/// RAII guard that resets migration flags when dropped.
struct MigrationRunGuard<'a> {
    in_progress: &'a AtomicBool,
    cancel_requested: &'a AtomicBool,
}

impl<'a> MigrationRunGuard<'a> {
    fn new(in_progress: &'a AtomicBool, cancel_requested: &'a AtomicBool) -> Self {
        Self {
            in_progress,
            cancel_requested,
        }
    }
}

impl Drop for MigrationRunGuard<'_> {
    fn drop(&mut self) {
        self.in_progress.store(false, Ordering::SeqCst);
        self.cancel_requested.store(false, Ordering::SeqCst);
    }
}

impl StorageState {
    /// Rollback a partial migration by removing copied files and created directories.
    fn rollback_partial_migration(copied_files: &[PathBuf], created_dirs: &mut Vec<PathBuf>) {
        for file in copied_files.iter().rev() {
            let _ = std::fs::remove_file(file);
        }

        created_dirs.sort();
        created_dirs.dedup();
        created_dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));
        for dir in created_dirs.iter() {
            let _ = std::fs::remove_dir(dir);
        }
    }

    /// Restore source directory and reinitialize storage after a failed or cancelled migration.
    fn restore_source_and_reinitialize(
        &self,
        app_handle: &AppHandle,
        src: &PathBuf,
        message: String,
        cancelled: bool,
    ) -> Result<serde_json::Value, String> {
        {
            let mut data_guard = self.data_dir.lock().unwrap();
            *data_guard = src.clone();
            let mut ss_guard = self.screenshot_dir.lock().unwrap();
            *ss_guard = src.join("screenshots");
        }

        if let Err(e) = self.initialize() {
            let msg = format!(
                "{}; failed to reinitialize source storage: {}",
                message, e
            );
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false, "cancelled": cancelled }),
            );
            return Err(msg);
        }

        let _ = app_handle.emit(
            "storage-migration-error",
            json!({ "message": message.clone(), "recoverable": true, "cancelled": cancelled }),
        );

        Err(message)
    }

    /// Canonicalize path for safe comparison (resolves symlinks, `.`, `..`, etc.).
    fn canonicalize_for_compare(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// Migrate data directory. Optionally performs full migration (copy + remove),
    /// emitting progress events via app_handle.
    ///
    /// Returns JSON object `{ target: String, migrated: bool }`.
    pub fn migrate_data_dir_blocking(
        &self,
        app_handle: AppHandle,
        target: String,
        migrate_data_files: bool,
    ) -> Result<serde_json::Value, String> {
        if self
            .migration_in_progress
            .swap(true, Ordering::SeqCst)
        {
            return Err("A storage migration is already in progress".to_string());
        }
        self.migration_cancel_requested
            .store(false, Ordering::SeqCst);
        let _migration_guard =
            MigrationRunGuard::new(&self.migration_in_progress, &self.migration_cancel_requested);

        let src = self.data_dir.lock().unwrap().clone();
        // User selects a storage root; actual data_dir is always under its "data" subdirectory
        let dst = PathBuf::from(&target).join("data");

        // ---- Path safety checks ----
        let src_canon = Self::canonicalize_for_compare(&src);
        // dst may not exist yet; canonicalize its existing ancestor then append remaining parts
        let dst_canon = {
            let mut existing = dst.clone();
            let mut tail_parts: Vec<std::ffi::OsString> = Vec::new();
            while !existing.exists() {
                if let Some(name) = existing.file_name() {
                    tail_parts.push(name.to_os_string());
                    existing = existing.parent().unwrap_or(&existing).to_path_buf();
                } else {
                    break;
                }
            }
            let mut canon = Self::canonicalize_for_compare(&existing);
            for part in tail_parts.into_iter().rev() {
                canon = canon.join(part);
            }
            canon
        };

        // Prevent dst from being a subdirectory of src (remove_dir_all(src) would delete copied files)
        if dst_canon.starts_with(&src_canon) && dst_canon != src_canon {
            return Err(format!(
                "Target path ({}) is inside the current data directory ({}), cannot migrate",
                dst.display(),
                src.display()
            ));
        }

        let mut copied_files: Vec<PathBuf> = Vec::new();
        let mut created_dirs: Vec<PathBuf> = Vec::new();
        let mut source_removed = false;

        if let Err(e) = self.shutdown() {
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": format!("Failed to shutdown storage: {}", e), "recoverable": false }),
            );
            return Err(format!("Failed to shutdown storage: {}", e));
        }

        if self.is_migration_cancel_requested() {
            return self.restore_source_and_reinitialize(
                &app_handle,
                &src,
                "Migration cancelled by user".to_string(),
                true,
            );
        }

        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let msg = format!("Failed to create target parent dirs: {}", e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
            }
        }

        let should_migrate_files = migrate_data_files && src != dst;

        if should_migrate_files {
            let mut total_files: usize = 0;
            for entry in WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    total_files += 1;
                }
            }

            if total_files == 0 {
                let existed = dst.exists();
                if let Err(e) = std::fs::create_dir_all(&dst) {
                    let msg = format!("Failed to create target dir: {}", e);
                    let _ = app_handle.emit(
                        "storage-migration-error",
                        json!({ "message": msg.clone(), "recoverable": false }),
                    );
                    return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
                }
                if !existed {
                    created_dirs.push(dst.clone());
                }
            }

            let mut copied: usize = 0;

            for entry in WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
                if self.is_migration_cancel_requested() {
                    Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                    return self.restore_source_and_reinitialize(
                        &app_handle,
                        &src,
                        "Migration cancelled by user".to_string(),
                        true,
                    );
                }

                let rel_path = match entry.path().strip_prefix(&src) {
                    Ok(p) => p.to_path_buf(),
                    Err(_) => continue,
                };
                let target_path = dst.join(&rel_path);

                if entry.file_type().is_dir() {
                    let existed = target_path.exists();
                    if let Err(e) = std::fs::create_dir_all(&target_path) {
                        let msg =
                            format!("Failed to create dir {}: {}", target_path.display(), e);
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self
                            .restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                    if !existed {
                        created_dirs.push(target_path.clone());
                    }
                    continue;
                }

                if let Some(parent) = target_path.parent() {
                    let existed = parent.exists();
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        let msg = format!(
                            "Failed to create parent for file {}: {}",
                            target_path.display(),
                            e
                        );
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self
                            .restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                    if !existed {
                        created_dirs.push(parent.to_path_buf());
                    }
                }

                match std::fs::copy(entry.path(), &target_path) {
                    Ok(_) => {
                        copied += 1;
                        copied_files.push(target_path.clone());
                        let _ = app_handle.emit(
                            "storage-migration-progress",
                            json!({
                                "total_files": total_files,
                                "copied_files": copied,
                                "current_file": entry.path().to_string_lossy()
                            }),
                        );
                    }
                    Err(e) => {
                        let msg = format!("Failed to copy {}: {}", entry.path().display(), e);
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self
                            .restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                }
            }

            if self.is_migration_cancel_requested() {
                Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &src,
                    "Migration cancelled by user".to_string(),
                    true,
                );
            }

            if let Err(e) = std::fs::remove_dir_all(&src) {
                let msg = format!("Failed to remove source dir {}: {}", src.display(), e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return Err(msg);
            }
            source_removed = true;
        } else {
            let existed = dst.exists();
            if let Err(e) = std::fs::create_dir_all(&dst) {
                let msg = format!("Failed to create target dir: {}", e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
            }
            if !existed {
                created_dirs.push(dst.clone());
            }
        }

        if self.is_migration_cancel_requested() && !source_removed {
            Self::rollback_partial_migration(&copied_files, &mut created_dirs);
            return self.restore_source_and_reinitialize(
                &app_handle,
                &src,
                "Migration cancelled by user".to_string(),
                true,
            );
        }

        // Persist new data_dir to registry
        let dst_str = dst.to_string_lossy().to_string();
        if let Err(e) = crate::registry_config::set_string("data_dir", &dst_str) {
            let msg = format!("Failed to persist data_dir to registry: {}", e);
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false }),
            );
            return Err(msg);
        }

        {
            let mut data_guard = self.data_dir.lock().unwrap();
            *data_guard = dst.clone();
            let mut ss_guard = self.screenshot_dir.lock().unwrap();
            *ss_guard = dst.join("screenshots");
        }

        if let Err(e) = self.initialize() {
            let msg = format!("Failed to reinitialize storage after migration: {}", e);
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false }),
            );
            return Err(msg);
        }

        let _ = app_handle.emit(
            "storage-migration-done",
            json!({ "target": dst.to_string_lossy(), "migrated": should_migrate_files }),
        );

        Ok(json!({ "target": dst.to_string_lossy(), "migrated": should_migrate_files }))
    }
}
