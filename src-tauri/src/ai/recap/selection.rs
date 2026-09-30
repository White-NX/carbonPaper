use super::types::*;
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Preserve beginnings, changes and endings, rather than only an OCR prefix.
pub fn excerpt(text: &str, previous: Option<&str>, limit: usize) -> String {
    let mut seen = HashSet::new();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert(*s))
        .collect();
    let joined = lines.join("\n");
    if joined.chars().count() <= limit {
        return joined;
    }
    let prior: HashSet<&str> = previous.unwrap_or("").lines().map(str::trim).collect();
    let changed = lines
        .iter()
        .filter(|s| !prior.contains(**s))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let head = limited(&joined, limit / 4);
    let middle = limited(&changed, limit / 2);
    let tail = joined
        .chars()
        .rev()
        .take(limit / 4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n[…]\n{middle}\n[…]\n{tail}")
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
        let compact = excerpt(&e.text, prior.map(String::as_str), 900);
        previous.insert(e.context.clone(), e.text.clone());
        e.text = compact;
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
    let mut remaining: Vec<usize> = (0..records.len()).collect();
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

pub fn validate_draft(
    draft: &Draft,
    evidence: &[Evidence],
    prior_tasks: &HashSet<String>,
    day: &str,
) -> Result<Vec<RecapActivity>, String> {
    let sources: HashMap<i64, &Evidence> = evidence.iter().map(|e| (e.id, e)).collect();
    let mut out = Vec::new();
    for item in &draft.activities {
        if item.text.trim().is_empty()
            || item.task_title.trim().is_empty()
            || item.text.chars().count() > 1600
            || item.task_title.chars().count() > 120
            || item.evidence_ids.is_empty()
            || item.evidence_ids.len() > 128
        {
            return Err("RECAP_INVALID_ACTIVITY".into());
        }
        let mut selected = item
            .evidence_ids
            .iter()
            .map(|id| {
                sources
                    .get(id)
                    .copied()
                    .ok_or("RECAP_INVALID_CITATION".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        selected.sort_by_key(|e| (e.timestamp_ms, e.id));
        selected.dedup_by_key(|e| e.id);
        let task_id = match item.task_id.as_deref().filter(|s| !s.is_empty()) {
            Some(id) if prior_tasks.contains(id) => id.to_string(),
            Some(id) if id.starts_with("new:") && id.len() <= 96 => {
                format!("task-{}", &digest(&format!("{day}\n{id}"))[..20])
            }
            Some(_) => return Err("RECAP_INVALID_TASK".into()),
            None => format!(
                "task-{}",
                &digest(&format!(
                    "{day}\n{}\n{}",
                    item.task_title.trim(),
                    selected[0].id
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
            return Err("RECAP_MIXED_SEGMENTS".into());
        }
        for (segment, group) in groups {
            out.push(RecapActivity {
                id: format!("activity-{}-{}", group[0].id, &digest(&task_id)[..8]),
                task_id: task_id.clone(),
                task_title: item.task_title.trim().into(),
                text: item.text.trim().into(),
                start_ms: group[0].timestamp_ms,
                end_ms: group.last().unwrap().timestamp_ms,
                segments: vec![segment.to_string()],
                member_ids: group.iter().map(|e| e.id).collect(),
                sources: group.into_iter().map(SourceRef::from).collect(),
            });
        }
    }
    out.sort_by_key(|a| (a.start_ms, a.id.clone()));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
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
    fn coverage_does_not_let_repetition_hide_a_short_activity() {
        let mut rs = (1..100)
            .map(|i| e(i, i * 1000, "Repeated", "same screen"))
            .collect::<Vec<_>>();
        rs.push(e(100, 100_000, "Short", "new result"));
        prepare(&mut rs);
        let order = selection_order(&rs, 7);
        assert!(order[..5].contains(&99));
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
}
