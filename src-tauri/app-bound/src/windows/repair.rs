//! Fixed-root repair of an existing registration. Partial packages cannot
//! register owners, change policy, reset the ledger, or supply arbitrary targets.
use super::{activation, identity, install};
use crate::{ledger::Ledger, manifest, protocol::*, repair::COMPONENT_FILES};
use std::{
    fs::File,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use windows::Win32::UI::Shell::IsUserAnAdmin;

const CHECKPOINT: &str = "repair-runtime.json";

pub struct RepairPackage {
    pub directory: PathBuf,
    pub release: manifest::ReleaseManifest,
    pub runtime_id: String,
    // Pin metadata and executable bytes across elevation and execution.
    _files: Vec<File>,
}

pub fn read_package(directory: &Path) -> Result<RepairPackage> {
    let directory = directory
        .canonicalize()
        .map_err(|_| BrokerError::Integrity)?;
    let mut files = vec![
        identity::locked_file(&directory.join(manifest::MANIFEST_NAME))?,
        identity::locked_file(&directory.join(manifest::SIGNATURE_NAME))?,
    ];
    let (release, runtime_id) = manifest::read_manifest(&directory)?;
    let mut size = 0u64;
    for name in COMPONENT_FILES {
        let mut file = identity::locked_file(&directory.join(name))?;
        size = size.saturating_add(file.metadata().map_err(|_| BrokerError::Integrity)?.len());
        if size > crate::repair::MAX_COMPONENT_EXPANDED_BYTES {
            return Err(BrokerError::LimitExceeded);
        }
        identity::verify_file(
            &mut file,
            release.files.get(*name).ok_or(BrokerError::Integrity)?,
        )?;
        files.push(file);
    }
    Ok(RepairPackage {
        directory,
        release,
        runtime_id,
        _files: files,
    })
}

pub fn files_valid(
    directory: &Path,
    release: &manifest::ReleaseManifest,
    components: bool,
) -> bool {
    release
        .files
        .iter()
        .filter(|(name, _)| components || !COMPONENT_FILES.contains(&name.as_str()))
        .all(|(relative, expected)| {
            manifest::safe_relative_path(relative)
                .and_then(|relative| {
                    identity::verify_file(
                        &mut identity::locked_file(&directory.join(relative))?,
                        expected,
                    )
                })
                .is_ok()
        })
}

fn read_pointer(path: &Path) -> Result<Option<String>> {
    if !path.try_exists().map_err(|_| BrokerError::Storage)? {
        return Ok(None);
    }
    identity::assert_protected(path)?;
    let record: identity::Activation = serde_json::from_slice(&manifest::read_bounded(path, 1024)?)
        .map_err(|_| BrokerError::Integrity)?;
    if !identity::safe_runtime_id(&record.runtime_id) {
        return Err(BrokerError::Integrity);
    }
    crate::repair::runtime_version(&record.runtime_id)?;
    Ok(Some(record.runtime_id))
}

/// Readable discovery only. The elevated repair always consults the ledger.
pub fn service_checkpoint() -> Result<Option<String>> {
    let root = identity::protected_root()?;
    read_pointer(&root.join("System").join(CHECKPOINT))
        .or_else(|_| read_pointer(&root.join("System/runtime.json")))
        .and_then(|id| match id {
            Some(id) => Ok(Some(id)),
            None => read_pointer(&root.join("System/runtime.json")),
        })
}

pub(super) fn write_checkpoint(root: &Path, ledger: &Ledger) -> Result<()> {
    if let Some(runtime_id) = ledger.required_service_runtime()? {
        let bytes = serde_json::to_vec(&identity::Activation { runtime_id })
            .map_err(|_| BrokerError::Storage)?;
        let path = root.join("System").join(CHECKPOINT);
        if manifest::read_bounded(&path, 1024).ok().as_deref() != Some(bytes.as_slice()) {
            install::atomic_write(&path, &bytes)?;
        }
    }
    Ok(())
}

/// Seed legacy installations only from a protected, signed runtime identity.
#[cfg(not(test))]
pub(super) fn remember_installed_service(root: &Path, ledger: &Ledger) -> Result<()> {
    let id = read_pointer(&root.join("System/runtime.json"))?.ok_or(BrokerError::Integrity)?;
    let directory = root.join("Runtime").join(&id);
    identity::assert_protected(&directory)?;
    let (release, actual_id) = manifest::read_manifest(&directory)?;
    if actual_id != id {
        return Err(BrokerError::Integrity);
    }
    identity::verify_file(
        &mut identity::locked_file(&root.join("System/carbonpaper-key-service.exe"))?,
        release
            .files
            .get("carbonpaper-key-service.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    ledger.remember_service_runtime(&id)?;
    write_checkpoint(root, ledger)
}

pub struct RepairOptions {
    pub source: PathBuf,
    pub runtime_source: PathBuf,
    pub tool_source: PathBuf,
    pub caller_pid: u32,
    pub full_runtime: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("The complete runtime package is required")]
    FullRuntimeRequired,
    #[error(transparent)]
    Broker(#[from] BrokerError),
}

pub const FULL_RUNTIME_EXIT_CODE: i32 = 20;
pub const VERSION_EXIT_CODE: i32 = 21;

fn verify_requester(
    caller: &identity::ProcessIdentity,
    runtime: &RepairPackage,
    tool: &RepairPackage,
) -> Result<()> {
    let signed = caller
        .image
        .parent()
        .and_then(|directory| manifest::read_manifest(directory).ok())
        .map(|(release, _)| release)
        .unwrap_or_else(|| runtime.release.clone());
    let expected = signed
        .files
        .get("carbonpaper.exe")
        .ok_or(BrokerError::Integrity)?;
    identity::verify_file(&mut identity::locked_file(&caller.image)?, expected).or_else(|_| {
        identity::verify_file(
            &mut identity::locked_file(&caller.image)?,
            tool.release
                .files
                .get("carbonpaper.exe")
                .ok_or(BrokerError::Integrity)?,
        )
    })
}

fn preflight_target(
    root: &Path,
    package: &RepairPackage,
    allow_full: bool,
) -> std::result::Result<bool, RepairError> {
    let destination = root.join("Runtime").join(&package.runtime_id);
    if destination.exists() {
        identity::assert_protected(&destination)?;
    }
    let full = !files_valid(&destination, &package.release, false);
    if full && (!allow_full || !files_valid(&package.directory, &package.release, true)) {
        return Err(RepairError::FullRuntimeRequired);
    }
    Ok(full)
}

fn repair_target(root: &Path, package: &RepairPackage, full: bool, complete: bool) -> Result<()> {
    let destination = root.join("Runtime").join(&package.runtime_id);
    identity::ensure_directory(&destination, true)?;
    let mut release = package.release.clone();
    if !full {
        release
            .files
            .retain(|name, _| COMPONENT_FILES.contains(&name.as_str()));
    }
    // The immutable parent fixes the target names. Restore the standard ACL on
    // these two signed components without changing any archive or state files.
    for name in COMPONENT_FILES {
        let path = destination.join(name);
        if path.exists() {
            identity::no_reparse(&path)?;
            identity::protect_path(&path, true)?;
        }
    }
    install::repair_runtime(
        &package.directory,
        &destination,
        &release,
        &package.runtime_id,
    )?;
    install::verify_runtime_files(
        &destination,
        if complete { &package.release } else { &release },
    )
}

pub fn repair(options: RepairOptions) -> std::result::Result<PathBuf, RepairError> {
    // SAFETY: this reads the effective token and does not change privileges.
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return Err(BrokerError::AccessDenied.into());
    }
    let service = read_package(&options.source)?;
    let runtime = read_package(&options.runtime_source)?;
    let tool = read_package(&options.tool_source)?;
    let caller = identity::inspect_process(options.caller_pid)?;
    verify_requester(&caller, &runtime, &tool)?;
    // Direct invocation must also be this package's verified native helper.
    let executable = std::env::current_exe().map_err(|_| BrokerError::Integrity)?;
    identity::verify_file(
        &mut identity::locked_file(&executable)?,
        tool.release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or(BrokerError::Integrity)?,
    )?;
    let tool_version = crate::repair::runtime_version(&tool.runtime_id)?;
    for package in [&service, &runtime] {
        if tool_version
            .cmp_precedence(&crate::repair::runtime_version(&package.runtime_id)?)
            .is_lt()
        {
            return Err(BrokerError::VersionMismatch.into());
        }
    }
    let root = identity::protected_root()?;
    let state = identity::state_root()?;
    for path in [&root, &root.join("System"), &root.join("Runtime"), &state] {
        identity::assert_protected(path)?;
    }
    let lock_path = root.join("System/setup.lock");
    identity::assert_protected(&lock_path)?;
    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&lock_path)
        .map_err(|_| BrokerError::Busy)?;
    let ledger_path = state.join("keys.db");
    identity::assert_protected(&ledger_path)?;
    let ledger = Ledger::open_existing(&ledger_path)?;
    let owner_id = ledger
        .registered_runtime(&caller.principal.sid)?
        .ok_or(BrokerError::AccessDenied)?;
    if owner_id != runtime.runtime_id {
        crate::repair::runtime_version(&owner_id)?;
        install::atomic_write(
            &root
                .join("Activations")
                .join(format!("{}.json", caller.principal.sid)),
            &serde_json::to_vec(&identity::Activation {
                runtime_id: owner_id,
            })
            .map_err(|_| BrokerError::Storage)?,
        )?;
        write_checkpoint(&root, &ledger)?;
        return Err(BrokerError::VersionMismatch.into());
    }
    let expected_service = match ledger.service_runtime()? {
        Some(id) => id,
        None => {
            match read_pointer(&root.join("System/runtime.json")) {
                Ok(Some(id)) => id,
                _ => {
                    // A corrupt legacy marker alone may be reconstructed, but
                    // cannot authorize replacement of an unknown service version.
                    identity::verify_file(
                        &mut identity::locked_file(
                            &root.join("System/carbonpaper-key-service.exe"),
                        )?,
                        service
                            .release
                            .files
                            .get("carbonpaper-key-service.exe")
                            .ok_or(BrokerError::Integrity)?,
                    )?;
                    service.runtime_id.clone()
                }
            }
        }
    };
    let expected_service =
        crate::repair::newest_runtime(Some(expected_service), ledger.required_service_runtime()?)?
            .ok_or(BrokerError::Integrity)?;
    ledger.remember_service_runtime(&expected_service)?;
    if expected_service != service.runtime_id {
        write_checkpoint(&root, &ledger)?;
        return Err(BrokerError::VersionMismatch.into());
    }
    // Complete the source/target checks before stopping the service or replacing
    // files. Missing workers cannot turn a partial package into a runtime.
    let runtime_full = preflight_target(&root, &runtime, options.full_runtime)?;
    // Other users' workers are repaired by their own runtime recovery. A shared
    // service needs its signed service/helper snapshot, not every user's worker.
    activation::begin(
        &root,
        &activation::PendingActivation {
            sid: caller.principal.sid.clone(),
            runtime_id: runtime.runtime_id.clone(),
            system_runtime_id: service.runtime_id.clone(),
        },
    )?;
    install::stop_service()?;
    let repaired: std::result::Result<(), RepairError> = (|| {
        // Refresh inbox permissions while preserving pending retirement records.
        super::revocation::provision(&state, &caller.principal.sid)?;
        repair_target(&root, &runtime, runtime_full, true)?;
        if service.runtime_id != runtime.runtime_id {
            repair_target(&root, &service, false, false)?;
        }
        for name in COMPONENT_FILES {
            let target = root.join("System").join(name);
            if target.exists() {
                identity::no_reparse(&target)?;
                identity::protect_path(&target, true)?;
            }
            let expected = service
                .release
                .files
                .get(*name)
                .ok_or(BrokerError::Integrity)?;
            if identity::locked_file(&target)
                .and_then(|mut file| identity::verify_file(&mut file, expected))
                .is_err()
            {
                if target.exists() {
                    identity::assert_protected(&target)?;
                }
                install::copy_verified(&service.directory.join(name), &target, expected)?;
            }
        }
        install::atomic_write(
            &root.join("System/runtime.json"),
            &serde_json::to_vec(&identity::Activation {
                runtime_id: service.runtime_id.clone(),
            })
            .map_err(|_| BrokerError::Storage)?,
        )?;
        ledger.remember_service_runtime(&service.runtime_id)?;
        write_checkpoint(&root, &ledger)?;
        install::configure_service(&root.join("System/carbonpaper-key-service.exe"))?;
        activation::reconcile(&root, &ledger)?;
        install::start_service()?;
        Ok(())
    })();
    if repaired.is_err() {
        // Keep other owners usable after a failed file replacement. The
        // durable fence remains for the next repair/startup reconciliation.
        let _ = install::start_service();
    }
    repaired?;
    Ok(root.join("Runtime").join(&runtime.runtime_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn partial_repair_requires_full_source_for_missing_workers() {
        let directory = tempfile::tempdir().unwrap();
        let mut files = BTreeMap::new();
        files.insert(
            "carbonpaper-semantic-worker.exe".into(),
            crate::crypto::digest(b"worker"),
        );
        files.insert(
            "carbonpaper-key-service.exe".into(),
            crate::crypto::digest(b"service"),
        );
        let release = manifest::ReleaseManifest {
            format: 1,
            product: manifest::PRODUCT.into(),
            version: "1.2.3".into(),
            architecture: "x86_64".into(),
            service_protocol: PROTOCOL_VERSION,
            files,
        };
        std::fs::write(
            directory.path().join("carbonpaper-semantic-worker.exe"),
            b"worker",
        )
        .unwrap();
        assert!(files_valid(directory.path(), &release, false));
        assert!(!files_valid(directory.path(), &release, true));
        std::fs::write(
            directory.path().join("carbonpaper-semantic-worker.exe"),
            b"corrupt",
        )
        .unwrap();
        assert!(!files_valid(directory.path(), &release, false));
    }
}
