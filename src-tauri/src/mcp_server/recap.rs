//! Read-only recap tools shared by MCP clients and interactive AI search.
use super::{filter_identity, filter_joined_text, require_authenticated_session};
use crate::ai::recap::{self, RecapDay};
use crate::sensitive_filter::SensitiveFilterState;
use crate::storage::StorageState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::Manager;

#[derive(Default, Deserialize)]
struct Page {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

impl Page {
    fn limit(&self, default: usize, max: usize) -> Result<usize, String> {
        let limit = self.limit.unwrap_or(default);
        if limit == 0 || limit > max {
            return Err(format!("limit must be between 1 and {max}"));
        }
        Ok(limit)
    }
}

#[derive(Deserialize)]
struct DaysQuery {
    start_date: Option<String>,
    end_date: Option<String>,
    #[serde(flatten)]
    page: Page,
}

impl DaysQuery {
    fn validate(&self) -> Result<usize, String> {
        for date in [self.start_date.as_deref(), self.end_date.as_deref()]
            .into_iter()
            .flatten()
        {
            recap::day_bounds(date)?;
        }
        if matches!((&self.start_date, &self.end_date), (Some(a), Some(b)) if a > b) {
            return Err("start_date must not be after end_date".into());
        }
        self.page.limit(30, 100)
    }
}

#[derive(Deserialize)]
struct DayQuery {
    date: String,
    batch_start_ms: Option<i64>,
    #[serde(flatten)]
    page: Page,
}

fn page_result(items: Vec<Value>, offset: usize, limit: usize) -> Value {
    let total = items.len();
    let end = offset.saturating_add(limit).min(total);
    json!({
        "items": items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),
        "total": total,
        "offset": offset,
        "limit": limit,
        "next_offset": (end < total).then_some(end),
    })
}

fn dates_result(mut days: Vec<String>, query: &DaysQuery, limit: usize) -> Value {
    days.retain(|day| {
        query.start_date.as_ref().is_none_or(|start| day >= start)
            && query.end_date.as_ref().is_none_or(|end| day <= end)
    });
    days.sort_unstable_by(|a, b| b.cmp(a));
    page_result(
        days.into_iter().map(Value::String).collect(),
        query.page.offset,
        limit,
    )
}

pub(super) async fn tool_get_recap_days(
    app_handle: &tauri::AppHandle,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(app_handle)?;
    let query: DaysQuery = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let limit = query.validate()?;
    let app = app_handle.clone();
    tokio::task::spawn_blocking(move || {
        require_authenticated_session(&app)?;
        if crate::maintenance::is_active() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let storage = app.state::<Arc<StorageState>>();
        let generation = storage.db_generation();
        let result = dates_result(storage.recap_list_days()?, &query, limit);
        require_authenticated_session(&app)?;
        if generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(result)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Explicit projection prevents prompts, reasoning, record directories and
/// provider diagnostics from becoming model inputs. Refilter generated text
/// and user corrections as well as source identities before returning them.
fn day_result(
    day: RecapDay,
    query: &DayQuery,
    filter: &SensitiveFilterState,
) -> Result<Value, String> {
    let limit = query.page.limit(10, 50)?;
    if query
        .batch_start_ms
        .is_some_and(|start| !day.batches.iter().any(|b| b.start_ms == start))
    {
        return Err("Unknown recap period".into());
    }
    let mode = filter.mode();
    let mut periods = Vec::new();
    let mut items = Vec::new();
    for batch in day
        .batches
        .into_iter()
        .filter(|b| query.batch_start_ms.is_none_or(|start| b.start_ms == start))
    {
        let overview = batch
            .summary
            .and_then(|s| filter_joined_text(filter, mode, &s.overview).ok());
        periods.push(json!({
            "start_ms": batch.start_ms, "end_ms": batch.end_ms,
            "status": batch.status, "overview": overview,
        }));
        for activity in batch.activities {
            let clean =
                (|| {
                    let title = filter_identity(filter, mode, &activity.task_title)?;
                    let text = filter_joined_text(filter, mode, &activity.text)?;
                    let sources = activity.sources.iter().map(|source| {
                    Ok(json!({
                        "screenshot_id": source.id,
                        "timestamp": source.timestamp_ms,
                        "process_name": filter_identity(filter, mode, &source.process_name)?,
                        "window_title": filter_identity(filter, mode, &source.window_title)?,
                    }))
                }).collect::<Result<Vec<_>, super::Rejected>>()?;
                    Ok::<_, super::Rejected>(json!({
                        "activity_id": activity.id, "task_id": activity.task_id,
                        "task_title": title, "text": text,
                        "start_ms": activity.start_ms, "end_ms": activity.end_ms,
                        "batch_start_ms": batch.start_ms, "sources": sources,
                    }))
                })();
            if let Ok(item) = clean {
                items.push(item);
            }
        }
    }
    items.sort_by_key(|item| item["start_ms"].as_i64().unwrap_or_default());
    let mut result = page_result(items, query.page.offset, limit);
    result["date"] = day.date.into();
    result["running"] = day.running.into();
    result["periods"] = periods.into();
    Ok(result)
}

pub(super) async fn tool_get_recap_day(
    app_handle: &tauri::AppHandle,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(app_handle)?;
    let query: DayQuery = serde_json::from_value(args).map_err(|e| e.to_string())?;
    recap::day_bounds(&query.date)?;
    query.page.limit(10, 50)?;
    let app = app_handle.clone();
    tokio::task::spawn_blocking(move || {
        let filter = app.state::<Arc<SensitiveFilterState>>();
        recap::with_current_day(&app, &query.date, |day| day_result(day, &query, &filter))
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::recap::{RecapActivity, RecapAttempt, RecapBatch, RecapSummary, SourceRef};

    fn fixture() -> RecapDay {
        let source = SourceRef {
            id: 42,
            timestamp_ms: 1_790_784_000_000,
            process_name: "editor.exe".into(),
            window_title: "Release notes".into(),
        };
        RecapDay {
            date: "2026-10-01".into(),
            threads: vec![],
            usage: Default::default(),
            can_undo: false,
            running: false,
            error: Some("provider diagnostics".into()),
            batches: vec![RecapBatch {
                start_ms: source.timestamp_ms,
                end_ms: source.timestamp_ms + 1000,
                updated_at_ms: 0,
                status: "complete".into(),
                coverage: 1,
                error: None,
                summary_error: None,
                records: vec![source.clone()],
                attempts: vec![RecapAttempt {
                    reasoning: "private reasoning".into(),
                    ..Default::default()
                }],
                summary: Some(RecapSummary {
                    overview: "Read the notes".into(),
                    overview_activity_ids: vec![],
                    topics: vec![],
                }),
                activities: vec![RecapActivity {
                    id: "activity-1".into(),
                    task_id: "task-1".into(),
                    task_title: "Corrected title".into(),
                    text: "Read the notes".into(),
                    start_ms: source.timestamp_ms,
                    end_ms: source.timestamp_ms,
                    sources: vec![source],
                    segments: vec![],
                    member_ids: vec![42],
                }],
            }],
        }
    }

    fn query() -> DayQuery {
        serde_json::from_value(json!({"date": "2026-10-01"})).unwrap()
    }
    fn filter(mode: &str) -> SensitiveFilterState {
        let filter = SensitiveFilterState::with_test_words(&["private"]);
        let mut config = filter.get_config();
        config.mode = mode.into();
        filter.update_config(config);
        filter
    }

    #[test]
    fn recap_projection_preserves_corrected_content_and_citable_evidence_only() {
        let result = day_result(fixture(), &query(), &filter("mask")).unwrap();
        assert_eq!(result["items"][0]["task_title"], "Corrected title");
        assert_eq!(result["items"][0]["sources"][0]["screenshot_id"], 42);
        assert_eq!(
            result["items"][0]["sources"][0]["timestamp"],
            1_790_784_000_000_i64
        );
        assert_eq!(result["periods"][0]["overview"], "Read the notes");
        for field in [
            "reasoning",
            "attempts",
            "records",
            "usage",
            "provider diagnostics",
            "member_ids",
        ] {
            assert!(!result.to_string().contains(field));
        }
    }

    #[test]
    fn recap_filters_generated_text_corrections_and_source_identities() {
        for field in ["title", "text", "process", "window"] {
            let mut day = fixture();
            let activity = &mut day.batches[0].activities[0];
            match field {
                "title" => activity.task_title = "private title".into(),
                "text" => activity.text = "private text".into(),
                "process" => activity.sources[0].process_name = "private.exe".into(),
                _ => activity.sources[0].window_title = "private window".into(),
            }
            day.batches[0].summary.as_mut().unwrap().overview = "private overview".into();
            for mode in ["mask", "remove_paragraph", "reject"] {
                let result = day_result(day.clone(), &query(), &filter(mode)).unwrap();
                assert!(!result.to_string().contains("private"), "{field}/{mode}");
                if mode == "reject" {
                    assert_eq!(result["total"], 0);
                }
            }
        }
    }

    #[test]
    fn recap_paginates_filtered_activities_and_validates_periods() {
        let mut day = fixture();
        let mut second = day.batches[0].activities[0].clone();
        second.id = "activity-2".into();
        second.start_ms += 1;
        day.batches[0].activities.insert(0, second);
        let mut query = query();
        query.page.limit = Some(1);
        let first = day_result(day.clone(), &query, &filter("mask")).unwrap();
        assert_eq!(first["items"][0]["activity_id"], "activity-1");
        assert_eq!(first["next_offset"], 1);
        query.page.offset = 1;
        let last = day_result(day.clone(), &query, &filter("mask")).unwrap();
        assert_eq!(last["items"][0]["activity_id"], "activity-2");
        assert!(last["next_offset"].is_null());
        query.batch_start_ms = Some(1);
        assert!(day_result(day, &query, &filter("mask")).is_err());
    }

    #[test]
    fn recap_dates_are_inclusive_descending_and_bounded() {
        let query: DaysQuery = serde_json::from_value(
            json!({"start_date":"2026-10-01", "end_date":"2026-10-02", "limit":1}),
        )
        .unwrap();
        let limit = query.validate().unwrap();
        let result = dates_result(
            vec![
                "2026-10-01".into(),
                "2026-10-03".into(),
                "2026-10-02".into(),
            ],
            &query,
            limit,
        );
        assert_eq!(result["items"], json!(["2026-10-02"]));
        assert_eq!(result["total"], 2);
        assert_eq!(result["next_offset"], 1);
        for args in [
            json!({"start_date":"2026-02-30"}),
            json!({"start_date":"2026-10-02","end_date":"2026-10-01"}),
            json!({"limit":0}),
            json!({"limit":101}),
        ] {
            assert!(serde_json::from_value::<DaysQuery>(args)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(serde_json::from_value::<DaysQuery>(json!({"offset":-1})).is_err());
        assert!(serde_json::from_value::<DayQuery>(json!({})).is_err());
    }
}
