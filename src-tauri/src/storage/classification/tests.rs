use super::*;
use crate::classification::scoring::Anchor;
use crate::credential_manager::CredentialManagerState;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, StorageState) {
    let dir = tempfile::tempdir().unwrap();
    let storage = StorageState::new(
        dir.path().into(),
        Arc::new(CredentialManagerState::new(dir.path().into())),
    );
    let conn = Connection::open(dir.path().join("test.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute_batch("INSERT INTO screenshots(id,image_path,image_hash,status,category) VALUES(1,'one','hash-one','committed','old');
        INSERT INTO screenshot_ocr_status(screenshot_id,status,postprocess_status) VALUES(1,'completed','pending');").unwrap();
    *storage.db.lock().unwrap() = Some(conn);
    (dir, storage)
}

fn status(storage: &StorageState) -> (String, i64, Option<String>) {
    let guard = storage.get_connection_named("test_status").unwrap();
    guard.as_ref().unwrap().query_row("SELECT postprocess_status,postprocess_attempts,postprocess_lease FROM screenshot_ocr_status WHERE screenshot_id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}

fn category(storage: &StorageState) -> String {
    storage
        .get_connection_named("test_category")
        .unwrap()
        .as_ref()
        .unwrap()
        .query_row("SELECT category FROM screenshots WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn permit_retry(storage: &StorageState) {
    storage
        .get_connection_named("test_retry")
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE screenshot_ocr_status SET postprocess_next_retry_at=NULL WHERE screenshot_id=1",
            [],
        )
        .unwrap();
}

#[test]
fn legacy_import_is_atomic_resumable_and_keeps_the_source_file() {
    let (dir, storage) = fixture();
    let path = dir.path().join("anchors.json");
    std::fs::write(&path, "broken json").unwrap();
    assert!(storage.load_classification_anchors().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken json");
    let original = r#"{"custom":["old string",{"text":"learned","source":"user_feedback","weight":2.0,"scope":"local","process_name":"qq.exe","added_at":"old-date"}]}"#;
    std::fs::write(&path, original).unwrap();
    let loaded = storage.load_classification_anchors().unwrap();
    assert_eq!(loaded.anchors["custom"][1].added_at, "old-date");
    assert_eq!(loaded.anchors["custom"][1].weight, 2.0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    *storage.db.lock().unwrap() = None;
    *storage.db.lock().unwrap() = Some(Connection::open(dir.path().join("test.db")).unwrap());
    std::fs::write(&path, "legacy file is no longer the owner").unwrap();
    let reopened = storage.load_classification_anchors().unwrap();
    assert_eq!(loaded.anchors, reopened.anchors);
    assert_eq!(loaded.revision, reopened.revision);
}

#[test]
fn anchors_reject_stale_revisions_and_database_generations() {
    let (_dir, storage) = fixture();
    let snapshot = storage.load_classification_anchors().unwrap();
    assert_eq!(
        storage
            .save_classification_anchors(0, snapshot.revision, &snapshot.anchors)
            .unwrap(),
        snapshot.revision + 1
    );
    assert!(storage
        .save_classification_anchors(0, snapshot.revision, &snapshot.anchors)
        .is_err());
    storage.bump_db_generation();
    assert!(storage
        .save_classification_anchors(0, snapshot.revision + 1, &snapshot.anchors)
        .is_err());
}

#[test]
fn weighted_scores_commit_once_and_invalid_results_leave_the_lease_retryable() {
    let (_dir, storage) = fixture();
    let lease = storage.claim_native_classification(0, 1).unwrap().unwrap();
    assert!(storage.start_native_classification(&lease).unwrap());
    for score in [f64::NAN, f64::INFINITY, -0.5] {
        assert!(storage
            .commit_native_classification(&lease, Some("invalid"), Some(score))
            .is_err());
        assert_eq!(status(&storage).0, "processing");
        assert_eq!(category(&storage), "old");
    }
    assert!(storage
        .commit_native_classification(&lease, Some("learned"), Some(1.2196))
        .unwrap());
    assert!(!storage
        .commit_native_classification(&lease, Some("overwrite"), Some(0.3))
        .unwrap());
    assert_eq!(category(&storage), "learned");
    assert_eq!(status(&storage), ("completed".into(), 0, None));
}

#[test]
fn scheduling_deferral_preserves_budget_and_old_lease_cannot_touch_new_work() {
    let (_dir, storage) = fixture();
    let first = storage.claim_native_classification(0, 1).unwrap().unwrap();
    storage
        .defer_native_classification(&first, "foreground_busy", true)
        .unwrap();
    assert_eq!(status(&storage), ("pending".into(), 0, None));
    assert!(storage.claim_native_classification(0, 1).unwrap().is_none());
    permit_retry(&storage);
    let second = storage.claim_native_classification(0, 1).unwrap().unwrap();
    storage
        .defer_native_classification(&first, "stale failure", false)
        .unwrap();
    assert_eq!(
        status(&storage),
        ("queued".into(), 0, Some(second.token.clone()))
    );
    storage
        .defer_native_classification(&second, "model failure", false)
        .unwrap();
    assert_eq!(status(&storage).1, 1);
}

#[test]
fn user_correction_and_learning_intent_survive_in_flight_classification() {
    let (_dir, storage) = fixture();
    let lease = storage.claim_native_classification(0, 1).unwrap().unwrap();
    assert!(storage.update_category_with_feedback(1, "chosen").unwrap());
    assert!(storage
        .commit_native_classification(&lease, Some("automatic"), Some(1.3))
        .unwrap());
    assert_eq!(category(&storage), "chosen");
    let feedback = storage.next_classification_feedback().unwrap().unwrap();
    assert_eq!(feedback.old_category.as_deref(), Some("old"));
    let mut snapshot = storage.load_classification_anchors().unwrap();
    snapshot.anchors.insert(
        "chosen".into(),
        vec![Anchor::new(
            "synthetic feedback".into(),
            "user_feedback",
            2.0,
            "local",
            Some("app.exe".into()),
        )],
    );
    storage
        .commit_classification_feedback(0, snapshot.revision, &snapshot.anchors, &feedback)
        .unwrap();
    assert!(storage.next_classification_feedback().unwrap().is_none());
    assert_eq!(
        storage.load_classification_anchors().unwrap().anchors["chosen"][0].weight,
        2.0
    );
    assert!(storage
        .commit_classification_feedback(0, snapshot.revision, &snapshot.anchors, &feedback)
        .is_err());
    assert_eq!(storage.classification_user_revision(1).unwrap(), 1);
}

#[test]
fn feedback_persistence_failure_keeps_both_anchor_snapshot_and_intent() {
    let (_dir, storage) = fixture();
    storage.update_category_with_feedback(1, "chosen").unwrap();
    let feedback = storage.next_classification_feedback().unwrap().unwrap();
    let snapshot = storage.load_classification_anchors().unwrap();
    storage
        .save_classification_anchors(0, snapshot.revision, &snapshot.anchors)
        .unwrap();
    assert!(storage
        .commit_classification_feedback(0, snapshot.revision, &Anchors::new(), &feedback)
        .is_err());
    assert!(storage.next_classification_feedback().unwrap().is_some());
    assert_eq!(
        storage.load_classification_anchors().unwrap().anchors,
        snapshot.anchors
    );
}

#[test]
fn replaced_or_deleted_inputs_and_changed_databases_reject_old_results() {
    let (_dir, storage) = fixture();
    let first = storage.claim_native_classification(0, 1).unwrap().unwrap();
    storage
        .get_connection_named("test_change")
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE screenshots SET process_name='changed' WHERE id=1",
            [],
        )
        .unwrap();
    assert!(storage
        .commit_native_classification(&first, Some("stale"), Some(1.0))
        .is_err());
    storage
        .defer_native_classification(&first, "classification source changed", true)
        .unwrap();
    permit_retry(&storage);
    let second = storage.claim_native_classification(0, 1).unwrap().unwrap();
    storage.bump_db_generation();
    assert!(storage
        .commit_native_classification(&second, Some("wrong database"), Some(1.0))
        .is_err());
    assert_eq!(category(&storage), "old");
    storage
        .get_connection_named("test_delete")
        .unwrap()
        .as_ref()
        .unwrap()
        .execute("UPDATE screenshots SET is_deleted=1 WHERE id=1", [])
        .unwrap();
    assert!(storage
        .claim_native_classification(storage.db_generation(), 1)
        .unwrap()
        .is_none());
    assert!(storage
        .list_pending_ocr_postprocess_ids(10)
        .unwrap()
        .is_empty());
}

#[test]
fn disabled_classification_finishes_without_changing_the_category() {
    let (_dir, storage) = fixture();
    let lease = storage.claim_native_classification(0, 1).unwrap().unwrap();
    storage
        .commit_native_classification(&lease, None, None)
        .unwrap();
    assert_eq!(category(&storage), "old");
    assert_eq!(status(&storage).0, "completed");
}

#[test]
fn a_user_choice_made_before_dispatch_also_wins_over_automatic_classification() {
    let (_dir, storage) = fixture();
    storage
        .update_category_with_feedback(1, "chosen before dispatch")
        .unwrap();
    let lease = storage.claim_native_classification(0, 1).unwrap().unwrap();
    assert_eq!(lease.user_revision, 1);
    storage
        .commit_native_classification(&lease, Some("automatic"), Some(1.2))
        .unwrap();
    assert_eq!(category(&storage), "chosen before dispatch");
    assert_eq!(status(&storage).0, "completed");
}
