//! Public screenshot projections. Storage retains its existing seconds/UTC
//! contracts; MCP and the Agent only receive explicitly named milliseconds.
use serde_json::{json, Value};

use crate::ai::recap::SourceRef;
use crate::storage::smart_cluster::SmartClusterOcrCorpusItem;
use crate::storage::{wire_time, OcrResult, ScreenshotRecord, SearchResult};

pub(super) fn millis(seconds: Option<i64>, utc: &str) -> Option<i64> {
    seconds
        .or_else(|| wire_time::sqlite_utc_to_seconds(utc))?
        .checked_mul(1000)
}

pub(super) fn screenshot(record: &ScreenshotRecord) -> Value {
    json!({
        "screenshot_id": record.id,
        "captured_at_ms": millis(record.timestamp, &record.created_at),
        "window_title": record.window_title,
        "process_name": record.process_name,
        "source": record.source,
        "page_url": record.page_url,
        "category": record.category,
    })
}

pub(super) fn ocr_hit(hit: &SearchResult) -> Value {
    json!({
        "ocr_id": hit.id,
        "screenshot_id": hit.screenshot_id,
        "captured_at_ms": millis(hit.timestamp, &hit.screenshot_created_at),
        "ocr_processed_at_ms": millis(None, &hit.created_at),
        "text": hit.text,
        "confidence": hit.confidence,
        "window_title": hit.window_title,
        "process_name": hit.process_name,
        "category": hit.category,
    })
}

pub(super) fn details(record: &ScreenshotRecord, ocr: &[OcrResult], include_coords: bool) -> Value {
    let mut record_value = screenshot(record);
    record_value["width"] = json!(record.width);
    record_value["height"] = json!(record.height);
    record_value["visible_links"] = json!(record.visible_links);
    let rows: Vec<_> = ocr
        .iter()
        .map(|row| {
            let mut value = json!({
                "ocr_id": row.id,
                "text": row.text,
                "confidence": row.confidence,
                "ocr_processed_at_ms": millis(None, &row.created_at),
            });
            if include_coords {
                value["box_coords"] = json!(row.box_coords);
            }
            value
        })
        .collect();
    json!({"status": "found", "record": record_value, "ocr_results": rows})
}

pub(super) fn recap_source(source: &SourceRef) -> Value {
    json!({
        "screenshot_id": source.id,
        "captured_at_ms": source.timestamp_ms,
        "window_title": source.window_title,
        "process_name": source.process_name,
    })
}

pub(super) fn corpus_item(item: &SmartClusterOcrCorpusItem) -> Value {
    json!({
        "screenshot_id": item.screenshot_id,
        "captured_at_ms": millis(None, &item.created_at),
        "assigned_at_ms": millis(None, &item.assigned_at),
        "window_title": item.window_title,
        "process_name": item.process_name,
        "category": item.category,
        "rerank_score": item.rerank_score,
        "ocr_text": item.ocr_text,
    })
}

pub(super) fn visual_hit(item: &Value, screenshot_id: i64) -> Value {
    let metadata = &item["metadata"];
    let captured = item["screenshot_created_at"]
        .as_str()
        .or_else(|| metadata["screenshot_created_at"].as_str())
        .unwrap_or_default();
    json!({
        "screenshot_id": screenshot_id,
        "captured_at_ms": millis(None, captured),
        "window_title": metadata.get("window_title").or_else(|| item.get("window_title")),
        "process_name": metadata.get("process_name").or_else(|| item.get("process_name")),
        "ocr_text": item.get("ocr_text"),
        "similarity": item.get("similarity"),
    })
}

/// Only typed archive metadata is rewritten. User-authored evidence JSON is
/// opaque content and must not have its keys or meaning changed recursively.
pub(super) fn archive(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        for key in ["id", "smart_cluster_id"] {
            if let Some(id) = map.remove(key) {
                map.insert("cluster_id".into(), id);
            }
        }
        for key in ["created_at", "updated_at", "last_assigned_at"] {
            if let Some(time) = map.remove(key) {
                map.insert(
                    format!("{key}_ms"),
                    json!(millis(None, time.as_str().unwrap_or_default())),
                );
            }
        }
        if let Some(summary) = map.get_mut("summary").filter(|v| v.is_object()) {
            *summary = archive(summary.take());
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ai::tools, mcp_contract::TimeRange};

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../src/tests/fixtures/screenshot_contract_v3.json"
        ))
        .unwrap()
    }

    fn record(f: &Value) -> ScreenshotRecord {
        serde_json::from_value(json!({
            "id": f["screenshot_id"], "image_path": "memory://fixture", "image_hash": "fixture",
            "created_at": f["captured_at_utc"], "timestamp": f["captured_at_ms"].as_i64().unwrap() / 1000,
            "window_title": "Save location", "process_name": "editor.exe"
        })).unwrap()
    }

    #[test]
    fn the_same_capture_keeps_its_identity_and_instant_through_every_tool_and_agent() {
        let f = fixture();
        let record = record(&f);
        let ocr = OcrResult {
            id: f["ocr_id"].as_i64().unwrap(),
            screenshot_id: record.id,
            text: "Save location".into(),
            confidence: 0.99,
            box_coords: vec![vec![1.0, 2.0]],
            created_at: f["ocr_processed_at_utc"].as_str().unwrap().into(),
        };
        let hit = SearchResult {
            id: ocr.id,
            screenshot_id: record.id,
            text: ocr.text.clone(),
            confidence: ocr.confidence,
            box_coords: ocr.box_coords.clone(),
            image_path: record.image_path.clone(),
            window_title: record.window_title.clone(),
            process_name: record.process_name.clone(),
            category: None,
            created_at: ocr.created_at.clone(),
            screenshot_created_at: record.created_at.clone(),
            timestamp: record.timestamp,
        };
        let corpus = SmartClusterOcrCorpusItem {
            screenshot_id: record.id,
            rerank_score: Some(0.9),
            process_name: record.process_name.clone(),
            window_title: record.window_title.clone(),
            created_at: "2026-09-30 16:30:00".into(),
            category: None,
            assigned_at: ocr.created_at.clone(),
            ocr_text: ocr.text.clone(),
        };
        let source = SourceRef {
            id: record.id,
            timestamp_ms: f["captured_at_ms"].as_i64().unwrap(),
            process_name: record.process_name.clone().unwrap(),
            window_title: record.window_title.clone().unwrap(),
        };
        let details = details(&record, &[ocr], false);
        assert_eq!(
            details["ocr_results"][0]["ocr_processed_at_ms"],
            f["ocr_processed_at_ms"]
        );
        assert_eq!(
            ocr_hit(&hit)["ocr_processed_at_ms"],
            f["ocr_processed_at_ms"]
        );
        assert_eq!(ocr_hit(&hit)["ocr_id"], f["ocr_id"]);
        assert!(details["ocr_results"][0].get("box_coords").is_none());

        let projections = [
            ("get_snapshots_by_time_range", json!([screenshot(&record)])),
            ("get_snapshot_details", details),
            ("search_ocr_text", json!([ocr_hit(&hit)])),
            (
                "search_nl",
                json!([visual_hit(
                    &json!({"screenshot_created_at": record.created_at, "metadata": {"timestamp": 0}}),
                    record.id
                )]),
            ),
            (
                "get_smart_cluster_ocr_corpus",
                json!({"cluster_id": 1, "items": [corpus_item(&corpus)]}),
            ),
            (
                "get_recap_day",
                json!({"items": [{"sources": [recap_source(&source)]}]}),
            ),
        ];
        for (tool, projection) in projections {
            let outcome = tools::summarize(tool, projection);
            assert_eq!(outcome.snapshots.len(), 1, "{tool}");
            let reference = &outcome.snapshots[0];
            assert_eq!(reference.screenshot_id, record.id, "{tool}");
            assert_eq!(
                reference.captured_at_ms,
                f["captured_at_ms"].as_i64(),
                "{tool}"
            );
            // A +/- five-minute follow-up must stay ten minutes wide, and an
            // exclusive next-day boundary must not duplicate this capture.
            let time = reference.captured_at_ms.unwrap();
            let range = TimeRange::from_args(
                &json!({"start_ms":time-300_000,"end_ms":time+300_000}),
                true,
            )
            .unwrap();
            let (a, b) = range.storage_seconds();
            assert_eq!(b.unwrap() - a.unwrap() + 1.0, 600.0, "{tool}");
            let before =
                TimeRange::from_args(&json!({"start_ms":time-1000,"end_ms":time}), true).unwrap();
            assert!(!before.contains(reference.captured_at_ms), "{tool}");
            for ambiguous in ["timestamp", "created_at", "screenshot_created_at", "id"] {
                assert!(
                    !outcome.content.contains(&format!("\"{ambiguous}\":")),
                    "{tool}: {ambiguous}"
                );
            }
        }
    }

    #[test]
    fn missing_and_private_screenshots_are_errors_while_empty_ocr_is_a_found_record() {
        let f = fixture();
        let id = f["screenshot_id"].as_i64().unwrap();
        for (input, code) in [
            (None, "SNAPSHOT_NOT_FOUND"),
            (Some(Err(super::super::Rejected)), "PRIVACY_REJECTED"),
        ] {
            let error = super::super::snapshot_details_result(id, input, false).unwrap_err();
            let response = super::super::tool_call_response(Some(json!(1)), Err(error.clone()));
            assert!(response.error.is_none());
            let result = response.result.unwrap();
            assert_eq!(result["isError"], true);
            let payload: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(payload, crate::mcp_contract::error_payload(&error));
            assert_eq!(payload["error"]["code"], code);
            assert!(tools::summarize("get_snapshot_details", payload)
                .snapshots
                .is_empty());
        }
        let found =
            super::super::snapshot_details_result(id, Some(Ok((record(&f), vec![]))), false)
                .unwrap();
        assert_eq!(found["status"], "found");
        assert_eq!(found["record"]["screenshot_id"], id);
        assert_eq!(found["ocr_results"], json!([]));
        assert!(found.get("error").is_none());
    }

    #[test]
    fn unknown_capture_times_stay_null_and_archive_evidence_is_opaque() {
        let f = fixture();
        let mut record = record(&f);
        record.timestamp = None;
        record.created_at = "invalid".into();
        assert_eq!(screenshot(&record)["captured_at_ms"], Value::Null);
        assert_eq!(
            visual_hit(&json!({"metadata":{"timestamp":0}}), record.id)["captured_at_ms"],
            Value::Null
        );
        let evidence = json!({"id":"user-defined", "created_at":"quoted text", "timestamp":12});
        let projected = archive(
            json!({"id":1, "created_at":f["captured_at_utc"], "summary": {
                "smart_cluster_id":1, "updated_at":f["ocr_processed_at_utc"], "evidence":evidence
            }}),
        );
        assert_eq!(projected["cluster_id"], 1);
        assert_eq!(projected["created_at_ms"], f["captured_at_ms"]);
        assert_eq!(
            projected["summary"]["updated_at_ms"],
            f["ocr_processed_at_ms"]
        );
        assert_eq!(projected["summary"]["evidence"], evidence);
    }
}
