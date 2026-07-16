//! Shared translations for native (Rust/Tauri) UI surfaces.
//!
//! The React client and native notifications intentionally use the same JSON
//! locale files. Native callers get locale normalization and a predictable
//! fallback chain without embedding user-facing strings in business logic.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/native_locales.rs"));

fn catalog() -> &'static HashMap<String, Value> {
    static CATALOG: OnceLock<HashMap<String, Value>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        NATIVE_LOCALES
            .iter()
            .map(|(locale, raw)| {
                (
                    normalize_locale(locale),
                    serde_json::from_str(raw).expect("locale JSON must be valid"),
                )
            })
            .collect()
    })
}

fn locale_candidates(locale: &str) -> Vec<String> {
    let normalized = normalize_locale(locale);
    let mut result = Vec::new();
    let mut push = |candidate: String| {
        if !result.contains(&candidate) {
            result.push(candidate);
        }
    };
    push(normalized.clone());
    if let Some((language, _)) = normalized.split_once('-') {
        push(language.to_string());
    }
    push("zh-cn".to_string());
    push("en".to_string());
    result
}

fn normalize_locale(locale: &str) -> String {
    locale.trim().replace('_', "-").to_ascii_lowercase()
}

pub(crate) fn supported_locale(locale: &str) -> String {
    let normalized = normalize_locale(locale);
    NATIVE_LOCALES
        .iter()
        .find(|(name, _)| normalize_locale(name) == normalized)
        .or_else(|| {
            normalized.split_once('-').and_then(|(language, _)| {
                NATIVE_LOCALES
                    .iter()
                    .find(|(name, _)| normalize_locale(name) == language)
            })
        })
        .map(|(name, _)| (*name).to_string())
        .unwrap_or_else(|| "zh-CN".to_string())
}

fn lookup<'a>(data: &'a Value, key: &str) -> Option<&'a str> {
    let mut current = data;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    current.as_str()
}

/// Translate a native UI message. Missing keys intentionally return the key,
/// making missing native translations visible during development.
pub(crate) fn t(locale: &str, key: &str) -> String {
    let catalog = catalog();
    locale_candidates(locale)
        .iter()
        .filter_map(|candidate| catalog.get(candidate))
        .find_map(|data| lookup(data, key))
        .unwrap_or(key)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{supported_locale, t};

    #[test]
    fn falls_back_from_regional_locale() {
        assert_eq!(
            t("en-US", "notifications.ocr_model_repair.action"),
            "Repair model"
        );
    }

    #[test]
    fn falls_back_to_key_for_missing_message() {
        assert_eq!(t("zh-CN", "notifications.missing"), "notifications.missing");
    }

    #[test]
    fn resolves_supported_locale_case_insensitively() {
        assert_eq!(supported_locale("EN_us"), "en");
    }
}
