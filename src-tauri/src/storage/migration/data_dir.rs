//! Staged data-directory migration with database validation and rollback.

use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};
use walkdir::WalkDir;

use super::{super::database_snapshot, super::StorageState, MigrationRunGuard};

const CANCELLED: &str = "Migration cancelled by user";

impl StorageState {
    fn lexically_normalize_path(path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    let _ = normalized.pop();
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    }

    fn set_runtime_data_directory(&self, directory: &Path) {
        *self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = directory.to_path_buf();
        *self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = directory.join("screenshots");
        self.credential_state.set_data_dir(directory.to_path_buf());
    }

    fn restore_registry_data_directory(previous: Option<&str>) -> Result<(), String> {
        match previous {
            Some(value) => crate::registry_config::set_string("data_dir", value),
            None => crate::registry_config::delete_value("data_dir"),
        }
    }

    fn emit_migration_error(
        app_handle: &AppHandle,
        message: &str,
        recoverable: bool,
        cancelled: bool,
    ) {
        let _ = app_handle.emit(
            "storage-migration-error",
            json!({"message": message, "recoverable": recoverable, "cancelled": cancelled}),
        );
    }

    fn restore_source_and_reinitialize(
        &self,
        app_handle: &AppHandle,
        source: &Path,
        previous_registry: Option<&str>,
        message: String,
        cancelled: bool,
    ) -> Result<serde_json::Value, String> {
        self.set_runtime_data_directory(source);
        let registry_result = Self::restore_registry_data_directory(previous_registry);
        let initialize_result = self.initialize_under_maintenance();
        let mut recovery_errors = Vec::new();
        if let Err(error) = registry_result {
            recovery_errors.push(error);
        }
        if let Err(error) = initialize_result {
            recovery_errors.push(format!("failed to reinitialize source storage: {error}"));
        } else {
            crate::clip_ann::spawn_startup_arm(app_handle.clone());
        }
        let final_message = if recovery_errors.is_empty() {
            message
        } else {
            format!(
                "{message}; rollback failures: {}",
                recovery_errors.join("; ")
            )
        };
        Self::emit_migration_error(
            app_handle,
            &final_message,
            recovery_errors.is_empty(),
            cancelled,
        );
        Err(final_message)
    }

    fn rollback_activated_target(
        &self,
        app_handle: &AppHandle,
        source: &Path,
        previous_registry: Option<&str>,
        transaction: &mut database_snapshot::DirectorySwapTransaction,
        message: String,
    ) -> Result<serde_json::Value, String> {
        let _ = self.shutdown_under_maintenance();
        self.set_runtime_data_directory(source);
        let target_rollback = transaction.rollback();
        let registry_rollback = Self::restore_registry_data_directory(previous_registry);
        let source_reopen = self.initialize_under_maintenance();
        let mut errors = Vec::new();
        if let Err(error) = target_rollback {
            errors.push(error);
        }
        if let Err(error) = registry_rollback {
            errors.push(error);
        }
        if let Err(error) = source_reopen {
            errors.push(format!("failed to reopen source storage: {error}"));
        } else {
            crate::clip_ann::spawn_startup_arm(app_handle.clone());
        }
        let final_message = if errors.is_empty() {
            message
        } else {
            format!("{message}; rollback failures: {}", errors.join("; "))
        };
        Self::emit_migration_error(app_handle, &final_message, errors.is_empty(), false);
        Err(final_message)
    }

    fn canonicalize_for_compare(path: &Path) -> PathBuf {
        let mut existing = Self::lexically_normalize_path(path);
        let mut suffix = Vec::new();
        while !existing.exists() {
            let Some(name) = existing.file_name() else {
                break;
            };
            suffix.push(name.to_os_string());
            let Some(parent) = existing.parent() else {
                break;
            };
            existing = parent.to_path_buf();
        }
        let mut canonical = std::fs::canonicalize(&existing).unwrap_or(existing);
        for part in suffix.into_iter().rev() {
            canonical.push(part);
        }
        #[cfg(windows)]
        {
            // Windows paths are case-insensitive even when the spelling from
            // the picker differs from the registry value.
            return PathBuf::from(canonical.to_string_lossy().to_ascii_lowercase());
        }
        #[cfg(not(windows))]
        {
            canonical
        }
    }

    fn migration_file_count(source: &Path) -> Result<usize, String> {
        let mut count = 0usize;
        for entry in WalkDir::new(source).follow_links(false).into_iter() {
            let entry = entry.map_err(|error| format!("Failed to scan data directory: {error}"))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(source)
                .map_err(|error| format!("Failed to compute migration path: {error}"))?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !database_snapshot::DATABASE_RUNTIME_FILE_NAMES.contains(&relative.as_str()) {
                count += 1;
            }
        }
        Ok(count)
    }

    fn migration_paths_overlap(left: &Path, right: &Path) -> bool {
        left != right && (left.starts_with(right) || right.starts_with(left))
    }

    /// Copy to sibling staging, validate the staged database, then atomically
    /// swap the target directory. The source is removed only after all runtime
    /// and registry updates succeed.
    pub fn migrate_data_dir_blocking(
        &self,
        app_handle: AppHandle,
        target: String,
        migrate_data_files: bool,
    ) -> Result<serde_json::Value, String> {
        if self.migration_in_progress.swap(true, Ordering::SeqCst) {
            return Err("A storage migration is already in progress".to_string());
        }
        self.migration_cancel_requested
            .store(false, Ordering::SeqCst);
        let _migration_guard = MigrationRunGuard::new(
            &self.migration_in_progress,
            &self.migration_cancel_requested,
        );
        let _derived_publish_guard = self.derived_generation_publish_guard();
        let _database_maintenance = self.database_maintenance("migrate_data_dir");
        let source = self
            .data_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let destination = PathBuf::from(target).join("data");
        let source_canonical = Self::canonicalize_for_compare(&source);
        let destination_canonical = Self::canonicalize_for_compare(&destination);
        if Self::migration_paths_overlap(&source_canonical, &destination_canonical) {
            return Err(format!(
                "Target path ({}) overlaps the current data directory ({}), cannot migrate",
                destination.display(),
                source.display()
            ));
        }
        if source_canonical == destination_canonical {
            return Ok(json!({"target": destination.to_string_lossy(), "migrated": false}));
        }

        let previous_registry = crate::registry_config::get_string("data_dir");
        let journal_mode = if migrate_data_files {
            Some(self.prepare_database_snapshot_under_maintenance()?)
        } else {
            None
        };
        let database_key = if migrate_data_files {
            Some(self.database_key()?)
        } else {
            None
        };
        app_handle
            .state::<std::sync::Arc<crate::clip_ann::ClipAnnState>>()
            .disarm();
        if let Err(error) = self.shutdown_under_maintenance() {
            return self.restore_source_and_reinitialize(
                &app_handle,
                &source,
                previous_registry.as_deref(),
                format!("Failed to shutdown storage before migration: {error}"),
                false,
            );
        }
        if self.is_migration_cancel_requested() {
            return self.restore_source_and_reinitialize(
                &app_handle,
                &source,
                previous_registry.as_deref(),
                CANCELLED.to_string(),
                true,
            );
        }

        let parent = destination.parent().ok_or_else(|| {
            format!(
                "Target data directory has no parent: {}",
                destination.display()
            )
        })?;
        let staging = match database_snapshot::TemporaryDirectory::create(
            parent,
            "carbonpaper-data-migration-staging",
        ) {
            Ok(value) => value,
            Err(error) => {
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &source,
                    previous_registry.as_deref(),
                    error,
                    false,
                )
            }
        };

        if migrate_data_files {
            let total_files = match Self::migration_file_count(&source) {
                Ok(count) => {
                    count
                        + database_snapshot::DATABASE_FILE_NAMES
                            .iter()
                            .filter(|name| source.join(name).is_file())
                            .count()
                }
                Err(error) => {
                    return self.restore_source_and_reinitialize(
                        &app_handle,
                        &source,
                        previous_registry.as_deref(),
                        error,
                        false,
                    )
                }
            };
            let copy_result = database_snapshot::copy_directory_tree(
                &source,
                staging.path(),
                |relative, _| {
                    relative
                        .to_str()
                        .map(|value| {
                            database_snapshot::DATABASE_RUNTIME_FILE_NAMES
                                .contains(&value.replace('\\', "/").as_str())
                        })
                        .unwrap_or(false)
                },
                |copied, relative| {
                    if self.is_migration_cancel_requested() {
                        return Err(CANCELLED.to_string());
                    }
                    let _ = app_handle.emit("storage-migration-progress", json!({"total_files": total_files, "copied_files": copied, "current_file": relative.to_string_lossy()}));
                    Ok(())
                },
            );
            if let Err(error) = copy_result {
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &source,
                    previous_registry.as_deref(),
                    error.clone(),
                    error == CANCELLED,
                );
            }
            let manifest = match database_snapshot::copy_database_group(
                &source,
                staging.path(),
                journal_mode.as_deref().unwrap_or("delete"),
            ) {
                Ok(value) => value,
                Err(error) => {
                    return self.restore_source_and_reinitialize(
                        &app_handle,
                        &source,
                        previous_registry.as_deref(),
                        error,
                        false,
                    )
                }
            };
            if let Err(error) = database_snapshot::validate_database_snapshot(
                &staging.path(),
                &manifest,
                database_key.as_deref().unwrap_or_default(),
            ) {
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &source,
                    previous_registry.as_deref(),
                    error,
                    false,
                );
            }
        } else if destination.exists() {
            // "Only change path" keeps any existing target data. Stage a
            // verified copy so activation still uses the same directory-swap
            // transaction and can restore the untouched original on failure.
            if let Err(error) = database_snapshot::copy_directory_tree(
                &destination,
                staging.path(),
                |_, _| false,
                |_, _| Ok(()),
            ) {
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &source,
                    previous_registry.as_deref(),
                    error,
                    false,
                );
            }
        }
        if self.is_migration_cancel_requested() {
            return self.restore_source_and_reinitialize(
                &app_handle,
                &source,
                previous_registry.as_deref(),
                CANCELLED.to_string(),
                true,
            );
        }

        let mut transaction =
            match database_snapshot::DirectorySwapTransaction::install(staging, &destination) {
                Ok(value) => value,
                Err(error) => {
                    return self.restore_source_and_reinitialize(
                        &app_handle,
                        &source,
                        previous_registry.as_deref(),
                        error,
                        false,
                    )
                }
            };
        self.set_runtime_data_directory(&destination);
        if let Err(error) = self.initialize_under_maintenance() {
            return self.rollback_activated_target(
                &app_handle,
                &source,
                previous_registry.as_deref(),
                &mut transaction,
                format!("Failed to initialize migrated storage: {error}"),
            );
        }
        let destination_string = destination.to_string_lossy().to_string();
        if let Err(error) = crate::registry_config::set_string("data_dir", &destination_string) {
            return self.rollback_activated_target(
                &app_handle,
                &source,
                previous_registry.as_deref(),
                &mut transaction,
                format!("Failed to persist data_dir to registry: {error}"),
            );
        }

        if let Err(error) = transaction.commit() {
            tracing::warn!("Storage migration target cleanup failed: {error}");
        }
        // Source deletion is deliberately last. At this point the staged
        // target has validated, initialized, and been persisted to the
        // registry, so a locked log file can only leave a redundant safe copy;
        // it cannot invalidate or roll back the active target.
        if migrate_data_files {
            if let Err(error) = database_snapshot::remove_path(&source) {
                tracing::warn!("Storage migration source cleanup failed: {error}");
            }
        }
        crate::clip_ann::spawn_startup_arm(app_handle.clone());
        let _ = app_handle.emit(
            "storage-migration-done",
            json!({"target": destination.to_string_lossy(), "migrated": migrate_data_files}),
        );
        Ok(json!({"target": destination.to_string_lossy(), "migrated": migrate_data_files}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_copy_keeps_source_intact() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let staging = root.path().join("staging");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(source.join("one.bin"), b"one").unwrap();
        std::fs::write(source.join("two.bin"), b"two").unwrap();
        let error = database_snapshot::copy_directory_tree(
            &source,
            &staging,
            |_, _| false,
            |copied, _| {
                if copied == 1 {
                    Err("injected copy interruption".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert!(error.contains("injected"));
        assert_eq!(std::fs::read(source.join("one.bin")).unwrap(), b"one");
        assert_eq!(std::fs::read(source.join("two.bin")).unwrap(), b"two");
    }

    #[test]
    fn target_swap_rolls_back_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("data");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("old.txt"), b"old").unwrap();
        let staging =
            database_snapshot::TemporaryDirectory::create(root.path(), "staging").unwrap();
        std::fs::write(staging.path().join("new.txt"), b"new").unwrap();
        let mut transaction =
            database_snapshot::DirectorySwapTransaction::install(staging, &target).unwrap();
        transaction.rollback().unwrap();
        assert!(target.join("old.txt").exists());
        assert!(!target.join("new.txt").exists());
    }

    #[test]
    fn ancestor_or_descendant_migration_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let parent = StorageState::canonicalize_for_compare(&parent);
        let child = StorageState::canonicalize_for_compare(&child);
        let aliased_child =
            StorageState::canonicalize_for_compare(&parent.join("missing/../child"));

        assert!(StorageState::migration_paths_overlap(&parent, &child));
        assert!(StorageState::migration_paths_overlap(&child, &parent));
        assert!(!StorageState::migration_paths_overlap(&parent, &parent));
        assert_eq!(aliased_child, child);
    }
}
