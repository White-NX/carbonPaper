//! Shared wire contract for the desktop and browser native-messaging host.

use sha2::{Digest, Sha256};
use std::io;

pub const IPC_PROTOCOL_VERSION: u32 = 2;
pub const IPC_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

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
