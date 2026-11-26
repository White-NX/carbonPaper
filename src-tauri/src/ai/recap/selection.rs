use super::types::*;
use super::validation::{self, Report};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Preserve beginnings, changes and endings, rather than only an OCR prefix.
pub fn excerpt(text: &str, previous: Option<&str>, limit: usize) -> String {
    super::delivery::excerpt(text, previous, limit).0
}

pub fn prepare(records: &mut [Evidence]) {
    let mut previous: HashMap<String, String> = HashMap::new();
    let mut last_context = String::new();
    let mut last_time = 0;
    let mut segment = String::new();
    let mut spans: HashMap<String, (i64, i64)> = HashMap::new();
    for e in records.iter_mut() {
        e.context = digest(&format!(
            "{}\n{}\n{}\n{}",
            e.process_name, e.window_title, e.page_url, e.context
        ));
        if e.context != last_context || e.timestamp_ms - last_time > 600_000 {
            segment = format!("segment-{}", e.id);
        }
        e.segment = segment.clone();
        let span = spans
            .entry(segment.clone())
            .or_insert((e.timestamp_ms, e.timestamp_ms));
        span.1 = e.timestamp_ms;
        let prior = previous.get(&e.context);
        let words: HashSet<&str> = e.text.split_whitespace().collect();
        e.novelty = prior
            .map(|s| {
                let before: HashSet<&str> = s.split_whitespace().collect();
                1.0 - words.intersection(&before).count() as f64
                    / words.union(&before).count().max(1) as f64
            })
            .unwrap_or(1.0);
        previous.insert(e.context.clone(), e.text.clone());
        last_context = e.context.clone();
        last_time = e.timestamp_ms;
    }
    for e in records {
        let (start, end) = spans[&e.segment];
        e.span_ms = end - start;
    }
}

/// Deterministic coverage, change/result slots and seeded exploration (70/20/10).
pub fn selection_order(records: &[Evidence], seed: u64) -> Vec<usize> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut seen = HashSet::new();
    // Compare complete normalized OCR, before excerpting. Keep repeated visits
    // in different segments and keep every original record in the local index.
    let mut remaining: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let normalized = e
                .text
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            seen.insert((e.segment.as_str(), digest(&normalized)))
                .then_some(i)
        })
        .collect();
    let mut contexts = HashMap::<&str, u32>::new();
    let mut apps = HashMap::<&str, u32>::new();
    let mut times = HashMap::<i64, u32>::new();
    let mut categories = HashMap::<&str, u32>::new();
    let mut order = Vec::with_capacity(records.len());
    while !remaining.is_empty() {
        let slot = order.len() % 10;
        let position = if slot == 9 {
            rng.gen_range(0..remaining.len())
        } else {
            remaining
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    let score = |i: usize| {
                        let e = &records[i];
                        let count = *contexts.get(e.context.as_str()).unwrap_or(&0) as f64;
                        let coverage = 3.0 / (1.0 + count).powi(2)
                            + 1.0 / (1.0 + *apps.get(e.process_name.as_str()).unwrap_or(&0) as f64)
                            + 1.0
                                / (1.0
                                    + *times.get(&(e.timestamp_ms / 1_800_000)).unwrap_or(&0)
                                        as f64);
                        let semantic = if e.screening.confidence >= 0.8
                            && !e.screening.category.is_empty()
                            && e.screening.category != "unclear"
                        {
                            1.0 / (1.0
                                + *categories.get(e.screening.category.as_str()).unwrap_or(&0)
                                    as f64)
                        } else {
                            0.0
                        };
                        let signal = e.novelty + if e.screening.result { 2.0 } else { 0.0 };
                        if slot >= 7 {
                            signal + coverage * 0.1
                        } else {
                            coverage
                                + semantic
                                + signal * 0.15
                                + (e.span_ms.max(0) as f64 / 14_400_000.0).sqrt() * 0.15
                        }
                    };
                    score(**a)
                        .total_cmp(&score(**b))
                        .then_with(|| records[**b].id.cmp(&records[**a].id))
                })
                .map(|(p, _)| p)
                .unwrap()
        };
        let i = remaining.remove(position);
        let e = &records[i];
        *contexts.entry(&e.context).or_default() += 1;
        *apps.entry(&e.process_name).or_default() += 1;
        *times.entry(e.timestamp_ms / 1_800_000).or_default() += 1;
        *categories.entry(&e.screening.category).or_default() += 1;
        order.push(i);
    }
    order
}

pub fn parse_draft(text: &str) -> Result<Draft, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("RECAP_EMPTY_RESPONSE".into());
    }
    let raw = if trimmed.starts_with("```") {
        trimmed
            .split_once('\n')
            .map(|(_, body)| body.trim_end().trim_end_matches("```").trim())
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    // serde's Display may contain returned text; report only structural metadata.
    let draft: Draft = serde_json::from_str(raw).map_err(|e| {
        format!(
            "{}: category={:?}, line={}, column={}",
            if e.is_data() {
                "RECAP_INVALID_STRUCTURE"
            } else {
                "RECAP_INVALID_JSON"
            },
            e.classify(),
            e.line(),
            e.column()
        )
    })?;
    if draft.activities.len() > 80 || draft.gaps.len() > 3 {
        return Err("RECAP_RESPONSE_TOO_LARGE".into());
    }
    Ok(draft)
}

#[cfg(test)]
pub fn validate_draft(
    draft: &Draft,
    evidence: &[Evidence],
    prior_tasks: &HashSet<String>,
    day: &str,
) -> Result<Vec<RecapActivity>, String> {
    let report = validate_value(
        &json!(draft),
        evidence,
        prior_tasks,
        day,
        i64::MIN,
        i64::MAX,
    );
    if let Some(error) = report.first_error() {
        return Err(error.into());
    }
    Ok(report.value)
}

pub(super) fn validate_value(
    draft: &Value,
    evidence: &[Evidence],
    prior_tasks: &HashSet<String>,
    day: &str,
    start: i64,
    end: i64,
) -> Report<Vec<RecapActivity>> {
    let mut report = Report::new(vec![]);
    let Some(items) = draft.get("activities").and_then(Value::as_array) else {
        report.issues.push(validation::issue(
            "",
            "activities",
            "RECAP_INVALID_STRUCTURE",
            "invalid_field",
        ));
        return report;
    };
    if items.len() > 80 {
        for index in 80..items.len() {
            if !items[index].is_null() {
                report.issues.push(validation::issue(
                    &format!("/activities/{index}"),
                    "activity",
                    "RECAP_RESPONSE_TOO_LARGE",
                    "too_many_items",
                ));
            }
        }
    }
    if let Some(gaps) = draft.get("gaps") {
        if serde_json::from_value::<Vec<Gap>>(gaps.clone()).is_err()
            || gaps.as_array().is_some_and(|gaps| gaps.len() > 3)
        {
            report.issues.push(validation::issue(
                "/gaps",
                "gaps",
                "RECAP_INVALID_STRUCTURE",
                "invalid_field",
            ));
        }
    }
    let sources: HashMap<i64, &Evidence> = evidence.iter().map(|e| (e.id, e)).collect();
    let received = sources.keys().copied().collect::<HashSet<_>>();
    let allowed = evidence
        .iter()
        .filter(|source| {
            source.timestamp_ms >= start
                && source.timestamp_ms < end
                && !source.text.trim().is_empty()
        })
        .map(|source| source.id)
        .collect::<HashSet<_>>();
    for (index, value) in items.iter().take(80).enumerate() {
        if value.is_null() {
            continue;
        }
        let target = format!("/activities/{index}");
        let mut base = value.clone();
        let detail_values = base
            .as_object_mut()
            .and_then(|object| object.remove("details"));
        let Ok(mut item) = serde_json::from_value::<DraftActivity>(base) else {
            report.issues.push(validation::issue(
                &target,
                "activity",
                "RECAP_INVALID_ACTIVITY",
                "invalid_field",
            ));
            continue;
        };
        let mut detail_errors = Vec::new();
        let mut parsed_details = Vec::new();
        if let Some(value) = detail_values {
            if let Some(details) = value.as_array() {
                if details.len() > 3 {
                    detail_errors.push(validation::issue(
                        &target,
                        "details",
                        "RECAP_INVALID_DETAIL",
                        "too_many_items",
                    ));
                }
                for (detail_index, value) in details.iter().take(3).enumerate() {
                    match serde_json::from_value::<RecapDetail>(value.clone()) {
                        Ok(detail) => parsed_details.push((detail_index, detail)),
                        Err(_) => detail_errors.push(validation::issue(
                            &target,
                            &format!("details[{detail_index}]"),
                            "RECAP_INVALID_DETAIL",
                            "invalid_field",
                        )),
                    }
                }
            } else {
                detail_errors.push(validation::issue(
                    &target,
                    "details",
                    "RECAP_INVALID_DETAIL",
                    "invalid_field",
                ));
            }
        }
        if item.text.trim().is_empty()
            || item.task_title.trim().is_empty()
            || item.text.chars().count() > 1600
            || item.task_title.chars().count() > 120
        {
            report.issues.push(validation::issue(
                &target,
                "text_or_title",
                "RECAP_INVALID_ACTIVITY",
                "invalid_text",
            ));
            continue;
        }
        let mut issues = validation::check_ids(
            &mut item.evidence_ids,
            &received,
            &allowed,
            &target,
            "evidence_ids",
            128,
        );
        let mut selected = item
            .evidence_ids
            .iter()
            .filter_map(|id| sources.get(id).copied().filter(|_| allowed.contains(id)))
            .collect::<Vec<_>>();
        selected.sort_by_key(|e| (e.timestamp_ms, e.id));
        selected.dedup_by_key(|e| e.id);
        let task_id = match item.task_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) if prior_tasks.contains(id) => id.to_string(),
            Some(id) if id.starts_with("new:") && id.len() <= 96 => {
                format!("task-{}", &digest(&format!("{day}\n{id}"))[..20])
            }
            Some(_) => {
                let mut error =
                    validation::issue(&target, "task_id", "RECAP_INVALID_TASK", "unknown_task");
                error.available_references = prior_tasks.iter().map(|id| json!(id)).collect();
                error.available_references.sort_by_key(Value::to_string);
                report.issues.extend(issues);
                report.issues.push(error);
                continue;
            }
            None => format!(
                "task-{}",
                &digest(&format!(
                    "{day}\n{}\n{}",
                    item.task_title.trim(),
                    item.evidence_ids.first().copied().unwrap_or_default()
                ))[..20]
            ),
        };
        // Preserve discontinuous occurrences. A semantic task does not imply a
        // continuous interval through intervening activities or missing capture.
        let mut groups: BTreeMap<&str, Vec<&Evidence>> = BTreeMap::new();
        for e in selected {
            groups.entry(&e.segment).or_default().push(e);
        }
        if groups.len() > 1 {
            issues.push(validation::issue(
                &target,
                "evidence_ids",
                "RECAP_MIXED_SEGMENTS",
                "mixed_segments",
            ));
        }
        if issues.iter().any(|issue| !issue.retainable) {
            report.issues.extend(issues);
            continue;
        }
        let mut warnings = issues
            .iter()
            .map(|issue| issue.warning())
            .collect::<Vec<_>>();
        let mut details = Vec::new();
        issues.extend(detail_errors);
        let detail_sources = item
            .evidence_ids
            .iter()
            .copied()
            .filter(|id| allowed.contains(id))
            .collect();
        for (detail_index, mut detail) in parsed_details {
            let field = format!("details[{detail_index}].evidence_ids");
            if detail.text.trim().is_empty() || detail.text.chars().count() > 300 {
                issues.push(validation::issue(
                    &target,
                    &format!("details[{detail_index}].text"),
                    "RECAP_INVALID_DETAIL",
                    "invalid_text",
                ));
                continue;
            }
            let mut detail_issues = validation::check_ids(
                &mut detail.evidence_ids,
                &received,
                &detail_sources,
                &target,
                &field,
                12,
            );
            for error in &mut detail_issues {
                if !error.retainable {
                    error.code = "RECAP_INVALID_DETAIL".into();
                }
            }
            if detail_issues.iter().all(|issue| issue.retainable) {
                warnings.extend(detail_issues.iter().map(|issue| issue.warning()));
                details.push(detail);
            }
            issues.extend(detail_issues);
        }
        let group = groups.into_values().next().unwrap_or_default();
        let id = group.first().map_or_else(
            || {
                format!(
                    "unverified-{}",
                    &digest(&format!(
                        "{day}\n{task_id}\n{}\n{}",
                        item.text.trim(),
                        json!(item.evidence_ids)
                    ))[..20]
                )
            },
            |source| format!("activity-{}-{}", source.id, &digest(&task_id)[..8]),
        );
        report.value.push(RecapActivity {
            details,
            id,
            task_id,
            task_title: item.task_title.trim().into(),
            text: item.text.trim().into(),
            start_ms: group.first().map_or(start, |source| source.timestamp_ms),
            end_ms: group.last().map_or(start, |source| source.timestamp_ms),
            segments: group
                .first()
                .map(|source| vec![source.segment.clone()])
                .unwrap_or_default(),
            member_ids: group.iter().map(|source| source.id).collect(),
            sources: group.into_iter().map(SourceRef::from).collect(),
            warnings,
        });
        report.issues.extend(issues);
    }
    report
        .value
        .sort_by_key(|activity| (activity.start_ms, activity.id.clone()));
    report.value.dedup_by(|a, b| a.id == b.id);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(id: i64, time: i64, title: &str, text: &str) -> Evidence {
        Evidence {
            id,
            timestamp_ms: time,
            process_name: "app".into(),
            window_title: title.into(),
            page_url: String::new(),
            text: text.into(),
            segment: String::new(),
            context: String::new(),
            novelty: 0.0,
            span_ms: 0,
            screening: Default::default(),
            supplied_ranges: vec![],
        }
    }
    #[test]
    fn parse_errors_explain_the_failure_without_copying_model_text() {
        assert_eq!(parse_draft(" \n ").unwrap_err(), "RECAP_EMPTY_RESPONSE");
        let malformed =
            parse_draft(r#"{"private":"private-screen-text","activities":["#).unwrap_err();
        assert!(malformed.starts_with("RECAP_INVALID_JSON:"));
        assert!(malformed.contains("line="));
        assert!(!malformed.contains("private-screen-text"));
        let wrong_shape = parse_draft(r#"{"activities":"private-screen-text"}"#).unwrap_err();
        assert!(wrong_shape.starts_with("RECAP_INVALID_STRUCTURE:"));
        assert!(!wrong_shape.contains("private-screen-text"));
        assert!(parse_draft("```json\n{\"activities\":[]}\n```").is_ok());
    }
    #[test]
    fn evidence_keeps_interruptions_and_sparse_gaps_separate() {
        let mut rs = vec![
            e(1, 0, "A", "one"),
            e(2, 10_000, "B", "two"),
            e(3, 20_000, "A", "one"),
            e(4, 900_000, "A", "one"),
        ];
        prepare(&mut rs);
        assert_ne!(rs[0].segment, rs[2].segment);
        assert_ne!(rs[2].segment, rs[3].segment);
        let d = Draft {
            activities: vec![DraftActivity {
                details: vec![],
                task_id: None,
                task_title: "A".into(),
                text: "Observed A".into(),
                evidence_ids: vec![1, 3, 4],
            }],
            gaps: vec![],
        };
        assert_eq!(
            validate_draft(&d, &rs, &HashSet::new(), "2026-09-01").unwrap_err(),
            "RECAP_MIXED_SEGMENTS"
        );
    }
    #[test]
    fn rejects_fabricated_sources_and_task_ids() {
        let mut rs = vec![e(1, 0, "A", "text")];
        prepare(&mut rs);
        let mut d = Draft {
            activities: vec![DraftActivity {
                details: vec![],
                task_id: None,
                task_title: "A".into(),
                text: "x".into(),
                evidence_ids: vec![999],
            }],
            gaps: vec![],
        };
        assert!(validate_draft(&d, &rs, &HashSet::new(), "day").is_err());
        d.activities[0].evidence_ids = vec![1];
        d.activities[0].task_id = Some("invented".into());
        assert!(validate_draft(&d, &rs, &HashSet::new(), "day").is_err());
    }
    #[test]
    fn key_details_require_received_evidence_from_the_same_activity() {
        let mut records = vec![e(1, 0, "A", "cache comparison"), e(2, 1, "B", "unrelated")];
        prepare(&mut records);
        let mut draft: Draft = serde_json::from_value(serde_json::json!({
            "activities":[{"task_id":"new:1","task_title":"Compare caching","text":"Read a comparison.",
                "evidence_ids":[1],"details":[{"text":"The document proposes an in-memory cache.","evidence_ids":[1]}]}]
        })).unwrap();
        let result = validate_draft(&draft, &records, &HashSet::new(), "day").unwrap();
        assert_eq!(result[0].details[0].evidence_ids, vec![1]);
        draft.activities[0].details[0].evidence_ids = vec![2];
        assert_eq!(
            validate_draft(&draft, &records, &HashSet::new(), "day").unwrap_err(),
            "RECAP_INVALID_DETAIL"
        );
        draft.activities[0].details[0].evidence_ids = vec![1];
        draft.activities[0].details[0].text = "x".repeat(301);
        assert_eq!(
            validate_draft(&draft, &records, &HashSet::new(), "day").unwrap_err(),
            "RECAP_INVALID_DETAIL"
        );
    }
    #[test]
    fn coverage_does_not_let_repetition_hide_a_short_activity() {
        let mut rs = (1..100)
            .map(|i| e(i, i * 1000, "Repeated", "same screen"))
            .collect::<Vec<_>>();
        rs.push(e(100, 100_000, "Short", "new result"));
        prepare(&mut rs);
        let order = selection_order(&rs, 7);
        assert!(order.iter().take(5).any(|i| *i == 99));
        assert_eq!(order.len(), 2);
        assert_eq!(order, selection_order(&rs, 7));
    }
    #[test]
    fn excerpts_keep_changes_and_tail() {
        let old = "old line\n".repeat(100);
        let text = format!("{old}new result\nfinal confirmation");
        let part = excerpt(&text, Some(&old), 100);
        assert!(part.contains("new result"));
        assert!(part.contains("final confirmation"));
    }

    #[test]
    fn long_excerpts_never_repeat_source_positions_and_obey_unicode_limits() {
        let text: String = (0x4e00..0x5200)
            .map(|c| char::from_u32(c).unwrap())
            .collect();
        for cap in [0, 1, 15, 16, 240, 900] {
            let part = excerpt(&text, None, cap);
            assert!(part.chars().count() <= cap);
            let chars: Vec<char> = part.chars().filter(|c| text.contains(*c)).collect();
            assert_eq!(chars.len(), chars.iter().collect::<HashSet<_>>().len());
            if cap >= 16 {
                assert!(part.starts_with(text.chars().next().unwrap()));
                assert!(part.ends_with(text.chars().last().unwrap()));
            }
        }
    }

    #[test]
    fn dedup_preserves_revisits_full_text_changes_and_local_records() {
        let mut records = vec![
            e(1, 0, "A", "same\ntext"),
            e(2, 1000, "A", " same\n\ntext "),
            e(3, 2000, "B", "same\ntext"),
            e(4, 3000, "A", "same\ntext"),
            e(5, 900_000, "A", "same\ntext"),
        ];
        let full = "x".repeat(3000);
        records.push(e(6, 901_000, "A", &full));
        let mut changed = full.clone();
        changed.replace_range(700..701, "y");
        records.push(e(7, 902_000, "A", &changed));
        prepare(&mut records);
        let order = selection_order(&records, 1);
        assert_eq!(records.len(), 7);
        assert_eq!(order.len(), 6);
        assert!(!order.contains(&1));
        assert!(order.contains(&5) && order.contains(&6));
        assert_eq!(records[5].text, full);
    }
}
