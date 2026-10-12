//! CarbonPaper Native Messaging Host (NMH)
//!
//! Standalone binary launched by the browser via Chrome Native Messaging protocol.
//! Receives viewport screenshots + metadata from the browser extension,
//! relays them to CarbonPaper via the deterministic Named Pipe.
//!
//! Also listens on a command pipe so CarbonPaper can push capture requests
//! to the extension (forwarded via stdout NM protocol).
//!
//! Protocol:
//!   - stdin/stdout: Chrome NM protocol (4-byte LE length prefix + JSON)
//!   - Named Pipe (data): v2 frame (4-byte LE length prefix + JSON)
//!   - Named Pipe (cmd):  CarbonPaper connects here to request captures

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[path = "../nmh_protocol.rs"]
mod nmh_protocol;
#[cfg(windows)]
#[path = "../windows_platform.rs"]
mod windows_platform;
use nmh_protocol::IPC_PROTOCOL_VERSION;

/// The browser main process that (indirectly) spawned this NMH.
#[derive(Clone, Debug)]
struct BrowserInfo {
    pid: u32,
    exe_name: String,
    exe_path: String,
}

/// Shared per-process context for message handling and registration.
struct NmhContext {
    browser: Option<BrowserInfo>,
    cmd_pipe_name: String,
    registered: Arc<AtomicBool>,
}

fn main() {
    // Compute deterministic pipe name
    let pipe_name = match compute_nmh_pipe_name() {
        Ok(name) => name,
        Err(e) => {
            send_nm_response_raw(&serde_json::json!({
                "status": "error",
                "error": format!("Failed to compute pipe name: {}", e)
            }));
            return;
        }
    };

    // Create shared stdout mutex for thread-safe NM writes
    let stdout_mutex: Arc<Mutex<io::Stdout>> = Arc::new(Mutex::new(io::stdout()));

    // Detect the ancestor browser main process (PID + exe). May be None if
    // the process-tree walk fails; the data path still works in that case,
    // we just can't register a command pipe for capture requests.
    let browser = detect_browser();
    // Random-suffix command pipe name: unique per NMH instance, so multiple
    // browsers (or profiles) never contend for the same pipe.
    let cmd_pipe_name = generate_cmd_pipe_name();

    // Send startup diagnostic so the browser console shows the detected values
    send_nm_response(
        &stdout_mutex,
        &serde_json::json!({
            "type": "nmh_ready",
            "browser_exe": browser.as_ref().map(|b| b.exe_name.clone()),
            "browser_pid": browser.as_ref().map(|b| b.pid),
            "browser_exe_path": browser.as_ref().map(|b| b.exe_path.clone()),
            "cmd_pipe": cmd_pipe_name,
            "data_pipe": pipe_name,
        }),
    );

    let data_dir = get_data_dir();
    let auth_token_path = data_dir.join("nmh_auth_token");

    let registered = Arc::new(AtomicBool::new(false));

    if let Some(b) = &browser {
        // Command pipe + registration only make sense when we know which
        // browser we belong to (the main app routes by browser PID).
        start_command_pipe_thread(stdout_mutex.clone(), cmd_pipe_name.clone());
        start_registration_thread(
            pipe_name.clone(),
            cmd_pipe_name.clone(),
            b.clone(),
            auth_token_path.clone(),
            registered.clone(),
            stdout_mutex.clone(),
        );
    } else {
        send_nm_response(
            &stdout_mutex,
            &serde_json::json!({
                "type": "nmh_registration_failed",
                "error": "Could not detect ancestor browser process; capture requests disabled (screenshot relay still works)"
            }),
        );
    }

    let ctx = NmhContext {
        browser,
        cmd_pipe_name: cmd_pipe_name.clone(),
        registered,
    };

    // Auth token is read fresh from disk on every message.  This is
    // intentional: the main app regenerates the token on each startup, so a
    // cached value would go stale whenever CarbonPaper restarts while the
    // NMH process is still alive.  The file is tiny and local, so the I/O
    // cost is negligible.

    // Main stdin read loop
    loop {
        match read_nm_message() {
            Ok(None) => break, // stdin closed
            Ok(Some(msg)) => {
                // Read auth token fresh each time
                let auth_token = read_auth_token(&auth_token_path);

                let response = match auth_token {
                    Some(token) => handle_message(msg, &pipe_name, &token, &ctx),
                    None => serde_json::json!({
                        "status": "error",
                        "error": "CarbonPaper not running yet"
                    }),
                };
                send_nm_response(&stdout_mutex, &response);
            }
            Err(e) => {
                send_nm_response(
                    &stdout_mutex,
                    &serde_json::json!({
                        "status": "error",
                        "error": format!("Read error: {}", e)
                    }),
                );
                break;
            }
        }
    }

    // stdin closed: the browser is shutting down or the extension service
    // worker got suspended. Best-effort unregister — the main app's liveness
    // pruning is the authoritative cleanup if this doesn't go through.
    if ctx.registered.load(Ordering::SeqCst) {
        if let Some(token) = read_auth_token(&auth_token_path) {
            let _ = send_to_pipe(
                &pipe_name,
                &build_unregister_request(&token, &cmd_pipe_name),
            );
        }
    }
}

/// Read the NMH auth token file, if present and non-empty.
fn read_auth_token(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Generate a random-suffix command pipe name, unique per NMH instance.
fn generate_cmd_pipe_name() -> String {
    let bytes: [u8; 16] = rand::random();
    format!(r"\\.\pipe\carbon_nmh_cmd_r_{}", hex::encode(bytes))
}

/// Build the register_nmh request sent over the data pipe.
fn build_register_request(
    auth_token: &str,
    cmd_pipe_name: &str,
    browser: &BrowserInfo,
) -> serde_json::Value {
    serde_json::json!({
        "command": "register_nmh",
        "ipc_protocol_version": IPC_PROTOCOL_VERSION,
        "auth_token": auth_token,
        "browser_pid": browser.pid,
        "browser_exe_path": browser.exe_path,
        "browser_exe_name": browser.exe_name,
        "nmh_pid": std::process::id(),
        "cmd_pipe_name": cmd_pipe_name,
    })
}

/// Build the unregister_nmh request sent over the data pipe.
fn build_unregister_request(auth_token: &str, cmd_pipe_name: &str) -> serde_json::Value {
    serde_json::json!({
        "command": "unregister_nmh",
        "ipc_protocol_version": IPC_PROTOCOL_VERSION,
        "auth_token": auth_token,
        "nmh_pid": std::process::id(),
        "cmd_pipe_name": cmd_pipe_name,
    })
}

/// Background thread that registers this NMH's session with the main app and
/// keeps the registration alive.
///
/// - While unregistered: retry every 3s (covers CarbonPaper cold start — the
///   auth token file appears when the app launches).
/// - While registered: re-register every 60s as a heartbeat. The main app
///   wipes its session table and rotates the token on restart, so the
///   heartbeat restores the capture route without waiting for a screenshot.
///
/// Registration state transitions are reported to the extension console.
fn start_registration_thread(
    data_pipe_name: String,
    cmd_pipe_name: String,
    browser: BrowserInfo,
    auth_token_path: PathBuf,
    registered: Arc<AtomicBool>,
    stdout_mutex: Arc<Mutex<io::Stdout>>,
) {
    std::thread::spawn(move || {
        let mut last_error: Option<String> = None;
        loop {
            let was_registered = registered.load(Ordering::SeqCst);

            let result = match read_auth_token(&auth_token_path) {
                Some(token) => {
                    let req = build_register_request(&token, &cmd_pipe_name, &browser);
                    let resp = send_to_pipe(&data_pipe_name, &req);
                    if resp.get("status").and_then(|s| s.as_str()) == Some("success") {
                        Ok(())
                    } else {
                        Err(resp
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown error")
                            .to_string())
                    }
                }
                None => Err("CarbonPaper not running yet (no auth token)".to_string()),
            };

            match result {
                Ok(()) => {
                    registered.store(true, Ordering::SeqCst);
                    if !was_registered {
                        last_error = None;
                        send_nm_response(
                            &stdout_mutex,
                            &serde_json::json!({
                                "type": "nmh_registered",
                                "browser_exe": browser.exe_name,
                                "browser_pid": browser.pid,
                                "cmd_pipe": cmd_pipe_name,
                            }),
                        );
                    }
                }
                Err(e) => {
                    registered.store(false, Ordering::SeqCst);
                    // Report each distinct failure once to avoid console spam
                    // (cold-start "not running" errors repeat every 3s).
                    if last_error.as_deref() != Some(e.as_str()) {
                        send_nm_response(
                            &stdout_mutex,
                            &serde_json::json!({
                                "type": "nmh_registration_failed",
                                "error": e,
                            }),
                        );
                        last_error = Some(e);
                    }
                }
            }

            let delay = if registered.load(Ordering::SeqCst) {
                std::time::Duration::from_secs(60)
            } else {
                std::time::Duration::from_secs(3)
            };
            std::thread::sleep(delay);
        }
    });
}

/// Read a single NM message from stdin (4-byte LE length + JSON)
fn read_nm_message() -> io::Result<Option<serde_json::Value>> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    // Read 4-byte length prefix
    let mut len_buf = [0u8; 4];
    match handle.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > 10 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Message too large",
        ));
    }

    let mut buf = vec![0u8; len];
    handle.read_exact(&mut buf)?;

    let value: serde_json::Value =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(Some(value))
}

/// Send a NM response to stdout via shared mutex (4-byte LE length + JSON)
fn send_nm_response(stdout_mutex: &Arc<Mutex<io::Stdout>>, value: &serde_json::Value) {
    let data = serde_json::to_vec(value).unwrap_or_default();
    let len = (data.len() as u32).to_le_bytes();

    let mut handle = stdout_mutex.lock().unwrap_or_else(|e| e.into_inner());
    let _ = handle.write_all(&len);
    let _ = handle.write_all(&data);
    let _ = handle.flush();
}

/// Send a NM response directly to stdout (used before mutex is created)
fn send_nm_response_raw(value: &serde_json::Value) {
    let data = serde_json::to_vec(value).unwrap_or_default();
    let len = (data.len() as u32).to_le_bytes();

    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(&len);
    let _ = handle.write_all(&data);
    let _ = handle.flush();
}

/// Handle a single message from the extension.
/// The extension sends either a single complete message or chunked messages.
fn handle_message(
    msg: serde_json::Value,
    pipe_name: &str,
    auth_token: &str,
    ctx: &NmhContext,
) -> serde_json::Value {
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "save_screenshot" => {
            // Complete message with all data
            let image_data = match msg.get("image_data").and_then(|v| v.as_str()) {
                Some(d) => d,
                None => {
                    return serde_json::json!({"status": "error", "error": "Missing image_data"})
                }
            };
            let image_hash = match msg.get("image_hash").and_then(|v| v.as_str()) {
                Some(h) => h,
                None => {
                    return serde_json::json!({"status": "error", "error": "Missing image_hash"})
                }
            };

            // If the session isn't registered yet (app just started, or it
            // restarted and wiped the table), register before saving so the
            // capture route comes back together with the data path.
            if !ctx.registered.load(Ordering::SeqCst) {
                if let Some(browser) = &ctx.browser {
                    let req = build_register_request(auth_token, &ctx.cmd_pipe_name, browser);
                    let resp = send_to_pipe(pipe_name, &req);
                    if resp.get("status").and_then(|s| s.as_str()) == Some("success") {
                        ctx.registered.store(true, Ordering::SeqCst);
                    }
                }
            }

            // The browser identity comes from the NMH's own process-tree
            // detection — the extension's UA-sniffed value reports every
            // Chromium fork as "chrome.exe", so it's only a fallback.
            let browser_name = ctx
                .browser
                .as_ref()
                .map(|b| b.exe_name.clone())
                .or_else(|| {
                    msg.get("browser_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "browser-extension".to_string());

            let pipe_request = serde_json::json!({
                "command": "save_extension_screenshot",
                "ipc_protocol_version": IPC_PROTOCOL_VERSION,
                "auth_token": auth_token,
                "image_data": image_data,
                "image_hash": image_hash,
                "width": msg.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                "height": msg.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
                "page_url": msg.get("page_url").and_then(|v| v.as_str()).unwrap_or(""),
                "page_title": msg.get("page_title").and_then(|v| v.as_str()).unwrap_or(""),
                "page_icon": msg.get("page_icon").and_then(|v| v.as_str()),
                "visible_links": msg.get("visible_links"),
                "browser_name": browser_name,
                "nmh_pid": std::process::id(),
            });

            let response = send_to_pipe(pipe_name, &pipe_request);

            // App restart rotates the token — mark unregistered so the next
            // save (or the heartbeat) re-registers with the fresh token.
            if response.get("status").and_then(|s| s.as_str()) == Some("error") {
                let err = response.get("error").and_then(|e| e.as_str()).unwrap_or("");
                if err.contains("Authentication failed") || err.contains("Cannot connect") {
                    ctx.registered.store(false, Ordering::SeqCst);
                }
            }

            response
        }
        "ping" => {
            serde_json::json!({"status": "ok", "type": "pong"})
        }
        _ => {
            serde_json::json!({"status": "error", "error": format!("Unknown message type: {}", msg_type)})
        }
    }
}

fn write_pipe_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    writer.write_all(&nmh_protocol::frame_prefix(body.len())?)?;
    writer.write_all(body)
}

fn read_pipe_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix)?;
    let mut body = vec![0u8; nmh_protocol::frame_length(prefix)?];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn send_to_pipe(pipe_name: &str, request: &serde_json::Value) -> serde_json::Value {
    use std::fs::OpenOptions;

    let data = match serde_json::to_vec(request) {
        Ok(d) => d,
        Err(e) => {
            return serde_json::json!({"status": "error", "error": format!("Serialization failed: {}", e)})
        }
    };

    // Open the named pipe as a file (Windows named pipes can be opened as files)
    let mut pipe = match OpenOptions::new().read(true).write(true).open(pipe_name) {
        Ok(p) => p,
        Err(e) => {
            return serde_json::json!({
                "status": "error",
                "error": format!("Cannot connect to CarbonPaper (pipe {}): {}", pipe_name, e)
            })
        }
    };

    // Write v2 framed request.
    if let Err(e) = write_pipe_frame(&mut pipe, &data) {
        return serde_json::json!({"status": "error", "error": format!("Pipe write failed: {}", e)});
    }

    if let Err(e) = pipe.flush() {
        return serde_json::json!({"status": "error", "error": format!("Pipe flush failed: {}", e)});
    }

    let response_buf = match read_pipe_frame(&mut pipe) {
        Ok(buf) => buf,
        Err(e) => {
            return serde_json::json!({"status": "error", "error": format!("Pipe read failed: {}", e)})
        }
    };

    match serde_json::from_slice(&response_buf) {
        Ok(v) => v,
        Err(e) => {
            serde_json::json!({"status": "error", "error": format!("Invalid response JSON: {}", e)})
        }
    }
}

fn compute_nmh_pipe_name() -> Result<String, String> {
    Ok(nmh_protocol::pipe_name(&get_current_user_sid()?))
}

/// Windows Chromium browsers launch native messaging hosts through
/// `cmd.exe /c` so stdin/stdout can be redirected to the browser pipes.
fn is_native_messaging_launcher_wrapper(exe_name: &str) -> bool {
    exe_name.eq_ignore_ascii_case("cmd.exe") || exe_name.eq_ignore_ascii_case("cmd")
}

/// Select the browser main process from an ancestor chain ordered nearest to
/// farthest from the NMH process.
fn select_browser_ancestor(ancestors: &[(u32, String)]) -> Option<(u32, String)> {
    let first_browser_index = ancestors
        .iter()
        .position(|(_, name)| !name.is_empty() && !is_native_messaging_launcher_wrapper(name))?;
    let (first_pid, browser_name) = ancestors.get(first_browser_index)?.clone();

    // Chromium subprocesses normally share the browser executable name. Use
    // the topmost consecutive process with that name as the browser main PID.
    let mut main_pid = first_pid;
    for (ancestor_pid, ancestor_name) in ancestors.iter().skip(first_browser_index + 1) {
        if ancestor_name.eq_ignore_ascii_case(&browser_name) {
            main_pid = *ancestor_pid;
        } else {
            break;
        }
    }

    Some((main_pid, browser_name))
}

/// Detect the browser main process that spawned this NMH by walking up the
/// process tree. Returns the browser's PID, exe name, and full exe path.
///
/// After skipping the Native Messaging `cmd.exe` launcher, take the topmost
/// consecutive ancestor with the same executable name. This remains browser
/// name agnostic and therefore supports Chromium forks without a name list.
fn detect_browser() -> Option<BrowserInfo> {
    #[cfg(windows)]
    {
        let process_map = windows_platform::process_tree()?;
        {
            let current_pid = std::process::id();
            // Collect the ancestor chain (excluding this NMH process itself)
            let mut ancestors: Vec<(u32, String)> = Vec::new();
            let mut pid = current_pid;
            for _ in 0..16 {
                let (parent_pid, _) = match process_map.get(&pid) {
                    Some(entry) => entry,
                    None => break,
                };
                if *parent_pid == 0 || *parent_pid == pid {
                    break;
                }
                match process_map.get(parent_pid) {
                    Some((_, parent_name)) => {
                        ancestors.push((*parent_pid, parent_name.clone()));
                        pid = *parent_pid;
                    }
                    None => break,
                }
            }

            let (main_pid, browser_name) = select_browser_ancestor(&ancestors)?;

            let exe_path = query_process_image_path(main_pid).unwrap_or_default();
            Some(BrowserInfo {
                pid: main_pid,
                exe_name: browser_name,
                exe_path,
            })
        }
    }

    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
use windows_platform::process_image_path as query_process_image_path;

/// Start the command pipe thread that listens for capture requests from the main app.
/// When a `request_capture` command arrives, it forwards a `capture_request` message
/// to the extension via stdout (NM protocol).
fn start_command_pipe_thread(stdout_mutex: Arc<Mutex<io::Stdout>>, cmd_pipe_name: String) {
    std::thread::spawn(move || {
        run_command_pipe_server(&cmd_pipe_name, &stdout_mutex);
    });
}

/// Upper bound for a single command-pipe read or write. The pipe has one
/// instance, so a client that connects but never completes its I/O would
/// otherwise block every future capture request.
#[cfg(windows)]
const CMD_PIPE_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run a blocking command-pipe I/O operation, cancelling the pending I/O via
/// `CancelIoEx` if it does not complete within `timeout`.
///
/// The caller must keep the pipe handle open until this function returns (the
/// watchdog thread is joined before returning, so the handle stays valid for
/// the cancel call). Cancelling when no I/O is pending is a harmless no-op.
#[cfg(windows)]
fn with_pipe_io_timeout<T>(
    pipe: windows::Win32::Foundation::HANDLE,
    timeout: std::time::Duration,
    op: impl FnOnce() -> T,
) -> T {
    let pipe_addr = pipe.0 as usize;
    // The scoped watchdog joins on both normal return and unwind, before the
    // caller can release the pipe's RAII handle.
    std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            if done_rx.recv_timeout(timeout).is_err() {
                // SAFETY: the caller's pipe outlives this scope; no OVERLAPPED
                // pointer asks Windows to cancel all pending I/O on the handle.
                unsafe {
                    let _ = windows::Win32::System::IO::CancelIoEx(
                        windows::Win32::Foundation::HANDLE(pipe_addr as *mut _),
                        None,
                    );
                }
            }
        });
        let result = op();
        let _ = done_tx.send(());
        result
    })
}

/// Run the synchronous command pipe server loop using Win32 API.
#[cfg(windows)]
fn run_command_pipe_server(pipe_name: &str, stdout_mutex: &Arc<Mutex<io::Stdout>>) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED};
    use windows::Win32::Storage::FileSystem::{
        FlushFileBuffers, ReadFile, WriteFile, PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut create_failure_reported = false;
    loop {
        // SAFETY: the pipe name is NUL-terminated and live. Default security
        // attributes retain the host's existing access policy.
        let raw = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        if raw.is_invalid() {
            if !create_failure_reported {
                create_failure_reported = true;
                // SAFETY: read the error from the immediately preceding Win32 call.
                let error = unsafe { GetLastError() }.0;
                send_nm_response(
                    stdout_mutex,
                    &serde_json::json!({
                        "type": "nmh_registration_failed",
                        "error": format!("Failed to create command pipe (Win32 error {error}); capture requests unavailable")
                    }),
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        let pipe = windows_platform::OwnedHandle(raw);
        create_failure_reported = false;
        // SAFETY: the owned handle is live; no OVERLAPPED selects blocking I/O.
        if let Err(error) = unsafe { ConnectNamedPipe(pipe.0, None) } {
            if error.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        }

        let mut buffer = [0u8; 4096];
        let mut bytes_read = 0u32;
        // SAFETY: the connected pipe and writable output buffers outlive the I/O.
        let read = with_pipe_io_timeout(pipe.0, CMD_PIPE_IO_TIMEOUT, || unsafe {
            ReadFile(pipe.0, Some(&mut buffer), Some(&mut bytes_read), None)
        });
        if read.is_ok() && bytes_read > 0 {
            if let Ok(request) =
                serde_json::from_slice::<serde_json::Value>(&buffer[..bytes_read as usize])
            {
                if request.get("command").and_then(|value| value.as_str())
                    == Some("request_capture")
                {
                    let data = serde_json::to_vec(&serde_json::json!({"type": "capture_request"}))
                        .unwrap_or_default();
                    let forwarded = {
                        let mut stdout = stdout_mutex
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        stdout
                            .write_all(&(data.len() as u32).to_le_bytes())
                            .and_then(|_| stdout.write_all(&data))
                            .and_then(|_| stdout.flush())
                    };
                    let response = match forwarded {
                        Ok(()) => serde_json::json!({"status": "ok", "forwarded": true}),
                        Err(error) => serde_json::json!({
                            "status": "error",
                            "error": format!("Failed to forward capture request to extension: {error}")
                        }),
                    };
                    let bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let mut written = 0u32;
                    // SAFETY: pipe and bytes remain live until the scoped I/O
                    // watchdog joins; flushing is bounded by the same timeout.
                    with_pipe_io_timeout(pipe.0, CMD_PIPE_IO_TIMEOUT, || unsafe {
                        let _ = WriteFile(pipe.0, Some(&bytes), Some(&mut written), None);
                        let _ = FlushFileBuffers(pipe.0);
                    });
                }
            }
        }
        // SAFETY: disconnect before the owned handle drops; the watchdog has joined.
        unsafe {
            let _ = DisconnectNamedPipe(pipe.0);
        }
    }
}

#[cfg(not(windows))]
fn run_command_pipe_server(_pipe_name: &str, _stdout_mutex: &Arc<Mutex<io::Stdout>>) {
    // NMH command pipe is only supported on Windows
}

fn get_current_user_sid() -> Result<String, String> {
    #[cfg(windows)]
    {
        windows_platform::current_user_sid()
    }
    #[cfg(not(windows))]
    {
        Err("NMH is only supported on Windows".into())
    }
}

/// Get the CarbonPaper data directory
fn get_data_dir() -> PathBuf {
    // Check registry first
    #[cfg(windows)]
    {
        use winreg::enums::*;
        use winreg::RegKey;
        if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\CarbonPaper") {
            if let Ok(dir) = hkcu.get_value::<String, _>("data_dir") {
                return PathBuf::from(dir);
            }
        }
    }

    // Fallback to default
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(local_appdata)
        .join("CarbonPaper")
        .join("data")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nmh_protocol::IPC_MAX_MESSAGE_BYTES;

    #[test]
    fn test_generate_cmd_pipe_name_format() {
        let name = generate_cmd_pipe_name();
        assert!(
            name.starts_with(r"\\.\pipe\carbon_nmh_cmd_r_"),
            "unexpected pipe name: {}",
            name
        );
        // 16 random bytes -> 32 hex chars
        let suffix = name.trim_start_matches(r"\\.\pipe\carbon_nmh_cmd_r_");
        assert_eq!(suffix.len(), 32);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_cmd_pipe_name_unique() {
        assert_ne!(generate_cmd_pipe_name(), generate_cmd_pipe_name());
    }

    #[test]
    fn test_select_browser_ancestor_skips_native_messaging_cmd_wrapper() {
        let ancestors = vec![
            (100, "cmd.exe".to_string()),
            (200, "msedge.exe".to_string()),
        ];
        assert_eq!(
            select_browser_ancestor(&ancestors),
            Some((200, "msedge.exe".to_string()))
        );
    }

    #[test]
    fn test_select_browser_ancestor_uses_topmost_consecutive_browser_process() {
        let ancestors = vec![
            (100, "cmd.exe".to_string()),
            (200, "chrome.exe".to_string()),
            (300, "chrome.exe".to_string()),
            (400, "explorer.exe".to_string()),
        ];
        assert_eq!(
            select_browser_ancestor(&ancestors),
            Some((300, "chrome.exe".to_string()))
        );
    }

    #[test]
    fn test_select_browser_ancestor_rejects_wrapper_only_chain() {
        let ancestors = vec![(100, "cmd.exe".to_string())];
        assert_eq!(select_browser_ancestor(&ancestors), None);
    }

    #[test]
    fn test_build_register_request_shape() {
        let browser = BrowserInfo {
            pid: 12345,
            exe_name: "360chromex.exe".to_string(),
            exe_path: r"C:\Program Files\360\360chromex.exe".to_string(),
        };
        let req = build_register_request("tok", r"\\.\pipe\carbon_nmh_cmd_r_abc", &browser);
        assert_eq!(req["command"], "register_nmh");
        assert_eq!(req["ipc_protocol_version"], IPC_PROTOCOL_VERSION);
        assert_eq!(req["auth_token"], "tok");
        assert_eq!(req["browser_pid"], 12345);
        assert_eq!(req["browser_exe_name"], "360chromex.exe");
        assert_eq!(req["cmd_pipe_name"], r"\\.\pipe\carbon_nmh_cmd_r_abc");
        assert!(req["nmh_pid"].is_number());
    }

    #[test]
    fn test_build_unregister_request_shape() {
        let req = build_unregister_request("tok", r"\\.\pipe\carbon_nmh_cmd_r_abc");
        assert_eq!(req["command"], "unregister_nmh");
        assert_eq!(req["auth_token"], "tok");
        assert_eq!(req["cmd_pipe_name"], r"\\.\pipe\carbon_nmh_cmd_r_abc");
        assert!(req["nmh_pid"].is_number());
    }

    #[test]
    fn test_read_pipe_frame_rejects_frames_above_limit() {
        let oversized = ((IPC_MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes();
        let error = read_pipe_frame(&mut oversized.as_slice()).unwrap_err();
        assert!(error.kind() == std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_write_pipe_frame_rejects_frames_above_limit() {
        let body = vec![0u8; IPC_MAX_MESSAGE_BYTES + 1];
        let error = write_pipe_frame(&mut Vec::new(), &body).unwrap_err();
        assert!(error.kind() == std::io::ErrorKind::InvalidData);
    }
}
