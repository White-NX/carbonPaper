use super::*;
use crate::{
    credential_manager::{get_cached_master_key, CredentialManagerState},
    processing_stage::{Broker, ProcessingInput, ProcessingStaging, STAGING_FILE},
};
use carbonpaper_app_bound::{
    crypto::KeyProtector,
    ledger::{Ledger, Principal},
    protocol::*,
};
use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    Arc, Mutex,
};

struct TestProtection;
impl KeyProtector for TestProtection {
    fn protect(&self, key: &TaskKey) -> carbonpaper_app_bound::protocol::Result<Vec<u8>> {
        Ok(key.0.iter().map(|v| v ^ 0xa5).collect())
    }
    fn unprotect(&self, bytes: &[u8]) -> carbonpaper_app_bound::protocol::Result<TaskKey> {
        Ok(TaskKey(bytes.iter().map(|v| v ^ 0xa5).collect()))
    }
}

struct TestBroker {
    ledger: Mutex<Ledger>,
    principal: Mutex<Principal>,
    clock: AtomicI64,
    online: AtomicBool,
    installed: AtomicBool,
    acquisitions: AtomicUsize,
    inspected: Mutex<Vec<String>>,
}
impl Broker for TestBroker {
    fn installed(&self) -> carbonpaper_app_bound::protocol::Result<bool> {
        Ok(self.installed.load(Ordering::SeqCst))
    }
    fn supported(&self) -> bool {
        true
    }
    fn call(&self, request: Request) -> carbonpaper_app_bound::protocol::Result<Response> {
        if !self.online.load(Ordering::SeqCst) {
            return Err(BrokerError::Unavailable);
        }
        if matches!(&request, Request::AcquireTask { .. }) {
            self.acquisitions.fetch_add(1, Ordering::SeqCst);
        }
        if let Request::InspectTask { task_id } = &request {
            self.inspected.lock().unwrap().push(task_id.clone());
        }
        self.ledger.lock().unwrap().handle(
            &self.principal.lock().unwrap(),
            request,
            self.clock.load(Ordering::SeqCst),
            &TestProtection,
        )
    }
}

fn fixture() -> (tempfile::TempDir, StorageState, Arc<TestBroker>) {
    let dir = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(dir.path().to_path_buf()));
    let mut storage = StorageState::new(dir.path().to_path_buf(), credentials);
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    storage.init_tables(&conn).unwrap();
    *storage.db.lock().unwrap() = Some(conn);
    let principal = Principal {
        sid: "test-user".into(),
        runtime_id: "test-release".into(),
        process_identity: "10:100".into(),
    };
    let mut ledger = Ledger::open(&dir.path().join("test-service.db")).unwrap();
    ledger
        .register_owner(&principal.sid, &principal.runtime_id, true)
        .unwrap();
    let broker = Arc::new(TestBroker {
        ledger: Mutex::new(ledger),
        principal: Mutex::new(principal),
        clock: AtomicI64::new(now_secs()),
        online: AtomicBool::new(true),
        installed: AtomicBool::new(true),
        acquisitions: AtomicUsize::new(0),
        inspected: Mutex::new(Vec::new()),
    });
    storage.processing_stage = ProcessingStaging::with_test_broker(broker.clone());
    storage
        .processing_stage
        .initialize(
            dir.path(),
            storage.processing_dataset_id().unwrap(),
            storage.db_generation(),
        )
        .unwrap();
    storage.processing_stage.refresh().unwrap();
    (dir, storage, broker)
}

fn sql(storage: &StorageState, statement: &str) {
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute_batch(statement)
        .unwrap();
}

fn insert_input(storage: &StorageState, id: i64, consumers: u8) -> ProcessingInput {
    {
        let guard = storage.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshots(id,image_path,image_hash,status) VALUES(?1,?2,?3,'committed')",
            params![id, format!("{id}.enc"), format!("hash-{id}")],
        )
        .unwrap();
        conn.execute("INSERT INTO screenshot_ocr_status(screenshot_id,status,postprocess_status) VALUES(?1,'completed','pending')", [id]).unwrap();
        conn.execute("INSERT INTO ocr_results(screenshot_id,text_hash,text_enc,text_key_encrypted) VALUES(?1,?2,X'01',X'02')", params![id, format!("text-{id}")]).unwrap();
    }
    let input = ProcessingInput {
        image_hash: format!("hash-{id}"),
        window_title: "Editor".into(),
        process_name: "code.exe".into(),
        timestamp_ms: 1234,
        ocr_text: format!("private captured input {id}"),
        source_revision: storage.staged_source_revision(id).unwrap(),
        clip: None,
    };
    assert!(storage
        .processing_stage
        .stage(id, &input, consumers)
        .unwrap());
    input
}

fn restart_stage(dir: &std::path::Path, storage: &mut StorageState, broker: &Arc<TestBroker>) {
    storage.processing_stage.shutdown();
    storage.processing_stage = ProcessingStaging::with_test_broker(broker.clone());
    storage
        .processing_stage
        .initialize(
            dir,
            storage.processing_dataset_id().unwrap(),
            storage.db_generation(),
        )
        .unwrap();
    storage.processing_stage.refresh().unwrap();
}

fn category(storage: &StorageState, id: i64) -> Option<String> {
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .query_row("SELECT category FROM screenshots WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn locked_restart_only_recovers_pending_inputs_and_never_authorizes_archive_reads() {
    let (dir, mut storage, broker) = fixture();
    let input = insert_input(&storage, 1, 7);
    let bytes = std::fs::read(dir.path().join(STAGING_FILE)).unwrap();
    assert!(!bytes
        .windows(input.ocr_text.len())
        .any(|w| w == input.ocr_text.as_bytes()));
    let work = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap();
    storage
        .commit_staged_category(&work.receipt, Some("Development"), Some(0.9))
        .unwrap();
    storage
        .processing_stage
        .finish(&storage, &work.receipt)
        .unwrap();
    restart_stage(dir.path(), &mut storage, &broker);
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .is_none());
    let pending = storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .unwrap();
    assert_eq!(pending.input.ocr_text, input.ocr_text);
    assert!(!storage.credential_state.background_authorized());
    assert!(!storage.credential_state.is_session_valid());
    assert!(get_cached_master_key(&storage.credential_state).is_none());
    assert!(storage
        .processing_stage
        .check_receipt(&work.receipt)
        .is_err());
}

#[test]
fn callback_fields_cannot_retarget_an_issued_receipt() {
    let (dir, storage, _) = fixture();
    insert_input(&storage, 1, 7);
    insert_input(&storage, 2, 7);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    let mut variants = Vec::new();
    let mut changed = receipt.clone();
    changed.screenshot_id = if receipt.screenshot_id == 1 { 2 } else { 1 };
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.task_id = random_id();
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.dataset_id = random_id();
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.source_revision += 1;
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.db_generation += 1;
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.consumer = Consumer::Clip;
    variants.push(changed);
    let mut changed = receipt.clone();
    changed.lease_id = random_id();
    variants.push(changed);
    let conn = Connection::open(dir.path().join(STAGING_FILE)).unwrap();
    conn.execute(
        "UPDATE staged_work SET deadline=?1",
        [now_secs() + DAY_SECS],
    )
    .unwrap();
    for changed in variants {
        assert!(storage.processing_stage.check_receipt(&changed).is_err());
        assert!(storage
            .commit_staged_category(&changed, Some("Injected"), Some(1.0))
            .is_err());
        assert!(storage.processing_stage.release(&changed, true).is_err());
    }
    assert_eq!(category(&storage, 1), None);
    assert_eq!(category(&storage, 2), None);
    storage
        .commit_staged_category(&receipt, Some("Development"), Some(0.9))
        .unwrap();
}

#[test]
fn malformed_input_does_not_request_a_key_or_block_the_next_task() {
    let (dir, storage, broker) = fixture();
    insert_input(&storage, 1, 1);
    insert_input(&storage, 2, 1);
    Connection::open(dir.path().join(STAGING_FILE))
        .unwrap()
        .execute(
            "UPDATE staged_inputs SET binding='{',created=0 WHERE screenshot_id=1",
            [],
        )
        .unwrap();
    let work = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap();
    assert_eq!(work.receipt.screenshot_id, 2);
    assert_eq!(broker.acquisitions.load(Ordering::SeqCst), 1);
}

#[test]
fn deletion_revokes_even_when_the_user_queue_lost_its_task_row() {
    let (dir, mut storage, broker) = fixture();
    insert_input(&storage, 1, 1);
    let snapshot = std::fs::read(dir.path().join(STAGING_FILE)).unwrap();
    Connection::open(dir.path().join(STAGING_FILE))
        .unwrap()
        .execute("DELETE FROM staged_inputs", [])
        .unwrap();
    sql(&storage, "UPDATE screenshots SET is_deleted=1 WHERE id=1");
    storage.finish_staged_deletions().unwrap();
    sql(&storage, "UPDATE screenshots SET is_deleted=0 WHERE id=1");
    storage.processing_stage.shutdown();
    std::fs::write(dir.path().join(STAGING_FILE), snapshot).unwrap();
    restart_stage(dir.path(), &mut storage, &broker);
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .is_none());
}

#[test]
fn failed_deletion_keeps_the_outbox_and_blocks_further_key_claims() {
    let (_, storage, broker) = fixture();
    insert_input(&storage, 1, 1);
    insert_input(&storage, 2, 1);
    sql(&storage, "UPDATE screenshots SET is_deleted=1 WHERE id=1");
    broker.online.store(false, Ordering::SeqCst);
    assert!(storage.finish_staged_deletions().is_err());
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .is_err());
    let count: i64 = storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM app_bound_revocations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(broker.acquisitions.load(Ordering::SeqCst), 0);
    broker.online.store(true, Ordering::SeqCst);
    storage.finish_staged_deletions().unwrap();
    assert_eq!(
        storage
            .processing_stage
            .claim(&storage, Consumer::Classification)
            .unwrap()
            .unwrap()
            .receipt
            .screenshot_id,
        2
    );
}

#[test]
fn category_and_receipt_commit_atomically_and_duplicate_callback_does_not_rewrite() {
    let (_, storage, _) = fixture();
    insert_input(&storage, 1, 1);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    sql(&storage, "CREATE TRIGGER reject_receipt BEFORE INSERT ON app_bound_receipts BEGIN SELECT RAISE(ABORT,'simulated disk failure'); END");
    assert!(storage
        .commit_staged_category(&receipt, Some("Development"), Some(0.9))
        .is_err());
    assert_eq!(category(&storage, 1), None);
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    sql(&storage, "DROP TRIGGER reject_receipt");
    storage
        .commit_staged_category(&receipt, Some("Development"), Some(0.9))
        .unwrap();
    storage.processing_stage.finish(&storage, &receipt).unwrap();
    storage
        .commit_staged_category(&receipt, Some("Changed by replay"), Some(1.0))
        .unwrap();
    assert_eq!(category(&storage, 1).as_deref(), Some("Development"));
}

#[test]
fn committed_result_recovers_an_expired_process_lease_without_reprocessing() {
    let (dir, mut storage, broker) = fixture();
    insert_input(&storage, 1, 3);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage
        .commit_staged_category(&receipt, Some("Development"), Some(0.9))
        .unwrap();
    broker.online.store(false, Ordering::SeqCst);
    assert!(storage.processing_stage.finish(&storage, &receipt).is_err());
    broker.online.store(true, Ordering::SeqCst);
    broker.principal.lock().unwrap().process_identity = "10:200".into();
    broker.clock.fetch_add(LEASE_TTL_SECS + 1, Ordering::SeqCst);
    restart_stage(dir.path(), &mut storage, &broker);
    storage.processing_stage.reconcile(&storage).unwrap();
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    assert_eq!(category(&storage, 1).as_deref(), Some("Development"));
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .is_none());
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .is_some());
}

#[test]
fn recovered_completion_does_not_revoke_other_consumers() {
    let (dir, mut storage, broker) = fixture();
    insert_input(&storage, 1, 3);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage
        .commit_staged_category(&receipt, Some("Development"), Some(0.9))
        .unwrap();
    broker
        .call(Request::FinishConsumer {
            task_id: receipt.task_id.clone(),
            consumer: receipt.consumer,
            lease_id: receipt.lease_id.clone(),
        })
        .unwrap();
    let mut old_receipt = receipt.clone();
    old_receipt.lease_id = random_id();
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE app_bound_receipts SET receipt_json=?1",
            [serde_json::to_string(&old_receipt).unwrap()],
        )
        .unwrap();
    restart_stage(dir.path(), &mut storage, &broker);
    storage.processing_stage.reconcile(&storage).unwrap();
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .is_some());
}

#[test]
fn exhausted_consumer_waits_for_unlock_while_other_consumers_keep_their_grants() {
    let (dir, storage, _) = fixture();
    insert_input(&storage, 1, 3);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    let conn = Connection::open(dir.path().join(STAGING_FILE)).unwrap();
    conn.execute("UPDATE staged_work SET attempts=4 WHERE consumer=1", [])
        .unwrap();
    storage.processing_stage.release(&receipt, true).unwrap();
    storage.processing_stage.reconcile(&storage).unwrap();
    let state: String = conn
        .query_row("SELECT state FROM staged_work WHERE consumer=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "waiting_for_auth");
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .is_some());
}

#[test]
fn source_mutation_and_database_generation_fence_archive_writes() {
    let (_, storage, _) = fixture();
    insert_input(&storage, 1, 1);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    sql(&storage, "DELETE FROM ocr_results WHERE screenshot_id=1");
    assert!(!storage.staged_source_is_current(&receipt).unwrap());
    assert!(storage
        .commit_staged_category(&receipt, Some("Stale"), Some(0.9))
        .is_err());
    insert_input(&storage, 2, 1);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage.bump_db_generation();
    assert!(storage
        .commit_staged_category(&receipt, Some("Stale"), Some(0.9))
        .is_err());
    assert_eq!(category(&storage, 2), None);
}

#[test]
fn cold_dataset_reset_revokes_old_grants_and_changes_the_identity() {
    let (dir, mut storage, broker) = fixture();
    insert_input(&storage, 1, 1);
    let old_dataset = storage.processing_dataset_id().unwrap();
    let snapshot = std::fs::read(dir.path().join(STAGING_FILE)).unwrap();
    storage.processing_stage.shutdown();
    storage.processing_stage = ProcessingStaging::with_test_broker(broker.clone());
    storage
        .processing_stage
        .initialize(dir.path(), old_dataset.clone(), storage.db_generation())
        .unwrap();
    assert!(!storage.processing_stage.status().installed);
    storage.reset_processing_dataset().unwrap();
    let new_dataset = storage.processing_dataset_id().unwrap();
    assert_ne!(old_dataset, new_dataset);
    storage.processing_stage.shutdown();
    std::fs::write(dir.path().join(STAGING_FILE), snapshot).unwrap();
    storage
        .processing_stage
        .initialize(dir.path(), old_dataset, storage.db_generation())
        .unwrap();
    storage.processing_stage.refresh().unwrap();
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .is_none());
}

#[test]
fn disable_preserves_completed_state_and_uninstall_clears_enabled_status() {
    let (dir, storage, broker) = fixture();
    insert_input(&storage, 1, 3);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage
        .commit_staged_category(&receipt, None, None)
        .unwrap();
    storage.processing_stage.finish(&storage, &receipt).unwrap();
    storage
        .processing_stage
        .set_policy(false, Limits::default())
        .unwrap();
    let state: String = Connection::open(dir.path().join(STAGING_FILE))
        .unwrap()
        .query_row("SELECT state FROM staged_work WHERE consumer=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "completed");
    storage
        .processing_stage
        .set_policy(true, Limits::default())
        .unwrap();
    assert!(storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .is_none());
    broker.installed.store(false, Ordering::SeqCst);
    storage.processing_stage.refresh().unwrap();
    assert!(!storage.processing_stage.status().enabled);
    assert!(!storage.processing_stage.available());
}

#[test]
fn maintenance_visits_tasks_beyond_its_first_batch() {
    let (_, storage, broker) = fixture();
    for id in 1..=70 {
        insert_input(&storage, id, 1);
    }
    storage.processing_stage.reconcile(&storage).unwrap();
    storage.processing_stage.reconcile(&storage).unwrap();
    let mut inspected = broker.inspected.lock().unwrap().clone();
    inspected.sort();
    inspected.dedup();
    assert_eq!(inspected.len(), 70);
}

#[test]
fn malformed_durable_receipt_does_not_block_valid_completions() {
    let (_, storage, _) = fixture();
    insert_input(&storage, 1, 1);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage
        .commit_staged_category(&receipt, None, None)
        .unwrap();
    sql(&storage, "INSERT INTO app_bound_receipts(task_id,consumer,dataset_id,receipt_json) VALUES('bad',1,'bad','{')");
    assert_eq!(
        storage.pending_staged_receipts().unwrap(),
        vec![receipt.clone()]
    );
    storage.processing_stage.reconcile(&storage).unwrap();
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
}

#[test]
fn missing_local_stage_cannot_skip_dataset_retirement() {
    let (_, storage, broker) = fixture();
    insert_input(&storage, 1, 1);
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::Classification)
        .unwrap()
        .unwrap()
        .receipt;
    storage.processing_stage.shutdown();
    storage.processing_stage.retire_dataset().unwrap();
    let Response::Status(status) = broker.call(Request::Status {}).unwrap() else {
        panic!()
    };
    assert_eq!(status.active_tasks, 0);
    assert!(status.dataset_id.is_none());
    assert!(broker
        .call(Request::AcquireTask {
            task_id: receipt.task_id,
            consumer: receipt.consumer,
            ciphertext_digest: "a".repeat(64)
        })
        .is_err());
}

#[test]
fn staged_vector_and_receipt_share_a_transaction_and_an_exact_subject() {
    use crate::storage::{DerivedEmbeddingWrite, DerivedIndexJobSpec, DerivedIndexKind};
    let (_, storage, _) = fixture();
    insert_input(&storage, 1, Consumer::MiniLm.bit());
    let receipt = storage
        .processing_stage
        .claim(&storage, Consumer::MiniLm)
        .unwrap()
        .unwrap()
        .receipt;
    let spec = DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::SemanticText,
        subject_key: "1".into(),
        model_id: "test-model".into(),
        model_revision: "test-revision".into(),
        embedding_version: 1,
        source_fingerprint: "test-source".into(),
    };
    storage.ensure_derived_index_job(&spec).unwrap();
    let lease_token = storage.mark_derived_index_job_processing(&spec).unwrap();
    let write = DerivedEmbeddingWrite {
        job: spec.clone(),
        lease_token,
        vector: vec![0.6, 0.8],
    };
    let mut wrong = write.clone();
    wrong.job.subject_key = "2".into();
    assert!(storage.commit_staged_embedding(&wrong, &receipt).is_err());
    sql(&storage,"CREATE TRIGGER reject_vector_receipt BEFORE INSERT ON app_bound_receipts BEGIN SELECT RAISE(ABORT,'simulated disk failure'); END");
    assert!(storage.commit_staged_embedding(&write, &receipt).is_err());
    assert!(storage
        .get_query_visible_embedding(spec.index_kind, "1")
        .unwrap()
        .is_none());
    assert!(storage.pending_staged_receipts().unwrap().is_empty());
    sql(&storage, "DROP TRIGGER reject_vector_receipt");
    storage.commit_staged_embedding(&write, &receipt).unwrap();
    assert!(storage
        .get_query_visible_embedding(spec.index_kind, "1")
        .unwrap()
        .is_some());
    assert_eq!(storage.pending_staged_receipts().unwrap(), vec![receipt]);
}
