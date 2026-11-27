use crate::{
    dispatcher,
    repository::AgentRepository,
    sensitive_filter::{SensitiveFilterConfig, SensitiveFilterState},
    types::*,
};
use carbonpaper_app_bound::protocol::AgentViewBinding;
use serde_json::{json, Value};

fn fixture() -> (tempfile::TempDir, AgentRepository, AgentViewBinding) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-memory.db");
    let binding = AgentViewBinding {
        dataset_id: "a".repeat(64),
        generation: "b".repeat(64),
        view_path: path.to_string_lossy().into_owned(),
    };
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    crate::test_source::configure(&mut repo, &binding);
    (dir, repo, binding)
}
fn put(
    repo: &mut AgentRepository,
    binding: &AgentViewBinding,
    revision: i64,
    record: impl Into<crate::test_source::Record>,
) {
    crate::test_source::put(repo, binding, revision, record);
}
fn call(repo: &AgentRepository, filter: &SensitiveFilterState, name: &str, args: Value) -> Value {
    dispatcher::call(repo, filter, name, args, None).unwrap()
}
fn snapshot(id: i64, time: i64, title: &str) -> TextRecord {
    TextRecord::Snapshot(SnapshotText {
        screenshot_id: id,
        captured_at_ms: Some(time),
        window_title: Some(title.into()),
        process_name: Some("editor.exe".into()),
        category: Some("work".into()),
        ..Default::default()
    })
}

#[test]
fn visual_search_reads_existing_vectors_and_observes_source_deletion() {
    let (_dir, mut repo, binding) = fixture();
    let filter = SensitiveFilterState::default();
    for id in 1..=2 {
        put(
            &mut repo,
            &binding,
            id,
            snapshot(id, id * 1000, "source notes"),
        );
        put(
            &mut repo,
            &binding,
            id + 2,
            crate::test_source::Record::Ocr(OcrText {
                ocr_id: id,
                screenshot_id: id,
                text: format!("source OCR {id}"),
                ..Default::default()
            }),
        );
        put(
            &mut repo,
            &binding,
            id + 4,
            crate::test_source::Record::Vector(VectorText {
                kind: "clip_image".into(),
                screenshot_id: id,
                model_id: "fixture".into(),
                model_revision: "fixture".into(),
                vector: if id == 1 {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                },
            }),
        );
    }
    let search = |repo: &AgentRepository| {
        dispatcher::call(
            repo,
            &filter,
            "search_nl",
            json!({"query":"source notes"}),
            Some(("fixture", "fixture", &[1.0, 0.0])),
        )
        .unwrap()
    };
    let before = search(&repo);
    assert_eq!(before["indexed_snapshots"], 2);
    assert_eq!(before["items"][0]["screenshot_id"], 1);
    assert_eq!(before["items"][0]["ocr_text"], "source OCR 1");
    crate::test_source::connection(&binding)
        .execute_batch(
            "INSERT INTO vector_blocks(id,data)
                SELECT 1,vector_f32 FROM derived_embeddings WHERE subject_key='image-1';
             UPDATE derived_embeddings SET vector_block_id=1,vector_offset=0,
                vector_length=length(vector_f32),vector_f32=X'' WHERE subject_key='image-1';",
        )
        .unwrap();
    assert_eq!(search(&repo), before);
    crate::test_source::connection(&binding).execute("DELETE FROM derived_embeddings WHERE index_kind='clip_image' AND subject_key='image-1'",[]).unwrap();
    repo.apply(&ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision: 7,
        change: TextChange::SourceChanged,
    })
    .unwrap();
    let after = search(&repo);
    assert_eq!(after["indexed_snapshots"], 1);
    assert_eq!(after["items"][0]["screenshot_id"], 2);
    assert_eq!(after["index_status"], "partial");
}

#[test]
fn additional_storage_depends_on_record_count_instead_of_text_and_vector_length() {
    let pages = |text_repeats: usize, dimensions: usize| {
        let (_dir, mut repo, binding) = fixture();
        put(&mut repo, &binding, 1, snapshot(1, 1000, "notes"));
        for id in 1..=12 {
            put(
                &mut repo,
                &binding,
                id + 1,
                crate::test_source::Record::Ocr(OcrText {
                    ocr_id: id,
                    screenshot_id: 1,
                    text: format!("{id}:{}", "长篇文字".repeat(text_repeats)),
                    ..Default::default()
                }),
            );
            put(
                &mut repo,
                &binding,
                id + 20,
                crate::test_source::Record::Vector(VectorText {
                    kind: "semantic_text".into(),
                    screenshot_id: id,
                    model_id: "fixture".into(),
                    model_revision: "fixture".into(),
                    vector: vec![1.0; dimensions],
                }),
            );
        }
        assert!(repo.all::<serde_json::Value>("ocr").unwrap().is_empty());
        assert!(repo.all::<serde_json::Value>("vector").unwrap().is_empty());
        repo.conn
            .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let small = pages(2, 4);
    let large = pages(2000, 768);
    assert!(
        large <= small + 1,
        "authorized source growth expanded the sidecar from {small} to {large} pages"
    );
}

#[test]
fn ocr_authorizations_reject_swapped_ciphertexts_and_other_datasets() {
    let (_dir, mut repo, binding) = fixture();
    put(&mut repo, &binding, 1, snapshot(1, 1000, "notes"));
    for id in 1..=2 {
        put(
            &mut repo,
            &binding,
            id + 1,
            crate::test_source::Record::Ocr(OcrText {
                ocr_id: id,
                screenshot_id: 1,
                text: format!("authorized {id}"),
                ..Default::default()
            }),
        );
    }
    assert_eq!(repo.ocr(1).unwrap().len(), 2);
    let conn = crate::test_source::connection(&binding);
    conn.execute(
        "UPDATE ocr_results SET text_enc=(SELECT text_enc FROM ocr_results WHERE id=2) WHERE id=1",
        [],
    )
    .unwrap();
    assert_eq!(repo.ocr(1).unwrap_err(), "AGENT_SOURCE_CHANGED");
    conn.execute(
        "UPDATE app_metadata SET value='other-dataset' WHERE key='protected_content_dataset_id'",
        [],
    )
    .unwrap();
    let reopened = AgentRepository::open(
        std::path::Path::new(&binding.view_path),
        &[7; 32],
        &binding,
        true,
    )
    .unwrap();
    assert_eq!(reopened.ocr(1).unwrap_err(), "AGENT_DATASET_MISMATCH");
}

#[test]
fn split_card_fragments_are_hidden_in_detail_and_search_evidence() {
    let (_dir, mut repo, binding) = fixture();
    let filter = SensitiveFilterState::default();
    filter.update_config(SensitiveFilterConfig {
        mode: "mask".into(),
        pii_entities: vec![crate::pii::PiiKind::BankCard.name().into()],
        ..filter.get_config()
    });
    put(&mut repo, &binding, 1, snapshot(1, 1000, "notes"));
    for (index, text) in ["2233", "0012", "3456", "0020"].into_iter().enumerate() {
        let left = 10.0 + 206.0 * index as f64;
        let id = index as i64 + 2;
        put(
            &mut repo,
            &binding,
            id,
            crate::test_source::Record::Ocr(OcrText {
                ocr_id: id,
                screenshot_id: 1,
                text: text.into(),
                box_coords: vec![
                    vec![left, 10.0],
                    vec![left + 200.0, 10.0],
                    vec![left + 200.0, 30.0],
                    vec![left, 30.0],
                ],
                ..Default::default()
            }),
        );
    }
    let details = call(
        &repo,
        &filter,
        "get_snapshot_details",
        json!({"screenshot_id":1}),
    );
    let blocks = details["ocr_results"].as_array().unwrap();
    assert_eq!(blocks.len(), 4);
    assert!(blocks.iter().all(|block| block["text"] == "[CN_BANK_CARD]"));
    let hits = call(
        &repo,
        &filter,
        "search_ocr_text",
        json!({"query":"0012","fuzzy":false}),
    );
    let hit = &hits["items"][0];
    assert_eq!(hit["text"], "[CN_BANK_CARD]");
    assert!(hit.get("match_evidence").is_none());
}

#[test]
fn full_history_pagination_crosses_privacy_gaps_without_disclosing_images_or_keys() {
    let (_dir, mut repo, binding) = fixture();
    let filter = SensitiveFilterState::with_test_words(&["withheld"]);
    filter.update_config(SensitiveFilterConfig {
        mode: "reject".into(),
        ..Default::default()
    });
    put(&mut repo, &binding, 1, snapshot(1, 1000, "ancient notes"));
    put(
        &mut repo,
        &binding,
        2,
        snapshot(2, 2000, "withheld contact details"),
    );
    put(&mut repo, &binding, 3, snapshot(3, 3000, "future notes"));
    let mut offset = 0;
    let mut ids = Vec::new();
    loop {
        let result = call(
            &repo,
            &filter,
            "get_snapshots_by_time_range",
            json!({"start_ms":0,"end_ms":4000,"limit":1,"offset":offset}),
        );
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["screenshot_id"].as_i64().unwrap()),
        );
        assert!(!result.to_string().contains("withheld"));
        assert!(!result.to_string().contains("image_path"));
        match result["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(ids, vec![3, 1]);
    assert!(dispatcher::call(
        &repo,
        &filter,
        "upsert_smart_cluster_summary",
        json!({"cluster_id":1}),
        None
    )
    .unwrap_err()
    .starts_with("READ_ONLY_GRANT"));
}

#[test]
fn ocr_matching_preserves_fuzzy_evidence_and_scope_and_reports_missing_visual_index() {
    let (_dir, mut repo, binding) = fixture();
    let filter = SensitiveFilterState::default();
    put(&mut repo, &binding, 1, snapshot(1, 1000, "notes"));
    put(&mut repo, &binding, 2, snapshot(2, 2000, "notes"));
    for (id, text) in [
        (1, "Save notes in this location"),
        (2, "Six Degrees of Separation"),
    ] {
        put(
            &mut repo,
            &binding,
            3 + id,
            crate::test_source::Record::Ocr(OcrText {
                ocr_id: id + 10,
                screenshot_id: id,
                text: text.into(),
                ocr_processed_at_ms: Some(3000 + id),
                ..Default::default()
            }),
        );
    }
    let words = call(
        &repo,
        &filter,
        "search_ocr_text",
        json!({"query":"save \"location\"","fuzzy":false,"process_names":[],"categories":["work"]}),
    );
    assert_eq!(words["items"][0]["screenshot_id"], 1);
    assert_eq!(
        words["items"][0]["match_evidence"]["matched_terms"],
        json!(["save", "location"])
    );
    assert_eq!(words["items"][0]["captured_at_ms"], 1000);
    assert_eq!(
        call(
            &repo,
            &filter,
            "search_ocr_text",
            json!({"query":"sepation","fuzzy":false})
        )["total"],
        0
    );
    let fuzzy = call(
        &repo,
        &filter,
        "search_ocr_text",
        json!({"query":"sepation","fuzzy":true}),
    );
    assert_eq!(fuzzy["items"][0]["screenshot_id"], 2);
    assert_eq!(
        fuzzy["items"][0]["match_evidence"]["match_type"],
        "approximate"
    );
    assert_eq!(
        call(
            &repo,
            &filter,
            "search_ocr_text",
            json!({"query":"save","start_ms":1001})
        )["total"],
        0
    );
    assert_eq!(
        call(
            &repo,
            &filter,
            "search_ocr_text",
            json!({"query":"save","categories":["personal"]})
        )["total"],
        0
    );
    let descriptor =
        crate::semantic_models::descriptor(crate::ml_protocol::MlSemanticModel::ChineseClip);
    let query = vec![1.0; 512];
    assert!(dispatcher::call(
        &repo,
        &filter,
        "search_nl",
        json!({"query":"notes"}),
        Some((descriptor.model_id, descriptor.revision, &query))
    )
    .unwrap_err()
    .starts_with("AGENT_INDEX_UNAVAILABLE"));
}

#[test]
fn recap_search_visits_late_activities_applies_corrections_and_tracks_semantic_coverage() {
    let (_dir, mut repo, binding) = fixture();
    let filter = SensitiveFilterState::default();
    let date = "2020-01-01";
    let mut records = Vec::new();
    let mut activities = Vec::new();
    for id in 1..=61 {
        put(&mut repo, &binding, id, snapshot(id, id * 1000, "notes"));
        let source = SourceText {
            screenshot_id: id,
            captured_at_ms: id * 1000,
            process_name: "editor.exe".into(),
            window_title: "notes".into(),
        };
        records.push(source.clone());
        activities.push(ActivityText {
            id: format!("activity-{id}"),
            task_id: "task".into(),
            task_title: "saved work".into(),
            text: if id == 61 {
                "uniquely late evidence".into()
            } else {
                "ordinary notes".into()
            },
            start_ms: id * 1000,
            end_ms: id * 1000,
            sources: vec![source],
            member_ids: vec![id],
            ..Default::default()
        });
    }
    put(
        &mut repo,
        &binding,
        62,
        TextRecord::Calendar(CalendarText {
            date: date.into(),
            source_revision: 4,
            ..Default::default()
        }),
    );
    let key = "2020-01-01:0";
    let batch = RecapContent {
        start_ms: 0,
        end_ms: 100_000,
        status: "complete".into(),
        activities,
        records,
        ..Default::default()
    };
    put(
        &mut repo,
        &binding,
        63,
        TextRecord::Recap(RecapText {
            date: date.into(),
            key: key.into(),
            part: "batch".into(),
            revision: 4,
            content: batch,
        }),
    );
    let mut corrections = RecapContent::default();
    corrections.assignments.insert(61, "corrected".into());
    corrections
        .names
        .insert("corrected".into(), "renamed task".into());
    put(
        &mut repo,
        &binding,
        64,
        TextRecord::Recap(RecapText {
            date: date.into(),
            key: date.into(),
            part: "corrections".into(),
            revision: -1,
            content: corrections,
        }),
    );
    let late = call(
        &repo,
        &filter,
        "search_recaps",
        json!({"query":"uniquely late","mode":"keyword","process_names":["EDITOR.EXE"],"include_details":false}),
    );
    assert_eq!(late["total"], 1);
    assert_eq!(late["items"][0]["task_title"], "renamed task");
    assert_eq!(late["items"][0]["member_ids"], json!([61]));
    assert_eq!(
        late["items"][0]["time_semantics"],
        "evidence_observation_span"
    );
    assert_eq!(late["items"][0]["span_ms"], 0);
    assert!(late["coverage"].is_null());
    assert_eq!(late["execution"]["keyword_complete"], true);
    assert_eq!(
        call(
            &repo,
            &filter,
            "get_recap_day",
            json!({"date":date,"offset":50,"limit":20})
        )["returned_count"],
        11
    );
    let hybrid = call(
        &repo,
        &filter,
        "search_recaps",
        json!({"query":"uniquely late"}),
    );
    assert_eq!(hybrid["items"][0]["task_id"], "corrected");
    assert_eq!(hybrid["semantic_status"], "not_prepared");
    assert_eq!(hybrid["coverage"]["missing_dates"], json!([date]));
    let sem = call(
        &repo,
        &filter,
        "search_recaps",
        json!({"query":"uniquely late","mode":"semantic"}),
    );
    assert_eq!(sem["complete"], false);
    let chunks = crate::recap_text_contract::chunks(
        "renamed task",
        "uniquely late evidence",
        std::iter::empty(),
    );
    let vector = vec![1.0; 384];
    let mut saved = RecapContent {
        version: crate::recap_text_contract::INDEX_VERSION,
        model_revision: crate::semantic_models::MINILM_VECTOR_SPACE_REVISION.into(),
        ..Default::default()
    };
    saved.vectors.insert(chunks[1].key.clone(), vector.clone());
    put(
        &mut repo,
        &binding,
        65,
        TextRecord::Recap(RecapText {
            date: date.into(),
            key: date.into(),
            part: "search".into(),
            revision: 4,
            content: saved,
        }),
    );
    let sem = dispatcher::call(
        &repo,
        &filter,
        "search_recaps",
        json!({"query":"late work","mode":"semantic"}),
        Some((
            crate::semantic_models::MINILM_MODEL_ID,
            crate::semantic_models::MINILM_VECTOR_SPACE_REVISION,
            &vector,
        )),
    )
    .unwrap();
    assert_eq!(sem["total"], 1);
    assert_eq!(sem["semantic_status"], "partial");
    assert_eq!(sem["execution"]["semantic_executed"], true);
    assert!(dispatcher::call(&repo,&filter,"search_recaps",json!({"query":"uniquely late","mode":"keyword","ranking_revision":late["ranking_revision"]}),None).unwrap_err().starts_with("RECAP_SEARCH_RESULTS_CHANGED"));
}
