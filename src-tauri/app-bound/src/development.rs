//! Separate, build-time development identity. This module is never in release
//! builds and never selects production service names, roots or signing keys.
use crate::{
    manifest::{self, ReleaseManifest},
    protocol::*,
    windows::identity::{self, ProcessIdentity, VerifiedCaller},
};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom},
    os::windows::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf, Prefix},
};
use zeroize::Zeroizing;

pub const INSTANCE_ID: &str = env!("CARBONPAPER_APP_BOUND_DEV_INSTANCE");
pub const USER_DIRECTORY_NAME: &str = concat!(
    "CarbonPaperDev-",
    env!("CARBONPAPER_APP_BOUND_DEV_INSTANCE")
);
pub const CLIENT_FILE: &str = "development-client.json";
const HELPERS: [&str; 2] = [
    "carbonpaper-key-service.exe",
    "carbonpaper-protected-setup.exe",
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientRegistration {
    format: u32,
    instance_id: String,
    executable_path: PathBuf,
    executable_sha256: String,
}

impl ClientRegistration {
    fn validate(&self) -> Result<()> {
        let local = matches!(self.executable_path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
        if self.format != 1
            || self.instance_id != INSTANCE_ID
            || !local
            || !self.executable_path.is_absolute()
            || self.executable_path.file_name().is_none_or(|name| {
                !name
                    .to_string_lossy()
                    .eq_ignore_ascii_case("carbonpaper.exe")
            })
            || !valid_id(&self.executable_sha256)
        {
            return Err(BrokerError::Integrity);
        }
        Ok(())
    }
}

/// Build cache and signed registration material, independent of app settings.
pub fn user_root() -> Result<PathBuf> {
    Ok(identity::local_app_data_directory()?.join(USER_DIRECTORY_NAME))
}

pub fn package_directory() -> Result<PathBuf> {
    Ok(user_root()?.join("package"))
}

fn checksum(path: &Path) -> Result<String> {
    let mut file = identity::locked_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let length = file.read(&mut buffer).map_err(|_| BrokerError::Storage)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn client_registration(directory: &Path, release: &ReleaseManifest) -> Result<ClientRegistration> {
    let bytes = manifest::read_bounded(&directory.join(CLIENT_FILE), 64 * 1024)?;
    if release.files.get(CLIENT_FILE) != Some(&crate::crypto::digest(&bytes)) {
        return Err(BrokerError::Integrity);
    }
    let registration: ClientRegistration =
        serde_json::from_slice(&bytes).map_err(|_| BrokerError::Integrity)?;
    registration.validate()?;
    Ok(registration)
}

fn verify_registered_image(
    process: &ProcessIdentity,
    registration: &ClientRegistration,
) -> Result<fs::File> {
    if !identity::path_eq(&process.image, &registration.executable_path) {
        return Err(BrokerError::AccessDenied);
    }
    let mut image = identity::locked_file(&process.image)?;
    identity::verify_file(&mut image, &registration.executable_sha256)?;
    Ok(image)
}

pub(crate) fn verify_bootstrap_caller(
    process: &ProcessIdentity,
    source: &Path,
    release: &ReleaseManifest,
) -> Result<()> {
    // The installer already verified the development signature. Its signed
    // metadata binds the OS-derived caller to one exact debug image and path.
    verify_registered_image(process, &client_registration(source, release)?)?;
    Ok(())
}

pub(crate) fn verify_client(mut process: ProcessIdentity) -> Result<VerifiedCaller> {
    let runtime = identity::active_runtime_for_sid(&process.principal.sid)?
        .ok_or(BrokerError::AccessDenied)?;
    let (release, runtime_id) = manifest::read_manifest(&runtime)?;
    let image_lock = verify_registered_image(&process, &client_registration(&runtime, &release)?)?;
    process.principal.runtime_id = runtime_id;
    Ok(VerifiedCaller {
        principal: process.principal,
        image_lock,
    })
}

/// Assemble a tiny signed broker package after Cargo has linked the desktop.
/// The debug desktop stays in Cargo's target directory for Tauri/Vite hot reload.
/// Only its signed path/hash registration is copied into protected storage.
pub fn prepare_package(version: &str) -> std::result::Result<PathBuf, String> {
    let result = (|| -> Result<PathBuf> {
        let root = user_root()?;
        fs::create_dir_all(&root).map_err(|_| BrokerError::Storage)?;
        let _lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(root.join("package.lock"))
            .map_err(|_| BrokerError::Busy)?;
        let source_image = std::env::current_exe().map_err(|_| BrokerError::Unavailable)?;
        let registration = ClientRegistration {
            format: 1,
            instance_id: INSTANCE_ID.into(),
            executable_path: source_image.clone(),
            executable_sha256: checksum(&source_image)?,
        };
        registration.validate()?;
        let registration_bytes =
            serde_json::to_vec(&registration).map_err(|_| BrokerError::Integrity)?;
        let package = package_directory()?;
        fs::create_dir_all(&package).map_err(|_| BrokerError::Storage)?;
        let mut files = BTreeMap::new();
        for name in HELPERS {
            let source = root.join("components").join(name);
            let expected = checksum(&source)?;
            let target = package.join(name);
            if checksum(&target).ok().as_deref() != Some(&expected) {
                let temporary = target.with_extension(format!("tmp-{}", &random_id()[..16]));
                let mut input = identity::locked_file(&source)?;
                identity::verify_file(&mut input, &expected)?;
                input
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| BrokerError::Storage)?;
                let copied = (|| -> Result<()> {
                    let mut output = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&temporary)
                        .map_err(|_| BrokerError::Storage)?;
                    std::io::copy(&mut input, &mut output).map_err(|_| BrokerError::Storage)?;
                    output.sync_all().map_err(|_| BrokerError::Storage)?;
                    drop(output);
                    identity::verify_file(&mut identity::locked_file(&temporary)?, &expected)?;
                    fs::rename(&temporary, &target).map_err(|_| BrokerError::Storage)
                })();
                if copied.is_err() {
                    let _ = fs::remove_file(&temporary);
                }
                copied?;
            }
            files.insert(name.into(), expected);
        }
        files.insert(
            CLIENT_FILE.into(),
            crate::crypto::digest(&registration_bytes),
        );
        let release = ReleaseManifest {
            format: 1,
            product: manifest::PRODUCT.into(),
            version: version.into(),
            architecture: "x86_64".into(),
            service_protocol: PROTOCOL_VERSION,
            files,
        };
        release.validate()?;
        let bytes = serde_json::to_vec(&release).map_err(|_| BrokerError::Integrity)?;
        let encoded = Zeroizing::new(manifest::read_bounded(&root.join("signing-seed.txt"), 256)?);
        let seed = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(
                    std::str::from_utf8(&encoded)
                        .map_err(|_| BrokerError::Integrity)?
                        .trim(),
                )
                .map_err(|_| BrokerError::Integrity)?,
        );
        let key = SigningKey::from_bytes(
            seed.as_slice()
                .try_into()
                .map_err(|_| BrokerError::Integrity)?,
        );
        if base64::engine::general_purpose::STANDARD.encode(key.verifying_key().as_bytes())
            != manifest::RELEASE_PUBLIC_KEY
        {
            return Err(BrokerError::Integrity);
        }
        let mut signed = manifest::SIGNING_CONTEXT.to_vec();
        signed.extend_from_slice(&bytes);
        let signature =
            base64::engine::general_purpose::STANDARD.encode(key.sign(&signed).to_bytes());
        for (name, data) in [
            (CLIENT_FILE, registration_bytes.as_slice()),
            (manifest::MANIFEST_NAME, bytes.as_slice()),
            (manifest::SIGNATURE_NAME, signature.as_bytes()),
        ] {
            if manifest::read_bounded(&package.join(name), 1024 * 1024)
                .ok()
                .as_deref()
                != Some(data)
            {
                crate::windows::install::atomic_write(&package.join(name), data)?;
            }
        }
        manifest::read_manifest(&package)?;
        Ok(package)
    })();
    result.map_err(|e| format!("Cannot prepare the development broker package: {e}"))
}

pub fn install_package(source: &Path, enable: bool) -> std::result::Result<PathBuf, String> {
    let (release, _) = manifest::read_manifest(source).map_err(|e| e.to_string())?;
    let setup = source.join("carbonpaper-protected-setup.exe");
    let mut locked = identity::locked_file(&setup).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut locked,
        release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or("Installer is missing")?,
    )
    .map_err(|e| e.to_string())?;
    let mut command = runas::Command::new(&setup);
    command
        .arg("--source")
        .arg(source)
        .arg("--caller-pid")
        .arg(std::process::id().to_string())
        .gui(true)
        .show(false);
    if enable {
        command.arg("--enable");
    }
    let status = command.status().map_err(|error| {
        if error.raw_os_error() == Some(1223) {
            "APP_BOUND_CANCELLED".to_string()
        } else {
            format!("APP_BOUND_INSTALL_FAILED: {error}")
        }
    })?;
    if !status.success() {
        return Err("APP_BOUND_INSTALL_FAILED".into());
    }
    identity::active_runtime()
        .map_err(|e| e.to_string())?
        .ok_or("Development registration is missing".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_identity_is_separate_from_release() {
        assert_ne!(SERVICE_NAME, "CarbonPaperKeyService");
        assert_ne!(PIPE_NAME, r"\\.\pipe\CarbonPaper.AppBound.v1");
        assert_ne!(manifest::PRODUCT, "carbonpaper");
        assert_ne!(
            manifest::RELEASE_PUBLIC_KEY,
            include_str!("../../update-public-key.txt").trim()
        );
        assert!(identity::protected_root()
            .unwrap()
            .components()
            .any(|p| p.as_os_str() == USER_DIRECTORY_NAME));
        assert!(!identity::state_root()
            .unwrap()
            .to_string_lossy()
            .contains("CarbonPaperKeyService\\State"));
    }

    #[test]
    fn client_registration_rejects_foreign_instances_and_nonlocal_paths() {
        let mut client = ClientRegistration {
            format: 1,
            instance_id: INSTANCE_ID.into(),
            executable_path: PathBuf::from(r"D:\workspace\target\debug\carbonpaper.exe"),
            executable_sha256: "a".repeat(64),
        };
        assert!(client.validate().is_ok());
        client.instance_id = "another-instance".into();
        assert_eq!(client.validate(), Err(BrokerError::Integrity));
        client.instance_id = INSTANCE_ID.into();
        for path in [
            r"\\server\share\carbonpaper.exe",
            r"relative\carbonpaper.exe",
            r"D:\workspace\python.exe",
        ] {
            client.executable_path = path.into();
            assert_eq!(client.validate(), Err(BrokerError::Integrity));
        }
    }
}
