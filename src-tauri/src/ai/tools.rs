//! Contract tools exposed to the in-app AI.
//!
//! Tools run in-process through [`crate::mcp_server::call_tool`], so the AI
//! sees exactly what external MCP clients see: the same session check,
//! maintenance gate and privacy filtering. This module only adapts the
//! catalog to [`ToolSpec`] and keeps results small enough for a context window.

use serde::Serialize;
use serde_json::{Map, Value};

use super::provider::{ToolCall, ToolSpec};
use crate::mcp_contract;

/// Upper bound on one tool result as sent to the model, in characters.
const MAX_RESULT_CHARS: usize = 12_000;
/// Longest single string kept inside a tool result, in characters.
const MAX_STRING_CHARS: usize = 1_500;
/// Fields that cost tokens without helping the model answer.
const DROPPED_FIELDS: &[&str] = &["image_path", "confidence", "distance", "box_coords"];

/// Which contract tools a run may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolScope {
    /// Only tools that read. Interactive search always uses this.
    #[default]
    ReadOnly,
    /// Also the tools in [`mcp_contract::WRITE_TOOL_NAMES`], for tasks the
    /// user explicitly allowed to change stored data.
    ReadWrite,
}

pub fn specs(scope: ToolScope) -> Vec<ToolSpec> {
    let definitions = match scope {
        ToolScope::ReadOnly => mcp_contract::read_only_tool_definitions(),
        ToolScope::ReadWrite => mcp_contract::tool_definitions()
            .as_array()
            .cloned()
            .unwrap_or_default(),
    };
    definitions
        .into_iter()
        .filter_map(|tool| {
            Some(ToolSpec {
                name: tool.get("name")?.as_str()?.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                parameters: tool.get("inputSchema")?.clone(),
            })
        })
        .collect()
}

/// A screenshot a tool result mentioned, with the fields the interface needs
/// to open it. Time fields are passed through in whatever unit the tool used.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SnapshotRef {
    pub id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_created_at: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
}

impl SnapshotRef {
    fn from_record(id: i64, map: &Map<String, Value>) -> Self {
        let value = |key: &str| map.get(key).filter(|v| !v.is_null()).cloned();
        let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_string);
        Self {
            id,
            timestamp: value("timestamp"),
            screenshot_created_at: value("screenshot_created_at"),
            created_at: value("created_at"),
            window_title: text("window_title"),
            process_name: text("process_name"),
        }
    }

    /// Fills fields this reference lacks from another sighting of the same id.
    fn absorb(&mut self, other: SnapshotRef) {
        self.timestamp = self.timestamp.take().or(other.timestamp);
        self.screenshot_created_at = self
            .screenshot_created_at
            .take()
            .or(other.screenshot_created_at);
        self.created_at = self.created_at.take().or(other.created_at);
        self.window_title = self.window_title.take().or(other.window_title);
        self.process_name = self.process_name.take().or(other.process_name);
    }
}

/// Adds references to `into`, merging repeated ids in first-seen order.
pub fn merge_snapshots(into: &mut Vec<SnapshotRef>, refs: impl IntoIterator<Item = SnapshotRef>) {
    for r in refs {
        match into.iter_mut().find(|existing| existing.id == r.id) {
            Some(existing) => existing.absorb(r),
            None => into.push(r),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Text handed back to the model.
    pub content: String,
    /// Screenshots the result mentions, in first-seen order.
    pub snapshots: Vec<SnapshotRef>,
    /// Number of top-level items when the result is a list.
    pub item_count: Option<usize>,
    pub error: Option<String>,
}

/// Runs one tool call. Failures become results the model can read and react
/// to; they never abort the loop.
pub async fn execute(
    app_handle: &tauri::AppHandle,
    call: &ToolCall,
    allowed: &[ToolSpec],
) -> ToolOutcome {
    if !allowed.iter().any(|tool| tool.name == call.name) {
        return failure(format!("Tool `{}` is not available.", call.name));
    }
    let args = match parse_arguments(&call.arguments) {
        Ok(args) => args,
        Err(e) => return failure(format!("Arguments are not valid JSON: {e}")),
    };
    match crate::mcp_server::call_tool(app_handle, &call.name, args).await {
        Ok(result) => summarize(&call.name, result),
        Err(e) => failure(e),
    }
}

fn failure(message: String) -> ToolOutcome {
    ToolOutcome {
        content: serde_json::json!({ "error": message }).to_string(),
        snapshots: Vec::new(),
        item_count: None,
        error: Some(message),
    }
}

fn parse_arguments(raw: &str) -> Result<Value, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(raw)
}

pub(crate) fn summarize(tool_name: &str, mut result: Value) -> ToolOutcome {
    let mut snapshots = Vec::new();
    collect_snapshots(tool_name, &result, &mut snapshots, 0);
    compact(&mut result);
    let item_count = list_of(&result).map(Vec::len);
    ToolOutcome {
        content: fit_to_budget(result),
        snapshots,
        item_count,
        error: None,
    }
}

/// The list a result is built around: either the value itself or `items`.
fn list_of(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("items").and_then(Value::as_array))
}

fn list_of_mut(value: &mut Value) -> Option<&mut Vec<Value>> {
    if value.is_array() {
        return value.as_array_mut();
    }
    value.get_mut("items").and_then(Value::as_array_mut)
}

/// Screenshot ids appear as `screenshot_id` everywhere, except in the two
/// snapshot tools whose records are screenshots and use a plain `id`.
fn collect_snapshots(tool_name: &str, value: &Value, out: &mut Vec<SnapshotRef>, depth: usize) {
    let plain_id_is_snapshot = matches!(
        tool_name,
        "get_snapshots_by_time_range" | "get_snapshot_details"
    );
    match value {
        Value::Object(map) => {
            let id = map
                .get("screenshot_id")
                .and_then(Value::as_i64)
                .or_else(|| {
                    (plain_id_is_snapshot && depth <= 1)
                        .then(|| map.get("id").and_then(Value::as_i64))
                        .flatten()
                });
            if let Some(id) = id.filter(|id| *id > 0) {
                merge_snapshots(out, [SnapshotRef::from_record(id, map)]);
            }
            for child in map.values() {
                collect_snapshots(tool_name, child, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_snapshots(tool_name, item, out, depth);
            }
        }
        _ => {}
    }
}

fn compact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for field in DROPPED_FIELDS {
                map.remove(*field);
            }
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(compact);
        }
        Value::Array(items) => items.iter_mut().for_each(compact),
        Value::String(text) if text.chars().count() > MAX_STRING_CHARS => {
            let cut: String = text.chars().take(MAX_STRING_CHARS).collect();
            *text = format!("{cut}…");
        }
        _ => {}
    }
}

/// Drops trailing list items until the serialized result fits, and tells the
/// model how many it lost so it can narrow the query or page further.
fn fit_to_budget(mut value: Value) -> String {
    let mut text = value.to_string();
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text;
    }
    let mut omitted = 0usize;
    while let Some(list) = list_of_mut(&mut value) {
        if list.len() <= 1 {
            break;
        }
        // Halve rather than pop one at a time; results can hold hundreds.
        let keep = list.len() / 2;
        omitted += list.len() - keep;
        list.truncate(keep);
        text = value.to_string();
        if text.chars().count() <= MAX_RESULT_CHARS {
            break;
        }
    }
    if text.chars().count() > MAX_RESULT_CHARS {
        let partial: String = text.chars().take(MAX_RESULT_CHARS).collect();
        return serde_json::json!({ "note": "result cut to fit", "partial": partial }).to_string();
    }
    if omitted > 0 {
        return serde_json::json!({
            "note": format!("{omitted} more items omitted to fit; narrow the query or use offset/limit"),
            "result": value,
        })
        .to_string();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn write_scope_adds_exactly_the_write_tools() {
        let all: Vec<String> = specs(ToolScope::ReadWrite)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(all, mcp_contract::TOOL_NAMES);
    }

    #[test]
    fn read_only_specs_exclude_write_tools() {
        let names: Vec<String> = specs(ToolScope::ReadOnly)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(names.contains(&"search_ocr_text".to_string()));
        assert!(!names.iter().any(|n| mcp_contract::is_write_tool(n)));
    }

    fn ids(outcome: &ToolOutcome) -> Vec<i64> {
        outcome.snapshots.iter().map(|s| s.id).collect()
    }

    #[test]
    fn repeated_sightings_fill_in_missing_fields() {
        let result = json!([
            { "screenshot_id": 7 },
            { "screenshot_id": 7, "process_name": "code.exe", "timestamp": 1700000000 }
        ]);
        let outcome = summarize("search_nl", result);
        assert_eq!(outcome.snapshots.len(), 1);
        assert_eq!(
            outcome.snapshots[0].process_name.as_deref(),
            Some("code.exe")
        );
        assert_eq!(outcome.snapshots[0].timestamp, Some(json!(1700000000)));
    }

    #[test]
    fn snapshot_ids_come_from_the_right_fields() {
        let ocr = json!([
            { "id": 900, "screenshot_id": 7, "text": "a", "image_path": "memory://x" },
            { "id": 901, "screenshot_id": 7 },
            { "id": 902, "screenshot_id": 3 }
        ]);
        let outcome = summarize("search_ocr_text", ocr);
        assert_eq!(ids(&outcome), vec![7, 3]);
        assert_eq!(outcome.item_count, Some(3));
        assert!(!outcome.content.contains("image_path"));

        let snapshots = json!([{ "id": 11, "window_title": "t" }, { "id": 12 }]);
        let outcome = summarize("get_snapshots_by_time_range", snapshots);
        assert_eq!(ids(&outcome), vec![11, 12]);
        assert_eq!(outcome.snapshots[0].window_title.as_deref(), Some("t"));

        let clusters = json!([{ "id": 5, "name": "c" }]);
        assert!(summarize("get_smart_clusters", clusters)
            .snapshots
            .is_empty());
    }

    #[test]
    fn oversized_lists_are_trimmed_with_a_note() {
        let items: Vec<Value> = (0..400)
            .map(|i| json!({ "screenshot_id": i + 1, "text": "x".repeat(200) }))
            .collect();
        let outcome = summarize("search_nl", Value::Array(items));
        assert!(outcome.content.chars().count() < MAX_RESULT_CHARS + 200);
        assert!(outcome.content.contains("more items omitted"));
        assert_eq!(outcome.snapshots.len(), 400);
        serde_json::from_str::<Value>(&outcome.content).unwrap();
    }

    #[test]
    fn empty_arguments_mean_an_empty_object() {
        assert_eq!(parse_arguments(" ").unwrap(), json!({}));
        assert!(parse_arguments("{bad").is_err());
    }
}
