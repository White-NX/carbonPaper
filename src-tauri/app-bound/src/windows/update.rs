//! Signed ordinary updates. Only signed package files can be installed, and
//! verified file handles remain locked through replacement and application launch.
use super::identity::{self, OwnedHandle};
use crate::{
    manifest::{self, ReleaseManifest, MANIFEST_NAME, SIGNATURE_NAME},
    protocol::{BrokerError, Result},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Seek, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use windows::Wdk::Storage::FileSystem::{
    FileRenameInformation, NtSetInformationFile, FILE_RENAME_INFORMATION,
};
use windows::Win32::{
    Foundation::{BOOLEAN, GENERIC_READ, HANDLE, WAIT_OBJECT_0},
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    Storage::FileSystem::*,
    System::{Diagnostics::ToolHelp::*, Threading::*, IO::IO_STATUS_BLOCK},
};

pub const HELPER_NAME: &str = "carbonpaper-protected-setup.exe";
pub const READY: &str = "READY";
pub const ELEVATION_CANCELLED: &str = "UPDATER_ELEVATION_CANCELLED";
pub const ELEVATING: &str = "ELEVATING";

mod elevation;
pub use elevation::{check_local_path, run_elevated};

/// Pin every directory component, including mutable ancestors, against deletion.
/// Open reparse points themselves, then inspect the handle so validation cannot
/// follow a swap. Execution locks also deny reparse metadata writes.
#[derive(Default)]
pub struct DirectoryLocks {
    entries: Vec<(PathBuf, File)>,
}

impl DirectoryLocks {
    pub fn pin(&mut self, directory: &Path) -> Result<()> {
        // NT file renames need write sharing on their source/target directories.
        self.pin_with_sharing(directory, FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
    }

    fn pin_with_sharing(&mut self, directory: &Path, sharing: u32) -> Result<()> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(BrokerError::Integrity);
        }
        for path in directory.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if self
                .entries
                .iter()
                .any(|(existing, _)| identity::path_eq(existing, path))
            {
                continue;
            }
            let file = OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY.0)
                .share_mode(sharing)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                .open(path)
                .map_err(|_| BrokerError::Integrity)?;
            use std::os::windows::fs::MetadataExt;
            let metadata = file.metadata().map_err(|_| BrokerError::Integrity)?;
            if !metadata.is_dir()
                || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            {
                return Err(BrokerError::Integrity);
            }
            self.entries.push((path.to_path_buf(), file));
        }
        Ok(())
    }

    pub fn for_execution(directory: &Path) -> Result<Self> {
        let mut locks = Self::default();
        locks.pin_with_sharing(directory, FILE_SHARE_READ.0)?;
        Ok(locks)
    }

    fn directory(&self, path: &Path) -> Result<&File> {
        self.entries
            .iter()
            .find(|(existing, _)| identity::path_eq(existing, path))
            .map(|(_, file)| file)
            .ok_or(BrokerError::Integrity)
    }
}

pub fn is_elevated() -> bool {
    elevation_status().unwrap_or(true)
}

fn elevation_status() -> Result<bool> {
    // SAFETY: the process and owned token handles stay live, and elevation is a
    // correctly sized output buffer. Unavailable token information fails closed.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|_| BrokerError::AccessDenied)?;
        let token = OwnedHandle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut length = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of_val(&elevation) as u32,
            &mut length,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        Ok(elevation.TokenIsElevated != 0)
    }
}

/// Open the leaf object itself and validate its handle. A pre-open pathname
/// check alone could race with substitution of a file reparse point.
pub fn locked_file(path: &Path) -> Result<File> {
    open_file(path, GENERIC_READ.0, FILE_SHARE_READ.0)
}

fn movable_file(path: &Path) -> Result<File> {
    open_file(path, GENERIC_READ.0 | DELETE.0, FILE_SHARE_READ.0)
}

fn open_file(path: &Path, access: u32, sharing: u32) -> Result<File> {
    let file = OpenOptions::new()
        .access_mode(access)
        .share_mode(sharing)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .map_err(|_| BrokerError::Storage)?;
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata().map_err(|_| BrokerError::Integrity)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(BrokerError::Integrity);
    }
    if metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(BrokerError::LimitExceeded);
    }
    Ok(file)
}

/// Rename the verified object itself, without closing it and reopening a path.
fn rename_locked(file: &File, target: &Path, directory: &File) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    let metadata = directory.metadata().map_err(|_| BrokerError::Integrity)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(BrokerError::Integrity);
    }
    let name = target
        .file_name()
        .ok_or(BrokerError::Integrity)?
        .encode_wide()
        .collect::<Vec<_>>();
    let offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let length = (offset + name.len() * 2).max(std::mem::size_of::<FILE_RENAME_INFORMATION>());
    let mut buffer = vec![0u64; length.div_ceil(8)];
    // SAFETY: aligned storage includes the variable-length name, and the file
    // owns DELETE access. No source pathname is resolved by this operation.
    unsafe {
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        (*info).Anonymous.ReplaceIfExists = BOOLEAN(1);
        (*info).RootDirectory = HANDLE(directory.as_raw_handle());
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        let status = NtSetInformationFile(
            HANDLE(file.as_raw_handle()),
            &mut IO_STATUS_BLOCK::default(),
            info.cast(),
            length as u32,
            FileRenameInformation,
        );
        if status.0 < 0 {
            return Err(BrokerError::Storage);
        }
    }
    Ok(())
}

fn file_id(file: &File) -> Result<(u32, u32, u32)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle remains live, and information is a sized output buffer.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(|_| BrokerError::Integrity)?;
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

fn delete_locked(file: &File) {
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(1),
    };
    // SAFETY: the live file owns DELETE access; this deletes only that object.
    unsafe {
        let _ = SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        );
    }
}

/// Claim an idle stage before touching its contents. Active downloaders and
/// helpers pin the directory without delete sharing, so DELETE access cannot
/// be acquired while either still uses it. This handle also prevents another
/// helper from pinning the stage, or a path swap during cleanup.
pub fn remove_inactive_staging(directory: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(directory)
        .map_err(|_| BrokerError::Busy)?;
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata().map_err(|_| BrokerError::Integrity)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(BrokerError::Integrity);
    }
    for entry in fs::read_dir(directory).map_err(|_| BrokerError::Storage)? {
        let entry = entry.map_err(|_| BrokerError::Storage)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| BrokerError::Storage)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
            // remove_dir_all does not follow directory reparse points.
            fs::remove_dir_all(entry.path()).map_err(|_| BrokerError::Storage)?;
        } else {
            fs::remove_file(entry.path()).map_err(|_| BrokerError::Storage)?;
        }
    }
    // Delete only the claimed object, and only if it is now empty.
    delete_locked(&file);
    Ok(())
}

struct Release {
    manifest: ReleaseManifest,
    bytes: Vec<u8>,
    signature: Vec<u8>,
}

fn read_release(directory: &Path, key: &str) -> Result<Release> {
    let bytes = manifest::read_bounded(&directory.join(MANIFEST_NAME), 1024 * 1024)?;
    let signature = manifest::read_bounded(&directory.join(SIGNATURE_NAME), 256)?;
    let release = manifest::verify_manifest(
        &bytes,
        std::str::from_utf8(&signature).map_err(|_| BrokerError::Integrity)?,
        key,
    )?;
    Ok(Release {
        manifest: release,
        bytes,
        signature,
    })
}

struct Replacement {
    relative: PathBuf,
    file: File,
    backup: Option<File>,
    installed: bool,
}

struct Transaction {
    destination: PathBuf,
    staging: PathBuf,
    replacements: Vec<Replacement>,
    locks: DirectoryLocks,
    created_directories: Vec<PathBuf>,
    committed: bool,
    protected_files: Option<BTreeMap<PathBuf, String>>,
}

impl Transaction {
    fn prepare(source: &Path, destination: &Path, release: &Release) -> Result<Self> {
        Self::prepare_with_policy(source, destination, release, None)
    }

    fn prepare_protected(
        source: &Path,
        destination: &Path,
        release: &Release,
        current: &Release,
    ) -> Result<Self> {
        identity::assert_protected(destination)?;
        let mut files = current
            .manifest
            .files
            .iter()
            .map(|(name, hash)| Ok((manifest::safe_relative_path(name)?, hash.clone())))
            .collect::<Result<BTreeMap<_, _>>>()?;
        files.insert(
            PathBuf::from(MANIFEST_NAME),
            hex::encode(Sha256::digest(&current.bytes)),
        );
        files.insert(
            PathBuf::from(SIGNATURE_NAME),
            hex::encode(Sha256::digest(&current.signature)),
        );
        Self::prepare_with_policy(source, destination, release, Some(files))
    }

    fn prepare_with_policy(
        source: &Path,
        destination: &Path,
        release: &Release,
        protected_files: Option<BTreeMap<PathBuf, String>>,
    ) -> Result<Self> {
        let mut locks = DirectoryLocks::default();
        locks.pin(source)?;
        locks.pin(destination)?;
        let staging = destination.join(format!(
            ".carbonpaper-update-{:032x}",
            rand::random::<u128>()
        ));
        fs::create_dir(&staging).map_err(|_| BrokerError::Storage)?;
        if protected_files.is_some() {
            identity::protect_path(&staging, true)?;
        }
        let mut transaction = Self {
            destination: destination.to_path_buf(),
            staging: staging.clone(),
            replacements: Vec::new(),
            locks,
            created_directories: vec![staging],
            committed: false,
            protected_files,
        };
        transaction.locks.pin(&transaction.staging.clone())?;
        for (relative, expected) in &release.manifest.files {
            let relative = manifest::safe_relative_path(relative)?;
            transaction.ensure_parent(&source.join(&relative), false)?;
            let mut input = locked_file(&source.join(&relative))?;
            identity::verify_file(&mut input, expected)?;
            input.rewind().map_err(|_| BrokerError::Storage)?;
            transaction.stage(&relative, &mut input, expected)?;
        }
        for (name, bytes) in [
            (MANIFEST_NAME, release.bytes.as_slice()),
            (SIGNATURE_NAME, release.signature.as_slice()),
        ] {
            let hash = hex::encode(Sha256::digest(bytes));
            transaction.stage(Path::new(name), &mut &bytes[..], &hash)?;
        }
        if transaction.protected_files.is_some() {
            for index in 0..transaction.replacements.len() {
                let relative = transaction.replacements[index].relative.clone();
                let target = destination.join(&relative);
                if target.try_exists().map_err(|_| BrokerError::Storage)? {
                    transaction.ensure_parent(&target, false)?;
                    // Detect corrupt files and delete-access failures while the
                    // requesting application is still running.
                    drop(transaction.previous_file(&relative)?);
                }
            }
        }
        Ok(transaction)
    }

    fn ensure_parent(&mut self, path: &Path, create: bool) -> Result<()> {
        let parent = path.parent().ok_or(BrokerError::Integrity)?;
        for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !directory.exists() {
                if !create {
                    return Err(BrokerError::Integrity);
                }
                fs::create_dir(directory).map_err(|_| BrokerError::Storage)?;
                self.created_directories.push(directory.to_path_buf());
                if self.protected_files.is_some() && directory.starts_with(&self.destination) {
                    identity::protect_path(directory, true)?;
                }
            }
            self.locks.pin(directory)?;
            if self.protected_files.is_some() && directory.starts_with(&self.destination) {
                identity::assert_protected(directory)?;
            }
        }
        Ok(())
    }

    fn stage(&mut self, relative: &Path, input: &mut impl Read, hash: &str) -> Result<()> {
        let path = self.staging.join("prepared").join(relative);
        self.ensure_parent(&path, true)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .map_err(|_| BrokerError::Storage)?;
        let copied = std::io::copy(&mut input.take(2 * 1024 * 1024 * 1024 + 1), &mut output)
            .map_err(|_| BrokerError::Storage)?;
        if copied > 2 * 1024 * 1024 * 1024 {
            return Err(BrokerError::LimitExceeded);
        }
        output.sync_all().map_err(|_| BrokerError::Storage)?;
        if self.protected_files.is_some() {
            identity::protect_path(&path, true)?;
            identity::assert_protected(&path)?;
        }
        drop(output);
        let mut file = movable_file(&path)?;
        identity::verify_file(&mut file, hash)?;
        self.replacements.push(Replacement {
            relative: relative.to_path_buf(),
            file,
            backup: None,
            installed: false,
        });
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        for index in 0..self.replacements.len() {
            let relative = self.replacements[index].relative.clone();
            let target = self.destination.join(&relative);
            let backup = self.staging.join("previous").join(&relative);
            self.ensure_parent(&target, true)?;
            self.ensure_parent(&backup, true)?;
            if let Some(file) = self.previous_file(&relative)? {
                rename_locked(
                    &file,
                    &backup,
                    self.locks.directory(backup.parent().unwrap())?,
                )?;
                self.replacements[index].backup = Some(file);
            }
            rename_locked(
                &self.replacements[index].file,
                &target,
                self.locks.directory(target.parent().unwrap())?,
            )?;
            self.replacements[index].installed = true;
        }
        Ok(())
    }

    fn previous_file(&self, relative: &Path) -> Result<Option<File>> {
        let target = self.destination.join(relative);
        if !target.try_exists().map_err(|_| BrokerError::Storage)? {
            return Ok(None);
        }
        let mut file = movable_file(&target)?;
        if let Some(expected) = &self.protected_files {
            identity::assert_protected(&target)?;
            identity::verify_file(
                &mut file,
                expected.get(relative).ok_or(BrokerError::Integrity)?,
            )?;
        }
        Ok(Some(file))
    }

    fn finish(&mut self) {
        self.committed = true;
    }

    fn execution_locks(&self) -> Result<DirectoryLocks> {
        let mut paths = DirectoryLocks::for_execution(&self.destination)?;
        for replacement in &self.replacements {
            let target = self.destination.join(&replacement.relative);
            paths.pin_with_sharing(target.parent().unwrap(), FILE_SHARE_READ.0)?;
        }
        // Freeze the namespace, then confirm every path resolves to the object
        // we verified and moved. The retained replacement handles deny writes
        // and deletion; the extra readers must share our existing DELETE access.
        for replacement in &self.replacements {
            let actual = open_file(
                &self.destination.join(&replacement.relative),
                GENERIC_READ.0,
                FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0,
            )?;
            if !replacement.installed || file_id(&actual)? != file_id(&replacement.file)? {
                return Err(BrokerError::Integrity);
            }
        }
        Ok(paths)
    }

    fn rollback(&mut self) -> Result<()> {
        for replacement in self.replacements.iter_mut().rev() {
            if replacement.installed {
                let prepared = self.staging.join("prepared").join(&replacement.relative);
                rename_locked(
                    &replacement.file,
                    &prepared,
                    self.locks.directory(prepared.parent().unwrap())?,
                )?;
                replacement.installed = false;
            }
            if let Some(backup) = replacement.backup.take() {
                let target = self.destination.join(&replacement.relative);
                if let Err(error) = rename_locked(
                    &backup,
                    &target,
                    self.locks.directory(target.parent().unwrap())?,
                ) {
                    replacement.backup = Some(backup);
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // A failed rollback preserves the staged objects for recovery.
        if !self.committed && self.rollback().is_err() {
            return;
        }
        for replacement in &self.replacements {
            if self.committed {
                if let Some(backup) = &replacement.backup {
                    delete_locked(backup);
                }
            } else {
                delete_locked(&replacement.file);
            }
        }
        self.replacements.clear();
        self.locks.entries.clear();
        // Empty-directory deletion cannot follow a substituted tree or remove
        // unrelated user files. Concurrent insertions simply leave a directory.
        for directory in self.created_directories.iter().rev() {
            let _ = fs::remove_dir(directory);
        }
    }
}

fn parent_pid() -> Result<u32> {
    parent_pid_of(std::process::id())
}

fn parent_pid_of(pid: u32) -> Result<u32> {
    // SAFETY: the snapshot is owned, and PROCESSENTRY32W is correctly sized.
    unsafe {
        let snapshot = OwnedHandle(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|_| BrokerError::AccessDenied)?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut entry).map_err(|_| BrokerError::AccessDenied)?;
        loop {
            if entry.th32ProcessID == pid {
                return Ok(entry.th32ParentProcessID);
            }
            if Process32NextW(snapshot.0, &mut entry).is_err() {
                return Err(BrokerError::AccessDenied);
            }
        }
    }
}

/// The protected development helper has a different signing domain and must
/// never acquire a general-purpose update mode.
pub fn run(args: &[String]) -> Result<()> {
    if cfg!(feature = "development-runtime") || is_elevated() {
        return Err(BrokerError::AccessDenied);
    }
    let elevate = args.len() == 7 && args[6] == "--elevate";
    if (!elevate && args.len() != 6)
        || args[0] != "--source"
        || args[2] != "--caller-pid"
        || args[4] != "--version"
    {
        return Err(BrokerError::InvalidRequest);
    }
    let pid = args[3]
        .parse::<u32>()
        .map_err(|_| BrokerError::InvalidRequest)?;
    if parent_pid()? != pid {
        return Err(BrokerError::AccessDenied);
    }
    let caller = identity::inspect_process(pid)?;
    if caller.principal.sid != identity::current_sid()?
        || caller
            .image
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.eq_ignore_ascii_case("carbonpaper.exe"))
        || identity::active_runtime()?.is_some()
    {
        return Err(BrokerError::AccessDenied);
    }
    let destination = caller.image.parent().ok_or(BrokerError::Integrity)?;
    let source = PathBuf::from(&args[1]);
    let mut locks = DirectoryLocks::default();
    locks.pin(destination)?;
    locks.pin(&source)?;
    let current = read_release(destination, manifest::RELEASE_PUBLIC_KEY)?;
    let release = read_release(&source, manifest::RELEASE_PUBLIC_KEY)?;
    if release.manifest.version != args[5]
        || semver::Version::parse(&release.manifest.version)
            .map_err(|_| BrokerError::VersionMismatch)?
            <= semver::Version::parse(&current.manifest.version)
                .map_err(|_| BrokerError::VersionMismatch)?
    {
        return Err(BrokerError::VersionMismatch);
    }
    identity::verify_file(
        &mut locked_file(&caller.image)?,
        current
            .manifest
            .files
            .get("carbonpaper.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    let own_path = std::env::current_exe().map_err(|_| BrokerError::Integrity)?;
    if !identity::path_eq(&own_path, &source.join(HELPER_NAME)) {
        return Err(BrokerError::Integrity);
    }
    let mut own_file = locked_file(&own_path)?;
    identity::verify_file(
        &mut own_file,
        release
            .manifest
            .files
            .get(HELPER_NAME)
            .ok_or(BrokerError::Integrity)?,
    )?;
    // Retain the verified execution namespace through any UAC prompt and the
    // lifetime of the elevated copy process.
    let _execution_paths = DirectoryLocks::for_execution(&source)?;
    // SAFETY: this retains a waitable handle to the already authenticated live
    // parent. Holding the original process handle prevents PID reuse.
    let parent = unsafe {
        OwnedHandle(
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
            .map_err(|_| BrokerError::AccessDenied)?,
        )
    };
    if elevate {
        return elevation::coordinate(&source, destination, &current, &release, pid, parent.0);
    }
    let mut transaction = Transaction::prepare(&source, destination, &release)?;
    // The app learns that all bytes are verified and prepared through an
    // anonymous pipe owned by its exact child, before stopping capture or exiting.
    writeln!(std::io::stdout(), "{READY}").map_err(|_| BrokerError::Unavailable)?;
    std::io::stdout()
        .flush()
        .map_err(|_| BrokerError::Unavailable)?;
    let mut decision = String::new();
    std::io::stdin()
        .lock()
        .take(16)
        .read_line(&mut decision)
        .map_err(|_| BrokerError::Unavailable)?;
    if decision != "COMMIT\n" {
        return Err(BrokerError::InvalidRequest);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // SAFETY: the process handle remains owned for the entire wait.
        if unsafe { WaitForSingleObject(parent.0, 200) } == WAIT_OBJECT_0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(BrokerError::Busy);
        }
    }
    let installed = transaction.commit().and_then(|_| {
        // All replacement handles still deny writes/deletion while the new
        // process opens its executable and DLLs. No elevation is involved.
        let _launch_paths = transaction.execution_locks()?;
        std::process::Command::new(destination.join("carbonpaper.exe"))
            .current_dir(destination)
            .spawn()
            .map_err(|_| BrokerError::Unavailable)?;
        Ok(())
    });
    if let Err(error) = installed {
        transaction.rollback()?;
        let _launch_paths = DirectoryLocks::for_execution(destination)?;
        let mut executable = locked_file(&caller.image)?;
        identity::verify_file(
            &mut executable,
            current
                .manifest
                .files
                .get("carbonpaper.exe")
                .ok_or(BrokerError::Integrity)?,
        )?;
        let _ = std::process::Command::new(&caller.image)
            .current_dir(destination)
            .spawn();
        return Err(error);
    }
    transaction.finish();
    Ok(())
}

#[cfg(all(test, not(feature = "development-runtime")))]
mod tests;

#[cfg(all(test, feature = "development-runtime"))]
mod development_tests {
    #[test]
    fn development_helper_disables_native_updates_before_argument_or_source_processing() {
        assert!(matches!(
            super::run(&[]),
            Err(crate::protocol::BrokerError::AccessDenied)
        ));
        assert!(matches!(
            super::run_elevated(&[]),
            Err(crate::protocol::BrokerError::AccessDenied)
        ));
    }
}
