//! Verify the loader policy of a signed helper before starting it with UAC.
use crate::protocol::*;
use std::{
    fs::File,
    io::{Read, Seek},
};

const MAX_EXECUTABLE_BYTES: u64 = 64 * 1024 * 1024;
const LOAD_CONFIG_HEADER_BYTES: usize = 80;
const SYSTEM32_ONLY: u16 = 0x800;

pub fn verify_loader_policy(file: &mut File) -> Result<()> {
    file.rewind().map_err(|_| BrokerError::Integrity)?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(MAX_EXECUTABLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BrokerError::Integrity)?;
    file.rewind().map_err(|_| BrokerError::Integrity)?;
    if bytes.len() as u64 > MAX_EXECUTABLE_BYTES {
        return Err(BrokerError::LimitExceeded);
    }
    verify_bytes(&bytes)
}

fn verify_bytes(bytes: &[u8]) -> Result<()> {
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
        ))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let pe = u32_at(0x3c).ok_or(BrokerError::Integrity)? as usize;
    if u16_at(0) != Some(0x5a4d) || u32_at(pe) != Some(0x4550) || u16_at(pe + 4) != Some(0x8664) {
        return Err(BrokerError::Integrity);
    }
    let optional = pe + 24;
    let optional_size = u16_at(pe + 20).ok_or(BrokerError::Integrity)? as usize;
    let sections = u16_at(pe + 6).ok_or(BrokerError::Integrity)? as usize;
    if u16_at(optional) != Some(0x20b)
        || optional_size < 240
        || u32_at(optional + 108).is_none_or(|count| count < 11)
        || !(1..=96).contains(&sections)
    {
        return Err(BrokerError::Integrity);
    }
    let entry = optional + 112 + 10 * 8;
    let rva = u32_at(entry).ok_or(BrokerError::Integrity)? as usize;
    let size = u32_at(entry + 4).ok_or(BrokerError::Integrity)? as usize;
    if rva == 0 || size < LOAD_CONFIG_HEADER_BYTES {
        return Err(BrokerError::Integrity);
    }
    let end = rva
        .checked_add(LOAD_CONFIG_HEADER_BYTES)
        .ok_or(BrokerError::Integrity)?;
    for index in 0..sections {
        let section = optional + optional_size + index * 40;
        let address = u32_at(section + 12).ok_or(BrokerError::Integrity)? as usize;
        let length = u32_at(section + 16).ok_or(BrokerError::Integrity)? as usize;
        let raw = u32_at(section + 20).ok_or(BrokerError::Integrity)? as usize;
        if rva >= address && end <= address.checked_add(length).ok_or(BrokerError::Integrity)? {
            let config = raw
                .checked_add(rva - address)
                .ok_or(BrokerError::Integrity)?;
            let declared = u32_at(config).ok_or(BrokerError::Integrity)? as usize;
            if declared >= LOAD_CONFIG_HEADER_BYTES
                && declared <= size
                && u16_at(config + 78) == Some(SYSTEM32_ONLY)
            {
                return Ok(());
            }
            break;
        }
    }
    Err(BrokerError::Integrity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn executable(flags: u16) -> Vec<u8> {
        let mut bytes = vec![0; 1024];
        for (at, value) in [
            (0, 0x5a4du16),
            (0x84, 0x8664),
            (0x86, 1),
            (0x94, 240),
            (0x98, 0x20b),
            (0x2ce, flags),
        ] {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [
            (0x3c, 0x80u32),
            (0x80, 0x4550),
            (0x104, 16),
            (0x158, 0x1080),
            (0x15c, 80),
            (0x194, 0x1000),
            (0x198, 0x200),
            (0x19c, 0x200),
            (0x280, 80),
        ] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn loader_policy_rejects_adjacent_dll_search_and_missing_metadata() {
        assert!(verify_bytes(&executable(SYSTEM32_ONLY)).is_ok());
        for flags in [0, 0x200, 0xa00] {
            assert_eq!(
                verify_bytes(&executable(flags)),
                Err(BrokerError::Integrity)
            );
        }
        let mut missing = executable(SYSTEM32_ONLY);
        missing[0x158..0x15c].fill(0);
        assert_eq!(verify_bytes(&missing), Err(BrokerError::Integrity));
        assert_eq!(
            verify_bytes(&executable(SYSTEM32_ONLY)[..0x2ce]),
            Err(BrokerError::Integrity)
        );
    }

    #[test]
    fn loader_policy_checks_the_file_after_hashing_and_rewinds_it() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&executable(SYSTEM32_ONLY)).unwrap();
        assert_eq!(verify_loader_policy(&mut file), Ok(()));
        assert_eq!(file.stream_position().unwrap(), 0);
    }
}
