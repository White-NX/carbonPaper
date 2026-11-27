//! Indexed OCR candidates, verified with the desktop's plaintext ranker.
use crate::{
    dispatcher::{clean_snapshot, ocr_layout},
    mcp_contract::TimeRange,
    page::Page,
    pii,
    repository::AgentRepository,
    search_plan::QueryPlan,
    search_rank,
    sensitive_filter::{filter_segment_with_findings, SensitiveFilterState},
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(crate) fn evidence(rank: &search_rank::RankedText, scope: &str) -> Value {
    json!({"scope":scope,"relevance_score":rank.score,"matched_terms":rank.matched_terms,
        "literal_terms":rank.literal_terms,"match_type":if rank.literal{"literal"}else{"approximate"},
        "phrase_matched":rank.phrase_matched})
}

pub(crate) fn ocr(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    args: &Value,
) -> Result<Value, String> {
    let page = Page::parse(args, 20, 500)?;
    let query = args["query"].as_str().unwrap().trim();
    if query.is_empty() || query.chars().count() > 256 {
        return Err(crate::mcp_contract::invalid_argument(
            "query must contain 1 to 256 characters",
        ));
    }
    let plan = QueryPlan::build(query);
    let short = plan.keywords.iter().any(|word| word.len() < 2);
    let range = TimeRange::from_args(args, false)?;
    let processes: Vec<String> =
        serde_json::from_value(args["process_names"].clone()).unwrap_or_default();
    let categories: Vec<String> =
        serde_json::from_value(args["categories"].clone()).unwrap_or_default();
    let fuzzy = args["fuzzy"].as_bool().unwrap_or(true);
    let mut ranked = Vec::new();
    let source = repo.source()?;
    let candidates = if short {
        None
    } else {
        Some(source.candidates(&plan)?)
    };
    let mut indexed = candidates.as_ref().map(|v| v.iter());
    let mut cursor = 0;
    loop {
        // Each source query releases its SQLite read lock before text ranking.
        let ids: Vec<i64> = match indexed.as_mut() {
            Some(ids) => ids.take(256).map(i64::from).collect(),
            None => source.all_ocr_ids(cursor, 256)?,
        };
        let Some(last) = ids.last() else {
            break;
        };
        cursor = *last;
        let mut scoped = Vec::new();
        let mut snapshots = std::collections::HashMap::new();
        for id in ids {
            let Some(snapshot) = repo.ocr_snapshot(id)? else {
                continue;
            };
            if !range.contains(snapshot.captured_at_ms)
                || !processes.is_empty()
                    && !snapshot
                        .process_name
                        .as_ref()
                        .is_some_and(|v| processes.contains(v))
                || !categories.is_empty()
                    && !snapshot
                        .category
                        .as_ref()
                        .is_some_and(|v| categories.contains(v))
            {
                continue;
            }
            snapshots.insert(snapshot.screenshot_id, snapshot);
            scoped.push(id);
        }
        for row in repo.ocr_ids(&scoped)? {
            let rank = search_rank::score_text(&plan, &row.text);
            if rank.matched && (fuzzy || rank.literal) {
                let snapshot = snapshots[&row.screenshot_id].clone();
                ranked.push((row, snapshot, rank));
            }
        }
    }
    ranked.sort_by(|a, b| {
        b.2.score
            .total_cmp(&a.2.score)
            .then_with(|| b.1.captured_at_ms.cmp(&a.1.captured_at_ms))
            .then_with(|| a.0.ocr_id.cmp(&b.0.ocr_id))
    });
    // One representative OCR row per screenshot, as in the interactive tool.
    let mut seen = HashSet::new();
    ranked.retain(|r| seen.insert(r.0.screenshot_id));
    let total = ranked.len();
    let rows: Vec<_> = ranked
        .into_iter()
        .skip(page.offset)
        .take(page.limit)
        .collect();
    let mut split_cards = HashSet::new();
    if filter.filters_bank_cards() {
        let screenshots: Vec<_> = rows
            .iter()
            .filter(|(row, _, _)| pii::is_bank_card_fragment(&row.text))
            .map(|(row, _, _)| row.screenshot_id)
            .collect();
        let mut blocks_by_screenshot = HashMap::<_, Vec<_>>::new();
        for block in repo.ocr_for_screenshots(&screenshots)? {
            blocks_by_screenshot
                .entry(block.screenshot_id)
                .or_default()
                .push(block);
        }
        for blocks in blocks_by_screenshot.into_values() {
            let marked = pii::split_bank_card_blocks(&ocr_layout(&blocks));
            split_cards.extend(
                blocks
                    .into_iter()
                    .zip(marked)
                    .filter_map(|(block, marked)| marked.then_some(block.ocr_id)),
            );
        }
    }
    let mut result = page.filtered(rows, |(row, snapshot, rank)| {
        let snapshot = clean_snapshot(snapshot, filter).ok()?;
        let findings = if split_cards.contains(&row.ocr_id) {
            filter.inspect_bank_card_fragment(&row.text)
        } else {
            filter.inspect(&row.text, pii::Context::default())
        };
        let text = filter_segment_with_findings(filter, filter.mode(), &row.text, &findings).ok()??;
        let mut item = json!({"ocr_id":row.ocr_id,"screenshot_id":row.screenshot_id,"captured_at_ms":snapshot.captured_at_ms,
            "ocr_processed_at_ms":row.ocr_processed_at_ms,"text":text,"confidence":row.confidence,
            "window_title":snapshot.window_title,"process_name":snapshot.process_name,"category":snapshot.category});
        if !findings.pii.iter().any(|span| span.kind == pii::PiiKind::BankCard) {
            item["match_evidence"] = evidence(&rank, "representative_ocr_row_before_privacy_filter");
        }
        Some(item)
    });
    result["total"] = json!(total);
    result["complete"] = json!(true);
    if page.offset + page.limit >= total {
        result["next_offset"] = Value::Null;
    }
    Ok(result)
}
