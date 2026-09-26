//! Removal of the Python environment that releases before the Python removal
//! installed under `%LOCALAPPDATA%\CarbonPaper\.venv`.
//!
//! The application no longer starts Python, so the environment (several GiB of
//! interpreter, Torch, Paddle and spaCy packages) is dead weight on every
//! upgraded machine. Its path never depended on the install directory, which
//! is why this module looks at exactly one place.
//!
//! The removal is shaped by three constraints:
//!
//! - Only a directory that is plainly ours is touched. The top level must be a
//!   real directory rather than a symbolic link or junction, and must carry the
//!   `pyvenv.cfg` that `python -m venv` writes. Anything else is logged and left
//!   alone. The interpreter the environment was created from lives elsewhere
//!   (the old installer put it under `Programs\Python`), may serve other
//!   software, and is never touched.
//! - The environment is first renamed to [`PENDING_DIR`]. A rename within one
//!   volume is atomic, so a downgraded release sees either a complete
//!   environment or none, never a half-deleted one it would try to run.
//! - Deleting tens of thousands of files is real disk work, so it runs on its
//!   own thread in Windows background mode (lowered CPU and I/O priority),
//!   after a startup delay. A failure is logged and retried on the next launch:
//!   whether [`PENDING_DIR`] or `.venv` still exists is the whole state, so no
//!   sentinel is kept.
//!
//! The uninstaller removes both names as well, for users who uninstall without
//! ever launching a release that carries this module.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Name of the environment directory under `%LOCALAPPDATA%\CarbonPaper`.
pub const LEGACY_VENV_DIR: &str = ".venv";
/// Name the environment is renamed to before its contents are deleted.
pub const PENDING_DIR: &str = ".venv.removing";
/// File `python -m venv` writes at the root of every environment it creates.
const VENV_MARKER: &str = "pyvenv.cfg";
/// Keeps the removal out of the way of storage startup and the first capture.
const STARTUP_DELAY: Duration = Duration::from_secs(90);

/// What one cleanup pass found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// Neither `.venv` nor [`PENDING_DIR`] exists: a fresh install, or one
    /// already cleaned up.
    NothingToDo,
    /// The environment was deleted. `bytes` is the size of the files found
    /// in it, counted before deletion, for the log.
    Removed { bytes: u64 },
    /// A directory exists but does not look like ours, so it was left alone.
    Skipped { path: PathBuf, reason: &'static str },
}

/// `true` for a real directory; `false` for a symbolic link, a junction, any
/// other reparse point, a file, or a missing path.
fn is_plain_directory(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

/// Total size of the regular files under `root`. Links are counted as entries
/// but never followed, so the walk cannot leave the directory.
fn directory_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            }
        }
    }
    total
}

fn remove_pending(pending: &Path) -> Result<u64, String> {
    let bytes = directory_size(pending);
    std::fs::remove_dir_all(pending)
        .map_err(|error| format!("failed to delete {}: {error}", pending.display()))?;
    Ok(bytes)
}

/// One cleanup pass over `appdata_dir` (`%LOCALAPPDATA%\CarbonPaper` in
/// production). Finishes a deletion an earlier launch left behind, then
/// renames and deletes `.venv` if it is ours.
pub fn clean_legacy_venv(appdata_dir: &Path) -> Result<CleanupOutcome, String> {
    let venv = appdata_dir.join(LEGACY_VENV_DIR);
    let pending = appdata_dir.join(PENDING_DIR);
    let mut removed_bytes = None;

    if std::fs::symlink_metadata(&pending).is_ok() {
        // Only this module creates the name, and only by renaming a directory
        // it checked. Anything else under that name was not made here.
        if !is_plain_directory(&pending) {
            return Ok(CleanupOutcome::Skipped {
                path: pending,
                reason: "not a plain directory",
            });
        }
        removed_bytes = Some(remove_pending(&pending)?);
    }

    if std::fs::symlink_metadata(&venv).is_ok() {
        if !is_plain_directory(&venv) {
            return Ok(CleanupOutcome::Skipped {
                path: venv,
                reason: "not a plain directory",
            });
        }
        if !venv.join(VENV_MARKER).is_file() {
            return Ok(CleanupOutcome::Skipped {
                path: venv,
                reason: "no pyvenv.cfg",
            });
        }
        // Fails while a leftover Python process still holds a file open; the
        // environment then stays whole until the next launch tries again.
        std::fs::rename(&venv, &pending).map_err(|error| {
            format!(
                "failed to rename {} to {}: {error}",
                venv.display(),
                pending.display()
            )
        })?;
        let bytes = remove_pending(&pending)?;
        removed_bytes = Some(removed_bytes.unwrap_or(0) + bytes);
    }

    Ok(match removed_bytes {
        Some(bytes) => CleanupOutcome::Removed { bytes },
        None => CleanupOutcome::NothingToDo,
    })
}

/// Puts the calling thread in background mode, which lowers its CPU and I/O
/// priority until the thread exits.
#[cfg(windows)]
fn enter_background_mode() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: `GetCurrentThread` returns a pseudo-handle that needs no closing
    // and is always valid for the calling thread.
    let result = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    if let Err(error) = result {
        tracing::debug!("[LEGACY_PYTHON] could not enter background mode: {error}");
    }
}

#[cfg(not(windows))]
fn enter_background_mode() {}

/// Starts the cleanup on a dedicated background-mode thread after
/// [`STARTUP_DELAY`]. Does nothing when `%LOCALAPPDATA%` is unavailable.
pub fn spawn_legacy_venv_cleanup() {
    let Some(appdata_dir) = crate::resource_utils::file_in_local_appdata() else {
        return;
    };
    if std::fs::symlink_metadata(appdata_dir.join(LEGACY_VENV_DIR)).is_err()
        && std::fs::symlink_metadata(appdata_dir.join(PENDING_DIR)).is_err()
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("legacy-venv-cleanup".to_string())
        .spawn(move || {
            std::thread::sleep(STARTUP_DELAY);
            enter_background_mode();
            match clean_legacy_venv(&appdata_dir) {
                Ok(CleanupOutcome::NothingToDo) => {}
                Ok(CleanupOutcome::Removed { bytes }) => tracing::info!(
                    "[LEGACY_PYTHON] removed the retired Python environment ({:.1} MiB)",
                    bytes as f64 / (1024.0 * 1024.0)
                ),
                Ok(CleanupOutcome::Skipped { path, reason }) => {
                    tracing::warn!("[LEGACY_PYTHON] left {} in place: {reason}", path.display())
                }
                Err(error) => tracing::warn!(
                    "[LEGACY_PYTHON] cleanup will be retried on the next launch: {error}"
                ),
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("[LEGACY_PYTHON] failed to start the cleanup thread: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_venv(root: &Path) -> PathBuf {
        let venv = root.join(LEGACY_VENV_DIR);
        let site_packages = venv.join("Lib").join("site-packages").join("torch");
        std::fs::create_dir_all(&site_packages).unwrap();
        std::fs::write(venv.join(VENV_MARKER), b"home = C:\\Python312\n").unwrap();
        std::fs::write(site_packages.join("lib.dll"), vec![0u8; 1000]).unwrap();
        venv
    }

    #[test]
    fn a_fresh_install_has_nothing_to_do() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            clean_legacy_venv(tmp.path()).unwrap(),
            CleanupOutcome::NothingToDo
        );
    }

    #[test]
    fn an_environment_is_removed_and_its_size_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = make_venv(tmp.path());
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        let expected = 1000 + b"home = C:\\Python312\n".len() as u64;
        assert_eq!(outcome, CleanupOutcome::Removed { bytes: expected });
        assert!(!venv.exists());
        assert!(!tmp.path().join(PENDING_DIR).exists());
        // The parent and its other contents are untouched.
        assert!(tmp.path().is_dir());
    }

    #[test]
    fn a_directory_without_pyvenv_cfg_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join(LEGACY_VENV_DIR);
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("notes.txt"), b"not an environment").unwrap();
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        assert!(matches!(
            outcome,
            CleanupOutcome::Skipped {
                reason: "no pyvenv.cfg",
                ..
            }
        ));
        assert!(venv.join("notes.txt").is_file());
    }

    #[test]
    fn a_deletion_interrupted_by_an_earlier_launch_is_finished() {
        let tmp = tempfile::tempdir().unwrap();
        let pending = tmp.path().join(PENDING_DIR);
        std::fs::create_dir_all(pending.join("Lib")).unwrap();
        std::fs::write(pending.join("Lib").join("left.pyd"), vec![0u8; 10]).unwrap();
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        assert_eq!(outcome, CleanupOutcome::Removed { bytes: 10 });
        assert!(!pending.exists());
    }

    #[test]
    fn a_leftover_and_a_whole_environment_are_both_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let pending = tmp.path().join(PENDING_DIR);
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("left.pyd"), vec![0u8; 10]).unwrap();
        let venv = make_venv(tmp.path());
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        assert!(matches!(outcome, CleanupOutcome::Removed { bytes } if bytes > 1000));
        assert!(!venv.exists());
        assert!(!pending.exists());
    }

    #[test]
    fn a_linked_environment_is_left_alone_with_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(VENV_MARKER), b"home = C:\\Python312\n").unwrap();
        let link = tmp.path().join(LEGACY_VENV_DIR);
        // Creating a directory symbolic link needs Developer Mode or elevation
        // on Windows; without either the case cannot be set up here.
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&target, &link);
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(&target, &link);
        if created.is_err() {
            return;
        }
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        assert!(matches!(
            outcome,
            CleanupOutcome::Skipped {
                reason: "not a plain directory",
                ..
            }
        ));
        assert!(target.join(VENV_MARKER).is_file());
    }

    #[cfg(windows)]
    #[test]
    fn a_junctioned_environment_is_left_alone_with_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(VENV_MARKER), b"home = C:\\Python312\n").unwrap();
        let link = tmp.path().join(LEGACY_VENV_DIR);
        // Junctions need no privilege, unlike directory symbolic links.
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        let outcome = clean_legacy_venv(tmp.path()).unwrap();
        assert!(matches!(
            outcome,
            CleanupOutcome::Skipped {
                reason: "not a plain directory",
                ..
            }
        ));
        assert!(target.join(VENV_MARKER).is_file());
        assert!(!tmp.path().join(PENDING_DIR).exists());
    }
}
