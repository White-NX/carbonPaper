//! OS-derived identities and protected filesystem roots. Never use request or
//! environment supplied paths/SIDs as an authority across the SYSTEM boundary.
use crate::{ledger::Principal, manifest, protocol::*};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, LocalFree, BOOL, FILETIME, HANDLE, HLOCAL},
        Security::{
            self, Authorization::*, TokenUser, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ},
        System::{Com::CoTaskMemFree, Threading::*},
        UI::Shell::{
            FOLDERID_ProgramData, FOLDERID_ProgramFilesX64, FOLDERID_System, SHGetKnownFolderPath,
            KF_FLAG_DEFAULT,
        },
    },
};

pub struct OwnedHandle(pub HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

pub fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}

fn known_folder(id: &windows::core::GUID) -> Result<PathBuf> {
    // SAFETY: the documented API allocates the output with CoTaskMemAlloc;
    // copy the terminated string and free that allocation exactly once.
    unsafe {
        let value = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)
            .map_err(|_| BrokerError::Unavailable)?;
        let result = value
            .to_string()
            .map(PathBuf::from)
            .map_err(|_| BrokerError::Unavailable);
        CoTaskMemFree(Some(value.0.cast()));
        result
    }
}
pub fn protected_root() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramFilesX64)?
        .join("CarbonPaper")
        .join("Protected"))
}
pub fn state_root() -> Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramData)?
        .join("CarbonPaper")
        .join("KeyService"))
}
pub fn system_directory() -> Result<PathBuf> {
    known_folder(&FOLDERID_System)
}

fn sid_string(sid: PSID) -> Result<String> {
    // SAFETY: callers retain the token/descriptor containing this valid SID;
    // ConvertSidToStringSid allocates an independent LocalAlloc string.
    unsafe {
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(sid, &mut text).map_err(|_| BrokerError::AccessDenied)?;
        let value = text.to_string().map_err(|_| BrokerError::AccessDenied);
        LocalFree(HLOCAL(text.0.cast()));
        value
    }
}

pub fn process_sid(process: HANDLE) -> Result<String> {
    // SAFETY: the live process handle is queried only; aligned storage is sized
    // by GetTokenInformation and retained until the returned SID is copied.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token)
            .map_err(|_| BrokerError::AccessDenied)?;
        let token = OwnedHandle(token);
        token_sid(token.0)
    }
}

pub fn token_sid(token: HANDLE) -> Result<String> {
    // SAFETY: the caller retains the token; aligned storage holds TOKEN_USER
    // until the SID has been converted to an independently owned string.
    unsafe {
        let mut length = 0;
        let _ = Security::GetTokenInformation(token, TokenUser, None, 0, &mut length);
        if length == 0 || length > 64 * 1024 {
            return Err(BrokerError::AccessDenied);
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        Security::GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        sid_string(user.User.Sid)
    }
}

pub fn current_sid() -> Result<String> {
    unsafe { process_sid(GetCurrentProcess()) }
}

pub struct ProcessIdentity {
    pub principal: Principal,
    pub image: PathBuf,
    pub handle: OwnedHandle,
}

pub fn inspect_process(pid: u32) -> Result<ProcessIdentity> {
    // SAFETY: handles are owned, and each API receives correctly sized writable
    // output buffers that remain live for the duration of its synchronous call.
    unsafe {
        let handle = OwnedHandle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .map_err(|_| BrokerError::AccessDenied)?,
        );
        let sid = process_sid(handle.0)?;
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
        .map_err(|_| BrokerError::AccessDenied)?;
        let image = PathBuf::from(
            String::from_utf16(&path[..length as usize]).map_err(|_| BrokerError::AccessDenied)?,
        );
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user)
            .map_err(|_| BrokerError::AccessDenied)?;
        let stamp = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        Ok(ProcessIdentity {
            principal: Principal {
                sid,
                runtime_id: String::new(),
                process_identity: format!("{pid}:{stamp}"),
            },
            image,
            handle,
        })
    }
}

pub fn path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .eq_ignore_ascii_case(right.to_string_lossy().trim_start_matches(r"\\?\"))
}

pub fn no_reparse(path: &Path) -> Result<()> {
    let info = std::fs::symlink_metadata(path).map_err(|_| BrokerError::Integrity)?;
    if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(BrokerError::Integrity);
    }
    Ok(())
}

fn trusted_sid(sid: &str) -> bool {
    matches!(
        sid,
        "S-1-5-18"
            | "S-1-5-32-544"
            | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
    )
}

/// Reject mutable ownership, null DACLs, unsupported ACE forms, and any write
/// grant outside SYSTEM, Administrators and Windows TrustedInstaller.
pub fn assert_protected(path: &Path) -> Result<()> {
    no_reparse(path)?;
    let name = wide(path.as_os_str());
    // SAFETY: GetNamedSecurityInfo allocates the descriptor. Owner, ACL and ACE
    // pointers refer into it and are inspected before it is freed below.
    unsafe {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut owner = PSID::default();
        let mut acl = std::ptr::null_mut();
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            Security::OWNER_SECURITY_INFORMATION | Security::DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            &mut descriptor,
        )
        .ok()
        .map_err(|_| BrokerError::AccessDenied)?;
        let result = (|| {
            if acl.is_null() || !trusted_sid(&sid_string(owner)?) {
                return Err(BrokerError::AccessDenied);
            }
            // Generic/standard write rights and every file/directory mutation.
            const WRITE_RIGHTS: u32 = 0x1000_0000
                | 0x4000_0000
                | 0x0001_0000
                | 0x0004_0000
                | 0x0008_0000
                | 0x2
                | 0x4
                | 0x10
                | 0x40
                | 0x100;
            for index in 0..(*acl).AceCount {
                let mut ace = std::ptr::null_mut();
                Security::GetAce(acl, index as u32, &mut ace)
                    .map_err(|_| BrokerError::AccessDenied)?;
                let header = &*ace.cast::<Security::ACE_HEADER>();
                if header.AceType == 1 {
                    continue;
                } // deny ACE cannot grant writes
                if header.AceType != 0 {
                    return Err(BrokerError::AccessDenied);
                }
                let allowed = &*ace.cast::<Security::ACCESS_ALLOWED_ACE>();
                if allowed.Mask & WRITE_RIGHTS != 0 {
                    let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
                    if !trusted_sid(&sid_string(sid)?) {
                        return Err(BrokerError::AccessDenied);
                    }
                }
            }
            Ok(())
        })();
        LocalFree(HLOCAL(descriptor.0));
        result
    }
}

/// Applies an explicit owner and protected DACL to a freshly created object.
pub fn protect_path(path: &Path, readable: bool) -> Result<()> {
    no_reparse(path)?;
    let sddl = if readable {
        "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)"
    } else {
        "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    };
    let text = wide(sddl.as_ref());
    let name = wide(path.as_os_str());
    // SAFETY: the SDDL conversion owns the returned descriptor; extracted owner
    // and DACL pointers are valid until SetNamedSecurityInfo has copied them.
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
            let mut owner = PSID::default();
            let mut defaulted = BOOL::default();
            Security::GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted)
                .map_err(|_| BrokerError::AccessDenied)?;
            let (mut present, mut acl) = (BOOL::default(), std::ptr::null_mut());
            Security::GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
                .map_err(|_| BrokerError::AccessDenied)?;
            SetNamedSecurityInfoW(
                PCWSTR(name.as_ptr()),
                SE_FILE_OBJECT,
                Security::OWNER_SECURITY_INFORMATION
                    | Security::DACL_SECURITY_INFORMATION
                    | Security::PROTECTED_DACL_SECURITY_INFORMATION,
                owner,
                PSID::default(),
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

pub fn ensure_directory(path: &Path, readable: bool) -> Result<()> {
    if path.exists() {
        return assert_protected(path);
    }
    std::fs::create_dir(path).map_err(|_| BrokerError::Storage)?;
    protect_path(path, readable)
}

pub fn locked_file(path: &Path) -> Result<File> {
    no_reparse(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .map_err(|_| BrokerError::Integrity)?;
    let metadata = file.metadata().map_err(|_| BrokerError::Integrity)?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(BrokerError::LimitExceeded);
    }
    Ok(file)
}

pub fn verify_file(file: &mut File, expected: &str) -> Result<()> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let length = file.read(&mut buffer).map_err(|_| BrokerError::Integrity)?;
        if length == 0 {
            break;
        }
        total += length as u64;
        if total > 2 * 1024 * 1024 * 1024 {
            return Err(BrokerError::LimitExceeded);
        }
        hash.update(&buffer[..length]);
    }
    if hex::encode(hash.finalize()) != expected {
        return Err(BrokerError::Integrity);
    }
    Ok(())
}

pub struct VerifiedCaller {
    pub principal: Principal,
    pub image_lock: File,
}

pub fn verify_main(mut process: ProcessIdentity) -> Result<VerifiedCaller> {
    let root = protected_root()?;
    let directory = process.image.parent().ok_or(BrokerError::AccessDenied)?;
    if process
        .image
        .file_name()
        .is_none_or(|v| !v.to_string_lossy().eq_ignore_ascii_case("carbonpaper.exe"))
    {
        return Err(BrokerError::AccessDenied);
    }
    let id = directory
        .file_name()
        .ok_or(BrokerError::AccessDenied)?
        .to_string_lossy();
    if !safe_runtime_id(&id) || !path_eq(directory, &root.join("Runtime").join(id.as_ref())) {
        return Err(BrokerError::AccessDenied);
    }
    for path in [
        root.parent()
            .ok_or(BrokerError::AccessDenied)?
            .to_path_buf(),
        root.clone(),
        root.join("Runtime"),
        directory.to_path_buf(),
        process.image.clone(),
    ] {
        assert_protected(&path)?;
    }
    let (manifest, manifest_id) = manifest::read_manifest(directory)?;
    if manifest_id != id {
        return Err(BrokerError::Integrity);
    }
    let mut file = locked_file(&process.image)?;
    verify_file(
        &mut file,
        manifest
            .files
            .get("carbonpaper.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    process.principal.runtime_id = manifest_id;
    Ok(VerifiedCaller {
        principal: process.principal,
        image_lock: file,
    })
}

pub fn safe_runtime_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b".+-".contains(&v))
        && !value.starts_with('.')
        && !value.ends_with('.')
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub runtime_id: String,
}

pub fn active_runtime() -> Result<Option<PathBuf>> {
    let root = protected_root()?;
    let path = root
        .join("Activations")
        .join(format!("{}.json", current_sid()?));
    if !path.exists() {
        return Ok(None);
    }
    for candidate in [&root, &root.join("Activations"), &path] {
        assert_protected(candidate)?;
    }
    let bytes = manifest::read_bounded(&path, 1024)?;
    let activation: Activation =
        serde_json::from_slice(&bytes).map_err(|_| BrokerError::Integrity)?;
    if !safe_runtime_id(&activation.runtime_id) {
        return Err(BrokerError::Integrity);
    }
    let directory = root.join("Runtime").join(&activation.runtime_id);
    assert_protected(&directory)?;
    let (_, id) = manifest::read_manifest(&directory)?;
    if id != activation.runtime_id {
        return Err(BrokerError::Integrity);
    }
    Ok(Some(directory))
}
