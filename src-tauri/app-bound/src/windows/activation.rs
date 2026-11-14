//! The ledger is the durable activation authority. Readable launch pointers
//! are projections and may be rebuilt after an interrupted elevated install.
use super::{identity, install};
use crate::{ledger::Ledger, manifest, protocol::*};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

const PENDING: &str = "activation.pending.json";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingActivation {
    pub sid: String,
    pub runtime_id: String,
    pub system_runtime_id: String,
}

impl PendingActivation {
    fn validate(&self) -> Result<()> {
        if !identity::safe_sid(&self.sid)
            || !identity::safe_runtime_id(&self.runtime_id)
            || !identity::safe_runtime_id(&self.system_runtime_id)
        {
            return Err(BrokerError::Integrity);
        }
        Ok(())
    }
}

pub(super) fn read_pending(root: &Path) -> Result<Option<PendingActivation>> {
    let path = root.join("System").join(PENDING);
    if !path.try_exists().map_err(|_| BrokerError::Storage)? {
        return Ok(None);
    }
    let pending: PendingActivation = serde_json::from_slice(&manifest::read_bounded(&path, 1024)?)
        .map_err(|_| BrokerError::Integrity)?;
    pending.validate()?;
    Ok(Some(pending))
}

pub(super) fn begin(root: &Path, pending: &PendingActivation) -> Result<()> {
    pending.validate()?;
    install::atomic_write(
        &root.join("System").join(PENDING),
        &serde_json::to_vec(pending).map_err(|_| BrokerError::Storage)?,
    )
}

pub(super) fn finish(root: &Path) -> Result<()> {
    let path = root.join("System").join(PENDING);
    if path.try_exists().map_err(|_| BrokerError::Storage)? {
        fs::remove_file(path).map_err(|_| BrokerError::Storage)?;
    }
    Ok(())
}

pub(super) fn wait_for_projection(root: &Path, sid: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Some(pending) = read_pending(root)? {
        if pending.sid != sid {
            return Ok(());
        }
        identity::assert_protected(&root.join("System"))?;
        identity::assert_protected(&root.join("System").join(PENDING))?;
        if Instant::now() >= deadline {
            // Never launch the previous pointer while registration is unsettled.
            return Err(BrokerError::Busy);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Caller holds the elevated setup lock. This never rolls back grants or keys.
pub(super) fn reconcile(root: &Path, ledger: &Ledger) -> Result<()> {
    for path in [
        root.to_path_buf(),
        root.join("System"),
        root.join("Activations"),
        root.join("Runtime"),
    ] {
        identity::assert_protected(&path)?;
    }
    if let Some(pending) = read_pending(root)? {
        reconcile_system(root, &pending)?;
    }
    project(root, ledger, |runtime| {
        let directory = root.join("Runtime").join(runtime);
        identity::assert_protected(&directory)?;
        let (_, id) = manifest::read_manifest(&directory)?;
        if id != runtime {
            return Err(BrokerError::Integrity);
        }
        Ok(())
    })
}

fn reconcile_system(root: &Path, pending: &PendingActivation) -> Result<()> {
    let system = root.join("System");
    let service = system.join("carbonpaper-key-service.exe");
    if !service.try_exists().map_err(|_| BrokerError::Storage)? {
        // The first installation can stop before creating the service. With no
        // committed owner there is no launch authority to recover yet.
        return Ok(());
    }
    identity::assert_protected(&service)?;
    let marker = system.join("runtime.json");
    let mut candidates = vec![pending.system_runtime_id.clone()];
    if marker.exists() {
        identity::assert_protected(&marker)?;
        let previous: identity::Activation =
            serde_json::from_slice(&manifest::read_bounded(&marker, 1024)?)
                .map_err(|_| BrokerError::Integrity)?;
        if !identity::safe_runtime_id(&previous.runtime_id) {
            return Err(BrokerError::Integrity);
        }
        candidates.push(previous.runtime_id);
    }
    // Replacement is atomic. Either signed service may have been installed
    // when the process stopped; retain that version and repair its companion.
    for id in candidates {
        let directory = root.join("Runtime").join(&id);
        identity::assert_protected(&directory)?;
        let (release, actual_id) = manifest::read_manifest(&directory)?;
        if actual_id != id {
            return Err(BrokerError::Integrity);
        }
        let expected = release
            .files
            .get("carbonpaper-key-service.exe")
            .ok_or(BrokerError::Integrity)?;
        if identity::verify_file(&mut identity::locked_file(&service)?, expected).is_err() {
            continue;
        }
        let setup = system.join("carbonpaper-protected-setup.exe");
        let setup_hash = release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or(BrokerError::Integrity)?;
        if identity::locked_file(&setup)
            .and_then(|mut file| identity::verify_file(&mut file, setup_hash))
            .is_err()
        {
            identity::verify_file(
                &mut identity::locked_file(&directory.join("carbonpaper-protected-setup.exe"))?,
                setup_hash,
            )?;
            install::copy_trusted(&directory.join("carbonpaper-protected-setup.exe"), &setup)?;
        }
        return install::atomic_write(
            &marker,
            &serde_json::to_vec(&identity::Activation { runtime_id: id })
                .map_err(|_| BrokerError::Storage)?,
        );
    }
    Err(BrokerError::Integrity)
}

fn project(root: &Path, ledger: &Ledger, verify: impl Fn(&str) -> Result<()>) -> Result<()> {
    let pending = read_pending(root)?;
    for sid in ledger.registered_owner_sids()? {
        if !identity::safe_sid(&sid) {
            return Err(BrokerError::Integrity);
        }
        let id = ledger
            .registered_runtime(&sid)?
            .ok_or(BrokerError::Integrity)?;
        if !identity::safe_runtime_id(&id) {
            return Err(BrokerError::Integrity);
        }
        if let Err(error) = verify(&id) {
            if pending.as_ref().is_some_and(|pending| pending.sid == sid) {
                return Err(error);
            }
            // A damaged runtime belonging to another user must not stop this
            // service or force an unrelated installation to change authority.
            super::diagnostics::event(
                "warn",
                "activation_projection_deferred",
                "startup",
                Some(error),
            );
            continue;
        }
        let path = root.join("Activations").join(format!("{sid}.json"));
        let bytes = serde_json::to_vec(&identity::Activation { runtime_id: id })
            .map_err(|_| BrokerError::Storage)?;
        if manifest::read_bounded(&path, 1024).ok().as_deref() != Some(bytes.as_slice()) {
            install::atomic_write(&path, &bytes)?;
        }
    }
    if let Some(pending) = pending {
        if ledger.registered_runtime(&pending.sid)?.is_none() {
            let path = root
                .join("Activations")
                .join(format!("{}.json", pending.sid));
            if path.exists() {
                fs::remove_file(path).map_err(|_| BrokerError::Storage)?;
            }
        }
    }
    finish(root)
}

#[cfg(not(test))]
pub(super) fn reconcile_after_interruption(root: &Path, ledger: &Ledger) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let lock = root.join("System/setup.lock");
    identity::assert_protected(&lock)?;
    let _lock = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&lock)
    {
        Ok(file) => file,
        // A live installer writes the projection before starting the service.
        // It owns the lock and completes or retains the recovery marker itself.
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(()),
        Err(_) => return Err(BrokerError::Storage),
    };
    reconcile(root, ledger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_activation_projects_the_committed_owner_and_preserves_other_users() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("System")).unwrap();
        fs::create_dir(root.join("Activations")).unwrap();
        let sid = "S-1-5-21-100";
        let other = "S-1-5-21-101";
        let mut ledger = Ledger::open(&root.join("keys.db")).unwrap();
        ledger.register_owner(sid, "old-runtime", true).unwrap();
        ledger
            .register_owner(other, "other-runtime", false)
            .unwrap();
        project(root, &ledger, |id| {
            if id == "other-runtime" {
                Err(BrokerError::Integrity)
            } else {
                Ok(())
            }
        })
        .unwrap();
        let pending = PendingActivation {
            sid: sid.into(),
            runtime_id: "new-runtime".into(),
            system_runtime_id: "new-runtime".into(),
        };
        begin(root, &pending).unwrap();
        // Crash before the registration transaction commits: the old authority wins.
        project(root, &ledger, |_| Ok(())).unwrap();
        assert!(
            fs::read_to_string(root.join("Activations").join(format!("{sid}.json")))
                .unwrap()
                .contains("old-runtime")
        );
        begin(root, &pending).unwrap();
        ledger.register_owner(sid, "new-runtime", false).unwrap();
        // Crash after commit but before the readable pointer is replaced.
        drop(ledger);
        let ledger = Ledger::open(&root.join("keys.db")).unwrap();
        project(root, &ledger, |_| Ok(())).unwrap();
        assert!(
            fs::read_to_string(root.join("Activations").join(format!("{sid}.json")))
                .unwrap()
                .contains("new-runtime")
        );
        assert!(
            fs::read_to_string(root.join("Activations").join(format!("{other}.json")))
                .unwrap()
                .contains("other-runtime")
        );
        assert_eq!(
            ledger.owner_registration(sid).unwrap(),
            Some(("new-runtime".into(), true))
        );
        assert!(read_pending(root).unwrap().is_none());
        project(root, &ledger, |_| Ok(())).unwrap();

        // An unavailable target cannot erase the launch fence or rewind authority.
        begin(root, &pending).unwrap();
        assert_eq!(
            project(root, &ledger, |_| Err(BrokerError::Integrity)),
            Err(BrokerError::Integrity)
        );
        assert!(read_pending(root).unwrap().is_some());
        assert_eq!(
            ledger.registered_runtime(sid).unwrap(),
            Some("new-runtime".into())
        );
    }
}
