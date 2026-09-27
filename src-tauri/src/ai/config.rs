//! User-configured model endpoints.
//!
//! Endpoints live under the `ai` key of the policy file. API keys are encrypted
//! with a key derived from the master key, so reading or changing them needs an
//! unlocked session. The frontend never receives the `ai` policy key directly;
//! it reads [`AiSettingsView`] instead, which only says whether a key is set.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::credential_manager::{
    self, decrypt_with_master_key, encrypt_with_master_key, CredentialManagerState,
};
use crate::storage::StorageState;

/// Policy key that holds [`AiSettings`]. Hidden from the generic policy commands.
pub const POLICY_KEY: &str = "ai";

const KEY_PREFIX: &str = "v1:";
const MAX_NAME_CHARS: usize = 64;
const MAX_FIELD_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// The Chat Completions protocol served by OpenAI and most compatible hosts.
    #[default]
    OpenaiCompatible,
    /// The Anthropic Messages protocol.
    Anthropic,
}

/// Whether the endpoint's model can call tools, as last measured by a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolCalling {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_encrypted: Option<String>,
    #[serde(default)]
    pub tool_calling: ToolCalling,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default)]
    pub providers: Vec<StoredProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider_id: Option<String>,
    /// The user agreed that screenshot text may be sent to endpoints that are
    /// not on this computer.
    #[serde(default)]
    pub remote_consent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub has_api_key: bool,
    pub tool_calling: ToolCalling,
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiSettingsView {
    pub providers: Vec<ProviderView>,
    pub default_provider_id: Option<String>,
    pub remote_consent: bool,
}

/// A provider as edited in settings. `api_key` of `None` keeps the stored key,
/// and an empty string clears it.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Everything needed to send a request, with the key already decrypted.
#[derive(Clone)]
pub struct ResolvedProvider {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub tool_calling: ToolCalling,
}

impl std::fmt::Debug for ResolvedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedProvider")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

impl AiSettings {
    pub fn load(storage: &StorageState) -> Result<Self, String> {
        let policy = storage.load_policy()?;
        Ok(policy
            .get(POLICY_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default())
    }

    pub fn save(&self, storage: &StorageState) -> Result<(), String> {
        let mut policy = storage.load_policy()?;
        let obj = policy
            .as_object_mut()
            .ok_or_else(|| "Existing policy is not a valid JSON object".to_string())?;
        obj.insert(
            POLICY_KEY.into(),
            serde_json::to_value(self).map_err(|e| e.to_string())?,
        );
        storage.save_policy(&policy)
    }

    pub fn view(&self) -> AiSettingsView {
        AiSettingsView {
            providers: self
                .providers
                .iter()
                .map(|p| ProviderView {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    kind: p.kind,
                    base_url: p.base_url.clone(),
                    model: p.model.clone(),
                    has_api_key: p.api_key_encrypted.is_some(),
                    tool_calling: p.tool_calling,
                    is_local: is_local_url(&p.base_url),
                })
                .collect(),
            default_provider_id: self.effective_default_id().map(str::to_string),
            remote_consent: self.remote_consent,
        }
    }

    pub fn find(&self, id: &str) -> Option<&StoredProvider> {
        self.providers.iter().find(|p| p.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut StoredProvider> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// The configured default, or the first provider when none is chosen.
    pub fn effective_default_id(&self) -> Option<&str> {
        self.default_provider_id
            .as_deref()
            .filter(|id| self.find(id).is_some())
            .or_else(|| self.providers.first().map(|p| p.id.as_str()))
    }

    /// Creates or updates a provider and returns its id.
    pub fn upsert(
        &mut self,
        credential_state: &CredentialManagerState,
        input: ProviderInput,
    ) -> Result<String, String> {
        let input = validate(input)?;
        let api_key_encrypted = match input.api_key.as_deref() {
            None => None,
            Some("") => Some(None),
            Some(key) => Some(Some(encrypt_key(credential_state, key)?)),
        };

        if let Some(id) = input.id.as_deref() {
            let existing = self
                .find_mut(id)
                .ok_or_else(|| "AI_PROVIDER_NOT_FOUND".to_string())?;
            let endpoint_changed = existing.kind != input.kind
                || existing.base_url != input.base_url
                || existing.model != input.model;
            existing.name = input.name;
            existing.kind = input.kind;
            existing.base_url = input.base_url;
            existing.model = input.model;
            if let Some(encrypted) = api_key_encrypted {
                existing.api_key_encrypted = encrypted;
            }
            if endpoint_changed {
                existing.tool_calling = ToolCalling::Unknown;
            }
            return Ok(id.to_string());
        }

        let id = new_id();
        self.providers.push(StoredProvider {
            id: id.clone(),
            name: input.name,
            kind: input.kind,
            base_url: input.base_url,
            model: input.model,
            api_key_encrypted: api_key_encrypted.flatten(),
            tool_calling: ToolCalling::Unknown,
        });
        if self.default_provider_id.is_none() {
            self.default_provider_id = Some(id.clone());
        }
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.providers.len() == before {
            return Err("AI_PROVIDER_NOT_FOUND".into());
        }
        if self.default_provider_id.as_deref() == Some(id) {
            self.default_provider_id = self.providers.first().map(|p| p.id.clone());
        }
        Ok(())
    }

    pub fn set_default(&mut self, id: &str) -> Result<(), String> {
        if self.find(id).is_none() {
            return Err("AI_PROVIDER_NOT_FOUND".into());
        }
        self.default_provider_id = Some(id.to_string());
        Ok(())
    }

    pub fn set_tool_calling(&mut self, id: &str, value: ToolCalling) {
        if let Some(p) = self.find_mut(id) {
            p.tool_calling = value;
        }
    }

    pub fn resolve(
        &self,
        credential_state: &CredentialManagerState,
        id: &str,
    ) -> Result<ResolvedProvider, String> {
        let p = self
            .find(id)
            .ok_or_else(|| "AI_PROVIDER_NOT_FOUND".to_string())?;
        Ok(ResolvedProvider {
            kind: p.kind,
            base_url: p.base_url.clone(),
            model: p.model.clone(),
            api_key: p
                .api_key_encrypted
                .as_deref()
                .map(|enc| decrypt_key(credential_state, enc))
                .transpose()?,
            tool_calling: p.tool_calling,
        })
    }

    /// Resolves an unsaved input for a connection test. A missing key falls
    /// back to the stored key of the provider being edited.
    pub fn resolve_input(
        &self,
        credential_state: &CredentialManagerState,
        input: ProviderInput,
    ) -> Result<ResolvedProvider, String> {
        let input = validate(input)?;
        let api_key = match input.api_key {
            Some(key) if key.is_empty() => None,
            Some(key) => Some(key),
            None => match input.id.as_deref().and_then(|id| self.find(id)) {
                Some(p) => p
                    .api_key_encrypted
                    .as_deref()
                    .map(|enc| decrypt_key(credential_state, enc))
                    .transpose()?,
                None => None,
            },
        };
        Ok(ResolvedProvider {
            kind: input.kind,
            base_url: input.base_url,
            model: input.model,
            api_key,
            tool_calling: ToolCalling::Unknown,
        })
    }
}

fn validate(mut input: ProviderInput) -> Result<ProviderInput, String> {
    input.name = input.name.trim().to_string();
    input.base_url = input.base_url.trim().trim_end_matches('/').to_string();
    input.model = input.model.trim().to_string();
    if let Some(key) = input.api_key.as_mut() {
        *key = key.trim().to_string();
    }
    if input.base_url.is_empty() || input.model.is_empty() {
        return Err("AI_PROVIDER_INCOMPLETE".into());
    }
    if !(input.base_url.starts_with("https://") || input.base_url.starts_with("http://")) {
        return Err("AI_PROVIDER_INVALID_URL".into());
    }
    if input.name.is_empty() {
        input.name = input.model.clone();
    }
    if input.name.chars().count() > MAX_NAME_CHARS
        || input.base_url.len() > MAX_FIELD_CHARS
        || input.model.len() > MAX_FIELD_CHARS
        || input
            .api_key
            .as_ref()
            .is_some_and(|k| k.len() > MAX_FIELD_CHARS * 4)
    {
        return Err("AI_PROVIDER_FIELD_TOO_LONG".into());
    }
    Ok(input)
}

/// Endpoints on this machine. Sending screenshot text to them does not leave
/// the computer, so the privacy notice is skipped for them.
pub fn is_local_url(base_url: &str) -> bool {
    let rest = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .unwrap_or(base_url);
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

fn new_id() -> String {
    let bytes: [u8; 8] = rand::random();
    hex::encode(bytes)
}

fn derive_key(credential_state: &CredentialManagerState) -> Result<[u8; 32], String> {
    let master_key = credential_manager::get_cached_master_key(credential_state)
        .ok_or_else(|| "AUTH_REQUIRED".to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&master_key);
    hasher.update(b"CarbonPaper-AI-Provider-Key-v1");
    Ok(hasher.finalize().into())
}

fn encrypt_key(
    credential_state: &CredentialManagerState,
    plaintext: &str,
) -> Result<String, String> {
    let key = derive_key(credential_state)?;
    let encrypted = encrypt_with_master_key(&key, plaintext.as_bytes())
        .map_err(|e| format!("API key encryption failed: {}", e))?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, encrypted);
    Ok(format!("{KEY_PREFIX}{encoded}"))
}

fn decrypt_key(credential_state: &CredentialManagerState, stored: &str) -> Result<String, String> {
    let encoded = stored
        .strip_prefix(KEY_PREFIX)
        .ok_or_else(|| "AI_PROVIDER_KEY_UNREADABLE".to_string())?;
    let key = derive_key(credential_state)?;
    let encrypted = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(|_| "AI_PROVIDER_KEY_UNREADABLE".to_string())?;
    let plaintext = decrypt_with_master_key(&key, &encrypted)
        .map_err(|_| "AI_PROVIDER_KEY_UNREADABLE".to_string())?;
    String::from_utf8(plaintext).map_err(|_| "AI_PROVIDER_KEY_UNREADABLE".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(base_url: &str) -> ProviderInput {
        ProviderInput {
            id: None,
            name: " ".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: base_url.into(),
            model: " qwen3 ".into(),
            api_key: None,
        }
    }

    #[test]
    fn validation_trims_fields_and_defaults_the_name_to_the_model() {
        let v = validate(input(" https://api.example.com/v1/ ")).unwrap();
        assert_eq!(v.base_url, "https://api.example.com/v1");
        assert_eq!(v.name, "qwen3");
        assert_eq!(v.model, "qwen3");
    }

    #[test]
    fn validation_rejects_missing_or_non_http_urls() {
        assert_eq!(validate(input("")).unwrap_err(), "AI_PROVIDER_INCOMPLETE");
        assert_eq!(
            validate(input("ftp://example.com")).unwrap_err(),
            "AI_PROVIDER_INVALID_URL"
        );
    }

    #[test]
    fn local_urls_cover_loopback_hosts_only() {
        assert!(is_local_url("http://localhost:11434/v1"));
        assert!(is_local_url("http://127.0.0.1:1234/v1"));
        assert!(is_local_url("http://[::1]:8080"));
        assert!(!is_local_url("https://api.deepseek.com"));
        assert!(!is_local_url("http://localhost.evil.com/v1"));
    }

    #[test]
    fn removing_the_default_promotes_the_next_provider() {
        let stored = |id: &str| StoredProvider {
            id: id.into(),
            name: id.into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "http://localhost".into(),
            model: "m".into(),
            api_key_encrypted: None,
            tool_calling: ToolCalling::Unknown,
        };
        let mut settings = AiSettings {
            providers: vec![stored("a"), stored("b")],
            default_provider_id: Some("a".into()),
            remote_consent: false,
        };
        settings.remove("a").unwrap();
        assert_eq!(settings.effective_default_id(), Some("b"));
        assert!(settings.remove("a").is_err());
    }

    #[test]
    fn the_view_never_contains_the_encrypted_key() {
        let settings = AiSettings {
            providers: vec![StoredProvider {
                id: "a".into(),
                name: "A".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: "https://api.example.com".into(),
                model: "m".into(),
                api_key_encrypted: Some("v1:secret".into()),
                tool_calling: ToolCalling::Supported,
            }],
            default_provider_id: None,
            remote_consent: false,
        };
        let json = serde_json::to_string(&settings.view()).unwrap();
        assert!(!json.contains("secret"));
        assert!(json.contains("\"has_api_key\":true"));
    }
}
