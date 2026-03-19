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
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::credential_manager::{
    self, encrypt_with_master_key, decrypt_with_master_key, CredentialManagerState,
};
use crate::monitor::{self, MonitorState};
use crate::sensitive_filter::SensitiveFilterState;
use crate::storage::StorageState;
use percent_encoding::percent_decode_str;
use tauri::Manager;

// ==================== Default config ====================

const DEFAULT_MCP_PORT: u16 = 23816;
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
/// Fallback instructions embedded at compile time.
const SKILL_INSTRUCTIONS_FALLBACK: &str = include_str!("../../ai_embedding/skill.md");

// ==================== Runtime state ====================

/// Tauri-managed state for the MCP server lifecycle.
pub struct McpRuntimeState {
    server_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    token_hash: Mutex<Option<[u8; 32]>>,
    idle_check_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl McpRuntimeState {
    pub fn new() -> Self {
        Self {
            server_handle: Mutex::new(None),
            shutdown_tx: Mutex::new(None),
            token_hash: Mutex::new(None),
            idle_check_handle: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        let guard = self.server_handle.lock().unwrap();
        match &*guard {
            Some(h) => !h.is_finished(),
            None => false,
        }
    }

    pub fn set_token_hash(&self, hash: [u8; 32]) {
        let mut guard = self.token_hash.lock().unwrap();
        *guard = Some(hash);
    }

    pub fn get_token_hash(&self) -> Option<[u8; 32]> {
        *self.token_hash.lock().unwrap()
    }
}

/// Internal shared state passed to axum handlers.
struct McpServerInner {
    app_handle: tauri::AppHandle,
    token_hash: [u8; 32],
    skill_instructions: String,
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
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }
    fn error(id: Option<Value>, code: i64, message: String) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(JsonRpcError { code, message, data: None }) }
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
    let auth_header = req.headers().get("authorization").and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            tracing::warn!("MCP {} {} — 401 missing/invalid auth header", method, uri);
            return (StatusCode::UNAUTHORIZED, "Missing or invalid Authorization header").into_response();
        }
    };

    let provided_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
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
        "initialize" => handle_initialize(req.id, &state.skill_instructions),
        "notifications/initialized" => {
            JsonRpcResponse::success(req.id, serde_json::json!({}))
        }
        "ping" => JsonRpcResponse::success(req.id, serde_json::json!({})),
        "tools/list" => handle_tools_list(req.id),
        "tools/call" => handle_tools_call(&state, req.id, req.params).await,
        other => {
            tracing::warn!("MCP unknown method: {}", other);
            JsonRpcResponse::error(req.id, -32601, format!("Method not found"))
        }
    };

    (StatusCode::OK, headers, Json(resp))
}

fn handle_initialize(id: Option<Value>, instructions: &str) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "CarbonPaper",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": instructions
    }))
}

// ==================== Tool definitions ====================

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    let tools = serde_json::json!({
        "tools": [
            {
                "name": "get_snapshots_by_time_range",
                "description": "Get screenshot snapshots within a time range. Returns metadata only (no image data). Timestamps are in milliseconds since Unix epoch.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "start_time": { "type": "number", "description": "Start timestamp in milliseconds" },
                        "end_time": { "type": "number", "description": "End timestamp in milliseconds" },
                        "max_records": { "type": "integer", "description": "Maximum number of records to return (default 500)" }
                    },
                    "required": ["start_time", "end_time"]
                }
            },
            {
                "name": "get_snapshot_details",
                "description": "Get full details of a specific snapshot including metadata, OCR text, and the task cluster it belongs to (if any). By default OCR bounding box coordinates are omitted to save tokens; set include_coords=true to include them.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "description": "Screenshot ID" },
                        "include_coords": { "type": "boolean", "description": "Include OCR bounding box coordinates (default false)" }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "search_ocr_text",
                "description": "Search screenshot OCR text using full-text search. Supports CJK and English text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query text" },
                        "limit": { "type": "integer", "description": "Max results (default 20)" },
                        "offset": { "type": "integer", "description": "Pagination offset (default 0)" },
                        "fuzzy": { "type": "boolean", "description": "Enable fuzzy matching (default true)" },
                        "process_names": { "type": "array", "items": { "type": "string" }, "description": "Filter by process names" },
                        "start_time": { "type": "number", "description": "Filter start time (ms)" },
                        "end_time": { "type": "number", "description": "Filter end time (ms)" },
                        "categories": { "type": "array", "items": { "type": "string" }, "description": "Filter by categories" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "search_nl",
                "description": "Natural language semantic search over screenshots using vector embeddings. Requires the Python monitor process to be running.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural language search query" },
                        "limit": { "type": "integer", "description": "Max results (default 20)" },
                        "offset": { "type": "integer", "description": "Pagination offset (default 0)" },
                        "process_names": { "type": "array", "items": { "type": "string" }, "description": "Filter by process names" },
                        "start_time": { "type": "number", "description": "Filter start time (ms)" },
                        "end_time": { "type": "number", "description": "Filter end time (ms)" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "get_task_clusters",
                "description": "Get task clustering results. Tasks are groups of related screenshots identified by activity patterns.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "layer": { "type": "string", "description": "Clustering layer (e.g. 'hot', 'cold')" },
                        "start_time": { "type": "number", "description": "Filter start time (ms)" },
                        "end_time": { "type": "number", "description": "Filter end time (ms)" },
                        "hide_inactive": { "type": "boolean", "description": "Hide inactive tasks" }
                    }
                }
            },
            {
                "name": "get_task_screenshots",
                "description": "Get screenshots belonging to a specific task cluster, with pagination.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "integer", "description": "Task cluster ID" },
                        "page": { "type": "integer", "description": "Page number (0-based, default 0)" },
                        "page_size": { "type": "integer", "description": "Page size (default 50)" }
                    },
                    "required": ["task_id"]
                }
            },
            {
                "name": "rename_task",
                "description": "Rename a task cluster.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "integer", "description": "Task cluster ID" },
                        "label": { "type": "string", "description": "New label for the task" }
                    },
                    "required": ["task_id", "label"]
                }
            }
        ]
    });
    JsonRpcResponse::success(id, tools)
}

// ==================== Tool dispatch ====================

async fn handle_tools_call(
    state: &McpServerInner,
    id: Option<Value>,
    params: Option<Value>,
) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => return JsonRpcResponse::error(id, -32602, "Missing params".into()),
    };

    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

    tracing::info!("MCP tools/call: tool={}", tool_name);

    let result = match tool_name {
        "get_snapshots_by_time_range" => tool_get_snapshots(state, args).await,
        "get_snapshot_details" => tool_get_snapshot_details(state, args).await,
        "search_ocr_text" => tool_search_ocr(state, args).await,
        "search_nl" => tool_search_nl(state, args).await,
        "get_task_clusters" => tool_get_task_clusters(state, args).await,
        "get_task_screenshots" => tool_get_task_screenshots(state, args).await,
        "rename_task" => tool_rename_task(state, args).await,
        _ => Err(format!("Unknown tool: {}", tool_name)),
    };

    match result {
        Ok(content) => {
            tracing::info!("MCP tools/call: tool={} — ok", tool_name);
            JsonRpcResponse::success(id, serde_json::json!({
                "content": [{ "type": "text", "text": content.to_string() }]
            }))
        }
        Err(e) => {
            tracing::warn!("MCP tools/call: tool={} — error: {}", tool_name, e);
            let code = if e.contains("CNG") || e.contains("authentication") {
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

// ==================== Presidio PII helper ====================

/// Entity detected by Presidio (Python side).
#[derive(Debug, Deserialize)]
struct PiiEntity {
    entity_type: String,
    start: usize,
    end: usize,
    score: f64,
}

/// Call Python's `presidio_analyze` IPC command with a 15-second timeout.
/// The longer timeout accommodates transformer models (trf) which may need
/// 10–30s for first-time loading.
/// Returns per-text entity lists.  Falls back to empty lists on timeout/error.
async fn presidio_analyze_texts(
    app_handle: &tauri::AppHandle,
    texts: &[String],
    language: &str,
    entity_types: &[String],
) -> Vec<Vec<PiiEntity>> {
    let empty: Vec<Vec<PiiEntity>> = texts.iter().map(|_| Vec::new()).collect();

    let monitor_state = match app_handle.try_state::<MonitorState>() {
        Some(s) => s,
        None => return empty,
    };

    let mut payload = serde_json::json!({
        "command": "presidio_analyze",
        "texts": texts,
        "language": language,
    });
    if !entity_types.is_empty() {
        payload.as_object_mut().unwrap().insert(
            "entity_types".to_string(),
            serde_json::to_value(entity_types).unwrap(),
        );
    }

    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        monitor::forward_command_to_python(&monitor_state, payload),
    )
    .await
    {
        Ok(Ok(val)) => val,
        Ok(Err(e)) => {
            tracing::debug!("Presidio IPC error (non-fatal): {}", e);
            return empty;
        }
        Err(_) => {
            tracing::debug!("Presidio IPC timeout (15s), falling back to dict-only");
            return empty;
        }
    };

    // Parse response: { results: [ { entities: [...] }, ... ] }
    let results_arr = match result.get("results").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return empty,
    };

    results_arr
        .iter()
        .map(|item| {
            item.get("entities")
                .and_then(|v| v.as_array())
                .map(|entities| {
                    entities
                        .iter()
                        .filter_map(|e| serde_json::from_value::<PiiEntity>(e.clone()).ok())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// Mask PII entity spans in text with `[TYPE]` labels.
///
/// **Important:** Presidio (Python) returns *character* (codepoint) indices,
/// but Rust `&str` is UTF-8 — slicing by byte offset.  We must map
/// char indices → byte offsets to avoid panicking on multi-byte text.
fn mask_pii_in_text(text: &str, entities: &[PiiEntity]) -> String {
    if entities.is_empty() {
        return text.to_string();
    }

    // Build char-index → byte-offset lookup.  Entry `i` is the byte offset
    // where the `i`-th codepoint starts; the final entry equals `text.len()`.
    let char_to_byte: Vec<usize> = text
        .char_indices()
        .map(|(byte_off, _)| byte_off)
        .chain(std::iter::once(text.len()))
        .collect();
    let char_count = char_to_byte.len() - 1;

    // Filter & sort by char-index start position
    let mut spans: Vec<(usize, usize, &str)> = entities
        .iter()
        .filter(|e| e.score >= 0.3 && e.start < e.end && e.end <= char_count)
        .map(|e| (e.start, e.end, e.entity_type.as_str()))
        .collect();
    spans.sort_by_key(|s| (s.0, std::cmp::Reverse(s.1)));

    // Merge overlapping spans (still in char indices)
    let mut merged: Vec<(usize, usize, &str)> = Vec::new();
    for span in &spans {
        if let Some(last) = merged.last_mut() {
            if span.0 < last.1 {
                if span.1 > last.1 {
                    last.1 = span.1;
                }
                continue;
            }
        }
        merged.push(*span);
    }

    // Reconstruct string, converting char indices to byte offsets for slicing
    let mut result = String::with_capacity(text.len());
    let mut pos = 0usize; // current char index
    for (start, end, entity_type) in &merged {
        if *start > pos {
            result.push_str(&text[char_to_byte[pos]..char_to_byte[*start]]);
        }
        result.push('[');
        result.push_str(entity_type);
        result.push(']');
        pos = *end;
    }
    if pos < char_count {
        result.push_str(&text[char_to_byte[pos]..]);
    }
    result
}

/// Check if any PII entity has score >= threshold.
fn has_pii(entities: &[PiiEntity]) -> bool {
    entities.iter().any(|e| e.score >= 0.3)
}

// ==================== Tool implementations ====================

const CENSORED_LABEL: &str = "[censored]";

/// Decode percent-encoded URL to UTF-8 for sensitive content checking.
/// e.g. `https://zh.wikipedia.org/wiki/%E5%85%AD%E5%9B%9B` → `…/六四`
fn decode_url_for_filter(url: &str) -> String {
    percent_decode_str(url).decode_utf8_lossy().into_owned()
}

async fn tool_get_snapshots(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let start_time = args.get("start_time").and_then(|v| v.as_f64())
        .ok_or("Missing required parameter: start_time")?;
    let end_time = args.get("end_time").and_then(|v| v.as_f64())
        .ok_or("Missing required parameter: end_time")?;
    let max_records = args.get("max_records").and_then(|v| v.as_i64());

    // Convert ms to seconds if needed
    let start_ts = if start_time > 10_000_000_000.0 { start_time / 1000.0 } else { start_time };
    let end_ts = if end_time > 10_000_000_000.0 { end_time / 1000.0 } else { end_time };

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();
    let app_handle = state.app_handle.clone();

    let mut records: Vec<_> = tokio::task::spawn_blocking(move || {
        let filter_mode = filter.get_mode();
        let records = storage.get_screenshots_by_time_range_limited(start_ts, end_ts, max_records.or(Some(500)))?;
        let records: Vec<_> = records.into_iter()
            .filter(|r| {
                match filter_mode.as_str() {
                    // In remove_paragraph/mask mode, don't reject based on window_title alone
                    "remove_paragraph" | "mask" => true,
                    _ => !filter.is_record_sensitive(r.window_title.as_deref(), &[]),
                }
            })
            .map(|mut r| {
                // Identity fields: [censored] for remove_paragraph, mask for mask
                if filter.is_enabled() {
                    if let Some(ref title) = r.window_title {
                        if filter.contains_sensitive(title) {
                            r.window_title = Some(match filter_mode.as_str() {
                                "mask" => filter.mask_sensitive(title),
                                _ => CENSORED_LABEL.to_string(),
                            });
                        }
                    }
                }
                r
            })
            .collect();
        Ok::<_, String>(records)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Presidio second-pass: filter records whose window titles contain PII
    if presidio_enabled && !records.is_empty() {
        let titles: Vec<String> = records.iter()
            .map(|r| r.window_title.clone().unwrap_or_default())
            .collect();
        let pii_results = presidio_analyze_texts(&app_handle, &titles, &presidio_lang, &presidio_entities).await;
        let filter_reload = app_handle.state::<Arc<SensitiveFilterState>>();
        let mode = filter_reload.get_mode();
        match mode.as_str() {
            "reject" => {
                let mut keep = Vec::with_capacity(records.len());
                for (i, r) in records.into_iter().enumerate() {
                    if !has_pii(&pii_results[i]) {
                        keep.push(r);
                    }
                }
                records = keep;
            }
            // remove_paragraph and mask both handle the title (no paragraphs in metadata-only view)
            "remove_paragraph" => {
                for (i, r) in records.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        r.window_title = Some(CENSORED_LABEL.to_string());
                    }
                }
            }
            "mask" => {
                for (i, r) in records.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        r.window_title = Some(mask_pii_in_text(
                            &r.window_title.clone().unwrap_or_default(),
                            &pii_results[i],
                        ));
                    }
                }
            }
            _ => {} // remove_paragraph doesn't apply to metadata-only results
        }
    }

    // Build compact response (strip metadata, image_hash, image_path which are not useful to AI clients)
    let output: Vec<Value> = records.iter().map(|r| {
        let mut obj = serde_json::json!({
            "id": r.id,
            "window_title": r.window_title,
            "process_name": r.process_name,
            "created_at": r.created_at,
            "timestamp": r.timestamp,
        });
        let m = obj.as_object_mut().unwrap();
        if let Some(ref v) = r.source { m.insert("source".into(), serde_json::json!(v)); }
        if let Some(ref v) = r.page_url { m.insert("page_url".into(), serde_json::json!(v)); }
        if let Some(ref v) = r.category { m.insert("category".into(), serde_json::json!(v)); }
        obj
    }).collect();
    Ok(Value::Array(output))
}

async fn tool_get_snapshot_details(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let id = args.get("id").and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: id")?;
    let include_coords = args.get("include_coords").and_then(|v| v.as_bool()).unwrap_or(false);

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();
    let app_handle = state.app_handle.clone();

    let result = tokio::task::spawn_blocking(move || {
        let record = storage.get_screenshot_by_id(id)?;
        match record {
            Some(mut r) => {
                r.metadata = None;
                r.page_icon = None;
                let ocr_results = storage.get_screenshot_ocr_results(r.id)?;
                let filter_mode = filter.get_mode();

                // Dictionary-based filtering (tier 1)
                if filter.is_enabled() {
                    // --- Identity fields: window_title, page_url ---
                    // reject → reject entire record
                    // remove_paragraph → replace with [censored]
                    // mask → character-level mask (█)
                    let title_sensitive = r.window_title.as_ref()
                        .map_or(false, |t| filter.contains_sensitive(t));
                    let url_sensitive = r.page_url.as_ref()
                        .map_or(false, |u| filter.contains_sensitive(&decode_url_for_filter(u)));

                    if title_sensitive || url_sensitive {
                        match filter_mode.as_str() {
                            "mask" => {
                                if title_sensitive {
                                    r.window_title = Some(filter.mask_sensitive(
                                        r.window_title.as_deref().unwrap_or_default(),
                                    ));
                                }
                                if url_sensitive {
                                    r.page_url = Some(CENSORED_LABEL.to_string());
                                }
                            }
                            "remove_paragraph" => {
                                if title_sensitive {
                                    r.window_title = Some(CENSORED_LABEL.to_string());
                                }
                                if url_sensitive {
                                    r.page_url = Some(CENSORED_LABEL.to_string());
                                }
                            }
                            _ => {
                                // "reject" mode
                                return Ok((r, ocr_results, None, true));
                            }
                        }
                    }

                    // --- visible_links (filter by link text only) ---
                    if let Some(ref links) = r.visible_links {
                        match filter_mode.as_str() {
                            "reject" => {
                                if links.iter().any(|l| filter.contains_sensitive(&l.text)) {
                                    return Ok((r, ocr_results, None, true));
                                }
                            }
                            "remove_paragraph" => {
                                let filtered: Vec<_> = links.iter()
                                    .filter(|l| !filter.contains_sensitive(&l.text))
                                    .cloned()
                                    .collect();
                                r.visible_links = if filtered.is_empty() { None } else { Some(filtered) };
                            }
                            "mask" => {
                                let masked: Vec<_> = links.iter()
                                    .map(|l| crate::storage::VisibleLink {
                                        text: filter.mask_sensitive(&l.text),
                                        url: l.url.clone(),
                                    })
                                    .collect();
                                r.visible_links = Some(masked);
                            }
                            _ => {}
                        }
                    }

                    // --- OCR texts (paragraph-level content) ---
                    match filter_mode.as_str() {
                        "reject" => {
                            let any_sensitive = ocr_results.iter()
                                .any(|o| filter.contains_sensitive(&o.text));
                            if any_sensitive {
                                return Ok((r, ocr_results, None, true));
                            }
                        }
                        "remove_paragraph" => {
                            let ocr_results: Vec<_> = ocr_results.into_iter()
                                .filter(|o| !filter.contains_sensitive(&o.text))
                                .collect();
                            let related = storage.get_related_screenshots(r.id, 0)?;
                            return Ok((r, ocr_results, Some(related), false));
                        }
                        "mask" => {
                            let ocr_results: Vec<_> = ocr_results.into_iter()
                                .map(|mut o| { o.text = filter.mask_sensitive(&o.text); o })
                                .collect();
                            let related = storage.get_related_screenshots(r.id, 0)?;
                            return Ok((r, ocr_results, Some(related), false));
                        }
                        _ => {}
                    }
                }

                let related = storage.get_related_screenshots(r.id, 0)?;
                Ok((r, ocr_results, Some(related), false))
            }
            None => Err("not_found".to_string()),
        }
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?;

    // Handle not found or DB errors
    let (mut r, mut ocr_results, related_opt, dict_rejected) = match result {
        Ok(tuple) => tuple,
        Err(ref e) if e == "not_found" => {
            return Ok(serde_json::json!({
                "record": null,
                "ocr_results": [],
                "task": null
            }));
        }
        Err(e) => return Err(e),
    };

    // Check if dictionary filter already rejected
    if dict_rejected {
        return Ok(serde_json::json!({
            "error": "Rejected by user's privacy settings"
        }));
    }
    let related = related_opt.unwrap();

    // Presidio second-pass (tier 2)
    let has_content = !ocr_results.is_empty()
        || r.visible_links.as_ref().map_or(false, |l| !l.is_empty())
        || r.window_title.is_some()
        || r.page_url.is_some();
    if presidio_enabled && has_content {
        let mut all_texts: Vec<String> = Vec::new();
        // Slot 0: window_title (if present)
        if let Some(ref title) = r.window_title {
            all_texts.push(title.clone());
        }
        let title_offset = if r.window_title.is_some() { 1 } else { 0 };
        // Slot title_offset..title_offset+N: OCR texts
        for o in &ocr_results {
            all_texts.push(o.text.clone());
        }
        let link_start_idx = all_texts.len();
        // Slot link_start_idx..: visible_link texts
        if let Some(ref links) = r.visible_links {
            for l in links {
                all_texts.push(l.text.clone());
            }
        }
        let url_idx = all_texts.len();
        // Slot url_idx: decoded page_url (if present)
        if let Some(ref url) = r.page_url {
            all_texts.push(decode_url_for_filter(url));
        }

        let pii_results = presidio_analyze_texts(&app_handle, &all_texts, &presidio_lang, &presidio_entities).await;

        let filter_reload = app_handle.state::<Arc<SensitiveFilterState>>();
        let mode = filter_reload.get_mode();

        // --- Identity fields: title + page_url ---
        let title_has_pii = title_offset == 1 && has_pii(&pii_results[0]);
        let url_has_pii = r.page_url.is_some() && has_pii(&pii_results[url_idx]);

        if title_has_pii || url_has_pii {
            match mode.as_str() {
                "mask" => {
                    if title_has_pii {
                        r.window_title = Some(mask_pii_in_text(
                            &r.window_title.clone().unwrap_or_default(),
                            &pii_results[0],
                        ));
                    }
                    if url_has_pii {
                        r.page_url = Some(CENSORED_LABEL.to_string());
                    }
                }
                "remove_paragraph" => {
                    if title_has_pii {
                        r.window_title = Some(CENSORED_LABEL.to_string());
                    }
                    if url_has_pii {
                        r.page_url = Some(CENSORED_LABEL.to_string());
                    }
                }
                _ => {
                    // "reject" mode
                    return Ok(serde_json::json!({
                        "error": "Rejected by user's privacy settings"
                    }));
                }
            }
        }

        // --- OCR texts (paragraph-level) ---
        match mode.as_str() {
            "reject" => {
                for i in 0..ocr_results.len() {
                    if has_pii(&pii_results[i + title_offset]) {
                        return Ok(serde_json::json!({
                            "error": "Rejected by user's privacy settings"
                        }));
                    }
                }
            }
            "remove_paragraph" => {
                let mut keep = Vec::new();
                for (i, o) in ocr_results.into_iter().enumerate() {
                    if !has_pii(&pii_results[i + title_offset]) {
                        keep.push(o);
                    }
                }
                ocr_results = keep;
            }
            "mask" => {
                for (i, o) in ocr_results.iter_mut().enumerate() {
                    if has_pii(&pii_results[i + title_offset]) {
                        o.text = mask_pii_in_text(&o.text, &pii_results[i + title_offset]);
                    }
                }
            }
            _ => {}
        }

        // Apply to visible_links (text only, not URLs)
        if let Some(ref mut links) = r.visible_links {
            match mode.as_str() {
                "reject" => {
                    for i in 0..links.len() {
                        if has_pii(&pii_results[link_start_idx + i]) {
                            return Ok(serde_json::json!({
                                "error": "Rejected by user's privacy settings"
                            }));
                        }
                    }
                }
                "remove_paragraph" => {
                    let mut keep = Vec::new();
                    for (i, l) in links.iter().enumerate() {
                        if !has_pii(&pii_results[link_start_idx + i]) {
                            keep.push(l.clone());
                        }
                    }
                    *links = keep;
                }
                "mask" => {
                    for (i, l) in links.iter_mut().enumerate() {
                        if has_pii(&pii_results[link_start_idx + i]) {
                            l.text = mask_pii_in_text(&l.text, &pii_results[link_start_idx + i]);
                        }
                    }
                }
                _ => {}
            }
            if links.is_empty() {
                r.visible_links = None;
            }
        }
    }

    // Build response
    let ocr_value: Value = if include_coords {
        serde_json::to_value(&ocr_results).unwrap_or(Value::Null)
    } else {
        ocr_results.iter().map(|o| serde_json::json!({
            "text": o.text,
            "confidence": o.confidence,
        })).collect::<Vec<_>>().into()
    };
    let task = if related.task_id >= 0 {
        serde_json::json!({
            "task_id": related.task_id,
            "task_label": related.task_label
        })
    } else {
        Value::Null
    };
    Ok(serde_json::json!({
        "record": r,
        "ocr_results": ocr_value,
        "task": task
    }))
}

async fn tool_search_ocr(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let query = args.get("query").and_then(|v| v.as_str())
        .ok_or("Missing required parameter: query")?.to_string();
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20) as i32;
    let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let fuzzy = args.get("fuzzy").and_then(|v| v.as_bool()).unwrap_or(true);
    let process_names: Option<Vec<String>> = args.get("process_names")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let start_time = args.get("start_time").and_then(|v| v.as_f64());
    let end_time = args.get("end_time").and_then(|v| v.as_f64());
    let categories: Option<Vec<String>> = args.get("categories")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();
    let app_handle = state.app_handle.clone();

    let mut results = tokio::task::spawn_blocking(move || {
        let results = storage.search_text(&query, limit, offset, fuzzy, process_names, start_time, end_time, categories)?;
        let results: Vec<_> = results.into_iter()
            .filter(|r| !filter.is_record_sensitive(
                r.window_title.as_deref(),
                &[r.text.as_str()],
            ))
            .collect();
        Ok::<_, String>(results)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Presidio second-pass on search results
    if presidio_enabled && !results.is_empty() {
        let texts: Vec<String> = results.iter()
            .map(|r| r.text.clone())
            .collect();
        let pii_results = presidio_analyze_texts(&app_handle, &texts, &presidio_lang, &presidio_entities).await;
        let filter_reload = app_handle.state::<Arc<SensitiveFilterState>>();
        let mode = filter_reload.get_mode();
        match mode.as_str() {
            "reject" | "remove_paragraph" => {
                let mut keep = Vec::new();
                for (i, r) in results.into_iter().enumerate() {
                    if !has_pii(&pii_results[i]) {
                        keep.push(r);
                    }
                }
                results = keep;
            }
            "mask" => {
                for (i, r) in results.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        r.text = mask_pii_in_text(&r.text, &pii_results[i]);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(serde_json::to_value(&results).unwrap_or(Value::Null))
}

async fn tool_search_nl(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let monitor_state = state.app_handle.state::<MonitorState>();

    let payload = serde_json::json!({
        "command": "search_nl",
        "query": args.get("query").ok_or("Missing required parameter: query")?,
        "limit": args.get("limit").unwrap_or(&serde_json::json!(20)),
        "offset": args.get("offset").unwrap_or(&serde_json::json!(0)),
        "process_names": args.get("process_names"),
        "start_time": args.get("start_time"),
        "end_time": args.get("end_time"),
    });

    let result = monitor::forward_command_to_python(&monitor_state, payload).await?;

    // Dictionary filter (tier 1)
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();

    let dict_filter = |item: &Value| -> bool {
        if !filter.is_enabled() {
            return true;
        }
        let title = item.get("metadata")
            .and_then(|m| m.get("window_title"))
            .and_then(|v| v.as_str())
            .or_else(|| item.get("window_title").and_then(|v| v.as_str()));
        let ocr = item.get("ocr_text").and_then(|v| v.as_str());
        let mut texts: Vec<&str> = Vec::new();
        if let Some(t) = ocr { texts.push(t); }
        !filter.is_record_sensitive(title, &texts)
    };

    // Extract items array from the response
    let mut items: Vec<Value> = if let Some(arr) = result.as_array() {
        arr.iter().filter(|item| dict_filter(item)).cloned().collect()
    } else if let Some(obj) = result.as_object() {
        if let Some(arr) = obj.get("results").and_then(|v| v.as_array()) {
            arr.iter().filter(|item| dict_filter(item)).cloned().collect()
        } else {
            return Ok(result.clone());
        }
    } else {
        return Ok(result);
    };

    // Presidio second-pass (tier 2)
    if presidio_enabled && !items.is_empty() {
        let texts: Vec<String> = items.iter()
            .map(|item| {
                item.get("ocr_text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        let pii_results = presidio_analyze_texts(&state.app_handle, &texts, &presidio_lang, &presidio_entities).await;
        let mode = filter.get_mode();

        items = items.into_iter().enumerate().filter_map(|(i, mut item)| {
            if !has_pii(&pii_results[i]) {
                return Some(item);
            }
            match mode.as_str() {
                "reject" | "remove_paragraph" => None,
                "mask" => {
                    if let Some(ocr) = item.get("ocr_text").and_then(|v| v.as_str()) {
                        let masked = mask_pii_in_text(ocr, &pii_results[i]);
                        item.as_object_mut().map(|o| o.insert("ocr_text".to_string(), Value::String(masked)));
                    }
                    Some(item)
                }
                _ => Some(item),
            }
        }).collect();
    }

    // Resolve screenshot_ids from image_hash and clean up output
    let hashes: Vec<String> = items.iter()
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
        tokio::task::spawn_blocking(move || {
            storage.batch_get_screenshot_ids_by_hash(&hashes)
        })
        .await
        .map_err(|e| format!("Task join error: {:?}", e))?
        .unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };

    // Build clean output: add screenshot_id, flatten metadata, remove internal fields
    let cleaned: Vec<Value> = items.iter().map(|item| {
        let image_path = item.get("image_path").and_then(|v| v.as_str()).unwrap_or("");
        let screenshot_id = image_path
            .strip_prefix("memory://")
            .and_then(|hash| hash_to_id.get(hash));

        let window_title = item.get("metadata")
            .and_then(|m| m.get("window_title"))
            .and_then(|v| v.as_str())
            .or_else(|| item.get("window_title").and_then(|v| v.as_str()));
        let process_name = item.get("metadata")
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
        let m = obj.as_object_mut().unwrap();
        if let Some(t) = window_title { m.insert("window_title".into(), Value::String(t.to_string())); }
        if let Some(p) = process_name { m.insert("process_name".into(), Value::String(p.to_string())); }
        obj
    }).collect();

    Ok(Value::Array(cleaned))
}

async fn tool_get_task_clusters(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let layer = args.get("layer").and_then(|v| v.as_str()).map(String::from);
    let start_time = args.get("start_time").and_then(|v| v.as_f64());
    let end_time = args.get("end_time").and_then(|v| v.as_f64());
    let hide_inactive = args.get("hide_inactive").and_then(|v| v.as_bool());

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage_clone = storage.inner().clone();
    let layer_clone = layer.clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();
    let app_handle = state.app_handle.clone();

    let mut tasks = tokio::task::spawn_blocking(move || {
        let filter_mode = filter.get_mode();
        let tasks = storage_clone.get_tasks(
            layer_clone.as_deref(), start_time, end_time,
            hide_inactive, None, None,
        )?;
        let tasks: Vec<_> = tasks.into_iter()
            .filter(|t| {
                match filter_mode.as_str() {
                    "remove_paragraph" | "mask" => true,
                    _ => {
                        let label = t.label.as_deref();
                        let auto_label = t.auto_label.as_deref();
                        let mut texts: Vec<&str> = Vec::new();
                        if let Some(l) = auto_label { texts.push(l); }
                        !filter.is_record_sensitive(label, &texts)
                    }
                }
            })
            .map(|mut t| {
                if filter.is_enabled() {
                    let is_mask = filter_mode == "mask";
                    if let Some(ref label) = t.label {
                        if filter.contains_sensitive(label) {
                            t.label = Some(if is_mask {
                                filter.mask_sensitive(label)
                            } else {
                                CENSORED_LABEL.to_string()
                            });
                        }
                    }
                    if let Some(ref auto_label) = t.auto_label {
                        if filter.contains_sensitive(auto_label) {
                            t.auto_label = Some(if is_mask {
                                filter.mask_sensitive(auto_label)
                            } else {
                                CENSORED_LABEL.to_string()
                            });
                        }
                    }
                }
                t
            })
            .collect();
        Ok::<_, String>(tasks)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Presidio second-pass on task labels
    if presidio_enabled && !tasks.is_empty() {
        let labels: Vec<String> = tasks.iter()
            .map(|t| {
                t.label.clone()
                    .or_else(|| t.auto_label.clone())
                    .unwrap_or_default()
            })
            .collect();
        let pii_results = presidio_analyze_texts(&app_handle, &labels, &presidio_lang, &presidio_entities).await;
        let filter_reload = app_handle.state::<Arc<SensitiveFilterState>>();
        let mode = filter_reload.get_mode();
        match mode.as_str() {
            "reject" => {
                let mut keep = Vec::new();
                for (i, t) in tasks.into_iter().enumerate() {
                    if !has_pii(&pii_results[i]) {
                        keep.push(t);
                    }
                }
                tasks = keep;
            }
            "remove_paragraph" => {
                for (i, t) in tasks.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        t.label = Some(CENSORED_LABEL.to_string());
                        t.auto_label = Some(CENSORED_LABEL.to_string());
                    }
                }
            }
            "mask" => {
                for (i, t) in tasks.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        if let Some(ref label) = t.label {
                            t.label = Some(mask_pii_in_text(label, &pii_results[i]));
                        }
                        if let Some(ref auto_label) = t.auto_label {
                            t.auto_label = Some(mask_pii_in_text(auto_label, &pii_results[i]));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(serde_json::to_value(&tasks).unwrap_or(Value::Null))
}

async fn tool_get_task_screenshots(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let task_id = args.get("task_id").and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: task_id")?;
    let page = args.get("page").and_then(|v| v.as_i64()).unwrap_or(0);
    let page_size = args.get("page_size").and_then(|v| v.as_i64()).unwrap_or(50);

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    let filter = state.app_handle.state::<Arc<SensitiveFilterState>>();
    let filter = filter.inner().clone();
    let (presidio_enabled, presidio_lang, presidio_entities) = filter.get_presidio_config();
    let app_handle = state.app_handle.clone();

    let mut screenshots = tokio::task::spawn_blocking(move || {
        let filter_mode = filter.get_mode();
        let screenshots = storage.get_task_screenshots(task_id, page, page_size)?;
        let screenshots: Vec<_> = screenshots.into_iter()
            .filter(|s| {
                match filter_mode.as_str() {
                    "remove_paragraph" | "mask" => true,
                    _ => !filter.is_record_sensitive(s.window_title.as_deref(), &[]),
                }
            })
            .map(|mut s| {
                if filter.is_enabled() {
                    if let Some(ref title) = s.window_title {
                        if filter.contains_sensitive(title) {
                            s.window_title = Some(match filter_mode.as_str() {
                                "mask" => filter.mask_sensitive(title),
                                _ => CENSORED_LABEL.to_string(),
                            });
                        }
                    }
                }
                s
            })
            .collect();
        Ok::<_, String>(screenshots)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))??;

    // Presidio second-pass on window titles
    if presidio_enabled && !screenshots.is_empty() {
        let titles: Vec<String> = screenshots.iter()
            .map(|s| s.window_title.clone().unwrap_or_default())
            .collect();
        let pii_results = presidio_analyze_texts(&app_handle, &titles, &presidio_lang, &presidio_entities).await;
        let filter_reload = app_handle.state::<Arc<SensitiveFilterState>>();
        let mode = filter_reload.get_mode();
        match mode.as_str() {
            "reject" => {
                let mut keep = Vec::new();
                for (i, s) in screenshots.into_iter().enumerate() {
                    if !has_pii(&pii_results[i]) {
                        keep.push(s);
                    }
                }
                screenshots = keep;
            }
            "remove_paragraph" => {
                for (i, s) in screenshots.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        s.window_title = Some(CENSORED_LABEL.to_string());
                    }
                }
            }
            "mask" => {
                for (i, s) in screenshots.iter_mut().enumerate() {
                    if has_pii(&pii_results[i]) {
                        s.window_title = Some(mask_pii_in_text(
                            &s.window_title.clone().unwrap_or_default(),
                            &pii_results[i],
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    Ok(serde_json::to_value(&screenshots).unwrap_or(Value::Null))
}

async fn tool_rename_task(state: &McpServerInner, args: Value) -> Result<Value, String> {
    let task_id = args.get("task_id").and_then(|v| v.as_i64())
        .ok_or("Missing required parameter: task_id")?;
    let label = args.get("label").and_then(|v| v.as_str())
        .ok_or("Missing required parameter: label")?.to_string();

    let storage = state.app_handle.state::<Arc<StorageState>>();
    let storage = storage.inner().clone();
    tokio::task::spawn_blocking(move || {
        storage.update_task_label(task_id, &label)
    })
    .await
    .map_err(|e| format!("Task join error: {:?}", e))?
    .map(|_| serde_json::json!({"status": "ok"}))
}

// ==================== Token helpers ====================

/// Derive an AES-256 key from the public key for MCP token encryption.
/// This key is always available without Windows Hello authentication.
pub fn derive_mcp_key(credential_state: &CredentialManagerState) -> Result<[u8; 32], String> {
    let public_key = credential_manager::get_cached_public_key(credential_state)
        .or_else(|| credential_manager::load_public_key_from_file(credential_state).ok())
        .ok_or("Public key not available")?;
    let mut hasher = Sha256::new();
    hasher.update(&public_key);
    hasher.update(b"CarbonPaper-MCP-Token-Key-v1");
    Ok(hasher.finalize().into())
}

/// Generate a random 64-character hex token.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

/// Encrypt a token with the MCP-derived key and return base64.
pub fn encrypt_token(credential_state: &CredentialManagerState, token: &str) -> Result<String, String> {
    let key = derive_mcp_key(credential_state)?;
    let encrypted = encrypt_with_master_key(&key, token.as_bytes())
        .map_err(|e| format!("Token encryption failed: {}", e))?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted))
}

/// Decrypt a base64-encoded encrypted token.
pub fn decrypt_token(credential_state: &CredentialManagerState, encrypted_b64: &str) -> Result<String, String> {
    let key = derive_mcp_key(credential_state)?;
    let encrypted = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_b64)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let decrypted = decrypt_with_master_key(&key, &encrypted)
        .map_err(|e| format!("Token decryption failed: {}", e))?;
    String::from_utf8(decrypted).map_err(|e| format!("Invalid UTF-8: {}", e))
}

/// Compute SHA-256 hash of a token string.
pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

// ==================== Presidio model lifecycle helpers ====================

/// Send `presidio_unload` to Python (fire-and-forget, best-effort).
#[allow(dead_code)]
async fn presidio_unload_model(app_handle: &tauri::AppHandle) {
    let monitor_state = match app_handle.try_state::<MonitorState>() {
        Some(s) => s,
        None => return,
    };
    let payload = serde_json::json!({ "command": "presidio_unload" });
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        monitor::forward_command_to_python(&monitor_state, payload),
    ).await {
        Ok(Ok(_)) => tracing::info!("Presidio model unloaded via IPC"),
        Ok(Err(e)) => tracing::debug!("Presidio unload IPC error (non-fatal): {}", e),
        Err(_) => tracing::debug!("Presidio unload IPC timeout"),
    }
}

/// Send `presidio_check_idle` to Python (fire-and-forget, best-effort).
async fn presidio_check_idle(app_handle: &tauri::AppHandle) {
    let monitor_state = match app_handle.try_state::<MonitorState>() {
        Some(s) => s,
        None => return,
    };
    let payload = serde_json::json!({ "command": "presidio_check_idle" });
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        monitor::forward_command_to_python(&monitor_state, payload),
    ).await {
        Ok(Ok(val)) => {
            let unloaded = val.get("unloaded").and_then(|v| v.as_bool()).unwrap_or(false);
            if unloaded {
                tracing::info!("Presidio model idle-unloaded");
            }
        }
        Ok(Err(e)) => tracing::debug!("Presidio idle check IPC error (non-fatal): {}", e),
        Err(_) => tracing::debug!("Presidio idle check IPC timeout"),
    }
}

// ==================== Server lifecycle ====================

/// Start the MCP HTTP server.
/// Automatically stops any existing server before starting.
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

    // Load skill instructions from bundled resource, fall back to compile-time embedded version.
    let skill_instructions = app_handle.path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("ai_embedding").join("skill.md"))
        .and_then(|path| std::fs::read_to_string(&path).ok())
        .unwrap_or_else(|| {
            tracing::warn!("Failed to load ai_embedding/skill.md from resource dir, using embedded fallback");
            SKILL_INSTRUCTIONS_FALLBACK.to_string()
        });

    let inner = Arc::new(McpServerInner { app_handle: app_handle.clone(), token_hash, skill_instructions });

    let app = Router::new()
        .route("/mcp", post(handle_mcp))
        .layer(middleware::from_fn_with_state(inner.clone(), auth_middleware))
        .layer(CorsLayer::permissive())
        .with_state(inner);

    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();

    // Use TcpSocket with SO_REUSEADDR so we can always rebind the port,
    // even if a previous listener wasn't fully released by the OS yet.
    let socket = tokio::net::TcpSocket::new_v4()
        .map_err(|e| format!("Failed to create socket: {}", e))?;
    socket.set_reuseaddr(true)
        .map_err(|e| format!("Failed to set SO_REUSEADDR: {}", e))?;
    socket.bind(addr)
        .map_err(|e| format!("Failed to bind port {}: {}", port, e))?;
    let listener = socket.listen(1024)
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
    {
        let mut guard = mcp_runtime.server_handle.lock().unwrap();
        *guard = Some(handle);
    }
    {
        let mut guard = mcp_runtime.shutdown_tx.lock().unwrap();
        *guard = Some(shutdown_tx);
    }

    // Start periodic idle check for Presidio model (every 60s)
    {
        let app_for_idle = app_handle.clone();
        let idle_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                presidio_check_idle(&app_for_idle).await;
            }
        });
        let mut guard = mcp_runtime.idle_check_handle.lock().unwrap();
        *guard = Some(idle_handle);
    }

    Ok(())
}

/// Stop the MCP HTTP server.
pub async fn stop_server(mcp_runtime: &McpRuntimeState) {
    // Abort the idle check timer
    let idle_handle = {
        let mut guard = mcp_runtime.idle_check_handle.lock().unwrap();
        guard.take()
    };
    if let Some(h) = idle_handle {
        h.abort();
        let _ = h.await;
        tracing::info!("Presidio idle check timer stopped");
    }

    // Send shutdown signal — select! in the server task will drop the
    // serve future (and TcpListener).
    let tx = {
        let mut guard = mcp_runtime.shutdown_tx.lock().unwrap();
        guard.take()
    };
    if let Some(tx) = tx {
        let _ = tx.send(());
        tracing::info!("MCP shutdown signal sent");
    }
    let handle = {
        let mut guard = mcp_runtime.server_handle.lock().unwrap();
        guard.take()
    };
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
        tracing::info!("MCP server task joined");
    }
}

/// Auto-start the MCP server on app launch (called from setup if mcp_enabled).
pub async fn auto_start(
    app_handle: tauri::AppHandle,
    credential_state: &CredentialManagerState,
    storage_state: &StorageState,
    mcp_runtime: &McpRuntimeState,
) -> Result<(), String> {
    let policy = storage_state.load_policy()?;

    let port = policy.get("mcp_port")
        .and_then(|v| v.as_u64())
        .map(|v| v as u16)
        .unwrap_or(DEFAULT_MCP_PORT);

    let encrypted_b64 = policy.get("mcp_token_encrypted")
        .and_then(|v| v.as_str())
        .ok_or("No MCP token found in policy")?;

    let token = decrypt_token(credential_state, encrypted_b64)?;
    let token_hash = hash_token(&token);

    mcp_runtime.set_token_hash(token_hash);

    start_server(app_handle, port, token_hash).await
}

/// Get the configured port from policy.
pub fn get_port(storage_state: &StorageState) -> u16 {
    storage_state.load_policy().ok()
        .and_then(|p| p.get("mcp_port").and_then(|v| v.as_u64()))
        .map(|v| v as u16)
        .unwrap_or(DEFAULT_MCP_PORT)
}
