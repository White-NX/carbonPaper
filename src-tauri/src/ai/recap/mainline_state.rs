//! Accepted mainline dependencies are independent of the latest update attempt.
use super::types::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Dependency {
    pub task_id: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub input_hash: String,
    pub language: String,
    pub calendar_hash: String,
    pub updated_at_ms: i64,
    pub activities: BTreeMap<String, Dependency>,
    pub sources: BTreeMap<i64, String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Delta {
    pub added_activity_ids: Vec<String>,
    pub changed_activity_ids: Vec<String>,
    pub removed_activity_ids: Vec<String>,
    pub affected_task_ids: Vec<String>,
}

fn activity_hash(activity: &RecapActivity) -> String {
    digest(&json!(activity).to_string())
}

fn source_hash(source: &SourceRef) -> String {
    digest(&json!(source).to_string())
}

impl Snapshot {
    pub(super) fn capture(
        day: &RecapDay,
        mainlines: &RecapMainlines,
        language: &str,
        hash: &str,
        now: i64,
    ) -> Self {
        Self {
            input_hash: hash.into(),
            language: language.into(),
            calendar_hash: digest(&json!(day.calendar).to_string()),
            updated_at_ms: now,
            activities: day
                .batches
                .iter()
                .flat_map(|batch| &batch.activities)
                .map(|activity| {
                    (
                        activity.id.clone(),
                        Dependency {
                            task_id: activity.task_id.clone(),
                            hash: activity_hash(activity),
                        },
                    )
                })
                .collect(),
            sources: mainlines
                .items
                .iter()
                .flat_map(|item| &item.sources)
                .map(|source| (source.id, source_hash(source)))
                .collect(),
        }
    }

    pub(super) fn matches_scope(&self, day: &RecapDay, language: &str) -> bool {
        self.language == language && self.calendar_hash == digest(&json!(day.calendar).to_string())
    }

    pub(super) fn retain(
        &self,
        mainlines: &RecapMainlines,
        day: &RecapDay,
    ) -> Option<RecapMainlines> {
        let current = day
            .batches
            .iter()
            .flat_map(|batch| &batch.activities)
            .map(|activity| (activity.id.as_str(), activity_hash(activity)))
            .collect::<BTreeMap<_, _>>();
        let sources = day
            .batches
            .iter()
            .flat_map(|batch| &batch.records)
            .chain(
                day.batches
                    .iter()
                    .flat_map(|batch| &batch.activities)
                    .flat_map(|a| &a.sources),
            )
            .map(|source| (source.id, source_hash(source)))
            .collect::<BTreeMap<_, _>>();
        let items = mainlines
            .items
            .iter()
            .filter(|item| {
                // All previous activity dependencies of a task must still agree.
                // Appending an occurrence is safe; changing an old occurrence is not.
                !item.task_ids.is_empty()
                    && item.task_ids.iter().all(|task| {
                        let dependencies = self
                            .activities
                            .iter()
                            .filter(|(_, dep)| dep.task_id == *task)
                            .collect::<Vec<_>>();
                        !dependencies.is_empty()
                            && dependencies
                                .iter()
                                .all(|(id, dep)| current.get(id.as_str()) == Some(&dep.hash))
                    })
                    && item.activity_ids.iter().all(|id| {
                        self.activities
                            .get(id)
                            .is_some_and(|dep| current.get(id.as_str()) == Some(&dep.hash))
                            || warned(&item.warnings, "activity_ids", &json!(id))
                    })
                    && item.sources.iter().all(|source| {
                        let hash = source_hash(source);
                        self.sources.get(&source.id) == Some(&hash)
                            && sources.get(&source.id) == Some(&hash)
                    })
                    && item.evidence_ids.iter().all(|id| {
                        item.sources.iter().any(|source| source.id == *id)
                            || warned(&item.warnings, "evidence_ids", &json!(id))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        (!items.is_empty() || mainlines.items.is_empty()).then_some(RecapMainlines { items })
    }

    pub(super) fn delta(&self, day: &RecapDay) -> Delta {
        let current = day
            .batches
            .iter()
            .flat_map(|batch| &batch.activities)
            .map(|activity| (activity.id.as_str(), activity))
            .collect::<BTreeMap<_, _>>();
        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        let mut tasks = std::collections::BTreeSet::new();
        for (id, activity) in &current {
            match self.activities.get(*id) {
                None => {
                    added.push((*id).into());
                    tasks.insert(activity.task_id.clone());
                }
                Some(old) if old.hash != activity_hash(activity) => {
                    changed.push((*id).into());
                    tasks.insert(old.task_id.clone());
                    tasks.insert(activity.task_id.clone());
                }
                _ => {}
            }
        }
        for (id, dep) in &self.activities {
            if !current.contains_key(id.as_str()) {
                removed.push(id.clone());
                tasks.insert(dep.task_id.clone());
            }
        }
        Delta {
            added_activity_ids: added,
            changed_activity_ids: changed,
            removed_activity_ids: removed,
            affected_task_ids: tasks.into_iter().collect(),
        }
    }
}

fn warned(warnings: &[RecapWarning], field: &str, reference: &serde_json::Value) -> bool {
    warnings.iter().any(|warning| {
        warning.field == field
            && warning.reason == "unknown_reference"
            && warning.reference_ids.contains(reference)
    })
}

/// Only an exact copy of an accepted, still-valid entry may reuse unseen old
/// references. Updated text must pass normal received-evidence validation.
pub(super) fn same_entry(a: &RecapMainline, b: &RecapMainline) -> bool {
    let key = |item: &RecapMainline| {
        let mut tasks = item.task_ids.clone();
        tasks.sort();
        tasks.dedup();
        let mut activities = item.activity_ids.clone();
        activities.sort();
        activities.dedup();
        let mut evidence = item.evidence_ids.clone();
        evidence.sort();
        evidence.dedup();
        json!([
            item.title.trim(),
            item.text.trim(),
            tasks,
            activities,
            evidence
        ])
    };
    key(a) == key(b)
}

/// One previous ID can be reused once. Prefer the largest membership overlap
/// so appending a subtask does not break the mainline's navigation identity.
pub(super) fn reuse_ids(current: &mut RecapMainlines, previous: &RecapMainlines) {
    let mut pairs = Vec::new();
    for (next, item) in current.items.iter().enumerate() {
        for (old, before) in previous.items.iter().enumerate() {
            let overlap = item
                .task_ids
                .iter()
                .filter(|id| before.task_ids.contains(id))
                .count();
            if overlap > 0 {
                pairs.push((overlap, item.id == before.id, next, old));
            }
        }
    }
    pairs.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then_with(|| previous.items[a.3].id.cmp(&previous.items[b.3].id))
            .then(a.2.cmp(&b.2))
    });
    let mut assigned = HashSet::new();
    let mut reused = HashSet::new();
    for (_, _, next, old) in pairs {
        if !assigned.contains(&next) && !reused.contains(&old) {
            current.items[next].id = previous.items[old].id.clone();
            assigned.insert(next);
            reused.insert(old);
        }
    }
    let mut used = assigned
        .iter()
        .map(|index| current.items[*index].id.clone())
        .collect::<HashSet<_>>();
    for (index, item) in current.items.iter_mut().enumerate() {
        if assigned.contains(&index) {
            continue;
        }
        if used.contains(&item.id) {
            // A split may reclaim a canonical ID kept by an expanded mainline.
            let seed = item.id.clone();
            let mut tasks = item.task_ids.clone();
            tasks.sort();
            let mut serial = 0;
            loop {
                let id = format!(
                    "mainline-{}",
                    &digest(&format!("split\n{seed}\n{}\n{serial}", tasks.join("\n")))[..20]
                );
                if !used.contains(&id) {
                    item.id = id;
                    break;
                }
                serial += 1;
            }
        }
        used.insert(item.id.clone());
    }
}

pub(super) fn merge_partial(current: &mut RecapMainlines, previous: &RecapMainlines) {
    for item in &previous.items {
        if current.items.len() >= 8 {
            break;
        }
        if !item
            .task_ids
            .iter()
            .any(|id| current.items.iter().any(|next| next.task_ids.contains(id)))
        {
            current.items.push(item.clone());
        }
    }
}
