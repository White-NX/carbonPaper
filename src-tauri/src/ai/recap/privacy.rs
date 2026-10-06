//! Stable privacy identities, with bounded compatibility for pre-canonical caches.
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Serialize, Serializer};

use crate::sensitive_filter::SensitiveFilterConfig;

use super::types::digest;

pub(crate) struct PrivacyFingerprint {
    hash: String,
    config: SensitiveFilterConfig,
}

impl PrivacyFingerprint {
    pub(crate) fn new(config: SensitiveFilterConfig) -> Result<Self, String> {
        let mut value = serde_json::to_value(&config).map_err(|_| "RECAP_INVALID_SETTINGS")?;
        // Explicitly sort recursively, including when serde_json's preserve_order
        // feature is enabled by another dependency.
        value.sort_all_objects();
        Ok(Self {
            hash: format!("v1:{}", digest(&value.to_string())),
            config,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.hash
    }

    pub(crate) fn matches_legacy(&self, hash: &str) -> Result<bool, String> {
        // The old format hashed the struct directly, so only category order
        // varied. Prove equivalence before adopting a legacy day; never relabel
        // its payload revisions, which may have been invalidated by source edits.
        if self.config.version != 2
            || hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Ok(false);
        }
        let mut categories = self.config.categories.iter().collect::<Vec<_>>();
        // Production has five categories. Bound legacy work to at most 6! hashes
        // for unexpected policies; larger maps safely regenerate once.
        if categories.len() > 6 {
            return Ok(false);
        }
        fn search(
            config: &SensitiveFilterConfig,
            categories: &mut [(&String, &bool)],
            offset: usize,
            hash: &str,
        ) -> Result<bool, String> {
            if offset == categories.len() {
                let serialized = serde_json::to_string(&LegacyConfig { config, categories })
                    .map_err(|_| "RECAP_INVALID_SETTINGS")?;
                return Ok(digest(&serialized) == hash);
            }
            for i in offset..categories.len() {
                categories.swap(offset, i);
                let matched = search(config, categories, offset + 1, hash)?;
                categories.swap(offset, i);
                if matched {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        search(&self.config, &mut categories, 0, hash)
    }
}

// Frozen serialization of the old SensitiveFilterConfig layout. New config
// fields belong only to the canonical fingerprint and must not be added here.
struct LegacyConfig<'a> {
    config: &'a SensitiveFilterConfig,
    categories: &'a [(&'a String, &'a bool)],
}

struct LegacyCategories<'a>(&'a [(&'a String, &'a bool)]);

impl Serialize for LegacyCategories<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Serialize for LegacyConfig<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("SensitiveFilterConfig", 7)?;
        state.serialize_field("enabled", &self.config.enabled)?;
        state.serialize_field("categories", &LegacyCategories(self.categories))?;
        state.serialize_field("mode", &self.config.mode)?;
        state.serialize_field("pii_enabled", &self.config.pii_enabled)?;
        state.serialize_field("pii_entities", &self.config.pii_entities)?;
        state.serialize_field("pii_mask_long_numbers", &self.config.pii_mask_long_numbers)?;
        state.serialize_field("version", &self.config.version)?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_policy_reloads_have_one_fingerprint_and_match_old_hashes() {
        let config = SensitiveFilterConfig {
            version: 2,
            ..SensitiveFilterConfig::default()
        };
        let saved = serde_json::to_value(&config).unwrap();
        let expected = PrivacyFingerprint::new(config).unwrap();
        for _ in 0..64 {
            let loaded: SensitiveFilterConfig = serde_json::from_value(saved.clone()).unwrap();
            let old_hash = digest(&serde_json::to_string(&loaded).unwrap());
            let actual = PrivacyFingerprint::new(loaded).unwrap();
            assert_eq!(expected.as_str(), actual.as_str());
            assert!(expected.matches_legacy(&old_hash).unwrap());
        }
    }

    #[test]
    fn curated_dictionary_policy_does_not_reuse_previous_summaries() {
        let previous = SensitiveFilterConfig {
            version: 2,
            ..SensitiveFilterConfig::default()
        };
        let old_hash = digest(&serde_json::to_string(&previous).unwrap());
        let old = PrivacyFingerprint::new(previous).unwrap();
        let current = PrivacyFingerprint::new(SensitiveFilterConfig::default()).unwrap();
        assert_ne!(old.as_str(), current.as_str());
        assert!(!current.matches_legacy(&old_hash).unwrap());
    }

    #[test]
    fn actual_privacy_changes_match_neither_canonical_nor_legacy_identity() {
        let config = SensitiveFilterConfig::default();
        let old_hash = digest(&serde_json::to_string(&config).unwrap());
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
            assert!(!fingerprint.matches_legacy(&old_hash).unwrap());
        }
    }

    #[test]
    fn unsupported_legacy_shapes_are_bounded_and_canonical_hashes_are_not_legacy() {
        let mut config = SensitiveFilterConfig::default();
        config
            .categories
            .extend([("extra_1".into(), true), ("extra_2".into(), true)]);
        let legacy = digest(&serde_json::to_string(&config).unwrap());
        let fingerprint = PrivacyFingerprint::new(config).unwrap();
        assert!(!fingerprint.matches_legacy(&legacy).unwrap());
        assert!(!fingerprint.matches_legacy(fingerprint.as_str()).unwrap());
        assert!(!fingerprint.matches_legacy("invalid").unwrap());
    }
}
