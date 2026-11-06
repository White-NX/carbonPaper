//! Single-instance NMH pipes and OS-derived peer authentication.
//!
//! All server handles live through disconnect/reconnect. In particular, checking
//! GetNamedPipeServerProcessId is insufficient if another process can add an
//! instance to the same name: Windows can report the original pipe creator.

// The desktop and standalone NMH compile this shared module and use different
// client/server halves. Unused members in one binary are used by the other.
#![allow(dead_code)]

use crate::nmh_protocol::{self, constant_time_eq, IPC_PROTOCOL_VERSION, RESPONSE_ACK};
use crate::reverse_ipc_protocol::{read_server_ipc_frame, write_ipc_frame, StorageResponse};
use crate::windows_platform::{self, ProcessIdentity};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};
use tokio::sync::mpsc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{
    CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

#[cfg(test)]
#[path = "nmh_pipe_tests.rs"]
mod tests;

// Read/write data and attributes, READ_CONTROL and SYNCHRONIZE; deliberately
// exclude FILE_APPEND_DATA / FILE_CREATE_PIPE_INSTANCE (0x4).
pub(crate) const PIPE_CLIENT_ACCESS: u32 = 0x0012_0183;
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
pub(crate) enum PeerRole {
    Desktop,
    NativeHost,
}

impl PeerRole {
    fn executable(self) -> &'static str {
        match self {
            Self::Desktop => "carbonpaper.exe",
            Self::NativeHost => "carbonpaper-nmh.exe",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PipeSide {
    Client,
    Server,
}

pub(crate) struct PeerVerifier {
    user_sid: String,
    directory: PathBuf,
    role: PeerRole,
    #[cfg(test)]
    test_image: Option<PathBuf>,
}

impl PeerVerifier {
    pub(crate) fn new(role: PeerRole) -> Result<Self, String> {
        Ok(Self {
            user_sid: windows_platform::current_user_sid()?,
            directory: std::env::current_exe()
                .map_err(|e| e.to_string())?
                .parent()
                .ok_or("Cannot locate the native runtime")?
                .to_path_buf(),
            role,
            #[cfg(test)]
            test_image: None,
        })
    }

    pub(crate) fn inspect(
        &self,
        handle: HANDLE,
        side: PipeSide,
    ) -> Result<ProcessIdentity, String> {
        let mut pid = 0;
        // SAFETY: the caller owns a live pipe handle; pid is writable stack storage.
        unsafe {
            match side {
                PipeSide::Client => GetNamedPipeServerProcessId(handle, &mut pid),
                PipeSide::Server => GetNamedPipeClientProcessId(handle, &mut pid),
            }
            .map_err(|e| format!("Cannot query pipe peer PID: {e}"))?;
        }
        let peer = windows_platform::inspect_process(pid)?;
        if peer.user_sid != self.user_sid {
            return Err("Pipe peer belongs to another user".into());
        }
        Ok(peer)
    }

    pub(crate) fn verify_identity(&self, peer: &ProcessIdentity) -> Result<(), String> {
        if peer.user_sid != self.user_sid {
            return Err("Pipe peer belongs to another user".into());
        }
        #[cfg(test)]
        if let Some(expected) = &self.test_image {
            return path_matches(&peer.image, expected)
                .then_some(())
                .ok_or_else(|| "Unexpected pipe peer executable".into());
        }
        if cfg!(debug_assertions) {
            return path_matches(&peer.image, &self.directory.join(self.role.executable()))
                .then_some(())
                .ok_or_else(|| "Unexpected pipe peer executable".into());
        }
        self.verify_release_image(&peer.image)
    }

    fn verify_release_image(&self, image: &Path) -> Result<(), String> {
        use carbonpaper_app_bound::{manifest, windows::identity};
        if image.file_name().is_none_or(|name| {
            !name
                .to_string_lossy()
                .eq_ignore_ascii_case(self.role.executable())
        }) {
            return Err("Unexpected pipe peer executable".into());
        }
        let directory = image.parent().ok_or("Invalid pipe peer image")?;
        let root = identity::protected_root().map_err(|e| e.to_string())?;
        let runtime_id = directory
            .file_name()
            .ok_or("Invalid runtime directory")?
            .to_string_lossy();
        let protected = identity::safe_runtime_id(&runtime_id)
            && path_matches(directory, &root.join("Runtime").join(runtime_id.as_ref()));
        if !protected && !path_matches(directory, &self.directory) {
            return Err("Pipe peer is outside the trusted native runtime".into());
        }
        if protected {
            for path in [
                root.parent().ok_or("Invalid protected root")?.to_path_buf(),
                root.clone(),
                root.join("Runtime"),
                directory.to_path_buf(),
                image.to_path_buf(),
            ] {
                identity::assert_protected(&path).map_err(|e| e.to_string())?;
            }
        }
        // Accept a still-running NMH from another signed protected release, so
        // restarting/updating the desktop does not require restarting browsers.
        let (release, id) = manifest::read_manifest(directory).map_err(|e| e.to_string())?;
        if protected && id != runtime_id {
            return Err("Pipe peer runtime identity mismatch".into());
        }
        let hash = release
            .files
            .get(self.role.executable())
            .ok_or("Peer is absent from the release")?;
        identity::verify_file(
            &mut identity::locked_file(image).map_err(|e| e.to_string())?,
            hash,
        )
        .map_err(|e| e.to_string())
    }

    pub(crate) fn verify(
        &self,
        handle: HANDLE,
        side: PipeSide,
        expected: Option<&ProcessIdentity>,
    ) -> Result<ProcessIdentity, String> {
        let peer = self.inspect(handle, side)?;
        if let Some(expected) = expected {
            if peer != *expected {
                return Err("Pipe peer process identity changed".into());
            }
        } else {
            self.verify_identity(&peer)?;
        }
        Ok(peer)
    }

    #[cfg(test)]
    pub(crate) fn for_test(image: PathBuf) -> Self {
        Self {
            user_sid: windows_platform::current_user_sid().unwrap(),
            directory: image.parent().unwrap().to_path_buf(),
            role: PeerRole::Desktop,
            test_image: Some(image),
        }
    }
}

pub(crate) fn path_matches(a: &Path, b: &Path) -> bool {
    carbonpaper_app_bound::windows::identity::path_eq(a, b)
}

pub(crate) fn create_server_file(name: &str, overlapped: bool) -> io::Result<File> {
    let sid = windows_platform::current_user_sid().map_err(io::Error::other)?;
    let buffer_size = if name.starts_with(nmh_protocol::CMD_PIPE_PREFIX) {
        4096
    } else {
        1024 * 1024
    };
    // OWNER_RIGHTS suppresses the owner's implicit WRITE_DAC grant. A process
    // sharing this user's SID must not be able to restore the instance-create bit.
    let sddl: Vec<u16> = format!("D:P(A;;0x{PIPE_CLIENT_ACCESS:08x};;;{sid})(A;;RC;;;OW)")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: Windows owns the allocated descriptor until LocalFree; the name
    // and security attributes remain live until CreateNamedPipe copies them.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let mut mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE;
        if overlapped {
            mode |= FILE_FLAG_OVERLAPPED;
        }
        let handle = CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            buffer_size,
            buffer_size,
            0,
            Some(&attributes),
        );
        let result = if handle.is_invalid() {
            Err(io::Error::last_os_error())
        } else {
            Ok(File::from_raw_handle(handle.0))
        };
        LocalFree(HLOCAL(descriptor.0));
        result
    }
}

pub(crate) fn async_server(file: File) -> io::Result<NamedPipeServer> {
    let handle = file.into_raw_handle();
    // SAFETY: Tokio consumes this uniquely owned handle, including closing it
    // if registration fails. The caller must not close it a second time.
    unsafe { NamedPipeServer::from_raw_handle(handle) }
}

fn try_open(name: &str, overlapped: bool) -> io::Result<File> {
    let mut flags = SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
    if overlapped {
        flags |= FILE_FLAG_OVERLAPPED;
    }
    OpenOptions::new()
        .access_mode(PIPE_CLIENT_ACCESS)
        .custom_flags(flags.0)
        .open(name)
}

fn retryable(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(2 | 231))
}

pub(crate) fn open_client(name: &str) -> io::Result<File> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match try_open(name, false) {
            Err(e) if retryable(&e) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            result => return result,
        }
    }
}

pub(crate) async fn open_async_client(name: &str) -> io::Result<NamedPipeClient> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let file = loop {
        match try_open(name, true) {
            Ok(file) => break file,
            Err(e) if retryable(&e) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            Err(e) => return Err(e),
        }
    };
    let handle = file.into_raw_handle();
    // SAFETY: Tokio consumes ownership of this overlapped handle on every path.
    unsafe { NamedPipeClient::from_raw_handle(handle) }
}

pub(crate) fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    writer.write_all(&nmh_protocol::frame_prefix(body.len())?)?;
    writer.write_all(body)
}

pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let mut body = vec![0; nmh_protocol::frame_length(prefix)?];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// A synchronous pipe read/write needs CancelSynchronousIo on its issuing
/// thread; CancelIoEx alone does not reliably bound blocking ReadFile calls.
pub(crate) fn with_io_timeout<T>(
    pipe: HANDLE,
    timeout: Duration,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    use std::sync::mpsc::RecvTimeoutError;
    use windows::Win32::System::Threading::{GetCurrentThreadId, OpenThread, THREAD_TERMINATE};
    use windows::Win32::System::IO::CancelSynchronousIo;
    // SAFETY: query a real handle to this live thread; it outlives the watchdog.
    let thread = windows_platform::OwnedHandle(unsafe {
        OpenThread(THREAD_TERMINATE, false, GetCurrentThreadId())
            .map_err(|e| io::Error::other(e.to_string()))?
    });
    let thread_address = thread.0 .0 as usize;
    let pipe_address = pipe.0 as usize;
    std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            let mut wait = timeout;
            while matches!(done_rx.recv_timeout(wait), Err(RecvTimeoutError::Timeout)) {
                // The deadline can fall between two synchronous calls. Keep
                // cancelling until completion so the next call cannot miss it.
                // SAFETY: the caller retains both handles until this scope joins.
                unsafe {
                    let _ = CancelSynchronousIo(HANDLE(thread_address as *mut _));
                    let _ = windows::Win32::System::IO::CancelIoEx(
                        HANDLE(pipe_address as *mut _),
                        None,
                    );
                }
                wait = Duration::from_millis(10);
            }
        });
        let result = operation();
        let _ = done_tx.send(());
        result
    })
}

pub(crate) fn exchange(
    file: &mut File,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let handle = HANDLE(file.as_raw_handle());
    let reply = with_io_timeout(handle, IO_TIMEOUT, || {
        write_frame(file, &bytes)?;
        let reply = read_frame(file)?;
        // The server may disconnect/reuse its handle only after the reply was
        // consumed; DisconnectNamedPipe otherwise discards unread response bytes.
        file.write_all(&[RESPONSE_ACK])?;
        Ok(reply)
    })
    .map_err(|e| format!("Pipe exchange failed: {e}"))?;
    serde_json::from_slice(&reply).map_err(|e| format!("Invalid pipe response: {e}"))
}

struct ClientChannel {
    name: String,
    desktop: ProcessIdentity,
}

pub(crate) struct NmhClient {
    verifier: PeerVerifier,
    // Registration/heartbeat and screenshot threads share an NMH. Serialize
    // their round-trips within this channel, without serializing other NMHs.
    channel: Mutex<Option<ClientChannel>>,
    registered: Arc<AtomicBool>,
}

impl NmhClient {
    pub(crate) fn new(registered: Arc<AtomicBool>) -> Result<Self, String> {
        Ok(Self {
            verifier: PeerVerifier::new(PeerRole::Desktop)?,
            channel: Mutex::new(None),
            registered,
        })
    }

    pub(crate) fn ensure_channel(&self, bootstrap: &str, auth_token: &str) -> Result<(), String> {
        let mut channel = self.channel.lock().unwrap_or_else(|e| e.into_inner());
        if channel
            .as_ref()
            .is_some_and(|endpoint| windows_platform::process_is_same(&endpoint.desktop))
        {
            return Ok(());
        }
        *channel = None;
        self.registered.store(false, Ordering::SeqCst);
        *channel = Some(self.negotiate(bootstrap, auth_token)?);
        Ok(())
    }

    fn negotiate(&self, bootstrap: &str, auth_token: &str) -> Result<ClientChannel, String> {
        let mut file = open_client(bootstrap).map_err(|e| e.to_string())?;
        let desktop = self
            .verifier
            .verify(HANDLE(file.as_raw_handle()), PipeSide::Client, None)?;
        // Peer identity is verified before even the bearer token is sent.
        let response = exchange(
            &mut file,
            &serde_json::json!({
                "command": "open_nmh_channel",
                "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                "auth_token": auth_token,
                "nmh_pid": std::process::id(),
            }),
        )?;
        if response["status"] != "success" {
            return Err(response["error"]
                .as_str()
                .unwrap_or("Channel negotiation failed")
                .into());
        }
        let data = &response["data"];
        let name = data["data_pipe_name"]
            .as_str()
            .ok_or("Missing NMH data pipe")?;
        if data["ipc_protocol_version"] != IPC_PROTOCOL_VERSION
            || nmh_protocol::request_pid(data, "nmh_pid") != Some(std::process::id())
            || !nmh_protocol::valid_private_pipe_name(name, nmh_protocol::DATA_PIPE_PREFIX)
        {
            return Err("Invalid NMH channel identity".into());
        }
        Ok(ClientChannel {
            name: name.to_string(),
            desktop,
        })
    }

    pub(crate) fn send(
        &self,
        bootstrap: &str,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let auth_token = request["auth_token"].as_str().ok_or("Missing auth token")?;
        let mut channel = self.channel.lock().unwrap_or_else(|e| e.into_inner());
        // Retry only failures before the request is sent. A lost response must
        // not cause a screenshot submission or session mutation to be replayed.
        for attempt in 0..2 {
            if channel.is_none() {
                self.registered.store(false, Ordering::SeqCst);
                *channel = Some(self.negotiate(bootstrap, auth_token)?);
            }
            let endpoint = channel.as_ref().unwrap();
            let connected = open_client(&endpoint.name)
                .map_err(|e| e.to_string())
                .and_then(|file| {
                    self.verifier.verify(
                        HANDLE(file.as_raw_handle()),
                        PipeSide::Client,
                        Some(&endpoint.desktop),
                    )?;
                    Ok(file)
                });
            match connected {
                Ok(mut file) => {
                    let response = exchange(&mut file, request);
                    // Update registration while this channel is still locked.
                    // A delayed caller must not mark an obsolete desktop epoch
                    // registered after another thread negotiated its replacement.
                    if request["command"] == "register_nmh" {
                        self.registered.store(
                            response
                                .as_ref()
                                .is_ok_and(|response| response["status"] == "success"),
                            Ordering::SeqCst,
                        );
                    }
                    if response.as_ref().is_err()
                        || response
                            .as_ref()
                            .is_ok_and(|response| response["error"] == "Authentication failed")
                    {
                        *channel = None;
                        self.registered.store(false, Ordering::SeqCst);
                    }
                    return response;
                }
                Err(error) => {
                    *channel = None;
                    self.registered.store(false, Ordering::SeqCst);
                    if attempt == 1 {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!()
    }

    #[cfg(test)]
    fn for_test(image: PathBuf) -> Self {
        Self {
            verifier: PeerVerifier::for_test(image),
            channel: Mutex::new(None),
            registered: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct NmhChannel {
    peer: ProcessIdentity,
    name: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for NmhChannel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) type RequestHandler = Arc<
    dyn Fn(
            ProcessIdentity,
            serde_json::Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = StorageResponse> + Send>>
        + Send
        + Sync,
>;

pub(crate) type CloseHandler = Arc<dyn Fn(&ProcessIdentity) + Send + Sync>;

struct ChannelCleanup {
    peer: ProcessIdentity,
    on_close: CloseHandler,
}

impl Drop for ChannelCleanup {
    fn drop(&mut self) {
        (self.on_close)(&self.peer);
    }
}

async fn handle_channel_request(
    server: &mut NamedPipeServer,
    peer: &ProcessIdentity,
    handler: &RequestHandler,
    expected_token: &str,
) {
    let response = match read_server_ipc_frame(server).await {
        Ok(bytes) if !bytes.is_empty() => match serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            Ok(request)
                if !nmh_protocol::constant_time_eq(
                    request["auth_token"].as_str().unwrap_or(""),
                    expected_token,
                ) =>
            {
                StorageResponse::error("Authentication failed")
            }
            Ok(request)
                if request["ipc_protocol_version"] != IPC_PROTOCOL_VERSION
                    || nmh_protocol::request_pid(&request, "nmh_pid") != Some(peer.pid) =>
            {
                StorageResponse::error("Invalid NMH client identity")
            }
            Ok(request) => handler(peer.clone(), request).await,
            Err(e) => StorageResponse::error(&format!("Invalid NMH JSON: {e}")),
        },
        Ok(_) => return,
        Err(e) => StorageResponse::error(&e),
    };
    send_nmh_response(server, response).await;
}
async fn send_nmh_response(server: &mut NamedPipeServer, response: StorageResponse) {
    use tokio::io::AsyncReadExt;
    let bytes = serde_json::to_vec(&response).unwrap_or_default();
    let _ = tokio::time::timeout(IO_TIMEOUT, async {
        write_ipc_frame(server, &bytes).await?;
        let mut ack = [0u8];
        server
            .read_exact(&mut ack)
            .await
            .map_err(|e| e.to_string())?;
        if ack[0] != RESPONSE_ACK {
            return Err("Invalid NMH response acknowledgement".into());
        }
        Ok::<(), String>(())
    })
    .await;
}

pub(crate) async fn serve_broker(
    mut server: NamedPipeServer,
    verifier: PeerVerifier,
    mut shutdown_rx: mpsc::Receiver<()>,
    token: String,
    handler: RequestHandler,
    on_close: CloseHandler,
) {
    let mut channels: HashMap<u32, NmhChannel> = HashMap::new();
    let mut prune = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            _ = prune.tick() => {
                channels.retain(|_, channel| !channel.task.is_finished()
                    && windows_platform::process_is_same(&channel.peer));
                continue;
            }
            connected = server.connect() => {
                if let Err(e) = connected { tracing::warn!("NMH bootstrap connection failed: {e}"); break; }
            }
        }
        let exchange = async {
            let peer = verifier.inspect(HANDLE(server.as_raw_handle()), PipeSide::Server)?;
            let already_verified = channels
                .get(&peer.pid)
                .is_some_and(|channel| channel.peer == peer && !channel.task.is_finished());
            if !already_verified {
                verifier.verify_identity(&peer)?;
            }
            let bytes = read_server_ipc_frame(&mut server).await?;
            let request: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if !constant_time_eq(request["auth_token"].as_str().unwrap_or(""), &token) {
                send_nmh_response(&mut server, StorageResponse::error("Authentication failed"))
                    .await;
                return Ok::<(), String>(());
            }
            if nmh_protocol::request_pid(&request, "nmh_pid") != Some(peer.pid) {
                send_nmh_response(
                    &mut server,
                    StorageResponse::error("Invalid NMH client identity"),
                )
                .await;
                return Ok(());
            }
            // A host from before the channel protocol cannot safely keep sending
            // bearer tokens/screenshots to its old public endpoint. Restart only
            // this kernel-identified, authenticated host. The extension's existing
            // native-port disconnect handler reconnects to the refreshed manifest.
            if request["ipc_protocol_version"] == 2
                && matches!(
                    request["command"].as_str(),
                    Some("register_nmh" | "unregister_nmh" | "save_extension_screenshot")
                )
            {
                windows_platform::terminate_process_if_same(&peer)?;
                return Ok(());
            }
            if request["ipc_protocol_version"] != IPC_PROTOCOL_VERSION
                || request["command"] != "open_nmh_channel"
            {
                send_nmh_response(
                    &mut server,
                    StorageResponse::error("Unsupported NMH channel protocol"),
                )
                .await;
                return Ok(());
            }
            if !already_verified {
                let name = format!(
                    "{}{}",
                    nmh_protocol::DATA_PIPE_PREFIX,
                    hex::encode(rand::random::<[u8; 16]>())
                );
                let private =
                    async_server(create_server_file(&name, true).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                let task = tokio::spawn(run_channel(
                    private,
                    peer.clone(),
                    handler.clone(),
                    on_close.clone(),
                    token.clone(),
                ));
                channels.insert(
                    peer.pid,
                    NmhChannel {
                        peer: peer.clone(),
                        name,
                        task,
                    },
                );
            }
            let channel = channels.get(&peer.pid).unwrap();
            send_nmh_response(
                &mut server,
                StorageResponse::success(serde_json::json!({
                    "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                    "nmh_pid": peer.pid,
                    "data_pipe_name": channel.name,
                })),
            )
            .await;
            Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), exchange).await {
            Ok(Err(e)) => tracing::warn!("NMH bootstrap request rejected: {e}"),
            Err(_) => tracing::warn!("NMH bootstrap exchange timed out"),
            _ => {}
        }
        if let Err(e) = server.disconnect() {
            tracing::warn!("Cannot reuse NMH bootstrap pipe: {e}");
            break;
        }
    }
    // Dropping each entry aborts its task and closes its pinned server handle.
    drop(channels);
}

async fn run_channel(
    mut server: NamedPipeServer,
    peer: ProcessIdentity,
    handler: RequestHandler,
    on_close: CloseHandler,
    token: String,
) {
    let _cleanup = ChannelCleanup {
        peer: peer.clone(),
        on_close,
    };
    let verifier = match PeerVerifier::new(PeerRole::NativeHost) {
        Ok(verifier) => verifier,
        Err(e) => {
            tracing::warn!("Cannot verify NMH channel: {e}");
            return;
        }
    };
    let mut liveness = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = liveness.tick() => {
                if !windows_platform::process_is_same(&peer) { break; }
                continue;
            }
            connected = server.connect() => {
                if let Err(e) = connected { tracing::warn!("NMH data connection failed: {e}"); break; }
            }
        }
        let verified = verifier.verify(
            HANDLE(server.as_raw_handle()),
            PipeSide::Server,
            Some(&peer),
        );
        if verified.is_ok() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                handle_channel_request(&mut server, &peer, &handler, &token),
            )
            .await;
        }
        if let Err(e) = server.disconnect() {
            tracing::warn!("Cannot reuse NMH data pipe: {e}");
            break;
        }
    }
}
