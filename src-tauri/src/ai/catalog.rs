//! Public model capabilities and account-scoped endpoint discovery. Only
//! normalized metadata is cached under the active data directory; credentials
//! and screenshot data never enter these files.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::config::{
    fallback_context_tokens, is_local_url, models_endpoint, AiSettingsView, ImageInput,
    ProviderKind, ResolvedProvider, ToolCalling, MAX_CONTEXT_TOKENS,
};
use super::provider::{self, ProviderError};
use crate::storage::StorageState;

const CATALOG_URL: &str = "https://models.dev/api.json";
const CACHE_VERSION: u32 = 1;
const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODELS: usize = 5000;
const METADATA_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub image_input: ImageInput,
    #[serde(default)]
    pub tool_calling: ToolCalling,
    #[serde(default)]
    pub context_tokens: Option<u32>,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderPreset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogData {
    pub providers: Vec<ProviderPreset>,
}

#[derive(Serialize, Deserialize)]
struct Cache<T> {
    version: u32,
    fetched_at_ms: i64,
    data: T,
}

impl<T> Cache<T> {
    fn fresh(&self) -> bool {
        let age = chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(self.fetched_at_ms);
        self.version == CACHE_VERSION && (0..CACHE_TTL_MS).contains(&age)
    }
}

#[derive(Serialize)]
pub struct CatalogView {
    pub providers: Vec<ProviderPreset>,
    pub fetched_at_ms: Option<i64>,
    pub stale: bool,
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct ModelListView {
    pub models: Vec<ModelInfo>,
    pub fetched_at_ms: Option<i64>,
    pub stale: bool,
    pub warning: Option<String>,
}

pub fn presets() -> CatalogData {
    serde_json::from_str(include_str!("../../../src/lib/ai_provider_presets.json"))
        .expect("bundled AI provider presets must be valid")
}

fn root(storage: &StorageState) -> PathBuf {
    storage
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .join("ai-models")
}

fn read_cache<T: DeserializeOwned>(path: &Path) -> Option<Cache<T>> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_RESPONSE_BYTES as u64 {
        return None;
    }
    let cache: Cache<T> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (cache.version == CACHE_VERSION
        && cache.fetched_at_ms > 0
        && cache.fetched_at_ms <= chrono::Utc::now().timestamp_millis() + 60_000)
        .then_some(cache)
}

fn write_cache<T: Serialize>(path: &Path, data: &T) -> Result<(), String> {
    let parent = path.parent().ok_or("AI_METADATA_CACHE_FAILED")?;
    std::fs::create_dir_all(parent).map_err(|_| "AI_METADATA_CACHE_FAILED")?;
    let bytes = serde_json::to_vec(&Cache {
        version: CACHE_VERSION,
        fetched_at_ms: chrono::Utc::now().timestamp_millis(),
        data,
    })
    .map_err(|_| "AI_METADATA_CACHE_FAILED")?;
    let temp = parent.join(format!(
        ".metadata-{}.tmp",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let result = std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|_| "AI_METADATA_CACHE_FAILED".into())
}

fn connection_key(provider: &ResolvedProvider) -> Result<String, String> {
    let url = models_endpoint(provider.kind, &provider.base_url)?;
    let mut hash = Sha256::new();
    hash.update(b"CarbonPaper-model-discovery-v1\0");
    hash.update(serde_json::to_vec(&provider.kind).unwrap_or_default());
    hash.update(url.as_str().as_bytes());
    hash.update(b"\0");
    hash.update(provider.api_key.as_deref().unwrap_or("").as_bytes());
    Ok(format!("provider-{}.json", hex::encode(hash.finalize())))
}

/// Public directory aliases do not alter account-scoped discovery URLs.
fn catalog_endpoint(kind: ProviderKind, base_url: &str) -> Result<reqwest::Url, String> {
    let mut endpoint = models_endpoint(kind, base_url)?;
    if kind == ProviderKind::OpenaiCompatible
        && endpoint.host_str() == Some("api.deepseek.com")
        && endpoint.path() == "/models"
    {
        endpoint.set_path("/v1/models");
    }
    Ok(endpoint)
}

fn cached_catalog(storage: &StorageState) -> CatalogData {
    read_cache::<CatalogData>(&root(storage).join("catalog.json"))
        .map(|cache| cache.data)
        .unwrap_or_else(presets)
}

pub fn enrich_settings_view(storage: &StorageState, view: &mut AiSettingsView) {
    let catalog = cached_catalog(storage);
    for provider in &mut view.providers {
        let Ok(endpoint) = catalog_endpoint(provider.kind, &provider.base_url) else {
            continue;
        };
        let model = catalog
            .providers
            .iter()
            .find(|p| {
                p.kind == provider.kind
                    && catalog_endpoint(p.kind, &p.base_url).ok().as_ref() == Some(&endpoint)
            })
            .and_then(|p| p.models.iter().find(|m| m.id == provider.model));
        if provider.context_tokens_auto {
            if let Some(tokens) = model.and_then(|m| context_capacity(m.context_tokens)) {
                provider.context_tokens = tokens;
            }
        }
        if provider.image_input == ImageInput::Unknown {
            provider.image_input = model.map(|m| m.image_input).unwrap_or_default();
        }
    }
}

fn context_capacity(tokens: Option<u32>) -> Option<u32> {
    tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| tokens.min(MAX_CONTEXT_TOKENS))
}

/// Use the configured account's advertised capacity before the public directory.
/// Reading defaults never contacts a provider or changes a saved manual limit.
pub fn resolve_context_tokens(storage: &StorageState, provider: &ResolvedProvider) -> u32 {
    if let Ok(key) = connection_key(provider) {
        if let Some(cache) = read_cache::<Vec<ModelInfo>>(&root(storage).join(key)) {
            if cache.fresh() {
                if let Some(tokens) = cache
                    .data
                    .iter()
                    .find(|model| model.id == provider.model)
                    .and_then(|model| context_capacity(model.context_tokens))
                {
                    return tokens;
                }
            }
        }
    }
    let model = catalog_endpoint(provider.kind, &provider.base_url)
        .ok()
        .and_then(|endpoint| {
            cached_catalog(storage)
                .providers
                .into_iter()
                .find(|preset| {
                    preset.kind == provider.kind
                        && catalog_endpoint(preset.kind, &preset.base_url)
                            .ok()
                            .as_ref()
                            == Some(&endpoint)
                })
                .and_then(|preset| {
                    preset
                        .models
                        .into_iter()
                        .find(|model| model.id == provider.model)
                })
        });
    model
        .and_then(|model| context_capacity(model.context_tokens))
        .unwrap_or_else(|| fallback_context_tokens(&provider.base_url))
}

pub fn preset_image_input(
    storage: &StorageState,
    kind: ProviderKind,
    base_url: &str,
    model: &str,
) -> ImageInput {
    let Ok(endpoint) = catalog_endpoint(kind, base_url) else {
        return ImageInput::Unknown;
    };
    cached_catalog(storage)
        .providers
        .into_iter()
        .find(|p| {
            p.kind == kind && catalog_endpoint(p.kind, &p.base_url).ok().as_ref() == Some(&endpoint)
        })
        .and_then(|p| p.models.into_iter().find(|m| m.id == model))
        .map(|m| m.image_input)
        .unwrap_or_default()
}

pub fn resolve_image_input(storage: &StorageState, provider: &ResolvedProvider) -> ImageInput {
    // Resolved overrides and fresh explicit probes take precedence.
    if provider.image_input != ImageInput::Unknown {
        return provider.image_input;
    }
    if let Some(value) = cached_image_probe(storage, provider) {
        return value;
    }
    if let Ok(key) = connection_key(provider) {
        if let Some(cache) = read_cache::<Vec<ModelInfo>>(&root(storage).join(key)) {
            if cache.fresh() {
                if let Some(model) = cache.data.iter().find(|m| m.id == provider.model) {
                    if model.image_input != ImageInput::Unknown {
                        return model.image_input;
                    }
                }
            }
        }
    }
    preset_image_input(storage, provider.kind, &provider.base_url, &provider.model)
}

fn probe_path(storage: &StorageState, provider: &ResolvedProvider) -> Result<PathBuf, String> {
    let connection = connection_key(provider)?;
    let model = hex::encode(Sha256::digest(provider.model.as_bytes()));
    Ok(root(storage).join(format!(
        "probe-{}-{model}.json",
        connection.trim_end_matches(".json")
    )))
}

pub fn cached_image_probe(
    storage: &StorageState,
    provider: &ResolvedProvider,
) -> Option<ImageInput> {
    let cache = read_cache::<ImageInput>(&probe_path(storage, provider).ok()?)?;
    let age = chrono::Utc::now()
        .timestamp_millis()
        .saturating_sub(cache.fetched_at_ms);
    ((0..30 * CACHE_TTL_MS).contains(&age) && cache.data != ImageInput::Unknown)
        .then_some(cache.data)
}

pub fn record_image_probe(
    storage: &StorageState,
    provider: &ResolvedProvider,
    value: ImageInput,
) -> Result<(), String> {
    if value == ImageInput::Unknown {
        return Ok(());
    }
    write_cache(&probe_path(storage, provider)?, &value)
}

async fn response_json(mut response: reqwest::Response) -> Result<Value, ProviderError> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(ProviderError::InvalidResponse(
                "Model metadata response is too large.".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_status(
            status,
            &String::from_utf8_lossy(&bytes),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ProviderError::InvalidResponse("Model metadata is not valid JSON.".into()))
}

fn image_input(raw: &Value) -> ImageInput {
    let modalities = raw
        .pointer("/architecture/input_modalities")
        .or_else(|| raw.pointer("/modalities/input"))
        .or_else(|| raw.get("input_modalities"));
    if let Some(values) = modalities.and_then(Value::as_array) {
        if values.iter().any(|v| v.as_str() == Some("image")) {
            return ImageInput::Supported;
        }
        if values.iter().any(Value::is_string) {
            return ImageInput::Unsupported;
        }
    }
    for value in [
        raw.get("supports_vision"),
        raw.pointer("/capabilities/vision"),
        raw.pointer("/capabilities/image_input/supported"),
    ] {
        if let Some(value) = value.and_then(Value::as_bool) {
            return if value {
                ImageInput::Supported
            } else {
                ImageInput::Unsupported
            };
        }
    }
    if raw
        .get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some("vision")))
    {
        return ImageInput::Supported;
    }
    ImageInput::Unknown
}

fn model_info(raw: &Value, source: &str) -> Option<ModelInfo> {
    let id = raw.get("id").and_then(Value::as_str)?;
    if id.is_empty() || id.len() > 512 {
        return None;
    }
    let output = raw
        .pointer("/architecture/output_modalities")
        .or_else(|| raw.pointer("/modalities/output"));
    if output.and_then(Value::as_array).is_some_and(|values| {
        values.iter().any(Value::is_string) && !values.iter().any(|v| v.as_str() == Some("text"))
    }) {
        return None;
    }
    let context_tokens = raw
        .pointer("/limit/context")
        .or_else(|| raw.get("context_length"))
        .or_else(|| raw.get("context_window"))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0);
    let tool_calling = match raw.get("tool_call").and_then(Value::as_bool) {
        Some(true) => ToolCalling::Supported,
        Some(false) => ToolCalling::Unsupported,
        None => {
            if raw
                .get("supported_parameters")
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(|v| v.as_str() == Some("tools")))
            {
                ToolCalling::Supported
            } else {
                ToolCalling::Unknown
            }
        }
    };
    Some(ModelInfo {
        id: id.into(),
        name: raw
            .get("name")
            .or_else(|| raw.get("display_name"))
            .and_then(Value::as_str)
            .filter(|v| v.len() <= 512)
            .unwrap_or(id)
            .into(),
        recommended: false,
        image_input: image_input(raw),
        tool_calling,
        context_tokens,
        source: source.into(),
    })
}

fn normalized_catalog(raw: Value) -> Result<CatalogData, ProviderError> {
    let providers = raw
        .as_object()
        .ok_or_else(|| ProviderError::InvalidResponse("Expected a provider directory.".into()))?;
    let mut catalog = presets();
    let mut endpoint_set: HashSet<String> = catalog
        .providers
        .iter()
        .filter_map(|p| {
            catalog_endpoint(p.kind, &p.base_url)
                .ok()
                .map(|url| format!("{:?}:{url}", p.kind))
        })
        .collect();
    for (id, value) in providers {
        let kind = if value.get("npm").and_then(Value::as_str) == Some("@ai-sdk/anthropic") {
            ProviderKind::Anthropic
        } else {
            ProviderKind::OpenaiCompatible
        };
        let api = value
            .get("api")
            .and_then(Value::as_str)
            .or_else(|| match id.as_str() {
                "openai" => Some("https://api.openai.com/v1"),
                "anthropic" => Some("https://api.anthropic.com"),
                _ => None,
            });
        let Some(api) = api.filter(|api| api.len() <= 512) else {
            continue;
        };
        let Ok(endpoint) = catalog_endpoint(kind, api) else {
            continue;
        };
        let Some(models) = value.get("models").and_then(Value::as_object) else {
            continue;
        };
        let mut models: Vec<_> = models
            .values()
            .filter_map(|raw| model_info(raw, "models.dev"))
            .take(MAX_MODELS)
            .collect();
        models.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        if let Some(existing) = catalog.providers.iter_mut().find(|p| {
            p.kind == kind && catalog_endpoint(p.kind, &p.base_url).ok().as_ref() == Some(&endpoint)
        }) {
            let mut merged: BTreeMap<_, _> = existing
                .models
                .drain(..)
                .map(|m| (m.id.clone(), m))
                .collect();
            for mut model in models {
                if let Some(seed) = merged.get(&model.id) {
                    model.recommended = seed.recommended;
                    model.name.clone_from(&seed.name);
                }
                merged.insert(model.id.clone(), model);
            }
            existing.models = merged.into_values().collect();
            existing
                .models
                .sort_by(|a, b| b.recommended.cmp(&a.recommended).then(a.name.cmp(&b.name)));
            continue;
        }
        // Only protocols implemented by this app can be added remotely. SDK
        // package names are data here; no downloaded code is ever executed.
        if !matches!(
            value.get("npm").and_then(Value::as_str),
            Some("@ai-sdk/openai-compatible" | "@ai-sdk/openai" | "@ai-sdk/anthropic")
        ) || models.is_empty()
            || is_local_url(api)
        {
            continue;
        }
        if endpoint_set.insert(format!("{kind:?}:{endpoint}")) {
            let preset_id = if catalog.providers.iter().any(|p| p.id == *id) {
                format!(
                    "catalog-{id}-{}",
                    &hex::encode(Sha256::digest(endpoint.as_str().as_bytes()))[..8]
                )
            } else {
                id.clone()
            };
            catalog.providers.push(ProviderPreset {
                id: preset_id,
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .chars()
                    .take(128)
                    .collect(),
                kind,
                base_url: api.into(),
                local: false,
                aliases: Vec::new(),
                models,
            });
        }
    }
    Ok(catalog)
}

/// Public directory refreshes once a day; failed refreshes preserve the last
/// successful cache and bundled defaults, without changing saved settings.
pub async fn get_catalog(storage: &StorageState, refresh: bool) -> CatalogView {
    let cache = read_cache::<CatalogData>(&root(storage).join("catalog.json"));
    if !refresh && cache.as_ref().is_some_and(Cache::fresh) {
        let cache = cache.unwrap();
        return CatalogView {
            providers: cache.data.providers,
            fetched_at_ms: Some(cache.fetched_at_ms),
            stale: false,
            warning: None,
        };
    }
    let fetch = async {
        let client = provider::http_client().map_err(ProviderError::Network)?;
        normalized_catalog(response_json(client.get(CATALOG_URL).send().await?).await?)
    };
    let result = tokio::time::timeout(METADATA_TIMEOUT, fetch)
        .await
        .unwrap_or(Err(ProviderError::Timeout));
    match result {
        Ok(data) => {
            let warning = write_cache(&root(storage).join("catalog.json"), &data).err();
            CatalogView {
                providers: data.providers,
                fetched_at_ms: Some(chrono::Utc::now().timestamp_millis()),
                stale: false,
                warning,
            }
        }
        Err(error) => {
            let fetched_at_ms = cache.as_ref().map(|cache| cache.fetched_at_ms);
            CatalogView {
                providers: cache
                    .map(|cache| cache.data)
                    .unwrap_or_else(presets)
                    .providers,
                fetched_at_ms,
                stale: true,
                warning: Some(error.to_string()),
            }
        }
    }
}

async fn fetch_models(provider: &ResolvedProvider) -> Result<Vec<ModelInfo>, ProviderError> {
    let client = provider::http_client().map_err(ProviderError::Network)?;
    let mut url =
        models_endpoint(provider.kind, &provider.base_url).map_err(ProviderError::BadRequest)?;
    let mut models = BTreeMap::new();
    let mut after_ids = HashSet::new();
    for _ in 0..20 {
        let mut request = client.get(url.clone());
        if provider.kind == ProviderKind::Anthropic {
            request = request.header("anthropic-version", "2023-06-01");
            if let Some(key) = &provider.api_key {
                request = request.header("x-api-key", key);
            }
        } else if let Some(key) = &provider.api_key {
            request = request.bearer_auth(key);
        }
        let body = response_json(request.send().await?).await?;
        let rows = body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(Value::as_array)
            .or_else(|| body.as_array())
            .ok_or_else(|| ProviderError::InvalidResponse("Expected a model list.".into()))?;
        for raw in rows {
            if let Some(model) = model_info(raw, "provider") {
                models.insert(model.id.clone(), model);
            }
            if models.len() > MAX_MODELS {
                return Err(ProviderError::InvalidResponse(
                    "Model list is too large.".into(),
                ));
            }
        }
        if provider.kind != ProviderKind::Anthropic
            || body.get("has_more").and_then(Value::as_bool) != Some(true)
        {
            return Ok(models.into_values().collect());
        }
        let last = body
            .get("last_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ProviderError::InvalidResponse("Model list has no continuation ID.".into())
            })?;
        if !after_ids.insert(last.to_string()) {
            return Err(ProviderError::InvalidResponse(
                "Model list repeated its continuation ID.".into(),
            ));
        }
        url = models_endpoint(provider.kind, &provider.base_url)
            .map_err(ProviderError::BadRequest)?;
        url.query_pairs_mut().append_pair("after_id", last);
    }
    Err(ProviderError::InvalidResponse(
        "Model list exceeds the paging limit.".into(),
    ))
}

/// Opening an editor reads the cache only. Explicit discovery contacts exactly
/// the edited endpoint, using its scoped key and refusing redirects.
pub async fn discover_models(
    storage: &StorageState,
    provider: &ResolvedProvider,
    refresh: bool,
) -> Result<ModelListView, String> {
    let path = root(storage).join(connection_key(provider)?);
    let cache = read_cache::<Vec<ModelInfo>>(&path);
    if !refresh {
        return Ok(ModelListView {
            fetched_at_ms: cache.as_ref().map(|c| c.fetched_at_ms),
            stale: cache.as_ref().is_none_or(|c| !c.fresh()),
            models: cache.map(|c| c.data).unwrap_or_default(),
            warning: None,
        });
    }
    let result = tokio::time::timeout(METADATA_TIMEOUT, fetch_models(provider))
        .await
        .unwrap_or(Err(ProviderError::Timeout));
    Ok(match result {
        Ok(mut models) => {
            let catalog = cached_catalog(storage);
            let endpoint = catalog_endpoint(provider.kind, &provider.base_url)?;
            let preset = catalog.providers.iter().find(|p| {
                p.kind == provider.kind
                    && catalog_endpoint(p.kind, &p.base_url).ok().as_ref() == Some(&endpoint)
            });
            for model in &mut models {
                if let Some(known) = preset.and_then(|p| p.models.iter().find(|m| m.id == model.id))
                {
                    model.recommended = known.recommended;
                    if model.image_input == ImageInput::Unknown {
                        model.image_input = known.image_input;
                    }
                    if model.context_tokens.is_none() {
                        model.context_tokens = known.context_tokens;
                    }
                }
            }
            let warning =
                write_cache(&root(storage).join(connection_key(provider)?), &models).err();
            ModelListView {
                models,
                fetched_at_ms: Some(chrono::Utc::now().timestamp_millis()),
                stale: false,
                warning,
            }
        }
        Err(error) => ModelListView {
            fetched_at_ms: cache.as_ref().map(|c| c.fetched_at_ms),
            models: cache.map(|c| c.data).unwrap_or_default(),
            stale: true,
            warning: Some(error.to_string()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn automatic_context_uses_model_capacity_and_preserves_legacy_manual_limits() {
        use super::super::config::{AiSettings, DEFAULT_CONTEXT_TOKENS, LOCAL_CONTEXT_TOKENS};
        let temp = tempfile::tempdir().unwrap();
        let credentials = std::sync::Arc::new(
            crate::credential_manager::CredentialManagerState::new(temp.path().to_path_buf()),
        );
        let storage = StorageState::new(temp.path().join("data"), credentials.clone());
        let catalog = CatalogData {
            providers: vec![ProviderPreset {
                id: "example".into(),
                name: "Example".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: "https://api.example.com/v1".into(),
                local: false,
                aliases: vec![],
                models: vec![model_info(
                    &json!({"id":"large", "context_length":1048576}),
                    "catalog",
                )
                .unwrap()],
            }],
        };
        write_cache(&root(&storage).join("catalog.json"), &catalog).unwrap();
        let settings: AiSettings = serde_json::from_value(json!({"providers":[
            {"id":"auto","name":"Auto","base_url":"https://api.example.com/v1","model":"large"},
            {"id":"manual","name":"Manual","base_url":"https://api.example.com/v1","model":"large","context_tokens":32768},
            {"id":"unknown","name":"Unknown","base_url":"https://other.example.com/v1","model":"unknown","context_tokens":null},
            {"id":"local","name":"Local","base_url":"http://localhost:11434/v1","model":"unknown"}
        ]})).unwrap();
        let auto = settings.resolve(&storage, &credentials, "auto").unwrap();
        assert_eq!(auto.context_tokens, 1048576);
        let mut view = settings.view();
        enrich_settings_view(&storage, &mut view);
        assert!(view.providers[0].context_tokens_auto);
        assert_eq!(view.providers[0].context_tokens, 1048576);
        assert!(!view.providers[1].context_tokens_auto);
        assert_eq!(view.providers[1].context_tokens, 32768);
        assert_eq!(
            settings
                .resolve(&storage, &credentials, "manual")
                .unwrap()
                .context_tokens,
            32768
        );
        assert_eq!(
            settings
                .resolve(&storage, &credentials, "unknown")
                .unwrap()
                .context_tokens,
            DEFAULT_CONTEXT_TOKENS
        );
        assert_eq!(
            settings
                .resolve(&storage, &credentials, "local")
                .unwrap()
                .context_tokens,
            LOCAL_CONTEXT_TOKENS
        );
        let key = root(&storage).join(connection_key(&auto).unwrap());
        write_cache(
            &key,
            &vec![model_info(&json!({"id":"large", "context_length":64000}), "provider").unwrap()],
        )
        .unwrap();
        assert_eq!(
            settings
                .resolve(&storage, &credentials, "auto")
                .unwrap()
                .context_tokens,
            64000
        );
        assert_eq!(
            settings
                .resolve(&storage, &credentials, "manual")
                .unwrap()
                .context_tokens,
            32768
        );
        let mut other_account = auto;
        other_account.api_key = Some("different-account".into());
        assert_eq!(resolve_context_tokens(&storage, &other_account), 1048576);
    }

    #[test]
    fn discovery_does_not_infer_vision_from_model_names_or_image_output() {
        assert_eq!(
            model_info(&json!({"id":"vision-latest"}), "provider")
                .unwrap()
                .image_input,
            ImageInput::Unknown
        );
        assert_eq!(
            model_info(&json!({"id":"empty","input_modalities":[]}), "provider")
                .unwrap()
                .image_input,
            ImageInput::Unknown
        );
        assert_eq!(
            model_info(
                &json!({"id":"malformed","input_modalities":[null,42]}),
                "provider"
            )
            .unwrap()
            .image_input,
            ImageInput::Unknown
        );
        assert_eq!(model_info(&json!({"id":"m","architecture":{"input_modalities":["text","image"],"output_modalities":["text"]}}),"provider").unwrap().image_input,ImageInput::Supported);
        assert_eq!(
            model_info(
                &json!({"id":"m","modalities":{"input":["text"],"output":["text"]}}),
                "provider"
            )
            .unwrap()
            .image_input,
            ImageInput::Unsupported
        );
        assert!(model_info(
            &json!({"id":"paint","modalities":{"input":["text","image"],"output":["image"]}}),
            "provider"
        )
        .is_none());
    }

    #[test]
    fn known_directory_aliases_merge_without_sharing_account_caches() {
        let directory = normalized_catalog(json!({"deepseek": {
            "api": "https://api.deepseek.com", "npm": "@ai-sdk/openai-compatible",
            "models": {"new-model": {"id":"new-model", "modalities":{"input":["text","image"],"output":["text"]}}}
        }})).unwrap();
        let deepseek: Vec<_> = directory
            .providers
            .iter()
            .filter(|p| p.base_url.contains("api.deepseek.com"))
            .collect();
        assert_eq!(deepseek.len(), 1);
        assert_eq!(deepseek[0].id, "deepseek");
        assert!(deepseek[0]
            .models
            .iter()
            .any(|m| m.id == "new-model" && m.image_input == ImageInput::Supported));
        let mut provider = ResolvedProvider {
            base_url: "https://api.deepseek.com".into(),
            ..Default::default()
        };
        let root_key = connection_key(&provider).unwrap();
        provider.base_url = "https://api.deepseek.com/v1".into();
        assert_ne!(root_key, connection_key(&provider).unwrap());
    }

    #[test]
    fn directory_expansion_uses_only_implemented_protocols() {
        let catalog = normalized_catalog(json!({
            "anthropic-proxy": {"api":"https://example.com/anthropic", "npm":"@ai-sdk/anthropic", "models":{
                "chat":{"id":"chat","modalities":{"input":["text","image"],"output":["text"]}}
            }},
            "other-protocol": {"api":"https://example.com/other", "npm":"@ai-sdk/google", "models":{
                "chat":{"id":"chat","modalities":{"input":["text"],"output":["text"]}}
            }}
        })).unwrap();
        assert!(catalog
            .providers
            .iter()
            .any(|p| p.id == "anthropic-proxy" && p.kind == ProviderKind::Anthropic));
        assert!(!catalog.providers.iter().any(|p| p.id == "other-protocol"));
    }

    #[test]
    fn caches_are_scoped_to_endpoint_and_account_and_reject_corruption() {
        let mut provider = ResolvedProvider {
            id: None,
            kind: ProviderKind::OpenaiCompatible,
            base_url: "https://api.example.com/v1".into(),
            model: "a".into(),
            api_key: Some("first-secret".into()),
            tool_calling: ToolCalling::Unknown,
            context_tokens: 32768,
            allow_images: false,
            image_input: ImageInput::Unknown,
        };
        let first = connection_key(&provider).unwrap();
        assert!(!first.contains("first-secret"));
        provider.model = "b".into();
        assert_eq!(first, connection_key(&provider).unwrap());
        provider.api_key = Some("second-secret".into());
        assert_ne!(first, connection_key(&provider).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        write_cache(
            &path,
            &vec![model_info(&json!({"id":"m"}), "provider").unwrap()],
        )
        .unwrap();
        assert!(read_cache::<Vec<ModelInfo>>(&path).unwrap().fresh());
        std::fs::write(&path, b"broken").unwrap();
        assert!(read_cache::<Vec<ModelInfo>>(&path).is_none());
    }

    #[tokio::test]
    async fn endpoint_discovery_reuses_cache_and_preserves_it_after_failed_refresh() {
        use axum::{
            http::{HeaderMap, StatusCode},
            routing::get,
            Json, Router,
        };
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let server = Router::new().route(
            "/v1/models",
            get(move |headers: HeaderMap| {
                let recorder = recorder.clone();
                async move {
                    let mut calls = recorder.lock().unwrap();
                    calls.push(
                        headers
                            .get("authorization")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string(),
                    );
                    if calls.len() == 1 {
                        (
                            StatusCode::OK,
                            Json(json!({"data":[
                                {"id":"new-model","input_modalities":["text","image"]},
                                {"id":"vision-name-only"}
                            ]})),
                        )
                    } else {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error":{"message":"outage"}})),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let storage = StorageState::new(
            dir.path().into(),
            Arc::new(crate::credential_manager::CredentialManagerState::new(
                dir.path().into(),
            )),
        );
        let mut provider = ResolvedProvider {
            base_url: format!("http://{address}/v1"),
            api_key: Some("test-key".into()),
            ..Default::default()
        };
        let first = discover_models(&storage, &provider, true).await.unwrap();
        assert!(first.warning.is_none());
        assert_eq!(first.models[0].id, "new-model");
        assert_eq!(first.models[0].image_input, ImageInput::Supported);
        assert_eq!(first.models[1].image_input, ImageInput::Unknown);
        let cached = discover_models(&storage, &provider, false).await.unwrap();
        assert_eq!(cached.models.len(), 2);
        assert_eq!(seen.lock().unwrap().len(), 1);
        let failed = discover_models(&storage, &provider, true).await.unwrap();
        assert!(failed.stale && failed.warning.is_some());
        assert_eq!(failed.models.len(), 2);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["Bearer test-key", "Bearer test-key"]
        );
        let bytes =
            std::fs::read_to_string(root(&storage).join(connection_key(&provider).unwrap()))
                .unwrap();
        assert!(!bytes.contains("test-key"));
        provider.api_key = Some("another-account".into());
        assert!(discover_models(&storage, &provider, false)
            .await
            .unwrap()
            .models
            .is_empty());
        task.abort();
    }
}
