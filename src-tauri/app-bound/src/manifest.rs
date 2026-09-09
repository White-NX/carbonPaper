//! Detached signatures cover the exact manifest bytes with a domain separator.
use crate::{crypto::digest, protocol::*};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const MANIFEST_NAME: &str = "protected-runtime.json";
pub const SIGNATURE_NAME: &str = "protected-runtime.sig";
pub const SIGNING_CONTEXT: &[u8] = b"CarbonPaper protected runtime v1\n";
pub const RELEASE_PUBLIC_KEY: &str = include_str!("../../update-public-key.txt");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub format: u32,
    pub product: String,
    pub version: String,
    pub architecture: String,
    pub service_protocol: u32,
    pub files: BTreeMap<String, String>,
}

impl ReleaseManifest {
    pub fn validate(&self) -> Result<()> {
        if self.format != 1
            || self.product != "carbonpaper"
            || self.architecture != "x86_64"
            || self.service_protocol != PROTOCOL_VERSION
            || self.files.len() > 4096
            || self.version.len() > 83
            || semver::Version::parse(&self.version).is_err()
        {
            return Err(BrokerError::VersionMismatch);
        }
        for required in [
            "carbonpaper.exe",
            "carbonpaper-python.exe",
            "carbonpaper-key-service.exe",
            "carbonpaper-protected-setup.exe",
            "carbonpaper-semantic-worker.exe",
            "carbonpaper-ml.exe",
            "carbonpaper-office.exe",
            "carbonpaper-nmh.exe",
            "monitor.pyz",
        ] {
            if !self.files.contains_key(required) {
                return Err(BrokerError::Integrity);
            }
        }
        let mut names = BTreeSet::new();
        for (path, hash) in &self.files {
            safe_relative_path(path)?;
            if !valid_id(hash) || !names.insert(path.to_ascii_lowercase()) {
                return Err(BrokerError::Integrity);
            }
        }
        Ok(())
    }
}

pub fn verify_manifest(bytes: &[u8], signature: &str, public_key: &str) -> Result<ReleaseManifest> {
    if bytes.len() > 1024 * 1024 {
        return Err(BrokerError::LimitExceeded);
    }
    let public_key = base64::engine::general_purpose::STANDARD
        .decode(public_key.trim())
        .map_err(|_| BrokerError::Integrity)?;
    let key = VerifyingKey::from_bytes(
        public_key
            .as_slice()
            .try_into()
            .map_err(|_| BrokerError::Integrity)?,
    )
    .map_err(|_| BrokerError::Integrity)?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature.trim())
        .map_err(|_| BrokerError::Integrity)?;
    let signature = Signature::from_slice(&signature).map_err(|_| BrokerError::Integrity)?;
    let mut signed = SIGNING_CONTEXT.to_vec();
    signed.extend_from_slice(bytes);
    key.verify(&signed, &signature)
        .map_err(|_| BrokerError::Integrity)?;
    let manifest: ReleaseManifest =
        serde_json::from_slice(bytes).map_err(|_| BrokerError::Integrity)?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    #[cfg(windows)]
    let file = crate::windows::identity::locked_file(path)?;
    #[cfg(not(windows))]
    let file = std::fs::File::open(path).map_err(|_| BrokerError::Integrity)?;
    let metadata = file.metadata().map_err(|_| BrokerError::Integrity)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(BrokerError::LimitExceeded);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BrokerError::Integrity)?;
    if bytes.len() as u64 > limit {
        return Err(BrokerError::LimitExceeded);
    }
    Ok(bytes)
}

pub fn read_manifest(directory: &Path) -> Result<(ReleaseManifest, String)> {
    let bytes = read_bounded(&directory.join(MANIFEST_NAME), 1024 * 1024)?;
    let signature = String::from_utf8(read_bounded(&directory.join(SIGNATURE_NAME), 256)?)
        .map_err(|_| BrokerError::Integrity)?;
    let manifest = verify_manifest(&bytes, &signature, RELEASE_PUBLIC_KEY)?;
    let id = format!("{}-{}", manifest.version, &digest(&bytes)[..16]);
    Ok((manifest, id))
}

pub fn safe_relative_path(value: &str) -> Result<PathBuf> {
    // Validate using Windows rules on all test hosts, including ADS and device names.
    if value.is_empty()
        || !value.is_ascii()
        || value.len() > 240
        || value.contains(['\\', ':', '\0'])
        || value.starts_with('/')
    {
        return Err(BrokerError::Integrity);
    }
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.bytes().any(|b| b < 32 || b"<>\"|?*".contains(&b))
            || ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(BrokerError::Integrity);
        }
    }
    let path = PathBuf::from(value);
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(BrokerError::Integrity);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn signature_covers_all_files_and_rejects_update_manifest_reuse() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let files = [
            "carbonpaper.exe",
            "carbonpaper-python.exe",
            "carbonpaper-key-service.exe",
            "carbonpaper-protected-setup.exe",
            "carbonpaper-semantic-worker.exe",
            "carbonpaper-ml.exe",
            "carbonpaper-office.exe",
            "carbonpaper-nmh.exe",
            "monitor.pyz",
        ]
        .into_iter()
        .map(|v| (v.into(), "a".repeat(64)))
        .collect();
        let manifest = ReleaseManifest {
            format: 1,
            product: "carbonpaper".into(),
            version: "0.8.5".into(),
            architecture: "x86_64".into(),
            service_protocol: 1,
            files,
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut payload = SIGNING_CONTEXT.to_vec();
        payload.extend_from_slice(&bytes);
        let sig = base64::engine::general_purpose::STANDARD.encode(key.sign(&payload).to_bytes());
        let pk = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_bytes());
        assert!(verify_manifest(&bytes, &sig, &pk).is_ok());
        let wrong_sig =
            base64::engine::general_purpose::STANDARD.encode(key.sign(&bytes).to_bytes());
        assert!(verify_manifest(&bytes, &wrong_sig, &pk).is_err());
        let mut changed = bytes;
        changed.push(b' ');
        assert!(verify_manifest(&changed, &sig, &pk).is_err());
    }
    #[test]
    fn rejects_windows_path_escapes_and_aliases() {
        for path in [
            "../evil",
            "C:/evil",
            "a\\b",
            "a:stream",
            "//server/share",
            "a/CON.dll",
            "a./b",
            "a//b",
            "LPT1",
            "a/../b",
        ] {
            assert!(safe_relative_path(path).is_err(), "{path}");
        }
        assert!(safe_relative_path("onnxruntime/1.24.2/onnxruntime.dll").is_ok());
    }
}
