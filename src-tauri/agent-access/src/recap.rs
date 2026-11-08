//! Corrected recap projections and search over every prepared date and activity.
use crate::{
    dispatcher::{clean_snapshot, cosine, query_page},
    mcp_contract,
    page::Page,
    recap_corrections,
    recap_text_contract::{self, Chunk},
    repository::AgentRepository,
    search_plan::QueryPlan,
    search_rank,
    semantic_models::{MINILM_DIMENSIONS, MINILM_MODEL_ID, MINILM_VECTOR_SPACE_REVISION},
    sensitive_filter::{filter_identity, filter_joined_text, Rejected, SensitiveFilterState},
    types::*,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};

fn validate_dates(args: &Value) -> Result<(), String> {
    for key in ["date", "start_date", "end_date"] {
        if let Some(s) = args[key].as_str() {
            if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .is_none_or(|d| d.to_string() != s)
            {
                return Err(mcp_contract::invalid_argument("Invalid calendar date"));
            }
        }
    }
    if matches!((args["start_date"].as_str(),args["end_date"].as_str()),(Some(a),Some(b)) if a>b) {
        return Err(mcp_contract::invalid_argument(
            "start_date must not be after end_date",
        ));
    }
    Ok(())
}

fn activity(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    a: &ActivityText,
    batch: &RecapContent,
) -> Result<Option<Value>, String> {
    if a.sources.is_empty() {
        return Ok(None);
    }
    for source in &a.sources {
        if repo.snapshot(source.screenshot_id)?.is_none() {
            return Ok(None);
        }
    }
    let mut known = HashSet::new();
    let mut exportable = HashSet::new();
    for source in batch.records.iter().filter(|s| s.screenshot_id > 0) {
        known.insert(source.screenshot_id);
        if let Some(snapshot) = repo.snapshot(source.screenshot_id)? {
            if clean_snapshot(snapshot, filter).is_ok()
                && filter_identity(filter, filter.mode(), &source.process_name).is_ok()
                && filter_identity(filter, filter.mode(), &source.window_title).is_ok()
            {
                exportable.insert(source.screenshot_id);
            }
        }
    }
    let clean = (|| -> Result<Value, Rejected> {
        let title = filter_identity(filter, filter.mode(), &a.task_title)?;
        let text = filter_joined_text(filter, filter.mode(), &a.text)?;
        let sources: Vec<Value> = a.sources.iter().map(|s|Ok(json!({"screenshot_id":s.screenshot_id,"captured_at_ms":s.captured_at_ms,
            "window_title":filter_identity(filter,filter.mode(),&s.window_title)?,"process_name":filter_identity(filter,filter.mode(),&s.process_name)?})))
            .collect::<Result<_,Rejected>>()?;
        let details: Vec<Value> = a
            .details
            .iter()
            .filter(|d| {
                !d.evidence_ids.is_empty()
                    && d.evidence_ids
                        .iter()
                        .all(|id| a.sources.iter().any(|s| s.screenshot_id == *id))
            })
            .filter_map(|d| {
                filter_joined_text(filter, filter.mode(), &d.text)
                    .ok()
                    .map(|text| json!({"text":text,"evidence_ids":d.evidence_ids}))
            })
            .collect();
        let mut recorded = a.member_ids.clone();
        recorded.sort_unstable();
        recorded.dedup();
        let members: Vec<i64> = recorded
            .iter()
            .copied()
            .filter(|id| {
                *id > 0
                    && (exportable.contains(id) || a.sources.iter().any(|s| s.screenshot_id == *id))
            })
            .collect();
        let membership = if recorded.is_empty() {
            "unavailable"
        } else if members.len() == recorded.len() {
            "complete"
        } else if recorded
            .iter()
            .any(|id| !known.contains(id) && !a.sources.iter().any(|s| s.screenshot_id == *id))
        {
            "incomplete"
        } else {
            "filtered"
        };
        Ok(
            json!({"activity_id":a.id,"task_id":a.task_id,"task_title":title,"text":text,"details":details,
            "start_ms":a.start_ms,"end_ms":a.end_ms,"time_semantics":"evidence_observation_span",
            "observation_kind":if a.start_ms==a.end_ms{"instant"}else{"span"},"span_ms":a.end_ms.saturating_sub(a.start_ms).max(0),
            "member_ids":members,"membership_status":membership,"batch_start_ms":batch.start_ms,"sources":sources}),
        )
    })();
    Ok(clean.ok())
}

fn load_day(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    date: &str,
    period: Option<i64>,
) -> Result<Value, String> {
    let calendar = repo
        .get::<CalendarText>("calendar", date)?
        .ok_or("RECAP_NOT_FOUND: No prepared recap for this date")?;
    let parts: Vec<RecapText> = query_page(
        repo,
        "SELECT body FROM records WHERE kind='recap' AND date=?1 AND body IS NOT NULL ORDER BY key",
        [date],
    )?;
    let current = |p: &&RecapText| p.revision == calendar.source_revision;
    let mut batches: Vec<RecapContent> = parts
        .iter()
        .filter(|p| p.part == "batch")
        .filter(current)
        .map(|p| p.content.clone())
        .collect();
    if let Some(corrections) = parts.iter().find(|p| p.part == "corrections") {
        recap_corrections::apply_corrections(&mut batches, &corrections.content);
    }
    if period.is_some_and(|start| !batches.iter().any(|b| b.start_ms == start)) {
        return Err(mcp_contract::invalid_argument("Unknown recap period"));
    }
    let mut periods = Vec::new();
    let mut items = Vec::new();
    for batch in batches
        .iter()
        .filter(|b| period.is_none_or(|start| b.start_ms == start))
    {
        let key = parts
            .iter()
            .filter(|p| p.part == "batch")
            .filter(current)
            .find(|p| p.content.start_ms == batch.start_ms)
            .map(|p| p.key.as_str());
        let overview = parts
            .iter()
            .filter(|p| p.part == "summary")
            .filter(current)
            .find(|p| Some(p.key.as_str()) == key)
            .and_then(|p| filter_joined_text(filter, filter.mode(), &p.content.overview).ok());
        periods.push(json!({"start_ms":batch.start_ms,"end_ms":batch.end_ms,"status":batch.status,"overview":overview}));
        for a in &batch.activities {
            if let Some(value) = activity(repo, filter, a, batch)? {
                items.push(value);
            }
        }
    }
    items.sort_by(|a, b| {
        a["start_ms"]
            .as_i64()
            .cmp(&b["start_ms"].as_i64())
            .then_with(|| a["activity_id"].as_str().cmp(&b["activity_id"].as_str()))
    });
    Ok(json!({"date":date,"calendar":calendar,"running":false,"periods":periods,"items":items}))
}

pub(crate) fn day(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    args: &Value,
) -> Result<Value, String> {
    validate_dates(args)?;
    let page = Page::parse(args, 10, 50)?;
    let mut result = load_day(
        repo,
        filter,
        args["date"].as_str().ok_or("RECAP_INVALID_DATE")?,
        args["batch_start_ms"].as_i64(),
    )?;
    page.complete_field(&mut result, "items");
    Ok(result)
}

pub(crate) fn days(repo: &AgentRepository, args: &Value) -> Result<Value, String> {
    validate_dates(args)?;
    let page = Page::parse(args, 30, 100)?;
    let mut calendars: Vec<CalendarText> = repo.all("calendar")?;
    calendars.retain(|c| in_date_scope(&c.date, args));
    calendars.sort_by(|a, b| b.date.cmp(&a.date));
    let mut result = page.complete(calendars.iter().map(|c| json!(c.date)).collect());
    result["calendars"] = Value::Object(
        calendars
            .into_iter()
            .filter(|c| result["items"].as_array().unwrap().contains(&json!(c.date)))
            .map(|c| (c.date.clone(), json!(c)))
            .collect(),
    );
    Ok(result)
}

fn in_date_scope(date: &str, args: &Value) -> bool {
    args["start_date"].as_str().is_none_or(|s| date >= s)
        && args["end_date"].as_str().is_none_or(|s| date <= s)
}
struct Document {
    item: Value,
    chunks: Vec<Chunk>,
}
fn coverage(documents: &[&Document], vectors: &HashMap<String, Vec<f32>>, details: bool) -> Value {
    let mut keys = HashSet::new();
    let (mut full, mut partial, mut missing) = (0, 0, 0);
    for d in documents {
        let chunks: Vec<_> = d.chunks.iter().filter(|c| details || !c.detail).collect();
        let indexed = chunks
            .iter()
            .filter(|c| vectors.contains_key(&c.key))
            .count();
        if !chunks.is_empty() {
            if indexed == 0 {
                missing += 1;
            } else if indexed == chunks.len() {
                full += 1;
            } else {
                partial += 1;
            }
        }
        keys.extend(chunks.into_iter().map(|c| c.key.as_str()));
    }
    let indexed = keys
        .iter()
        .filter(|key| vectors.contains_key(**key))
        .count();
    json!({"status":if keys.is_empty(){"empty"}else if indexed==0{"not_prepared"}else if indexed<keys.len(){"partial"}else{"ready"},
        "total_chunks":keys.len(),"indexed_chunks":indexed,"missing_chunks":keys.len()-indexed,"activities":documents.len(),
        "fully_indexed_activities":full,"partially_indexed_activities":partial,"unindexed_activities":missing})
}

pub(crate) fn search(
    repo: &AgentRepository,
    filter: &SensitiveFilterState,
    args: &Value,
    query_vector: Option<(&str, &str, &[f32])>,
) -> Result<Value, String> {
    let started = std::time::Instant::now();
    validate_dates(args)?;
    let page = Page::parse(args, 20, 50)?;
    let query = args["query"].as_str().unwrap().trim();
    if query.is_empty() {
        return Err(mcp_contract::invalid_argument("query must not be empty"));
    }
    let mode = args["mode"].as_str().unwrap_or("hybrid");
    let details = args["include_details"].as_bool().unwrap_or(true);
    let processes: Vec<String> =
        serde_json::from_value(args["process_names"].clone()).unwrap_or_default();
    let mut documents = Vec::new();
    let mut calendars = BTreeMap::new();
    let mut vectors = HashMap::new();
    for calendar in repo.all::<CalendarText>("calendar")? {
        let day = load_day(repo, filter, &calendar.date, None)?;
        for mut item in day["items"].as_array().unwrap().iter().cloned() {
            item["date"] = json!(calendar.date);
            let chunks = recap_text_contract::chunks(
                item["task_title"].as_str().unwrap(),
                item["text"].as_str().unwrap(),
                item["details"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d["text"].as_str().unwrap()),
            );
            documents.push(Document { item, chunks });
        }
        let saved: Vec<RecapText> = query_page(
            repo,
            "SELECT body FROM records WHERE kind='recap' AND date=?1 AND body IS NOT NULL",
            [&calendar.date],
        )?;
        for part in saved.into_iter().filter(|p| {
            p.part == "search"
                && p.revision == calendar.source_revision
                && p.content.version == recap_text_contract::INDEX_VERSION
                && p.content.model_revision == MINILM_VECTOR_SPACE_REVISION
        }) {
            vectors.extend(
                part.content
                    .vectors
                    .into_iter()
                    .filter(|(_, v)| v.len() == MINILM_DIMENSIONS && cosine(v, v).is_some()),
            );
        }
        calendars.insert(calendar.date.clone(), json!(calendar));
    }
    let all: Vec<&Document> = documents.iter().collect();
    let scoped: Vec<&Document> = all
        .iter()
        .copied()
        .filter(|d| {
            in_date_scope(d.item["date"].as_str().unwrap(), args)
                && (processes.is_empty()
                    || d.item["sources"].as_array().unwrap().iter().any(|s| {
                        processes
                            .iter()
                            .any(|p| s["process_name"].as_str().unwrap().eq_ignore_ascii_case(p))
                    }))
        })
        .collect();
    let global = coverage(&all, &vectors, true);
    let scope = coverage(&scoped, &vectors, details);
    let mut by_date = BTreeMap::<String, Vec<&Document>>::new();
    for d in &scoped {
        by_date
            .entry(d.item["date"].as_str().unwrap().into())
            .or_default()
            .push(d);
    }
    let mut missing_dates = Vec::new();
    let dates: Vec<Value> = by_date
        .into_iter()
        .map(|(date, docs)| {
            let mut c = coverage(&docs, &vectors, details);
            if c["missing_chunks"].as_u64().unwrap() > 0 {
                missing_dates.push(date.clone());
            }
            c["date"] = json!(date);
            c
        })
        .collect();
    let keyword_executed = mode != "semantic";
    let plan = QueryPlan::build(query);
    let mut keyword = Vec::new();
    if keyword_executed {
        for (index, d) in scoped.iter().enumerate() {
            let text = d
                .chunks
                .iter()
                .filter(|c| details || !c.detail)
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let rank = search_rank::score_text(&plan, &text);
            if rank.matched {
                keyword.push((index, rank));
            }
        }
    }
    keyword.sort_by(|a, b| {
        b.1.score
            .total_cmp(&a.1.score)
            .then_with(|| {
                scoped[b.0].item["start_ms"]
                    .as_i64()
                    .cmp(&scoped[a.0].item["start_ms"].as_i64())
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    let valid_query = query_vector.filter(|(id, revision, v)| {
        *id == MINILM_MODEL_ID
            && *revision == MINILM_VECTOR_SPACE_REVISION
            && v.len() == MINILM_DIMENSIONS
            && cosine(v, v).is_some()
    });
    let indexed = global["indexed_chunks"].as_u64().unwrap() as usize;
    let total_chunks = global["total_chunks"].as_u64().unwrap() as usize;
    let semantic_executed = mode != "keyword" && valid_query.is_some() && indexed > 0;
    let mut semantic = Vec::new();
    if semantic_executed {
        let query = valid_query.unwrap().2;
        for (index, d) in scoped.iter().enumerate() {
            if let Some(score) = d
                .chunks
                .iter()
                .filter(|c| details || !c.detail)
                .filter_map(|c| vectors.get(&c.key))
                .filter_map(|v| cosine(query, v))
                .filter(|s| *s >= 0.5)
                .max_by(f32::total_cmp)
            {
                semantic.push((index, score));
            }
        }
    }
    semantic.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let semantic_matches = semantic.len();
    let truncated = semantic_matches > 200;
    semantic.truncate(200);
    let mut fused = BTreeMap::<usize, (f64, Value)>::new();
    for (rank, (index, score)) in keyword.iter().enumerate() {
        let mut item = scoped[*index].item.clone();
        item["keyword_rank"] = json!(rank + 1);
        item["keyword_score"] = json!(score.score);
        item["semantic_rank"] = Value::Null;
        item["semantic_score"] = Value::Null;
        item["match_evidence"] = crate::text_search::evidence(score, "corrected_activity_text");
        fused.insert(*index, (1.0 / (60.0 + (rank + 1) as f64), item));
    }
    for (rank, (index, score)) in semantic.iter().enumerate() {
        let entry = fused.entry(*index).or_insert_with(|| {
            let mut item = scoped[*index].item.clone();
            item["keyword_rank"] = Value::Null;
            item["keyword_score"] = Value::Null;
            item["match_evidence"] = Value::Null;
            (0.0, item)
        });
        entry.0 += 1.0 / (60.0 + (rank + 1) as f64);
        entry.1["semantic_rank"] = json!(rank + 1);
        entry.1["semantic_score"] = json!(score);
    }
    let mut ranked: Vec<_> = fused.into_values().collect();
    ranked.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| b.1["start_ms"].as_i64().cmp(&a.1["start_ms"].as_i64()))
            .then_with(|| {
                a.1["activity_id"]
                    .as_str()
                    .cmp(&b.1["activity_id"].as_str())
            })
    });
    let rows: Vec<Value> = ranked
        .into_iter()
        .map(|(_, mut item)| {
            if !details {
                item["details"] = json!([]);
            }
            item
        })
        .collect();
    let revision = repo.revision()?;
    let ranking_revision = hex::encode(Sha256::digest(
        format!("{revision}:{}:{mode}:{}", semantic_executed, json!(rows)).as_bytes(),
    ));
    if args["ranking_revision"]
        .as_str()
        .is_some_and(|previous| previous != ranking_revision)
    {
        return Err("RECAP_SEARCH_RESULTS_CHANGED: Restart pagination at offset 0".into());
    }
    let mut result = page.complete(rows);
    let status = if mode == "keyword" {
        "not_requested"
    } else if indexed == 0 {
        "not_prepared"
    } else if valid_query.is_none() {
        "unavailable"
    } else if indexed < total_chunks {
        "partial"
    } else {
        "ready"
    };
    result["revision"] = json!(revision);
    result["ranking_revision"] = json!(ranking_revision);
    result["mode"] = json!(mode);
    result["semantic_status"] = json!(status);
    result["complete"] = json!(!truncated && (keyword_executed || semantic_executed));
    result["indexed_chunks"] = if mode == "keyword" {
        Value::Null
    } else {
        json!(indexed)
    };
    result["total_chunks"] = if mode == "keyword" {
        Value::Null
    } else {
        json!(total_chunks)
    };
    result["coverage"] = if mode == "keyword" {
        Value::Null
    } else {
        json!({"global":global,"query_scope":scope,"dates":dates,"missing_dates":missing_dates})
    };
    result["execution"] = json!({"requested_mode":mode,"keyword_executed":keyword_executed,"keyword_matches":keyword.len(),"keyword_complete":true,
        "semantic_executed":semantic_executed,"semantic_matches":semantic_matches,"semantic_candidates":semantic.len(),"semantic_candidate_limit":200,
        "semantic_candidates_truncated":truncated,"fallback_reason":if mode!="keyword"&&!semantic_executed{Some(if indexed==0{"semantic_not_prepared"}else{"semantic_unavailable"})}else{None}});
    result["index_progress"] = json!({"run_id":0,"revision":0,"processed":indexed,"indexed":indexed,"total":total_chunks,"stopping":false,"running":false,
        "total_known":true,"phase":"idle","wait_reason":null,"retry_at_ms":null,"last_progress_at_ms":null});
    result["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    result["calendars"] = Value::Object(
        calendars
            .into_iter()
            .filter(|(date, _)| {
                result["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["date"].as_str() == Some(date))
            })
            .collect(),
    );
    Ok(result)
}
