use super::search::{identity, search_inner, RecapSearchHit, RecapSearchQuery, SearchMode};
use crate::storage::{SearchResult, StorageState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use tauri::Manager;

const CANDIDATE_LIMIT: usize = 200;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UnifiedSearchQuery {
    pub query: String,
    pub process_names: Vec<String>,
    pub categories: Vec<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub include_semantic: bool,
    pub offset: usize,
    pub limit: usize,
}

impl Default for UnifiedSearchQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            process_names: vec![],
            categories: vec![],
            start_time: None,
            end_time: None,
            include_semantic: false,
            offset: 0,
            limit: 40,
        }
    }
}

impl UnifiedSearchQuery {
    fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty()
            || self.query.chars().count() > 256
            || !(1..=50).contains(&self.limit)
            || self.offset > 100_000
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        if self.start_time.is_some_and(|value| !value.is_finite())
            || self.end_time.is_some_and(|value| !value.is_finite())
            || matches!((self.start_time, self.end_time), (Some(a), Some(b)) if a > b)
        {
            return Err("RECAP_INVALID_DATE".into());
        }
        if self.process_names.len() > 100
            || self.categories.len() > 100
            || self
                .process_names
                .iter()
                .chain(&self.categories)
                .any(|value| value.len() > 1024)
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchItem {
    pub kind: &'static str,
    pub rank: usize,
    pub score: f64,
    pub snapshot: Option<SearchResult>,
    pub recap: Option<RecapSearchHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub items: Vec<UnifiedSearchItem>,
    pub total: usize,
    pub snapshot_count: usize,
    pub recap_count: usize,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
    pub revision: String,
    pub semantic_status: String,
    pub elapsed_ms: u64,
}

#[derive(Clone)]
struct Candidate {
    kind: &'static str,
    id: String,
    timestamp: i64,
    snapshot: Option<SearchResult>,
    recap: Option<RecapSearchHit>,
    score: f64,
}

pub async fn search(
    app: tauri::AppHandle,
    query: UnifiedSearchQuery,
) -> Result<UnifiedSearchResult, String> {
    let started = Instant::now();
    query.validate()?;
    let identity = identity(&app)?;
    let storage = app.state::<std::sync::Arc<StorageState>>().inner().clone();
    let search_storage = storage.clone();
    let search_query = query.clone();
    let screenshots = tokio::task::spawn_blocking(move || {
        search_storage.search_text(
            search_query.query.trim(),
            CANDIDATE_LIMIT as i32,
            0,
            true,
            (!search_query.process_names.is_empty()).then_some(search_query.process_names.clone()),
            search_query.start_time,
            search_query.end_time,
            (!search_query.categories.is_empty()).then_some(search_query.categories.clone()),
        )
    })
    .await
    .map_err(|error| format!("Search task failed: {error}"))??;

    let recap_query = RecapSearchQuery {
        query: query.query.clone(),
        mode: if query.include_semantic {
            SearchMode::Hybrid
        } else {
            SearchMode::Keyword
        },
        start_date: None,
        end_date: None,
        include_details: true,
        offset: 0,
        limit: CANDIDATE_LIMIT * 2,
        ranking_revision: None,
        process_names: query.process_names.clone(),
        start_ms: query.start_time.map(|value| (value * 1000.0) as i64),
        end_ms: query.end_time.map(|value| (value * 1000.0) as i64),
    };
    let recaps = search_inner(app.clone(), recap_query).await?;

    let candidates = fuse(screenshots, recaps.items, query.include_semantic);
    identity.check(&app)?;
    let revision = identity.token();
    let total = candidates.len();
    let snapshot_count = candidates
        .iter()
        .filter(|item| item.kind == "snapshot")
        .count();
    let recap_count = total - snapshot_count;
    let end = query.offset.saturating_add(query.limit).min(total);
    let items: Vec<_> = candidates
        .into_iter()
        .enumerate()
        .skip(query.offset)
        .take(query.limit)
        .map(|(position, candidate)| UnifiedSearchItem {
            kind: candidate.kind,
            rank: position + 1,
            score: candidate.score,
            snapshot: candidate.snapshot,
            recap: candidate.recap,
        })
        .collect();
    Ok(UnifiedSearchResult {
        items,
        total,
        offset: query.offset,
        limit: query.limit,
        snapshot_count,
        recap_count,
        next_offset: (end < total).then_some(end),
        revision,
        semantic_status: recaps.semantic_status,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn fuse(
    screenshots: Vec<SearchResult>,
    recaps: Vec<RecapSearchHit>,
    include_semantic: bool,
) -> Vec<Candidate> {
    let mut candidates = HashMap::<String, Candidate>::new();
    for (position, item) in screenshots.into_iter().take(CANDIDATE_LIMIT).enumerate() {
        let id = format!("snapshot:{}", item.screenshot_id);
        let timestamp = item.timestamp.unwrap_or_default().saturating_mul(1000);
        candidates.entry(id.clone()).or_insert_with(|| Candidate {
            kind: "snapshot",
            id,
            timestamp,
            snapshot: Some(item),
            recap: None,
            score: 1.0 / (60 + position + 1) as f64,
        });
    }
    for item in recaps {
        if item.keyword_rank.is_none_or(|rank| rank > CANDIDATE_LIMIT)
            && item.semantic_rank.is_none_or(|rank| rank > CANDIDATE_LIMIT)
        {
            continue;
        }
        let id = format!("recap:{}:{}", item.date, item.activity_id);
        let keyword = item
            .keyword_rank
            .filter(|rank| *rank <= CANDIDATE_LIMIT)
            .map(|rank| 1.0 / (60 + rank) as f64)
            .unwrap_or(0.0);
        let semantic = if include_semantic {
            item.semantic_rank
                .filter(|rank| *rank <= CANDIDATE_LIMIT)
                .map(|rank| 1.0 / (60 + rank) as f64)
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let score = keyword + semantic;
        if score == 0.0 {
            continue;
        }
        candidates.insert(
            id.clone(),
            Candidate {
                kind: "recap",
                id,
                timestamp: item.start_ms,
                snapshot: None,
                recap: Some(item),
                score,
            },
        );
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.id.cmp(&b.id))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: i64, timestamp: i64) -> SearchResult {
        SearchResult {
            id,
            screenshot_id: id,
            text: "matched text".into(),
            confidence: 1.0,
            box_coords: vec![],
            image_path: format!("memory://{id}"),
            window_title: None,
            process_name: Some("code.exe".into()),
            category: None,
            created_at: String::new(),
            screenshot_created_at: String::new(),
            timestamp: Some(timestamp),
        }
    }

    fn recap(
        id: &str,
        time: i64,
        keyword: Option<usize>,
        semantic: Option<usize>,
    ) -> RecapSearchHit {
        RecapSearchHit {
            date: "2026-10-10".into(),
            batch_start_ms: 0,
            activity_id: id.into(),
            task_id: "task".into(),
            task_title: "matched activity".into(),
            text: "details".into(),
            details: vec![],
            start_ms: time,
            end_ms: time,
            sources: vec![],
            keyword_rank: keyword,
            semantic_rank: semantic,
        }
    }

    #[test]
    fn fusion_uses_three_routes_deduplicates_snapshots_and_bounds_each_route() {
        let hits = fuse(
            vec![snapshot(1, 3), snapshot(1, 3), snapshot(2, 2)],
            vec![
                recap("both", 1, Some(2), Some(2)),
                recap("semantic", 1, None, Some(1)),
                recap("tail", 1, Some(201), Some(201)),
            ],
            true,
        );
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].id, "recap:2026-10-10:both");
        assert!((hits[0].score - 2.0 / 62.0).abs() < 1e-12);
        assert_eq!(hits[1].id, "snapshot:1");
        assert_eq!(hits[2].id, "recap:2026-10-10:semantic");
        let keywords = fuse(
            vec![snapshot(1, 3)],
            vec![recap("semantic", 1, None, Some(1))],
            false,
        );
        assert_eq!(keywords.len(), 1);
    }

    #[test]
    fn ties_use_occurrence_time_then_stable_id_independent_of_input_order() {
        let a = recap("a", 2000, Some(1), None);
        let b = recap("b", 2000, Some(1), None);
        let ids = |recaps| {
            fuse(vec![snapshot(1, 3)], recaps, false)
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(vec![b.clone(), a.clone()]), ids(vec![a, b]));
        assert_eq!(
            ids(vec![recap("a", 4000, Some(1), None)])[0],
            "recap:2026-10-10:a"
        );
    }

    #[test]
    fn request_accepts_unversioned_pages_and_rejects_invalid_times() {
        let mut query = UnifiedSearchQuery {
            query: "keyword".into(),
            ..Default::default()
        };
        assert!(query.validate().is_ok());
        query.offset = 1;
        assert!(query.validate().is_ok());
        query.start_time = Some(f64::NAN);
        assert_eq!(query.validate().unwrap_err(), "RECAP_INVALID_DATE");
        query.start_time = Some(2.0);
        query.end_time = Some(1.0);
        assert_eq!(query.validate().unwrap_err(), "RECAP_INVALID_DATE");
    }
}
