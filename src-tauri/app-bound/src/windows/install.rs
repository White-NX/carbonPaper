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
    core::{PCWSTR, PWSTR},
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
    // SAFETY: this only queries the caller's effective token and takes no pointers.
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
            // SAFETY: ProcessIdentity owns this query-capable process handle;
            // the initialized exit-code output stays live during the call.
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
    // SAFETY: this only queries the caller's effective token and takes no pointers.
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
        #[cfg(feature = "development-runtime")]
        crate::development::verify_bootstrap_caller(&caller, &source, &manifest)?;
        #[cfg(not(feature = "development-runtime"))]
        {
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
    }
    let root = protected_root()?;
    let state = state_root()?;
    for path in [
        root.parent().unwrap().to_path_buf(),
        root.clone(),
        root.join("System"),
        root.join("Runtime"),
        root.join("Activations"),
        root.join("Detached"),
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
    super::revocation::provision(&state, &sid)?;
    super::revocation::drain(&state, &sid, &mut ledger)?;
    super::activation::reconcile(&root, &ledger)?;
    let previous = ledger.owner_registration(&sid)?;
    if let Some((old_id, _)) = &previous {
        let old_version = crate::repair::runtime_version(old_id)?;
        if semver::Version::parse(&manifest.version)
            .map_err(|_| BrokerError::VersionMismatch)?
            .cmp_precedence(&old_version)
            .is_lt()
        {
            return Err(BrokerError::VersionMismatch);
        }
    }
    let activation_path = root.join("Activations").join(format!("{sid}.json"));
    let old_activation = read_optional(&activation_path, 1024)?;
    let detached_path = root.join("Detached").join(format!("{sid}.json"));
    let old_detached = read_optional(&detached_path, 1024)?;
    let service_path = root.join("System").join("carbonpaper-key-service.exe");
    let setup_path = root.join("System").join("carbonpaper-protected-setup.exe");
    let service_registration = root.join("System").join("runtime.json");
    let old_service_registration = read_optional(&service_registration, 1024)?;
    let previous_service_id = if let Some(id) = ledger.service_runtime()? {
        crate::repair::runtime_version(&id)?;
        Some(id)
    } else if let Some(bytes) = &old_service_registration {
        let installed: Activation =
            serde_json::from_slice(bytes).map_err(|_| BrokerError::Integrity)?;
        if !safe_runtime_id(&installed.runtime_id) {
            return Err(BrokerError::Integrity);
        }
        let installed_dir = root.join("Runtime").join(&installed.runtime_id);
        assert_protected(&installed_dir)?;
        let (_, current_id) = manifest::read_manifest(&installed_dir)?;
        if current_id != installed.runtime_id {
            return Err(BrokerError::Integrity);
        }
        Some(current_id)
    } else {
        None
    };
    let previous_service_id =
        crate::repair::newest_runtime(previous_service_id, ledger.required_service_runtime()?)?;
    let replace_service = if let Some(installed_id) = &previous_service_id {
        // The service-owned checkpoint survives damage to the old manifest.
        semver::Version::parse(&manifest.version)
            .map_err(|_| BrokerError::VersionMismatch)?
            .cmp_precedence(&crate::repair::runtime_version(installed_id)?)
            .is_ge()
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
    let system_runtime_id = if replace_service {
        runtime_id.clone()
    } else {
        previous_service_id.clone().ok_or(BrokerError::Integrity)?
    };
    let mut maintenance = ServiceMaintenance::prepare()?;
    super::activation::begin(
        &root,
        &super::activation::PendingActivation {
            sid: sid.clone(),
            runtime_id: runtime_id.clone(),
            system_runtime_id: system_runtime_id.clone(),
        },
    )?;
    let result = (|| {
        maintenance.pause()?;
        ledger.enable_reclamation()?;
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
        if detached_path.exists() {
            fs::remove_file(&detached_path).map_err(|_| BrokerError::Storage)?;
        }
        maintenance.start()?;
        ledger.remember_service_runtime(&system_runtime_id)?;
        super::repair::write_checkpoint(&root, &ledger)?;
        Ok(())
    })();
    if let Err(error) = result {
        // Restore registration only, never rewind the task/key database.
        let mut restored = maintenance.pause().is_ok();
        restored &= ledger.restore_registration(&sid, previous).is_ok();
        restored &= restore_optional(&activation_path, old_activation.as_deref()).is_ok();
        restored &= restore_optional(&detached_path, old_detached.as_deref()).is_ok();
        if ledger.requires_current_service().unwrap_or(true)
            || ledger
                .service_runtime()
                .map(|id| id != previous_service_id)
                .unwrap_or(true)
        {
            // Another owner may have compacted revocations while the new
            // service was starting. Never restore a service that cannot read
            // that committed authority. Reconcile the installed signed service
            // and its companion while projecting the restored registrations.
            if restored {
                restored = super::activation::reconcile(&root, &ledger).is_ok();
            }
        } else if restored {
            restored &=
                restore_optional(&service_registration, old_service_registration.as_deref())
                    .is_ok();
            if let Some(backup) = &old_service {
                restored &= copy_trusted(backup, &service_path).is_ok();
            }
            if let Some(backup) = &old_setup {
                restored &= copy_trusted(backup, &setup_path).is_ok();
            }
        }
        if restored {
            let _ = super::activation::finish(&root);
        }
        if service_existed {
            let _ = maintenance.restart_after_failure();
        } else if let Ok(Some(handle)) = service::try_open_service(0x0001_0000) {
            // SAFETY: the newly created service's owned SCM handle has DELETE
            // access and remains live while rollback marks it for removal.
            unsafe {
                let _ = DeleteService(handle.0);
            }
        }
        return Err(error);
    }
    super::activation::finish(&root)?;
    maintenance.commit();
    drop(maintenance);
    for backup in [old_service, old_setup].into_iter().flatten() {
        let _ = fs::remove_file(backup);
    }
    if let Err(error) = super::collection::schedule_after(&root, options.caller_pid) {
        super::diagnostics::event(
            "warn",
            "runtime_collection_deferred",
            "install",
            Some(error),
        );
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

pub(super) fn repair_runtime(
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

pub(super) fn copy_trusted(source: &Path, target: &Path) -> Result<()> {
    assert_protected(source)?;
    copy_locked(&mut locked_file(source)?, target)
}

pub(super) fn copy_verified(source: &Path, target: &Path, expected: &str) -> Result<()> {
    let mut input = locked_file(source)?;
    verify_file(&mut input, expected)?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| BrokerError::Storage)?;
    copy_locked(&mut input, target)?;
    assert_protected(target)?;
    verify_file(&mut locked_file(target)?, expected)
}

fn copy_locked(input: &mut fs::File, target: &Path) -> Result<()> {
    let temporary = target.with_extension(format!("tmp-{}", &random_id()[..16]));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| BrokerError::Storage)?;
    std::io::copy(input, &mut output).map_err(|_| BrokerError::Storage)?;
    output.sync_all().map_err(|_| BrokerError::Storage)?;
    drop(output);
    replace(&temporary, target)
}

pub(super) fn configure_service(path: &Path) -> Result<()> {
    // The installation lock does not serialize SCM starts. Keep even a newly
    // created service disabled until its files and registration are committed.
    configure_named_service_with_start_type(
        path,
        SERVICE_NAME,
        SERVICE_DISPLAY_NAME,
        SERVICE_DISABLED,
    )
}

#[cfg(test)]
fn configure_named_service(path: &Path, service_name: &str, display_name: &str) -> Result<()> {
    configure_named_service_with_start_type(path, service_name, display_name, SERVICE_AUTO_START)
}

fn configure_named_service_with_start_type(
    path: &Path,
    service_name: &str,
    display_name: &str,
    start_type: SERVICE_START_TYPE,
) -> Result<()> {
    let name = wide(service_name.as_ref());
    let display = wide(display_name.as_ref());
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
        // SC_ACTION_RESTART also requires SERVICE_START on this handle, even
        // though configuration itself does not start the service. Repair must
        // request it explicitly; only the creation branch has ALL_ACCESS.
        let service = match OpenServiceW(
            manager.0,
            PCWSTR(name.as_ptr()),
            SERVICE_CHANGE_CONFIG | SERVICE_START | 0x0004_0000,
        ) {
            Ok(handle) => {
                let handle = ServiceHandle(handle);
                ChangeServiceConfigW(
                    handle.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    start_type,
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
                handle
            }
            Err(error) if error.code() == windows::core::HRESULT::from_win32(1060) => {
                ServiceHandle(
                    CreateServiceW(
                        manager.0,
                        PCWSTR(name.as_ptr()),
                        PCWSTR(display.as_ptr()),
                        SERVICE_ALL_ACCESS,
                        SERVICE_WIN32_OWN_PROCESS,
                        start_type,
                        SERVICE_ERROR_NORMAL,
                        PCWSTR(quoted.as_ptr()),
                        PCWSTR::null(),
                        None,
                        PCWSTR::null(),
                        PCWSTR::null(),
                        PCWSTR::null(),
                    )
                    .map_err(|_| BrokerError::AccessDenied)?,
                )
            }
            Err(_) => return Err(BrokerError::AccessDenied),
        };
        configure_failure_actions(&service)?;
        configure_start_permissions(&service)?;
    }
    Ok(())
}

fn configure_start_permissions(handle: &ServiceHandle) -> Result<()> {
    use windows::Win32::{
        Foundation::{LocalFree, HLOCAL},
        Security::{
            self, Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            PSECURITY_DESCRIPTOR,
        },
    };
    // Ordinary clients can start/query this fixed service, but cannot stop it,
    // replace its command line or change the service's access policy.
    let sddl = wide(std::ffi::OsStr::new(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0014;;;AU)",
    ));
    // SAFETY: SCM copies the allocated descriptor into this owned service;
    // LocalFree releases it only after that synchronous call returns.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let result =
            SetServiceObjectSecurity(handle.0, Security::DACL_SECURITY_INFORMATION, descriptor)
                .map_err(|_| BrokerError::AccessDenied);
        LocalFree(HLOCAL(descriptor.0));
        result
    }
}

fn configure_failure_actions(handle: &ServiceHandle) -> Result<()> {
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 30_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 60_000,
        },
    ];
    let config = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 24 * 60 * 60,
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    // SAFETY: SCM copies the configuration synchronously. The action array and
    // its containing structure remain alive for the duration of this call.
    unsafe {
        ChangeServiceConfig2W(
            handle.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some((&config as *const SERVICE_FAILURE_ACTIONSW).cast()),
        )
        .map_err(|_| BrokerError::AccessDenied)
    }
}

const MAINTENANCE_ACCESS: u32 = SERVICE_QUERY_CONFIG
    | SERVICE_CHANGE_CONFIG
    | SERVICE_START
    | SERVICE_STOP
    | SERVICE_QUERY_STATUS;

/// SCM checks the disabled start type even on handles opened before maintenance.
/// The setup lock remains the authority for coordinating file/ledger changes.
pub(super) struct ServiceMaintenance {
    handle: Option<ServiceHandle>,
    original_start_type: Option<SERVICE_START_TYPE>,
}

impl ServiceMaintenance {
    pub(super) fn prepare() -> Result<Self> {
        let Some(handle) = service::try_open_service(MAINTENANCE_ACCESS)? else {
            return Ok(Self {
                handle: None,
                original_start_type: None,
            });
        };
        Self::from_handle(handle)
    }

    fn from_handle(handle: ServiceHandle) -> Result<Self> {
        let original_start_type = service_start_type(&handle)?;
        Ok(Self {
            handle: Some(handle),
            original_start_type: Some(original_start_type),
        })
    }

    pub(super) fn stop() -> Result<Self> {
        let mut guard = Self::prepare()?;
        guard.pause()?;
        Ok(guard)
    }

    fn open_if_present(&mut self) -> Result<()> {
        if self.handle.is_none() {
            self.handle = service::try_open_service(MAINTENANCE_ACCESS)?;
        }
        Ok(())
    }

    pub(super) fn pause(&mut self) -> Result<()> {
        self.open_if_present()?;
        if let Some(handle) = &self.handle {
            set_service_start_type(handle, SERVICE_DISABLED)?;
            stop_named_service(handle)?;
        }
        Ok(())
    }

    pub(super) fn start(&mut self) -> Result<()> {
        self.open_if_present()?;
        let handle = self.handle.as_ref().ok_or(BrokerError::Unavailable)?;
        set_service_start_type(handle, SERVICE_AUTO_START)?;
        start_named_service(handle)
    }

    pub(super) fn restart_after_failure(&mut self) -> Result<()> {
        self.open_if_present()?;
        let Some(handle) = &self.handle else {
            return Ok(());
        };
        let start_type = self.original_start_type.unwrap_or(SERVICE_AUTO_START);
        set_service_start_type(handle, start_type)?;
        if start_type != SERVICE_DISABLED {
            start_named_service(handle)?;
        }
        Ok(())
    }

    pub(super) fn commit(&mut self) {
        self.original_start_type = None;
    }
}

impl Drop for ServiceMaintenance {
    fn drop(&mut self) {
        if let (Some(handle), Some(start_type)) = (&self.handle, self.original_start_type) {
            if let Err(error) = set_service_start_type(handle, start_type) {
                super::diagnostics::event(
                    "warn",
                    "service_start_config_restore_failed",
                    "maintenance",
                    Some(error),
                );
            }
        }
    }
}

fn service_start_type(handle: &ServiceHandle) -> Result<SERVICE_START_TYPE> {
    let mut needed = 0;
    // SAFETY: the first query obtains the size only. The second uses aligned,
    // initialized storage that remains live while reading the fixed header.
    unsafe {
        let _ = QueryServiceConfigW(handle.0, None, 0, &mut needed);
        if needed < std::mem::size_of::<QUERY_SERVICE_CONFIGW>() as u32 || needed > 64 * 1024 {
            return Err(BrokerError::Storage);
        }
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        QueryServiceConfigW(
            handle.0,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        Ok((*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>()).dwStartType)
    }
}

fn set_service_start_type(handle: &ServiceHandle, start_type: SERVICE_START_TYPE) -> Result<()> {
    // SAFETY: SCM copies this fixed start type; null optional strings preserve
    // the signed image, account, arguments, dependencies and display name.
    unsafe {
        ChangeServiceConfigW(
            handle.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            start_type,
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
        .map_err(|_| BrokerError::AccessDenied)
    }
}

pub fn stop_service() -> Result<()> {
    let Some(handle) = service::try_open_service(SERVICE_STOP | SERVICE_QUERY_STATUS)? else {
        return Ok(());
    };
    stop_named_service(&handle)
}

fn stop_named_service(handle: &ServiceHandle) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let state = service::query_status(handle)?.dwCurrentState;
        if state == SERVICE_STOPPED {
            return Ok(());
        }
        if state != SERVICE_START_PENDING && state != SERVICE_STOP_PENDING {
            // SAFETY: the owned SCM handle has STOP access and the output is
            // initialized. Retry after a start/control transition settles.
            unsafe {
                let mut status = SERVICE_STATUS::default();
                let _ = ControlService(handle.0, SERVICE_CONTROL_STOP, &mut status);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(BrokerError::Busy)
}

fn start_named_service(handle: &ServiceHandle) -> Result<()> {
    if service::query_status(handle)?.dwCurrentState == SERVICE_RUNNING {
        return Ok(());
    }
    // SAFETY: the caller retains an SCM handle with START access. No argument
    // strings or borrowed buffers are passed to the synchronous start request.
    unsafe {
        if let Err(error) = StartServiceW(handle.0, None) {
            if error.code() != windows::core::HRESULT::from_win32(1056) {
                return Err(BrokerError::Unavailable);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = service::query_status(handle)?;
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
    uninstall_owner(caller_pid, false)
}

pub fn uninstall_owner(caller_pid: u32, remove_runtime: bool) -> Result<()> {
    // SAFETY: this only queries the caller's effective token and takes no pointers.
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied);
    }
    let caller = verify_uninstall_caller(caller_pid)?;
    let root = protected_root()?;
    let state = state_root()?;
    if !root.try_exists().map_err(|_| BrokerError::Storage)?
        || !state
            .join("keys.db")
            .try_exists()
            .map_err(|_| BrokerError::Storage)?
    {
        return Ok(());
    }
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
    if !state
        .join("keys.db")
        .try_exists()
        .map_err(|_| BrokerError::Storage)?
    {
        return Ok(());
    }
    let mut ledger = Ledger::open_existing(&state.join("keys.db"))?;
    let activation = root
        .join("Activations")
        .join(format!("{}.json", caller.principal.sid));
    let detached_root = root.join("Detached");
    let detached = detached_root.join(format!("{}.json", caller.principal.sid));
    let registered = ledger.registered_runtime(&caller.principal.sid)?;
    let pending = super::activation::read_pending(&root)?;
    if registered.is_none()
        && !activation.try_exists().map_err(|_| BrokerError::Storage)?
        && (!remove_runtime || !detached.try_exists().map_err(|_| BrokerError::Storage)?)
        && pending
            .as_ref()
            .is_none_or(|pending| pending.sid != caller.principal.sid)
    {
        // Other users may own the machine-wide service. Repeated removal of
        // this user's package does not stop it or create new empty authority.
        if remove_runtime && ledger.registered_owners()? == 0 {
            if let Err(error) = super::collection::schedule_after(&root, caller_pid) {
                super::diagnostics::event(
                    "warn",
                    "runtime_collection_deferred",
                    "uninstall",
                    Some(error),
                );
            }
        }
        return Ok(());
    }
    let mut maintenance = ServiceMaintenance::stop()?;
    // Recover a prior interrupted registration before replacing its fence with
    // removal. The installed service may already differ from its old marker.
    super::activation::reconcile(&root, &ledger)?;
    ledger.enable_reclamation()?;
    let service_marker = root.join("System/runtime.json");
    assert_protected(&service_marker)?;
    let system: Activation =
        serde_json::from_slice(&manifest::read_bounded(&service_marker, 1024)?)
            .map_err(|_| BrokerError::Integrity)?;
    let runtime_id = registered
        .clone()
        .unwrap_or_else(|| caller.principal.runtime_id.clone());
    super::activation::begin(
        &root,
        &super::activation::PendingActivation {
            sid: caller.principal.sid.clone(),
            runtime_id,
            system_runtime_id: system.runtime_id,
        },
    )?;
    ensure_directory(&detached_root, true)?;
    if remove_runtime {
        if detached.exists() {
            fs::remove_file(&detached).map_err(|_| BrokerError::Storage)?;
        }
    } else if let Some(runtime_id) = registered {
        // Component removal leaves the application's current binaries and
        // native-messaging path available. Keep at most one detached copy per SID.
        atomic_write(
            &detached,
            &serde_json::to_vec(&Activation { runtime_id }).map_err(|_| BrokerError::Storage)?,
        )?;
    }
    ledger.unregister_owner(&caller.principal.sid)?;
    if activation.exists() {
        fs::remove_file(activation).map_err(|_| BrokerError::Storage)?;
    }
    super::activation::finish(&root)?;
    if let Err(error) = super::revocation::remove(&state, &caller.principal.sid) {
        super::diagnostics::event(
            "warn",
            "revocation_cleanup_deferred",
            "uninstall",
            Some(error),
        );
    }
    if ledger.registered_owners()? == 0 {
        if let Some(handle) = service::try_open_service(0x0001_0000)? {
            // SAFETY: this stopped service's owned SCM handle has DELETE
            // access and is retained until SCM accepts the removal request.
            unsafe {
                DeleteService(handle.0).map_err(|_| BrokerError::AccessDenied)?;
            }
        }
    } else {
        maintenance.start()?;
    }
    maintenance.commit();
    drop(maintenance);
    // Cleanup uses a verified private helper in System and waits for the caller
    // to exit, so current executable locks cannot strand every old release.
    if let Err(error) = super::collection::schedule_after(&root, caller_pid) {
        super::diagnostics::event(
            "warn",
            "runtime_collection_deferred",
            "uninstall",
            Some(error),
        );
    }
    Ok(())
}

fn verify_uninstall_caller(pid: u32) -> Result<VerifiedCaller> {
    if let Ok(caller) = verify_main(inspect_process(pid)?) {
        return Ok(caller);
    }
    #[cfg(feature = "development-runtime")]
    return Err(BrokerError::AccessDenied);
    #[cfg(not(feature = "development-runtime"))]
    {
        let mut process = inspect_process(pid)?;
        if process
            .image
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.eq_ignore_ascii_case("carbonpaper.exe"))
        {
            return Err(BrokerError::AccessDenied);
        }
        let directory = process.image.parent().ok_or(BrokerError::Integrity)?;
        let (release, id) = manifest::read_manifest(directory)?;
        let mut image = locked_file(&process.image)?;
        verify_file(
            &mut image,
            release
                .files
                .get("carbonpaper.exe")
                .ok_or(BrokerError::Integrity)?,
        )?;
        process.principal.runtime_id = id;
        Ok(VerifiedCaller::new(process, image))
    }
}

pub fn collect_after(pid: u32) -> Result<()> {
    super::collection::run_after(pid)
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

#[cfg(test)]
mod tests;
