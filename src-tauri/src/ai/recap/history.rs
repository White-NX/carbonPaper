//! Bounded navigation over the already cached, corrected day. No extra model
//! call or persistent copy of decrypted activity history is needed.
use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use super::types::*;

const MAX_TASKS: usize = 64;
const MAX_CANDIDATES: usize = 12;
const MAX_HISTORY_TOKENS: u64 = 2_000;

pub(super) struct TaskHistory {
    pub directory: Vec<Value>,
    pub entries: Vec<Value>,
    pub omitted_tasks: usize,
}

fn terms(text: &str, limit: usize) -> HashSet<String> {
    const COMMON: &[&str] = &[
        "the", "and", "for", "with", "from", "this", "that", "read", "viewed", "work", "task",
        "project", "app", "chrome", "edge", "查看", "阅读", "进行", "相关", "项目", "内容", "页面",
        "工作", "一个", "以及", "了解", "使用",
    ];
    let mut result = HashSet::new();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-') {
        if word.len() >= 3 {
            let word = word.to_lowercase();
            if !COMMON.contains(&word.as_str()) {
                result.insert(word);
            }
        }
        if result.len() >= limit {
            return result;
        }
    }
    for word in text.split(|c: char| !('\u{3400}'..='\u{9fff}').contains(&c)) {
        let mut chars = word.chars();
        let mut previous = chars.next();
        for c in chars {
            let term = format!("{}{c}", previous.unwrap());
            if !COMMON.contains(&term.as_str()) {
                result.insert(term);
            }
            previous = Some(c);
            if result.len() >= limit {
                return result;
            }
        }
    }
    result
}

pub(super) fn select(
    day: &RecapDay,
    records: &[Evidence],
    start: i64,
    token_limit: u64,
) -> TaskHistory {
    // One pass replaces a full scan and sort of all activities for every task.
    let mut first = HashMap::<&str, i64>::new();
    let mut latest = HashMap::<&str, &RecapActivity>::new();
    for activity in day.batches.iter().flat_map(|b| &b.activities) {
        first
            .entry(&activity.task_id)
            .and_modify(|time| *time = (*time).min(activity.start_ms))
            .or_insert(activity.start_ms);
        if activity.end_ms < start {
            latest
                .entry(&activity.task_id)
                .and_modify(|old| {
                    if (activity.start_ms, &activity.id) > (old.start_ms, &old.id) {
                        *old = activity;
                    }
                })
                .or_insert(activity);
        }
    }
    // Appending a later task keeps the earlier directory entries in place.
    let mut tasks = day.threads.iter().collect::<Vec<_>>();
    tasks.sort_by_key(|task| {
        (
            first.get(task.id.as_str()).copied().unwrap_or(i64::MAX),
            &task.id,
        )
    });
    let directory = tasks
        .iter()
        .take(MAX_TASKS)
        .map(|task| json!({"id": task.id, "title": limited(&task.title, 120)}))
        .collect::<Vec<_>>();

    let mut current_terms = HashSet::new();
    let mut add_terms = |text: &str, limit| {
        let mut values = terms(text, limit).into_iter().collect::<Vec<_>>();
        values.sort_unstable();
        for value in values {
            if current_terms.len() >= 8_192 {
                break;
            }
            current_terms.insert(value);
        }
        current_terms.len() < 8_192
    };
    // Prefer titles and document locations before filling the bounded set
    // with sampled OCR. Do not retain another concatenated daily corpus.
    for record in records {
        add_terms(&record.window_title, 64);
        if !add_terms(&record.page_url, 64) {
            break;
        }
    }
    for record in records {
        if !add_terms(&super::selection::excerpt(&record.text, None, 1800), 256) {
            break;
        }
    }
    let related = records
        .iter()
        .filter_map(|record| record.screening.related_previous)
        .collect::<HashSet<_>>();
    let mut ranked = tasks
        .iter()
        .filter_map(|task| {
            let activity = *latest.get(task.id.as_str())?;
            let title_matches = terms(&task.title, 32).intersection(&current_terms).count();
            let mut body_terms = terms(&activity.text, 64);
            for detail in activity.details.iter().take(3) {
                body_terms.extend(terms(&detail.text, 32));
            }
            let body_matches = body_terms.intersection(&current_terms).count();
            let linked = activity
                .sources
                .iter()
                .any(|source| related.contains(&source.id));
            (linked || title_matches > 0 || body_matches >= 2).then_some((
                usize::from(linked) * 48 + title_matches * 4 + body_matches,
                activity,
            ))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, activity)| {
        (
            std::cmp::Reverse(*score),
            std::cmp::Reverse(activity.start_ms),
            &activity.id,
        )
    });
    let mut entries = Vec::new();
    let mut tokens = 0;
    for (_, activity) in ranked.into_iter().take(MAX_CANDIDATES) {
        let value = json!({
            "id": activity.id, "task_id": activity.task_id, "task_title": activity.task_title,
            "text": limited(&activity.text, 240), "time_ms": activity.start_ms,
            "key_details": activity.details.iter().take(1).map(|detail|
                json!({"text": limited(&detail.text, 160), "evidence_ids": detail.evidence_ids})
            ).collect::<Vec<_>>(),
            "source_ids": activity.sources.iter().take(3).map(|source| source.id).collect::<Vec<_>>(),
        });
        let cost = crate::ai::tokenizer::estimate(&value.to_string()).saturating_add(8);
        if tokens + cost > token_limit.min(MAX_HISTORY_TOKENS) {
            continue;
        }
        tokens += cost;
        entries.push(value);
    }
    TaskHistory {
        omitted_tasks: tasks.len().saturating_sub(directory.len()),
        directory,
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(count: usize) -> RecapDay {
        let activities = (0..count)
            .map(|index| RecapActivity {
                id: format!("activity-{index}"),
                task_id: format!("task-{index}"),
                task_title: format!("Atlas{index}"),
                text: format!("Read Atlas{index} documentation."),
                start_ms: index as i64 + 1,
                end_ms: index as i64 + 1,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let threads = activities
            .iter()
            .map(|activity| TaskThread {
                id: activity.task_id.clone(),
                title: activity.task_title.clone(),
                activity_ids: vec![activity.id.clone()],
            })
            .collect();
        RecapDay {
            date: "2026-11-25".into(),
            calendar: CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap(),
            batches: vec![RecapBatch {
                activities,
                ..Default::default()
            }],
            threads,
            mainlines: None,
            mainlines_error: None,
            mainlines_updated_at_ms: 0,
            mainlines_pending: false,
            mainlines_attempts: vec![],
            usage: Default::default(),
            can_undo: false,
            running: false,
            error: None,
        }
    }

    fn record(text: &str) -> Evidence {
        Evidence {
            id: 100,
            timestamp_ms: 100,
            process_name: "app".into(),
            window_title: text.into(),
            page_url: String::new(),
            text: text.into(),
            segment: String::new(),
            context: String::new(),
            novelty: 0.,
            span_ms: 0,
            screening: Default::default(),
        }
    }

    #[test]
    fn relevant_history_outside_the_directory_limit_remains_readable_and_future_text_is_excluded() {
        let mut day = day(72);
        day.batches[0].activities[71].task_title = "Atlas70 future".into();
        day.batches[0].activities[71].text = "Atlas70 future discovery".into();
        day.batches[0].activities[71].end_ms = 200;
        let selected = select(&day, &[record("Atlas70 documentation")], 100, 2_000);
        assert_eq!(selected.directory.len(), 64);
        assert_eq!(selected.omitted_tasks, 8);
        assert_eq!(selected.entries.len(), 1);
        assert_eq!(selected.entries[0]["task_id"], "task-70");
        assert!(!selected
            .directory
            .iter()
            .any(|item| item["id"] == "task-70"));
    }

    #[test]
    fn history_has_an_independent_body_budget_and_appending_occurrences_keeps_the_directory_stable()
    {
        let mut day = day(30);
        for activity in &mut day.batches[0].activities {
            activity.task_title = "Atlas queue".into();
            activity.text = "Atlas queue result ".repeat(80);
            activity.details = vec![RecapDetail {
                text: "Atlas queue finding ".repeat(50),
                evidence_ids: vec![1],
            }];
        }
        let records = [record("Atlas queue result")];
        let before = select(&day, &records, 100, 2_000);
        assert!(!before.entries.is_empty());
        assert!(before.entries.len() <= 12);
        let cost: u64 = before
            .entries
            .iter()
            .map(|value| crate::ai::tokenizer::estimate(&value.to_string()) + 8)
            .sum();
        assert!(cost <= 2_000);
        let mut later = day.batches[0].activities[0].clone();
        later.id = "later-occurrence".into();
        later.start_ms = 101;
        later.end_ms = 101;
        later.text = "Atlas queue result changed outcome".into();
        day.batches[0].activities.push(later);
        let after = select(&day, &records, 200, 2_000);
        assert_eq!(before.directory, after.directory);
        assert_ne!(before.entries, after.entries);
        day.threads[0].title = "My corrected task".into();
        assert_ne!(
            after.directory,
            select(&day, &records, 200, 2_000).directory
        );
    }
}
