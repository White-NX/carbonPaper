use super::*;
use windows::Win32::{
    Security::{
        GetTokenInformation, RevertToSelf, SecurityIdentification, TokenImpersonationLevel,
        SECURITY_IMPERSONATION_LEVEL, TOKEN_QUERY,
    },
    System::Threading::{GetCurrentThread, OpenThreadToken},
    UI::Shell::CommandLineToArgvW,
};

#[test]
fn quoted_uac_arguments_round_trip_through_windows_parsing() {
    let arguments = [
        "setup.exe",
        r"C:\Program Files\User's app\",
        "literal\"quote",
        "1.2.3+build",
    ];
    let command = arguments
        .iter()
        .map(|value| quote(value))
        .collect::<Vec<_>>()
        .join(" ");
    let command = identity::wide(command.as_ref());
    // SAFETY: the parser's allocated array is read within its returned length,
    // then freed exactly once after copying its terminated arguments.
    let actual = unsafe {
        let mut count = 0;
        let pointer = CommandLineToArgvW(PCWSTR(command.as_ptr()), &mut count);
        assert!(!pointer.is_null());
        let values = (0..count as usize)
            .map(|index| (*pointer.add(index)).to_string().unwrap())
            .collect::<Vec<_>>();
        LocalFree(HLOCAL(pointer.cast()));
        values
    };
    assert_eq!(actual, arguments);
}

#[test]
fn elevation_pipe_has_one_instance_and_an_identification_only_client() {
    let name = pipe_name(&format!("{:032x}", rand::random::<u128>())).unwrap();
    let mut server = create_pipe(&name).unwrap();
    assert!(create_pipe(&name).is_err());
    let mut client = open_pipe(&name, Instant::now() + Duration::from_secs(3)).unwrap();
    // SAFETY: the pseudo handle refers to this live process throughout connect.
    connect(
        &server,
        unsafe { GetCurrentProcess() },
        Instant::now() + Duration::from_secs(3),
    )
    .unwrap();
    send(&mut client, COMMIT).unwrap();
    assert_eq!(
        receive(&mut server, Instant::now() + Duration::from_secs(3)).unwrap(),
        COMMIT
    );
    // SAFETY: both handles are live. The impersonation is always reverted, even
    // if inspection fails; only identification-level token access is queried.
    let level = unsafe {
        ImpersonateNamedPipeClient(HANDLE(server.as_raw_handle())).unwrap();
        let inspected = (|| -> Result<SECURITY_IMPERSONATION_LEVEL> {
            let mut token = HANDLE::default();
            OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token)
                .map_err(|_| BrokerError::AccessDenied)?;
            let token = OwnedHandle(token);
            let mut level = SECURITY_IMPERSONATION_LEVEL::default();
            let mut length = 0;
            GetTokenInformation(
                token.0,
                TokenImpersonationLevel,
                Some((&mut level as *mut SECURITY_IMPERSONATION_LEVEL).cast()),
                std::mem::size_of_val(&level) as u32,
                &mut length,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            Ok(level)
        })();
        RevertToSelf().unwrap();
        inspected.unwrap()
    };
    assert_eq!(level, SecurityIdentification);
}

#[test]
fn elevated_updates_reject_user_mutable_installation_directories() {
    let directory = tempfile::tempdir().unwrap();
    assert!(verify_destination(directory.path()).is_err());
}

#[test]
fn invalid_pipe_names_cannot_select_another_endpoint() {
    for value in [
        "",
        "../other",
        "\\other",
        "g0000000000000000000000000000000",
    ] {
        assert!(pipe_name(value).is_err());
    }
}

#[test]
fn elevation_rejects_remote_and_relative_paths() {
    for path in [
        r"\\server\share\update",
        r"\\?\UNC\server\share\update",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\update",
        r"\\?\Volume{not-a-volume}\update",
        r"relative\update",
    ] {
        assert!(check_local_path(Path::new(path)).is_err());
    }
    check_local_path(tempfile::tempdir().unwrap().path()).unwrap();
}

#[test]
fn locked_executables_launch_through_the_shell_in_paths_with_spaces() {
    let directory = tempfile::Builder::new()
        .prefix("CarbonPaper user's update ")
        .tempdir()
        .unwrap();
    let original = directory.path().join("setup.exe");
    fs::copy(std::env::current_exe().unwrap(), &original).unwrap();
    let _paths = DirectoryLocks::for_execution(directory.path()).unwrap();
    let _executable = locked_file(&original).unwrap();
    let process = shell_execute(&original, directory.path(), "open", &["--list".into()]).unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(process.0, 30_000) },
        WAIT_OBJECT_0
    );
    let mut exit = 1;
    unsafe { GetExitCodeProcess(process.0, &mut exit) }.unwrap();
    assert_eq!(exit, 0);
    assert!(fs::write(&original, b"changed before UAC execution").is_err());
}
