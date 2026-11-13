//! Signed, version-pinned service repair downloads. Runtime signatures remain
//! the authority for individual files, including when a package is only a subset.
use crate::{crypto::digest, manifest, protocol::*};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const COMPONENT_FILES: &[&str] = &[
    "carbonpaper-key-service.exe",
    "carbonpaper-protected-setup.exe",
];
pub const RELEASE_METADATA: &str = "service-repair.json";
pub const MAX_COMPONENT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_COMPONENT_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
pub const SIGNING_CONTEXT: &str = "CarbonPaper service repair v1\n";
pub const RELEASE_ROOT: &str = "https://github.com/White-NX/carbonPaper/releases/download";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRelease {
    pub format: u32,
    pub product: String,
    pub architecture: String,
    pub version: String,
    pub service_protocol: u32,
    pub runtime_id: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub signature: String,
}

pub fn runtime_version(runtime_id: &str) -> Result<semver::Version> {
    let (version, suffix) = runtime_id.rsplit_once('-').ok_or(BrokerError::Integrity)?;
    if suffix.len() != 16
        || !suffix
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || version.len() > 83
    {
        return Err(BrokerError::Integrity);
    }
    semver::Version::parse(version).map_err(|_| BrokerError::VersionMismatch)
}

/// Keep the known service identity on equal versions, while honoring any
/// higher version required by a committed owner or a legacy service marker.
pub(crate) fn newest_runtime(
    preferred: Option<String>,
    minimum: Option<String>,
) -> Result<Option<String>> {
    let preferred_version = preferred.as_deref().map(runtime_version).transpose()?;
    let minimum_version = minimum.as_deref().map(runtime_version).transpose()?;
    if minimum_version.is_some_and(|minimum| {
        preferred_version.is_none_or(|version| minimum.cmp_precedence(&version).is_gt())
    }) {
        Ok(minimum)
    } else {
        Ok(preferred)
    }
}

pub fn metadata_url(version: &str) -> Result<String> {
    semver::Version::parse(version).map_err(|_| BrokerError::VersionMismatch)?;
    if version.len() > 83 {
        return Err(BrokerError::LimitExceeded);
    }
    Ok(format!("{RELEASE_ROOT}/v{version}/{RELEASE_METADATA}"))
}

impl RepairRelease {
    pub fn signing_payload(&self) -> String {
        format!(
            "{SIGNING_CONTEXT}{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.format,
            self.product,
            self.architecture,
            self.version,
            self.service_protocol,
            self.runtime_id,
            self.url,
            self.sha256,
            self.size_bytes,
        )
    }

    pub fn verify(&self, expected_version: &str) -> Result<()> {
        self.verify_with_key(expected_version, manifest::RELEASE_PUBLIC_KEY)
    }

    pub fn verify_with_key(&self, expected_version: &str, public_key: &str) -> Result<()> {
        let version = runtime_version(&self.runtime_id)?;
        if self.format != 1
            || self.product != "carbonpaper"
            || self.architecture != "x86_64"
            || self.service_protocol != PROTOCOL_VERSION
            || self.version != expected_version
            || version.to_string() != self.version
            || !valid_id(&self.sha256)
            || self.size_bytes == 0
            || self.size_bytes > MAX_COMPONENT_BYTES
            || self.url
                != format!(
                    "{RELEASE_ROOT}/v{}/carbonpaper_{}_x64_service-repair.zip",
                    self.version, self.version,
                )
        {
            return Err(BrokerError::VersionMismatch);
        }
        let key = base64::engine::general_purpose::STANDARD
            .decode(public_key.trim())
            .map_err(|_| BrokerError::Integrity)?;
        let key = VerifyingKey::from_bytes(
            key.as_slice()
                .try_into()
                .map_err(|_| BrokerError::Integrity)?,
        )
        .map_err(|_| BrokerError::Integrity)?;
        let signature = base64::engine::general_purpose::STANDARD
            .decode(self.signature.trim())
            .map_err(|_| BrokerError::Integrity)?;
        let signature = Signature::from_slice(&signature).map_err(|_| BrokerError::Integrity)?;
        key.verify(self.signing_payload().as_bytes(), &signature)
            .map_err(|_| BrokerError::Integrity)
    }

    pub fn verify_runtime(&self, bytes: &[u8]) -> Result<()> {
        if format!("{}-{}", self.version, &digest(bytes)[..16]) != self.runtime_id {
            return Err(BrokerError::Integrity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn repair_signature_pins_version_runtime_download_and_size() {
        let key = SigningKey::from_bytes(&[37; 32]);
        let public =
            base64::engine::general_purpose::STANDARD.encode(key.verifying_key().as_bytes());
        let mut release = RepairRelease {
            format: 1,
            product: "carbonpaper".into(),
            architecture: "x86_64".into(),
            version: "1.2.3".into(),
            service_protocol: PROTOCOL_VERSION,
            runtime_id: "1.2.3-0123456789abcdef".into(),
            url: format!("{RELEASE_ROOT}/v1.2.3/carbonpaper_1.2.3_x64_service-repair.zip"),
            sha256: "a".repeat(64),
            size_bytes: 100,
            signature: String::new(),
        };
        release.signature = base64::engine::general_purpose::STANDARD
            .encode(key.sign(release.signing_payload().as_bytes()).to_bytes());
        assert!(release.verify_with_key("1.2.3", &public).is_ok());
        assert!(release.verify_with_key("1.2.2", &public).is_err());
        let mut changed = release.clone();
        changed.size_bytes += 1;
        assert!(changed.verify_with_key("1.2.3", &public).is_err());
        changed = release.clone();
        changed.runtime_id = "1.2.3-fedcba9876543210".into();
        assert!(changed.verify_with_key("1.2.3", &public).is_err());
        changed = release;
        changed.url = "https://example.com/repair.zip".into();
        assert!(changed.verify_with_key("1.2.3", &public).is_err());
    }

    #[test]
    fn recorded_runtime_versions_keep_prerelease_and_build_identifiers() {
        assert_eq!(
            runtime_version("1.2.3-beta.4+build.5-0123456789abcdef")
                .unwrap()
                .to_string(),
            "1.2.3-beta.4+build.5"
        );
        assert!(runtime_version("1.2.3-../../bad").is_err());
        assert!(metadata_url("../../latest").is_err());
    }

    #[test]
    fn shared_service_floor_preserves_legacy_versions_and_committed_owner_requirements() {
        let older = "1.2.3-0123456789abcdef";
        let newer = "1.2.4-0123456789abcdef";
        assert_eq!(
            newest_runtime(Some(newer.into()), Some(older.into()))
                .unwrap()
                .as_deref(),
            Some(newer)
        );
        assert_eq!(
            newest_runtime(Some(older.into()), Some(newer.into()))
                .unwrap()
                .as_deref(),
            Some(newer)
        );
        assert_eq!(
            newest_runtime(None, Some(newer.into())).unwrap().as_deref(),
            Some(newer)
        );
        assert_eq!(
            newest_runtime(Some(newer.into()), Some("1.2.4-fedcba9876543210".into()))
                .unwrap()
                .as_deref(),
            Some(newer)
        );
        assert!(newest_runtime(Some("corrupt".into()), Some(newer.into())).is_err());
    }
}
