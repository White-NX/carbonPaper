//! Update and restart helpers keep the caller's logon identity and real parent,
//! but never inherit its administrative authority or unrelated handles.
use super::*;
use std::{
    ffi::{OsStr, OsString},
    os::windows::io::{FromRawHandle, OwnedHandle as WindowsHandle},
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            LocalFree, SetHandleInformation, BOOL, ERROR_NO_TOKEN, HANDLE_FLAG_INHERIT, HLOCAL,
        },
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
            },
            CreateRestrictedToken, EqualSid, GetLengthSid, GetSecurityDescriptorDacl,
            ImpersonateLoggedOnUser, RevertToSelf, SetTokenInformation, TokenDefaultDacl,
            TokenGroups, TokenIntegrityLevel, TokenOwner, TokenPrivileges, TokenStatistics,
            DISABLE_MAX_PRIVILEGE, LUA_TOKEN, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
            SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL,
            TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_IMPERSONATE, TOKEN_INFORMATION_CLASS,
            TOKEN_MANDATORY_LABEL, TOKEN_OWNER, TOKEN_PRIVILEGES, TOKEN_STATISTICS,
        },
        System::SystemServices::{
            SE_GROUP_ENABLED, SE_GROUP_INTEGRITY, SE_GROUP_LOGON_ID, SE_GROUP_OWNER,
        },
    },
};

struct LocalSid(PSID);

impl LocalSid {
    fn new(value: &str) -> Result<Self> {
        let value = identity::wide(value.as_ref());
        let mut sid = PSID::default();
        // SAFETY: Windows allocates the SID; LocalSid owns that allocation.
        unsafe { ConvertStringSidToSidW(PCWSTR(value.as_ptr()), &mut sid) }
            .map_err(|_| BrokerError::AccessDenied)?;
        Ok(Self(sid))
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        // SAFETY: this SID was allocated by ConvertStringSidToSidW.
        let sid = self.0;
        unsafe { LocalFree(HLOCAL(sid.0)) };
    }
}

struct LocalDescriptor {
    pointer: PSECURITY_DESCRIPTOR,
}

impl LocalDescriptor {
    fn new(sddl: &str) -> Result<Self> {
        let sddl = identity::wide(sddl.as_ref());
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: Windows allocates the descriptor, owned by this guard.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|_| BrokerError::AccessDenied)?;
        Ok(Self {
            pointer: descriptor,
        })
    }
}

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        // SAFETY: this descriptor was allocated by the conversion above.
        unsafe { LocalFree(HLOCAL(self.pointer.0)) };
    }
}

fn token_information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    let mut length = 0;
    // SAFETY: the first call sizes an aligned buffer owned through the query.
    unsafe { GetTokenInformation(token, class, None, 0, &mut length) }.ok();
    if length == 0 || length > 64 * 1024 {
        return Err(BrokerError::AccessDenied);
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .map_err(|_| BrokerError::AccessDenied)?;
    Ok(buffer)
}

fn validate_restricted_token(token: HANDLE, original: HANDLE) -> Result<()> {
    if identity::token_sid(token)? != identity::token_sid(original)? {
        return Err(BrokerError::AccessDenied);
    }
    let elevation = token_information(token, TokenElevation)?;
    let label = token_information(token, TokenIntegrityLevel)?;
    let groups = token_information(token, TokenGroups)?;
    let privileges = token_information(token, TokenPrivileges)?;
    let statistics = token_information(token, TokenStatistics)?;
    let original_statistics = token_information(original, TokenStatistics)?;
    let medium = LocalSid::new("S-1-16-8192")?;
    let administrators = LocalSid::new("S-1-5-32-544")?;
    let mut change_notify = windows::Win32::Foundation::LUID::default();
    let privilege_name = identity::wide("SeChangeNotifyPrivilege".as_ref());
    // SAFETY: every cast uses the corresponding OS token-information class;
    // the aligned buffers and SID allocations stay live throughout inspection.
    unsafe {
        windows::Win32::Security::LookupPrivilegeValueW(
            PCWSTR::null(),
            PCWSTR(privilege_name.as_ptr()),
            &mut change_notify,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let elevation = &*elevation.as_ptr().cast::<TOKEN_ELEVATION>();
        let label = &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let groups = &*groups.as_ptr().cast::<TOKEN_GROUPS>();
        let privileges = &*privileges.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let statistics = &*statistics.as_ptr().cast::<TOKEN_STATISTICS>();
        let original_statistics = &*original_statistics.as_ptr().cast::<TOKEN_STATISTICS>();
        let groups = std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
        let privileges = std::slice::from_raw_parts(
            privileges.Privileges.as_ptr(),
            privileges.PrivilegeCount as usize,
        );
        if elevation.TokenIsElevated != 0
            || EqualSid(label.Label.Sid, medium.0).is_err()
            || statistics.AuthenticationId != original_statistics.AuthenticationId
            || groups.iter().any(|group| {
                EqualSid(group.Sid, administrators.0).is_ok()
                    && group.Attributes & SE_GROUP_ENABLED as u32 != 0
            })
            || privileges
                .iter()
                .any(|privilege| privilege.Luid != change_notify)
        {
            return Err(BrokerError::AccessDenied);
        }
    }
    Ok(())
}

/// Derive the coordinator's permissions from the caller's own primary token.
/// No Explorer process, credentials or UAC-linked filtered token is required.
pub struct CoordinatorContext {
    token: Option<WindowsHandle>,
}

impl CoordinatorContext {
    pub fn new() -> Result<Self> {
        if !elevation_status()? {
            return Ok(Self { token: None });
        }
        // SAFETY: each returned token is owned exactly once. The restricted
        // primary token retains the same user, logon session and desktop access.
        unsafe {
            let mut original = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY
                    | TOKEN_DUPLICATE
                    | TOKEN_ASSIGN_PRIMARY
                    | TOKEN_ADJUST_DEFAULT
                    | TOKEN_IMPERSONATE,
                &mut original,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            let original = OwnedHandle(original);
            // Restrict this primary token directly. An intervening duplicate
            // would require SeAssignPrimaryTokenPrivilege during process creation.
            let source = original.0;
            let group_data = token_information(source, TokenGroups)?;
            let groups = &*group_data.as_ptr().cast::<TOKEN_GROUPS>();
            let groups =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            let administrators = LocalSid::new("S-1-5-32-544")?;
            let disabled_groups = groups
                .iter()
                .filter(|group| {
                    group.Attributes & SE_GROUP_LOGON_ID as u32 != SE_GROUP_LOGON_ID as u32
                        && (group.Attributes & SE_GROUP_OWNER as u32 != 0
                            || EqualSid(group.Sid, administrators.0).is_ok())
                })
                .map(|group| SID_AND_ATTRIBUTES {
                    Sid: group.Sid,
                    Attributes: 0,
                })
                .collect::<Vec<_>>();
            let mut token = HANDLE::default();
            CreateRestrictedToken(
                source,
                LUA_TOKEN | DISABLE_MAX_PRIVILEGE,
                Some(&disabled_groups),
                None,
                None,
                &mut token,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            let token = WindowsHandle::from_raw_handle(token.0);
            let user_sid = identity::token_sid(source)?;
            let user = LocalSid::new(&user_sid)?;
            let owner = TOKEN_OWNER { Owner: user.0 };
            SetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenOwner,
                (&owner as *const TOKEN_OWNER).cast(),
                std::mem::size_of_val(&owner) as u32,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            // Elevated tokens commonly grant full default-object access only
            // to Administrators and SYSTEM. Once Administrators is deny-only,
            // the child must still be able to use its own process/thread objects.
            let descriptor = LocalDescriptor::new(&format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)"))?;
            let mut present = BOOL::default();
            let mut defaulted = BOOL::default();
            let mut dacl = std::ptr::null_mut();
            GetSecurityDescriptorDacl(descriptor.pointer, &mut present, &mut dacl, &mut defaulted)
                .map_err(|_| BrokerError::AccessDenied)?;
            if !present.as_bool() || dacl.is_null() {
                return Err(BrokerError::AccessDenied);
            }
            let default_dacl = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
            SetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenDefaultDacl,
                (&default_dacl as *const TOKEN_DEFAULT_DACL).cast(),
                std::mem::size_of_val(&default_dacl) as u32,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            let medium = LocalSid::new("S-1-16-8192")?;
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: medium.0,
                    Attributes: SE_GROUP_INTEGRITY as u32,
                },
            };
            SetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenIntegrityLevel,
                (&label as *const TOKEN_MANDATORY_LABEL).cast(),
                (std::mem::size_of_val(&label) + GetLengthSid(medium.0) as usize) as u32,
            )
            .map_err(|_| BrokerError::AccessDenied)?;
            validate_restricted_token(HANDLE(token.as_raw_handle()), source)?;
            Ok(Self { token: Some(token) })
        }
    }

    /// Run only synchronous filesystem checks with the permissions that the
    /// coordinator will actually have, then restore the prior thread token.
    pub fn with_permissions<T>(&self, action: impl FnOnce() -> T) -> Result<T> {
        let Some(token) = &self.token else {
            return Ok(action());
        };
        struct Restore(Option<WindowsHandle>);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: the previous token remains owned until restoration.
                let result = unsafe {
                    match &self.0 {
                        Some(token) => SetThreadToken(None, HANDLE(token.as_raw_handle())),
                        None => RevertToSelf(),
                    }
                };
                if result.is_err() {
                    std::process::abort();
                }
            }
        }
        // SAFETY: impersonation is confined to this synchronous stack frame;
        // Restore also runs when action unwinds, before the thread can be reused.
        unsafe {
            let mut previous = HANDLE::default();
            let previous =
                match OpenThreadToken(GetCurrentThread(), TOKEN_IMPERSONATE, true, &mut previous) {
                    Ok(()) => Some(WindowsHandle::from_raw_handle(previous.0)),
                    Err(error)
                        if error.code() == windows::core::HRESULT::from_win32(ERROR_NO_TOKEN.0) =>
                    {
                        None
                    }
                    Err(_) => return Err(BrokerError::AccessDenied),
                };
            ImpersonateLoggedOnUser(HANDLE(token.as_raw_handle()))
                .map_err(|_| BrokerError::AccessDenied)?;
            let _restore = Restore(previous);
            Ok(action())
        }
    }

    pub fn spawn_coordinator(
        &self,
        executable: &Path,
        arguments: &[OsString],
        directory: &Path,
        kill_on_drop: bool,
    ) -> Result<(CoordinatorChild, NamedPipeServer)> {
        let name = format!(
            r"\\.\pipe\CarbonPaper.Update.Parent.{:032x}",
            rand::random::<u128>()
        );
        let descriptor = LocalDescriptor::new(&format!(
            "D:P(A;;FA;;;{})(A;;FA;;;SY)S:(ML;;NW;;;ME)",
            identity::current_sid()?
        ))?;
        // A high-integrity parent must create a medium-integrity channel: the
        // restricted child needs to use stdio without inheriting higher rights.
        // SAFETY: the owned descriptor outlives creation, which copies it.
        let server = unsafe {
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.pointer.0,
                bInheritHandle: false.into(),
            };
            ServerOptions::new()
                .first_pipe_instance(true)
                .max_instances(1)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
                .map_err(|_| BrokerError::Unavailable)?
        };
        // Open the sole client before spawning. There is no connection slot for
        // another process, and only this exact child inherits the client handle.
        let client = self
            .with_permissions(|| {
                OpenOptions::new()
                    .access_mode(super::super::transport::PIPE_CLIENT_ACCESS)
                    .custom_flags((SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION).0)
                    .open(&name)
            })?
            .map_err(|_| BrokerError::Unavailable)?;
        let stderr_file = OpenOptions::new()
            .write(true)
            .open("NUL")
            .map_err(|_| BrokerError::Unavailable)?;
        let input = HANDLE(client.as_raw_handle());
        let stderr = HANDLE(stderr_file.as_raw_handle());
        let child = self.spawn(
            executable,
            arguments,
            directory,
            Some([input, stderr]),
            kill_on_drop,
        )?;
        Ok((child, server))
    }

    pub fn spawn_restart(
        &self,
        executable: &Path,
        arguments: &[OsString],
        directory: &Path,
    ) -> Result<CoordinatorChild> {
        self.spawn(executable, arguments, directory, None, true)
    }

    fn spawn(
        &self,
        executable: &Path,
        arguments: &[OsString],
        directory: &Path,
        stdio: Option<[HANDLE; 2]>,
        kill_on_drop: bool,
    ) -> Result<CoordinatorChild> {
        let executable_name = identity::wide(executable.as_os_str());
        let directory = identity::wide(directory.as_os_str());
        let mut command = command_line(executable.as_os_str(), arguments);
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut flags = CREATE_NO_WINDOW;
        let mut storage = Vec::<usize>::new();
        struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for Attributes {
            fn drop(&mut self) {
                // SAFETY: the initialized attribute list outlives this guard.
                unsafe { DeleteProcThreadAttributeList(self.0) };
            }
        }
        // SAFETY: all command strings, attribute storage and stdio handles are
        // retained through creation. No parent override or job breakaway is used.
        unsafe {
            let _attributes = if let Some(handles) = &stdio {
                let [channel, error] = *handles;
                for &handle in handles {
                    SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)
                        .map_err(|_| BrokerError::Unavailable)?;
                }
                let mut size = 0;
                InitializeProcThreadAttributeList(
                    LPPROC_THREAD_ATTRIBUTE_LIST::default(),
                    1,
                    0,
                    &mut size,
                )
                .ok();
                if size == 0 || size > 64 * 1024 {
                    return Err(BrokerError::Unavailable);
                }
                storage.resize(size.div_ceil(std::mem::size_of::<usize>()), 0);
                let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
                InitializeProcThreadAttributeList(list, 1, 0, &mut size)
                    .map_err(|_| BrokerError::Unavailable)?;
                let attributes = Attributes(list);
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    Some(handles.as_ptr().cast()),
                    std::mem::size_of_val(handles),
                    None,
                    None,
                )
                .map_err(|_| BrokerError::Unavailable)?;
                startup.lpAttributeList = list;
                startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
                startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                startup.StartupInfo.hStdInput = channel;
                startup.StartupInfo.hStdOutput = channel;
                startup.StartupInfo.hStdError = error;
                flags |= EXTENDED_STARTUPINFO_PRESENT;
                Some(attributes)
            } else {
                None
            };
            let mut process = PROCESS_INFORMATION::default();
            let result = match &self.token {
                Some(token) => CreateProcessAsUserW(
                    HANDLE(token.as_raw_handle()),
                    PCWSTR(executable_name.as_ptr()),
                    PWSTR(command.as_mut_ptr()),
                    None,
                    None,
                    stdio.is_some(),
                    flags,
                    None,
                    PCWSTR(directory.as_ptr()),
                    &startup.StartupInfo,
                    &mut process,
                ),
                None => CreateProcessW(
                    PCWSTR(executable_name.as_ptr()),
                    PWSTR(command.as_mut_ptr()),
                    None,
                    None,
                    stdio.is_some(),
                    flags,
                    None,
                    PCWSTR(directory.as_ptr()),
                    &startup.StartupInfo,
                    &mut process,
                ),
            };
            result.map_err(|_| BrokerError::AccessDenied)?;
            let _thread = OwnedHandle(process.hThread);
            Ok(CoordinatorChild {
                process: WindowsHandle::from_raw_handle(process.hProcess.0),
                kill_on_drop,
            })
        }
    }
}

pub struct CoordinatorChild {
    process: WindowsHandle,
    kill_on_drop: bool,
}

impl CoordinatorChild {
    pub fn detach(&mut self) {
        self.kill_on_drop = false;
    }
}

impl Drop for CoordinatorChild {
    fn drop(&mut self) {
        if self.kill_on_drop {
            // SAFETY: this is the exact process handle returned by creation.
            unsafe { TerminateProcess(HANDLE(self.process.as_raw_handle()), 1) }.ok();
        }
    }
}

fn command_line(executable: &OsStr, arguments: &[OsString]) -> Vec<u16> {
    let mut command = Vec::new();
    for argument in std::iter::once(executable).chain(arguments.iter().map(OsString::as_os_str)) {
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        let characters = argument.encode_wide().collect::<Vec<_>>();
        if !characters.is_empty()
            && !characters
                .iter()
                .any(|&character| matches!(character, 9 | 32 | 34))
        {
            command.extend(characters);
            continue;
        }
        command.push(b'"' as u16);
        let mut slashes = 0;
        for character in characters {
            if character == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            command.extend(std::iter::repeat_n(
                b'\\' as u16,
                if character == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            command.push(character);
            slashes = 0;
        }
        command.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        command.push(b'"' as u16);
    }
    command.push(0);
    command
}

#[cfg(test)]
mod tests;
