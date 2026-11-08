//! The desktop correction algorithm over typed, attachment-free text records.
use crate::types::*;
use sha2::Digest;
use std::collections::BTreeMap;

pub fn apply_corrections(
    batches: &mut [RecapContent],
    corrections: &RecapContent,
) -> Vec<TaskText> {
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
    let mut threads: BTreeMap<String, TaskText> = BTreeMap::new();
    for batch in batches {
        let original = std::mem::take(&mut batch.activities);
        for activity in original {
            let mut groups = BTreeMap::<String, Vec<SourceText>>::new();
            for source in &activity.sources {
                let task = resolve(
                    corrections
                        .assignments
                        .get(&source.screenshot_id)
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
                    a.id = format!(
                        "{}-{}",
                        a.id,
                        &hex::encode(sha2::Sha256::digest(a.task_id.as_bytes()))[..8]
                    );
                }
                if let Some(title) = corrections.names.get(&a.task_id) {
                    a.task_title = title.clone();
                }
                a.start_ms = sources
                    .iter()
                    .map(|s| s.captured_at_ms)
                    .min()
                    .unwrap_or(a.start_ms);
                a.end_ms = sources
                    .iter()
                    .map(|s| s.captured_at_ms)
                    .max()
                    .unwrap_or(a.end_ms);
                a.sources = sources;
                // A fact requiring evidence from both corrected tasks cannot
                // be attributed to either task on its own.
                a.details.retain(|detail| {
                    !detail.evidence_ids.is_empty()
                        && detail
                            .evidence_ids
                            .iter()
                            .all(|id| a.sources.iter().any(|s| s.screenshot_id == *id))
                });
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
                    .or_insert_with(|| TaskText {
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
