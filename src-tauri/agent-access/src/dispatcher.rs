//! A fixed read-only catalog over the independent text repository.
use crate::{
    mcp_contract::{self, TimeRange},
    page::Page,
    pii,
    repository::AgentRepository,
    sensitive_filter::{
        filter_identity, filter_joined_text, filter_segment, filter_url, Rejected,
        SensitiveFilterState,
    },
    types::*,
};
use rusqlite::params;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

pub fn tools_list() -> Value {
    let mut tools = mcp_contract::tool_definitions();
    tools.as_array_mut().unwrap().retain(|tool| {
        !matches!(
            tool["name"].as_str(),
            Some("upsert_smart_cluster_summary" | "delete_smart_cluster_summary")
        )
    });
    for tool in tools.as_array_mut().unwrap() {
        if tool["name"] == "search_recaps" {
            tool["description"] = json!("Search all prepared, corrected recap activities by keyword, semantic meaning, or hybrid. Keywords are verified against saved corrected text. Semantic search reads existing stored MiniLM vectors and never generates embeddings for history. Date filters are inclusive and use saved calendars. coverage reports global, scoped and per-date chunk coverage; keyword-only coverage is null. execution reports branch availability and the semantic top-200 cap. Pass ranking_revision when continuing a page. Sources use screenshot_id and captured_at_ms in Unix milliseconds.");
        } else if tool["name"] == "get_recap_days" {
            tool["description"] = json!("List all prepared non-deleted daily recap dates, newest first, with offset/limit and saved calendars[date]. A listed date may have pending periods. Does not generate recaps.");
        }
    }
    json!({"tools":tools})
}
fn decode<T: DeserializeOwned>(rows: Vec<String>) -> Result<Vec<T>, String> {
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(|_| "AGENT_INVALID_RECORD".into()))
        .collect()
}
fn id(args: &Value) -> i64 {
    args.get("cluster_id")
        .or_else(|| args.get("smart_cluster_id"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}
pub(crate) fn clean_snapshot(
    mut v: SnapshotText,
    filter: &SensitiveFilterState,
) -> Result<SnapshotText, Rejected> {
    let mode = filter.mode();
    if let Some(text) = v.window_title.take() {
        v.window_title = Some(filter_identity(filter, mode, &text)?);
    }
    if let Some(text) = v.process_name.take() {
        v.process_name = Some(filter_identity(filter, mode, &text)?);
    }
    if let Some(text) = v.page_url.take() {
        v.page_url = Some(filter_url(filter, mode, &text)?);
    }
    let mut links = Vec::new();
    for link in v.visible_links {
        if let Some(text) = filter_segment(filter, mode, &link.text, pii::Context::default())? {
            links.push(TextLink {
                text,
                url: filter_url(filter, mode, &link.url)?,
            });
        }
    }
    v.visible_links = links;
    Ok(v)
}
fn metadata(v: &SnapshotText) -> Value {
    json!({"screenshot_id":v.screenshot_id,"captured_at_ms":v.captured_at_ms,
    "window_title":v.window_title,"process_name":v.process_name,"source":v.source,"page_url":v.page_url,"category":v.category})
}
fn clean_blocks(
    blocks: Vec<OcrText>,
    filter: &SensitiveFilterState,
) -> Result<Vec<OcrText>, Rejected> {
    let layout: Vec<_> = blocks
        .iter()
        .map(|v| (v.text.as_str(), pii::Bounds::from_points(&v.box_coords)))
        .collect();
    let contexts = pii::block_contexts(&layout);
    let mut kept = Vec::new();
    for (mut block, context) in blocks.into_iter().zip(contexts) {
        if let Some(text) = filter_segment(filter, filter.mode(), &block.text, context)? {
            block.text = text;
            kept.push(block);
        }
    }
    Ok(kept)
}
fn clean_summary(mut v: SummaryText, filter: &SensitiveFilterState) -> Option<SummaryText> {
    for slot in [&mut v.title, &mut v.summary, &mut v.ocr_summary] {
        if let Some(text) = slot.take() {
            *slot = Some(filter_joined_text(filter, filter.mode(), &text).ok()?);
        }
    }
    v.key_points = v
        .key_points
        .into_iter()
        .filter_map(|s| filter_joined_text(filter, filter.mode(), &s).ok())
        .collect();
    v.evidence = v
        .evidence
        .into_iter()
        .filter_map(|mut e| {
            for slot in [&mut e.text, &mut e.window_title, &mut e.process_name] {
                if let Some(text) = slot.take() {
                    *slot = Some(filter_joined_text(filter, filter.mode(), &text).ok()?);
                }
            }
            Some(e)
        })
        .collect();
    Some(v)
}
pub(crate) fn query_page<T: DeserializeOwned>(
    repo: &AgentRepository,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<T>, String> {
    let mut statement = repo
        .conn
        .prepare(sql)
        .map_err(|e| format!("AGENT_STORAGE_FAILED: {e}"))?;
    let rows = statement
        .query_map(parameters, |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let rows = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    decode(rows)
}

pub fn call(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    name: &str,
    args: Value,
    query_vector: Option<(&str, &str, &[f32])>,
) -> Result<Value, String> {
    if matches!(
        name,
        "upsert_smart_cluster_summary" | "delete_smart_cluster_summary"
    ) {
        return Err("READ_ONLY_GRANT: Unattended credentials authorize text reads only".into());
    }
    mcp_contract::validate_arguments(name, &args)?;
    if repo.has_gaps()? {
        return Err("AGENT_SYNC_INCOMPLETE: Unlock CarbonPaper to repair the text view".into());
    }
    let mode = filter.mode();
    match name {
        "get_snapshots_by_time_range" => {
            let page = Page::parse(&args, 500, 500)?;
            let range = TimeRange::from_args(&args, true)?;
            let rows:Vec<SnapshotText>=query_page(repo,"SELECT body FROM records WHERE kind='snapshot' AND body IS NOT NULL
                AND captured_at_ms>=?1 AND captured_at_ms<?2 ORDER BY captured_at_ms DESC,CAST(key AS INTEGER) DESC LIMIT ?3 OFFSET ?4",
                params![range.start_ms,range.end_ms,page.limit as i64,page.offset as i64])?;
            Ok(page.filtered(rows, |v| {
                clean_snapshot(v, filter).ok().map(|v| metadata(&v))
            }))
        }
        "get_snapshot_details" => {
            let screenshot_id = mcp_contract::screenshot_id(&args)?;
            let page = Page::parse(&args, 100, 500)?;
            let raw = repo.snapshot(screenshot_id)?.ok_or_else(|| {
                format!("SNAPSHOT_NOT_FOUND: Screenshot {screenshot_id} was not found")
            })?;
            let snapshot = clean_snapshot(raw, filter)
                .map_err(|_| "PRIVACY_REJECTED: Rejected by user's privacy settings")?;
            let blocks = clean_blocks(repo.ocr(screenshot_id)?, filter)
                .map_err(|_| "PRIVACY_REJECTED: Rejected by user's privacy settings")?;
            let coords = args["include_coords"].as_bool().unwrap_or(false);
            let mut record = metadata(&snapshot);
            record["width"] = json!(snapshot.width);
            record["height"] = json!(snapshot.height);
            record["visible_links"] = json!(snapshot.visible_links);
            let rows=blocks.into_iter().map(|v|{let mut row=json!({"ocr_id":v.ocr_id,"text":v.text,"confidence":v.confidence,"ocr_processed_at_ms":v.ocr_processed_at_ms});if coords {row["box_coords"]=json!(v.box_coords);}row}).collect::<Vec<_>>();
            let mut result = json!({"status":"found","record":record,"ocr_results":rows});
            page.complete_field(&mut result, "ocr_results");
            Ok(result)
        }
        "search_ocr_text" => crate::text_search::ocr(repo, filter, &args),
        "search_nl" => {
            let page = Page::parse(&args, 20, 500)?;
            let range = TimeRange::from_args(&args, false)?;
            let Some((model, revision, query)) = query_vector else {
                return Err("AGENT_INDEX_UNAVAILABLE: Query model or prepared visual embeddings are unavailable".into());
            };
            let processes: Vec<String> =
                serde_json::from_value(args["process_names"].clone()).unwrap_or_default();
            let mut scores = Vec::new();
            let mut indexed = 0;
            let mut cursor = 0;
            let total = repo.snapshot_count()?;
            loop {
                let vectors = repo.vector_page("clip_image", cursor, 256)?;
                let Some(last) = vectors.last() else {
                    break;
                };
                cursor = last.screenshot_id;
                for v in vectors {
                    if v.model_id != model
                        || v.model_revision != revision
                        || cosine(query, &v.vector).is_none()
                    {
                        continue;
                    }
                    indexed += 1;
                    let Some(snapshot) = repo.snapshot(v.screenshot_id)? else {
                        continue;
                    };
                    if !range.contains(snapshot.captured_at_ms)
                        || !processes.is_empty()
                            && !snapshot
                                .process_name
                                .as_ref()
                                .is_some_and(|p| processes.contains(p))
                    {
                        continue;
                    }
                    if let Some(score) = cosine(query, &v.vector) {
                        scores.push((score, snapshot));
                    }
                }
            }
            if indexed == 0 {
                return Err(
                    "AGENT_INDEX_UNAVAILABLE: No matching visual embeddings have been prepared"
                        .into(),
                );
            }
            scores.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| b.1.screenshot_id.cmp(&a.1.screenshot_id))
            });
            let rows = scores
                .into_iter()
                .skip(page.offset)
                .take(page.limit)
                .collect();
            let mut error = None;
            let mut result=page.filtered(rows,|(score,v)| {
                let v=clean_snapshot(v,filter).ok()?;
                let blocks=match repo.ocr(v.screenshot_id) {Ok(b)=>b,Err(e)=>{error=Some(e);return None}};
                let blocks=clean_blocks(blocks,filter).ok()?;
                Some(json!({"screenshot_id":v.screenshot_id,"captured_at_ms":v.captured_at_ms,"window_title":v.window_title,"process_name":v.process_name,
                    "ocr_text":blocks.iter().map(|b|b.text.as_str()).collect::<Vec<_>>().join(" "),"similarity":score}))
            });
            result["index_status"] = json!(if indexed < total { "partial" } else { "ready" });
            result["indexed_snapshots"] = json!(indexed);
            result["total_snapshots"] = json!(total);
            if let Some(e) = error {
                Err(e)
            } else {
                Ok(result)
            }
        }
        "get_smart_clusters" => {
            let page = Page::parse(&args, 50, 200)?;
            let rows: Vec<ClusterText> = repo.all("cluster")?;
            let mut values = Vec::new();
            for mut v in rows {
                let assignments: Vec<AssignmentText> = query_page(repo,
                    "SELECT a.body FROM records a JOIN records s ON s.kind='snapshot' AND s.key=CAST(a.screenshot_id AS TEXT)
                    WHERE a.kind='assignment' AND a.cluster_id=?1 AND a.body IS NOT NULL AND s.body IS NOT NULL",[v.cluster_id])?;
                v.snapshot_count = assignments.len() as i64;
                v.last_assigned_at_ms = assignments.iter().filter_map(|a| a.assigned_at_ms).max();
                let Some(anchor) = filter_joined_text(filter, mode, &v.anchor_text).ok() else {
                    continue;
                };
                v.anchor_text = anchor;
                if let Some(name) = v.display_name.take() {
                    let Ok(name) = filter_identity(filter, mode, &name) else {
                        continue;
                    };
                    v.display_name = Some(name);
                }
                let mut value = json!(v);
                value["summary"] = json!(repo
                    .get::<SummaryText>("summary", &v.cluster_id.to_string())?
                    .and_then(|s| clean_summary(s, filter)));
                values.push(value);
            }
            Ok(page.complete(values))
        }
        "get_smart_cluster_summary" => Ok(
            json!({"cluster_id":id(&args),"summary":repo.get::<SummaryText>("summary",&id(&args).to_string())?.and_then(|v|clean_summary(v,filter))}),
        ),
        "get_smart_cluster_ocr_corpus" => {
            let page = Page::parse(&args, 50, 200)?;
            let cluster_id = id(&args);
            let rows:Vec<AssignmentText>=query_page(repo,"SELECT a.body FROM records a JOIN records s ON s.kind='snapshot' AND s.key=CAST(a.screenshot_id AS TEXT)
                WHERE a.kind='assignment' AND a.cluster_id=?1 AND a.body IS NOT NULL AND s.body IS NOT NULL
                ORDER BY COALESCE(json_extract(a.body,'$.rerank_score'),-1) DESC,a.screenshot_id DESC LIMIT ?2 OFFSET ?3",
                params![cluster_id,page.limit as i64,page.offset as i64])?;
            let mut error = None;
            let mut result=page.filtered(rows,|a| {
                let raw=match repo.snapshot(a.screenshot_id) {Ok(Some(v))=>v,Ok(None)=>return None,Err(e)=>{error=Some(e);return None}};
                let s=clean_snapshot(raw,filter).ok()?;let raw=match repo.ocr(a.screenshot_id) {Ok(v)=>v,Err(e)=>{error=Some(e);return None}};
                let blocks=clean_blocks(raw,filter).ok()?;let text=blocks.iter().map(|b|b.text.as_str()).collect::<Vec<_>>().join(" ");
                if text.is_empty() && !args["include_empty_ocr"].as_bool().unwrap_or(false) {return None;}
                Some(json!({"screenshot_id":s.screenshot_id,"captured_at_ms":s.captured_at_ms,"assigned_at_ms":a.assigned_at_ms,
                    "window_title":s.window_title,"process_name":s.process_name,"category":s.category,"rerank_score":a.rerank_score,"ocr_text":text}))
            });
            result["cluster_id"] = json!(cluster_id);
            if let Some(e) = error {
                Err(e)
            } else {
                Ok(result)
            }
        }
        "get_recap_days" => crate::recap::days(repo, &args),
        "get_recap_day" => crate::recap::day(repo, filter, &args),
        "search_recaps" => crate::recap::search(repo, filter, &args, query_vector),
        _ => Err(format!("TOOL_NOT_FOUND: {name}")),
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (a, b) in a.iter().zip(b) {
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
        dot += a * b;
        aa += a * a;
        bb += b * b;
    }
    (aa > 0.0 && bb > 0.0).then(|| dot / (aa.sqrt() * bb.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_excludes_writes_and_binary_capabilities_and_is_fixed() {
        let tools = tools_list();
        let tools = tools["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 10);
        assert!(tools.iter().all(|t| !matches!(
            t["name"].as_str(),
            Some("upsert_smart_cluster_summary" | "delete_smart_cluster_summary")
        )));
        assert!(!tools
            .iter()
            .any(|t| t["name"].as_str().unwrap().contains("image")));
    }
}
