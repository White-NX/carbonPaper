//! Content filter for MCP server responses.
//!
//! Loads AES-256-GCM-encrypted dictionary files from bundled resources at startup,
//! builds Aho-Corasick automata per category, and provides O(n) text scanning
//! to filter out records containing flagged words.

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use aho_corasick::AhoCorasick;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;
use tauri::Manager;

const DICT_KEY_MATERIAL: &[u8] = b"CarbonPaper-SensitiveDict-v1";

/// Minimum word length (in Unicode chars) for a pattern to be included in the
/// Aho-Corasick automaton.  Short entries are too common for blind substring
/// matching and would cause nearly every record to be flagged.  2 chars
/// strikes a good balance.
const MIN_WORD_CHARS: usize = 2;

const CATEGORY_IDS: &[&str] = &[
    "cat_01",
    "cat_02",
    "cat_03",
    "cat_04",
    "cat_05",
];

/// Maps category ID to its dictionary filename.
const DICT_FILES: &[(&str, &str)] = &[
    ("cat_01", "dict_01.dict.enc"),
    ("cat_02", "dict_02.dict.enc"),
    ("cat_03", "dict_03.dict.enc"),
    ("cat_04", "dict_04.dict.enc"),
    ("cat_05", "dict_05.dict.enc"),
];

/// Configuration for sensitive data detection and masking (categories, mode, Presidio settings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitiveFilterConfig {
    pub enabled: bool,
    pub categories: HashMap<String, bool>,
    /// Filter mode: "reject" (reject entire snapshot), "remove_paragraph" (strip
    /// OCR entries containing sensitive words), "mask" (replace sensitive words
    /// with █ characters).  Defaults to "reject" for backward compatibility.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Whether Presidio PII detection is enabled (independent toggle).
    #[serde(default = "default_true")]
    pub presidio_enabled: bool,
    /// Presidio language code, auto-synced from frontend i18n.
    #[serde(default)]
    pub presidio_language: String,
    /// Which PII entity types to detect (empty = all).
    #[serde(default)]
    pub presidio_entities: Vec<String>,
}

fn default_mode() -> String {
    "reject".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for SensitiveFilterConfig {
    fn default() -> Self {
        let mut categories = HashMap::new();
        for id in CATEGORY_IDS {
            categories.insert(id.to_string(), true);
        }
        Self {
            enabled: true,
            categories,
            mode: "reject".to_string(),
            presidio_enabled: true,
            presidio_language: String::new(),
            presidio_entities: Vec::new(),
        }
    }
}

/// Shared state for the sensitive data filter (config, word lists, Aho-Corasick automaton).
pub struct SensitiveFilterState {
    config: RwLock<SensitiveFilterConfig>,
    /// Per-category word lists, populated once via load_dicts()
    word_lists: RwLock<HashMap<String, Vec<String>>>,
    /// Active composite automaton (rebuilt when categories toggle)
    active_automaton: RwLock<Option<AhoCorasick>>,
}

impl Default for SensitiveFilterState {
    fn default() -> Self {
        Self {
            config: RwLock::new(SensitiveFilterConfig::default()),
            word_lists: RwLock::new(HashMap::new()),
            active_automaton: RwLock::new(None),
        }
    }
}

impl SensitiveFilterState {
    /// Load encrypted dictionary files from Tauri resources and build automata.
    pub fn load_dicts(&self, app_handle: &tauri::AppHandle) {
        let key: [u8; 32] = Sha256::digest(DICT_KEY_MATERIAL).into();
        let mut wl = self.word_lists.write().unwrap();

        for &(cat_id, dict_file) in DICT_FILES {
            let filename = format!(
                "compliance_process/dicts/{}",
                dict_file
            );

            let resource_path = match app_handle.path().resource_dir() {
                Ok(dir) => dir.join(&filename),
                Err(e) => {
                    tracing::warn!(
                        "Cannot resolve resource dir for {}: {}",
                        cat_id,
                        e
                    );
                    continue;
                }
            };

            if !resource_path.exists() {
                tracing::warn!(
                    "Dict file not found: {}",
                    resource_path.display()
                );
                continue;
            }

            let encrypted = match std::fs::read(&resource_path) {
                Ok(data) => data,
                Err(e) => {
                    tracing::error!(
                        "Failed to read {}: {}",
                        resource_path.display(),
                        e
                    );
                    continue;
                }
            };

            match decrypt_dict(&key, &encrypted) {
                Ok(words) => {
                    tracing::info!(
                        "Loaded {} words for '{}'",
                        words.len(),
                        cat_id
                    );
                    wl.insert(cat_id.to_string(), words);
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to decrypt dict for '{}': {}",
                        cat_id,
                        e
                    );
                }
            }
        }
        drop(wl);

        // Build initial composite automaton from config
        let config = self.config.read().unwrap().clone();
        self.rebuild_automaton(&config);
    }

    /// Check if the filter is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.read().unwrap().enabled
    }

    /// Check if text contains any flagged words from enabled categories.
    pub fn contains_sensitive(&self, text: &str) -> bool {
        if !self.is_enabled() {
            return false;
        }

        let guard = self.active_automaton.read().unwrap();
        match &*guard {
            // ascii_case_insensitive is set on the automaton, no need to lowercase
            Some(automaton) => automaton.is_match(text),
            None => false,
        }
    }

    /// Check if a record (window title + OCR texts) is flagged.
    pub fn is_record_sensitive(
        &self,
        window_title: Option<&str>,
        ocr_texts: &[&str],
    ) -> bool {
        if !self.is_enabled() {
            return false;
        }

        if let Some(title) = window_title {
            if self.contains_sensitive(title) {
                return true;
            }
        }

        for text in ocr_texts {
            if self.contains_sensitive(text) {
                return true;
            }
        }

        false
    }

    /// Get the current filter mode.
    pub fn get_mode(&self) -> String {
        self.config.read().unwrap().mode.clone()
    }

    /// Replace all sensitive word occurrences in `text` with █ characters
    /// (one █ per matched character).  Returns the original text if the
    /// filter is disabled or no matches are found.
    pub fn mask_sensitive(&self, text: &str) -> String {
        if !self.is_enabled() {
            return text.to_string();
        }

        let guard = self.active_automaton.read().unwrap();
        let automaton = match &*guard {
            Some(ac) => ac,
            None => return text.to_string(),
        };

        // ascii_case_insensitive is set on the automaton – match directly on
        // the original text so byte offsets stay valid.
        let matches: Vec<_> = automaton.find_iter(text).collect();
        if matches.is_empty() {
            return text.to_string();
        }

        // Build a mask bitmap over the *byte* positions, then rebuild the
        // string replacing masked chars with '█'.
        let mut masked = vec![false; text.len()];
        for m in &matches {
            for i in m.start()..m.end() {
                masked[i] = true;
            }
        }

        let mut result = String::with_capacity(text.len());
        let mut i = 0;
        for ch in text.chars() {
            let byte_len = ch.len_utf8();
            if masked[i] {
                result.push('█');
            } else {
                result.push(ch);
            }
            i += byte_len;
        }
        result
    }

    /// Update the configuration and rebuild the composite automaton.
    pub fn update_config(&self, config: SensitiveFilterConfig) {
        self.rebuild_automaton(&config);
        let mut guard = self.config.write().unwrap();
        *guard = config;
    }

    /// Get the current configuration.
    pub fn get_config(&self) -> SensitiveFilterConfig {
        self.config.read().unwrap().clone()
    }

    /// Check if Presidio PII detection is enabled.
    pub fn is_presidio_enabled(&self) -> bool {
        let cfg = self.config.read().unwrap();
        cfg.presidio_enabled
    }

    /// Get Presidio config tuple: (enabled, language, entity_types).
    pub fn get_presidio_config(&self) -> (bool, String, Vec<String>) {
        let cfg = self.config.read().unwrap();
        (
            cfg.presidio_enabled,
            cfg.presidio_language.clone(),
            cfg.presidio_entities.clone(),
        )
    }

    /// Rebuild the composite Aho-Corasick automaton from enabled categories.
    fn rebuild_automaton(&self, config: &SensitiveFilterConfig) {
        if !config.enabled {
            let mut guard = self.active_automaton.write().unwrap();
            *guard = None;
            return;
        }

        let wl = self.word_lists.read().unwrap();
        let mut all_words: Vec<String> = Vec::new();
        let mut skipped: usize = 0;
        for (cat_id, words) in wl.iter() {
            if config
                .categories
                .get(cat_id)
                .copied()
                .unwrap_or(true)
            {
                for w in words {
                    if w.chars().count() >= MIN_WORD_CHARS {
                        all_words.push(w.clone());
                    } else {
                        skipped += 1;
                    }
                }
            }
        }
        drop(wl);

        if skipped > 0 {
            tracing::info!(
                "Skipped {} words shorter than {} chars",
                skipped,
                MIN_WORD_CHARS
            );
        }

        // Deduplicate
        all_words.sort_unstable();
        all_words.dedup();

        let automaton = if all_words.is_empty() {
            None
        } else {
            match AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build(&all_words)
            {
                Ok(ac) => {
                    tracing::info!(
                        "Built automaton with {} patterns",
                        all_words.len()
                    );
                    Some(ac)
                }
                Err(e) => {
                    tracing::error!("Failed to build automaton: {}", e);
                    None
                }
            }
        };

        let mut guard = self.active_automaton.write().unwrap();
        *guard = automaton;
    }
}

/// Decrypt an encrypted dictionary file and return the list of words.
///
/// File format: `[12-byte nonce][ciphertext + 16-byte GCM tag]`
fn decrypt_dict(key: &[u8; 32], encrypted: &[u8]) -> Result<Vec<String>, String> {
    if encrypted.len() < 12 + 16 {
        return Err("Encrypted data too short".to_string());
    }

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| format!("Failed to create cipher: {}", e))?;

    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    let text = String::from_utf8(plaintext)
        .map_err(|e| format!("Invalid UTF-8 in dict: {}", e))?;

    let words: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a SensitiveFilterState with given words in a single category.
    fn make_state_with_words(words: Vec<&str>) -> SensitiveFilterState {
        let state = SensitiveFilterState::default();
        {
            let mut wl = state.word_lists.write().unwrap();
            wl.insert(
                "cat_01".to_string(),
                words.into_iter().map(|w| w.to_string()).collect(),
            );
        }
        // Rebuild automaton with default config (all categories enabled)
        let config = state.get_config();
        state.rebuild_automaton(&config);
        state
    }

    #[test]
    fn test_contains_sensitive_match() {
        let state = make_state_with_words(vec!["secret", "password"]);
        assert!(state.contains_sensitive("my secret data"));
        assert!(state.contains_sensitive("enter your password here"));
    }

    #[test]
    fn test_contains_sensitive_no_match() {
        let state = make_state_with_words(vec!["secret", "password"]);
        assert!(!state.contains_sensitive("hello world"));
    }

    #[test]
    fn test_contains_sensitive_case_insensitive() {
        let state = make_state_with_words(vec!["secret"]);
        assert!(state.contains_sensitive("MY SECRET DATA"));
        assert!(state.contains_sensitive("Secret"));
    }

    #[test]
    fn test_contains_sensitive_disabled() {
        let state = make_state_with_words(vec!["secret"]);
        // Disable the filter
        let mut config = state.get_config();
        config.enabled = false;
        state.update_config(config);
        assert!(!state.contains_sensitive("this is secret"));
    }

    #[test]
    fn test_mask_sensitive_basic() {
        let state = make_state_with_words(vec!["secret"]);
        let result = state.mask_sensitive("my secret data");
        assert!(!result.contains("secret"), "masked text should not contain 'secret': {}", result);
        assert!(result.contains("my "), "non-sensitive part should remain");
        assert!(result.contains(" data"), "non-sensitive part should remain");
    }

    #[test]
    fn test_mask_sensitive_no_match() {
        let state = make_state_with_words(vec!["secret"]);
        let result = state.mask_sensitive("hello world");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_mask_sensitive_disabled() {
        let state = make_state_with_words(vec!["secret"]);
        let mut config = state.get_config();
        config.enabled = false;
        state.update_config(config);
        let result = state.mask_sensitive("my secret data");
        assert_eq!(result, "my secret data");
    }

    #[test]
    fn test_is_record_sensitive() {
        let state = make_state_with_words(vec!["secret"]);
        assert!(state.is_record_sensitive(Some("secret title"), &[]));
        assert!(state.is_record_sensitive(None, &["contains secret text"]));
        assert!(!state.is_record_sensitive(Some("normal title"), &["normal text"]));
    }

    #[test]
    fn test_short_words_filtered() {
        // Words shorter than MIN_WORD_CHARS (2) should be excluded
        let state = make_state_with_words(vec!["a", "ab", "abc"]);
        // "a" is too short (1 char), should not match
        assert!(!state.contains_sensitive("a"));
        // "ab" meets the minimum, should match
        assert!(state.contains_sensitive("ab"));
        assert!(state.contains_sensitive("abc"));
    }

    #[test]
    fn test_default_config() {
        let config = SensitiveFilterConfig::default();
        assert!(config.enabled);
        assert_eq!(config.mode, "reject");
        assert!(config.presidio_enabled);
        assert_eq!(config.categories.len(), CATEGORY_IDS.len());
    }
}
