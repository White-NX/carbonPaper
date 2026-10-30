//! Stable, canonical privacy identities for recap caches.
use crate::sensitive_filter::SensitiveFilterConfig;

use super::types::digest;

pub(crate) struct PrivacyFingerprint {
    hash: String,
}

impl PrivacyFingerprint {
    pub(crate) fn new(config: SensitiveFilterConfig) -> Result<Self, String> {
        let mut value = serde_json::to_value(&config).map_err(|_| "RECAP_INVALID_SETTINGS")?;
        // Explicitly sort recursively, including when serde_json's preserve_order
        // feature is enabled by another dependency.
        value.sort_all_objects();
        Ok(Self {
            hash: format!("v1:{}", digest(&value.to_string())),
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_policy_reloads_have_one_fingerprint() {
        let config = SensitiveFilterConfig::default();
        let saved = serde_json::to_value(&config).unwrap();
        let expected = PrivacyFingerprint::new(config).unwrap();
        for _ in 0..64 {
            let loaded: SensitiveFilterConfig = serde_json::from_value(saved.clone()).unwrap();
            let actual = PrivacyFingerprint::new(loaded).unwrap();
            assert_eq!(expected.as_str(), actual.as_str());
        }
    }

    #[test]
    fn actual_privacy_changes_have_distinct_fingerprints() {
        let config = SensitiveFilterConfig::default();
        let original = PrivacyFingerprint::new(config.clone()).unwrap();
        let mut variants = Vec::new();
        let mut changed = config.clone();
        changed.enabled = !changed.enabled;
        variants.push(changed);
        let mut changed = config.clone();
        changed.categories.insert("cat_01".into(), false);
        variants.push(changed);
        let mut changed = config.clone();
        changed.mode = "mask".into();
        variants.push(changed);
        let mut changed = config.clone();
        changed.pii_enabled = !changed.pii_enabled;
        variants.push(changed);
        let mut changed = config.clone();
        changed.pii_entities.clear();
        variants.push(changed);
        let mut changed = config.clone();
        changed.pii_mask_long_numbers = !changed.pii_mask_long_numbers;
        variants.push(changed);
        let mut changed = config;
        changed.version += 1;
        variants.push(changed);
        for changed in variants {
            let fingerprint = PrivacyFingerprint::new(changed).unwrap();
            assert_ne!(original.as_str(), fingerprint.as_str());
        }
    }
}
