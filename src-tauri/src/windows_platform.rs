//! Win32 identity helpers shared by the desktop and native-messaging host.

use std::collections::HashMap;
use std::path::PathBuf;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, LocalFree, FILETIME, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub(crate) struct OwnedHandle(pub HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: callers transfer a successfully opened, uniquely owned handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) fn current_process_token() -> windows::core::Result<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo process handle is valid; token points to writable storage.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
    }
    Ok(OwnedHandle(token))
}

pub(crate) fn current_user_sid() -> Result<String, String> {
    let token =
        current_process_token().map_err(|error| format!("OpenProcessToken failed: {error}"))?;
    token_user_sid(token.0)
}

fn token_user_sid(token: HANDLE) -> Result<String, String> {
    // SAFETY: Windows supplies the buffer size, storage is pointer-aligned, and
    // the token stays live through conversion. The allocated SID is freed even
    // when UTF-16 conversion fails.
    unsafe {
        let mut size = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
        .map_err(|error| format!("GetTokenInformation failed: {error}"))?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)
            .map_err(|error| format!("ConvertSidToStringSid failed: {error}"))?;
        let result = sid
            .to_string()
            .map_err(|error| format!("SID string conversion failed: {error}"));
        let _ = LocalFree(HLOCAL(sid.0.cast()));
        result
    }
}

/// A PID alone is not a process identity: Windows can reuse it after exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    /// Windows FILETIME ticks (100 ns); internal identity, never a wire timestamp.
    pub created_at: u64,
    pub user_sid: String,
    pub image: PathBuf,
}

pub(crate) fn inspect_process(pid: u32) -> Result<ProcessIdentity, String> {
    // SAFETY: the returned query-only handle is owned until all queries finish.
    let process = OwnedHandle(unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| format!("Cannot open peer process: {e}"))?
    });
    inspect_process_handle(pid, process.0)
}

fn inspect_process_handle(pid: u32, process: HANDLE) -> Result<ProcessIdentity, String> {
    // SAFETY: every query uses an owned, live process/token handle and initialized
    // output storage. The returned identity contains no borrowed OS pointers.
    unsafe {
        let mut raw_token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut raw_token)
            .map_err(|e| format!("Cannot query peer token: {e}"))?;
        let token = OwnedHandle(raw_token);
        let user_sid = token_user_sid(token.0)?;
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .map_err(|e| format!("Cannot query peer image: {e}"))?;
        let image =
            PathBuf::from(String::from_utf16(&path[..length as usize]).map_err(|e| e.to_string())?);
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user)
            .map_err(|e| format!("Cannot query peer creation time: {e}"))?;
        Ok(ProcessIdentity {
            pid,
            created_at: ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64,
            user_sid,
            image,
        })
    }
}

pub(crate) fn process_is_same(peer: &ProcessIdentity) -> bool {
    inspect_process(peer.pid).is_ok_and(|current| current == *peer)
}

pub(crate) fn terminate_process_if_same(peer: &ProcessIdentity) -> Result<(), String> {
    use windows::Win32::System::Threading::{TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: open and inspect the same process object that is terminated; a
    // PID reuse between queries cannot redirect termination to a new process.
    let process = OwnedHandle(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            false,
            peer.pid,
        )
        .map_err(|e| e.to_string())?
    });
    if inspect_process_handle(peer.pid, process.0)? != *peer {
        return Err("NMH process identity changed before reconnect".into());
    }
    // SAFETY: this owned handle was just verified against the authenticated NMH.
    unsafe { TerminateProcess(process.0, 0).map_err(|e| e.to_string()) }
}

pub(crate) fn browser_ancestor(nmh_pid: u32) -> Option<ProcessIdentity> {
    let nmh = inspect_process(nmh_pid).ok()?;
    let processes = process_tree()?;
    let mut ancestors = Vec::new();
    let mut pid = nmh_pid;
    for _ in 0..16 {
        let parent = processes.get(&pid)?.0;
        if parent == 0 || parent == pid {
            break;
        }
        let Some((_, name)) = processes.get(&parent) else {
            break;
        };
        ancestors.push((parent, name.clone()));
        pid = parent;
    }
    let (pid, _) = crate::nmh_protocol::select_browser_ancestor(&ancestors)?;
    let browser = inspect_process(pid).ok()?;
    (browser.pid != nmh.pid
        && browser.created_at <= nmh.created_at
        && browser.user_sid == nmh.user_sid)
        .then_some(browser)
}

pub(crate) fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: the RAII handle owns the process query handle and the UTF-16
    // buffer remains writable and live throughout the synchronous query.
    unsafe {
        let process = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .ok()?;
        (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

pub(crate) fn process_tree() -> Option<HashMap<u32, (u32, String)>> {
    // SAFETY: the snapshot is owned until enumeration finishes and dwSize
    // describes the windows crate's native PROCESSENTRY32W layout.
    unsafe {
        let snapshot = OwnedHandle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut entry).ok()?;
        let mut processes = HashMap::new();
        loop {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            processes.insert(
                entry.th32ProcessID,
                (
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..end]).to_lowercase(),
                ),
            );
            if Process32NextW(snapshot.0, &mut entry).is_err() {
                break;
            }
        }
        Some(processes)
    }
}
