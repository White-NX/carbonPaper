//! Pagination positions refer to rows before export filtering. Keeping each
//! retained row's position lets Agent compaction resume across privacy gaps.
use serde_json::{json, Value};

use crate::mcp_contract::invalid_argument;

#[derive(Clone, Copy, Debug)]
pub(super) struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl Page {
    pub fn parse(args: &Value, default: usize, max: usize) -> Result<Self, String> {
        let legacy_limit = args.get("max_records").or_else(|| args.get("page_size"));
        if args.get("limit").is_some() && legacy_limit.is_some() {
            return Err(invalid_argument(
                "Use limit without max_records or page_size",
            ));
        }
        if args.get("offset").is_some() && args.get("page").is_some() {
            return Err(invalid_argument("Use offset without page"));
        }
        let limit = args
            .get("limit")
            .or(legacy_limit)
            .map(|v| v.as_u64().ok_or_else(|| invalid_argument("Invalid limit")))
            .transpose()?
            .unwrap_or(default as u64) as usize;
        if limit == 0 || limit > max {
            return Err(invalid_argument(format!(
                "limit must be between 1 and {max}"
            )));
        }
        let offset = args
            .get("offset")
            .map(|v| v.as_u64().ok_or_else(|| invalid_argument("Invalid offset")))
            .transpose()?
            .unwrap_or(0) as usize;
        let offset = match args.get("page") {
            Some(value) => value
                .as_u64()
                .and_then(|n| n.checked_mul(limit as u64))
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| invalid_argument("Invalid page"))?,
            None => offset,
        };
        if offset > 100_000 {
            return Err(invalid_argument("offset must not exceed 100000"));
        }
        Ok(Self { offset, limit })
    }

    /// A full storage page may have a successor, including when every row is
    /// withheld. A short page is terminal. One final empty probe is harmless.
    pub fn filtered<T>(self, rows: Vec<T>, mut project: impl FnMut(T) -> Option<Value>) -> Value {
        let scanned = rows.len();
        let items: Vec<_> = rows
            .into_iter()
            .enumerate()
            .filter_map(|(position, row)| {
                let mut item = project(row)?;
                item["result_offset"] = (self.offset + position).into();
                Some(item)
            })
            .collect();
        self.exported(items, scanned)
    }

    pub fn exported(self, items: Vec<Value>, scanned: usize) -> Value {
        let mut value = json!({
            "returned_count": items.len(), "items": items,
            "offset": self.offset, "limit": self.limit,
            "next_offset": (scanned >= self.limit).then_some(self.offset + scanned),
        });
        bound_continuation(&mut value);
        value
    }

    /// Used when the complete, already privacy-filtered collection is known.
    pub fn complete(self, rows: Vec<Value>) -> Value {
        let total = rows.len();
        let items: Vec<_> = rows
            .into_iter()
            .skip(self.offset)
            .take(self.limit)
            .collect();
        let end = self.offset.saturating_add(items.len());
        let mut value = json!({
            "returned_count": items.len(), "items": items, "total": total,
            "offset": self.offset, "limit": self.limit,
            "next_offset": (end < total).then_some(end),
        });
        bound_continuation(&mut value);
        value
    }

    pub fn complete_field(self, value: &mut Value, key: &str) {
        let rows = value[key]
            .as_array_mut()
            .map(std::mem::take)
            .unwrap_or_default();
        let mut page = self.complete(rows);
        value[key] = page["items"].take();
        for key in ["offset", "limit", "total", "next_offset", "returned_count"] {
            value[key] = page[key].take();
        }
        if let Some(reason) = page.get("end_reason") {
            value["end_reason"] = reason.clone();
        }
    }
}

pub(super) fn bound_continuation(value: &mut Value) {
    if value["next_offset"]
        .as_u64()
        .is_some_and(|offset| offset > 100_000)
    {
        value["next_offset"] = Value::Null;
        value["end_reason"] = "offset_limit".into();
    }
}

pub(crate) fn list_key(value: &Value) -> Option<&'static str> {
    ["items", "ocr_results"]
        .into_iter()
        .find(|key| value[*key].is_array())
}

/// Shorten a page without changing its item shape or cutting an individual
/// record. Legacy arrays and unpaged objects cannot be safely shortened.
pub(crate) fn fit(mut value: Value, budget: usize) -> Value {
    let Some(key) = list_key(&value) else {
        return value;
    };
    let Some(offset) = value["offset"].as_u64() else {
        return value;
    };
    let original = value[key].as_array().unwrap().len();
    value["returned_count"] = original.into();
    while value.to_string().chars().count() > budget && value[key].as_array().unwrap().len() > 1 {
        let removed = value[key].as_array_mut().unwrap().pop().unwrap();
        let kept = value[key].as_array().unwrap().len();
        // The first omitted row is the next unread storage position. Offsets
        // cannot be inferred from the number of exported rows after filtering.
        value["next_offset"] = removed
            .get("result_offset")
            .and_then(Value::as_u64)
            .unwrap_or(offset + kept as u64)
            .into();
        value["returned_count"] = kept.into();
        value["truncated"] = true.into();
        value.as_object_mut().unwrap().remove("end_reason");
        if value["calendars"].is_object() {
            let dates: std::collections::HashSet<String> = value[key]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|item| item.as_str().or_else(|| item["date"].as_str()))
                .map(str::to_owned)
                .collect();
            value["calendars"]
                .as_object_mut()
                .unwrap()
                .retain(|date, _| dates.contains(date));
        }
    }
    // Large single records retain their full evidence and text. A size target
    // must never turn a valid result into a clipped JSON fragment.
    bound_continuation(&mut value);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_visits_every_visible_row_across_filtering_and_compaction() {
        let mut offset = 0;
        let mut seen = Vec::new();
        loop {
            let raw = (offset..70).take(31).collect();
            let page = Page { offset, limit: 31 }.filtered(raw, |id| {
                (id % 4 != 0).then(|| json!({"screenshot_id":id + 1,"text":"证据".repeat(120)}))
            });
            let page = fit(page, 1500);
            seen.extend(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v["screenshot_id"].as_u64().unwrap()),
            );
            match page["next_offset"].as_u64() {
                Some(next) => {
                    assert!(next > offset as u64);
                    offset = next as usize;
                }
                None => break,
            }
        }
        assert_eq!(
            seen,
            (0..70)
                .filter(|id| id % 4 != 0)
                .map(|id| id + 1)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_fully_filtered_page_still_advances_and_an_empty_probe_terminates() {
        let page = Page {
            offset: 20,
            limit: 10,
        };
        assert_eq!(page.filtered(vec![(); 10], |_| None)["next_offset"], 30);
        assert!(page.filtered(Vec::<()>::new(), |_| None)["next_offset"].is_null());
    }

    #[test]
    fn reaching_the_offset_ceiling_is_distinct_from_an_exhausted_result_set() {
        let page = Page {
            offset: 100_000,
            limit: 1,
        }
        .filtered(vec![1], |id| Some(json!({"screenshot_id":id})));
        assert!(page["next_offset"].is_null());
        assert_eq!(page["end_reason"], "offset_limit");
    }

    #[test]
    fn screenshot_details_can_continue_all_ocr_rows_without_losing_the_record() {
        let mut page = json!({"record":{"screenshot_id":42},"status":"found", "ocr_results": (0..40).map(|id| json!({"ocr_id":id,"text":"x".repeat(500)})).collect::<Vec<_>>()});
        Page {
            offset: 7,
            limit: 20,
        }
        .complete_field(&mut page, "ocr_results");
        let result = fit(page, 2000);
        let shown = result["ocr_results"].as_array().unwrap().len();
        assert_eq!(result["record"]["screenshot_id"], 42);
        assert_eq!(result["ocr_results"][0]["ocr_id"], 7);
        assert_eq!(result["next_offset"], 7 + shown);
        assert_eq!(result["total"], 40);
    }

    #[test]
    fn oversized_records_and_nested_evidence_remain_complete_json() {
        let row = json!({"text":"证据".repeat(10000),"sources":[{"screenshot_id":42}]});
        let page = fit(
            Page {
                offset: 0,
                limit: 1,
            }
            .complete(vec![row.clone()]),
            1000,
        );
        assert_eq!(page["items"][0], row);
        assert!(page["next_offset"].is_null());
        assert_eq!(fit(json!({"summary":row}), 100), json!({"summary":row}));
    }

    #[test]
    fn recap_compaction_keeps_only_the_calendars_for_retained_dates() {
        let rows: Vec<_> = (1..=30)
            .map(|n| json!({"date":format!("2026-10-{n:02}"),"text":"x".repeat(80)}))
            .collect();
        let mut page = Page {
            offset: 10,
            limit: 20,
        }
        .complete(rows);
        page["calendars"] = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["date"].as_str().unwrap().to_owned(),
                    json!({"bounds":"x".repeat(300)}),
                )
            })
            .collect::<serde_json::Map<String, Value>>()
            .into();
        page["ranking_revision"] = "stable".into();
        let page = fit(page, 1600);
        assert!(page.to_string().chars().count() <= 1600);
        assert_eq!(
            page["next_offset"],
            10 + page["items"].as_array().unwrap().len()
        );
        assert_eq!(
            page["calendars"].as_object().unwrap().len(),
            page["items"].as_array().unwrap().len()
        );
        assert_eq!(page["ranking_revision"], "stable");
    }

    #[test]
    fn legacy_limits_are_explicit_aliases_and_mixed_positions_are_rejected() {
        assert_eq!(
            Page::parse(&json!({"max_records":201,"offset":35}), 20, 500)
                .unwrap()
                .limit,
            201
        );
        assert_eq!(
            Page::parse(&json!({"page":2,"page_size":10}), 20, 200)
                .unwrap()
                .offset,
            20
        );
        for args in [
            json!({"limit":2,"max_records":2}),
            json!({"offset":2,"page":1}),
            json!({"limit":0}),
        ] {
            assert!(Page::parse(&args, 20, 500).is_err());
        }
    }
}
