//! An unelevated coordinator owns restart; its UAC child only installs signed
//! files into an existing administrator-owned ordinary installation.
use super::*;
use std::os::windows::io::FromRawHandle;
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{
        Foundation::{LocalFree, ERROR_CANCELLED, ERROR_NO_DATA, HLOCAL},
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::{
            Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
            Pipes::*,
        },
        UI::{
            Shell::{
                ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                SHELLEXECUTEINFOW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
};

const PREPARED: u8 = 1;
const COMMIT: u8 = 2;
const INSTALLED: u8 = 3;
const FINISH: u8 = 4;
const ABORT: u8 = 5;
const ROLLED_BACK: u8 = 6;
const FAILED: u8 = 7;
const PIPE_ACCESS: u32 = super::super::transport::PIPE_CLIENT_ACCESS;

/// Remote servers cannot enforce the local file and directory locks used for
/// the UAC bootstrap or the privileged destination.
pub fn check_local_path(path: &Path) -> Result<()> {
    use std::path::Prefix;
    if !path.is_absolute()
        || !matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err(BrokerError::AccessDenied);
    }
    let root = path.ancestors().last().ok_or(BrokerError::Integrity)?;
    let root = identity::wide(root.as_os_str());
    // SAFETY: root is a terminated absolute drive name, queried synchronously.
    match unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } {
        2 | 3 => Ok(()), // DRIVE_REMOVABLE or DRIVE_FIXED
        _ => Err(BrokerError::AccessDenied),
    }
}

fn pipe_name(id: &str) -> Result<String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(BrokerError::InvalidRequest);
    }
    Ok(format!(r"\\.\pipe\CarbonPaper.Update.{id}"))
}

fn create_pipe(name: &str) -> Result<File> {
    // Elevated clients receive no FILE_CREATE_PIPE_INSTANCE permission. Only
    // one server instance is allowed, even if its owner's DACL is changed.
    let sddl = format!(
        "D:P(A;;FA;;;{})(A;;0x00120183;;;BA)(A;;0x00120183;;;SY)",
        identity::current_sid()?
    );
    let security_text = identity::wide(sddl.as_ref());
    let name = identity::wide(name.as_ref());
    // SAFETY: the descriptor stays live until creation copies it. The handle
    // is transferred once to File, and no asynchronous I/O is submitted.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(security_text.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let handle = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1024,
            1024,
            1000,
            Some(&attributes),
        );
        LocalFree(HLOCAL(descriptor.0));
        if handle.is_invalid() {
            return Err(BrokerError::Unavailable);
        }
        Ok(File::from_raw_handle(handle.0))
    }
}

fn open_pipe(name: &str, deadline: Instant) -> Result<File> {
    loop {
        match OpenOptions::new()
            .access_mode(PIPE_ACCESS)
            // The ordinary server must never impersonate an administrator.
            .custom_flags((SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION).0)
            .open(name)
        {
            Ok(file) => {
                // SAFETY: file owns a synchronous pipe handle.
                unsafe {
                    SetNamedPipeHandleState(
                        HANDLE(file.as_raw_handle()),
                        Some(&PIPE_NOWAIT),
                        None,
                        None,
                    )
                }
                .map_err(|_| BrokerError::Unavailable)?;
                return Ok(file);
            }
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 231)) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(BrokerError::Unavailable),
        }
    }
}

fn connect(pipe: &File, process: HANDLE, deadline: Instant) -> Result<()> {
    // SAFETY: the caller retains the process and pipe handles throughout.
    let expected = unsafe { GetProcessId(process) };
    if expected == 0 {
        return Err(BrokerError::AccessDenied);
    }
    loop {
        unsafe {
            let _ = ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None);
            let mut pid = 0;
            if GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid).is_ok()
                && pid != 0
            {
                return if pid == expected {
                    Ok(())
                } else {
                    Err(BrokerError::AccessDenied)
                };
            }
            if WaitForSingleObject(process, 0) == WAIT_OBJECT_0 {
                return Err(BrokerError::Unavailable);
            }
        }
        if Instant::now() >= deadline {
            return Err(BrokerError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn receive(pipe: &mut File, deadline: Instant) -> Result<u8> {
    let mut byte = [0];
    loop {
        if Instant::now() >= deadline {
            return Err(BrokerError::Unavailable);
        }
        let mut count = 0;
        // SAFETY: this is synchronous I/O into a one-byte exclusive buffer.
        match unsafe {
            ReadFile(
                HANDLE(pipe.as_raw_handle()),
                Some(&mut byte),
                Some(&mut count),
                None,
            )
        } {
            Ok(()) if count == 1 => return Ok(byte[0]),
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_DATA.0) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => return Err(BrokerError::Unavailable),
        }
    }
}

fn send(pipe: &mut File, byte: u8) -> Result<()> {
    pipe.write_all(&[byte])
        .map_err(|_| BrokerError::Unavailable)
}

fn quote(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        result.push_str(&"\\".repeat(if character == '"' {
            slashes * 2 + 1
        } else {
            slashes
        }));
        result.push(character);
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}

fn shell_execute(
    executable: &Path,
    directory: &Path,
    verb: &str,
    parameters: &[String],
) -> std::result::Result<OwnedHandle, windows::core::Error> {
    // Shell verbs can use COM extensions. Initialize this dedicated helper
    // thread before calling ShellExecuteEx, and balance even an S_FALSE result.
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
    let _apartment = Apartment;
    let executable = identity::wide(executable.as_os_str());
    let verb = identity::wide(verb.as_ref());
    let directory = identity::wide(directory.as_os_str());
    let parameters = parameters
        .iter()
        .map(|argument| quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters = identity::wide(parameters.as_ref());
    let mut information = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        lpDirectory: PCWSTR(directory.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: all terminated strings remain live. The caller retains the
    // verified executable and every execution-path component through launch.
    unsafe { ShellExecuteExW(&mut information) }?;
    if information.hProcess.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    Ok(OwnedHandle(information.hProcess))
}

fn request_uac(
    source: &Path,
    caller: u32,
    release: &Release,
    id: &str,
) -> std::result::Result<OwnedHandle, windows::core::Error> {
    let parameters = [
        "--apply-elevated-update".to_string(),
        "--source".into(),
        source.to_string_lossy().into_owned(),
        "--caller-pid".into(),
        caller.to_string(),
        "--coordinator-pid".into(),
        std::process::id().to_string(),
        "--version".into(),
        release.manifest.version.clone(),
        "--pipe-id".into(),
        id.into(),
        "--manifest-hash".into(),
        hex::encode(Sha256::digest(&release.bytes)),
    ];
    shell_execute(&source.join(HELPER_NAME), source, "runas", &parameters)
}

fn verify_destination(destination: &Path) -> Result<DirectoryLocks> {
    check_local_path(destination)?;
    // Parent components may be user-owned. Freeze them against replacement and
    // reparse metadata changes; only protected directories below this point
    // are permitted to share writes for NT replacement operations.
    let parents =
        DirectoryLocks::for_execution(destination.parent().ok_or(BrokerError::Integrity)?)?;
    let protected_root = identity::protected_root()?;
    if destination
        .ancestors()
        .any(|path| identity::path_eq(path, &protected_root))
    {
        return Err(BrokerError::AccessDenied);
    }
    identity::assert_ordinary_installation(destination)?;
    Ok(parents)
}

fn start_application(destination: &Path, release: &Release) -> Result<()> {
    if elevation_status()? {
        return Err(BrokerError::AccessDenied);
    }
    let mut paths = DirectoryLocks::for_execution(destination)?;
    let mut files = Vec::new();
    for (name, hash) in &release.manifest.files {
        let path = destination.join(manifest::safe_relative_path(name)?);
        paths.pin_with_sharing(path.parent().unwrap(), FILE_SHARE_READ.0)?;
        identity::assert_protected(&path)?;
        let mut file = open_file(
            &path,
            GENERIC_READ.0,
            FILE_SHARE_READ.0 | FILE_SHARE_DELETE.0,
        )?;
        identity::verify_file(&mut file, hash)?;
        files.push(file);
    }
    std::process::Command::new(destination.join("carbonpaper.exe"))
        .current_dir(destination)
        .spawn()
        .map_err(|_| BrokerError::Unavailable)?;
    Ok(())
}

pub(super) fn coordinate(
    source: &Path,
    destination: &Path,
    current: &Release,
    release: &Release,
    caller: u32,
    parent: HANDLE,
) -> Result<()> {
    check_local_path(source)?;
    // SAFETY: the original parent handle is retained by the caller.
    if unsafe { WaitForSingleObject(parent, 0) } == WAIT_OBJECT_0 {
        return Err(BrokerError::AccessDenied);
    }
    let _parents = verify_destination(destination)?;
    let mut bootstrap = locked_file(&source.join(HELPER_NAME))?;
    identity::verify_file(
        &mut bootstrap,
        release
            .manifest
            .files
            .get(HELPER_NAME)
            .ok_or(BrokerError::Integrity)?,
    )?;
    crate::privileged::verify_loader_policy(&mut bootstrap)?;
    let id = format!("{:032x}", rand::random::<u128>());
    let mut pipe = create_pipe(&pipe_name(&id)?)?;
    // UAC waits for the user's choice without an application timeout killing
    // this process and releasing the verified executable's locks prematurely.
    writeln!(std::io::stdout(), "{ELEVATING}").map_err(|_| BrokerError::Unavailable)?;
    std::io::stdout()
        .flush()
        .map_err(|_| BrokerError::Unavailable)?;
    let elevated = match request_uac(source, caller, release, &id) {
        Ok(process) => process,
        Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
            writeln!(std::io::stdout(), "{ELEVATION_CANCELLED}")
                .map_err(|_| BrokerError::Unavailable)?;
            std::io::stdout()
                .flush()
                .map_err(|_| BrokerError::Unavailable)?;
            return Err(BrokerError::Unavailable);
        }
        Err(_) => return Err(BrokerError::Unavailable),
    };
    connect(&pipe, elevated.0, Instant::now() + Duration::from_secs(30))?;
    if receive(&mut pipe, Instant::now() + Duration::from_secs(120))? != PREPARED {
        return Err(BrokerError::Integrity);
    }
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
        let _ = send(&mut pipe, ABORT);
        return Err(BrokerError::InvalidRequest);
    }
    send(&mut pipe, COMMIT)?;
    let status = receive(&mut pipe, Instant::now() + Duration::from_secs(180));
    // The elevated child also waits on this exact parent, but the coordinator
    // must not restart an existing application on an early preparation failure.
    if unsafe { WaitForSingleObject(parent, 60_000) } != WAIT_OBJECT_0 {
        return Err(BrokerError::Busy);
    }
    match status {
        Ok(INSTALLED) => match start_application(destination, release) {
            Ok(()) => {
                let _ = send(&mut pipe, FINISH);
                Ok(())
            }
            Err(error) => {
                send(&mut pipe, ABORT)?;
                if receive(&mut pipe, Instant::now() + Duration::from_secs(30))? == ROLLED_BACK {
                    let _ = start_application(destination, current);
                }
                Err(error)
            }
        },
        _ => {
            // A lost reply may follow a completed commit. Verify one complete
            // signed release before restarting; never launch mixed file sets.
            if start_application(destination, release).is_err() {
                let _ = start_application(destination, current);
            }
            Err(BrokerError::Unavailable)
        }
    }
}

pub fn run_elevated(args: &[String]) -> Result<()> {
    if cfg!(feature = "development-runtime") || !elevation_status()? {
        return Err(BrokerError::AccessDenied);
    }
    if args.len() != 12
        || args[0] != "--source"
        || args[2] != "--caller-pid"
        || args[4] != "--coordinator-pid"
        || args[6] != "--version"
        || args[8] != "--pipe-id"
        || args[10] != "--manifest-hash"
    {
        return Err(BrokerError::InvalidRequest);
    }
    let caller_pid = args[3]
        .parse::<u32>()
        .map_err(|_| BrokerError::InvalidRequest)?;
    let coordinator_pid = args[5]
        .parse::<u32>()
        .map_err(|_| BrokerError::InvalidRequest)?;
    let source = PathBuf::from(&args[1]);
    check_local_path(&source)?;
    let _source_paths = DirectoryLocks::for_execution(&source)?;
    let caller = identity::inspect_process(caller_pid)?;
    let coordinator = identity::inspect_process(coordinator_pid)?;
    if caller.principal.sid != coordinator.principal.sid
        || parent_pid_of(coordinator_pid)? != caller_pid
        || caller
            .image
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.eq_ignore_ascii_case("carbonpaper.exe"))
    {
        return Err(BrokerError::AccessDenied);
    }
    let destination = caller.image.parent().ok_or(BrokerError::Integrity)?;
    let _parents = verify_destination(destination)?;
    let release = read_release(&source, manifest::RELEASE_PUBLIC_KEY)?;
    let current = read_release(destination, manifest::RELEASE_PUBLIC_KEY)?;
    if hex::encode(Sha256::digest(&release.bytes)) != args[11]
        || release.manifest.version != args[7]
        || semver::Version::parse(&release.manifest.version)
            .map_err(|_| BrokerError::VersionMismatch)?
            <= semver::Version::parse(&current.manifest.version)
                .map_err(|_| BrokerError::VersionMismatch)?
    {
        return Err(BrokerError::VersionMismatch);
    }
    identity::assert_protected(&caller.image)?;
    identity::verify_file(
        &mut locked_file(&caller.image)?,
        current
            .manifest
            .files
            .get("carbonpaper.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    let own = std::env::current_exe().map_err(|_| BrokerError::Integrity)?;
    let expected = source.join(HELPER_NAME);
    if !identity::path_eq(&own, &expected) || !identity::path_eq(&coordinator.image, &expected) {
        return Err(BrokerError::AccessDenied);
    }
    let mut own_file = locked_file(&own)?;
    identity::verify_file(
        &mut own_file,
        release
            .manifest
            .files
            .get(HELPER_NAME)
            .ok_or(BrokerError::Integrity)?,
    )?;
    let mut pipe = open_pipe(
        &pipe_name(&args[9])?,
        Instant::now() + Duration::from_secs(30),
    )?;
    // Verify the real server PID before consuming any command. IDENTIFICATION
    // SQOS prevents that ordinary server from acquiring this elevated token.
    let mut server_pid = 0;
    unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut server_pid) }
        .map_err(|_| BrokerError::AccessDenied)?;
    if server_pid != coordinator_pid {
        return Err(BrokerError::AccessDenied);
    }
    let parent = unsafe {
        OwnedHandle(
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                caller_pid,
            )
            .map_err(|_| BrokerError::AccessDenied)?,
        )
    };
    let mut transaction =
        match Transaction::prepare_protected(&source, destination, &release, &current) {
            Ok(transaction) => transaction,
            Err(error) => {
                let _ = send(&mut pipe, FAILED);
                return Err(error);
            }
        };
    send(&mut pipe, PREPARED)?;
    if receive(&mut pipe, Instant::now() + Duration::from_secs(120))? != COMMIT {
        return Err(BrokerError::InvalidRequest);
    }
    if unsafe { WaitForSingleObject(parent.0, 60_000) } != WAIT_OBJECT_0 {
        return Err(BrokerError::Busy);
    }
    let installed = transaction
        .commit()
        .and_then(|_| transaction.execution_locks());
    let execution = match installed {
        Ok(locks) => locks,
        Err(error) => {
            transaction.rollback()?;
            let _ = send(&mut pipe, FAILED);
            return Err(error);
        }
    };
    // Once installed, a broken channel must not roll back a possibly running
    // new application. Only an explicit pre-launch failure requests rollback.
    transaction.finish();
    send(&mut pipe, INSTALLED)?;
    if matches!(
        receive(&mut pipe, Instant::now() + Duration::from_secs(120)),
        Ok(ABORT)
    ) {
        drop(execution);
        transaction.committed = false;
        transaction.rollback()?;
        send(&mut pipe, ROLLED_BACK)?;
    }
    Ok(())
}

#[cfg(all(test, not(feature = "development-runtime")))]
mod tests;
