//! Read-only snapshot associations using corrected, saved activity membership.
use super::{runner::privacy_fingerprint, types::*, views::with_current_day};
use crate::{
    calendar::CalendarDay, credential_manager::CredentialManagerState, storage::StorageState,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::Manager;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRecapStatus {
    Unavailable,
    Pending,
    Unmatched,
    Ready,
}

#[derive(Serialize)]
pub struct SnapshotRecapActivity {
    date: String,
    calendar: CalendarDay,
    activity_id: String,
    task_id: String,
    task_title: String,
    text: String,
    details: Vec<RecapDetail>,
    start_ms: i64,
    end_ms: i64,
}

#[derive(Serialize)]
pub struct SnapshotRecapContext {
    status: SnapshotRecapStatus,
    items: Vec<SnapshotRecapActivity>,
}

fn snapshot_items(day: &RecapDay, screenshot_id: i64) -> Vec<SnapshotRecapActivity> {
    day.batches
        .iter()
        .flat_map(|batch| &batch.activities)
        .filter(|activity| {
            activity.member_ids.contains(&screenshot_id)
                || activity
                    .sources
                    .iter()
                    .any(|source| source.id == screenshot_id)
        })
        .map(|activity| SnapshotRecapActivity {
            date: day.date.clone(),
            calendar: day.calendar.clone(),
            activity_id: activity.id.clone(),
            task_id: activity.task_id.clone(),
            task_title: activity.task_title.clone(),
            text: activity.text.clone(),
            details: activity.details.clone(),
            start_ms: activity.start_ms,
            end_ms: activity.end_ms,
        })
        .collect()
}

fn snapshot_status(
    has_items: bool,
    pending: bool,
    enabled: bool,
    has_dates: bool,
    only_deleted: bool,
) -> SnapshotRecapStatus {
    if has_items {
        SnapshotRecapStatus::Ready
    } else if only_deleted {
        SnapshotRecapStatus::Unmatched
    } else if pending || (enabled && !has_dates) {
        SnapshotRecapStatus::Pending
    } else if !enabled && !has_dates {
        SnapshotRecapStatus::Unavailable
    } else {
        SnapshotRecapStatus::Unmatched
    }
}

pub fn read_snapshot_context(
    app: &tauri::AppHandle,
    screenshot_id: i64,
) -> Result<SnapshotRecapContext, String> {
    let credentials = app.state::<Arc<CredentialManagerState>>();
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if crate::maintenance::is_active() {
        return Err("MAINTENANCE_IN_PROGRESS".into());
    }
    if screenshot_id <= 0 {
        return Err("RECAP_INVALID_SOURCE".into());
    }
    let storage = app.state::<Arc<StorageState>>();
    let generation = storage.db_generation();
    let privacy = privacy_fingerprint(app)?;
    let config_revision = storage.recap_config_revision()?;
    let Some(scope) = storage.recap_snapshot_scope(screenshot_id)? else {
        return Ok(SnapshotRecapContext {
            status: SnapshotRecapStatus::Unavailable,
            items: vec![],
        });
    };
    let enabled = storage
        .recap_read::<RecapSettings>("settings", "settings", None)?
        .unwrap_or_default()
        .enabled;
    let mut items = Vec::new();
    let mut pending = false;
    let mut versions = Vec::new();
    for date in &scope.dates {
        let (start, end) = day_bounds(date)?;
        let (revision, _, _) = storage.recap_day_revision(date, start, end, &privacy)?;
        let correction_version = storage.recap_correction_version(date)?;
        let (matches, awaiting) = with_current_day(app, date, |day| {
            let awaiting = day
                .batches
                .iter()
                .find(|batch| {
                    batch.start_ms <= scope.timestamp_ms && scope.timestamp_ms < batch.end_ms
                })
                .is_none_or(|batch| batch.status == "pending");
            Ok((snapshot_items(&day, screenshot_id), awaiting))
        })?;
        items.extend(matches);
        pending |= awaiting;
        versions.push((date, revision, correction_version));
    }
    // Multiple saved calendars can overlap after a zone change. Fence the
    // entire projection, including corrections, rather than only the last day.
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if generation != storage.db_generation()
        || privacy.as_str() != privacy_fingerprint(app)?.as_str()
        || config_revision != storage.recap_config_revision()?
        || storage.recap_snapshot_scope(screenshot_id)?.as_ref() != Some(&scope)
    {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    for (date, revision, correction_version) in versions {
        if !storage.recap_revision_matches(date, revision, generation)?
            || storage.recap_correction_version(date)? != correction_version
        {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
    }
    items.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| a.date.cmp(&b.date))
            .then_with(|| a.activity_id.cmp(&b.activity_id))
    });
    Ok(SnapshotRecapContext {
        status: snapshot_status(
            !items.is_empty(),
            pending,
            enabled,
            !scope.dates.is_empty(),
            scope.only_deleted,
        ),
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> RecapDay {
        let source = |id| {
            serde_json::json!({
                "id": id, "timestamp_ms": 100, "process_name": "Editor", "window_title": "Same title"
            })
        };
        let batch: RecapBatch = serde_json::from_value(serde_json::json!({
            "start_ms": 0, "end_ms": 1000, "updated_at_ms": 1, "status": "ready",
            "activities": [{
                "id": "activity-a", "task_id": "task-a", "task_title": "Original title",
                "text": "Reviewed the capture settings.", "details": [], "start_ms": 100,
                "end_ms": 100, "segments": ["segment-a"], "sources": [source(1)], "member_ids": [1, 2]
            }, {
                "id": "activity-b", "task_id": "task-b", "task_title": "Other task",
                "text": "Reviewed a different topic.", "details": [], "start_ms": 200,
                "end_ms": 200, "segments": ["segment-b"], "sources": [source(3)]
            }],
            "records": [source(1), source(2), source(3), source(4)], "coverage": 2, "error": null
        })).unwrap();
        RecapDay {
            date: "2026-01-01".into(),
            calendar: CalendarDay::in_zone("2026-01-01", chrono_tz::Asia::Tokyo).unwrap(),
            batches: vec![batch],
            threads: vec![],
            usage: RecapUsage::default(),
            can_undo: false,
            running: false,
            error: None,
        }
    }

    #[test]
    fn includes_uncited_members_and_legacy_sources_without_title_inference() {
        let day = day();
        assert_eq!(snapshot_items(&day, 2)[0].activity_id, "activity-a");
        assert_eq!(snapshot_items(&day, 3)[0].activity_id, "activity-b");
        assert!(snapshot_items(&day, 4).is_empty());
        let value = serde_json::to_value(&snapshot_items(&day, 2)[0]).unwrap();
        assert_eq!(value["calendar"]["time_zone"], "Asia/Tokyo");
        assert_eq!(value["start_ms"], 100);
        assert!(value.get("member_ids").is_none());
        assert!(value.get("sources").is_none());
        assert!(value.get("records").is_none());
    }

    #[test]
    fn snapshot_associations_follow_corrected_tasks_and_names() {
        let mut day = day();
        let mut corrections = Corrections::default();
        corrections.assignments.insert(1, "task-b".into());
        corrections.assignments.insert(2, "task-b".into());
        corrections.names.insert("task-b".into(), "My event".into());
        apply_corrections(&mut day.batches, &corrections);
        let items = snapshot_items(&day, 2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].task_id, "task-b");
        assert_eq!(items[0].task_title, "My event");
    }

    #[test]
    fn distinguishes_unconfigured_pending_unmatched_and_deleted_recaps() {
        assert_eq!(
            snapshot_status(true, true, false, true, false),
            SnapshotRecapStatus::Ready
        );
        assert_eq!(
            snapshot_status(false, false, false, false, false),
            SnapshotRecapStatus::Unavailable
        );
        assert_eq!(
            snapshot_status(false, false, true, false, false),
            SnapshotRecapStatus::Pending
        );
        assert_eq!(
            snapshot_status(false, false, true, true, false),
            SnapshotRecapStatus::Unmatched
        );
        assert_eq!(
            snapshot_status(false, true, true, false, true),
            SnapshotRecapStatus::Unmatched
        );
    }
}
