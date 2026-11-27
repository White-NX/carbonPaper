//! Privileged, append-only, per-SID retirement inbox. Replay is idempotent;
//! truncation happens only after the ledger transaction is durably committed.
use super::identity;
use crate::{ledger::Ledger, protocol::*, revocation::Revocation};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use windows::Win32::Storage::FileSystem::{
    FILE_APPEND_DATA, FILE_FLAG_WRITE_THROUGH, FILE_READ_ATTRIBUTES,
};

const DIRECTORY: &str = "Revocations";
const MAX_INBOX_BYTES: u64 = 4 * 1024 * 1024;

fn path(root: &Path, sid: &str) -> Result<PathBuf> {
    if !sid.starts_with("S-1-")
        || sid.len() > 184
        || !sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
    {
        return Err(BrokerError::AccessDenied);
    }
    Ok(root.join(DIRECTORY).join(format!("{sid}.log")))
}

pub fn provision(root: &Path, sid: &str) -> Result<()> {
    let target = path(root, sid)?;
    identity::assert_protected(root)?;
    identity::protect_inbox_directory(root)?;
    identity::ensure_directory(&root.join(DIRECTORY), false)?;
    identity::protect_inbox_directory(&root.join(DIRECTORY))?;
    if !target.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| BrokerError::Storage)?;
    } else {
        identity::no_reparse(&target)?;
    }
    identity::protect_revocation_inbox(&target, sid)?;
    identity::assert_revocation_inbox(&target, sid)
}

/// None is a legacy installation without an inbox, requiring its online pipe.
pub fn append(sid: &str, intent: &Revocation) -> Result<Option<()>> {
    let root = identity::state_root()?;
    append_at(&root, sid, intent)
}

fn append_at(root: &Path, sid: &str, intent: &Revocation) -> Result<Option<()>> {
    let target = path(root, sid)?;
    if !target.exists() {
        return Ok(None);
    }
    identity::assert_protected(root)?;
    identity::assert_protected(&root.join(DIRECTORY))?;
    identity::assert_revocation_inbox(&target, sid)?;
    let file = OpenOptions::new()
        .access_mode(FILE_APPEND_DATA.0 | FILE_READ_ATTRIBUTES.0 | 0x0010_0000)
        .custom_flags(FILE_FLAG_WRITE_THROUGH.0)
        .share_mode(0)
        .open(&target)
        .map_err(|_| BrokerError::Busy)?;
    append_file(file, intent)
}

fn append_file(mut file: File, intent: &Revocation) -> Result<Option<()>> {
    intent.validate()?;
    let size = file.metadata().map_err(|_| BrokerError::Storage)?.len();
    // The size limit itself is a durable retire-all marker. Clients stop
    // appending once it is reached; the service retires all this owner's grants.
    if size >= MAX_INBOX_BYTES {
        return Ok(Some(()));
    }
    let mut bytes = serde_json::to_vec(intent).map_err(|_| BrokerError::InvalidRequest)?;
    bytes.push(b'\n');
    if size + bytes.len() as u64 >= MAX_INBOX_BYTES {
        bytes.resize((MAX_INBOX_BYTES - size) as usize, 0);
    }
    file.write_all(&bytes).map_err(|_| BrokerError::Storage)?;
    // WRITE_THROUGH supplies durability on an append-only handle, without
    // requesting the overwrite rights required by a generic writer.
    Ok(Some(()))
}

pub fn drain(root: &Path, sid: &str, ledger: &mut Ledger) -> Result<()> {
    let target = path(root, sid)?;
    if !target.exists() {
        return Ok(());
    }
    identity::assert_protected(&root.join(DIRECTORY))?;
    identity::assert_revocation_inbox(&target, sid)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&target)
        .map_err(|_| BrokerError::Busy)?;
    drain_file(file, sid, ledger)
}

fn drain_file(mut file: File, sid: &str, ledger: &mut Ledger) -> Result<()> {
    let size = file.metadata().map_err(|_| BrokerError::Storage)?.len();
    if size == 0 {
        return Ok(());
    }
    let mut bytes = Vec::new();
    if size < MAX_INBOX_BYTES {
        (&mut file)
            .take(MAX_INBOX_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|_| BrokerError::Storage)?;
    }
    let intents = if size >= MAX_INBOX_BYTES || !bytes.ends_with(b"\n") {
        vec![Revocation::RetireOwner]
    } else {
        let parsed: Option<Vec<Revocation>> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_slice::<Revocation>(line)
                    .ok()
                    .filter(|intent| intent.validate().is_ok())
            })
            .collect();
        parsed.unwrap_or_else(|| vec![Revocation::RetireOwner])
    };
    ledger.apply_revocations(sid, &intents)?;
    file.set_len(0).map_err(|_| BrokerError::Storage)?;
    file.sync_all().map_err(|_| BrokerError::Storage)
}

pub fn drain_all(root: &Path, ledger: &mut Ledger) -> Result<()> {
    for sid in ledger.registered_owner_sids()? {
        match drain(root, &sid, ledger) {
            Err(BrokerError::Busy) => continue,
            result => result?,
        }
    }
    Ok(())
}

pub(super) fn remove(root: &Path, sid: &str) -> Result<()> {
    let target = path(root, sid)?;
    if target.exists() {
        identity::assert_revocation_inbox(&target, sid)?;
        std::fs::remove_file(target).map_err(|_| BrokerError::Busy)?;
    }
    Ok(())
}

pub(super) fn collect_unregistered(root: &Path, ledger: &Ledger) -> Result<()> {
    let directory = root.join(DIRECTORY);
    if !directory.try_exists().map_err(|_| BrokerError::Storage)? {
        return Ok(());
    }
    identity::assert_protected(root)?;
    identity::assert_protected(&directory)?;
    let owners: std::collections::BTreeSet<_> =
        ledger.registered_owner_sids()?.into_iter().collect();
    for entry in std::fs::read_dir(&directory).map_err(|_| BrokerError::Storage)? {
        let target = entry.map_err(|_| BrokerError::Storage)?.path();
        if target
            .extension()
            .is_none_or(|extension| extension != "log")
        {
            continue;
        }
        let sid = target
            .file_stem()
            .and_then(|sid| sid.to_str())
            .ok_or(BrokerError::Integrity)?;
        if !identity::safe_sid(sid) || owners.contains(sid) {
            continue;
        }
        // Inbox append rights are intentional. Validate their dedicated ACL,
        // rather than treating an owner's append permission as unsafe writes.
        match remove(root, sid) {
            Ok(()) | Err(BrokerError::Busy) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires administrator access; uses only an isolated protected directory"]
    fn restricted_client_can_append_but_cannot_read_overwrite_or_erase_revocations() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("state");
        identity::ensure_directory(&root, false).unwrap();
        let sid = identity::current_sid().unwrap();
        provision(&root, &sid).unwrap();
        let target = path(&root, &sid).unwrap();
        let mut ledger = Ledger::open(&root.join("keys.db")).unwrap();
        ledger.register_owner(&sid, "runtime", true).unwrap();
        let context = crate::windows::update::CoordinatorContext::new().unwrap();
        context
            .with_permissions(|| {
                assert_eq!(
                    append_at(&root, &sid, &Revocation::RetireOwner).unwrap(),
                    Some(())
                );
                assert!(std::fs::read_dir(&root).is_err());
                assert!(std::fs::read_dir(root.join(DIRECTORY)).is_err());
                assert!(File::open(root.join("keys.db")).is_err());
                assert!(File::open(&target).is_err());
                assert!(OpenOptions::new().write(true).open(&target).is_err());
                assert!(std::fs::remove_file(&target).is_err());
            })
            .unwrap();
        let pending = std::fs::read(&target).unwrap();
        provision(&root, &sid).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), pending);
        drain(&root, &sid, &mut ledger).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
    }
    #[test]
    fn a_partial_or_full_inbox_retires_owner_grants_and_stays_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let mut ledger = Ledger::open(&directory.path().join("keys.db")).unwrap();
        let sid = "S-1-5-21-100";
        ledger.register_owner(sid, "runtime", true).unwrap();
        let target = directory.path().join("inbox");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .unwrap();
        file.set_len(MAX_INBOX_BYTES - 3).unwrap();
        drop(file);
        append_file(
            OpenOptions::new().append(true).open(&target).unwrap(),
            &Revocation::RetireOwner,
        )
        .unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), MAX_INBOX_BYTES);
        let file = OpenOptions::new().append(true).open(&target).unwrap();
        append_file(file, &Revocation::RetireOwner).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), MAX_INBOX_BYTES);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&target)
            .unwrap();
        drain_file(file, sid, &mut ledger).unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
        std::fs::write(&target, b"{\"operation\":").unwrap();
        drain_file(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&target)
                .unwrap(),
            sid,
            &mut ledger,
        )
        .unwrap();
        assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
    }
}
