use super::*;
use crate::credential_manager::CredentialManagerState;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, StorageState, Arc<CredentialManagerState>) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(directory.path().to_path_buf()));
    credentials.set_foreground_state(true);
    credentials.update_auth_time();
    let storage = StorageState::new(directory.path().to_path_buf(), credentials.clone());
    let connection = Connection::open(directory.path().join("jobs.db")).unwrap();
    storage.init_tables(&connection).unwrap();
    connection.execute_batch(
        "INSERT INTO screenshots(id,image_path,image_hash,status) VALUES(1,'new.enc','v1','committed');
         INSERT INTO screenshot_ocr_status(screenshot_id,status) VALUES(1,'pending');
         INSERT INTO screenshot_edit_ocr_jobs(screenshot_id,image_hash) VALUES(1,'v1');",
    ).unwrap();
    *storage.db.lock().unwrap() = Some(connection);
    (directory, storage, credentials)
}

#[test]
fn recognition_queue_survives_reopen_and_recovers_an_abandoned_lease() {
    let (directory, storage, _) = fixture();
    let old = storage.claim_edited_ocr_job().unwrap().unwrap();
    *storage.db.lock().unwrap() = None;
    *storage.db.lock().unwrap() = Some(Connection::open(directory.path().join("jobs.db")).unwrap());
    storage
        .recover_edited_ocr_jobs(storage.db_generation())
        .unwrap();
    let recovered = storage.claim_edited_ocr_job().unwrap().unwrap();
    assert_ne!(old.lease, recovered.lease);
    assert_eq!(old.image_hash, recovered.image_hash);
    assert!(!storage
        .complete_edited_ocr_job(&old, &[], 0.0, "cpu")
        .unwrap());
    assert!(storage
        .complete_edited_ocr_job(&recovered, &[], 0.0, "cpu")
        .unwrap());
    assert_eq!(
        storage.edited_ocr_status(1).unwrap().unwrap().status,
        "completed"
    );
}

#[test]
fn later_edits_and_deletion_reject_old_results_and_error_updates() {
    let (_directory, storage, _) = fixture();
    let old = storage.claim_edited_ocr_job().unwrap().unwrap();
    {
        let guard = storage.db.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .execute_batch(
                "UPDATE screenshots SET image_hash='v2' WHERE id=1;
             INSERT INTO screenshot_edit_ocr_jobs(screenshot_id,image_hash) VALUES(1,'v2');",
            )
            .unwrap();
    }
    assert!(!storage
        .complete_edited_ocr_job(&old, &[], 0.0, "cpu")
        .unwrap());
    assert!(storage
        .defer_edited_ocr_job(&old, "old failure", false)
        .unwrap()
        .is_none());
    let latest = storage.claim_edited_ocr_job().unwrap().unwrap();
    let key = WriteKey::new(|bytes| Ok(bytes.to_vec())).unwrap();
    let results = vec![OcrResultInput {
        text: "current edited text".into(),
        confidence: 0.9,
        box_coords: vec![vec![0.0, 0.0]; 4],
    }];
    {
        let mut guard = storage.db.lock().unwrap();
        assert!(publish_results(
            guard.as_mut().unwrap(),
            &latest,
            &results,
            Some(&key),
            12.0,
            "cpu"
        )
        .unwrap());
        let conn = guard.as_ref().unwrap();
        let (plain, encrypted): (Option<String>, Vec<u8>) = conn
            .query_row(
                "SELECT text,text_enc FROM ocr_results WHERE screenshot_id=1 AND is_deleted=0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(plain.is_none());
        assert!(!encrypted
            .windows(results[0].text.len())
            .any(|bytes| bytes == results[0].text.as_bytes()));
        conn.execute("UPDATE screenshots SET is_deleted=1 WHERE id=1", [])
            .unwrap();
    }
    assert!(!storage
        .complete_edited_ocr_job(&latest, &[], 0.0, "cpu")
        .unwrap());
    assert!(storage.edited_ocr_status(1).unwrap().is_none());
}

#[test]
fn recognition_failure_preserves_the_saved_image_and_auth_wait_does_not_spend_retries() {
    let (_directory, storage, credentials) = fixture();
    let first = storage.claim_edited_ocr_job().unwrap().unwrap();
    storage
        .defer_edited_ocr_job(&first, "AUTH_REQUIRED", true)
        .unwrap();
    credentials.invalidate_session();
    credentials.set_foreground_state(false);
    assert!(storage.claim_edited_ocr_job().unwrap().is_none());
    assert_eq!(
        storage
            .complete_edited_ocr_job(&first, &[], 0.0, "cpu")
            .unwrap_err(),
        "AUTH_REQUIRED"
    );
    credentials.set_foreground_state(true);
    credentials.update_auth_time();
    let second = storage.claim_edited_ocr_job().unwrap().unwrap();
    assert_eq!(second.attempts, 0);
    storage
        .defer_edited_ocr_job(&second, "recognition unavailable", false)
        .unwrap();
    assert_eq!(
        storage.edited_ocr_status(1).unwrap().unwrap().status,
        "failed"
    );
    assert!(storage.claim_edited_ocr_job().unwrap().is_none());
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        assert_eq!(
            conn.query_row("SELECT image_path FROM screenshots WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "new.enc"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM ocr_results WHERE is_deleted=0",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT attempts FROM screenshot_edit_ocr_jobs WHERE screenshot_id=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert!(storage.retry_edited_ocr_job(1, "stale").is_err());
    storage.retry_edited_ocr_job(1, "v1").unwrap();
    assert_eq!(storage.claim_edited_ocr_job().unwrap().unwrap().attempts, 0);
}

#[test]
fn a_database_generation_change_fences_publication_and_retry_state() {
    let (_directory, storage, _) = fixture();
    let old = storage.claim_edited_ocr_job().unwrap().unwrap();
    storage.db_generation.fetch_add(1, Ordering::AcqRel);
    assert!(!storage
        .complete_edited_ocr_job(&old, &[], 0.0, "cpu")
        .unwrap());
    assert!(storage
        .defer_edited_ocr_job(&old, "old failure", false)
        .unwrap()
        .is_none());
    storage
        .recover_edited_ocr_jobs(storage.db_generation())
        .unwrap();
    let latest = storage.claim_edited_ocr_job().unwrap().unwrap();
    assert_ne!(old.generation, latest.generation);
    assert!(storage
        .complete_edited_ocr_job(&latest, &[], 0.0, "cpu")
        .unwrap());
}
