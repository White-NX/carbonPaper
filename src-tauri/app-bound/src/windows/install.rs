//! Administrator-only fixed-root installer. It copies bytes from locked file
//! handles and validates the release signature again after elevation.
use crate::{
    ledger::Ledger,
    manifest::{self, ReleaseManifest, MANIFEST_NAME, SIGNATURE_NAME},
    protocol::*,
    windows::{
        identity::*,
        service::{self, ServiceHandle},
    },
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
        System::Services::*,
        UI::Shell::IsUserAnAdmin,
    },
};

pub struct InstallOptions {
    pub source: PathBuf,
    pub caller_pid: u32,
    pub enable: bool,
}

/// Unelevated restart helper. It can only launch this user's registered main
/// executable and accepts no executable path, DLL path or arbitrary arguments.
pub fn launch_after(pid: u32) -> Result<()> {
    if unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied);
    }
    if let Ok(process) = inspect_process(pid) {
        if process.principal.sid != current_sid()? {
            return Err(BrokerError::AccessDenied);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let mut exit_code = 0;
            unsafe {
                windows::Win32::System::Threading::GetExitCodeProcess(
                    process.handle.0,
                    &mut exit_code,
                )
                .map_err(|_| BrokerError::Unavailable)?;
            }
            if exit_code != 259 {
                break;
            }
            if Instant::now() >= deadline {
                return Err(BrokerError::Busy);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let runtime = active_runtime()?.ok_or(BrokerError::Unavailable)?;
    let (manifest, _) = manifest::read_manifest(&runtime)?;
    let target = runtime.join("carbonpaper.exe");
    let mut locked = locked_file(&target)?;
    verify_file(
        &mut locked,
        manifest
            .files
            .get("carbonpaper.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    std::process::Command::new(target)
        .current_dir(runtime)
        .spawn()
        .map_err(|_| BrokerError::Unavailable)?;
    Ok(())
}

pub fn install(options: InstallOptions) -> Result<PathBuf> {
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied);
    }
    let source = options
        .source
        .canonicalize()
        .map_err(|_| BrokerError::Integrity)?;
    let (manifest, runtime_id) = manifest::read_manifest(&source)?;
    let caller = inspect_process(options.caller_pid)?;
    let sid = caller.principal.sid.clone();
    // Updates may be requested by a previously verified protected release;
    // initial activation must originate in this exact signed release folder.
    if verify_main(inspect_process(options.caller_pid)?).is_err() {
        if !path_eq(&caller.image, &source.join("carbonpaper.exe")) {
            return Err(BrokerError::AccessDenied);
        }
        let mut file = locked_file(&caller.image)?;
        verify_file(
            &mut file,
            manifest
                .files
                .get("carbonpaper.exe")
                .ok_or(BrokerError::Integrity)?,
        )?;
    }
    let root = protected_root()?;
    let state = state_root()?;
    for path in [
        root.parent().unwrap().to_path_buf(),
        root.clone(),
        root.join("System"),
        root.join("Runtime"),
        root.join("Activations"),
    ] {
        ensure_directory(&path, true)?;
    }
    for path in [state.parent().unwrap().to_path_buf(), state.clone()] {
        ensure_directory(&path, false)?;
    }
    let _lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("System").join("setup.lock"))
        .map_err(|_| BrokerError::Busy)?;
    let destination = root.join("Runtime").join(&runtime_id);
    if !destination.exists() {
        let staging = root
            .join("Runtime")
            .join(format!("installing-{}", &random_id()[..16]));
        ensure_directory(&staging, true)?;
        let copied = copy_runtime(&source, &staging, &manifest).and_then(|_| {
            let (_, copied_id) = manifest::read_manifest(&staging)?;
            if copied_id != runtime_id {
                return Err(BrokerError::Integrity);
            }
            fs::rename(&staging, &destination).map_err(|_| BrokerError::Storage)
        });
        if copied.is_err() {
            let _ = remove_runtime_tree(&root, &staging);
        }
        copied?;
    } else {
        assert_protected(&destination)?;
        repair_runtime(&source, &destination, &manifest, &runtime_id)?;
    }
    let mut ledger = Ledger::open(&state.join("keys.db"))?;
    let previous = ledger.owner_registration(&sid)?;
    if let Some((old_id, _)) = &previous {
        let old_root = root.join("Runtime").join(old_id);
        let (old_manifest, _) = manifest::read_manifest(&old_root)?;
        if semver::Version::parse(&manifest.version).map_err(|_| BrokerError::VersionMismatch)?
            < semver::Version::parse(&old_manifest.version)
                .map_err(|_| BrokerError::VersionMismatch)?
        {
            return Err(BrokerError::VersionMismatch);
        }
    }
    let activation_path = root.join("Activations").join(format!("{sid}.json"));
    let old_activation = read_optional(&activation_path, 1024)?;
    let service_path = root.join("System").join("carbonpaper-key-service.exe");
    let setup_path = root.join("System").join("carbonpaper-protected-setup.exe");
    let service_registration = root.join("System").join("runtime.json");
    let old_service_registration = read_optional(&service_registration, 1024)?;
    let replace_service = if let Some(bytes) = &old_service_registration {
        let installed: Activation =
            serde_json::from_slice(bytes).map_err(|_| BrokerError::Integrity)?;
        if !safe_runtime_id(&installed.runtime_id) {
            return Err(BrokerError::Integrity);
        }
        let installed_dir = root.join("Runtime").join(&installed.runtime_id);
        assert_protected(&installed_dir)?;
        let (current, current_id) = manifest::read_manifest(&installed_dir)?;
        if current_id != installed.runtime_id {
            return Err(BrokerError::Integrity);
        }
        // One machine-wide service serves several users. Activating an older
        // portable package must not downgrade another user's service binary.
        semver::Version::parse(&manifest.version).map_err(|_| BrokerError::VersionMismatch)?
            >= semver::Version::parse(&current.version).map_err(|_| BrokerError::VersionMismatch)?
    } else {
        if service_path.exists() {
            // A missing registration can be repaired from the same signed
            // service, but cannot turn a corrupt marker into a downgrade path.
            verify_file(
                &mut locked_file(&service_path)?,
                manifest
                    .files
                    .get("carbonpaper-key-service.exe")
                    .ok_or(BrokerError::Integrity)?,
            )?;
        }
        true
    };
    let old_service = (replace_service && service_path.exists())
        .then(|| root.join("System").join("service.previous"));
    let old_setup = (replace_service && setup_path.exists())
        .then(|| root.join("System").join("setup.previous"));
    if let Some(backup) = &old_service {
        copy_trusted(&service_path, backup)?;
    }
    if let Some(backup) = &old_setup {
        copy_trusted(&setup_path, backup)?;
    }
    let service_existed = service::try_open_service(SERVICE_QUERY_STATUS)?.is_some();
    let result = (|| {
        stop_service()?;
        if replace_service {
            copy_trusted(
                &destination.join("carbonpaper-key-service.exe"),
                &service_path,
            )?;
            copy_trusted(
                &destination.join("carbonpaper-protected-setup.exe"),
                &setup_path,
            )?;
            atomic_write(
                &service_registration,
                &serde_json::to_vec(&Activation {
                    runtime_id: runtime_id.clone(),
                })
                .map_err(|_| BrokerError::Storage)?,
            )?;
        }
        configure_service(&service_path)?;
        ledger.register_owner(&sid, &runtime_id, options.enable)?;
        let activation = serde_json::to_vec(&Activation {
            runtime_id: runtime_id.clone(),
        })
        .map_err(|_| BrokerError::Storage)?;
        atomic_write(&activation_path, &activation)?;
        start_service()?;
        Ok(())
    })();
    if let Err(error) = result {
        // Restore registration only, never rewind the task/key database.
        let _ = stop_service();
        let _ = ledger.restore_registration(&sid, previous);
        let _ = restore_optional(&activation_path, old_activation.as_deref());
        let _ = restore_optional(&service_registration, old_service_registration.as_deref());
        if let Some(backup) = &old_service {
            let _ = copy_trusted(backup, &service_path);
        }
        if let Some(backup) = &old_setup {
            let _ = copy_trusted(backup, &setup_path);
        }
        if service_existed {
            let _ = start_service();
        } else if let Ok(Some(handle)) = service::try_open_service(0x0001_0000) {
            unsafe {
                let _ = DeleteService(handle.0);
            }
        }
        return Err(error);
    }
    for backup in [old_service, old_setup].into_iter().flatten() {
        let _ = fs::remove_file(backup);
    }
    Ok(destination)
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    if !path.try_exists().map_err(|_| BrokerError::Storage)? {
        return Ok(None);
    }
    assert_protected(path)?;
    manifest::read_bounded(path, limit).map(Some)
}

fn restore_optional(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    if let Some(bytes) = bytes {
        atomic_write(path, bytes)
    } else if path.try_exists().map_err(|_| BrokerError::Storage)? {
        fs::remove_file(path).map_err(|_| BrokerError::Storage)
    } else {
        Ok(())
    }
}

fn copy_runtime(source: &Path, destination: &Path, manifest: &ReleaseManifest) -> Result<()> {
    for (relative, expected) in &manifest.files {
        let relative = manifest::safe_relative_path(relative)?;
        let target = destination.join(&relative);
        ensure_relative_parents(destination, &relative)?;
        let mut input = locked_file(&source.join(&relative))?;
        verify_file(&mut input, expected)?;
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| BrokerError::Storage)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| BrokerError::Storage)?;
        let copied = std::io::copy(&mut input.take(2 * 1024 * 1024 * 1024 + 1), &mut output)
            .map_err(|_| BrokerError::Storage)?;
        if copied > 2 * 1024 * 1024 * 1024 {
            return Err(BrokerError::LimitExceeded);
        }
        output.sync_all().map_err(|_| BrokerError::Storage)?;
        drop(output);
        assert_protected(&target)?;
        // Validate the bytes actually installed as well. A hostile remote
        // source must not exploit a verify/rewind/copy gap in its file server.
        verify_file(&mut locked_file(&target)?, expected)?;
    }
    for file in [MANIFEST_NAME, SIGNATURE_NAME] {
        let input = manifest::read_bounded(
            &source.join(file),
            if file == MANIFEST_NAME {
                1024 * 1024
            } else {
                256
            },
        )?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(file))
            .map_err(|_| BrokerError::Storage)?;
        output.write_all(&input).map_err(|_| BrokerError::Storage)?;
        output.sync_all().map_err(|_| BrokerError::Storage)?;
    }
    Ok(())
}

fn ensure_relative_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            current.push(component);
            ensure_directory(&current, true)?;
        }
    }
    Ok(())
}

fn repair_runtime(
    source: &Path,
    destination: &Path,
    release: &ReleaseManifest,
    runtime_id: &str,
) -> Result<()> {
    for (relative, expected) in &release.files {
        let relative = manifest::safe_relative_path(relative)?;
        ensure_relative_parents(destination, &relative)?;
        let target = destination.join(&relative);
        if target.exists() {
            assert_protected(&target)?;
            if locked_file(&target)
                .and_then(|mut file| verify_file(&mut file, expected))
                .is_ok()
            {
                continue;
            }
        }
        let mut input = locked_file(&source.join(&relative))?;
        verify_file(&mut input, expected)?;
        input
            .seek(SeekFrom::Start(0))
            .map_err(|_| BrokerError::Storage)?;
        let temporary = target.with_extension(format!("repair-{}", &random_id()[..16]));
        let result = (|| {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|_| BrokerError::Storage)?;
            let copied = std::io::copy(&mut input.take(2 * 1024 * 1024 * 1024 + 1), &mut output)
                .map_err(|_| BrokerError::Storage)?;
            if copied > 2 * 1024 * 1024 * 1024 {
                return Err(BrokerError::LimitExceeded);
            }
            output.sync_all().map_err(|_| BrokerError::Storage)?;
            drop(output);
            verify_file(&mut locked_file(&temporary)?, expected)?;
            replace(&temporary, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }
    for file in [MANIFEST_NAME, SIGNATURE_NAME] {
        let bytes = manifest::read_bounded(
            &source.join(file),
            if file == MANIFEST_NAME {
                1024 * 1024
            } else {
                256
            },
        )?;
        // Avoid replacing a healthy manifest while another process reads it.
        if manifest::read_bounded(&destination.join(file), 1024 * 1024)
            .ok()
            .as_deref()
            != Some(bytes.as_slice())
        {
            atomic_write(&destination.join(file), &bytes)?;
        }
    }
    let (_, installed_id) = manifest::read_manifest(destination)?;
    if installed_id != runtime_id {
        return Err(BrokerError::Integrity);
    }
    Ok(())
}

pub fn verify_runtime_files(directory: &Path, manifest: &ReleaseManifest) -> Result<()> {
    for (relative, expected) in &manifest.files {
        let path = directory.join(manifest::safe_relative_path(relative)?);
        assert_protected(&path)?;
        verify_file(&mut locked_file(&path)?, expected)?;
    }
    Ok(())
}

fn replace(source: &Path, target: &Path) -> Result<()> {
    let source = wide(source.as_os_str());
    let target = wide(target.as_os_str());
    // SAFETY: both names refer to validated paths under the fixed protected root
    // and remain terminated and alive until the synchronous atomic rename ends.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|_| BrokerError::Storage)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", &random_id()[..16]));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| BrokerError::Storage)?;
    output.write_all(bytes).map_err(|_| BrokerError::Storage)?;
    output.sync_all().map_err(|_| BrokerError::Storage)?;
    drop(output);
    replace(&temporary, path)
}

fn copy_trusted(source: &Path, target: &Path) -> Result<()> {
    assert_protected(source)?;
    let temporary = target.with_extension(format!("tmp-{}", &random_id()[..16]));
    let mut input = locked_file(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| BrokerError::Storage)?;
    std::io::copy(&mut input, &mut output).map_err(|_| BrokerError::Storage)?;
    output.sync_all().map_err(|_| BrokerError::Storage)?;
    drop(output);
    replace(&temporary, target)
}

fn configure_service(path: &Path) -> Result<()> {
    let name = wide(SERVICE_NAME.as_ref());
    let display = wide("CarbonPaper Background Processing".as_ref());
    let quoted = wide(format!("\"{}\"", path.display()).as_ref());
    // SAFETY: all SCM strings are fixed or derived from Known Folders, never a
    // request-supplied command. A null account explicitly selects LocalSystem.
    unsafe {
        let manager = ServiceHandle(
            OpenSCManagerW(
                PCWSTR::null(),
                PCWSTR::null(),
                SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
            )
            .map_err(|_| BrokerError::AccessDenied)?,
        );
        match OpenServiceW(manager.0, PCWSTR(name.as_ptr()), SERVICE_CHANGE_CONFIG) {
            Ok(handle) => {
                let handle = ServiceHandle(handle);
                ChangeServiceConfigW(
                    handle.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(quoted.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR(wide("LocalSystem".as_ref()).as_ptr()),
                    PCWSTR::null(),
                    PCWSTR(display.as_ptr()),
                )
                .map_err(|_| BrokerError::AccessDenied)?;
            }
            Err(error) if error.code() == windows::core::HRESULT::from_win32(1060) => {
                let _handle = ServiceHandle(
                    CreateServiceW(
                        manager.0,
                        PCWSTR(name.as_ptr()),
                        PCWSTR(display.as_ptr()),
                        SERVICE_ALL_ACCESS,
                        SERVICE_WIN32_OWN_PROCESS,
                        SERVICE_AUTO_START,
                        SERVICE_ERROR_NORMAL,
                        PCWSTR(quoted.as_ptr()),
                        PCWSTR::null(),
                        None,
                        PCWSTR::null(),
                        PCWSTR::null(),
                        PCWSTR::null(),
                    )
                    .map_err(|_| BrokerError::AccessDenied)?,
                );
            }
            Err(_) => return Err(BrokerError::AccessDenied),
        }
    }
    Ok(())
}

pub fn stop_service() -> Result<()> {
    let Some(handle) = service::try_open_service(SERVICE_STOP | SERVICE_QUERY_STATUS)? else {
        return Ok(());
    };
    if service::query_status(&handle)?.dwCurrentState == SERVICE_STOPPED {
        return Ok(());
    }
    // SAFETY: SCM handle and status output are live and valid.
    unsafe {
        let mut status = SERVICE_STATUS::default();
        let _ = ControlService(handle.0, SERVICE_CONTROL_STOP, &mut status);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if service::query_status(&handle)?.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(BrokerError::Busy)
}

fn start_service() -> Result<()> {
    let handle = service::open_service(SERVICE_START | SERVICE_QUERY_STATUS)?;
    if service::query_status(&handle)?.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }
    unsafe {
        StartServiceW(handle.0, None).map_err(|_| BrokerError::Unavailable)?;
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = service::query_status(&handle)?;
        if status.dwCurrentState == SERVICE_RUNNING {
            return Ok(());
        }
        if status.dwCurrentState == SERVICE_STOPPED {
            return Err(BrokerError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(BrokerError::Unavailable)
}

pub fn uninstall(caller_pid: u32) -> Result<()> {
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied);
    }
    let caller = verify_main(inspect_process(caller_pid)?)?;
    let root = protected_root()?;
    let state = state_root()?;
    assert_protected(&root)?;
    assert_protected(&state)?;
    let _lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("System/setup.lock"))
        .map_err(|_| BrokerError::Busy)?;
    stop_service()?;
    let mut ledger = Ledger::open(&state.join("keys.db"))?;
    ledger.unregister_owner(&caller.principal.sid)?;
    let activation = root
        .join("Activations")
        .join(format!("{}.json", caller.principal.sid));
    if activation.exists() {
        fs::remove_file(activation).map_err(|_| BrokerError::Storage)?;
    }
    if ledger.registered_owners()? == 0 {
        if let Some(handle) = service::try_open_service(0x0001_0000)? {
            unsafe {
                DeleteService(handle.0).map_err(|_| BrokerError::AccessDenied)?;
            }
        }
    } else {
        start_service()?;
    }
    // Runtime files may still be mapped by this user's application. They are
    // retained for safe removal after exit; no user archive paths are touched.
    Ok(())
}

fn remove_runtime_tree(root: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().ok_or(BrokerError::Integrity)?;
    if !path_eq(parent, &root.join("Runtime"))
        || target
            .file_name()
            .is_none_or(|s| !safe_runtime_id(&s.to_string_lossy()))
    {
        return Err(BrokerError::Integrity);
    }
    assert_protected(target)?;
    fs::remove_dir_all(target).map_err(|_| BrokerError::Storage)
}
