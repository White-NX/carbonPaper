//! Fixed-root reclamation retains every activation, owner, service version,
//! pending installation and running image. No archive directory is accepted.
use super::{activation, identity, install, service};
use crate::{ledger::Ledger, manifest, protocol::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Storage::FileSystem::{
            MoveFileExW, FILE_SHARE_DELETE, FILE_SHARE_READ, MOVEFILE_DELAY_UNTIL_REBOOT,
        },
        System::{Diagnostics::ToolHelp::*, Threading::*},
        UI::Shell::IsUserAnAdmin,
    },
};

const MAX_CLEANUP_HELPERS: usize = 16;

fn add_activation(
    path: &Path,
    keep: &mut BTreeSet<String>,
    validate: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    validate(path)?;
    let record: identity::Activation = serde_json::from_slice(&manifest::read_bounded(path, 1024)?)
        .map_err(|_| BrokerError::Integrity)?;
    if !identity::safe_runtime_id(&record.runtime_id) {
        return Err(BrokerError::Integrity);
    }
    keep.insert(record.runtime_id);
    Ok(())
}

fn references(root: &Path, ledger: &Ledger, retain_service: bool) -> Result<BTreeSet<String>> {
    let mut keep = persistent_references(root, ledger, identity::assert_protected, retain_service)?;
    keep.extend(running_runtimes(root)?);
    Ok(keep)
}

fn persistent_references(
    root: &Path,
    ledger: &Ledger,
    validate: impl Fn(&Path) -> Result<()>,
    retain_service: bool,
) -> Result<BTreeSet<String>> {
    let mut keep = BTreeSet::new();
    for sid in ledger.registered_owner_sids()? {
        let runtime = ledger
            .registered_runtime(&sid)?
            .ok_or(BrokerError::Integrity)?;
        if !identity::safe_runtime_id(&runtime) {
            return Err(BrokerError::Integrity);
        }
        keep.insert(runtime);
    }
    for item in fs::read_dir(root.join("Activations")).map_err(|_| BrokerError::Storage)? {
        let path = item.map_err(|_| BrokerError::Storage)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            add_activation(&path, &mut keep, &validate)?;
        }
    }
    if root.join("Detached").exists() {
        validate(&root.join("Detached"))?;
        for item in fs::read_dir(root.join("Detached")).map_err(|_| BrokerError::Storage)? {
            let path = item.map_err(|_| BrokerError::Storage)?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                add_activation(&path, &mut keep, &validate)?;
            }
        }
    }
    let system = root.join("System/runtime.json");
    if retain_service {
        if let Some(id) = ledger.service_runtime()? {
            if !identity::safe_runtime_id(&id) {
                return Err(BrokerError::Integrity);
            }
            keep.insert(id);
        }
    }
    if retain_service && system.exists() {
        add_activation(&system, &mut keep, &validate)?;
    }
    if let Some(pending) = activation::read_pending(root)? {
        keep.insert(pending.runtime_id);
        keep.insert(pending.system_runtime_id);
    }
    Ok(keep)
}

fn running_runtimes(root: &Path) -> Result<BTreeSet<String>> {
    let mut keep = BTreeSet::new();
    // SAFETY: this snapshot only reads process identity. It is retained through
    // enumeration and no process is stopped, modified or trusted by its name.
    unsafe {
        let snapshot = identity::OwnedHandle(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|_| BrokerError::Unavailable)?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot.0, &mut entry).is_err() {
            return Err(BrokerError::Unavailable);
        }
        loop {
            let length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]).to_ascii_lowercase();
            if name.starts_with("carbonpaper") {
                // An inaccessible matching image keeps collection conservative.
                if let Some(image) = collection_image(entry.th32ProcessID)? {
                    if let Some(directory) = image.parent() {
                        if directory
                            .parent()
                            .is_some_and(|parent| identity::path_eq(parent, &root.join("Runtime")))
                        {
                            let id = directory
                                .file_name()
                                .and_then(|name| name.to_str())
                                .ok_or(BrokerError::Integrity)?;
                            if !identity::safe_runtime_id(id) {
                                return Err(BrokerError::Integrity);
                            }
                            keep.insert(id.to_string());
                        }
                    }
                }
            }
            if Process32NextW(snapshot.0, &mut entry).is_err() {
                break;
            }
        }
    }
    Ok(keep)
}

fn collection_image(pid: u32) -> Result<Option<PathBuf>> {
    // SAFETY: this query supplies retention hints only. It grants no authority
    // and does not require opening another user's token. A process that exited
    // after the snapshot is harmless; an inaccessible live image stops cleanup.
    unsafe {
        let process = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => identity::OwnedHandle(handle),
            Err(error) if error.code() == windows::core::HRESULT::from_win32(87) => {
                return Ok(None)
            }
            Err(_) => return Err(BrokerError::Unavailable),
        };
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        if QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .is_err()
        {
            let (mut creation, mut exit, mut kernel, mut user) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            if GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user).is_ok()
                && (exit.dwHighDateTime != 0 || exit.dwLowDateTime != 0)
            {
                return Ok(None);
            }
            return Err(BrokerError::Unavailable);
        }
        String::from_utf16(&path[..length as usize])
            .map(PathBuf::from)
            .map(Some)
            .map_err(|_| BrokerError::Integrity)
    }
}

fn files_in_tree(
    root: &Path,
    files: &mut Vec<PathBuf>,
    validate: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    validate(root)?;
    identity::no_reparse(root)?;
    if root.is_dir() {
        for item in fs::read_dir(root).map_err(|_| BrokerError::Storage)? {
            files_in_tree(
                &item.map_err(|_| BrokerError::Storage)?.path(),
                files,
                validate,
            )?;
        }
    } else {
        if files.len() >= 8192 {
            return Err(BrokerError::LimitExceeded);
        }
        files.push(root.to_path_buf());
    }
    Ok(())
}

fn remove_unused_tree(
    root: &Path,
    target: &Path,
    validate: impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    if target
        .parent()
        .is_none_or(|parent| !identity::path_eq(parent, &root.join("Runtime")))
        || target
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !identity::safe_runtime_id(name))
    {
        return Err(BrokerError::Integrity);
    }
    let mut files = Vec::new();
    files_in_tree(target, &mut files, &validate)?;
    remove_unused_files(&files, validate)?;
    fs::remove_dir_all(target).map_err(|_| BrokerError::Storage)
}

fn pin_unused_files(
    files: &[PathBuf],
    validate: impl Fn(&Path) -> Result<()>,
) -> Result<Vec<File>> {
    let mut pinned: Vec<File> = Vec::new();
    for path in files {
        validate(path)?;
        identity::no_reparse(path)?;
        if !path.is_file() {
            return Err(BrokerError::Integrity);
        }
        // WRITE_DATA prevents a new process from mapping the executable while
        // collection is prepared. DELETE sharing lets us retire its files.
        // Any existing reader or executable that denies writes keeps it intact.
        pinned.push(
            fs::OpenOptions::new()
                .access_mode(0x0001_0002)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
                .open(path)
                .map_err(|_| BrokerError::Busy)?,
        );
    }
    Ok(pinned)
}

fn delete_pinned_files(files: &[PathBuf]) -> Result<()> {
    for path in files {
        let name = identity::wide(path.as_os_str());
        // SAFETY: every file was validated and exclusively pinned for writing
        // before the first deletion. No running/new image can map these bytes.
        unsafe { windows::Win32::Storage::FileSystem::DeleteFileW(PCWSTR(name.as_ptr())) }
            .map_err(|_| BrokerError::Busy)?;
    }
    Ok(())
}

fn remove_unused_files(files: &[PathBuf], validate: impl Fn(&Path) -> Result<()>) -> Result<()> {
    let pinned = pin_unused_files(files, validate)?;
    delete_pinned_files(files)?;
    drop(pinned);
    Ok(())
}

/// The installer or maintenance caller owns the setup lock.
pub(super) fn collect(root: &Path, ledger: &Ledger) -> Result<usize> {
    for path in [
        root.to_path_buf(),
        root.join("System"),
        root.join("Runtime"),
        root.join("Activations"),
    ] {
        identity::assert_protected(&path)?;
    }
    let keep = references(root, ledger, true)?;
    collect_stale_setup_files(root, identity::assert_protected)?;
    let mut collected = 0;
    for item in fs::read_dir(root.join("Runtime")).map_err(|_| BrokerError::Storage)? {
        let path = item.map_err(|_| BrokerError::Storage)?.path();
        let id = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(BrokerError::Integrity)?;
        if !identity::safe_runtime_id(id) || keep.contains(id) {
            continue;
        }
        match remove_unused_tree(root, &path, identity::assert_protected) {
            Ok(()) => {
                collected += 1;
            }
            Err(BrokerError::Busy) => continue,
            Err(error) => return Err(error),
        }
        // Pace large release-tree removal outside the foreground/update path.
        if collected >= 2 {
            break;
        }
    }
    Ok(collected)
}

fn cleanup_stem(name: &str) -> Option<&str> {
    if !name.is_ascii()
        || name.len() < 28
        || !name.starts_with("cleanup-")
        || !name[8..24].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let suffix = &name[24..];
    if matches!(suffix, ".exe" | ".json" | ".sig")
        || suffix
            .strip_prefix(".tmp-")
            .is_some_and(|id| id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        Some(&name[..24])
    } else {
        None
    }
}

fn collect_stale_setup_files(root: &Path, validate: impl Fn(&Path) -> Result<()>) -> Result<usize> {
    let pending = activation::read_pending(root)?.is_some();
    let mut helpers: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for item in fs::read_dir(root.join("System")).map_err(|_| BrokerError::Storage)? {
        let path = item.map_err(|_| BrokerError::Storage)?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(BrokerError::Integrity)?;
        if let Some(stem) = cleanup_stem(name) {
            helpers.entry(stem.to_string()).or_default().push(path);
            continue;
        }
        let temporary = [
            "carbonpaper-key-service.tmp-",
            "carbonpaper-protected-setup.tmp-",
            "runtime.tmp-",
            "service.tmp-",
            "setup.tmp-",
            "activation.pending.tmp-",
        ]
        .into_iter()
        .any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 16 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
            })
        });
        if !temporary && (pending || !matches!(name, "service.previous" | "setup.previous")) {
            continue;
        }
        match remove_unused_files(&[path], &validate) {
            Ok(()) | Err(BrokerError::Busy) => {}
            Err(error) => return Err(error),
        }
    }
    let mut live_helpers = 0;
    for files in helpers.into_values() {
        // Pin a collector and all its companions before deleting any of them.
        // A mapped executable keeps the signed snapshot intact until exit;
        // failed launches and companion-only interrupted writes are reclaimed.
        match remove_unused_files(&files, &validate) {
            Ok(()) => {}
            Err(BrokerError::Busy) => live_helpers += 1,
            Err(error) => return Err(error),
        }
    }
    Ok(live_helpers)
}

fn prepare_cleanup(root: &Path, validate: impl Fn(&Path) -> Result<()>) -> Result<()> {
    // The setup lock serializes scheduling. Repairs while the desktop stays
    // open must not accumulate unlimited waiting processes and signed copies.
    if collect_stale_setup_files(root, validate)? >= MAX_CLEANUP_HELPERS {
        return Err(BrokerError::LimitExceeded);
    }
    Ok(())
}

#[cfg(not(test))]
pub(super) fn maintenance(root: &Path, ledger: &Ledger) -> Result<()> {
    identity::assert_protected(&root.join("System/setup.lock"))?;
    let _lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("System/setup.lock"))
    {
        Ok(lock) => lock,
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(()),
        Err(_) => return Err(BrokerError::Storage),
    };
    super::revocation::collect_unregistered(&identity::state_root()?, ledger)?;
    collect(root, ledger).map(|_| ())
}

pub(super) fn schedule_after(root: &Path, pid: u32) -> Result<()> {
    prepare_cleanup(root, identity::assert_protected)?;
    let own = identity::inspect_process(std::process::id())?.image;
    let (original, directory) = collector_source(root, &own)?;
    let bytes = manifest::read_bounded(&directory.join(manifest::MANIFEST_NAME), 1024 * 1024)?;
    let signature = manifest::read_bounded(&directory.join(manifest::SIGNATURE_NAME), 256)?;
    let signature_text = std::str::from_utf8(&signature).map_err(|_| BrokerError::Integrity)?;
    let release = manifest::verify_manifest(&bytes, signature_text, manifest::RELEASE_PUBLIC_KEY)?;
    let runtime_id = format!(
        "{}-{}",
        release.version,
        &crate::crypto::digest(&bytes)[..16]
    );
    if directory
        .parent()
        .is_some_and(|parent| identity::path_eq(parent, &root.join("Runtime")))
        && directory.file_name().and_then(|name| name.to_str()) != Some(runtime_id.as_str())
    {
        return Err(BrokerError::Integrity);
    }
    let helper = root
        .join("System")
        .join(format!("cleanup-{}.exe", &random_id()[..16]));
    install::copy_verified(
        &original,
        &helper,
        release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    install::atomic_write(&helper.with_extension("json"), &bytes)?;
    install::atomic_write(&helper.with_extension("sig"), &signature)?;
    std::process::Command::new(&helper)
        .args(["--collect-after", &pid.to_string()])
        .current_dir(root.join("System"))
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map_err(|_| BrokerError::Unavailable)?;
    Ok(())
}

#[cfg(not(test))]
pub(super) fn retire_unused_service(root: &Path, ledger: &Ledger) -> Result<bool> {
    identity::assert_protected(&root.join("System/setup.lock"))?;
    let _lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("System/setup.lock"))
    {
        Ok(lock) => lock,
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(false),
        Err(_) => return Err(BrokerError::Storage),
    };
    if ledger.registered_owners()? != 0 {
        return Ok(false);
    }
    schedule_after(root, std::process::id())?;
    if let Some(handle) = service::try_open_service(0x0001_0000)? {
        // SAFETY: LocalSystem may remove only this fixed service registration.
        // Its process exits normally, and the private collector waits for it.
        unsafe { windows::Win32::System::Services::DeleteService(handle.0) }
            .map_err(|_| BrokerError::AccessDenied)?;
    }
    Ok(true)
}

fn collector_source(root: &Path, own: &Path) -> Result<(PathBuf, PathBuf)> {
    let system = root.join("System");
    if own
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("carbonpaper-protected-setup.exe"))
        && own
            .parent()
            .is_some_and(|parent| !identity::path_eq(parent, &system))
    {
        // A new package can uninstall an older component without activating
        // it first. Its running, signed helper knows the collection protocol;
        // the old System companion may not support it yet.
        return Ok((own.to_path_buf(), own.parent().unwrap().to_path_buf()));
    }
    let marker = root.join("System/runtime.json");
    identity::assert_protected(&marker)?;
    let activation: identity::Activation =
        serde_json::from_slice(&manifest::read_bounded(&marker, 1024)?)
            .map_err(|_| BrokerError::Integrity)?;
    if !identity::safe_runtime_id(&activation.runtime_id) {
        return Err(BrokerError::Integrity);
    }
    let directory = root.join("Runtime").join(&activation.runtime_id);
    identity::assert_protected(&directory)?;
    let helper = system.join("carbonpaper-protected-setup.exe");
    identity::assert_protected(&helper)?;
    Ok((helper, directory))
}

pub fn run_after(pid: u32) -> Result<()> {
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied);
    }
    let root = identity::protected_root()?;
    let own = identity::inspect_process(std::process::id())?.image;
    if own
        .parent()
        .is_none_or(|parent| !identity::path_eq(parent, &root.join("System")))
        || own
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| {
                !name.is_ascii() || !name.ends_with(".exe") || cleanup_stem(name).is_none()
            })
    {
        return Err(BrokerError::AccessDenied);
    }
    identity::assert_protected(&own)?;
    // Pin the collector's own signed release, independent of a later service
    // update or reclamation of the original runtime directory.
    identity::assert_protected(&own.with_extension("json"))?;
    identity::assert_protected(&own.with_extension("sig"))?;
    let bytes = manifest::read_bounded(&own.with_extension("json"), 1024 * 1024)?;
    let signature = String::from_utf8(manifest::read_bounded(&own.with_extension("sig"), 256)?)
        .map_err(|_| BrokerError::Integrity)?;
    let release = manifest::verify_manifest(&bytes, &signature, manifest::RELEASE_PUBLIC_KEY)?;
    identity::verify_file(
        &mut identity::locked_file(&own)?,
        release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    // Only this unique retired helper is scheduled for reboot deletion; a
    // subsequent reinstall cannot reuse its pathname or lose an active image.
    for path in [
        own.clone(),
        own.with_extension("json"),
        own.with_extension("sig"),
    ] {
        let name = identity::wide(path.as_os_str());
        // SAFETY: these are this collector's unique protected files; none is
        // an activation pathname that a future installation could reuse.
        unsafe {
            let _ = MoveFileExW(
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );
        }
    }
    if let Ok(parent) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        let parent = identity::OwnedHandle(parent);
        loop {
            match unsafe { WaitForSingleObject(parent.0, 1000) } {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => continue,
                _ => return Err(BrokerError::Unavailable),
            }
        }
    }
    let state = identity::state_root()?;
    loop {
        match collect_after_exit_pass(&root, &state) {
            Ok(true) => return Ok(()),
            Ok(false) | Err(BrokerError::Busy) => {
                // Desktop images, file scanners and installers can outlive the
                // launcher. Retry without holding the setup or database lock.
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            Err(error) => return Err(error),
        }
    }
}

fn has_detached_application(root: &Path) -> Result<bool> {
    let directory = root.join("Detached");
    if !directory.try_exists().map_err(|_| BrokerError::Storage)? {
        return Ok(false);
    }
    identity::assert_protected(&directory)?;
    for entry in fs::read_dir(directory).map_err(|_| BrokerError::Storage)? {
        let path = entry.map_err(|_| BrokerError::Storage)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn collect_after_exit_pass(root: &Path, state: &Path) -> Result<bool> {
    if !state
        .join("keys.db")
        .try_exists()
        .map_err(|_| BrokerError::Storage)?
    {
        return Ok(true);
    }
    identity::assert_protected(root)?;
    identity::assert_protected(state)?;
    identity::assert_protected(&root.join("System/setup.lock"))?;
    let _lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("System/setup.lock"))
        .map_err(|_| BrokerError::Busy)?;
    if !state
        .join("keys.db")
        .try_exists()
        .map_err(|_| BrokerError::Storage)?
    {
        return Ok(true);
    }
    let ledger = Ledger::open_existing(&state.join("keys.db"))?;
    let empty = ledger.registered_owners()? == 0 && activation::read_pending(root)?.is_none();
    let detached = has_detached_application(root)?;
    if empty {
        // The authoritative owner count is checked under the installation lock.
        // Finish an interrupted service removal without racing a reinstall.
        install::stop_service()?;
        if let Some(handle) = service::try_open_service(0x0001_0000)? {
            unsafe { windows::Win32::System::Services::DeleteService(handle.0) }
                .map_err(|_| BrokerError::AccessDenied)?;
            return Ok(false);
        }
    }
    if empty && detached {
        ledger.compact_if_empty()?;
    }
    super::revocation::collect_unregistered(state, &ledger)?;
    // Repeat bounded passes after exit to collect more than two stale releases.
    for _ in 0..64 {
        if collect(root, &ledger)? == 0 {
            break;
        }
    }
    if empty && !detached {
        return finish_full_uninstall(root, state, ledger, identity::assert_protected);
    }
    Ok(true)
}

fn finish_full_uninstall(
    root: &Path,
    state: &Path,
    ledger: Ledger,
    validate: impl Fn(&Path) -> Result<()>,
) -> Result<bool> {
    // The current service runtime is removed last. Its marker stays usable for
    // reinstallation if an older tree or a running image delays reclamation.
    let mut keep = persistent_references(root, &ledger, &validate, false)?;
    keep.extend(running_runtimes(root)?);
    if !keep.is_empty() {
        return Ok(false);
    }
    let marker = root.join("System/runtime.json");
    let current = if marker.try_exists().map_err(|_| BrokerError::Storage)? {
        validate(&marker)?;
        let activation: identity::Activation =
            serde_json::from_slice(&manifest::read_bounded(&marker, 1024)?)
                .map_err(|_| BrokerError::Integrity)?;
        if !identity::safe_runtime_id(&activation.runtime_id) {
            return Err(BrokerError::Integrity);
        }
        Some(root.join("Runtime").join(activation.runtime_id))
    } else {
        None
    };
    for entry in fs::read_dir(root.join("Runtime")).map_err(|_| BrokerError::Storage)? {
        let path = entry.map_err(|_| BrokerError::Storage)?.path();
        if current
            .as_ref()
            .is_none_or(|current| !identity::path_eq(current, &path))
        {
            return Ok(false);
        }
    }
    // SQLite itself holds a file handle. Close it before pinning every service
    // file against readers/writers and executable starts for the final deletion.
    drop(ledger);
    let mut files = Vec::new();
    for path in [
        "keys.db",
        "keys.db-journal",
        "keys.db-wal",
        "keys.db-shm",
        "logs/service.log",
        "logs/service.log.1",
    ]
    .into_iter()
    .map(|file| state.join(file))
    .chain(
        [
            "carbonpaper-key-service.exe",
            "carbonpaper-protected-setup.exe",
            "service.previous",
            "setup.previous",
            "runtime.json",
            "repair-runtime.json",
        ]
        .into_iter()
        .map(|file| root.join("System").join(file)),
    ) {
        if path.try_exists().map_err(|_| BrokerError::Storage)? {
            files.push(path);
        }
    }
    let inboxes = state.join("Revocations");
    if inboxes.try_exists().map_err(|_| BrokerError::Storage)? {
        identity::assert_protected(&inboxes)?;
        for entry in fs::read_dir(&inboxes).map_err(|_| BrokerError::Storage)? {
            let path = entry.map_err(|_| BrokerError::Storage)?.path();
            if path.extension().is_none_or(|extension| extension != "log")
                || path
                    .file_stem()
                    .and_then(|sid| sid.to_str())
                    .is_none_or(|sid| !identity::safe_sid(sid))
            {
                return Err(BrokerError::Integrity);
            }
            files.push(path);
        }
    }
    let pinned = pin_unused_files(&files, |path| {
        if path
            .parent()
            .is_some_and(|parent| identity::path_eq(parent, &inboxes))
        {
            let sid = path
                .file_stem()
                .and_then(|sid| sid.to_str())
                .ok_or(BrokerError::Integrity)?;
            identity::assert_revocation_inbox(path, sid)
        } else {
            validate(path)
        }
    })?;
    if let Some(current) = current {
        if current.try_exists().map_err(|_| BrokerError::Storage)? {
            remove_unused_tree(root, &current, &validate)?;
        }
    }
    delete_pinned_files(&files)?;
    drop(pinned);
    // Preserve the shared setup lock so a future installer cannot obtain a
    // different lock during teardown. Remove only empty state namespaces.
    let _ = fs::remove_dir(state.join("logs"));
    let _ = fs::remove_dir(inboxes);
    let _ = fs::remove_dir(state);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_uninstall_uses_its_running_helper_when_the_installed_component_is_older() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("Protected");
        let package = directory.path().join("NewPackage");
        fs::create_dir_all(root.join("System")).unwrap();
        fs::create_dir(&package).unwrap();
        // The installed companion predates --collect-after. Its marker and
        // bytes must not become the private collector for this new uninstall.
        fs::write(
            root.join("System/carbonpaper-protected-setup.exe"),
            b"old helper",
        )
        .unwrap();
        fs::write(root.join("System/runtime.json"), b"old component marker").unwrap();
        let own = package.join("carbonpaper-protected-setup.exe");
        fs::write(&own, b"new running helper").unwrap();
        assert_eq!(
            collector_source(&root, &own).unwrap(),
            (own.clone(), package)
        );
        fs::remove_file(root.join("System/runtime.json")).unwrap();
        assert_eq!(collector_source(&root, &own).unwrap().0, own);
    }

    #[test]
    fn full_removal_waits_for_database_readers_before_deleting_the_current_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("Protected");
        let state = directory.path().join("State");
        let current = root.join("Runtime/current-runtime");
        for path in [
            &current,
            &root.join("System"),
            &root.join("Activations"),
            &state,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(current.join("carbonpaper.exe"), b"synthetic runtime").unwrap();
        let marker = root.join("System/runtime.json");
        fs::write(
            &marker,
            serde_json::to_vec(&identity::Activation {
                runtime_id: "current-runtime".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let archive = directory.path().join("archive.db");
        fs::write(&archive, b"user archive remains intact").unwrap();
        let ledger = Ledger::open(&state.join("keys.db")).unwrap();
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0 | windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE.0)
            .open(state.join("keys.db"))
            .unwrap();
        assert_eq!(
            finish_full_uninstall(&root, &state, ledger, |_| Ok(())),
            Err(BrokerError::Busy)
        );
        assert!(current.join("carbonpaper.exe").exists());
        assert!(marker.exists());
        assert!(state.join("keys.db").exists());
        drop(reader);
        let ledger = Ledger::open_existing(&state.join("keys.db")).unwrap();
        assert!(finish_full_uninstall(&root, &state, ledger, |_| Ok(())).unwrap());
        assert!(!current.exists());
        assert!(!marker.exists());
        assert!(!state.join("keys.db").exists());
        assert_eq!(fs::read(archive).unwrap(), b"user archive remains intact");
    }

    #[test]
    fn stale_collectors_keep_running_snapshots_and_reclaim_companion_only_orphans() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let system = root.join("System");
        fs::create_dir(&system).unwrap();
        let stem = "cleanup-0123456789abcdef";
        for suffix in ["exe", "json", "sig", "tmp-0123456789abcdef"] {
            fs::write(
                system.join(format!("{stem}.{suffix}")),
                b"synthetic collector",
            )
            .unwrap();
        }
        let pinned = identity::locked_file(&system.join(format!("{stem}.exe"))).unwrap();
        let orphan = system.join("cleanup-abcdef0123456789.json");
        fs::write(&orphan, b"orphan snapshot").unwrap();
        let marker = system.join("runtime.json");
        fs::write(&marker, b"active service marker").unwrap();
        let unrelated = system.join("cleanup-not-a-random-id.json");
        fs::write(&unrelated, b"unrelated file").unwrap();
        collect_stale_setup_files(root, |_| Ok(())).unwrap();
        assert!(!orphan.exists());
        for suffix in ["exe", "json", "sig", "tmp-0123456789abcdef"] {
            assert!(system.join(format!("{stem}.{suffix}")).exists());
        }
        drop(pinned);
        collect_stale_setup_files(root, |_| Ok(())).unwrap();
        for suffix in ["exe", "json", "sig", "tmp-0123456789abcdef"] {
            assert!(!system.join(format!("{stem}.{suffix}")).exists());
        }
        assert!(marker.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn cleanup_capacity_defers_new_helpers_until_an_existing_collector_exits() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let system = root.join("System");
        fs::create_dir(&system).unwrap();
        let mut pinned = Vec::new();
        for id in 0..16 {
            let executable = system.join(format!("cleanup-{id:016x}.exe"));
            fs::write(&executable, b"synthetic collector").unwrap();
            fs::write(executable.with_extension("json"), b"signed snapshot").unwrap();
            pinned.push(identity::locked_file(&executable).unwrap());
        }
        assert_eq!(
            prepare_cleanup(root, |_| Ok(())),
            Err(BrokerError::LimitExceeded)
        );
        assert_eq!(fs::read_dir(&system).unwrap().count(), 32);
        drop(pinned.pop());
        assert!(prepare_cleanup(root, |_| Ok(())).is_ok());
        assert_eq!(fs::read_dir(&system).unwrap().count(), 30);
        assert!(system.join("cleanup-0000000000000000.json").exists());
        assert!(!system.join("cleanup-000000000000000f.json").exists());
    }

    #[test]
    fn collection_keeps_locked_runtime_bytes_intact_then_removes_them_after_exit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let target = root.join("Runtime/old-runtime");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("carbonpaper.exe"), b"synthetic executable").unwrap();
        fs::write(target.join("resource.dll"), b"synthetic resource").unwrap();
        let pinned = identity::locked_file(&target.join("carbonpaper.exe")).unwrap();
        assert_eq!(
            remove_unused_tree(root, &target, |_| Ok(())),
            Err(BrokerError::Busy)
        );
        assert_eq!(
            fs::read(target.join("resource.dll")).unwrap(),
            b"synthetic resource"
        );
        drop(pinned);
        remove_unused_tree(root, &target, |_| Ok(())).unwrap();
        assert!(!target.exists());
        assert_eq!(fs::read_dir(root.join("Runtime")).unwrap().count(), 0);
        assert_eq!(
            remove_unused_tree(root, root, |_| Ok(())),
            Err(BrokerError::Integrity)
        );
    }

    #[test]
    fn collection_retains_registered_activated_detached_service_and_pending_versions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for path in ["Activations", "Detached", "System"] {
            fs::create_dir(root.join(path)).unwrap();
        }
        let mut ledger = Ledger::open(&root.join("keys.db")).unwrap();
        ledger
            .register_owner("S-1-5-21-100", "registered", true)
            .unwrap();
        for (relative, runtime_id) in [
            ("Activations/S-1-5-21-101.json", "activated"),
            ("Detached/S-1-5-21-102.json", "detached"),
            ("System/runtime.json", "service"),
        ] {
            install::atomic_write(
                &root.join(relative),
                &serde_json::to_vec(&identity::Activation {
                    runtime_id: runtime_id.into(),
                })
                .unwrap(),
            )
            .unwrap();
        }
        activation::begin(
            root,
            &activation::PendingActivation {
                sid: "S-1-5-21-100".into(),
                runtime_id: "pending-client".into(),
                system_runtime_id: "pending-service".into(),
            },
        )
        .unwrap();
        assert_eq!(
            persistent_references(root, &ledger, |_| Ok(()), true).unwrap(),
            [
                "registered",
                "activated",
                "detached",
                "service",
                "pending-client",
                "pending-service"
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );
    }

    #[test]
    fn preparation_write_handles_prevent_a_new_image_from_starting() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("synthetic-cmd.exe");
        fs::copy(
            identity::system_directory().unwrap().join("cmd.exe"),
            &executable,
        )
        .unwrap();
        let lock = fs::OpenOptions::new()
            .access_mode(0x0001_0002)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0)
            .open(&executable)
            .unwrap();
        assert!(std::process::Command::new(&executable)
            .args(["/c", "exit", "0"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .is_err());
        drop(lock);
        assert!(std::process::Command::new(&executable)
            .args(["/c", "exit", "0"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .status()
            .unwrap()
            .success());
    }
}
