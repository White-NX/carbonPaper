//! Versioned MCP and Agent Skill contract shared by runtime status and `tools/list`.

use serde_json::{json, Value};

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const TOOL_SCHEMA_VERSION: u64 = 3;

/// Shared by HTTP MCP errors and the in-process Agent. Business errors remain
/// `Result<T, String>` internally; the public payload always carries a code.
pub fn error_payload(message: &str) -> Value {
    let prefix = message.split(':').next().unwrap_or_default();
    let code = if !prefix.is_empty()
        && prefix.contains('_')
        && prefix.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
    {
        prefix
    } else if message.contains("AUTH_REQUIRED") || message.contains("authentication required") {
        "AUTH_REQUIRED"
    } else {
        "TOOL_FAILED"
    };
    json!({"error": {"code": code, "message": message}})
}

pub fn invalid_argument(message: impl std::fmt::Display) -> String {
    format!("INVALID_ARGUMENT: {message}")
}

/// A half-open interval of absolute Unix milliseconds. Never infer units from
/// magnitude: a value returned by one tool can be used by every other tool.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeRange {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl TimeRange {
    pub fn from_args(args: &Value, required: bool) -> Result<Self, String> {
        if args.get("start_time").is_some() || args.get("end_time").is_some() {
            return Err(invalid_argument(
                "Use start_ms and end_ms (Unix milliseconds) in tool schema v3",
            ));
        }
        let read = |key: &str| match args.get(key) {
            None | Some(Value::Null) if !required => Ok(None),
            Some(value) => value
                .as_i64()
                .filter(|n| *n >= 0 && *n <= 253_402_300_799_999)
                .map(Some)
                .ok_or_else(|| {
                    invalid_argument(format!(
                        "{key} must be an integer Unix-millisecond timestamp"
                    ))
                }),
            None => Err(invalid_argument(format!("Missing {key}"))),
        };
        let range = Self {
            start_ms: read("start_ms")?,
            end_ms: read("end_ms")?,
        };
        if matches!((range.start_ms, range.end_ms), (Some(a), Some(b)) if a >= b) {
            return Err(invalid_argument("start_ms must be before exclusive end_ms"));
        }
        Ok(range)
    }

    /// Legacy storage queries use inclusive second bounds. Captures have
    /// second precision, so ceil(start) / floor(end - 1) preserve [start, end).
    pub fn storage_seconds(self) -> (Option<f64>, Option<f64>) {
        (
            self.start_ms.map(|n| ((n + 999) / 1000) as f64),
            self.end_ms.map(|n| (n - 1).div_euclid(1000) as f64),
        )
    }

    pub fn contains(self, captured_at_ms: Option<i64>) -> bool {
        match captured_at_ms {
            Some(time) => {
                self.start_ms.is_none_or(|start| time >= start)
                    && self.end_ms.is_none_or(|end| time < end)
            }
            None => self.start_ms.is_none() && self.end_ms.is_none(),
        }
    }
}

pub fn screenshot_id(args: &Value) -> Result<i64, String> {
    if args.get("id").is_some() || args.get("ocr_id").is_some() {
        return Err(invalid_argument(
            "Use screenshot_id from a screenshot result, not an OCR row id",
        ));
    }
    args.get("screenshot_id")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid_argument("screenshot_id must be a positive screenshot ID"))
}

/// Validate the schema vocabulary used by this catalog before dispatch. This
/// prevents misspelled filters and invalid optional values becoming broad,
/// apparently successful searches. Opaque JSON fields have no schema type.
pub fn validate_arguments(name: &str, args: &Value) -> Result<(), String> {
    let definitions = tool_definitions();
    let tool = definitions
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or_else(|| format!("TOOL_NOT_FOUND: {name}"))?;
    if name == "get_snapshot_details" {
        screenshot_id(args)?;
    }
    if matches!(
        name,
        "get_snapshots_by_time_range" | "search_ocr_text" | "search_nl"
    ) {
        TimeRange::from_args(args, name == "get_snapshots_by_time_range")?;
    }
    validate_value(args, &tool["inputSchema"], "arguments")
}

fn validate_value(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let invalid = || invalid_argument(format!("Invalid {path}"));
    let valid_type = match schema["type"].as_str() {
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("integer") => value.as_i64().is_some(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        _ => true,
    };
    if !valid_type {
        return Err(invalid());
    }
    if schema["enum"]
        .as_array()
        .is_some_and(|values| !values.contains(value))
    {
        return Err(invalid());
    }
    if let Some(number) = value.as_f64() {
        if schema["minimum"].as_f64().is_some_and(|min| number < min)
            || schema["maximum"].as_f64().is_some_and(|max| number > max)
        {
            return Err(invalid());
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|min| length < min)
            || schema["maxLength"].as_u64().is_some_and(|max| length > max)
        {
            return Err(invalid());
        }
    }
    if let Some(object) = value.as_object() {
        for key in schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !object.contains_key(key) {
                return Err(invalid_argument(format!("Missing {path}.{key}")));
            }
        }
        for (key, child) in object {
            if let Some(property) = schema["properties"].get(key) {
                validate_value(child, property, &format!("{path}.{key}"))?;
            } else if schema["additionalProperties"] == false {
                return Err(invalid_argument(format!("Unknown {path}.{key}")));
            }
        }
    }
    if let Some(items) = value.as_array().filter(|_| schema.get("items").is_some()) {
        for item in items {
            validate_value(item, &schema["items"], path)?;
        }
    }
    Ok(())
}
pub const AGENT_SKILL_ID: &str = "carbonpaper-memory";
pub const AGENT_SKILL_SOURCE_REPOSITORY: &str = "https://github.com/White-NX/carbonPaperSkill";

pub const TOOL_NAMES: &[&str] = &[
    "get_snapshots_by_time_range",
    "get_snapshot_details",
    "search_ocr_text",
    "search_nl",
    "get_smart_clusters",
    "get_smart_cluster_ocr_corpus",
    "get_smart_cluster_summary",
    "get_recap_days",
    "get_recap_day",
    "search_recaps",
    "upsert_smart_cluster_summary",
    "delete_smart_cluster_summary",
];

/// Tools that change stored data. In-app AI callers only receive them when a
/// caller explicitly opts in; interactive AI search never does.
pub const WRITE_TOOL_NAMES: &[&str] = &[
    "upsert_smart_cluster_summary",
    "delete_smart_cluster_summary",
];

pub fn is_write_tool(name: &str) -> bool {
    WRITE_TOOL_NAMES.contains(&name)
}

/// The catalog without the tools listed in [`WRITE_TOOL_NAMES`].
pub fn read_only_tool_definitions() -> Vec<Value> {
    tool_definitions()
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| !is_write_tool(name))
        })
        .cloned()
        .collect()
}

pub fn tool_definitions() -> Value {
    let mut tools = json!([
        {
            "name": "get_snapshots_by_time_range",
            "description": "Get screenshot metadata in the half-open interval [start_ms, end_ms). Results use screenshot_id and captured_at_ms (Unix milliseconds); null capture time means unknown. Returns the earliest matching records up to max_records.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "start_ms": { "type": "integer", "minimum": 0, "description": "Inclusive start, Unix milliseconds" },
                    "end_ms": { "type": "integer", "minimum": 0, "description": "Exclusive end, Unix milliseconds" },
                    "max_records": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum number of records to return (default 500)" }
                },
                "required": ["start_ms", "end_ms"]
            }
        },
        {
            "name": "get_snapshot_details",
            "description": "Read one screenshot using screenshot_id from another tool. Returns status=found, record with captured_at_ms, and OCR rows with ocr_processed_at_ms. An empty OCR list means no text is currently available for export. Missing screenshots return SNAPSHOT_NOT_FOUND; privacy rejection returns PRIVACY_REJECTED. Include OCR bounding boxes with include_coords=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "screenshot_id": { "type": "integer", "minimum": 1, "description": "screenshot_id returned by another tool; OCR row IDs cannot be used here" },
                    "include_coords": { "type": "boolean", "description": "Include OCR bounding box coordinates (default false)" }
                },
                "required": ["screenshot_id"]
            }
        },
        {
            "name": "search_ocr_text",
            "description": "Find words visible on screen using lexical OCR search. Extract distinctive keywords from a question, e.g. 保存路径 or save location. Separate keywords with spaces; a Chinese question is otherwise one long phrase. Fuzzy matching tolerates spelling/OCR errors. Results use screenshot_id for evidence, ocr_id for the text row, captured_at_ms for capture time, and ocr_processed_at_ms for recognition completion; all times are Unix milliseconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Visible words or phrases; use space-separated keywords rather than a full question" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Max results (default 20)" },
                    "offset": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "Pagination offset (default 0)" },
                    "fuzzy": { "type": "boolean", "description": "Enable fuzzy matching (default true)" },
                    "process_names": { "type": "array", "items": { "type": "string" }, "description": "Filter by process names" },
                    "start_ms": { "type": "integer", "minimum": 0, "description": "Inclusive capture time, Unix milliseconds" },
                    "end_ms": { "type": "integer", "minimum": 0, "description": "Exclusive capture time, Unix milliseconds" },
                    "categories": { "type": "array", "items": { "type": "string" }, "description": "Filter by categories" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "search_nl",
            "description": "Natural language visual search over screenshots using Chinese-CLIP image embeddings. Describe what the screen looks like; use search_ocr_text for visible words. Results use screenshot_id and captured_at_ms (Unix milliseconds). A time filter excludes captures whose time is unknown.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language search query" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Max results (default 20)" },
                    "offset": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "Pagination offset (default 0)" },
                    "process_names": { "type": "array", "items": { "type": "string" }, "description": "Filter by process names" },
                    "start_ms": { "type": "integer", "minimum": 0, "description": "Inclusive capture time, Unix milliseconds" },
                    "end_ms": { "type": "integer", "minimum": 0, "description": "Exclusive capture time, Unix milliseconds" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "get_smart_clusters",
            "description": "List smart clusters with cluster_id, assignment counts, stored summaries, and explicit Unix-millisecond created_at_ms, updated_at_ms and last_assigned_at_ms fields. Unknown times are null.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "get_smart_cluster_ocr_corpus",
            "description": "Get smart-cluster screenshots with joined OCR text, screenshot_id, captured_at_ms and assigned_at_ms (Unix milliseconds). Results are paginated and ordered by rerank score.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cluster_id": { "type": "integer", "description": "Smart cluster ID" },
                    "page": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "Page number (0-based, default 0)" },
                    "page_size": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Page size (default 50, max 200)" },
                    "include_empty_ocr": { "type": "boolean", "description": "Include snapshots that have no OCR text (default false)" }
                },
                "required": ["cluster_id"]
            }
        },
        {
            "name": "get_smart_cluster_summary",
            "description": "Get the stored AI-generated summary for a smart cluster, if one exists.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cluster_id": { "type": "integer", "description": "Smart cluster ID" }
                },
                "required": ["cluster_id"]
            }
        },
        {
            "name": "get_recap_days",
            "description": "List recorded daily recap dates, newest first, from the most recent 730 non-deleted dates. Each YYYY-MM-DD date retains its saved calendar; inspect calendars[date] for the recorded time zone and UTC boundaries. A listed date may have pending or stale periods; use get_recap_day to read available content. Does not generate recaps.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "start_date": { "type": "string", "description": "Inclusive local date YYYY-MM-DD" },
                    "end_date": { "type": "string", "description": "Inclusive local date YYYY-MM-DD" },
                    "offset": { "type": "integer", "minimum": 0, "description": "Pagination offset (default 0)" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Max dates (default 30)" }
                }
            }
        },
        {
            "name": "get_recap_day",
            "description": "Read a saved daily recap with corrected activities, period overviews and screenshot evidence. Date is YYYY-MM-DD in the returned calendar, whose recorded time zone and day/period boundaries remain fixed. Sources use screenshot_id and captured_at_ms; all instants are Unix milliseconds. Activities are paginated chronologically; follow next_offset or narrow to batch_start_ms. Empty/pending periods do not establish inactivity. Does not generate or modify recap content. Use get_snapshot_details to verify source details.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "date": { "type": "string", "description": "Local date YYYY-MM-DD" },
                    "batch_start_ms": { "type": "integer", "description": "Optional period start from a previous response, in Unix milliseconds" },
                    "offset": { "type": "integer", "minimum": 0, "description": "Activity offset (default 0)" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max activities (default 10)" }
                },
                "required": ["date"]
            }
        },
        {
            "name": "search_recaps",
            "description": "Search saved, corrected daily recap activities across dates using keywords, local text meaning, or both. Date filters are inclusive saved YYYY-MM-DD labels; inspect calendars[date] for each result's recorded time zone and boundaries. Sources use screenshot_id and captured_at_ms (Unix milliseconds). Verify factual claims with get_snapshot_details. Unlike screenshot semantic search this covers all saved recap dates. Hybrid mode falls back to keywords when the local recap index is incomplete or unavailable; inspect semantic_status. Summaries can omit details, so use OCR or image search when needed. Does not generate recaps or build the index.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "mode": { "type": "string", "enum": ["keyword", "semantic", "hybrid"], "default": "hybrid" },
                    "start_date": { "type": "string", "description": "Inclusive local date YYYY-MM-DD" },
                    "end_date": { "type": "string", "description": "Inclusive local date YYYY-MM-DD" },
                    "include_details": { "type": "boolean", "default": true, "description": "Include saved key facts in retrieval" },
                    "ranking_revision": { "type": "string", "description": "When paging, pass ranking_revision from the previous result. If results changed, restart at offset 0 without this field." },
                    "offset": { "type": "integer", "minimum": 0, "maximum": 100000 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 20 }
                },
                "required": ["query"]
            }
        },
        {
            "name": "upsert_smart_cluster_summary",
            "description": "Create or replace the stored AI-generated title, cluster overview, OCR summary, key points, evidence, and model metadata for a smart cluster.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cluster_id": { "type": "integer", "description": "Smart cluster ID" },
                    "title": { "type": "string", "description": "Short AI-generated title" },
                    "summary": { "type": "string", "description": "Cluster-level introduction or overview" },
                    "ocr_summary": { "type": "string", "description": "Integrated summary of OCR information across assigned snapshots" },
                    "key_points": { "description": "Optional JSON array/object of key points" },
                    "evidence": { "description": "Optional JSON array/object describing source snapshot evidence" },
                    "source_snapshot_count": { "type": "integer", "minimum": 0, "description": "Number of source snapshots used" },
                    "source_hash": { "type": "string", "description": "Optional hash/fingerprint of the source corpus" },
                    "model_provider": { "type": "string", "description": "Model provider name" },
                    "model_name": { "type": "string", "description": "Model name" },
                    "prompt_version": { "type": "string", "description": "Prompt/template version" }
                },
                "required": ["cluster_id"]
            }
        },
        {
            "name": "delete_smart_cluster_summary",
            "description": "Delete the stored AI-generated summary for a smart cluster. The smart cluster and its assigned snapshots are preserved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cluster_id": { "type": "integer", "description": "Smart cluster ID" }
                },
                "required": ["cluster_id"]
            }
        }
    ]);
    for tool in tools.as_array_mut().expect("tool catalog is an array") {
        tool["inputSchema"]["additionalProperties"] = false.into();
        if let Some(property) = tool["inputSchema"]["properties"].get_mut("cluster_id") {
            property["minimum"] = 1.into();
        }
        for key in ["start_ms", "end_ms"] {
            if let Some(property) = tool["inputSchema"]["properties"].get_mut(key) {
                property["maximum"] = 253_402_300_799_999_i64.into();
            }
        }
    }
    tools
}

pub fn tools_list_result() -> Value {
    json!({ "tools": tool_definitions() })
}

pub fn contract_document() -> Value {
    json!({
        "tool_schema_version": TOOL_SCHEMA_VERSION,
        "mcp_protocol_version": MCP_PROTOCOL_VERSION,
        "time_contract": {
            "instant_unit": "unix_milliseconds",
            "capture_field": "captured_at_ms",
            "ocr_processing_field": "ocr_processed_at_ms",
            "unknown_instant": null,
            "range": "start_ms <= captured_at_ms < end_ms",
            "calendar": "Recap dates retain their recorded time zone and absolute day/period boundaries. Inspect calendar on a day, or calendars[date] on date lists and search results. Legacy days use rule=recorded_bounds with time_zone=null."
        },
        "identity_contract": {
            "screenshot": "screenshot_id",
            "ocr_row": "ocr_id",
            "cluster": "cluster_id"
        },
        "error_contract": {
            "shape": {"error": {"code": "STABLE_ERROR_CODE", "message": "Description"}},
            "mcp": "Tool failures are tools/call results with isError=true; malformed JSON-RPC requests remain protocol errors.",
            "codes": ["INVALID_ARGUMENT", "TOOL_NOT_FOUND", "SNAPSHOT_NOT_FOUND", "PRIVACY_REJECTED", "AUTH_REQUIRED", "MAINTENANCE_IN_PROGRESS", "TOOL_FAILED"],
            "domain_errors": "Existing RECAP_* and other domain error codes are preserved."
        },
        "skill": {
            "id": AGENT_SKILL_ID,
            "source_repository": AGENT_SKILL_SOURCE_REPOSITORY
        },
        "tools": tool_definitions()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tool_time_bounds_are_half_open_without_unit_inference() {
        let captured = 1_790_467_200_000_i64;
        let range = TimeRange::from_args(
            &json!({"start_ms": captured - 300_000, "end_ms": captured + 300_000}),
            true,
        )
        .unwrap();
        let (start, end) = range.storage_seconds();
        assert_eq!(start, Some(1_790_466_900.0));
        assert_eq!(end, Some(1_790_467_499.0));
        assert!(range.contains(Some(captured)));
        assert!(range.contains(range.start_ms));
        assert!(!range.contains(range.end_ms));
        assert!(!range.contains(None));
        assert!(TimeRange::default().contains(None));

        let early = TimeRange::from_args(&json!({"start_ms": 0, "end_ms": 1001}), true).unwrap();
        assert_eq!(early.storage_seconds(), (Some(0.0), Some(1.0)));
        let fractional_second =
            TimeRange::from_args(&json!({"start_ms": 1001, "end_ms": 2000}), true).unwrap();
        assert_eq!(fractional_second.storage_seconds(), (Some(2.0), Some(1.0)));
    }

    #[test]
    fn invalid_or_legacy_arguments_never_become_unfiltered_searches() {
        for args in [
            json!({"start_time": 1790467200, "end_time": 1790467300}),
            json!({"start_ms": 1000, "end_ms": 1000}),
            json!({"start_ms": 2000, "end_ms": 1000}),
            json!({"start_ms": -1, "end_ms": 1000}),
            json!({"start_ms": 0.5, "end_ms": 1000}),
            json!({"start_ms": "0", "end_ms": 1000}),
            json!({"start_ms": null, "end_ms": 1000}),
            json!({"start_ms": 0, "end_ms": 253_402_300_800_000_i64}),
            json!({"start_ms": 0}),
        ] {
            let error = validate_arguments("get_snapshots_by_time_range", &args).unwrap_err();
            assert_eq!(
                error_payload(&error)["error"]["code"],
                "INVALID_ARGUMENT",
                "{args}"
            );
        }
        for args in [
            json!({"query":"x", "start_time":1790467200}),
            json!({"query":"x", "limit":0}),
            json!({"query":"x", "fuzzy":"true"}),
            json!({"query":"x", "categories":[1]}),
            json!({"query":"x", "start_mss":0}),
        ] {
            assert!(
                validate_arguments("search_ocr_text", &args).is_err(),
                "{args}"
            );
        }
    }

    #[test]
    fn snapshot_lookup_requires_an_explicit_screenshot_id() {
        let error = validate_arguments(
            "search_ocr_text",
            &json!({"query":"x", "AUTH_REQUIRED":true}),
        )
        .unwrap_err();
        assert_eq!(error_payload(&error)["error"]["code"], "INVALID_ARGUMENT");
        for args in [
            json!({"id":42}),
            json!({"ocr_id":42}),
            json!({"screenshot_id":42,"id":42}),
            json!({"screenshot_id":"42"}),
            json!({"screenshot_id":0}),
        ] {
            assert!(
                validate_arguments("get_snapshot_details", &args).is_err(),
                "{args}"
            );
        }
        let args = json!({"screenshot_id":42});
        validate_arguments("get_snapshot_details", &args).unwrap();
        assert_eq!(screenshot_id(&args).unwrap(), 42);
    }

    #[test]
    fn catalog_names_are_unique_and_match_the_public_name_list() {
        let definitions = tool_definitions();
        let names: Vec<&str> = definitions
            .as_array()
            .expect("tool catalog must be an array")
            .iter()
            .map(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .expect("every tool needs a string name")
            })
            .collect();

        assert_eq!(names, TOOL_NAMES);
        assert_eq!(
            names.iter().copied().collect::<HashSet<_>>().len(),
            names.len()
        );
    }

    #[test]
    fn write_tools_are_catalog_members_and_excluded_from_the_read_only_view() {
        for name in WRITE_TOOL_NAMES {
            assert!(TOOL_NAMES.contains(name));
        }
        let read_only = read_only_tool_definitions();
        assert_eq!(read_only.len(), TOOL_NAMES.len() - WRITE_TOOL_NAMES.len());
        assert!(read_only.iter().all(|tool| {
            !is_write_tool(tool.get("name").and_then(Value::as_str).unwrap_or(""))
        }));
    }

    #[test]
    fn checked_in_contract_matches_the_runtime_catalog() {
        let checked_in: Value =
            serde_json::from_str(include_str!("../../docs/mcp-tool-contract-v3.json"))
                .expect("checked-in MCP contract must be valid JSON");
        assert_eq!(checked_in, contract_document());
    }
}
