//! Read-only recap tools shared by MCP clients and interactive AI search.
use super::{filter_identity, filter_joined_text, require_authenticated_session};
use crate::ai::recap::{self, RecapDay, RecapSearchResult};
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
        if self.offset > 100_000 {
            return Err(crate::mcp_contract::invalid_argument(
                "offset must not exceed 100000",
            ));
        }
        let limit = self.limit.unwrap_or(default);
        if limit == 0 || limit > max {
            return Err(crate::mcp_contract::invalid_argument(format!(
                "limit must be between 1 and {max}"
            )));
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
            crate::calendar::parse_date(date)?;
        }
        if matches!((&self.start_date, &self.end_date), (Some(a), Some(b)) if a > b) {
            return Err(crate::mcp_contract::invalid_argument(
                "start_date must not be after end_date",
            ));
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
    super::page::Page { offset, limit }.complete(items)
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
    let query: DaysQuery =
        serde_json::from_value(args).map_err(crate::mcp_contract::invalid_argument)?;
    let limit = query.validate()?;
    let app = app_handle.clone();
    tokio::task::spawn_blocking(move || {
        require_authenticated_session(&app)?;
        if crate::maintenance::is_active() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let storage = app.state::<Arc<StorageState>>();
        let generation = storage.db_generation();
        let mut result = dates_result(storage.recap_list_days()?, &query, limit);
        let mut calendars = serde_json::Map::new();
        for date in result["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            calendars.insert(date.into(), json!(storage.recap_calendar(date)?));
        }
        result["calendars"] = calendars.into();
        require_authenticated_session(&app)?;
        if generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(result)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

pub(super) async fn tool_search_recaps(
    app_handle: &tauri::AppHandle,
    args: Value,
) -> Result<Value, String> {
    require_authenticated_session(app_handle)?;
    let query: recap::RecapSearchQuery =
        serde_json::from_value(args).map_err(crate::mcp_contract::invalid_argument)?;
    let storage = app_handle.state::<Arc<StorageState>>().inner().clone();
    let generation = storage.db_generation();
    let result = recap::search_activities(app_handle.clone(), query)
        .await
        .map_err(search_error)?;
    let app = app_handle.clone();
    tokio::task::spawn_blocking(move || {
        let mut calendars = serde_json::Map::new();
        for hit in &result.items {
            if !calendars.contains_key(&hit.date) {
                calendars.insert(hit.date.clone(), json!(storage.recap_calendar(&hit.date)?));
            }
        }
        let mut result = search_result(result)?;
        result["calendars"] = calendars.into();
        require_authenticated_session(&app)?;
        if generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(result)
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

fn search_error(error: String) -> String {
    match error.split_once(':') {
        Some((code, _))
            if matches!(
                code,
                "RECAP_SEARCH_MODEL_UNAVAILABLE" | "RECAP_WORKER_FAILED"
            ) =>
        {
            code.into()
        }
        _ => error,
    }
}

fn search_result(mut result: RecapSearchResult) -> Result<Value, String> {
    // Runtime paths and raw worker diagnostics stay in the local UI and logs,
    // rather than becoming model inputs through MCP or interactive AI search.
    result.semantic_error = None;
    let returned_count = result.items.len();
    let mut result = serde_json::to_value(result).map_err(|_| "RECAP_INVALID_CACHE")?;
    result["returned_count"] = returned_count.into();
    super::page::bound_continuation(&mut result);
    // Keep the established MCP screenshot citation and Unix-millisecond contract.
    if let Some(items) = result["items"].as_array_mut() {
        for item in items {
            if let Some(sources) = item["sources"].as_array_mut() {
                for source in sources {
                    if let Some(source) = source.as_object_mut() {
                        if let Some(id) = source.remove("id") {
                            source.insert("screenshot_id".into(), id);
                        }
                        if let Some(time) = source.remove("timestamp_ms") {
                            source.insert("captured_at_ms".into(), time);
                        }
                    }
                }
            }
        }
    }
    Ok(result)
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
        return Err(crate::mcp_contract::invalid_argument(
            "Unknown recap period",
        ));
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
        let member_directory =
            crate::ai::recap::activity_contract::member_directory(&batch.records, filter);
        for activity in batch.activities {
            let clean = (|| {
                let title = filter_identity(filter, mode, &activity.task_title)?;
                let text = filter_joined_text(filter, mode, &activity.text)?;
                let details = activity
                    .details
                    .iter()
                    .filter_map(|detail| {
                        if detail.evidence_ids.is_empty()
                            || !detail
                                .evidence_ids
                                .iter()
                                .all(|id| activity.sources.iter().any(|s| s.id == *id))
                        {
                            return None;
                        }
                        filter_joined_text(filter, mode, &detail.text)
                            .ok()
                            .map(|text| json!({"text":text,"evidence_ids":detail.evidence_ids}))
                    })
                    .collect::<Vec<_>>();
                let sources = activity
                    .sources
                    .iter()
                    .map(|source| {
                        let mut source = source.clone();
                        source.process_name = filter_identity(filter, mode, &source.process_name)?;
                        source.window_title = filter_identity(filter, mode, &source.window_title)?;
                        Ok(super::wire::recap_source(&source))
                    })
                    .collect::<Result<Vec<_>, super::Rejected>>()?;
                let evidence = crate::ai::recap::activity_contract::ActivityEvidence::project(
                    &activity,
                    &member_directory,
                );
                Ok::<_, super::Rejected>(json!({
                    "activity_id": activity.id, "task_id": activity.task_id,
                    "task_title": title, "text": text, "details": details,
                    "start_ms": activity.start_ms, "end_ms": activity.end_ms,
                    "time_semantics": evidence.time_semantics,
                    "observation_kind": evidence.observation_kind,
                    "span_ms": evidence.observed_span_ms,
                    "member_ids": evidence.member_screenshot_ids,
                    "membership_status": evidence.membership_status,
                    "batch_start_ms": batch.start_ms, "sources": sources,
                }))
            })();
            if let Ok(item) = clean {
                items.push(item);
            }
        }
    }
    items.sort_by(|a, b| {
        a["start_ms"]
            .as_i64()
            .cmp(&b["start_ms"].as_i64())
            .then_with(|| a["activity_id"].as_str().cmp(&b["activity_id"].as_str()))
    });
    let mut result = page_result(items, query.page.offset, limit);
    result["date"] = day.date.into();
    result["calendar"] = json!(day.calendar);
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
            calendar: crate::calendar::CalendarDay::in_zone(
                "2026-10-01",
                chrono_tz::Asia::Shanghai,
            )
            .unwrap(),
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
                    details: vec![],
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
            result["items"][0]["sources"][0]["captured_at_ms"],
            1_790_784_000_000_i64
        );
        assert_eq!(result["periods"][0]["overview"], "Read the notes");
        assert_eq!(result["calendar"]["time_zone"], "Asia/Shanghai");
        for field in [
            "reasoning",
            "attempts",
            "records",
            "usage",
            "provider diagnostics",
        ] {
            assert!(!result.to_string().contains(field));
        }
        assert_eq!(result["items"][0]["member_ids"], json!([42]));
        assert_eq!(result["items"][0]["membership_status"], "complete");
        assert_eq!(
            result["items"][0]["time_semantics"],
            "evidence_observation_span"
        );
        assert_eq!(result["items"][0]["span_ms"], 0);
        assert_eq!(result["items"][0]["observation_kind"], "instant");
    }

    #[test]
    fn search_projection_omits_local_diagnostics_and_preserves_fallback_status() {
        let diagnostic = "RECAP_SEARCH_MODEL_UNAVAILABLE: model_missing: private runtime path";
        let capture: Value = serde_json::from_str(include_str!(
            "../../../src/tests/fixtures/screenshot_contract_v3.json"
        ))
        .unwrap();
        let source = SourceRef {
            id: capture["screenshot_id"].as_i64().unwrap(),
            timestamp_ms: capture["captured_at_ms"].as_i64().unwrap(),
            process_name: "editor.exe".into(),
            window_title: "Save location".into(),
        };
        let result = search_result(RecapSearchResult {
            revision: "source".into(),
            ranking_revision: "ranking".into(),
            items: vec![recap::search::RecapSearchHit {
                date: capture["local_date"].as_str().unwrap().into(),
                batch_start_ms: source.timestamp_ms,
                activity_id: "activity".into(),
                task_id: "task".into(),
                task_title: "Save location".into(),
                text: "Change the save location".into(),
                details: vec![],
                start_ms: source.timestamp_ms,
                end_ms: source.timestamp_ms,
                evidence: crate::ai::recap::activity_contract::ActivityEvidence {
                    time_semantics: "evidence_observation_span",
                    observation_kind: "instant",
                    observed_span_ms: 0,
                    member_screenshot_ids: vec![source.id, source.id + 1],
                    membership_status: "complete",
                },
                sources: vec![source],
                keyword_rank: Some(1),
                semantic_rank: None,
            }],
            total: 1,
            offset: 0,
            limit: 20,
            next_offset: None,
            mode: recap::RecapSearchQuery::default().mode,
            semantic_status: "unavailable".into(),
            semantic_error: Some(diagnostic.into()),
            indexed_chunks: 1,
            total_chunks: 1,
            coverage: Default::default(),
            execution: Default::default(),
            index_progress: Default::default(),
            elapsed_ms: 5000,
        })
        .unwrap();
        assert_eq!(result["semantic_status"], "unavailable");
        assert_eq!(result["returned_count"], 1);
        assert_eq!(
            result["items"][0]["member_ids"].as_array().unwrap().len(),
            2
        );
        assert_eq!(result["items"][0]["membership_status"], "complete");
        assert_eq!(result["items"][0]["span_ms"], 0);
        assert_eq!(result["items"][0]["observation_kind"], "instant");
        assert!(result.get("semantic_error").is_none());
        assert_eq!(
            result["items"][0]["sources"][0]["screenshot_id"],
            capture["screenshot_id"]
        );
        assert_eq!(
            result["items"][0]["sources"][0]["captured_at_ms"],
            capture["captured_at_ms"]
        );
        let outcome = crate::ai::tools::summarize("search_recaps", result.clone());
        assert_eq!(outcome.snapshots.len(), 1);
        assert_eq!(
            outcome.snapshots[0].screenshot_id,
            capture["screenshot_id"].as_i64().unwrap()
        );
        assert_eq!(
            outcome.snapshots[0].captured_at_ms,
            capture["captured_at_ms"].as_i64()
        );
        assert!(!result.to_string().contains("private runtime path"));
        assert_eq!(
            search_error(diagnostic.into()),
            "RECAP_SEARCH_MODEL_UNAVAILABLE"
        );
        assert_eq!(
            search_error("RECAP_WORKER_FAILED: stage=query_load: private panic detail".into()),
            "RECAP_WORKER_FAILED"
        );
        assert_eq!(search_error("AUTH_REQUIRED".into()), "AUTH_REQUIRED");
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
    fn membership_exports_all_assignments_with_explicit_privacy_and_legacy_status() {
        let mut day = fixture();
        let batch = &mut day.batches[0];
        let mut private_member = batch.records[0].clone();
        private_member.id = 43;
        private_member.window_title = "private window".into();
        let mut safe_member = batch.records[0].clone();
        safe_member.id = 44;
        batch.records.extend([private_member, safe_member]);
        batch.activities[0].member_ids = vec![44, 42, 43, 44];
        for mode in ["mask", "remove_paragraph", "reject"] {
            let result = day_result(day.clone(), &query(), &filter(mode)).unwrap();
            let item = &result["items"][0];
            assert_eq!(item["sources"].as_array().unwrap().len(), 1);
            if mode == "reject" {
                assert_eq!(item["member_ids"], json!([42, 44]));
                assert_eq!(item["membership_status"], "filtered");
            } else {
                assert_eq!(item["member_ids"], json!([42, 43, 44]));
                assert_eq!(item["membership_status"], "complete");
            }
            let outcome = crate::ai::tools::summarize("get_recap_day", result);
            assert_eq!(outcome.snapshots.len(), 1);
            assert_eq!(outcome.snapshots[0].screenshot_id, 42);
        }
        day.batches[0].activities[0].member_ids.push(45);
        let result = day_result(day.clone(), &query(), &filter("mask")).unwrap();
        assert_eq!(result["items"][0]["membership_status"], "incomplete");
        assert_eq!(result["items"][0]["member_ids"], json!([42, 43, 44]));
        day.batches[0].activities[0].member_ids.clear();
        let result = day_result(day, &query(), &filter("mask")).unwrap();
        assert_eq!(result["items"][0]["membership_status"], "unavailable");
        assert_eq!(result["items"][0]["member_ids"], json!([]));
    }

    #[test]
    fn recap_paginates_filtered_activities_and_validates_periods() {
        let mut day = fixture();
        let mut second = day.batches[0].activities[0].clone();
        second.id = "activity-2".into();
        day.batches[0].activities.insert(0, second);
        let mut query = query();
        query.page.limit = Some(1);
        let first = day_result(day.clone(), &query, &filter("mask")).unwrap();
        assert_eq!(first["items"][0]["activity_id"], "activity-1");
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["returned_count"], 1);
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
        assert_eq!(result["returned_count"], 1);
        for args in [
            json!({"start_date":"2026-02-30"}),
            json!({"start_date":"2026-10-02","end_date":"2026-10-01"}),
            json!({"limit":0}),
            json!({"limit":101}),
            json!({"offset":100001}),
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
