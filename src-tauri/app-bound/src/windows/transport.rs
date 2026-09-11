use crate::{
    protocol::*,
    windows::{identity, service},
};
use std::{
    fs::File,
    io::Write,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    time::{Duration, Instant},
};
use windows::{
    core::HRESULT,
    Win32::{
        Foundation::{ERROR_NO_DATA, HANDLE},
        Storage::FileSystem::{ReadFile, SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT},
        System::Pipes::*,
    },
};
use zeroize::Zeroizing;

// Excludes FILE_APPEND_DATA/FILE_CREATE_PIPE_INSTANCE. Generic write access on
// a named pipe would accidentally grant the ability to impersonate its server.
pub const PIPE_CLIENT_ACCESS: u32 = 0x0012_0183;

pub fn call(request: Request) -> Result<Response> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut file = open_pipe(PIPE_NAME, deadline)?;
    let handle = HANDLE(file.as_raw_handle());
    // Verify the peer before sending data that permits pipe impersonation.
    // SAFETY: file owns the live pipe handle and output storage is initialized.
    unsafe {
        let mut pid = 0;
        GetNamedPipeServerProcessId(handle, &mut pid).map_err(|_| BrokerError::AccessDenied)?;
        if service::running_pid()? != pid {
            return Err(BrokerError::AccessDenied);
        }
        let process = identity::inspect_process(pid)?;
        if process.principal.sid != "S-1-5-18" {
            return Err(BrokerError::AccessDenied);
        }
        let expected = identity::protected_root()?
            .join("System")
            .join("carbonpaper-key-service.exe");
        if !identity::path_eq(&process.image, &expected) {
            return Err(BrokerError::AccessDenied);
        }
        identity::assert_protected(&expected)?;
    }
    exchange(&mut file, request, deadline)
}

fn open_pipe(name: &str, deadline: Instant) -> Result<File> {
    loop {
        match std::fs::OpenOptions::new()
            .access_mode(PIPE_CLIENT_ACCESS)
            .custom_flags((SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION).0)
            .open(name)
        {
            Ok(file) => return Ok(file),
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 231)) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(_) => return Err(BrokerError::Unavailable),
        }
    }
}

fn exchange(file: &mut File, request: Request, deadline: Instant) -> Result<Response> {
    // SAFETY: the owning File keeps this synchronous pipe handle live. This
    // changes only its wait mode, after production has verified the server.
    unsafe {
        SetNamedPipeHandleState(HANDLE(file.as_raw_handle()), Some(&PIPE_NOWAIT), None, None)
            .map_err(|_| BrokerError::Unavailable)?;
    }
    let challenge: Challenge =
        serde_json::from_slice(&read_frame(file, deadline)?).map_err(|_| BrokerError::Integrity)?;
    if challenge.version != PROTOCOL_VERSION || !valid_id(&challenge.nonce) {
        return Err(BrokerError::VersionMismatch);
    }
    let frame = RequestFrame {
        version: PROTOCOL_VERSION,
        nonce: challenge.nonce,
        sequence: 1,
        request,
    };
    let bytes =
        Zeroizing::new(serde_json::to_vec(&frame).map_err(|_| BrokerError::InvalidRequest)?);
    write_frame(file, &bytes, deadline)?;
    let reply: Response =
        serde_json::from_slice(&read_frame(file, deadline)?).map_err(|_| BrokerError::Integrity)?;
    // The operation already has a complete reply; an interrupted final ACK
    // must not turn an accepted state change into a reported failure.
    let _ = write_bytes(file, &[RESPONSE_ACK], deadline);
    match reply {
        Response::Error(error) => Err(error),
        other => Ok(other),
    }
}

/// Native integration fixtures use isolated pipes and synthetic registrations.
/// This entry point is absent from every application and service build.
#[cfg(test)]
pub(super) fn call_test_pipe(name: &str, request: Request) -> Result<Response> {
    let deadline = Instant::now() + Duration::from_secs(10);
    exchange(&mut open_pipe(name, deadline)?, request, deadline)
}

fn transient(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(232 | 233 | 536))
        || error.kind() == std::io::ErrorKind::WouldBlock
}

fn read_exact(file: &mut File, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(BrokerError::Unavailable);
        }
        let mut count = 0;
        // File::read maps ERROR_NO_DATA on a PIPE_NOWAIT handle to Ok(0).
        // Preserve the Win32 error so an empty pipe remains distinct from EOF.
        // SAFETY: file owns a synchronous handle; the exclusive buffer and byte
        // count remain valid until ReadFile returns, with no pending I/O.
        let result = unsafe {
            ReadFile(
                HANDLE(file.as_raw_handle()),
                Some(bytes),
                Some(&mut count),
                None,
            )
        };
        match result {
            Ok(()) if count == 0 => return Err(BrokerError::Unavailable),
            Ok(()) => bytes = &mut bytes[count as usize..],
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_DATA.0) => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => return Err(BrokerError::Unavailable),
        }
    }
    Ok(())
}

fn read_frame(file: &mut File, deadline: Instant) -> Result<Zeroizing<Vec<u8>>> {
    let mut prefix = [0; 4];
    read_exact(file, &mut prefix, deadline)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(BrokerError::LimitExceeded);
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    read_exact(file, &mut bytes, deadline)?;
    Ok(bytes)
}

fn write_frame(file: &mut File, bytes: &[u8], deadline: Instant) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(BrokerError::LimitExceeded);
    }
    let mut data = Zeroizing::new(Vec::with_capacity(bytes.len() + 4));
    data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    data.extend_from_slice(bytes);
    write_bytes(file, &data, deadline)
}

fn write_bytes(file: &mut File, mut remaining: &[u8], deadline: Instant) -> Result<()> {
    while !remaining.is_empty() {
        if Instant::now() >= deadline {
            return Err(BrokerError::Unavailable);
        }
        match file.write(remaining) {
            Ok(0) => std::thread::sleep(Duration::from_millis(5)),
            Ok(count) => remaining = &remaining[count..],
            Err(error) if transient(&error) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err(BrokerError::Unavailable),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
