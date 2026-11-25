//! Half-open time accounting shared by persisted checkpoints and live data.
use super::types::*;
use crate::calendar::CalendarDay;
use std::collections::{BTreeMap, HashMap};

/// A checkpoint and its live continuation are the same observation. Clock
/// reversals or overlapping sessions never allow the day to be counted twice.
pub(crate) fn normalized(spans: Vec<UsageSpan>, start: i64, end: i64) -> Vec<UsageSpan> {
    let mut unique: BTreeMap<(i64, i64), UsageSpan> = BTreeMap::new();
    for mut span in spans {
        span.start_ms = span.start_ms.max(start);
        span.end_ms = span.end_ms.min(end);
        if span.end_ms <= span.start_ms {
            continue;
        }
        unique
            .entry((span.run_id, span.sequence))
            .and_modify(|old| {
                old.start_ms = old.start_ms.min(span.start_ms);
                old.end_ms = old.end_ms.max(span.end_ms);
                if old.identity.is_none() {
                    old.identity = span.identity.clone();
                }
            })
            .or_insert(span);
    }
    let mut spans: Vec<_> = unique.into_values().collect();
    spans.sort_by_key(|s| (s.start_ms, s.run_id, s.sequence));
    let mut covered = start;
    spans.retain_mut(|s| {
        s.start_ms = s.start_ms.max(covered);
        if s.end_ms <= s.start_ms {
            return false;
        }
        covered = s.end_ms;
        true
    });
    spans
}

pub(crate) fn app_id(app: &UsageAppIdentity) -> String {
    // Metadata encryption keys rotate between runs. The authenticated view
    // groups their catalog rows by executable rather than by encryption key.
    app.process_name.to_lowercase()
}

pub(crate) fn summarize(
    calendar: CalendarDay,
    spans: Vec<UsageSpan>,
    now_ms: i64,
    recorded_since_ms: Option<i64>,
    week: Vec<UsageWeekDay>,
) -> UsageDay {
    let end = now_ms.min(calendar.end_ms).max(calendar.start_ms);
    let spans = normalized(spans, calendar.start_ms, end);
    let mut result = UsageDay {
        generation: 0,
        date: calendar.date.clone(),
        calendar: calendar.clone(),
        recorded_since_ms,
        total_ms: 0,
        observed_ms: 0,
        first_ms: None,
        last_ms: None,
        switches: 0,
        longest_ms: 0,
        apps: vec![],
        categories: vec![],
        hours: vec![],
        intervals: vec![],
        week,
    };
    let mut apps: HashMap<String, UsageApp> = HashMap::new();
    let mut categories: HashMap<String, i64> = HashMap::new();
    let mut app_categories: HashMap<(String, String), i64> = HashMap::new();
    let mut hour_categories: Vec<HashMap<String, i64>> = vec![];
    let mut hour_start = calendar.start_ms;
    while hour_start < calendar.end_ms {
        let hour_end = (hour_start + 3_600_000).min(calendar.end_ms);
        result.hours.push(UsageHour {
            start_ms: hour_start,
            end_ms: hour_end,
            total_ms: 0,
            categories: vec![],
        });
        hour_categories.push(HashMap::new());
        hour_start = hour_end;
    }
    let mut covered = calendar.start_ms;
    let mut previous_app: Option<(String, i64, i64)> = None;
    let mut last_active_app: Option<String> = None;
    for span in spans {
        if span.start_ms > covered {
            push_interval(
                &mut result.intervals,
                UsageInterval {
                    start_ms: covered,
                    end_ms: span.start_ms,
                    state: UsageState::Unknown,
                    app_id: None,
                    category: None,
                },
            );
        }
        let duration = span.end_ms - span.start_ms;
        if span.state != UsageState::Unknown {
            result.observed_ms += duration;
        }
        let identity = span.identity.as_deref();
        let id = identity.map(|i| app_id(&i.app));
        let category = identity.map(|i| i.category.clone());
        if span.state == UsageState::Active {
            result.total_ms += duration;
            result.first_ms.get_or_insert(span.start_ms);
            result.last_ms = Some(span.end_ms);
            if let (Some(identity), Some(id)) = (identity, id.as_ref()) {
                let continuing = previous_app
                    .as_ref()
                    .is_some_and(|(old, _, end)| old == id && *end == span.start_ms);
                let session_start = if continuing {
                    previous_app.as_ref().unwrap().1
                } else {
                    span.start_ms
                };
                let app = apps.entry(id.clone()).or_insert_with(|| UsageApp {
                    id: id.clone(),
                    process_name: identity.app.process_name.clone(),
                    name: identity.app.name.clone(),
                    icon: identity.app.icon.clone(),
                    category: identity.category.clone(),
                    category_is_manual: false,
                    total_ms: 0,
                    visits: 0,
                    longest_ms: 0,
                });
                app.total_ms += duration;
                if !continuing {
                    app.visits += 1;
                }
                app.longest_ms = app.longest_ms.max(span.end_ms - session_start);
                result.longest_ms = result.longest_ms.max(app.longest_ms);
                if last_active_app.as_ref().is_some_and(|old| old != id) {
                    result.switches += 1;
                }
                last_active_app = Some(id.clone());
                previous_app = Some((id.clone(), session_start, span.end_ms));
                *categories.entry(identity.category.clone()).or_default() += duration;
                *app_categories
                    .entry((id.clone(), identity.category.clone()))
                    .or_default() += duration;
            }
            for (index, hour) in result.hours.iter_mut().enumerate() {
                let overlap = span.end_ms.min(hour.end_ms) - span.start_ms.max(hour.start_ms);
                if overlap > 0 {
                    hour.total_ms += overlap;
                    if let Some(category) = category.as_ref() {
                        *hour_categories[index].entry(category.clone()).or_default() += overlap;
                    }
                }
            }
        } else {
            previous_app = None;
        }
        push_interval(
            &mut result.intervals,
            UsageInterval {
                start_ms: span.start_ms,
                end_ms: span.end_ms,
                state: span.state,
                app_id: id,
                category,
            },
        );
        covered = span.end_ms;
    }
    if covered < end {
        push_interval(
            &mut result.intervals,
            UsageInterval {
                start_ms: covered,
                end_ms: end,
                state: UsageState::Unknown,
                app_id: None,
                category: None,
            },
        );
    }
    result.apps = apps.into_values().collect();
    for app in &mut result.apps {
        if let Some(((.., category), _)) = app_categories
            .iter()
            .filter(|((id, _), _)| id == &app.id)
            .max_by_key(|(_, ms)| **ms)
        {
            app.category = category.clone();
        }
    }
    result
        .apps
        .sort_by(|a, b| b.total_ms.cmp(&a.total_ms).then_with(|| a.id.cmp(&b.id)));
    result.categories = category_list(categories);
    for (hour, cats) in result.hours.iter_mut().zip(hour_categories) {
        hour.categories = category_list(cats);
    }
    result
}

fn category_list(categories: HashMap<String, i64>) -> Vec<UsageCategory> {
    let mut list: Vec<_> = categories
        .into_iter()
        .map(|(category, total_ms)| UsageCategory { category, total_ms })
        .collect();
    list.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.category.cmp(&b.category))
    });
    list
}

fn push_interval(intervals: &mut Vec<UsageInterval>, interval: UsageInterval) {
    if let Some(last) = intervals.last_mut() {
        if last.end_ms == interval.start_ms
            && last.state == interval.state
            && last.app_id == interval.app_id
            && last.category == interval.category
        {
            last.end_ms = interval.end_ms;
            return;
        }
    }
    intervals.push(interval);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn span(sequence: i64, start: i64, end: i64, title: &str) -> UsageSpan {
        UsageSpan {
            run_id: 1,
            sequence,
            start_ms: start,
            end_ms: end,
            state: UsageState::Active,
            identity: Some(Arc::new(UsageIdentity {
                app: Arc::new(UsageAppIdentity {
                    process_name: "code.exe".into(),
                    name: "Code".into(),
                    process_path: "code.exe".into(),
                    icon: None,
                }),
                title: title.into(),
                url: None,
                category: "编程开发".into(),
            })),
        }
    }
    #[test]
    fn checkpoints_titles_and_unknown_gaps_do_not_inflate_visits_or_time() {
        let day = CalendarDay::in_zone("2026-11-24", chrono_tz::UTC).unwrap();
        let s = day.start_ms;
        let result = summarize(
            day,
            vec![
                span(0, s, s + 60_000, "a"),
                span(0, s, s + 90_000, "a"),
                span(1, s + 90_000, s + 120_000, "b"),
                span(2, s + 180_000, s + 240_000, "c"),
            ],
            s + 300_000,
            None,
            vec![],
        );
        assert_eq!(result.total_ms, 180_000);
        assert_eq!(result.apps[0].visits, 2);
        assert_eq!(result.longest_ms, 120_000);
        assert_eq!(result.hours[0].total_ms, 180_000);
        assert_eq!(
            result
                .intervals
                .iter()
                .filter(|i| i.state == UsageState::Unknown)
                .count(),
            2
        );
        assert_eq!(result.observed_ms, 180_000);
    }
    #[test]
    fn dst_days_keep_real_hours_and_clip_live_midnight_continuations() {
        for (date, hours) in [("2026-03-08", 23), ("2026-11-01", 25)] {
            let day = CalendarDay::in_zone(date, chrono_tz::America::New_York).unwrap();
            let result = summarize(
                day.clone(),
                vec![span(0, day.start_ms - 1_000, day.end_ms + 1_000, "a")],
                day.end_ms + 1_000,
                None,
                vec![],
            );
            assert_eq!(result.hours.len(), hours);
            assert_eq!(result.total_ms, day.end_ms - day.start_ms);
            assert!(result.hours.iter().all(|h| h.total_ms == 3_600_000));
        }
    }
}
