//! Indexed OCR candidates, verified with the desktop's plaintext ranker.
use crate::{
    dispatcher::{clean_snapshot, query_page},
    mcp_contract::TimeRange,
    page::Page,
    pii,
    repository::AgentRepository,
    search_plan::QueryPlan,
    search_rank,
    sensitive_filter::{filter_segment, SensitiveFilterState},
    types::OcrText,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashSet;

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
    let terms: HashSet<String> = plan
        .phrase_groups
        .iter()
        .map(|g| crate::search_plan::fold(&g.bigram).iter().collect())
        .collect();
    let short = plan.keywords.iter().any(|word| word.len() < 2);
    let range = TimeRange::from_args(args, false)?;
    let list = |key: &str| {
        args.get(key)
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
            .map(|v| json!(v).to_string())
    };
    let rows: Vec<OcrText> = query_page(repo,
        "SELECT o.body FROM records o JOIN records s ON s.kind='snapshot' AND s.key=CAST(o.screenshot_id AS TEXT)
        WHERE o.kind='ocr' AND o.body IS NOT NULL AND s.body IS NOT NULL
        AND (?1=1 OR o.key IN (SELECT key FROM text_terms WHERE kind='ocr' AND term IN (SELECT value FROM json_each(?2))))
        AND (?3 IS NULL OR s.captured_at_ms>=?3) AND (?4 IS NULL OR s.captured_at_ms<?4)
        AND (?5 IS NULL OR json_extract(s.body,'$.process_name') IN (SELECT value FROM json_each(?5)))
        AND (?6 IS NULL OR json_extract(s.body,'$.category') IN (SELECT value FROM json_each(?6)))",
        params![short,json!(terms).to_string(),range.start_ms,range.end_ms,list("process_names"),list("categories")])?;
    let fuzzy = args["fuzzy"].as_bool().unwrap_or(true);
    let mut ranked = Vec::new();
    for row in rows {
        let rank = search_rank::score_text(&plan, &row.text);
        if rank.matched && (fuzzy || rank.literal) {
            let snapshot = repo
                .snapshot(row.screenshot_id)?
                .ok_or("AGENT_INVALID_RECORD")?;
            ranked.push((row, snapshot, rank));
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
    let rows = ranked
        .into_iter()
        .skip(page.offset)
        .take(page.limit)
        .collect();
    let mut result = page.filtered(rows,|(mut row,snapshot,rank)|{
        let snapshot = clean_snapshot(snapshot,filter).ok()?;
        row.text = filter_segment(filter,filter.mode(),&row.text,pii::Context::default()).ok()??;
        Some(json!({"ocr_id":row.ocr_id,"screenshot_id":row.screenshot_id,"captured_at_ms":snapshot.captured_at_ms,
            "ocr_processed_at_ms":row.ocr_processed_at_ms,"text":row.text,"confidence":row.confidence,
            "window_title":snapshot.window_title,"process_name":snapshot.process_name,"category":snapshot.category,
            "match_evidence":evidence(&rank,"representative_ocr_row_before_privacy_filter")}))
    });
    result["total"] = json!(total);
    result["complete"] = json!(true);
    if page.offset + page.limit >= total {
        result["next_offset"] = Value::Null;
    }
    Ok(result)
}
