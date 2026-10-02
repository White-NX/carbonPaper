//! UI projections. Generated payloads and corrections retain their storage format.
use super::{runner::privacy_fingerprint, types::*};
use crate::{credential_manager::CredentialManagerState, storage::StorageState};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tauri::Manager;

fn app_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn day_view(
    mut day: RecapDay,
    records: bool,
    attempts: bool,
    mut load_icon: impl FnMut(&[i64]) -> Result<Option<String>, String>,
) -> Result<serde_json::Value, String> {
    // Use the complete filtered directory, including records omitted from AI
    // requests. Resolve each app's icon once per read, shared across periods.
    let mut app_sources = BTreeMap::<String, Vec<i64>>::new();
    for source in day.batches.iter().flat_map(|batch| &batch.records) {
        let key = app_key(&source.process_name);
        if !key.is_empty() {
            app_sources.entry(key).or_default().push(source.id);
        }
    }
    let icons = app_sources
        .into_iter()
        .map(|(key, mut ids)| {
            // Filtered identities use block masks or bracketed PII labels.
            // An icon must not reveal the application behind those labels.
            if key.contains(['[', '█']) {
                return Ok((key, None));
            }
            ids.sort_unstable_by(|a, b| b.cmp(a));
            ids.dedup();
            load_icon(&ids).map(|icon| (key, icon))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let counts = day
        .batches
        .iter_mut()
        .map(|batch| {
            let mut apps = BTreeMap::new();
            for source in &batch.records {
                let key = app_key(&source.process_name);
                if !key.is_empty() {
                    apps.entry(key.clone()).or_insert_with(|| serde_json::json!({
                        "name": source.process_name.trim(), "icon": icons.get(&key).and_then(Option::as_ref),
                    }));
                }
            }
            let counts = (batch.records.len(), batch.attempts.len(), apps.into_values().collect::<Vec<_>>());
            if !records {
                batch.records.clear();
            }
            if !attempts {
                batch.attempts.clear();
            }
            counts
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::to_value(day).map_err(|_| "RECAP_INVALID_CACHE")?;
    if let Some(batches) = value["batches"].as_array_mut() {
        for (batch, (records, attempts, apps)) in batches.iter_mut().zip(counts) {
            batch["record_count"] = records.into();
            batch["attempt_count"] = attempts.into();
            batch["apps"] = apps.into();
        }
    }
    Ok(value)
}

pub fn read_day_view(
    app: &tauri::AppHandle,
    date: &str,
    records: bool,
    attempts: bool,
) -> Result<serde_json::Value, String> {
    let credentials = app.state::<Arc<CredentialManagerState>>();
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if crate::maintenance::is_active() {
        return Err("MAINTENANCE_IN_PROGRESS".into());
    }
    let storage = app.state::<Arc<StorageState>>();
    let generation = storage.db_generation();
    let privacy = privacy_fingerprint(app)?;
    let (start, end) = day_bounds(date)?;
    let (revision, _, _) = storage.recap_day_revision(date, start, end, &privacy)?;
    let result = day_view(super::read_day(app, date)?, records, attempts, |ids| {
        storage.recap_process_icon(ids)
    })?;
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if privacy.as_str() != privacy_fingerprint(app)?.as_str()
        || !storage.recap_revision_matches(date, revision, generation)?
    {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    Ok(result)
}

#[derive(Serialize)]
pub struct RecapRecordPage {
    items: Vec<SourceRef>,
    total: usize,
    next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    version: String,
    offset: usize,
}

fn page(
    mut records: Vec<SourceRef>,
    scope: &str,
    cursor: Option<&str>,
) -> Result<RecapRecordPage, String> {
    records.sort_by_key(|r| (r.timestamp_ms, r.id));
    records.dedup_by_key(|r| (r.timestamp_ms, r.id));
    // Include directory contents: regenerating a batch can replace its index
    // without changing the underlying screenshot revision.
    let version = digest(&format!(
        "{scope}:{}",
        serde_json::to_string(&records).map_err(|_| "RECAP_INVALID_CACHE")?
    ));
    let offset = if let Some(cursor) = cursor {
        if cursor.len() > 512 {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let cursor: Cursor =
            serde_json::from_slice(&hex::decode(cursor).map_err(|_| "RECAP_SOURCE_CHANGED")?)
                .map_err(|_| "RECAP_SOURCE_CHANGED")?;
        if cursor.version != version || cursor.offset > records.len() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        cursor.offset
    } else {
        0
    };
    let total = records.len();
    let end = offset.saturating_add(100).min(total);
    let next_cursor = if end < total {
        Some(hex::encode(
            serde_json::to_vec(&Cursor {
                version,
                offset: end,
            })
            .map_err(|_| "RECAP_INVALID_CACHE")?,
        ))
    } else {
        None
    };
    Ok(RecapRecordPage {
        items: records.into_iter().skip(offset).take(100).collect(),
        total,
        next_cursor,
    })
}

pub fn read_records(
    app: &tauri::AppHandle,
    date: &str,
    batch_start: Option<i64>,
    cursor: Option<&str>,
) -> Result<RecapRecordPage, String> {
    let credentials = app.state::<Arc<CredentialManagerState>>();
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if crate::maintenance::is_active() {
        return Err("MAINTENANCE_IN_PROGRESS".into());
    }
    let storage = app.state::<Arc<StorageState>>();
    let generation = storage.db_generation();
    let privacy = privacy_fingerprint(app)?;
    let (start, end) = day_bounds(date)?;
    let (revision, start, end) = storage.recap_day_revision(date, start, end, &privacy)?;
    let bounds = batch_bounds(start, end, chrono::Utc::now().timestamp_millis());
    if batch_start.is_some_and(|a| !bounds.iter().any(|(b, _)| a == *b)) {
        return Err("RECAP_INVALID_DATE".into());
    }
    let mut records = Vec::new();
    for (a, _) in bounds
        .into_iter()
        .filter(|(a, _)| batch_start.is_none_or(|b| *a == b))
    {
        let key = format!("{date}:{a}");
        if let Some(index) = storage.recap_read::<Vec<SourceRef>>("index", &key, Some(revision))? {
            records.extend(index);
        } else if let Some(batch) =
            storage.recap_read::<RecapBatch>("batch", &key, Some(revision))?
        {
            records.extend(batch.records);
        }
    }
    let result = page(
        records,
        &format!(
            "{date}:{batch_start:?}:{generation}:{revision}:{}",
            privacy.as_str()
        ),
        cursor,
    )?;
    if !credentials.is_session_valid() {
        return Err("AUTH_REQUIRED".into());
    }
    if privacy.as_str() != privacy_fingerprint(app)?.as_str()
        || !storage.recap_revision_matches(date, revision, generation)?
    {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn records() -> Vec<SourceRef> {
        (1..=205)
            .rev()
            .map(|id| SourceRef {
                id,
                timestamp_ms: 42,
                process_name: "Editor".into(),
                window_title: "Filtered title".into(),
            })
            .collect()
    }
    #[test]
    fn pages_identical_timestamps_without_gaps_or_duplicates() {
        let first = page(records(), "day:revision", None).unwrap();
        let second = page(records(), "day:revision", first.next_cursor.as_deref()).unwrap();
        let last = page(records(), "day:revision", second.next_cursor.as_deref()).unwrap();
        assert_eq!(first.total, 205);
        assert_eq!(first.items.len(), 100);
        assert_eq!(last.items.len(), 5);
        assert!(last.next_cursor.is_none());
        assert_eq!(
            first
                .items
                .into_iter()
                .chain(second.items)
                .chain(last.items)
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            (1..=205).collect::<Vec<_>>()
        );
    }
    #[test]
    fn rejects_changed_sources_scope_and_malformed_cursors() {
        let cursor = page(records(), "day:revision", None)
            .unwrap()
            .next_cursor
            .unwrap();
        assert!(page(records(), "other-day:revision", Some(&cursor)).is_err());
        let mut changed = records();
        changed[0].window_title = "New filtering".into();
        assert!(page(changed, "day:revision", Some(&cursor)).is_err());
        assert!(page(records(), "day:revision", Some("invalid")).is_err());
    }
    #[test]
    fn projection_keeps_counts_and_corrected_activities() {
        let batch: RecapBatch = serde_json::from_value(serde_json::json!({
            "start_ms": 0, "end_ms": 1, "updated_at_ms": 1, "status": "ready", "activities": [],
            "records": records(), "coverage": 205, "error": null, "attempts": [RecapAttempt::default()]
        })).unwrap();
        let day = RecapDay {
            date: "2026-10-01".into(),
            batches: vec![batch],
            threads: vec![],
            usage: RecapUsage::default(),
            can_undo: false,
            running: false,
            error: None,
        };
        let full = day_view(day.clone(), true, true, |_| Ok(None)).unwrap();
        let light = day_view(day, false, false, |_| Ok(None)).unwrap();
        assert_eq!(full["batches"][0]["records"].as_array().unwrap().len(), 205);
        assert_eq!(light["batches"][0]["record_count"], 205);
        assert_eq!(light["batches"][0]["attempt_count"], 1);
        assert_eq!(light["batches"][0]["records"], serde_json::json!([]));
        assert_eq!(light["batches"][0]["attempts"], serde_json::json!([]));
    }

    #[test]
    fn apps_include_uncited_records_deduplicate_across_periods_and_keep_masks() {
        let source = |id, name| {
            serde_json::json!({
                "id": id, "timestamp_ms": id, "process_name": name, "window_title": ""
            })
        };
        let batch = |start, records| {
            serde_json::json!({
                "start_ms": start, "end_ms": start + 10, "updated_at_ms": 1,
                "status": "ready", "activities": [], "records": records, "coverage": 0,
                "error": null, "attempts": []
            })
        };
        let day = RecapDay {
            date: "2026-10-01".into(),
            threads: vec![],
            usage: RecapUsage::default(),
            can_undo: false,
            running: false,
            error: None,
            batches: serde_json::from_value(serde_json::json!([
                batch(
                    0,
                    vec![
                        source(1, "Editor"),
                        source(2, " editor "),
                        source(3, "Terminal"),
                        source(4, "[censored]"),
                        source(5, " ")
                    ]
                ),
                batch(10, vec![source(11, "Editor"), source(12, "██.exe")]),
            ]))
            .unwrap(),
        };
        let mut calls = Vec::new();
        let view = day_view(day, false, false, |ids| {
            calls.push(ids.to_vec());
            Ok(ids.contains(&1).then(|| "editor-icon".to_string()))
        })
        .unwrap();
        assert_eq!(calls, vec![vec![11, 2, 1], vec![3]]);
        assert_eq!(view["batches"][0]["records"], serde_json::json!([]));
        assert_eq!(
            view["batches"][0]["apps"],
            serde_json::json!([
                {"name": "[censored]", "icon": null},
                {"name": "Editor", "icon": "editor-icon"},
                {"name": "Terminal", "icon": null},
            ])
        );
        assert_eq!(
            view["batches"][1]["apps"],
            serde_json::json!([
                {"name": "Editor", "icon": "editor-icon"}, {"name": "██.exe", "icon": null}
            ])
        );
    }
}
