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
//!   - Named Pipe (data): connects to CarbonPaper's NMH pipe (deterministic name from user SID)
//!   - Named Pipe (cmd):  CarbonPaper connects here to request captures

use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

    // Detect browser type and compute command pipe name for diagnostics
    let browser_type = detect_browser_type();
    let cmd_pipe_name = compute_nmh_cmd_pipe_name(&browser_type).unwrap_or_default();

    // Send startup diagnostic so the browser console shows the detected values
    send_nm_response(&stdout_mutex, &serde_json::json!({
        "type": "nmh_ready",
        "browser_type": browser_type,
        "cmd_pipe": cmd_pipe_name,
        "data_pipe": pipe_name,
    }));

    // Start command pipe thread immediately — it doesn't need the auth token
    // and allows the main app to send capture requests even during cold start
    let cmd_stdout = stdout_mutex.clone();
    start_command_pipe_thread(cmd_stdout);

    // Auth token is read fresh from disk on every message.  This is
    // intentional: the main app regenerates the token on each startup, so a
    // cached value would go stale whenever CarbonPaper restarts while the
    // NMH process is still alive.  The file is tiny and local, so the I/O
    // cost is negligible.
    let data_dir = get_data_dir();
    let auth_token_path = data_dir.join("nmh_auth_token");

    // Main stdin read loop
    loop {
        match read_nm_message() {
            Ok(None) => break, // stdin closed
            Ok(Some(msg)) => {
                // Read auth token fresh each time
                let auth_token = std::fs::read_to_string(&auth_token_path)
                    .ok()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());

                let response = match auth_token {
                    Some(token) => handle_message(msg, &pipe_name, &token),
                    None => serde_json::json!({
                        "status": "error",
                        "error": "CarbonPaper not running yet"
                    }),
                };
                send_nm_response(&stdout_mutex, &response);
            }
            Err(e) => {
                send_nm_response(&stdout_mutex, &serde_json::json!({
                    "status": "error",
                    "error": format!("Read error: {}", e)
                }));
                break;
            }
        }
    }
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
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Message too large"));
    }

    let mut buf = vec![0u8; len];
    handle.read_exact(&mut buf)?;

    let value: serde_json::Value = serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(Some(value))
}

/// Send a NM response to stdout via shared mutex (4-byte LE length + JSON)
fn send_nm_response(stdout_mutex: &Arc<Mutex<io::Stdout>>, value: &serde_json::Value) {
    let data = serde_json::to_vec(value).unwrap_or_default();
    let len = (data.len() as u32).to_le_bytes();

    let mut handle = stdout_mutex.lock().unwrap();
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
fn handle_message(msg: serde_json::Value, pipe_name: &str, auth_token: &str) -> serde_json::Value {
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "save_screenshot" => {
            // Complete message with all data
            let image_data = match msg.get("image_data").and_then(|v| v.as_str()) {
                Some(d) => d,
                None => return serde_json::json!({"status": "error", "error": "Missing image_data"}),
            };
            let image_hash = match msg.get("image_hash").and_then(|v| v.as_str()) {
                Some(h) => h,
                None => return serde_json::json!({"status": "error", "error": "Missing image_hash"}),
            };

            let pipe_request = serde_json::json!({
                "command": "save_extension_screenshot",
                "auth_token": auth_token,
                "image_data": image_data,
                "image_hash": image_hash,
                "width": msg.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                "height": msg.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
                "page_url": msg.get("page_url").and_then(|v| v.as_str()).unwrap_or(""),
                "page_title": msg.get("page_title").and_then(|v| v.as_str()).unwrap_or(""),
                "page_icon": msg.get("page_icon").and_then(|v| v.as_str()),
                "visible_links": msg.get("visible_links"),
                "browser_name": msg.get("browser_name").and_then(|v| v.as_str()).unwrap_or("browser-extension"),
            });

            send_to_pipe(pipe_name, &pipe_request)
        }
        "ping" => {
            serde_json::json!({"status": "ok", "type": "pong"})
        }
        _ => {
            serde_json::json!({"status": "error", "error": format!("Unknown message type: {}", msg_type)})
        }
    }
}

/// Send a JSON request to the Named Pipe and read the response
fn send_to_pipe(pipe_name: &str, request: &serde_json::Value) -> serde_json::Value {
    use std::fs::OpenOptions;

    let data = match serde_json::to_vec(request) {
        Ok(d) => d,
        Err(e) => return serde_json::json!({"status": "error", "error": format!("Serialization failed: {}", e)}),
    };

    // Open the named pipe as a file (Windows named pipes can be opened as files)
    let mut pipe = match OpenOptions::new().read(true).write(true).open(pipe_name) {
        Ok(p) => p,
        Err(e) => return serde_json::json!({
            "status": "error",
            "error": format!("Cannot connect to CarbonPaper (pipe {}): {}", pipe_name, e)
        }),
    };

    // Write request
    if let Err(e) = pipe.write_all(&data) {
        return serde_json::json!({"status": "error", "error": format!("Pipe write failed: {}", e)});
    }

    // Shutdown write side so server knows we're done sending
    // On Windows named pipes, we need to flush and then read
    if let Err(e) = pipe.flush() {
        return serde_json::json!({"status": "error", "error": format!("Pipe flush failed: {}", e)});
    }

    // Read response with timeout
    let mut response_buf = Vec::new();
    match pipe.read_to_end(&mut response_buf) {
        Ok(_) => {}
        Err(e) => {
            // Broken pipe is OK if we already got data
            if response_buf.is_empty() {
                return serde_json::json!({"status": "error", "error": format!("Pipe read failed: {}", e)});
            }
        }
    }

    if response_buf.is_empty() {
        return serde_json::json!({"status": "error", "error": "Empty response from CarbonPaper"});
    }

    match serde_json::from_slice(&response_buf) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"status": "error", "error": format!("Invalid response JSON: {}", e)}),
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

/// Compute deterministic NMH command pipe name.
/// Includes browser type so Chrome NMH and Edge NMH get separate command pipes.
fn compute_nmh_cmd_pipe_name(browser_type: &str) -> Result<String, String> {
    let sid = get_current_user_sid()?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{}carbonpaper_nmh_cmd_salt_{}", sid, browser_type));
    let hash = hasher.finalize();
    let hex_hash = hex::encode(hash);
    Ok(format!(r"\\.\pipe\carbon_nmh_cmd_{}", &hex_hash[..16]))
}

/// Detect browser type by walking up the process tree.
/// Returns "chrome" or "edge" based on ancestor process names.
/// The immediate parent may be a utility/broker subprocess, so we check
/// multiple levels of ancestors to find the actual browser process.
fn detect_browser_type() -> String {
    #[cfg(windows)]
    {
        use std::collections::HashMap;
        use std::mem;

        extern "system" {
            fn GetCurrentProcessId() -> u32;
        }

        extern "system" {
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
            fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
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

        unsafe {
            let current_pid = GetCurrentProcessId();
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
                return "chrome".to_string();
            }

            // Build a map of pid -> (parent_pid, exe_name) for all processes
            let mut process_map: HashMap<u32, (u32, String)> = HashMap::new();
            let mut entry: ProcessEntry32W = mem::zeroed();
            entry.dw_size = mem::size_of::<ProcessEntry32W>() as u32;

            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    let len = entry.sz_exe_file.iter().position(|&c| c == 0).unwrap_or(260);
                    let name = String::from_utf16_lossy(&entry.sz_exe_file[..len]).to_lowercase();
                    process_map.insert(
                        entry.th32_process_id,
                        (entry.th32_parent_process_id, name),
                    );

                    entry = mem::zeroed();
                    entry.dw_size = mem::size_of::<ProcessEntry32W>() as u32;
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }

            CloseHandle(snapshot);

            // Walk up the process tree (up to 16 levels) looking for a browser
            let mut pid = current_pid;
            for _ in 0..16 {
                let (parent_pid, name) = match process_map.get(&pid) {
                    Some(entry) => entry,
                    None => break,
                };
                // Check "msedge" before "chrome" — "msedge" is unambiguous
                if name.contains("msedge") {
                    return "edge".to_string();
                }
                if name.contains("chrome") {
                    return "chrome".to_string();
                }
                if *parent_pid == 0 || *parent_pid == pid {
                    break;
                }
                pid = *parent_pid;
            }

            // Default
            "chrome".to_string()
        }
    }

    #[cfg(not(windows))]
    {
        "chrome".to_string()
    }
}

/// Start the command pipe thread that listens for capture requests from the main app.
/// When a `request_capture` command arrives, it forwards a `capture_request` message
/// to the extension via stdout (NM protocol).
fn start_command_pipe_thread(stdout_mutex: Arc<Mutex<io::Stdout>>) {
    let browser_type = detect_browser_type();
    let cmd_pipe_name = match compute_nmh_cmd_pipe_name(&browser_type) {
        Ok(name) => name,
        Err(_) => return, // Silently fail — command pipe is optional
    };

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

    loop {
        // Create a new pipe instance
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,     // max instances
                4096,  // out buffer
                4096,  // in buffer
                0,     // default timeout
                ptr::null(),
            )
        };

        if pipe == INVALID_HANDLE_VALUE || pipe.is_null() {
            // Failed to create pipe, wait and retry
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }

        // Wait for client connection
        let connected = unsafe { ConnectNamedPipe(pipe, ptr::null()) };
        if connected == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_PIPE_CONNECTED {
                unsafe { CloseHandle(pipe); }
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        }

        // Read request from the command pipe
        let mut buf = vec![0u8; 4096];
        let mut bytes_read: u32 = 0;
        let read_ok = unsafe {
            ReadFile(pipe, buf.as_mut_ptr(), buf.len() as u32, &mut bytes_read, ptr::null())
        };

        if read_ok != 0 && bytes_read > 0 {
            let request_str = String::from_utf8_lossy(&buf[..bytes_read as usize]);

            if let Ok(req) = serde_json::from_str::<serde_json::Value>(&request_str) {
                let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");

                if command == "request_capture" {
                    // Forward capture request to extension via stdout NM protocol
                    let nm_msg = serde_json::json!({"type": "capture_request"});
                    let data = serde_json::to_vec(&nm_msg).unwrap_or_default();
                    let len = (data.len() as u32).to_le_bytes();

                    {
                        let mut handle = stdout_mutex.lock().unwrap();
                        let _ = handle.write_all(&len);
                        let _ = handle.write_all(&data);
                        let _ = handle.flush();
                    }

                    // Respond OK to main app
                    let response = serde_json::json!({"status": "ok"});
                    let response_bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let mut written: u32 = 0;
                    unsafe {
                        WriteFile(pipe, response_bytes.as_ptr(), response_bytes.len() as u32, &mut written, ptr::null());
                        FlushFileBuffers(pipe);
                    }
                }
            }
        }

        // Disconnect and loop to accept next connection
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
            fn ConvertSidToStringSidW(
                sid: *const u8,
                string_sid: *mut *mut u16,
            ) -> i32;
            fn LocalFree(hmem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }

        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_USER_INFO: u32 = 1; // TokenUser

        unsafe {
            let process = GetCurrentProcess();
            let mut token: *mut std::ffi::c_void = ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return Err("OpenProcessToken failed".into());
            }

            let mut return_length: u32 = 0;
            GetTokenInformation(token, TOKEN_USER_INFO, ptr::null_mut(), 0, &mut return_length);

            let mut buffer = vec![0u8; return_length as usize];
            if GetTokenInformation(token, TOKEN_USER_INFO, buffer.as_mut_ptr(), return_length, &mut return_length) == 0 {
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
            let result = String::from_utf16(slice).map_err(|e| format!("UTF-16 conversion failed: {}", e))?;

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
    PathBuf::from(local_appdata).join("CarbonPaper").join("data")
}
