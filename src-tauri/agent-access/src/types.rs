//! Explicit allowlists: no archive envelopes, icons, image locations or attachments.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SnapshotText {
    pub screenshot_id: i64,
    pub captured_at_ms: Option<i64>,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub source: Option<String>,
    pub page_url: Option<String>,
    pub category: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub visible_links: Vec<TextLink>,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextLink {
    pub text: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OcrText {
    pub ocr_id: i64,
    pub screenshot_id: i64,
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
    pub ocr_processed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClusterText {
    pub cluster_id: i64,
    pub display_name: Option<String>,
    pub anchor_text: String,
    pub enabled: bool,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub last_assigned_at_ms: Option<i64>,
    pub snapshot_count: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SummaryText {
    pub cluster_id: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub ocr_summary: Option<String>,
    pub key_points: Vec<String>,
    pub evidence: Vec<TextEvidence>,
    pub source_snapshot_count: Option<i64>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TextEvidence {
    pub screenshot_id: Option<i64>,
    pub captured_at_ms: Option<i64>,
    pub text: Option<String>,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssignmentText {
    pub cluster_id: i64,
    pub screenshot_id: i64,
    pub rerank_score: Option<f64>,
    pub assigned_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceText {
    #[serde(alias = "id")]
    pub screenshot_id: i64,
    #[serde(alias = "timestamp_ms")]
    pub captured_at_ms: i64,
    pub process_name: String,
    pub window_title: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActivityText {
    pub id: String,
    pub task_id: String,
    pub task_title: String,
    pub text: String,
    #[serde(default)]
    pub details: Vec<DetailText>,
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(default)]
    pub segments: Vec<String>,
    #[serde(default)]
    pub sources: Vec<SourceText>,
    #[serde(default)]
    pub member_ids: Vec<i64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DetailText {
    pub text: String,
    #[serde(default)]
    pub evidence_ids: Vec<i64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TopicText {
    pub task_id: String,
    #[serde(default)]
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub activity_ids: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskText {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub activity_ids: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecapContent {
    pub start_ms: i64,
    pub end_ms: i64,
    pub updated_at_ms: i64,
    pub status: String,
    pub activities: Vec<ActivityText>,
    pub records: Vec<SourceText>,
    pub overview: String,
    pub overview_activity_ids: Vec<String>,
    pub topics: Vec<TopicText>,
    pub names: BTreeMap<String, String>,
    pub merges: BTreeMap<String, String>,
    pub assignments: BTreeMap<i64, String>,
    pub threads: Vec<TaskText>,
    pub version: u32,
    pub model_revision: String,
    pub vectors: BTreeMap<String, Vec<f32>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecapText {
    pub date: String,
    pub key: String,
    pub part: String,
    pub revision: i64,
    pub content: RecapContent,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CalendarText {
    pub date: String,
    pub time_zone: Option<String>,
    pub rule: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub start_utc_offset_seconds: i32,
    pub end_utc_offset_seconds: i32,
    pub period_boundaries_ms: Vec<i64>,
    #[serde(default)]
    pub source_revision: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VectorText {
    pub kind: String,
    pub screenshot_id: i64,
    pub model_id: String,
    pub model_revision: String,
    pub vector: Vec<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", rename_all = "snake_case")]
pub enum TextRecord {
    Snapshot(SnapshotText),
    Ocr(OcrText),
    Cluster(ClusterText),
    Summary(SummaryText),
    Assignment(AssignmentText),
    Recap(RecapText),
    Calendar(CalendarText),
    Vector(VectorText),
    Privacy(crate::sensitive_filter::SensitiveFilterConfig),
}
impl TextRecord {
    pub fn identity(&self) -> (&'static str, String) {
        match self {
            Self::Snapshot(v) => ("snapshot", v.screenshot_id.to_string()),
            Self::Ocr(v) => ("ocr", v.ocr_id.to_string()),
            Self::Cluster(v) => ("cluster", v.cluster_id.to_string()),
            Self::Summary(v) => ("summary", v.cluster_id.to_string()),
            Self::Assignment(v) => (
                "assignment",
                format!("{}:{}", v.cluster_id, v.screenshot_id),
            ),
            Self::Recap(v) => ("recap", format!("{}:{}", v.part, v.key)),
            Self::Calendar(v) => ("calendar", v.date.clone()),
            Self::Vector(v) => ("vector", format!("{}:{}", v.kind, v.screenshot_id)),
            Self::Privacy(_) => ("privacy", "settings".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum TextChange {
    Put {
        record: TextRecord,
    },
    Delete {
        kind: String,
        key: String,
    },
    SnapshotState {
        screenshot_id: i64,
        category: Option<String>,
        status: String,
        deleted: bool,
    },
    Gap {
        kind: String,
        key: String,
    },
    DeleteDay {
        date: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportEvent {
    pub dataset_id: String,
    pub generation: String,
    pub revision: i64,
    pub change: TextChange,
}
