//! Public activity evidence semantics shared by day reads and search.
use super::{RecapActivity, SourceRef};
use crate::sensitive_filter::SensitiveFilterState;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize)]
pub struct ActivityEvidence {
    pub time_semantics: &'static str,
    pub observation_kind: &'static str,
    #[serde(rename = "span_ms")]
    pub observed_span_ms: i64,
    #[serde(rename = "member_ids")]
    pub member_screenshot_ids: Vec<i64>,
    pub membership_status: &'static str,
}

#[derive(Default)]
pub(crate) struct MemberDirectory {
    known: HashSet<i64>,
    exportable: HashSet<i64>,
}

/// Only current exportable identities can disclose membership. The directory
/// was collected under the recap privacy revision; recheck its identity fields
/// on every corpus load and day export, just as with direct citations.
pub(crate) fn member_directory(
    records: &[SourceRef],
    filter: &SensitiveFilterState,
) -> MemberDirectory {
    let mode = filter.mode();
    let mut directory = MemberDirectory::default();
    for source in records.iter().filter(|source| source.id > 0) {
        directory.known.insert(source.id);
        if crate::mcp_server::filter_identity(filter, mode, &source.process_name).is_ok()
            && crate::mcp_server::filter_identity(filter, mode, &source.window_title).is_ok()
        {
            directory.exportable.insert(source.id);
        }
    }
    directory
}

impl ActivityEvidence {
    /// Call after filtering the activity and its direct source identities.
    pub(crate) fn project(activity: &RecapActivity, directory: &MemberDirectory) -> Self {
        let mut recorded = activity.member_ids.clone();
        recorded.sort_unstable();
        recorded.dedup();
        let member_screenshot_ids: Vec<_> = recorded
            .iter()
            .copied()
            .filter(|id| {
                *id > 0
                    && (directory.exportable.contains(id)
                        || activity.sources.iter().any(|source| source.id == *id))
            })
            .collect();
        let membership_status = if recorded.is_empty() {
            "unavailable"
        } else if member_screenshot_ids.len() == recorded.len() {
            "complete"
        } else if recorded.iter().any(|id| {
            !directory.known.contains(id) && !activity.sources.iter().any(|source| source.id == *id)
        }) {
            "incomplete"
        } else {
            "filtered"
        };
        Self {
            time_semantics: "evidence_observation_span",
            observation_kind: if activity.start_ms == activity.end_ms {
                "instant"
            } else {
                "span"
            },
            observed_span_ms: activity.end_ms.saturating_sub(activity.start_ms).max(0),
            member_screenshot_ids,
            membership_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(members: Vec<i64>, start: i64, end: i64) -> RecapActivity {
        RecapActivity {
            id: "activity".into(),
            task_id: "task".into(),
            task_title: "title".into(),
            text: "text".into(),
            details: vec![],
            start_ms: start,
            end_ms: end,
            segments: vec!["segment".into()],
            sources: vec![SourceRef {
                id: 1,
                timestamp_ms: start,
                process_name: "editor.exe".into(),
                window_title: "Notes".into(),
            }],
            member_ids: members,
        }
    }

    #[test]
    fn membership_is_sorted_deduplicated_and_reports_missing_directory_rows() {
        let directory = MemberDirectory {
            known: HashSet::from([1, 3]),
            exportable: HashSet::from([1, 3]),
        };
        let complete = ActivityEvidence::project(&activity(vec![3, 1, 3], 10, 30), &directory);
        assert_eq!(complete.member_screenshot_ids, vec![1, 3]);
        assert_eq!(complete.membership_status, "complete");
        assert_eq!(complete.observation_kind, "span");
        assert_eq!(complete.observed_span_ms, 20);

        let mut filtered_directory = MemberDirectory::default();
        filtered_directory.known.extend([1, 2]);
        filtered_directory.exportable.insert(1);
        let filtered =
            ActivityEvidence::project(&activity(vec![1, 2], 10, 10), &filtered_directory);
        assert_eq!(filtered.member_screenshot_ids, vec![1]);
        assert_eq!(filtered.membership_status, "filtered");
        assert_eq!(filtered.observation_kind, "instant");
        assert_eq!(filtered.observed_span_ms, 0);

        let incomplete =
            ActivityEvidence::project(&activity(vec![1, 4], 10, 20), &filtered_directory);
        assert_eq!(incomplete.member_screenshot_ids, vec![1]);
        assert_eq!(incomplete.membership_status, "incomplete");

        let unavailable =
            ActivityEvidence::project(&activity(vec![], 30, 10), &MemberDirectory::default());
        assert!(unavailable.member_screenshot_ids.is_empty());
        assert_eq!(unavailable.membership_status, "unavailable");
        assert_eq!(unavailable.observed_span_ms, 0);
    }
}
