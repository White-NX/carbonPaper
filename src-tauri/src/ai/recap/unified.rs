use super::search::{
    search_context, search_inner, RecapSearchHit, RecapSearchQuery, RecapSearchRuntime,
    SearchContext, SearchMode,
};
use super::search_pages::SearchSnapshot;
use crate::storage::{SearchResult, StorageState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;

const CANDIDATE_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchTarget {
    Snapshots,
    #[default]
    Mixed,
    Recaps,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UnifiedSearchQuery {
    pub query: String,
    pub process_names: Vec<String>,
    pub categories: Vec<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
    pub include_semantic: bool,
    pub target: SearchTarget,
    pub ranking_revision: Option<String>,
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
            target: SearchTarget::Mixed,
            ranking_revision: None,
            offset: 0,
            limit: 40,
        }
    }
}

impl UnifiedSearchQuery {
    fn validate(&self) -> Result<(), String> {
        if self.query.chars().count() > 256
            || !(1..=50).contains(&self.limit)
            || self.offset > 100_000
        {
            return Err("RECAP_INVALID_SEARCH".into());
        }
        if self.ranking_revision.as_ref().is_some_and(|revision| {
            revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedSearchItem {
    pub kind: String,
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
    pub ranking_revision: String,
    pub semantic_status: String,
    pub complete: bool,
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
    for _ in 0..3 {
        let result = search_page(app.clone(), query.clone(), started).await;
        if !matches!(&result, Err(error) if error == "RECAP_SOURCE_CHANGED") {
            return result;
        }
        tokio::task::yield_now().await;
    }
    let context = search_context(&app)?;
    store_page(
        &app,
        &query,
        &context,
        UnifiedSearchResult {
            items: vec![],
            total: 0,
            snapshot_count: 0,
            recap_count: 0,
            offset: 0,
            limit: query.limit,
            next_offset: None,
            revision: context.token(),
            ranking_revision: String::new(),
            semantic_status: "not_prepared".into(),
            complete: false,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        started,
    )
}

fn query_key(query: &UnifiedSearchQuery) -> String {
    super::digest(
        &serde_json::json!([
            query.query.trim(),
            query.target,
            query.include_semantic,
            query.process_names,
            query.categories,
            query.start_time,
            query.end_time,
        ])
        .to_string(),
    )
}

async fn search_page(
    app: tauri::AppHandle,
    query: UnifiedSearchQuery,
    started: Instant,
) -> Result<UnifiedSearchResult, String> {
    let context = search_context(&app)?;
    let cached = app
        .state::<RecapSearchRuntime>()
        .unified_pages
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .find(
            &context,
            &query_key(&query),
            query.ranking_revision.as_deref(),
            query.offset,
        );
    if let Some(cached) = cached {
        return read_page(&app, &query, &context, cached, query.offset, started);
    }
    let result = collect_search(app.clone(), query.clone()).await?;
    store_page(&app, &query, &context, result, started)
}

fn store_page(
    app: &tauri::AppHandle,
    query: &UnifiedSearchQuery,
    context: &SearchContext,
    mut result: UnifiedSearchResult,
    started: Instant,
) -> Result<UnifiedSearchResult, String> {
    context.check(app)?;
    let items = std::mem::take(&mut result.items);
    let snapshot = app
        .state::<RecapSearchRuntime>()
        .unified_pages
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(context.clone(), query_key(query), result, &items)?;
    let offset = if query.ranking_revision.is_some() {
        0
    } else {
        query.offset
    };
    read_page(app, query, context, snapshot, offset, started)
}

fn read_page(
    app: &tauri::AppHandle,
    query: &UnifiedSearchQuery,
    context: &SearchContext,
    snapshot: Arc<SearchSnapshot<UnifiedSearchResult>>,
    offset: usize,
    started: Instant,
) -> Result<UnifiedSearchResult, String> {
    context.check(app)?;
    let mut result = snapshot.header.clone();
    result.items = snapshot.items()?;
    let storage = app.state::<Arc<StorageState>>();
    let rows: Vec<_> = result
        .items
        .iter()
        .filter_map(|item| item.snapshot.as_ref())
        .map(|row| (row.id, row.screenshot_id))
        .collect();
    let days: Vec<_> = result
        .items
        .iter()
        .filter_map(|item| item.recap.as_ref())
        .map(|hit| hit.date.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let snapshots = storage.search_visible_snapshots(&rows)?;
    let recaps = storage.search_visible_recap_days(&days)?;
    let end = offset.saturating_add(query.limit).min(result.total);
    result.items = result
        .items
        .into_iter()
        .skip(offset)
        .take(query.limit)
        .filter(|item| {
            item.snapshot
                .as_ref()
                .is_some_and(|row| snapshots.contains(&row.id))
                || item
                    .recap
                    .as_ref()
                    .is_some_and(|hit| recaps.contains(&hit.date))
        })
        .collect();
    result.offset = offset;
    result.limit = query.limit;
    result.next_offset = (end < result.total).then_some(end);
    result.ranking_revision = snapshot.token.clone();
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    context.check(app)?;
    Ok(result)
}

async fn collect_search(
    app: tauri::AppHandle,
    query: UnifiedSearchQuery,
) -> Result<UnifiedSearchResult, String> {
    let started = Instant::now();
    let context = search_context(&app)?;
    let storage = app.state::<std::sync::Arc<StorageState>>().inner().clone();
    let search_storage = storage.clone();
    let search_query = query.clone();
    let (screenshots, snapshot_complete) = if query.target != SearchTarget::Recaps {
        tokio::task::spawn_blocking(move || {
            search_storage.search_text_with_coverage(
                search_query.query.trim(),
                CANDIDATE_LIMIT as i32,
                0,
                true,
                (!search_query.process_names.is_empty())
                    .then_some(search_query.process_names.clone()),
                search_query.start_time,
                search_query.end_time,
                (!search_query.categories.is_empty()).then_some(search_query.categories.clone()),
            )
        })
        .await
        .map_err(|error| format!("Search task failed: {error}"))??
    } else {
        (vec![], true)
    };

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
    let (recap_items, semantic_status, complete) = if query.target != SearchTarget::Snapshots {
        let recaps = search_inner(app.clone(), recap_query).await?;
        let complete = recaps.complete && recaps.execution.keyword_matches <= CANDIDATE_LIMIT;
        (recaps.items, recaps.semantic_status, complete)
    } else {
        (vec![], "disabled".into(), true)
    };
    let mut candidates = fuse(
        screenshots,
        recap_items,
        query.include_semantic && query.target != SearchTarget::Snapshots,
    );
    if query.query.trim().is_empty() {
        candidates.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    }
    context.check(&app)?;
    let revision = context.token();
    let total = candidates.len();
    let snapshot_count = candidates
        .iter()
        .filter(|item| item.kind == "snapshot")
        .count();
    let recap_count = total - snapshot_count;
    let items: Vec<_> = candidates
        .into_iter()
        .enumerate()
        .map(|(position, candidate)| UnifiedSearchItem {
            kind: candidate.kind.into(),
            rank: position + 1,
            score: candidate.score,
            snapshot: candidate.snapshot,
            recap: candidate.recap,
        })
        .collect();
    Ok(UnifiedSearchResult {
        items,
        total,
        offset: 0,
        limit: query.limit,
        snapshot_count,
        recap_count,
        next_offset: None,
        revision,
        ranking_revision: String::new(),
        semantic_status,
        complete: complete && snapshot_complete,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Lexical rows share the plaintext scorer's scale. Only explicit vector
/// retrieval adds a second ranking channel, fused against the combined lexical list.
fn fuse(
    screenshots: Vec<SearchResult>,
    recaps: Vec<RecapSearchHit>,
    include_semantic: bool,
) -> Vec<Candidate> {
    let mut candidates = HashMap::<String, Candidate>::new();
    for item in screenshots.into_iter().take(CANDIDATE_LIMIT) {
        let id = format!("snapshot:{}", item.screenshot_id);
        let timestamp = item.timestamp.unwrap_or_default().saturating_mul(1000);
        let score = item
            .match_evidence
            .as_ref()
            .map(|e| e.relevance_score)
            .unwrap_or_default();
        let candidate = Candidate {
            kind: "snapshot",
            id: id.clone(),
            timestamp,
            snapshot: Some(item),
            recap: None,
            score,
        };
        if candidates.get(&id).is_none_or(|old| old.score < score) {
            candidates.insert(id, candidate);
        }
    }
    for item in recaps {
        if item.keyword_rank.is_none_or(|rank| rank > CANDIDATE_LIMIT)
            && (!include_semantic || item.semantic_rank.is_none_or(|rank| rank > CANDIDATE_LIMIT))
        {
            continue;
        }
        let id = format!("recap:{}:{}", item.date, item.activity_id);
        let score = item.keyword_score.unwrap_or_default();
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
    let tie =
        |a: &Candidate, b: &Candidate| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id));
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| tie(a, b)));
    if include_semantic {
        let mut lexical_rank = 0;
        for candidate in &mut candidates {
            let has_keyword = candidate.snapshot.is_some()
                || candidate
                    .recap
                    .as_ref()
                    .is_some_and(|r| r.keyword_rank.is_some());
            let lexical = if has_keyword {
                lexical_rank += 1;
                1.0 / (60 + lexical_rank) as f64
            } else {
                0.0
            };
            let semantic = candidate
                .recap
                .as_ref()
                .and_then(|r| r.semantic_rank)
                .filter(|rank| *rank <= CANDIDATE_LIMIT)
                .map(|rank| 1.0 / (60 + rank) as f64)
                .unwrap_or_default();
            candidate.score = lexical + semantic;
        }
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| tie(a, b)));
    }
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
            match_evidence: Some(crate::storage::SearchMatchEvidence {
                scope: "ocr_block".into(),
                relevance_score: 1000.0,
                matched_terms: vec![],
                literal_terms: vec![],
                match_type: "literal".into(),
                phrase_matched: true,
                matched_bigram_groups: None,
                total_bigram_groups: None,
            }),
        }
    }

    fn recap(
        id: &str,
        time: i64,
        keyword: Option<usize>,
        semantic: Option<usize>,
    ) -> RecapSearchHit {
        RecapSearchHit {
            result_offset: None,
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
            evidence: super::super::activity_contract::ActivityEvidence {
                time_semantics: "evidence_observation_span".into(),
                observation_kind: "instant".into(),
                observed_span_ms: 0,
                member_screenshot_ids: vec![],
                membership_status: "unavailable".into(),
            },
            keyword_rank: keyword,
            semantic_rank: semantic,
            keyword_score: keyword.map(|_| 1000.0),
            semantic_score: semantic.map(|_| 0.8),
            match_evidence: None,
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
        assert!((hits[0].score - (1.0 / 63.0 + 1.0 / 62.0)).abs() < 1e-12);
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
    fn mixed_keywords_compare_shared_text_scores_instead_of_separate_route_ranks() {
        let mut strong = recap("strong", 1, Some(2), None);
        strong.keyword_score = Some(1500.0);
        let mut weak = recap("weak", 5000, Some(1), None);
        weak.keyword_score = Some(500.0);
        let rows = fuse(vec![snapshot(1, 3)], vec![weak, strong], false);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec![
                "recap:2026-10-10:strong",
                "snapshot:1",
                "recap:2026-10-10:weak"
            ]
        );
        assert_eq!(rows[0].score, 1500.0);
    }

    #[test]
    fn targets_are_validated_and_vector_search_defaults_off() {
        let query: UnifiedSearchQuery =
            serde_json::from_value(serde_json::json!({"query": "notes"})).unwrap();
        assert_eq!(query.target, SearchTarget::Mixed);
        assert!(!query.include_semantic);
        for target in ["snapshots", "mixed", "recaps"] {
            assert!(serde_json::from_value::<UnifiedSearchQuery>(
                serde_json::json!({"target": target})
            )
            .is_ok());
        }
        assert!(serde_json::from_value::<UnifiedSearchQuery>(
            serde_json::json!({"target": "everything"})
        )
        .is_err());
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
