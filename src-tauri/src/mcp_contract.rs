//! Versioned MCP and Agent Skill contract shared by runtime status and `tools/list`.

use serde_json::{json, Value};

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const TOOL_SCHEMA_VERSION: u64 = 2;
pub const AGENT_SKILL_ID: &str = "carbonpaper-memory";
pub const AGENT_SKILL_SOURCE_REPOSITORY: &str = "https://github.com/White-NX/carbonPaperSkill";

pub const TOOL_NAMES: &[&str] = &[
    "get_snapshots_by_time_range",
    "get_snapshot_details",
    "search_ocr_text",
    "search_nl",
    "get_task_clusters",
    "get_task_screenshots",
    "rename_task",
    "get_smart_clusters",
    "get_smart_cluster_ocr_corpus",
    "get_smart_cluster_summary",
    "upsert_smart_cluster_summary",
    "delete_smart_cluster_summary",
];

pub fn tool_definitions() -> Value {
    json!([
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
            "description": "Natural language visual search over screenshots: a text query matched against what each screenshot looks like, using Chinese-CLIP image embeddings. Complements search_ocr_text, which matches the literal text on screen.",
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
        },
        {
            "name": "get_smart_clusters",
            "description": "List smart clusters with assignment counts and any stored AI-generated summary.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "get_smart_cluster_ocr_corpus",
            "description": "Get assigned smart-cluster snapshots with joined OCR text for AI summarization. Results are paginated and ordered by rerank score.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cluster_id": { "type": "integer", "description": "Smart cluster ID" },
                    "page": { "type": "integer", "description": "Page number (0-based, default 0)" },
                    "page_size": { "type": "integer", "description": "Page size (default 50, max 200)" },
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
                    "source_snapshot_count": { "type": "integer", "description": "Number of source snapshots used" },
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
    ])
}

pub fn tools_list_result() -> Value {
    json!({ "tools": tool_definitions() })
}

pub fn contract_document() -> Value {
    json!({
        "tool_schema_version": TOOL_SCHEMA_VERSION,
        "mcp_protocol_version": MCP_PROTOCOL_VERSION,
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
    fn checked_in_contract_matches_the_runtime_catalog() {
        let checked_in: Value =
            serde_json::from_str(include_str!("../../docs/mcp-tool-contract-v2.json"))
                .expect("checked-in MCP contract must be valid JSON");
        assert_eq!(checked_in, contract_document());
    }
}
