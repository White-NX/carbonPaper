//! Shared wire contract for the desktop and browser native-messaging host.

use sha2::{Digest, Sha256};
use std::io;

pub const IPC_PROTOCOL_VERSION: u32 = 3;
pub const IPC_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const RESPONSE_ACK: u8 = 0x06;
pub const DATA_PIPE_PREFIX: &str = r"\\.\pipe\carbon_nmh_data_r_";
pub const CMD_PIPE_PREFIX: &str = r"\\.\pipe\carbon_nmh_cmd_r_";

pub fn valid_private_pipe_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|suffix| suffix.len() == 32 && suffix.bytes().all(|c| c.is_ascii_hexdigit()))
}

pub fn request_pid(request: &serde_json::Value, field: &str) -> Option<u32> {
    request
        .get(field)?
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// Chromium can launch NMH through cmd.exe and a chain of browser subprocesses.
/// Keep browser discovery independent of a list of executable names.
pub fn select_browser_ancestor(ancestors: &[(u32, String)]) -> Option<(u32, String)> {
    let index = ancestors.iter().position(|(_, name)| {
        !name.is_empty()
            && !name.eq_ignore_ascii_case("cmd.exe")
            && !name.eq_ignore_ascii_case("cmd")
    })?;
    let (mut pid, name) = ancestors[index].clone();
    for (ancestor_pid, ancestor_name) in ancestors.iter().skip(index + 1) {
        if !ancestor_name.eq_ignore_ascii_case(&name) {
            break;
        }
        pid = *ancestor_pid;
    }
    Some((pid, name))
}

pub fn pipe_name(sid: &str) -> String {
    let hash = Sha256::digest(format!("{sid}carbonpaper_nmh_salt"));
    format!(r"\\.\pipe\carbon_nmh_{}", &hex::encode(hash)[..16])
}

pub fn frame_prefix(length: usize) -> io::Result<[u8; 4]> {
    if length == 0 || length > IPC_MAX_MESSAGE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!(
            "Invalid IPC v{IPC_PROTOCOL_VERSION} frame length: {length} (max {IPC_MAX_MESSAGE_BYTES})"
        )));
    }
    Ok((length as u32).to_le_bytes())
}

pub fn frame_length(prefix: [u8; 4]) -> io::Result<usize> {
    let length = u32::from_le_bytes(prefix) as usize;
    frame_prefix(length)?;
    Ok(length)
}
