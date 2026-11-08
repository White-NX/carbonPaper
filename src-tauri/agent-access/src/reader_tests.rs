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
    let repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    (dir, repo, binding)
}
fn put(repo: &mut AgentRepository, binding: &AgentViewBinding, revision: i64, record: TextRecord) {
    repo.apply(&ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change: TextChange::Put { record },
    })
    .unwrap();
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
            TextRecord::Ocr(OcrText {
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
