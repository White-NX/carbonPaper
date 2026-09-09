use crate::{
    crypto::KeyProtector,
    ledger::Ledger,
    protocol::*,
    windows::identity::{self, wide, VerifiedCaller},
};
use std::{
    collections::HashMap,
    os::windows::io::AsRawHandle,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::NamedPipeServer,
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            self, Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            Cryptography::*, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
        },
        System::{Pipes::*, Services::*},
    },
};
use zeroize::{Zeroize, Zeroizing};

static STOP: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);
// A task panic releases this guard instead of poisoning the shared ledger.
type LedgerLock = tokio::sync::Mutex<Ledger>;

pub struct ServiceHandle(pub SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

pub fn open_service(access: u32) -> Result<ServiceHandle> {
    try_open_service(access)?.ok_or(BrokerError::Unavailable)
}

pub fn try_open_service(access: u32) -> Result<Option<ServiceHandle>> {
    let name = wide(SERVICE_NAME.as_ref());
    // SAFETY: handles are closed by their owners; the terminated name lives
    // through OpenService and neither API retains pointers to Rust storage.
    unsafe {
        let manager = ServiceHandle(
            OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
                .map_err(|_| BrokerError::AccessDenied)?,
        );
        match OpenServiceW(manager.0, PCWSTR(name.as_ptr()), access) {
            Ok(handle) => Ok(Some(ServiceHandle(handle))),
            Err(error) if error.code() == windows::core::HRESULT::from_win32(1060) => Ok(None),
            Err(_) => Err(BrokerError::AccessDenied),
        }
    }
}

pub fn query_status(handle: &ServiceHandle) -> Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    // SAFETY: the byte view refers to an aligned initialized status struct and
    // is exactly the documented buffer size, with no alias active during call.
    unsafe {
        let bytes = std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>(),
        );
        QueryServiceStatusEx(handle.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed)
            .map_err(|_| BrokerError::Unavailable)?;
    }
    Ok(status)
}

pub fn running_pid() -> Result<u32> {
    let status = query_status(&open_service(SERVICE_QUERY_STATUS)?)?;
    if status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId == 0 {
        return Err(BrokerError::Unavailable);
    }
    Ok(status.dwProcessId)
}

pub fn run() -> Result<()> {
    let mut name = wide(SERVICE_NAME.as_ref());
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: the dispatcher blocks until ServiceMain returns; table and name
    // remain alive throughout, and callbacks use only process-owned state.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()).map_err(|_| BrokerError::Unavailable) }
}

unsafe extern "system" fn control(
    code: u32,
    _event: u32,
    _data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    if code == SERVICE_CONTROL_STOP || code == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::SeqCst);
    }
    0
}

fn report(state: SERVICE_STATUS_CURRENT_STATE, exit_code: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst);
    if handle == 0 {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: exit_code,
        dwWaitHint: if state == SERVICE_START_PENDING {
            10_000
        } else {
            0
        },
        ..Default::default()
    };
    // SAFETY: handle is supplied by SCM and remains valid through ServiceMain.
    unsafe {
        let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(handle as *mut _), &status);
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(SERVICE_NAME.as_ref());
    let Ok(handle) = RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(control), None)
    else {
        return;
    };
    STATUS_HANDLE.store(handle.0 as isize, Ordering::SeqCst);
    STOP.store(false, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 0);
    let result = std::panic::catch_unwind(|| -> Result<()> {
        if identity::current_sid()? != "S-1-5-18" {
            return Err(BrokerError::AccessDenied);
        }
        allow_peer_identity_queries()?;
        let directory = identity::state_root()?;
        identity::assert_protected(&directory)?;
        let ledger = Arc::new(LedgerLock::new(Ledger::open(&directory.join("keys.db"))?));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| BrokerError::Unavailable)?;
        runtime.block_on(serve(ledger))
    });
    if matches!(result, Ok(Ok(()))) || STOP.load(Ordering::SeqCst) {
        report(SERVICE_STOPPED, 0);
    } else {
        // Do not report SERVICE_STOPPED for a fatal result. This lets SCM
        // classify the process termination as a service failure and apply the
        // configured restart action.
        std::process::exit(1);
    }
}

fn allow_peer_identity_queries() -> Result<()> {
    // Standard-user clients must verify the actual SYSTEM process token. Give
    // them only QUERY_LIMITED_INFORMATION on the process and TOKEN_QUERY on its
    // token, never VM access, handle duplication, token duplication or mutation.
    unsafe {
        let process = windows::Win32::System::Threading::GetCurrentProcess();
        set_query_dacl(process, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x1000;;;AU)")?;
        let mut token = HANDLE::default();
        windows::Win32::System::Threading::OpenProcessToken(
            process,
            Security::TOKEN_QUERY | Security::TOKEN_ACCESS_MASK(0x0004_0000),
            &mut token,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let token = identity::OwnedHandle(token);
        set_query_dacl(token.0, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0008;;;AU)")
    }
}

fn set_query_dacl(handle: HANDLE, sddl: &str) -> Result<()> {
    let text = wide(sddl.as_ref());
    // SAFETY: the service owns these kernel objects. The allocated descriptor
    // stays alive until SetSecurityInfo copies its DACL, then is freed once.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(text.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let result = (|| {
            let mut acl = std::ptr::null_mut();
            let mut present = windows::Win32::Foundation::BOOL::default();
            let mut defaulted = present;
            Security::GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
                .map_err(|_| BrokerError::AccessDenied)?;
            Security::Authorization::SetSecurityInfo(
                handle,
                Security::Authorization::SE_KERNEL_OBJECT,
                Security::DACL_SECURITY_INFORMATION,
                Security::PSID::default(),
                Security::PSID::default(),
                Some(acl),
                None,
            )
            .ok()
            .map_err(|_| BrokerError::AccessDenied)
        })();
        LocalFree(HLOCAL(descriptor.0));
        result
    }
}

fn create_pipe(first: bool) -> Result<NamedPipeServer> {
    let text = wide("O:SYG:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x00120183;;;AU)".as_ref());
    let name = wide(PIPE_NAME.as_ref());
    // SAFETY: security descriptor is retained until CreateNamedPipe copies it;
    // the uniquely owned overlapped pipe handle is transferred to Tokio once.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(text.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let flags = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                Default::default()
            };
        let handle = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            flags,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            32,
            MAX_FRAME_BYTES as u32,
            MAX_FRAME_BYTES as u32,
            1000,
            Some(&security),
        );
        LocalFree(HLOCAL(descriptor.0));
        if handle.is_invalid() {
            return Err(BrokerError::Unavailable);
        }
        match NamedPipeServer::from_raw_handle(handle.0) {
            Ok(server) => Ok(server),
            Err(_) => {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
                Err(BrokerError::Unavailable)
            }
        }
    }
}

async fn serve(ledger: Arc<LedgerLock>) -> Result<()> {
    let cache = Arc::new(Mutex::new(HashMap::<String, VerifiedCaller>::new()));
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    let mut pipe = create_pipe(true)?;
    let mut last_maintenance = std::time::Instant::now();
    report(SERVICE_RUNNING, 0);
    while !STOP.load(Ordering::SeqCst) {
        if last_maintenance.elapsed() >= Duration::from_secs(30) {
            let mut ledger = ledger.lock().await;
            ledger.maintenance(now_secs())?;
            last_maintenance = std::time::Instant::now();
        }
        match tokio::time::timeout(Duration::from_millis(500), pipe.connect()).await {
            Err(_) => continue,
            Ok(Err(_)) => return Err(BrokerError::Unavailable),
            Ok(Ok(())) => {}
        }
        let accepted = std::mem::replace(&mut pipe, create_pipe(false)?);
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            drop(accepted);
            continue;
        };
        let ledger = ledger.clone();
        let cache = cache.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(
                Duration::from_secs(8),
                handle_connection(accepted, ledger, cache),
            )
            .await;
        });
    }
    Ok(())
}

async fn read_frame(pipe: &mut NamedPipeServer) -> Result<Zeroizing<Vec<u8>>> {
    let length = pipe
        .read_u32_le()
        .await
        .map_err(|_| BrokerError::Unavailable)? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(BrokerError::LimitExceeded);
    }
    let mut data = Zeroizing::new(vec![0; length]);
    pipe.read_exact(&mut data)
        .await
        .map_err(|_| BrokerError::Unavailable)?;
    Ok(data)
}
async fn write_json<T: serde::Serialize>(pipe: &mut NamedPipeServer, value: &T) -> Result<()> {
    let data = Zeroizing::new(serde_json::to_vec(value).map_err(|_| BrokerError::InvalidRequest)?);
    if data.len() > MAX_FRAME_BYTES {
        return Err(BrokerError::LimitExceeded);
    }
    pipe.write_u32_le(data.len() as u32)
        .await
        .map_err(|_| BrokerError::Unavailable)?;
    pipe.write_all(&data)
        .await
        .map_err(|_| BrokerError::Unavailable)
}

async fn handle_connection(
    mut pipe: NamedPipeServer,
    ledger: Arc<LedgerLock>,
    cache: Arc<Mutex<HashMap<String, VerifiedCaller>>>,
) -> Result<()> {
    let principal = {
        let handle = HANDLE(pipe.as_raw_handle());
        let mut pid = 0;
        // SAFETY: pipe owns this handle; PID comes from the kernel.
        unsafe {
            GetNamedPipeClientProcessId(handle, &mut pid).map_err(|_| BrokerError::AccessDenied)?;
        }
        let process = identity::inspect_process(pid)?;
        let mut cache = cache.lock().map_err(|_| BrokerError::Unavailable)?;
        let id = process.principal.process_identity.clone();
        if !cache.contains_key(&id) {
            if cache.len() >= 32 {
                cache.clear();
            }
            cache.insert(id.clone(), identity::verify_main(process)?);
        }
        cache
            .get(&id)
            .ok_or(BrokerError::AccessDenied)?
            .principal
            .clone()
    };
    let challenge = Challenge {
        version: PROTOCOL_VERSION,
        nonce: random_id(),
    };
    write_json(&mut pipe, &challenge).await?;
    let bytes = read_frame(&mut pipe).await?;
    let frame: RequestFrame =
        serde_json::from_slice(&bytes).map_err(|_| BrokerError::InvalidRequest)?;
    frame.validate(&challenge.nonce)?;
    let response = {
        // Acquire the lock before constructing the protector. No await occurs
        // while impersonating, so the protector stays on one OS thread.
        let result = {
            let mut ledger = ledger.lock().await;
            let protector = DpapiProtector {
                pipe: HANDLE(pipe.as_raw_handle()),
                sid: &principal.sid,
            };
            ledger.handle(&principal, frame.request, now_secs(), &protector)
        };
        result.unwrap_or_else(Response::Error)
    };
    write_json(&mut pipe, &response).await?;
    // Keep the server handle alive until the peer has read the entire reply.
    // Closing a Windows pipe with unread buffered data can discard that data.
    // The connection's outer timeout also bounds a missing acknowledgement.
    if pipe.read_u8().await.map_err(|_| BrokerError::Unavailable)? != RESPONSE_ACK {
        return Err(BrokerError::InvalidRequest);
    }
    Ok(())
}

struct Impersonation;
impl Impersonation {
    fn begin(pipe: HANDLE, expected_sid: &str) -> Result<Self> {
        unsafe {
            ImpersonateNamedPipeClient(pipe).map_err(|_| BrokerError::AccessDenied)?;
        }
        let guard = Self;
        // Verify the pipe token as well as the client process token. SQOS must
        // permit impersonation, and another user's token is never accepted.
        unsafe {
            let mut token = HANDLE::default();
            windows::Win32::System::Threading::OpenThreadToken(
                windows::Win32::System::Threading::GetCurrentThread(),
                Security::TOKEN_QUERY,
                true,
                &mut token,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            let token = identity::OwnedHandle(token);
            if identity::token_sid(token.0)? != expected_sid {
                return Err(BrokerError::AccessDenied);
            }
        }
        Ok(guard)
    }
}
impl Drop for Impersonation {
    fn drop(&mut self) {
        // Continuing on a pooled thread after a failed revert is unsafe.
        if unsafe { Security::RevertToSelf() }.is_err() {
            std::process::abort();
        }
    }
}

fn dpapi(input: &[u8], encrypt: bool) -> Result<Zeroizing<Vec<u8>>> {
    if input.len() > 4096 {
        return Err(BrokerError::LimitExceeded);
    }
    let data = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input is retained for the synchronous DPAPI call. Its result is
    // copied, erased, and freed with the API's prescribed allocator.
    unsafe {
        if encrypt {
            CryptProtectData(
                &data,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &data,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .map_err(|_| BrokerError::Protection)?;
        if output.pbData.is_null() {
            return Err(BrokerError::Protection);
        }
        let bytes = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let result = Zeroizing::new(bytes.to_vec());
        bytes.zeroize();
        LocalFree(HLOCAL(output.pbData.cast()));
        Ok(result)
    }
}

struct DpapiProtector<'a> {
    pipe: HANDLE,
    sid: &'a str,
}
impl KeyProtector for DpapiProtector<'_> {
    fn protect(&self, key: &TaskKey) -> Result<Vec<u8>> {
        let user = {
            let _guard = Impersonation::begin(self.pipe, self.sid)?;
            dpapi(&key.0, true)?
        };
        Ok(dpapi(&user, true)?.to_vec())
    }
    fn unprotect(&self, blob: &[u8]) -> Result<TaskKey> {
        let user = dpapi(blob, false)?;
        let plaintext = {
            let _guard = Impersonation::begin(self.pipe, self.sid)?;
            dpapi(&user, false)?
        };
        if plaintext.len() != 32 {
            return Err(BrokerError::Integrity);
        }
        Ok(TaskKey(plaintext.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ledger_lock_is_available_after_a_task_panic() {
        let directory = tempfile::tempdir().expect("create test directory");
        let ledger = Arc::new(LedgerLock::new(
            Ledger::open(&directory.path().join("keys.db")).expect("open test ledger"),
        ));
        let task_ledger = ledger.clone();
        let task = tokio::spawn(async move {
            let _guard = task_ledger.lock().await;
            panic!("intentional ledger lock panic");
        });

        assert!(task.await.expect_err("task should panic").is_panic());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), ledger.lock())
                .await
                .is_ok(),
            "ledger lock remained unavailable after task panic"
        );
    }
}
