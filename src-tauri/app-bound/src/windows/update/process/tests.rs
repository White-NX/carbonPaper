use super::*;
use std::os::windows::ffi::OsStringExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use windows::Win32::{
    Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, WAIT_TIMEOUT},
    Security::{SecurityIdentification, TokenImpersonationLevel, SECURITY_ATTRIBUTES},
    System::Pipes::ImpersonateNamedPipeClient,
    UI::Shell::{CommandLineToArgvW, IsUserAnAdmin},
};

#[test]
fn coordinator_arguments_preserve_windows_paths_and_utf16() {
    let arguments = [
        OsString::from("setup.exe"),
        OsString::from(r"C:\Program Files\User's app\"),
        OsString::from("literal\"quote"),
        OsString::from("更新"),
        OsString::from_wide(&[0xd800, b'\\' as u16]),
    ];
    let command = command_line(&arguments[0], &arguments[1..]);
    // SAFETY: CommandLineToArgvW owns a terminated argument array. Read within
    // its returned count and copy each UTF-16 string before freeing that array.
    let actual = unsafe {
        let mut count = 0;
        let pointer = CommandLineToArgvW(PCWSTR(command.as_ptr()), &mut count);
        assert!(!pointer.is_null());
        let values = (0..count as usize)
            .map(|index| {
                let argument = *pointer.add(index);
                let mut length = 0;
                while *argument.0.add(length) != 0 {
                    length += 1;
                }
                OsString::from_wide(std::slice::from_raw_parts(argument.0, length))
            })
            .collect::<Vec<_>>();
        LocalFree(HLOCAL(pointer.cast()));
        values
    };
    assert_eq!(actual, arguments);
}

async fn check_coordinator_process() {
    let context = CoordinatorContext::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    // Match extraction by an elevated app: it creates the directory and helper
    // before the restricted child reads and executes that helper.
    let executable = directory.path().join("coordinator 更新.exe");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    // SAFETY: the unnamed event is owned throughout this isolated regression;
    // its deliberately inheritable handle must not leak into the coordinator.
    let event = unsafe {
        OwnedHandle(CreateEventW(Some(&attributes), true, false, PCWSTR::null()).unwrap())
    };
    let (child, pipe) = context
        .spawn_coordinator(
            &executable,
            &[
                "windows::update::process::tests::coordinator_subprocess_requires_restricted_token_and_commit".into(),
                "--ignored".into(),
                "--exact".into(),
                "--nocapture".into(),
            ],
            directory.path(),
            true,
        )
        .unwrap();
    pipe.connect().await.unwrap();
    let mut pipe = BufReader::new(pipe);
    read_marker(
        &mut pipe,
        "COORDINATOR_READY",
        HANDLE(child.process.as_raw_handle()),
    )
    .await;
    let process = HANDLE(child.process.as_raw_handle());
    // SAFETY: this queries the effective caller token without passing pointers.
    let before = unsafe { IsUserAnAdmin() }.as_bool();
    assert!(!context
        .with_permissions(|| {
            // SAFETY: this only queries the temporarily restricted thread token.
            unsafe { IsUserAnAdmin() }.as_bool()
        })
        .unwrap());
    // SAFETY: the query has no pointer inputs and observes this restored thread.
    let after = unsafe { IsUserAnAdmin() }.as_bool();
    assert_eq!(after, before);
    // SAFETY: the child is blocked on its private stdin, so its actual process
    // and primary token remain live for the parent/SID/permission inspection.
    unsafe {
        let pid = GetProcessId(process);
        assert_eq!(
            super::super::parent_pid_of(pid).unwrap(),
            std::process::id()
        );
        assert_eq!(
            identity::process_sid(process).unwrap(),
            identity::current_sid().unwrap()
        );
        let mut original = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut original).unwrap();
        let original = OwnedHandle(original);
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).unwrap();
        let token = OwnedHandle(token);
        let elevation = token_information(token.0, TokenElevation).unwrap();
        assert_eq!(
            (*elevation.as_ptr().cast::<TOKEN_ELEVATION>()).TokenIsElevated,
            0
        );
        // Even when the parent is elevated, the private pipe must only expose
        // a restricted identification token, never impersonation authority.
        ImpersonateNamedPipeClient(HANDLE(pipe.get_ref().as_raw_handle())).unwrap();
        let mut client_token = HANDLE::default();
        let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut client_token);
        RevertToSelf().unwrap();
        opened.unwrap();
        let client_token = OwnedHandle(client_token);
        let level = token_information(client_token.0, TokenImpersonationLevel).unwrap();
        assert_eq!(*level.as_ptr().cast::<i32>(), SecurityIdentification.0);
        assert_eq!(
            identity::token_sid(client_token.0).unwrap(),
            identity::token_sid(original.0).unwrap()
        );
        let elevation = token_information(client_token.0, TokenElevation).unwrap();
        assert_eq!(
            (*elevation.as_ptr().cast::<TOKEN_ELEVATION>()).TokenIsElevated,
            0
        );
        if context.token.is_some() {
            validate_restricted_token(token.0, original.0).unwrap();
            validate_restricted_token(client_token.0, original.0).unwrap();
        }
        // A reused numeric handle can name an unrelated child object. Signal
        // any duplicated event, then check the original event itself for leaks.
        let mut duplicate = HANDLE::default();
        if DuplicateHandle(
            process,
            event.0,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .is_ok()
        {
            let duplicate = OwnedHandle(duplicate);
            SetEvent(duplicate.0).ok();
        }
        assert_eq!(WaitForSingleObject(event.0, 0), WAIT_TIMEOUT);
    }
    if context.token.is_some() {
        let protected = directory.path().join("administrator-owned");
        fs::create_dir(&protected).unwrap();
        identity::protect_path(&protected, true).unwrap();
        let path = protected.join("write-probe");
        fs::write(&path, b"administrator").unwrap();
        let error = context
            .with_permissions(|| fs::write(&path, b"coordinator"))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        // Thread impersonation must be restored before any subsequent work.
        fs::write(&path, b"administrator again").unwrap();
    }
    pipe.get_mut().write_all(b"COMMIT\n").await.unwrap();
    read_marker(&mut pipe, "COORDINATOR_COMMITTED", process).await;
    // SAFETY: `child` retains this SYNCHRONIZE-capable process handle during
    // the wait, with no transfer of ownership to Win32.
    let finished = unsafe { WaitForSingleObject(process, 10_000) };
    assert_eq!(finished, WAIT_OBJECT_0);
    let mut exit = 1;
    // SAFETY: the subprocess owner is still live and `exit` is initialized
    // writable output for the synchronous exit-code query.
    unsafe { GetExitCodeProcess(process, &mut exit) }.unwrap();
    assert_eq!(exit, 0);

    let restart = context
        .spawn_restart(
            &executable,
            &[
                "windows::update::process::tests::restart_subprocess_requires_restricted_token"
                    .into(),
                "--ignored".into(),
                "--exact".into(),
            ],
            directory.path(),
        )
        .unwrap();
    // SAFETY: `restart` retains the subprocess handle throughout the wait;
    // the API only observes this process and retains no borrowed memory.
    let restarted = unsafe { WaitForSingleObject(HANDLE(restart.process.as_raw_handle()), 10_000) };
    assert_eq!(restarted, WAIT_OBJECT_0);
    // SAFETY: the restart owner remains live and `exit` is writable output
    // borrowed only for this synchronous query.
    unsafe { GetExitCodeProcess(HANDLE(restart.process.as_raw_handle()), &mut exit) }.unwrap();
    assert_eq!(exit, 0);
}

async fn read_marker(pipe: &mut BufReader<NamedPipeServer>, marker: &str, process: HANDLE) {
    let mut output = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut line = String::new();
            let count = pipe.read_line(&mut line).await.unwrap();
            output.push_str(&line);
            if count == 0 {
                let mut exit = 0;
                // SAFETY: the caller retains the subprocess handle while this
                // marker reader runs; `exit` is initialized query output.
                unsafe { GetExitCodeProcess(process, &mut exit) }.unwrap();
                panic!("coordinator exited before {marker} with code {exit:#x}: {output}");
            }
            if line.contains(marker) {
                break;
            }
        }
    })
    .await
    .expect("coordinator handshake timed out");
}

fn check_child_identity() {
    assert!(!elevation_status().unwrap());
    let pid = super::super::parent_pid().unwrap();
    let parent = identity::inspect_process(pid).unwrap();
    assert_eq!(parent.principal.sid, identity::current_sid().unwrap());
    // SAFETY: these query/wait handles refer to this live child and its parent.
    // The restricted child must authenticate and wait on its elevated parent,
    // and its default DACL must still allow access to its own process objects.
    unsafe {
        let _parent = OwnedHandle(
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
            .unwrap(),
        );
        let _own = OwnedHandle(OpenProcess(PROCESS_ALL_ACCESS, false, std::process::id()).unwrap());
    }
}

#[test]
#[ignore = "subprocess fixture for the native coordinator handshake regression"]
fn coordinator_subprocess_requires_restricted_token_and_commit() {
    check_child_identity();
    let mut stdout = std::io::stdout();
    writeln!(stdout, "COORDINATOR_READY").unwrap();
    stdout.flush().unwrap();
    let mut decision = String::new();
    std::io::stdin()
        .lock()
        .take(16)
        .read_line(&mut decision)
        .unwrap();
    assert_eq!(decision, "COMMIT\n");
    writeln!(stdout, "COORDINATOR_COMMITTED").unwrap();
    stdout.flush().unwrap();
}

#[test]
#[ignore = "subprocess fixture for the native restart regression"]
fn restart_subprocess_requires_restricted_token() {
    check_child_identity();
}

#[tokio::test]
async fn coordinator_keeps_its_real_parent_and_inherits_only_private_stdio() {
    check_coordinator_process().await;
}

#[tokio::test]
#[ignore = "requires an administrator process to exercise native token restriction"]
async fn elevated_coordinator_keeps_parent_and_runs_with_restricted_permissions() {
    assert!(elevation_status().unwrap());
    check_coordinator_process().await;
}
