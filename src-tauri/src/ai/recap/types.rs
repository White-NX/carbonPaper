use std::collections::BTreeMap;

use chrono::{Local, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecapSettings {
    pub enabled: bool,
    pub enabled_since: Option<String>,
    pub provider_id: Option<String>,
    pub language: String,
    pub batch_input_tokens: u64,
    pub batch_output_tokens: u64,
    pub request_output_tokens: u32,
    pub answer_tokens: u32,
    #[serde(default)]
    pub budget_version: u32,
    pub daily_input_tokens: u64,
    pub daily_output_tokens: u64,
    pub screening_input_tokens: u64,
    pub screening: ScreeningSettings,
}

impl Default for RecapSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            enabled_since: None,
            provider_id: None,
            language: "zh-CN".into(),
            batch_input_tokens: 36_000,
            batch_output_tokens: 64_000,
            request_output_tokens: 32_000,
            answer_tokens: 4_000,
            budget_version: 1,
            daily_input_tokens: 432_000,
            daily_output_tokens: 384_000,
            screening_input_tokens: 2_000_000,
            screening: ScreeningSettings::default(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreeningSettings {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    /// Only the encrypted settings payload contains this field. Views clear it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub has_api_key: bool,
}

impl std::fmt::Debug for ScreeningSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreeningSettings")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

impl Default for ScreeningSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.typesafe.ai/v1".into(),
            model: "jev-1.13.0".into(),
            api_key: None,
            has_api_key: false,
        }
    }
}

impl RecapSettings {
    /// Upgrade only the complete, untouched legacy budget tuple. Explicitly
    /// customized limits remain authoritative, including small output budgets.
    pub fn current_defaults(mut self) -> Self {
        if self.budget_version == 0 {
            if (
                self.batch_input_tokens,
                self.batch_output_tokens,
                self.daily_input_tokens,
                self.daily_output_tokens,
            ) == (36_000, 6_000, 216_000, 36_000)
            {
                let defaults = Self::default();
                self.batch_output_tokens = defaults.batch_output_tokens;
                self.daily_input_tokens = defaults.daily_input_tokens;
                self.daily_output_tokens = defaults.daily_output_tokens;
            }
            self.budget_version = 1;
        }
        self
    }

    pub fn view(mut self) -> Self {
        self = self.current_defaults();
        self.screening.has_api_key = self
            .screening
            .api_key
            .as_ref()
            .is_some_and(|s| !s.is_empty());
        self.screening.api_key = None;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(4_000..=1_000_000).contains(&self.batch_input_tokens)
            || !(1_000..=64_000).contains(&self.batch_output_tokens)
            || !(1_000..=64_000).contains(&self.request_output_tokens)
            || !(512..=16_000).contains(&self.answer_tokens)
            || self.daily_input_tokens < self.batch_input_tokens
            || self.daily_output_tokens < self.batch_output_tokens
            || self.daily_input_tokens > 10_000_000
            || self.daily_output_tokens > 1_000_000
            || !(1_000..=20_000_000).contains(&self.screening_input_tokens)
            || self.language.len() > 32
        {
            return Err("RECAP_INVALID_SETTINGS".into());
        }
        let s = &self.screening;
        if s.enabled
            && (s.model.trim().is_empty()
                || s.model.len() > 200
                || s.base_url.len() > 512
                || !(s.base_url.starts_with("https://") || s.base_url.starts_with("http://"))
                || s.api_key.as_ref().is_some_and(|k| k.len() > 2048))
        {
            return Err("RECAP_INVALID_SETTINGS".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub id: i64,
    pub timestamp_ms: i64,
    pub process_name: String,
    pub window_title: String,
    pub page_url: String,
    pub text: String,
    pub segment: String,
    pub context: String,
    pub novelty: f64,
    pub span_ms: i64,
    #[serde(default)]
    pub screening: ScreeningSignal,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScreeningSignal {
    pub category: String,
    pub confidence: f64,
    pub result: bool,
    #[serde(default)]
    pub related_previous: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceRef {
    pub id: i64,
    pub timestamp_ms: i64,
    pub process_name: String,
    pub window_title: String,
}
impl From<&Evidence> for SourceRef {
    fn from(e: &Evidence) -> Self {
        Self {
            id: e.id,
            timestamp_ms: e.timestamp_ms,
            process_name: e.process_name.clone(),
            window_title: e.window_title.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecapActivity {
    pub id: String,
    pub task_id: String,
    pub task_title: String,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub segments: Vec<String>,
    pub sources: Vec<SourceRef>,
    #[serde(default)]
    pub member_ids: Vec<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecapBatch {
    pub start_ms: i64,
    pub end_ms: i64,
    pub updated_at_ms: i64,
    pub status: String,
    pub activities: Vec<RecapActivity>,
    /// Local directory also retains material that did not fit the model request.
    pub records: Vec<SourceRef>,
    pub coverage: usize,
    pub error: Option<String>,
    #[serde(default)]
    pub attempts: Vec<RecapAttempt>,
}

/// Bounded previews are private recap data and are persisted only inside the
/// encrypted batch payload. Progress notifications never carry this content.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecapAttempt {
    pub id: String,
    pub batch_start_ms: i64,
    pub kind: String,
    pub model: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: String,
    pub reasoning: String,
    pub text: String,
    pub reasoning_chars: usize,
    pub text_chars: usize,
    pub max_output_tokens: u32,
    pub input_estimate: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub truncated: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RecapProgress {
    pub run_id: String,
    pub date: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub version: u64,
    pub stage: String,
    pub total_batches: usize,
    pub completed_batches: usize,
    pub batch_start_ms: Option<i64>,
    pub attempts: Vec<RecapAttempt>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskThread {
    pub id: String,
    pub title: String,
    pub activity_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Corrections {
    pub names: BTreeMap<String, String>,
    /// Corrections follow evidence IDs across regenerated activity boundaries.
    pub assignments: BTreeMap<i64, String>,
    pub merges: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CorrectionHistory {
    pub current: Corrections,
    pub undo: Vec<Corrections>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Correction {
    Rename {
        task_id: String,
        title: String,
    },
    Merge {
        from: String,
        into: String,
    },
    Move {
        activity_id: String,
        task_id: Option<String>,
        title: Option<String>,
    },
    Undo,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecapUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub screening_input_tokens: u64,
    pub requests: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecapDay {
    pub date: String,
    pub batches: Vec<RecapBatch>,
    pub threads: Vec<TaskThread>,
    pub usage: RecapUsage,
    pub can_undo: bool,
    pub running: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DraftActivity {
    pub task_id: Option<String>,
    pub task_title: String,
    pub text: String,
    pub evidence_ids: Vec<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Gap {
    pub question: String,
    pub evidence_ids: Vec<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Draft {
    pub activities: Vec<DraftActivity>,
    #[serde(default)]
    pub gaps: Vec<Gap>,
}

pub fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}
pub fn limited(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub fn day_bounds(date: &str) -> Result<(i64, i64), String> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "RECAP_INVALID_DATE")?;
    if day.to_string() != date {
        return Err("RECAP_INVALID_DATE".into());
    }
    let next = day.succ_opt().ok_or("RECAP_INVALID_DATE")?;
    let midnight = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.timestamp_millis())
            .ok_or("RECAP_INVALID_DATE".to_string())
    };
    Ok((midnight(day)?, midnight(next)?))
}

pub fn batch_bounds(start: i64, end: i64, now: i64) -> Vec<(i64, i64)> {
    // Calendar boundaries survive DST; a day's UTC length need not be 24 h.
    let mut boundaries = vec![start];
    if let Some(t) = Local.timestamp_millis_opt(start).single() {
        for hour in [4, 8, 12, 16, 20] {
            if let Some(t) = t.with_hour(hour) {
                boundaries.push(t.timestamp_millis());
            }
        }
    }
    boundaries.push(end);
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .windows(2)
        .filter(|w| w[1] <= now)
        .map(|w| (w[0], w[1]))
        .collect()
}

pub fn apply_corrections(batches: &mut [RecapBatch], corrections: &Corrections) -> Vec<TaskThread> {
    let resolve = |id: &str| {
        let mut current = id.to_string();
        for _ in 0..corrections.merges.len() {
            match corrections.merges.get(&current) {
                Some(next) => current = next.clone(),
                None => break,
            }
        }
        current
    };
    let mut threads: BTreeMap<String, TaskThread> = BTreeMap::new();
    for batch in batches {
        let original = std::mem::take(&mut batch.activities);
        for activity in original {
            let mut groups = BTreeMap::<String, Vec<SourceRef>>::new();
            for source in &activity.sources {
                let task = resolve(
                    corrections
                        .assignments
                        .get(&source.id)
                        .map(String::as_str)
                        .unwrap_or(&activity.task_id),
                );
                groups.entry(task).or_default().push(source.clone());
            }
            let split = groups.len() > 1;
            for (task, sources) in groups {
                let mut a = activity.clone();
                a.task_id = task;
                if split {
                    a.id = format!("{}-{}", a.id, &digest(&a.task_id)[..8]);
                }
                if let Some(title) = corrections.names.get(&a.task_id) {
                    a.task_title = title.clone();
                }
                a.start_ms = sources
                    .iter()
                    .map(|s| s.timestamp_ms)
                    .min()
                    .unwrap_or(a.start_ms);
                a.end_ms = sources
                    .iter()
                    .map(|s| s.timestamp_ms)
                    .max()
                    .unwrap_or(a.end_ms);
                a.sources = sources;
                if split {
                    a.member_ids.retain(|id| {
                        resolve(
                            corrections
                                .assignments
                                .get(id)
                                .map(String::as_str)
                                .unwrap_or(&activity.task_id),
                        ) == a.task_id
                    });
                }
                let t = threads
                    .entry(a.task_id.clone())
                    .or_insert_with(|| TaskThread {
                        id: a.task_id.clone(),
                        title: a.task_title.clone(),
                        activity_ids: vec![],
                    });
                t.activity_ids.push(a.id.clone());
                batch.activities.push(a);
            }
        }
        batch.activities.sort_by_key(|a| a.start_ms);
    }
    threads.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn batch() -> RecapBatch {
        RecapBatch {
            start_ms: 0,
            end_ms: 100,
            updated_at_ms: 0,
            status: "ready".into(),
            records: vec![],
            coverage: 2,
            error: None,
            attempts: vec![],
            activities: vec![RecapActivity {
                id: "regenerated".into(),
                task_id: "new-model-id".into(),
                task_title: "Generated name".into(),
                text: "Observed activity".into(),
                start_ms: 10,
                end_ms: 20,
                segments: vec!["s".into()],
                member_ids: vec![1, 2, 3],
                sources: vec![
                    SourceRef {
                        id: 1,
                        timestamp_ms: 10,
                        process_name: String::new(),
                        window_title: String::new(),
                    },
                    SourceRef {
                        id: 2,
                        timestamp_ms: 20,
                        process_name: String::new(),
                        window_title: String::new(),
                    },
                ],
            }],
        }
    }
    #[test]
    fn regenerated_groups_preserve_conflicting_user_assignments_without_majority_overwrite() {
        let mut correction = Corrections::default();
        correction.assignments.insert(1, "task-a".into());
        correction.assignments.insert(2, "task-b".into());
        correction.names.insert("task-a".into(), "My title".into());
        let mut batches = vec![batch()];
        let threads = apply_corrections(&mut batches, &correction);
        assert_eq!(threads.len(), 2);
        assert_eq!(batches[0].activities.len(), 2);
        assert_eq!(batches[0].activities[0].task_title, "My title");
        assert_eq!(batches[0].activities[1].sources[0].id, 2);
        assert_eq!(batches[0].activities[0].member_ids, vec![1]);
    }
    #[test]
    fn defaults_are_opt_in_and_views_and_debug_omit_secrets() {
        let mut s = RecapSettings::default();
        assert!(!s.enabled);
        assert!(!s.screening.enabled);
        s.validate().unwrap();
        s.screening.api_key = Some("private-test-key".into());
        assert!(!format!("{s:?}").contains("private-test-key"));
        let view = s.view();
        assert!(view.screening.has_api_key);
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("private-test-key"));
    }
    #[test]
    fn legacy_default_budgets_upgrade_but_custom_and_current_budgets_survive() {
        let old = serde_json::json!({
            "enabled": true, "batch_input_tokens": 36000, "batch_output_tokens": 6000,
            "daily_input_tokens": 216000, "daily_output_tokens": 36000
        });
        let settings: RecapSettings = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(settings.budget_version, 0);
        let upgraded = settings.current_defaults();
        assert!(upgraded.enabled);
        assert_eq!(upgraded.batch_output_tokens, 64000);
        assert_eq!(upgraded.request_output_tokens, 32000);
        assert_eq!(
            upgraded.daily_input_tokens + upgraded.daily_output_tokens,
            816000
        );
        upgraded.validate().unwrap();
        let mut custom = old.clone();
        custom["daily_output_tokens"] = serde_json::json!(42000);
        let custom: RecapSettings = serde_json::from_value(custom).unwrap();
        let custom = custom.current_defaults();
        assert_eq!(custom.daily_output_tokens, 42000);
        assert_eq!(custom.batch_output_tokens, 6000);
        let mut intentional = old;
        intentional["budget_version"] = serde_json::json!(1);
        let intentional: RecapSettings = serde_json::from_value(intentional).unwrap();
        assert_eq!(intentional.current_defaults().batch_output_tokens, 6000);
    }
    #[test]
    fn four_hour_windows_are_half_open_and_only_closed_windows_are_returned() {
        let (start, end) = day_bounds("2026-01-01").unwrap();
        let windows = batch_bounds(start, end, end);
        assert_eq!(windows.len(), 6);
        assert_eq!(windows[0].0, start);
        assert_eq!(windows.last().unwrap().1, end);
        assert!(windows.windows(2).all(|w| w[0].1 == w[1].0));
        assert_eq!(batch_bounds(start, end, windows[0].1 - 1).len(), 0);
        assert_eq!(batch_bounds(start, end, windows[0].1).len(), 1);
        assert!(day_bounds("2026-02-30").is_err());
    }
}
