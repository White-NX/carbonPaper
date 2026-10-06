//! Content filter for MCP server responses.
//!
//! Loads AES-256-GCM-encrypted dictionary files from bundled resources at startup,
//! builds Aho-Corasick automata per category, and provides O(n) text scanning
//! to filter out records containing flagged words. Personal information is
//! found by the rules in [`crate::pii`]; [`SensitiveFilterState::inspect`]
//! runs both checks on one text field.

use crate::pii::{self, PiiKind, PiiSettings, PiiSpan};
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

const CATEGORY_IDS: &[&str] = &["cat_01", "cat_02", "cat_03", "cat_04", "cat_05"];

/// Maps category ID to its dictionary filename.
const DICT_FILES: &[(&str, &str)] = &[
    ("cat_01", "dict_01.dict.enc"),
    ("cat_02", "dict_02.dict.enc"),
    ("cat_03", "dict_03.dict.enc"),
    ("cat_04", "dict_04.dict.enc"),
    ("cat_05", "dict_05.dict.enc"),
];

/// Policy version of [`SensitiveFilterConfig`]. Version 2 replaced Presidio
/// with [`crate::pii`]. Version 3 uses curated dictionaries and narrower domains;
/// advancing the version invalidates recap caches filtered under the old policy.
pub const CONFIG_VERSION: u32 = 3;

/// What happens to content with a sensitive word or personal information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Withhold the whole snapshot.
    Reject,
    /// Drop the affected OCR segments and links; replace an affected title or
    /// URL with a placeholder.
    RemoveParagraph,
    /// Hide the matches in place.
    Mask,
}

impl FilterMode {
    /// Parses a stored mode; anything unknown falls back to the default.
    pub fn parse(mode: &str) -> Self {
        match mode {
            "reject" => FilterMode::Reject,
            "mask" => FilterMode::Mask,
            _ => FilterMode::RemoveParagraph,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            FilterMode::Reject => "reject",
            FilterMode::RemoveParagraph => "remove_paragraph",
            FilterMode::Mask => "mask",
        }
    }
}

/// Configuration for sensitive words and personal information in MCP responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitiveFilterConfig {
    pub enabled: bool,
    pub categories: HashMap<String, bool>,
    /// A [`FilterMode`] name: "reject", "remove_paragraph" (the default) or
    /// "mask". Applies to sensitive words and personal information alike.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Whether personal information detection is on.
    #[serde(default = "default_true", alias = "presidio_enabled")]
    pub pii_enabled: bool,
    /// Kinds to detect, by [`PiiKind::name`]. Empty means none.
    #[serde(default = "default_pii_entities", alias = "presidio_entities")]
    pub pii_entities: Vec<String>,
    /// Also mask long digit strings that match no specific rule.
    #[serde(default)]
    pub pii_mask_long_numbers: bool,
    /// Layout version; 0 for configurations saved before versioning.
    #[serde(default)]
    pub version: u32,
}

fn default_mode() -> String {
    FilterMode::RemoveParagraph.as_str().to_string()
}

fn default_true() -> bool {
    true
}

/// Every selectable kind except IP addresses, which are common and rarely
/// personal on a developer's screen.
fn default_pii_entities() -> Vec<String> {
    PiiKind::SELECTABLE
        .iter()
        .filter(|kind| **kind != PiiKind::IpAddress)
        .map(|kind| kind.name().to_string())
        .collect()
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
            mode: default_mode(),
            pii_enabled: true,
            pii_entities: default_pii_entities(),
            pii_mask_long_numbers: false,
            version: CONFIG_VERSION,
        }
    }
}

impl SensitiveFilterConfig {
    /// Brings a configuration read from the stored policy up to date.
    ///
    /// Before version 2 every save wrote the then-default "reject", so a stored
    /// "reject" says nothing about the user's choice and becomes the new
    /// default. An empty Presidio entity list meant "all kinds"; credentials
    /// did not exist then and are added to an explicit list.
    pub fn upgraded(mut self) -> Self {
        if self.version < 2 {
            if self.mode == FilterMode::Reject.as_str() {
                self.mode = default_mode();
            }
            if self.pii_entities.is_empty() {
                self.pii_entities = default_pii_entities();
            } else {
                self.pii_entities
                    .push(PiiKind::Credential.name().to_string());
            }
        }
        self.normalized()
    }

    /// Canonical entity names in settings order and a known mode. Applied to
    /// every configuration the settings page saves, which is current by
    /// definition.
    pub fn normalized(mut self) -> Self {
        let wanted: Vec<PiiKind> = self
            .pii_entities
            .iter()
            .filter_map(|name| PiiKind::from_name(name))
            .collect();
        self.pii_entities = PiiKind::SELECTABLE
            .iter()
            .filter(|kind| wanted.contains(kind))
            .map(|kind| kind.name().to_string())
            .collect();
        self.mode = FilterMode::parse(&self.mode).as_str().to_string();
        self.version = CONFIG_VERSION;
        self
    }

    fn pii_settings(&self) -> PiiSettings {
        if !self.pii_enabled {
            return PiiSettings::off();
        }
        PiiSettings::new(
            self.pii_entities
                .iter()
                .filter_map(|name| PiiKind::from_name(name)),
            self.pii_mask_long_numbers,
        )
    }
}

/// What the dictionary and the personal information rules found in one text.
#[derive(Debug, Default)]
pub struct Findings {
    /// A dictionary word from an enabled category.
    pub sensitive_word: bool,
    /// Personal information, in text order.
    pub pii: Vec<PiiSpan>,
}

impl Findings {
    /// Whether the filter mode applies. Long numbers alone are only masked.
    pub fn is_flagged(&self) -> bool {
        self.sensitive_word || self.pii.iter().any(PiiSpan::is_confident)
    }
}

/// Shared state for the sensitive data filter (config, word lists, Aho-Corasick automaton).
pub struct SensitiveFilterState {
    config: RwLock<SensitiveFilterConfig>,
    /// Per-category word lists, populated once via load_dicts()
    word_lists: RwLock<HashMap<String, Vec<String>>>,
    /// Active composite automaton (rebuilt when categories toggle)
    active_automaton: RwLock<Option<AhoCorasick>>,
    /// Derived from the configuration so each check avoids parsing names.
    pii: RwLock<PiiSettings>,
}

impl Default for SensitiveFilterState {
    fn default() -> Self {
        let config = SensitiveFilterConfig::default();
        let pii = config.pii_settings();
        Self {
            config: RwLock::new(config),
            word_lists: RwLock::new(HashMap::new()),
            active_automaton: RwLock::new(None),
            pii: RwLock::new(pii),
        }
    }
}

impl SensitiveFilterState {
    #[cfg(test)]
    pub(crate) fn with_test_words(words: &[&str]) -> Self {
        let state = Self::default();
        {
            let mut word_lists = state.word_lists.write().unwrap();
            word_lists.insert(
                "cat_01".to_string(),
                words.iter().map(|word| (*word).to_string()).collect(),
            );
        }
        let config = state.get_config();
        state.rebuild_automaton(&config);
        state
    }

    /// Load encrypted dictionary files from Tauri resources and build automata.
    pub fn load_dicts(&self, app_handle: &tauri::AppHandle) {
        let key: [u8; 32] = Sha256::digest(DICT_KEY_MATERIAL).into();
        let mut wl = self.word_lists.write().unwrap();

        for &(cat_id, dict_file) in DICT_FILES {
            let filename = format!("compliance_process/dicts/{}", dict_file);

            let resource_path = match app_handle.path().resource_dir() {
                Ok(dir) => dir.join(&filename),
                Err(e) => {
                    tracing::warn!("Cannot resolve resource dir for {}: {}", cat_id, e);
                    continue;
                }
            };

            if !resource_path.exists() {
                tracing::warn!("Dict file not found: {}", resource_path.display());
                continue;
            }

            let encrypted = match std::fs::read(&resource_path) {
                Ok(data) => data,
                Err(e) => {
                    tracing::error!("Failed to read {}: {}", resource_path.display(), e);
                    continue;
                }
            };

            match decrypt_dict(&key, &encrypted) {
                Ok(words) => {
                    tracing::info!("Loaded {} words for '{}'", words.len(), cat_id);
                    wl.insert(cat_id.to_string(), words);
                }
                Err(e) => {
                    tracing::error!("Failed to decrypt dict for '{}': {}", cat_id, e);
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
    pub fn is_record_sensitive(&self, window_title: Option<&str>, ocr_texts: &[&str]) -> bool {
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
    pub fn mode(&self) -> FilterMode {
        FilterMode::parse(&self.config.read().unwrap().mode)
    }

    /// Checks one text field against the dictionary and the personal
    /// information rules. `context` carries labels from neighbouring OCR
    /// blocks; see [`pii::block_contexts`].
    pub fn inspect(&self, text: &str, context: pii::Context) -> Findings {
        let settings = *self.pii.read().unwrap();
        Findings {
            sensitive_word: self.contains_sensitive(text),
            pii: pii::detect(text, settings, context),
        }
    }

    /// The field as returned when it is kept: only long numbers are hidden.
    pub fn kept_text(&self, text: &str, findings: &Findings) -> String {
        let long_numbers: Vec<PiiSpan> = findings
            .pii
            .iter()
            .filter(|span| !span.is_confident())
            .copied()
            .collect();
        pii::mask(text, &long_numbers)
    }

    /// The field as returned in mask mode: personal information replaced by
    /// its label, sensitive words by █.
    pub fn masked_text(&self, text: &str, findings: &Findings) -> String {
        self.mask_sensitive(&pii::mask(text, &findings.pii))
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
        *self.pii.write().unwrap() = config.pii_settings();
        let mut guard = self.config.write().unwrap();
        *guard = config;
    }

    /// Get the current configuration.
    pub fn get_config(&self) -> SensitiveFilterConfig {
        self.config.read().unwrap().clone()
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
            if config.categories.get(cat_id).copied().unwrap_or(true) {
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
                    tracing::info!("Built automaton with {} patterns", all_words.len());
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

    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| format!("Failed to create cipher: {}", e))?;

    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    let text = String::from_utf8(plaintext).map_err(|e| format!("Invalid UTF-8 in dict: {}", e))?;

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

    /// Explicit local curation check. Inputs and reports stay outside tracked
    /// sources, and assertion messages never contain dictionary text.
    #[test]
    #[ignore = "requires local dictionary curation artifacts"]
    fn curated_dictionary_replay() {
        let private =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.dictionary-cleaning.local");
        let key: [u8; 32] = Sha256::digest(DICT_KEY_MATERIAL).into();
        let load = |name: &str| {
            let state = SensitiveFilterState::default();
            {
                let mut lists = state.word_lists.write().unwrap();
                for (category, file) in DICT_FILES {
                    let data = std::fs::read(private.join(name).join(file))
                        .expect("Cannot read local encrypted replay dictionary");
                    lists.insert(
                        category.to_string(),
                        decrypt_dict(&key, &data).expect("Cannot decode local replay dictionary"),
                    );
                }
            }
            let mut config = state.get_config();
            config.pii_enabled = false;
            state.update_config(config);
            state
        };
        let old = load("backup");
        let new = load("candidate");
        let case_data =
            std::fs::read(private.join("replay-cases.json")).expect("Missing local replay cases");
        let cases: serde_json::Value =
            serde_json::from_slice(&case_data).expect("Invalid local replay cases");
        let mut candidate_hash = Sha256::new();
        for (_, file) in DICT_FILES {
            candidate_hash.update(file.as_bytes());
            candidate_hash.update(std::fs::read(private.join("candidate").join(file)).unwrap());
        }
        let cases = cases.as_array().expect("Invalid replay case shape");
        let mut kept_misses = 0;
        let mut mode_failures = 0;
        let mut category_failures = 0;
        let mut by_action: HashMap<String, [usize; 3]> = HashMap::new();
        for case in cases {
            let text = case["text"].as_str().expect("Missing replay text");
            let action = case["action"].as_str().expect("Missing replay action");
            let hit = new.contains_sensitive(text);
            let counts = by_action.entry(action.to_string()).or_default();
            counts[0] += 1;
            counts[1] += usize::from(old.contains_sensitive(text));
            counts[2] += usize::from(hit);
            if case["expected"].as_bool() == Some(true) && !hit {
                kept_misses += 1;
            }
            for mode in [
                FilterMode::Reject,
                FilterMode::RemoveParagraph,
                FilterMode::Mask,
            ] {
                let result = crate::mcp_server::filter_identity(&new, mode, text);
                let valid = match (hit, mode, result) {
                    (true, FilterMode::Reject, Err(_)) => true,
                    (true, FilterMode::RemoveParagraph, Ok(value)) => value == "[censored]",
                    (true, FilterMode::Mask, Ok(value)) => value != text,
                    (false, _, Ok(value)) => value == text,
                    _ => false,
                };
                mode_failures += usize::from(!valid);
            }
        }
        for category in CATEGORY_IDS {
            let mut config = new.get_config();
            for (id, enabled) in &mut config.categories {
                *enabled = id == category;
            }
            new.update_config(config);
            for case in cases.iter().filter(|case| {
                case["expected"].as_bool() == Some(true)
                    && case["category"].as_str() == Some(category)
            }) {
                category_failures +=
                    usize::from(!new.contains_sensitive(case["text"].as_str().unwrap()));
            }
        }
        let mut config = new.get_config();
        for enabled in config.categories.values_mut() {
            *enabled = false;
        }
        new.update_config(config);
        for case in cases {
            category_failures +=
                usize::from(new.contains_sensitive(case["text"].as_str().unwrap()));
        }
        let mut config = new.get_config();
        for enabled in config.categories.values_mut() {
            *enabled = true;
        }
        new.update_config(config);
        // Handwritten ordinary text, independent of the model classifications.
        let benign = [
            "请在周五之前提交项目报告。",
            "今天更新了浏览器和操作系统。",
            "我们讨论了数据库索引和查询性能。",
            "请打开设置页面调整字体大小。",
            "这份历史教材介绍了不同地区的文化。",
            "医生建议保持充足睡眠并定期体检。",
            "医院正在开展公共卫生知识讲座。",
            "法律课程讨论合同纠纷的处理流程。",
            "这篇论文分析了社会调查的数据。",
            "新闻报道应当核实信息来源。",
            "儿童健康教育需要家长和学校共同参与。",
            "图书馆举办了文学作品阅读活动。",
            "研究人员介绍了人体结构与生理功能。",
            "我们正在准备国际会议的材料。",
            "我想了解这本小说的人物关系。",
            "这个游戏支持多人合作模式。",
            "请检查网络连接和代理设置。",
            "照片保存在本地加密数据库中。",
            "用户可以随时关闭消息通知。",
            "今年的旅行计划包括参观博物馆。",
            "天气预报说明天可能下雨。",
            "我们需要改善搜索结果的相关性。",
            "应用支持深色模式和快捷键。",
            "这场讲座介绍了语言学习方法。",
            "学生正在学习世界地理和历史。",
            "表格记录了本月的采购数量。",
            "安全培训介绍了火灾逃生路线。",
            "请不要将密码写入日志文件。",
            "系统会在下载完成后显示通知。",
            "这份指南说明如何恢复备份。",
            "The className property controls the component style.",
            "Please review the assessment and update the documentation.",
            "The password field is empty in this example.",
            "Use an encrypted database for local screenshot history.",
            "The medical textbook explains anatomy and public health.",
            "The newspaper reports on the regional election results.",
            "A legal researcher is studying historical court decisions.",
            "The library has a collection of classical literature.",
            "The user can disable this category in settings.",
            "The release package contains five encrypted dictionaries.",
        ];
        let benign_old_hits = benign
            .iter()
            .filter(|text| old.contains_sensitive(text))
            .count();
        let benign_new_hits = benign
            .iter()
            .filter(|text| new.contains_sensitive(text))
            .count();
        let report = serde_json::json!({
            "candidate_sha256": format!("{:x}", candidate_hash.finalize()),
            "cases_sha256": format!("{:x}", Sha256::digest(&case_data)),
            "cases": cases.len(), "by_action_total_old_new": by_action,
            "kept_misses": kept_misses, "mode_failures": mode_failures,
            "category_failures": category_failures, "benign_cases": benign.len(),
            "benign_old_hits": benign_old_hits, "benign_new_hits": benign_new_hits,
            "limits": "Term replay is a consistency check, not ground truth; the 40 handwritten benign sentences are a small smoke corpus, not a production accuracy estimate. PII is disabled to isolate dictionary behavior."
        });
        std::fs::write(
            private.join("replay-report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .expect("Cannot write local replay report");
        assert!(
            kept_misses == 0 && mode_failures == 0 && category_failures == 0,
            "Local curation replay invariant failed; inspect aggregate report"
        );
    }

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
        assert!(
            !result.contains("secret"),
            "masked text should not contain 'secret': {}",
            result
        );
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
        assert_eq!(config.mode, "remove_paragraph");
        assert!(config.pii_enabled);
        assert!(!config.pii_mask_long_numbers);
        assert_eq!(config.version, CONFIG_VERSION);
        assert_eq!(
            config.pii_entities,
            [
                "PHONE_NUMBER",
                "CN_ID_CARD",
                "CN_BANK_CARD",
                "EMAIL_ADDRESS",
                "ADDRESS",
                "CREDENTIAL"
            ]
        );
        assert_eq!(config.categories.len(), CATEGORY_IDS.len());
    }

    fn stored(value: serde_json::Value) -> SensitiveFilterConfig {
        serde_json::from_value::<SensitiveFilterConfig>(value)
            .unwrap()
            .upgraded()
    }

    #[test]
    fn upgrades_presidio_era_configs() {
        // Saved by the old settings page after the user changed a category:
        // "reject" was the default, not a choice.
        let config = stored(serde_json::json!({
            "enabled": true,
            "categories": { "cat_01": false },
            "mode": "reject",
            "presidio_enabled": true,
            "presidio_language": "",
            "presidio_entities": ["PHONE_NUMBER", "CN_ID_CARD", "PERSON", "CREDIT_CARD"],
        }));
        assert_eq!(config.mode, "remove_paragraph");
        assert!(config.pii_enabled);
        assert_eq!(
            config.pii_entities,
            ["PHONE_NUMBER", "CN_ID_CARD", "CN_BANK_CARD", "CREDENTIAL"]
        );
        assert_eq!(config.version, CONFIG_VERSION);
        assert_eq!(config.categories.get("cat_01"), Some(&false));

        // An empty Presidio list meant every kind; a chosen mode is kept.
        let config = stored(serde_json::json!({
            "enabled": false,
            "categories": {},
            "mode": "mask",
            "presidio_enabled": false,
            "presidio_entities": [],
        }));
        assert_eq!(config.mode, "mask");
        assert!(!config.pii_enabled);
        assert_eq!(
            config.pii_entities,
            SensitiveFilterConfig::default().pii_entities
        );
    }

    #[test]
    fn current_configs_keep_an_explicit_reject() {
        let config = stored(serde_json::json!({
            "enabled": true,
            "categories": {},
            "mode": "reject",
            "pii_enabled": true,
            "pii_entities": [],
            "version": CONFIG_VERSION,
        }));
        assert_eq!(config.mode, "reject");
        assert!(config.pii_entities.is_empty());
        assert_eq!(config.pii_settings(), PiiSettings::new([], false));
    }

    #[test]
    fn curated_policy_upgrade_preserves_user_choices() {
        let config = stored(serde_json::json!({
            "enabled": false,
            "categories": { "cat_01": false, "cat_02": true, "cat_03": false },
            "mode": "reject",
            "pii_enabled": false,
            "pii_entities": [],
            "version": 2,
        }));
        assert_eq!(config.version, 3);
        assert!(!config.enabled);
        assert!(!config.pii_enabled);
        assert!(config.pii_entities.is_empty());
        assert_eq!(config.mode, "reject");
        assert_eq!(config.categories.get("cat_01"), Some(&false));
        assert_eq!(config.categories.get("cat_02"), Some(&true));
        assert_eq!(config.categories.get("cat_03"), Some(&false));
    }

    #[test]
    fn normalizes_what_the_settings_page_sends() {
        let config = SensitiveFilterConfig {
            mode: "unknown".to_string(),
            pii_entities: vec![
                "IP_ADDRESS".to_string(),
                "PHONE_NUMBER".to_string(),
                "PHONE_NUMBER".to_string(),
                "PERSON".to_string(),
            ],
            version: 0,
            ..SensitiveFilterConfig::default()
        }
        .normalized();
        assert_eq!(config.mode, "remove_paragraph");
        assert_eq!(config.pii_entities, ["PHONE_NUMBER", "IP_ADDRESS"]);
        assert_eq!(config.version, CONFIG_VERSION);
    }

    #[test]
    fn inspect_combines_words_and_personal_information() {
        let state = make_state_with_words(vec!["secret"]);
        let text = "secret plan, call 13812345678";
        let findings = state.inspect(text, pii::Context::default());
        assert!(findings.sensitive_word);
        assert_eq!(findings.pii.len(), 1);
        assert!(findings.is_flagged());
        assert_eq!(
            state.masked_text(text, &findings),
            "██████ plan, call [PHONE_NUMBER]"
        );
        assert_eq!(state.kept_text(text, &findings), text);

        let mut config = state.get_config();
        config.pii_enabled = false;
        state.update_config(config);
        assert!(state.inspect(text, pii::Context::default()).pii.is_empty());
    }

    #[test]
    fn long_numbers_are_masked_without_flagging() {
        let state = make_state_with_words(vec![]);
        let mut config = state.get_config();
        config.pii_mask_long_numbers = true;
        state.update_config(config);
        let text = "订单编号：203496817759901245";
        let findings = state.inspect(text, pii::Context::default());
        assert!(!findings.is_flagged());
        assert_eq!(state.kept_text(text, &findings), "订单编号：[LONG_NUMBER]");
    }

    #[test]
    fn filter_mode_parses_unknown_as_default() {
        assert_eq!(FilterMode::parse("reject"), FilterMode::Reject);
        assert_eq!(FilterMode::parse("mask"), FilterMode::Mask);
        assert_eq!(FilterMode::parse(""), FilterMode::RemoveParagraph);
        assert_eq!(FilterMode::RemoveParagraph.as_str(), "remove_paragraph");
    }
}
