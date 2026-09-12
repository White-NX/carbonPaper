use super::*;
use crate::{crypto, ledger::Principal, windows::transport};
use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

struct NativeBrokerFixture {
    directory: tempfile::TempDir,
    ledger: Option<Arc<LedgerLock>>,
    cache: CallerCache,
    principal: Principal,
    dataset: String,
}

impl NativeBrokerFixture {
    fn new(enabled: bool) -> Self {
        let directory = tempfile::tempdir().expect("create synthetic broker storage");
        let process = identity::inspect_process(std::process::id()).expect("inspect test process");
        let principal = Principal {
            runtime_id: "native-test-runtime".into(),
            ..process.principal
        };
        let mut ledger = Ledger::open(&directory.path().join("keys.db")).expect("open ledger");
        ledger
            .register_owner(&principal.sid, &principal.runtime_id, enabled)
            .expect("register synthetic owner");
        // Only this fixture's kernel-derived process identity is preapproved.
        // The real handler still checks the pipe client's PID and process token.
        // Signed installation and the SYSTEM peer check remain release tests.
        let caller = VerifiedCaller {
            principal: principal.clone(),
            image_lock: identity::locked_file(&process.image).expect("lock test executable"),
        };
        Self {
            directory,
            ledger: Some(Arc::new(LedgerLock::new(ledger))),
            cache: Arc::new(Mutex::new(HashMap::from([(
                principal.process_identity.clone(),
                Arc::new(caller),
            )]))),
            principal,
            dataset: random_id(),
        }
    }

    async fn exchange(
        &self,
        request: Request,
    ) -> (
        Result<Response>,
        std::result::Result<(), (&'static str, BrokerError)>,
    ) {
        // Tokio can still be retiring an overlapped handle after its task ends.
        // Give each fixture exchange its own name so cleanup cannot affect the
        // next connection; production keeps a separate listening instance.
        let name = format!(r"\\.\pipe\CarbonPaper.NativeTest.{}", random_id());
        let pipe = ServerOptions::new()
            .pipe_mode(PipeMode::Byte)
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .in_buffer_size(MAX_FRAME_BYTES as u32)
            .out_buffer_size(MAX_FRAME_BYTES as u32)
            .create(&name)
            .expect("create isolated async broker pipe");
        let ledger = self.ledger.as_ref().expect("open ledger").clone();
        let cache = self.cache.clone();
        let server = tokio::spawn(async move {
            pipe.connect().await.expect("accept test client");
            handle_connection_inner(pipe, ledger, cache)
                .await
                .map_err(|failure| (failure.stage, failure.error))
        });
        let client = tokio::task::spawn_blocking(move || transport::call_test_pipe(&name, request))
            .await
            .expect("client worker");
        let server = tokio::time::timeout(Duration::from_secs(12), server)
            .await
            .expect("server must finish, including its reply ACK")
            .expect("server worker");
        (client, server)
    }

    async fn request(&self, request: Request) -> Result<Response> {
        let operation = request.operation();
        let (client, server) = self.exchange(request).await;
        assert!(server.is_ok(), "{operation}: server failed at {server:?}");
        client
    }

    async fn prepare(&self, plaintext: &[u8]) -> (TaskBinding, Vec<u8>) {
        self.request(Request::AttachDataset {
            dataset_id: self.dataset.clone(),
        })
        .await
        .expect("attach synthetic dataset");
        let Response::Prepared(prepared) = self
            .request(Request::PrepareTask {
                dataset_id: self.dataset.clone(),
                screenshot_id: 1,
                consumers: 7,
                payload_bytes: plaintext.len() as u64 + 28,
            })
            .await
            .expect("prepare task through real pipe impersonation and DPAPI")
        else {
            panic!("expected prepared task");
        };
        let ciphertext = crypto::encrypt_input(&prepared.key, &prepared.task, plaintext)
            .expect("encrypt synthetic input");
        self.request(Request::ActivateTask {
            task_id: prepared.task.task_id.clone(),
            ciphertext_digest: crypto::digest(&ciphertext),
        })
        .await
        .expect("activate task");
        (prepared.task, ciphertext)
    }

    fn reopen_ledger(&mut self) {
        let ledger = self.ledger.take().expect("open ledger");
        assert_eq!(
            Arc::strong_count(&ledger),
            1,
            "all requests must finish first"
        );
        drop(ledger);
        self.ledger = Some(Arc::new(LedgerLock::new(
            Ledger::open(&self.directory.path().join("keys.db")).expect("reopen durable ledger"),
        )));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_policy_round_trip_uses_client_and_server() {
    let fixture = NativeBrokerFixture::new(false);
    let limits = Limits {
        retention_days: 1,
        capacity_bytes: 256 * 1024 * 1024,
    };
    for enabled in [true, false, true] {
        let Response::Status(changed) = fixture
            .request(Request::SetPolicy { enabled, limits })
            .await
            .expect("policy change must reach the client")
        else {
            panic!("expected policy status");
        };
        assert_eq!(changed.enabled, enabled);
        assert_eq!(changed.limits, limits);
        let Response::Status(refreshed) = fixture.request(Request::Status {}).await.unwrap() else {
            panic!("expected refreshed status");
        };
        assert_eq!(refreshed.enabled, enabled);
        assert_eq!(refreshed.limits, limits);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_dpapi_keys_survive_ledger_reopen_and_finish_all_consumers() {
    let mut fixture = NativeBrokerFixture::new(true);
    let plaintext = b"synthetic background processing input";
    let (task, ciphertext) = fixture.prepare(plaintext).await;
    fixture.reopen_ledger();

    for consumer in [Consumer::Classification, Consumer::MiniLm, Consumer::Clip] {
        let Response::Lease(lease) = fixture
            .request(Request::AcquireTask {
                task_id: task.task_id.clone(),
                consumer,
                ciphertext_digest: crypto::digest(&ciphertext),
            })
            .await
            .expect("recover DPAPI-wrapped key from persisted storage")
        else {
            panic!("expected task lease");
        };
        assert_eq!(lease.task, task);
        assert_eq!(
            crypto::decrypt_input(&lease.key, &lease.task, &ciphertext)
                .expect("decrypt synthetic input")
                .as_slice(),
            plaintext
        );
        fixture
            .request(Request::FinishConsumer {
                task_id: task.task_id.clone(),
                consumer,
                lease_id: lease.lease_id,
            })
            .await
            .expect("acknowledge consumer completion");
    }
    let Response::TaskState(state) = fixture
        .request(Request::InspectTask {
            task_id: task.task_id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected task state");
    };
    assert!(state.retired);
    assert_eq!(state.finished_consumers, 7);
    assert_eq!(
        fixture
            .request(Request::AcquireTask {
                task_id: task.task_id,
                consumer: Consumer::Classification,
                ciphertext_digest: crypto::digest(&ciphertext),
            })
            .await
            .unwrap_err(),
        BrokerError::Retired
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_disable_revokes_keys_and_returns_errors_over_the_pipe() {
    let fixture = NativeBrokerFixture::new(true);
    let (task, ciphertext) = fixture.prepare(b"synthetic input to revoke").await;
    for (enabled, expected) in [(false, BrokerError::Disabled), (true, BrokerError::Retired)] {
        fixture
            .request(Request::SetPolicy {
                enabled,
                limits: Limits::default(),
            })
            .await
            .expect("apply policy");
        assert_eq!(
            fixture
                .request(Request::AcquireTask {
                    task_id: task.task_id.clone(),
                    consumer: Consumer::Clip,
                    ciphertext_digest: crypto::digest(&ciphertext),
                })
                .await
                .unwrap_err(),
            expected
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_unapproved_process_is_rejected_before_the_request() {
    let fixture = NativeBrokerFixture::new(true);
    fixture.cache.lock().unwrap().clear();
    let (client, server) = fixture.exchange(Request::Status {}).await;
    assert!(client.is_err());
    assert_eq!(server, Err(("verify_caller", BrokerError::AccessDenied)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_dpapi_rejects_a_different_pipe_token_sid() {
    let fixture = NativeBrokerFixture::new(true);
    let other_sid = "S-1-5-21-1-2-3-1234";
    assert_ne!(fixture.principal.sid, other_sid);
    fixture
        .ledger
        .as_ref()
        .unwrap()
        .lock()
        .await
        .register_owner(other_sid, &fixture.principal.runtime_id, true)
        .unwrap();
    {
        let mut cache = fixture.cache.lock().unwrap();
        Arc::get_mut(cache.get_mut(&fixture.principal.process_identity).unwrap())
            .unwrap()
            .principal
            .sid = other_sid.into();
    }
    fixture
        .request(Request::AttachDataset {
            dataset_id: fixture.dataset.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request(Request::PrepareTask {
                dataset_id: fixture.dataset.clone(),
                screenshot_id: 1,
                consumers: 1,
                payload_bytes: 40,
            })
            .await
            .unwrap_err(),
        BrokerError::AccessDenied
    );
    let Response::Status(status) = fixture.request(Request::Status {}).await.unwrap() else {
        panic!("expected status after rejected protection");
    };
    assert_eq!(status.active_tasks, 0);
}
