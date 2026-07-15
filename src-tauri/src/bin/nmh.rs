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

use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const IPC_PROTOCOL_VERSION: u32 = 2;
const IPC_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

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

fn write_pipe_frame<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > IPC_MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "IPC request too large: {} bytes (max {})",
                body.len(),
                IPC_MAX_MESSAGE_BYTES
            ),
        ));
    }

    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(body)
}

fn read_pipe_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;

    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > IPC_MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Invalid IPC v{} frame length: {} (max {})",
                IPC_PROTOCOL_VERSION, len, IPC_MAX_MESSAGE_BYTES
            ),
        ));
    }

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// Send a JSON request to the Named Pipe and read the response
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

/// Compute deterministic NMH pipe name from current user's Windows SID.
/// Must match the formula in reverse_ipc.rs.
fn compute_nmh_pipe_name() -> Result<String, String> {
    let sid = get_current_user_sid()?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{}carbonpaper_nmh_salt", sid));
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);
    Ok(format!(r"\\.\pipe\carbon_nmh_{}", &hex_hash[..16]))
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
        use std::collections::HashMap;
        use std::mem;

        extern "system" {
            fn GetCurrentProcessId() -> u32;
        }

        extern "system" {
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
            fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W)
                -> i32;
            fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }

        const TH32CS_SNAPPROCESS: u32 = 0x00000002;
        const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

        #[repr(C)]
        struct ProcessEntry32W {
            dw_size: u32,
            cnt_usage: u32,
            th32_process_id: u32,
            th32_default_heap_id: usize,
            th32_module_id: u32,
            cnt_threads: u32,
            th32_parent_process_id: u32,
            pc_pri_class_base: i32,
            dw_flags: u32,
            sz_exe_file: [u16; 260],
        }

        // SAFETY: the Toolhelp snapshot is checked before use, the local structure layout
        // matches PROCESSENTRY32W and has `dw_size` initialized, and the snapshot handle
        // is closed after synchronous enumeration.
        unsafe {
            let current_pid = GetCurrentProcessId();
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return None;
            }

            // Build a map of pid -> (parent_pid, exe_name) for all processes
            let mut process_map: HashMap<u32, (u32, String)> = HashMap::new();
            let mut entry: ProcessEntry32W = mem::zeroed();
            entry.dw_size = mem::size_of::<ProcessEntry32W>() as u32;

            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    let len = entry
                        .sz_exe_file
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(260);
                    let name = String::from_utf16_lossy(&entry.sz_exe_file[..len]).to_lowercase();
                    process_map.insert(entry.th32_process_id, (entry.th32_parent_process_id, name));

                    entry = mem::zeroed();
                    entry.dw_size = mem::size_of::<ProcessEntry32W>() as u32;
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }

            CloseHandle(snapshot);

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

/// Query the full executable path of a process by PID.
#[cfg(windows)]
fn query_process_image_path(pid: u32) -> Option<String> {
    use std::ffi::c_void;

    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, pid: u32) -> *mut c_void;
        fn QueryFullProcessImageNameW(
            process: *mut c_void,
            flags: u32,
            exe_name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    // SAFETY: the process handle is checked and owned locally; the UTF-16 output buffer
    // and size pointer are valid for the synchronous query, then the handle is closed.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok != 0 && size > 0 {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

/// Start the command pipe thread that listens for capture requests from the main app.
/// When a `request_capture` command arrives, it forwards a `capture_request` message
/// to the extension via stdout (NM protocol).
fn start_command_pipe_thread(stdout_mutex: Arc<Mutex<io::Stdout>>, cmd_pipe_name: String) {
    std::thread::spawn(move || {
        run_command_pipe_server(&cmd_pipe_name, &stdout_mutex);
    });
}

/// Run the synchronous command pipe server loop using Win32 API.
#[cfg(windows)]
fn run_command_pipe_server(pipe_name: &str, stdout_mutex: &Arc<Mutex<io::Stdout>>) {
    use std::ffi::c_void;
    use std::ptr;

    // Win32 Named Pipe API
    extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout: u32,
            security_attributes: *const c_void,
        ) -> *mut c_void;
        fn ConnectNamedPipe(pipe: *mut c_void, overlapped: *const c_void) -> i32;
        fn ReadFile(
            file: *mut c_void,
            buffer: *mut u8,
            bytes_to_read: u32,
            bytes_read: *mut u32,
            overlapped: *const c_void,
        ) -> i32;
        fn WriteFile(
            file: *mut c_void,
            buffer: *const u8,
            bytes_to_write: u32,
            bytes_written: *mut u32,
            overlapped: *const c_void,
        ) -> i32;
        fn FlushFileBuffers(file: *mut c_void) -> i32;
        fn DisconnectNamedPipe(pipe: *mut c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const PIPE_ACCESS_DUPLEX: u32 = 0x00000003;
    const PIPE_TYPE_BYTE: u32 = 0x00000000;
    const PIPE_READMODE_BYTE: u32 = 0x00000000;
    const PIPE_WAIT: u32 = 0x00000000;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
    const ERROR_PIPE_CONNECTED: u32 = 535;

    let wide_name: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut create_failure_reported = false;

    loop {
        // Create a new pipe instance
        // SAFETY: `wide_name` is NUL-terminated and remains live; null security attributes
        // request Windows defaults, and the returned handle is owned by this loop.
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,    // max instances
                4096, // out buffer
                4096, // in buffer
                0,    // default timeout
                ptr::null(),
            )
        };

        if pipe == INVALID_HANDLE_VALUE || pipe.is_null() {
            // The name is random per NMH instance, so this is a systemic
            // failure, not contention. Report it once so it's visible in
            // the extension console instead of silently retrying forever.
            if !create_failure_reported {
                create_failure_reported = true;
                // SAFETY: `GetLastError` reads thread-local Win32 state without pointers.
                let err = unsafe { GetLastError() };
                send_nm_response(
                    stdout_mutex,
                    &serde_json::json!({
                        "type": "nmh_registration_failed",
                        "error": format!("Failed to create command pipe (Win32 error {}); capture requests unavailable", err)
                    }),
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        create_failure_reported = false;

        // Wait for client connection
        // SAFETY: `pipe` is a live, uniquely owned named-pipe handle; a null OVERLAPPED
        // pointer selects the synchronous connection mode used when the pipe was created.
        let connected = unsafe { ConnectNamedPipe(pipe, ptr::null()) };
        if connected == 0 {
            // SAFETY: `GetLastError` reads thread-local state for the immediately preceding
            // failed `ConnectNamedPipe` call.
            let err = unsafe { GetLastError() };
            if err != ERROR_PIPE_CONNECTED {
                // SAFETY: ownership of the live pipe handle remains local on this error
                // path and is released exactly once.
                unsafe {
                    CloseHandle(pipe);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        }

        // Read request from the command pipe
        let mut buf = vec![0u8; 4096];
        let mut bytes_read: u32 = 0;
        // SAFETY: `pipe` is connected and live; `buf` is uniquely writable for its full
        // advertised length and `bytes_read` is valid output storage.
        let read_ok = unsafe {
            ReadFile(
                pipe,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut bytes_read,
                ptr::null(),
            )
        };

        if read_ok != 0 && bytes_read > 0 {
            let request_str = String::from_utf8_lossy(&buf[..bytes_read as usize]);

            if let Ok(req) = serde_json::from_str::<serde_json::Value>(&request_str) {
                let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");

                if command == "request_capture" {
                    // Forward capture request to extension via stdout NM protocol.
                    // Only report success to the main app if the stdout write
                    // actually succeeded — a closed NM port means the extension
                    // can't capture, and the app should fall back to normal
                    // screen capture for this frame.
                    let nm_msg = serde_json::json!({"type": "capture_request"});
                    let data = serde_json::to_vec(&nm_msg).unwrap_or_default();
                    let len = (data.len() as u32).to_le_bytes();

                    let forward_result: io::Result<()> = {
                        let mut handle = stdout_mutex.lock().unwrap_or_else(|e| e.into_inner());
                        handle
                            .write_all(&len)
                            .and_then(|_| handle.write_all(&data))
                            .and_then(|_| handle.flush())
                    };

                    let response = match forward_result {
                        Ok(()) => serde_json::json!({"status": "ok", "forwarded": true}),
                        Err(e) => serde_json::json!({
                            "status": "error",
                            "error": format!("Failed to forward capture request to extension: {}", e)
                        }),
                    };
                    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let mut written: u32 = 0;
                    // SAFETY: the pipe is connected; the response slice is live for the
                    // synchronous write and `written` is valid output storage.
                    unsafe {
                        WriteFile(
                            pipe,
                            response_bytes.as_ptr(),
                            response_bytes.len() as u32,
                            &mut written,
                            ptr::null(),
                        );
                        FlushFileBuffers(pipe);
                    }
                }
            }
        }

        // Disconnect and loop to accept next connection
        // SAFETY: this loop still uniquely owns the live pipe handle; disconnect happens
        // before the single closing call and no later code reuses the handle.
        unsafe {
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        }
    }
}

#[cfg(not(windows))]
fn run_command_pipe_server(_pipe_name: &str, _stdout_mutex: &Arc<Mutex<io::Stdout>>) {
    // NMH command pipe is only supported on Windows
}

/// Get the current user's SID string via Windows API
fn get_current_user_sid() -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::ptr;

        // Use Win32 API directly through std::os::windows
        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn OpenProcessToken(
                process_handle: *mut std::ffi::c_void,
                desired_access: u32,
                token_handle: *mut *mut std::ffi::c_void,
            ) -> i32;
            fn GetTokenInformation(
                token_handle: *mut std::ffi::c_void,
                token_information_class: u32,
                token_information: *mut u8,
                token_information_length: u32,
                return_length: *mut u32,
            ) -> i32;
            fn ConvertSidToStringSidW(sid: *const u8, string_sid: *mut *mut u16) -> i32;
            fn LocalFree(hmem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }

        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_USER_INFO: u32 = 1; // TokenUser

        // SAFETY: buffer lengths are obtained from Windows, token and allocated SID-string
        // handles are released exactly once, and all pointers target live writable storage
        // for the duration of synchronous Win32 calls.
        unsafe {
            let process = GetCurrentProcess();
            let mut token: *mut std::ffi::c_void = ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return Err("OpenProcessToken failed".into());
            }

            let mut return_length: u32 = 0;
            GetTokenInformation(
                token,
                TOKEN_USER_INFO,
                ptr::null_mut(),
                0,
                &mut return_length,
            );

            let mut buffer = vec![0u8; return_length as usize];
            if GetTokenInformation(
                token,
                TOKEN_USER_INFO,
                buffer.as_mut_ptr(),
                return_length,
                &mut return_length,
            ) == 0
            {
                CloseHandle(token);
                return Err("GetTokenInformation failed".into());
            }

            // TOKEN_USER struct: first field is SID_AND_ATTRIBUTES which starts with a pointer to SID
            let sid_ptr = *(buffer.as_ptr() as *const *const u8);

            let mut string_sid: *mut u16 = ptr::null_mut();
            if ConvertSidToStringSidW(sid_ptr, &mut string_sid) == 0 {
                CloseHandle(token);
                return Err("ConvertSidToStringSidW failed".into());
            }

            // Convert wide string to Rust String
            let mut len = 0;
            let mut p = string_sid;
            while *p != 0 {
                len += 1;
                p = p.add(1);
            }
            let slice = std::slice::from_raw_parts(string_sid, len);
            let result = String::from_utf16(slice)
                .map_err(|e| format!("UTF-16 conversion failed: {}", e))?;

            LocalFree(string_sid as *mut _);
            CloseHandle(token);

            Ok(result)
        }
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
}
