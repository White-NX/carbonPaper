//! Win32 identity helpers shared by the desktop and native-messaging host.

use std::collections::HashMap;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
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
    // SAFETY: Windows supplies the buffer size, storage is pointer-aligned, and
    // the token stays live through conversion. The allocated SID is freed even
    // when UTF-16 conversion fails.
    unsafe {
        let mut size = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut size);
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        GetTokenInformation(
            token.0,
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
