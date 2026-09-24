//! HTTP MCP (Model Context Protocol) server for CarbonPaper.
//!
//! Exposes snapshot data to AI tools (e.g. Claude Desktop & Codex) via the MCP protocol
//! over Streamable HTTP. Binds to 127.0.0.1 only. Requires Bearer token auth.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::{oneshot, Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tower_http::cors::CorsLayer;

use crate::credential_manager::CredentialManagerState;
use crate::mcp_contract;
use crate::mcp_token;
use crate::pii;
use crate::sensitive_filter::{FilterMode, SensitiveFilterState};
use crate::storage::smart_cluster::{SmartClusterSummaryRecord, SmartClusterSummaryUpsert};
use crate::storage::{OcrResult, StorageState};
use percent_encoding::percent_decode_str;
use tauri::{Emitter, Manager};

// ==================== Default config ====================

const DEFAULT_MCP_PORT: u16 = 23816;

// ==================== Runtime state ====================

/// Tauri-managed state for the MCP server lifecycle.
pub struct McpRuntimeState {
    // All operations that can change the listener or the credential accepted by
    // it must hold this lock. In particular, a smoke test keeps it while its
    // bearer token is in use so it cannot race a restart or token rotation.
    lifecycle_lock: AsyncMutex<()>,
    server_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    active_port: Mutex<Option<u16>>,
    token_hash: Mutex<Option<[u8; 32]>>,
    last_error: Mutex<Option<String>>,
    generation: AtomicU64,
}

impl Default for McpRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl McpRuntimeState {
    pub fn new() -> Self {
        Self {
            lifecycle_lock: AsyncMutex::new(()),
            server_handle: Mutex::new(None),
            shutdown_tx: Mutex::new(None),
            active_port: Mutex::new(None),
            token_hash: Mutex::new(None),
            last_error: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    pub fn is_running(&self) -> bool {
        let guard = self.server_handle.lock().unwrap_or_else(|e| e.into_inner());
        match &*guard {
            Some(h) => !h.is_finished(),
            None => false,
        }
    }

    /// Serializes listener lifecycle, token-rotation, and smoke-test work.
    pub async fn lock_lifecycle(&self) -> AsyncMutexGuard<'_, ()> {
        self.lifecycle_lock.lock().await
    }

    /// Returns the port owned by the currently-live CarbonPaper listener.
    /// A completed server task is deliberately treated as having no active port,
    /// even if cleanup has not yet removed its bookkeeping.
    pub fn active_port(&self) -> Option<u16> {
        if !self.is_running() {
            return None;
        }
        *self.active_port.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_active_port(&self, port: u16) {
        let mut guard = self.active_port.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(port);
    }

    fn clear_active_port(&self) {
        let mut guard = self.active_port.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    pub fn set_token_hash(&self, hash: [u8; 32]) {
        let mut guard = self.token_hash.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(hash);
        self.bump_generation();
    }

    pub fn get_token_hash(&self) -> Option<[u8; 32]> {
        *self.token_hash.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Monotonically identifies a listener, port, or credential transition. It
    /// is exposed only as diagnostic state so the settings page can invalidate
    /// a report that no longer describes the current service.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn bump_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn clear_last_error(&self) {
        let mut guard = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    pub fn set_last_error(&self, error: String) {
        let mut guard = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(error);
    }

    pub fn get_last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Internal shared state passed to axum handlers.
struct McpServerInner {
    app_handle: tauri::AppHandle,
    token_hash: [u8; 32],
}

fn require_authenticated_session(app_handle: &tauri::AppHandle) -> Result<(), String> {
    let credential_state = app_handle.state::<Arc<CredentialManagerState>>();
    if credential_state.is_session_valid() {
        Ok(())
    } else {
        Err("AUTH_REQUIRED: CarbonPaper is locked. User needs to complete CNG authentication in the app before you retrying this MCP request.".to_string())
    }
}

// ==================== JSON-RPC types ====================

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

// ==================== Auth middleware ====================

async fn auth_middleware(
    State(state): State<Arc<McpServerInner>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            tracing::warn!("MCP {} {} — 401 missing/invalid auth header", method, uri);
            return (
                StatusCode::UNAUTHORIZED,
                "Missing or invalid Authorization header",
            )
                .into_response();
        }
    };

    let provided_hash = mcp_token::hash_token(token);
    if !constant_time_eq(&provided_hash, &state.token_hash) {
        tracing::warn!("MCP {} {} — 401 invalid token", method, uri);
        return (StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }

    tracing::info!("MCP {} {} — auth ok", method, uri);
    next.run(req).await
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ==================== MCP handler ====================

async fn handle_mcp(
    State(state): State<Arc<McpServerInner>>,
    Json(req): Json<JsonRpcRequest>,
) -> (StatusCode, HeaderMap, Json<JsonRpcResponse>) {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());

    tracing::info!("MCP request: method={}", req.method);

    let resp = match req.method.as_str() {
        "initialize" => handle_initialize(req.id),
        "notifications/initialized" => JsonRpcResponse::success(req.id, serde_json::json!({})),
        "ping" => JsonRpcResponse::success(req.id, serde_json::json!({})),
        "tools/list" => handle_tools_list(req.id),
        "tools/call" => handle_tools_call(&state, req.id, req.params).await,
        other => {
            tracing::warn!("MCP unknown method: {}", other);
            JsonRpcResponse::error(req.id, -32601, "Method not found".to_string())
        }
    };

    (StatusCode::OK, headers, Json(resp))
}

fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        serde_json::json!({
            "protocolVersion": mcp_contract::MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "CarbonPaper",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

// ==================== Tool definitions ====================

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    // Keep the tool table deterministic and independent of database/runtime
    // health. A backend may be temporarily unavailable when the tool is called;
    // that is a runtime result, not a change to the MCP tool contract.
    JsonRpcResponse::success(id, mcp_contract::tools_list_result())
}

#[cfg(test)]
mod tool_list_tests {
    use super::*;

    #[test]
    fn tools_list_matches_the_versioned_contract_and_dispatch_table() {
        let response = handle_tools_list(Some(serde_json::json!(1)));
        let tools = response
            .result
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|result| result.get("tools"))
            .and_then(Value::as_array)
            .expect("tools/list result should contain a tools array");

        assert_eq!(
            Value::Array(tools.clone()),
            mcp_contract::tool_definitions()
        );
        assert_eq!(DISPATCHED_TOOL_NAMES, mcp_contract::TOOL_NAMES);
        assert!(tools
            .iter()
            .any(|tool| { tool.get("name").and_then(Value::as_str) == Some("search_nl") }));
    }
}

// ==================== Tool dispatch ====================

macro_rules! define_mcp_tool_dispatch {
    ($($name:literal => $handler:ident),+ $(,)?) => {
        #[cfg(test)]
        const DISPATCHED_TOOL_NAMES: &[&str] = &[$($name),+];

        async fn dispatch_mcp_tool(
            state: &McpServerInner,
            tool_name: &str,
            args: Value,
        ) -> Result<Value, String> {
            match tool_name {
                $($name => $handler(state, args).await,)+
                _ => Err(format!("Unknown tool: {}", tool_name)),
            }
        }
    };
}

define_mcp_tool_dispatch!(
    "get_snapshots_by_time_range" => tool_get_snapshots,
    "get_snapshot_details" => tool_get_snapshot_details,
    "search_ocr_text" => tool_search_ocr,
    "search_nl" => tool_search_nl,
    "get_smart_clusters" => tool_get_smart_clusters,
    "get_smart_cluster_ocr_corpus" => tool_get_smart_cluster_ocr_corpus,
    "get_smart_cluster_summary" => tool_get_smart_cluster_summary,
    "upsert_smart_cluster_summary" => tool_upsert_smart_cluster_summary,
    "delete_smart_cluster_summary" => tool_delete_smart_cluster_summary,
);

async fn handle_tools_call(
    state: &McpServerInner,
    id: Option<Value>,
    params: Option<Value>,
) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => return JsonRpcResponse::error(id, -32602, "Missing params".into()),
    };

    if crate::maintenance::is_active() {
        return JsonRpcResponse::error(
            id,
            -32000,
            crate::maintenance::MAINTENANCE_IN_PROGRESS.into(),
        );
    }

    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    tracing::info!("MCP tools/call: tool={}", tool_name);

    let result = dispatch_mcp_tool(state, tool_name, args).await;

    match result {
        Ok(content) => {
            tracing::info!("MCP tools/call: tool={} — ok", tool_name);
            JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "content": [{ "type": "text", "text": content.to_string() }]
                }),
            )
        }
        Err(e) => {
            tracing::warn!("MCP tools/call: tool={} — error: {}", tool_name, e);
            let code =
                if e.contains("AUTH_REQUIRED") || e.contains("CNG") || e.contains("authentication")
                {
                    -32001 // CNG auth required
                } else if e.contains("Monitor not started") {
                    -32002 // Monitor not running
                } else {
                    -32000 // Generic tool error
                };
            JsonRpcResponse::error(id, code, e)
        }
    }
}

// ==================== Privacy filtering ====================

/// A record withheld because the filter mode is [`FilterMode::Reject`].
#[derive(Debug)]
struct Rejected;

/// One-line identity fields such as window titles: replaced, never removed.
fn filter_identity(
    filter: &SensitiveFilterState,
    mode: FilterMode,
    text: &str,
) -> Result<String, Rejected> {
    let findings = filter.inspect(text, pii::Context::default());
    if !findings.is_flagged() {
        return Ok(filter.kept_text(text, &findings));
    }
    match mode {
        FilterMode::Reject => Err(Rejected),
        FilterMode::RemoveParagraph => Ok(CENSORED_LABEL.to_string()),
        FilterMode::Mask => Ok(filter.masked_text(text, &findings)),
    }
}

/// An OCR segment or link text. `Ok(None)` drops it, which is what the
/// default mode does with every segment that has a sensitive word or
/// personal information.
fn filter_segment(
    filter: &SensitiveFilterState,
    mode: FilterMode,
    text: &str,
    context: pii::Context,
) -> Result<Option<String>, Rejected> {
    let findings = filter.inspect(text, context);
    if !findings.is_flagged() {
        return Ok(Some(filter.kept_text(text, &findings)));
    }
    match mode {
        FilterMode::Reject => Err(Rejected),
        FilterMode::RemoveParagraph => Ok(None),
        FilterMode::Mask => Ok(Some(filter.masked_text(text, &findings))),
    }
}

/// Text joined from several OCR segments, such as a search snippet. The
/// segments can no longer be told apart, so the default mode replaces the
/// whole text and keeps the hit; the snapshot details still return the
/// clean segments.
fn filter_joined_text(
    filter: &SensitiveFilterState,
    mode: FilterMode,
    text: &str,
) -> Result<String, Rejected> {
    Ok(filter_segment(filter, mode, text, pii::Context::default())?
        .unwrap_or_else(|| CENSORED_LABEL.to_string()))
}

/// URLs are checked after percent-decoding and replaced whole when flagged.
fn filter_url(
    filter: &SensitiveFilterState,
    mode: FilterMode,
    url: &str,
) -> Result<String, Rejected> {
    let findings = filter.inspect(&decode_url_for_filter(url), pii::Context::default());
    match (findings.is_flagged(), mode) {
        (false, _) => Ok(url.to_string()),
        (true, FilterMode::Reject) => Err(Rejected),
        (true, _) => Ok(CENSORED_LABEL.to_string()),
    }
}

/// The OCR blocks of one screenshot. Each block is checked with the labels of
/// its neighbours, because forms put "身份证号" and the number in separate
/// blocks.
fn filter_ocr_blocks(
    filter: &SensitiveFilterState,
    mode: FilterMode,
    blocks: Vec<OcrResult>,
) -> Result<Vec<OcrResult>, Rejected> {
    let contexts = {
        let layout: Vec<(&str, Option<pii::Bounds>)> = blocks
            .iter()
            .map(|block| {
                (
                    block.text.as_str(),
                    pii::Bounds::from_points(&block.box_coords),
                )
            })
            .collect();
        pii::block_contexts(&layout)
    };
    let mut kept = Vec::with_capacity(blocks.len());
    for (mut block, context) in blocks.into_iter().zip(contexts) {
        if let Some(text) = filter_segment(filter, mode, &block.text, context)? {
            block.text = text;
            kept.push(block);
        }
    }
    Ok(kept)
}

fn text_flagged(filter: &SensitiveFilterState, text: &str) -> bool {
    filter.inspect(text, pii::Context::default()).is_flagged()
}

fn masked(filter: &SensitiveFilterState, text: &str) -> String {
    let findings = filter.inspect(text, pii::Context::default());
    filter.masked_text(text, &findings)
}

// ==================== Tool implementations ====================

const CENSORED_LABEL: &str = "[censored]";

/// Decode percent-encoded URL to UTF-8 for sensitive content checking.
fn decode_url_for_filter(url: &str) -> String {
    percent_decode_str(url).decode_utf8_lossy().into_owned()
}

fn value_flagged(filter: &SensitiveFilterState, value: &Value) -> bool {
    match value {
        Value::String(s) => text_flagged(filter, s),
        Value::Array(items) => items.iter().any(|v| value_flagged(filter, v)),
        Value::Object(map) => map.values().any(|v| value_flagged(filter, v)),
        _ => false,
    }
}

fn mask_json_strings(filter: &SensitiveFilterState, value: &mut Value) {
    match value {
        Value::String(s) => {
            if text_flagged(filter, s) {
                *s = masked(filter, s);
            }
        }
        Value::Array(items) => {
            for item in items {
                mask_json_strings(filter, item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                mask_json_strings(filter, item);
            }
        }
        _ => {}
    }
}

fn cleanse_smart_cluster_summary(
    mut summary: SmartClusterSummaryRecord,
    filter: &SensitiveFilterState,
) -> Option<SmartClusterSummaryRecord> {
    let flagged = |text: &Option<String>| text.as_deref().is_some_and(|s| text_flagged(filter, s));
    let title = flagged(&summary.title);
    let body = flagged(&summary.summary);
    let ocr = flagged(&summary.ocr_summary);
    let key_points = summary
        .key_points
        .as_ref()
        .is_some_and(|v| value_flagged(filter, v));
    let evidence = summary
        .evidence
        .as_ref()
        .is_some_and(|v| value_flagged(filter, v));

    if !(title || body || ocr || key_points || evidence) {
        return Some(summary);
    }

    match filter.mode() {
        FilterMode::Mask => {
            for (hit, field) in [
                (title, &mut summary.title),
                (body, &mut summary.summary),
                (ocr, &mut summary.ocr_summary),
            ] {
                if let (true, Some(text)) = (hit, field.as_mut()) {
                    *text = masked(filter, text);
                }
            }
            if let Some(value) = summary.key_points.as_mut() {
                mask_json_strings(filter, value);
            }
            if let Some(value) = summary.evidence.as_mut() {
                mask_json_strings(filter, value);
            }
            Some(summary)
        }
        FilterMode::RemoveParagraph => {
            for (hit, field) in [
                (title, &mut summary.title),
                (body, &mut summary.summary),
                (ocr, &mut summary.ocr_summary),
            ] {
                if hit {
                    *field = Some(CENSORED_LABEL.to_string());
                }
            }
            if key_points {
                summary.key_points = None;
            }
            if evidence {
                summary.evidence = None;
            }
            Some(summary)
        }
        FilterMode::Reject => None,
    }
}

fn cleanse_smart_cluster_record(
    mut cluster: crate::storage::smart_cluster::SmartClusterRecord,
    filter: &SensitiveFilterState,
) -> Option<crate::storage::smart_cluster::SmartClusterRecord> {
    let flagged = |text: Option<&str>| text.is_some_and(|s| text_flagged(filter, s));
    let anchor = text_flagged(filter, &cluster.anchor_text);
    let display_name = flagged(cluster.display_name.as_deref());
    let process_name = flagged(cluster.last_process_name.as_deref());
    let window_title = flagged(cluster.last_window_title.as_deref());

    if !(anchor || display_name || process_name || window_title) {
        return Some(cluster);
    }

    match filter.mode() {
        FilterMode::Mask => {
            if anchor {
                cluster.anchor_text = masked(filter, &cluster.anchor_text);
            }
            for (hit, field) in [
                (display_name, &mut cluster.display_name),
                (process_name, &mut cluster.last_process_name),
                (window_title, &mut cluster.last_window_title),
            ] {
                if let (true, Some(text)) = (hit, field.as_mut()) {
                    *text = masked(filter, text);
                }
            }
            Some(cluster)
        }
        FilterMode::RemoveParagraph => {
            if anchor {
                cluster.anchor_text = CENSORED_LABEL.to_string();
            }
            for (hit, field) in [
                (display_name, &mut cluster.display_name),
                (process_name, &mut cluster.last_process_name),
                (window_title, &mut cluster.last_window_title),
            ] {
                if hit {
                    *field = Some(CENSORED_LABEL.to_string());
                }
            }
            Some(cluster)
        }
        FilterMode::Reject => None,
    }
}

#[cfg(test)]
mod smart_cluster_filter_tests {
    use super::*;
    use crate::sensitive_filter::SensitiveFilterState;
    use crate::storage::smart_cluster::SmartClusterRecord;

    fn cluster() -> SmartClusterRecord {
        SmartClusterRecord {
            id: 1,
            anchor_text: "research notes".to_string(),
            display_name: Some("Research".to_string()),
            threshold: 0.5,
            enabled: true,
            dominant_color: None,
            created_at: String::new(),
            updated_at: String::new(),
            assignment_count: Some(1),
            recent_assignment_count: Some(1),
            last_assigned_at: None,
            last_process_name: None,
            last_window_title: None,
            summary: None,
        }
    }

    fn filter_with_mode(mode: &str) -> SensitiveFilterState {
        let filter = SensitiveFilterState::with_test_words(&["alice", "chrome", "private"]);
        let mut config = filter.get_config();
        config.mode = mode.to_string();
        filter.update_config(config);
        filter
    }

    #[test]
    fn rejects_clusters_when_any_new_metadata_field_is_sensitive() {
        let filter = filter_with_mode("reject");

        for field in ["display_name", "last_process_name", "last_window_title"] {
            let mut record = cluster();
            match field {
                "display_name" => record.display_name = Some("alice's notes".to_string()),
                "last_process_name" => record.last_process_name = Some("chrome".to_string()),
                "last_window_title" => {
                    record.last_window_title = Some("private portal".to_string())
                }
                _ => unreachable!(),
            }
            assert!(
                cleanse_smart_cluster_record(record, &filter).is_none(),
                "sensitive {field} should reject the cluster"
            );
        }
    }

    #[test]
    fn masks_new_metadata_fields_without_changing_safe_fields() {
        let filter = filter_with_mode("mask");
        let mut record = cluster();
        record.display_name = Some("alice's notes".to_string());
        record.last_process_name = Some("chrome".to_string());
        record.last_window_title = Some("private portal".to_string());

        let result = cleanse_smart_cluster_record(record, &filter).expect("cluster retained");

        assert_eq!(result.anchor_text, "research notes");
        assert!(result.display_name.unwrap().contains('█'));
        assert!(result.last_process_name.unwrap().contains('█'));
        assert!(result.last_window_title.unwrap().contains('█'));
    }

    #[test]
    fn censors_new_metadata_fields_in_remove_paragraph_mode() {
        let filter = filter_with_mode("remove_paragraph");
        let mut record = cluster();
        record.display_name = Some("alice's notes".to_string());
        record.last_process_name = Some("chrome".to_string());
        record.last_window_title = Some("private portal".to_string());

        let result = cleanse_smart_cluster_record(record, &filter).expect("cluster retained");

        assert_eq!(result.display_name.as_deref(), Some(CENSORED_LABEL));
        assert_eq!(result.last_process_name.as_deref(), Some(CENSORED_LABEL));
        assert_eq!(result.last_window_title.as_deref(), Some(CENSORED_LABEL));
    }

    #[test]
    fn personal_information_counts_like_a_sensitive_word() {
        let filter = filter_with_mode("remove_paragraph");
        let mut record = cluster();
        record.last_window_title = Some("13812345678 - 微信".to_string());

        let result = cleanse_smart_cluster_record(record, &filter).expect("cluster retained");

        assert_eq!(result.last_window_title.as_deref(), Some(CENSORED_LABEL));
        assert_eq!(result.anchor_text, "research notes");
    }
}

#[cfg(test)]
mod privacy_filter_tests {
    use super::*;

    fn filter(mode: FilterMode) -> SensitiveFilterState {
        let filter = SensitiveFilterState::with_test_words(&["private"]);
        let mut config = filter.get_config();
        config.mode = mode.as_str().to_string();
        filter.update_config(config);
        filter
    }

    fn block(id: i64, text: &str, left: f64, top: f64) -> OcrResult {
        let (right, bottom) = (left + 200.0, top + 20.0);
        OcrResult {
            id,
            screenshot_id: 1,
            text: text.to_string(),
            confidence: 0.99,
            box_coords: vec![
                vec![left, top],
                vec![right, top],
                vec![right, bottom],
                vec![left, bottom],
            ],
            created_at: String::new(),
        }
    }

    fn texts(blocks: &[OcrResult]) -> Vec<&str> {
        blocks.iter().map(|block| block.text.as_str()).collect()
    }

    #[test]
    fn default_mode_removes_only_the_affected_segments() {
        let filter = SensitiveFilterState::with_test_words(&["private"]);
        assert_eq!(filter.mode(), FilterMode::RemoveParagraph);
        let blocks = vec![
            block(1, "会议纪要", 10.0, 10.0),
            block(2, "联系人手机 13812345678", 10.0, 40.0),
            block(3, "private roadmap", 10.0, 70.0),
            block(4, "下周三发布", 10.0, 100.0),
        ];

        let kept = filter_ocr_blocks(&filter, filter.mode(), blocks).unwrap();

        assert_eq!(texts(&kept), ["会议纪要", "下周三发布"]);
    }

    #[test]
    fn a_label_in_a_neighbouring_block_counts() {
        // Sixteen digits after OCR dropped two: an ID number only next to its label.
        let filter = filter(FilterMode::RemoveParagraph);
        let value = block(2, "1355782003011497", 300.0, 10.0);

        let alone =
            filter_ocr_blocks(&filter, FilterMode::RemoveParagraph, vec![value.clone()]).unwrap();
        assert_eq!(texts(&alone), ["1355782003011497"]);

        let label = block(1, "身份证号", 10.0, 10.0);
        let kept =
            filter_ocr_blocks(&filter, FilterMode::RemoveParagraph, vec![label, value]).unwrap();
        assert_eq!(texts(&kept), ["身份证号"]);
    }

    #[test]
    fn reject_withholds_the_record_and_mask_labels_the_match() {
        let blocks = || vec![block(1, "手机 13812345678", 10.0, 10.0)];
        assert!(
            filter_ocr_blocks(&filter(FilterMode::Reject), FilterMode::Reject, blocks()).is_err()
        );

        let kept =
            filter_ocr_blocks(&filter(FilterMode::Mask), FilterMode::Mask, blocks()).unwrap();
        assert_eq!(texts(&kept), ["手机 [PHONE_NUMBER]"]);
    }

    #[test]
    fn titles_urls_and_joined_snippets() {
        let mode = FilterMode::RemoveParagraph;
        let filter = filter(mode);

        assert_eq!(
            filter_identity(&filter, mode, "微信 - 张三").unwrap(),
            "微信 - 张三"
        );
        assert_eq!(
            filter_identity(&filter, mode, "13812345678 - 通话").unwrap(),
            CENSORED_LABEL
        );
        assert_eq!(
            filter_url(&filter, mode, "https://example.com/u?tel=138%201234%205678").unwrap(),
            CENSORED_LABEL
        );
        assert_eq!(
            filter_url(&filter, mode, "https://example.com/docs").unwrap(),
            "https://example.com/docs"
        );
        // A snippet joins several segments, so the whole text is replaced.
        assert_eq!(
            filter_joined_text(&filter, mode, "项目进展 联系 13812345678 下周发布").unwrap(),
            CENSORED_LABEL
        );
        assert_eq!(
            filter_joined_text(&filter, mode, "项目进展 下周发布").unwrap(),
            "项目进展 下周发布"
        );
    }

    #[test]
    fn long_numbers_are_masked_in_kept_segments_when_enabled() {
        let filter = filter(FilterMode::RemoveParagraph);
        let order = || vec![block(1, "订单编号：203496817759901245", 10.0, 10.0)];

        let kept = filter_ocr_blocks(&filter, FilterMode::RemoveParagraph, order()).unwrap();
        assert_eq!(texts(&kept), ["订单编号：203496817759901245"]);

        let mut config = filter.get_config();
        config.pii_mask_long_numbers = true;
        filter.update_config(config);
        let kept = filter_ocr_blocks(&filter, FilterMode::RemoveParagraph, order()).unwrap();
        assert_eq!(texts(&kept), ["订单编号：[LONG_NUMBER]"]);
    }
}

async fn tool_get_snapshots(state: &McpServerInner, args: Value) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let start_time = args
        .get("start_time")
        .and_then(|v| v.as_f64())
        .ok_or("Missing required parameter: start_time")?;
    let end_time = args
        .get("end_time")
        .and_then(|v| v.as_f64())
        .ok_or("Missing required parameter: end_time")?;
    let max_records = args.get("max_records").and_then(|v| v.as_i64());

    // Convert ms to seconds if needed
    let start_ts = if start_time > 10_000_000_000.0 {
        start_time / 1000.0
    } else {
        start_time
    };
    let end_ts = if end_time > 10_000_000_000.0 {
        end_time / 1000.0
    } else {
        end_time
    };

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let records: Vec<_> = tokio::task::spawn_blocking(move || {
        let mode = filter.mode();
        let records = storage.get_screenshots_by_time_range_limited(
            start_ts,
            end_ts,
            max_records.or(Some(500)),
        )?;
        // Metadata only: a flagged title or URL is replaced, or the record is
        // withheld in reject mode.
        let records: Vec<_> = records
            .into_iter()
            .filter_map(|mut r| {
                if let Some(title) = r.window_title.take() {
                    r.window_title = Some(filter_identity(&filter, mode, &title).ok()?);
                }
                if let Some(url) = r.page_url.take() {
                    r.page_url = Some(filter_url(&filter, mode, &url).ok()?);
                }
                Some(r)
            })
            .collect();
        Ok::<_, String>(records)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Build compact response (strip metadata, image_hash, image_path which are not useful to AI clients)
    let output: Vec<Value> = records
        .iter()
        .map(|r| {
            let mut obj = serde_json::json!({
                "id": r.id,
                "window_title": r.window_title,
                "process_name": r.process_name,
                "created_at": r.created_at,
                "timestamp": r.timestamp,
            });
            let m = obj.as_object_mut().expect("just constructed as object");
            if let Some(ref v) = r.source {
                m.insert("source".into(), serde_json::json!(v));
            }
            if let Some(ref v) = r.page_url {
                m.insert("page_url".into(), serde_json::json!(v));
            }
            if let Some(ref v) = r.category {
                m.insert("category".into(), serde_json::json!(v));
            }
            obj
        })
        .collect();
    Ok(Value::Array(output))
}

async fn tool_get_snapshot_details(state: &McpServerInner, args: Value) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let id = args
        .get("id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: id")?;
    let include_coords = args
        .get("include_coords")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let result = tokio::task::spawn_blocking(move || {
        let Some(mut r) = storage.get_screenshot_by_id(id)? else {
            return Ok(None);
        };
        r.metadata = None;
        r.page_icon = None;
        let ocr_results = storage.get_screenshot_ocr_results(r.id)?;
        let mode = filter.mode();

        let cleanse = || -> Result<Vec<OcrResult>, Rejected> {
            if let Some(title) = r.window_title.take() {
                r.window_title = Some(filter_identity(&filter, mode, &title)?);
            }
            if let Some(url) = r.page_url.take() {
                r.page_url = Some(filter_url(&filter, mode, &url)?);
            }
            // Links are filtered by their text; the URL is the target page.
            if let Some(links) = r.visible_links.take() {
                let mut kept = Vec::with_capacity(links.len());
                for link in links {
                    let text = filter_segment(&filter, mode, &link.text, pii::Context::default())?;
                    if let Some(text) = text {
                        kept.push(crate::storage::VisibleLink {
                            text,
                            url: link.url,
                        });
                    }
                }
                r.visible_links = (!kept.is_empty()).then_some(kept);
            }
            filter_ocr_blocks(&filter, mode, ocr_results)
        };
        let cleansed = cleanse();
        Ok::<_, String>(Some(cleansed.map(|ocr| (r, ocr))))
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    let (r, ocr_results) = match result {
        None => {
            return Ok(serde_json::json!({
                "record": null,
                "ocr_results": []
            }));
        }
        Some(Err(Rejected)) => {
            return Ok(serde_json::json!({
                "error": "Rejected by user's privacy settings"
            }));
        }
        Some(Ok(cleansed)) => cleansed,
    };

    // Build response
    let ocr_value: Value = if include_coords {
        serde_json::to_value(&ocr_results).unwrap_or(Value::Null)
    } else {
        ocr_results
            .iter()
            .map(|o| {
                serde_json::json!({
                    "text": o.text,
                    "confidence": o.confidence,
                })
            })
            .collect::<Vec<_>>()
            .into()
    };
    Ok(serde_json::json!({
        "record": r,
        "ocr_results": ocr_value
    }))
}

async fn tool_search_ocr(state: &McpServerInner, args: Value) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("Missing required parameter: query")?
        .to_string();
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20) as i32;
    let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let fuzzy = args.get("fuzzy").and_then(|v| v.as_bool()).unwrap_or(true);
    let process_names: Option<Vec<String>> = args
        .get("process_names")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let start_time = args.get("start_time").and_then(|v| v.as_f64());
    let end_time = args.get("end_time").and_then(|v| v.as_f64());
    let categories: Option<Vec<String>> = args
        .get("categories")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let results = tokio::task::spawn_blocking(move || {
        let mode = filter.mode();
        let results = storage.search_text(
            &query,
            limit,
            offset,
            fuzzy,
            process_names,
            start_time,
            end_time,
            categories,
        )?;
        // Each hit is one OCR segment. Its neighbours are not loaded, so only
        // labels inside the segment count as context.
        let results: Vec<_> = results
            .into_iter()
            .filter_map(|mut r| {
                if let Some(title) = r.window_title.take() {
                    r.window_title = Some(filter_identity(&filter, mode, &title).ok()?);
                }
                r.text = filter_segment(&filter, mode, &r.text, pii::Context::default()).ok()??;
                Some(r)
            })
            .collect();
        Ok::<_, String>(results)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Strip box_coords to save tokens — not useful in MCP search results
    let stripped: Vec<Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "screenshot_id": r.screenshot_id,
                "text": r.text,
                "confidence": r.confidence,
                "image_path": r.image_path,
                "window_title": r.window_title,
                "process_name": r.process_name,
                "category": r.category,
                "created_at": r.created_at,
                "screenshot_created_at": r.screenshot_created_at,
                "timestamp": r.timestamp,
            })
        })
        .collect();

    Ok(serde_json::to_value(&stripped).unwrap_or(Value::Null))
}

async fn tool_search_nl(state: &McpServerInner, args: Value) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or("Missing required parameter: query")?
        .to_string();
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(u64::from(crate::clip_query::MAX_CLIP_RESULTS)) as u32;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(crate::clip_query::MAX_CLIP_OFFSET)) as u32;
    let process_names: Vec<String> = args
        .get("process_names")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let start_time = args.get("start_time").and_then(Value::as_f64);
    let end_time = args.get("end_time").and_then(Value::as_f64);

    // Use the same Rust CLIP path as the application search surface.
    let rust = crate::clip_query::try_rust_clip_query(
        &state.app_handle,
        crate::clip_query::ClipQueryRequest {
            query: &query,
            limit,
            offset,
            process_names: &process_names,
            start_time,
            end_time,
        },
    )
    .await;

    let items = match rust {
        crate::clip_query::ClipQueryOutcome::Served(results) => results,
        crate::clip_query::ClipQueryOutcome::Unavailable(reason) => {
            return Err(format!("CLIP search unavailable: {reason}"));
        }
    };

    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let mode = filter.mode();
    let items: Vec<Value> = items
        .into_iter()
        .filter_map(|mut item| {
            let title = item
                .get("metadata")
                .and_then(|m| m.get("window_title"))
                .and_then(Value::as_str)
                .or_else(|| item.get("window_title").and_then(Value::as_str))
                .map(str::to_string);
            if let Some(title) = title {
                let title = Value::String(filter_identity(&filter, mode, &title).ok()?);
                if let Some(meta) = item.get_mut("metadata").and_then(Value::as_object_mut) {
                    if meta.contains_key("window_title") {
                        meta.insert("window_title".into(), title.clone());
                    }
                }
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("window_title".into(), title);
                }
            }
            // The snippet joins several OCR segments; see filter_joined_text.
            let snippet = item
                .get("ocr_text")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(snippet) = snippet {
                let snippet = filter_joined_text(&filter, mode, &snippet).ok()?;
                if let Some(obj) = item.as_object_mut() {
                    obj.insert("ocr_text".into(), Value::String(snippet));
                }
            }
            Some(item)
        })
        .collect();

    // Resolve screenshot_ids from image_hash and clean up output
    let hashes: Vec<String> = items
        .iter()
        .filter_map(|item| {
            item.get("image_path")
                .and_then(|v| v.as_str())
                .and_then(|p| p.strip_prefix("memory://"))
                .map(String::from)
        })
        .collect();

    let hash_to_id = if !hashes.is_empty() {
        let storage = state.app_handle.state::<Arc<StorageState>>();
        let storage = storage.inner().clone();
        tokio::task::spawn_blocking(move || storage.batch_get_screenshot_ids_by_hash(&hashes))
            .await
            .map_err(|e| format!("Task join error: {:?}", e))?
            .unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };

    // Build clean output: add screenshot_id, flatten metadata, remove internal fields
    let cleaned: Vec<Value> = items
        .iter()
        .map(|item| {
            let image_path = item
                .get("image_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let screenshot_id = image_path
                .strip_prefix("memory://")
                .and_then(|hash| hash_to_id.get(hash));

            let window_title = item
                .get("metadata")
                .and_then(|m| m.get("window_title"))
                .and_then(|v| v.as_str())
                .or_else(|| item.get("window_title").and_then(|v| v.as_str()));
            let process_name = item
                .get("metadata")
                .and_then(|m| m.get("process_name"))
                .and_then(|v| v.as_str())
                .or_else(|| item.get("process_name").and_then(|v| v.as_str()));

            let mut obj = serde_json::json!({
                "screenshot_id": screenshot_id,
                "ocr_text": item.get("ocr_text"),
                "distance": item.get("distance"),
                "similarity": item.get("similarity"),
                "screenshot_created_at": item.get("screenshot_created_at"),
            });
            let m = obj.as_object_mut().expect("just constructed as object");
            if let Some(t) = window_title {
                m.insert("window_title".into(), Value::String(t.to_string()));
            }
            if let Some(p) = process_name {
                m.insert("process_name".into(), Value::String(p.to_string()));
            }
            obj
        })
        .collect();

    Ok(Value::Array(cleaned))
}

async fn tool_get_smart_clusters(state: &McpServerInner, _args: Value) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let clusters = tokio::task::spawn_blocking(move || {
        let clusters = storage.list_smart_clusters()?;
        let clusters: Vec<_> = clusters
            .into_iter()
            .filter_map(|mut c| {
                c = cleanse_smart_cluster_record(c, &filter)?;
                c.summary = c
                    .summary
                    .and_then(|summary| cleanse_smart_cluster_summary(summary, &filter));
                Some(c)
            })
            .collect();
        Ok::<_, String>(clusters)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::to_value(&clusters).unwrap_or(Value::Null))
}

async fn tool_get_smart_cluster_ocr_corpus(
    state: &McpServerInner,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let cluster_id = args
        .get("cluster_id")
        .or_else(|| args.get("smart_cluster_id"))
        .and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: cluster_id")?;
    let page = args
        .get("page")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    let page_size = args
        .get("page_size")
        .and_then(|v| v.as_i64())
        .unwrap_or(50)
        .clamp(1, 200);
    let include_empty_ocr = args
        .get("include_empty_ocr")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let items = tokio::task::spawn_blocking(move || {
        let mode = filter.mode();
        let pages = storage.list_smart_cluster_ocr_corpus_blocks(cluster_id, page, page_size)?;
        let items: Vec<_> = pages
            .into_iter()
            .filter_map(|(mut item, blocks)| {
                if let Some(title) = item.window_title.take() {
                    item.window_title = Some(filter_identity(&filter, mode, &title).ok()?);
                }
                // Rebuilt from the remaining segments, joined with spaces as
                // the unfiltered corpus is.
                let kept = filter_ocr_blocks(&filter, mode, blocks).ok()?;
                item.ocr_text = kept
                    .iter()
                    .map(|block| block.text.as_str())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !include_empty_ocr && item.ocr_text.trim().is_empty() {
                    return None;
                }
                Some(item)
            })
            .collect();
        Ok::<_, String>(items)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({
        "cluster_id": cluster_id,
        "page": page,
        "page_size": page_size,
        "items": items,
    }))
}

async fn tool_get_smart_cluster_summary(
    state: &McpServerInner,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let cluster_id = args
        .get("cluster_id")
        .or_else(|| args.get("smart_cluster_id"))
        .and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: cluster_id")?;
    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();

    let summary = tokio::task::spawn_blocking(move || {
        let summary = storage.get_smart_cluster_summary(cluster_id)?;
        Ok::<_, String>(summary.and_then(|s| cleanse_smart_cluster_summary(s, &filter)))
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({
        "cluster_id": cluster_id,
        "summary": summary,
    }))
}

async fn tool_upsert_smart_cluster_summary(
    state: &McpServerInner,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let cluster_id = args
        .get("cluster_id")
        .or_else(|| args.get("smart_cluster_id"))
        .and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: cluster_id")?;
    let as_string = |name: &str| {
        args.get(name)
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
    };
    let as_value = |name: &str| args.get(name).filter(|v| !v.is_null()).cloned();
    let input = SmartClusterSummaryUpsert {
        smart_cluster_id: cluster_id,
        title: as_string("title"),
        summary: as_string("summary"),
        ocr_summary: as_string("ocr_summary"),
        key_points: as_value("key_points"),
        evidence: as_value("evidence"),
        source_snapshot_count: args.get("source_snapshot_count").and_then(|v| v.as_i64()),
        source_hash: as_string("source_hash"),
        model_provider: as_string("model_provider"),
        model_name: as_string("model_name"),
        prompt_version: as_string("prompt_version"),
    };

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let saved = tokio::task::spawn_blocking(move || storage.upsert_smart_cluster_summary(&input))
        .await
        .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({
        "status": "ok",
        "summary": saved,
    }))
}

async fn tool_delete_smart_cluster_summary(
    state: &McpServerInner,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(&state.app_handle)?;

    let cluster_id = args
        .get("cluster_id")
        .or_else(|| args.get("smart_cluster_id"))
        .and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: cluster_id")?;
    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let deleted =
        tokio::task::spawn_blocking(move || storage.delete_smart_cluster_summary(cluster_id))
            .await
            .map_err(|e| format!("Task join error: {:?}", e))??;

    Ok(serde_json::json!({
        "status": "ok",
        "deleted": deleted,
        "cluster_id": cluster_id,
    }))
}

// ==================== Server lifecycle ====================

/// Start the MCP HTTP server.
/// Automatically stops any existing server before starting. The caller must hold
/// [`McpRuntimeState::lock_lifecycle`] for the complete operation.
pub async fn start_server(
    app_handle: tauri::AppHandle,
    port: u16,
    token_hash: [u8; 32],
) -> Result<(), String> {
    use tauri::Manager;

    // Stop any existing server first
    {
        let mcp_runtime = app_handle.state::<McpRuntimeState>();
        stop_server(&mcp_runtime).await;
    }

    let inner = Arc::new(McpServerInner {
        app_handle: app_handle.clone(),
        token_hash,
    });

    let app = Router::new()
        .route("/mcp", post(handle_mcp))
        .layer(middleware::from_fn_with_state(
            inner.clone(),
            auth_middleware,
        ))
        .layer(CorsLayer::permissive())
        .with_state(inner);

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();

    // Keep the default exclusive bind behavior. On Windows, enabling
    // SO_REUSEADDR can allow another local process to share a listener port,
    // which would undermine the smoke test's ownership check.
    let socket =
        tokio::net::TcpSocket::new_v4().map_err(|e| format!("Failed to create socket: {}", e))?;
    socket
        .bind(addr)
        .map_err(|e| format!("Failed to bind port {}: {}", port, e))?;
    let listener = socket
        .listen(1024)
        .map_err(|e| format!("Failed to listen on port {}: {}", port, e))?;

    tracing::info!("MCP server listening on http://127.0.0.1:{}/mcp", port);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        // Use select! so that when shutdown_rx fires, the serve future
        // (including TcpListener) is synchronously dropped by Rust's
        // ownership rules, immediately freeing the port.
        tokio::select! {
            res = axum::serve(listener, app) => {
                if let Err(e) = res {
                    tracing::error!("MCP server error: {:?}", e);
                }
            }
            _ = shutdown_rx => {
                tracing::info!("MCP server shutdown signal received");
            }
        }
    });

    let mcp_runtime = app_handle.state::<McpRuntimeState>();
    mcp_runtime.clear_last_error();
    {
        let mut guard = mcp_runtime
            .server_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(handle);
    }
    {
        let mut guard = mcp_runtime
            .shutdown_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(shutdown_tx);
    }
    mcp_runtime.set_active_port(port);
    mcp_runtime.bump_generation();

    Ok(())
}

/// Stop the MCP HTTP server. The caller must hold
/// [`McpRuntimeState::lock_lifecycle`] for the complete operation.
pub async fn stop_server(mcp_runtime: &McpRuntimeState) {
    // Clear this before tearing down the task so no status or probe can mistake
    // a listener that is in the process of stopping for an active endpoint.
    mcp_runtime.clear_active_port();
    mcp_runtime.bump_generation();

    // Send shutdown signal — select! in the server task will drop the
    // serve future (and TcpListener).
    let tx = {
        let mut guard = mcp_runtime
            .shutdown_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(tx) = tx {
        let _ = tx.send(());
        tracing::info!("MCP shutdown signal sent");
    }
    let handle = {
        let mut guard = mcp_runtime
            .server_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
        tracing::info!("MCP server task joined");
    }
}

/// Auto-start the MCP server on app launch (called from setup if mcp_enabled).
///
/// v2 MCP tokens are encrypted with the Windows Hello master key, so auto-start
/// intentionally fails with AUTH_REQUIRED while CarbonPaper is locked. The
/// renderer-visible status remains enabled/not running until the user unlocks
/// and explicitly starts MCP again.
pub async fn auto_start(
    app_handle: tauri::AppHandle,
    credential_state: &CredentialManagerState,
    storage_state: &StorageState,
    mcp_runtime: &McpRuntimeState,
) -> Result<(), String> {
    let _lifecycle_guard = mcp_runtime.lock_lifecycle().await;
    auto_start_locked(app_handle, credential_state, storage_state, mcp_runtime).await
}

async fn auto_start_locked(
    app_handle: tauri::AppHandle,
    credential_state: &CredentialManagerState,
    storage_state: &StorageState,
    mcp_runtime: &McpRuntimeState,
) -> Result<(), String> {
    let policy = storage_state.load_policy()?;

    let port = port_from_policy(&policy);

    let encrypted_b64 = policy
        .get("mcp_token_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("No MCP token found in policy")?;

    let token = match mcp_token::decrypt_token(credential_state, encrypted_b64) {
        Ok(token) => token,
        Err(e) => {
            mcp_runtime.set_last_error(e.clone());
            let state = if e.contains("AUTH_REQUIRED") {
                "pending_auth"
            } else {
                "error"
            };
            let _ = app_handle.emit(
                "mcp-status-changed",
                serde_json::json!({ "state": state, "error": e.clone() }),
            );
            return Err(e);
        }
    };
    let token_hash = mcp_token::hash_token(&token);

    mcp_runtime.set_token_hash(token_hash);

    match start_server(app_handle.clone(), port, token_hash).await {
        Ok(()) => Ok(()),
        Err(e) => {
            mcp_runtime.set_last_error(e.clone());
            let _ = app_handle.emit(
                "mcp-status-changed",
                serde_json::json!({ "state": "error", "error": e.clone() }),
            );
            Err(e)
        }
    }
}

pub async fn restore_if_enabled(
    app_handle: tauri::AppHandle,
    credential_state: &CredentialManagerState,
    storage_state: &StorageState,
    mcp_runtime: &McpRuntimeState,
) -> Result<bool, String> {
    let _lifecycle_guard = mcp_runtime.lock_lifecycle().await;
    let policy = storage_state.load_policy()?;
    let enabled = policy
        .get("mcp_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !enabled || mcp_runtime.is_running() {
        return Ok(false);
    }

    match auto_start_locked(
        app_handle.clone(),
        credential_state,
        storage_state,
        mcp_runtime,
    )
    .await
    {
        Ok(()) => {
            let _ = app_handle.emit(
                "mcp-status-changed",
                serde_json::json!({ "state": "running" }),
            );
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// Get the configured port from policy.
pub(crate) fn port_from_policy(policy: &Value) -> u16 {
    policy
        .get("mcp_port")
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_MCP_PORT)
}

pub fn get_port(storage_state: &StorageState) -> u16 {
    storage_state
        .load_policy()
        .ok()
        .map(|policy| port_from_policy(&policy))
        .unwrap_or(DEFAULT_MCP_PORT)
}

#[cfg(test)]
mod runtime_state_tests {
    use super::*;

    #[test]
    fn policy_port_rejects_zero_and_values_outside_u16() {
        assert_eq!(port_from_policy(&serde_json::json!({ "mcp_port": 1 })), 1);
        assert_eq!(
            port_from_policy(&serde_json::json!({ "mcp_port": 65535 })),
            65535
        );
        assert_eq!(
            port_from_policy(&serde_json::json!({ "mcp_port": 0 })),
            DEFAULT_MCP_PORT
        );
        assert_eq!(
            port_from_policy(&serde_json::json!({ "mcp_port": 65536 })),
            DEFAULT_MCP_PORT
        );
    }
}
