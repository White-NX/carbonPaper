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
        let mut process =
            identity::inspect_process(std::process::id()).expect("inspect test process");
        process.principal.runtime_id = "native-test-runtime".into();
        let principal = process.principal.clone();
        let mut ledger = Ledger::open(&directory.path().join("keys.db")).expect("open ledger");
        ledger
            .register_owner(&principal.sid, &principal.runtime_id, enabled)
            .expect("register synthetic owner");
        // Only this fixture's kernel-derived process identity is preapproved.
        // The real handler still checks the pipe client's PID and process token.
        // Signed installation and the SYSTEM peer check remain release tests.
        let image_lock = identity::locked_file(&process.image).expect("lock test executable");
        let caller = VerifiedCaller::new(process, image_lock);
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
                source_revision: 0,
                consumers: Consumer::ALL_MASK,
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
    fn set_role(&mut self, role: RuntimeRole) {
        self.principal.role = role;
        let mut process = identity::inspect_process(std::process::id()).unwrap();
        process.principal = self.principal.clone();
        let image_lock = identity::locked_file(&process.image).unwrap();
        let caller = VerifiedCaller::new(process, image_lock);
        self.cache
            .lock()
            .unwrap()
            .insert(self.principal.process_identity.clone(), Arc::new(caller));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_agent_grants_and_independent_dpapi_keys_survive_reopen_and_revoke() {
    let mut fixture = NativeBrokerFixture::new(false);
    let Response::AgentKeys(keys) = fixture
        .request(Request::AgentPrepare {
            dataset_id: fixture.dataset.clone(),
            view_path: fixture
                .directory
                .path()
                .join("agent-memory.db")
                .to_string_lossy()
                .into_owned(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    let Response::AgentStatus(status) = fixture.request(Request::AgentStatus {}).await.unwrap()
    else {
        panic!()
    };
    fixture
        .request(Request::AgentPublish {
            generation: keys.binding.generation.clone(),
            revision: status.source_revision,
            complete: true,
        })
        .await
        .unwrap();
    let token_hash = "a".repeat(64);
    let Response::AgentGrant(grant) = fixture
        .request(Request::AgentIssue {
            generation: keys.binding.generation.clone(),
            label: "synthetic reader".into(),
            token_hash: token_hash.clone(),
            expires_at_ms: None,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    fixture.reopen_ledger();
    fixture.set_role(RuntimeRole::McpReader);
    assert_eq!(
        fixture
            .request(Request::AgentAuthorize {
                token_hash: token_hash.clone()
            })
            .await
            .unwrap_err(),
        BrokerError::Busy
    );
    assert_eq!(
        fixture
            .request(Request::AgentWriterKeys {
                generation: keys.binding.generation.clone()
            })
            .await
            .unwrap_err(),
        BrokerError::AccessDenied
    );
    fixture.set_role(RuntimeRole::Desktop);
    let Response::AgentKeys(restored) = fixture
        .request(Request::AgentWriterKeys {
            generation: keys.binding.generation.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(restored.content_key.0, keys.content_key.0);
    assert_eq!(
        restored.outbox_key.as_ref().unwrap().0,
        keys.outbox_key.as_ref().unwrap().0
    );
    let Response::AgentStatus(status) = fixture.request(Request::AgentStatus {}).await.unwrap()
    else {
        panic!()
    };
    fixture
        .request(Request::AgentPublish {
            generation: keys.binding.generation.clone(),
            revision: status.source_revision,
            complete: true,
        })
        .await
        .unwrap();
    fixture.set_role(RuntimeRole::McpReader);
    let Response::AgentKeys(reader_keys) = fixture
        .request(Request::AgentReaderKeys {
            generation: keys.binding.generation,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(reader_keys.content_key.0, keys.content_key.0);
    assert!(reader_keys.outbox_key.is_none());
    let Response::AgentContext(context) = fixture
        .request(Request::AgentAuthorize {
            token_hash: token_hash.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    fixture
        .request(Request::AgentCheck {
            context: context.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        fixture.request(Request::Status {}).await.unwrap_err(),
        BrokerError::AccessDenied
    );
    fixture.set_role(RuntimeRole::Desktop);
    let generation = restored.binding.generation.clone();
    let Response::AgentRevision(revision) = fixture
        .request(Request::AgentFenceBatch {
            generation: generation.clone(),
            count: MAX_AGENT_FENCE_BATCH_SIZE,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        revision,
        status.source_revision + i64::from(MAX_AGENT_FENCE_BATCH_SIZE)
    );
    fixture.set_role(RuntimeRole::McpReader);
    assert_eq!(
        fixture
            .request(Request::AgentCheck {
                context: context.clone()
            })
            .await
            .unwrap_err(),
        BrokerError::AccessDenied
    );
    assert_eq!(
        fixture
            .request(Request::AgentReaderKeys {
                generation: generation.clone(),
            })
            .await
            .unwrap_err(),
        BrokerError::Busy
    );
    fixture.set_role(RuntimeRole::Desktop);
    fixture
        .request(Request::AgentPublish {
            generation,
            revision,
            complete: true,
        })
        .await
        .unwrap();
    fixture.set_role(RuntimeRole::McpReader);
    let Response::AgentContext(context) = fixture
        .request(Request::AgentAuthorize {
            token_hash: token_hash.clone(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    fixture.set_role(RuntimeRole::Desktop);
    fixture
        .request(Request::AgentRevoke { grant_id: grant.id })
        .await
        .unwrap();
    fixture.set_role(RuntimeRole::McpReader);
    assert_eq!(
        fixture
            .request(Request::AgentCheck { context })
            .await
            .unwrap_err(),
        BrokerError::AccessDenied
    );
    assert_eq!(
        fixture
            .request(Request::AgentAuthorize { token_hash })
            .await
            .unwrap_err(),
        BrokerError::AccessDenied
    );
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

    for consumer in Consumer::ALL {
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
    assert_eq!(state.finished_consumers, Consumer::ALL_MASK);
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
async fn native_storage_domains_preserve_dpapi_grants_scope_and_write_provenance() {
    use crate::payload::CipherKey;

    let mut fixture = NativeBrokerFixture::new(true);
    fixture
        .request(Request::AttachDataset {
            dataset_id: fixture.dataset.clone(),
        })
        .await
        .unwrap();
    let Response::Status(status) = fixture.request(Request::Status {}).await.unwrap() else {
        panic!("expected broker capabilities");
    };
    assert!(status.supported_storage_keys);

    let mut domains = Vec::new();
    for (index, purpose) in ["metadata", "vectors", "ann"].into_iter().enumerate() {
        let binding = StorageKeyBinding {
            key_id: format!("{:032x}", index + 1),
            dataset_id: fixture.dataset.clone(),
            content_dataset_id: fixture.dataset.clone(),
            public_key_fingerprint: "a".repeat(64),
            purpose: purpose.into(),
            wrapped_key_digest: format!("{:064x}", index + 1),
        };
        let key = vec![index as u8 + 7; 32];
        fixture
            .request(Request::RegisterStorageKey {
                binding: binding.clone(),
                key: TaskKey(key.clone()),
                write_origin: purpose != "metadata",
            })
            .await
            .expect("register a domain through real impersonation and DPAPI");
        let cipher = CipherKey::new(
            binding.key_id.clone(),
            binding.content_dataset_id.clone(),
            purpose.into(),
            &key,
        )
        .unwrap();
        let encrypted = cipher
            .seal("synthetic:1", b"synthetic protected history")
            .unwrap();
        domains.push((binding, key, encrypted));
    }
    fixture.reopen_ledger();
    for (binding, expected, ciphertext) in &domains {
        let Response::StorageKey(grant) = fixture
            .request(Request::AcquireStorageKey {
                binding: binding.clone(),
            })
            .await
            .expect("recover the persisted domain key")
        else {
            panic!("expected a domain grant");
        };
        assert_eq!(&grant.binding, binding);
        assert_eq!(&grant.key.0, expected);
        assert_eq!(grant.write_origin, binding.purpose != "metadata");
        let cipher = CipherKey::new(
            binding.key_id.clone(),
            binding.content_dataset_id.clone(),
            binding.purpose.clone(),
            &grant.key.0,
        )
        .unwrap();
        assert_eq!(
            cipher.open("synthetic:1", ciphertext).unwrap().as_slice(),
            b"synthetic protected history"
        );
        let mut changed = binding.clone();
        changed.content_dataset_id = random_id();
        assert_eq!(
            fixture
                .request(Request::AcquireStorageKey { binding: changed })
                .await
                .unwrap_err(),
            BrokerError::Integrity
        );
    }
    let (binding, key, _) = &domains[2];
    for (enabled, expected) in [(false, BrokerError::Disabled), (true, BrokerError::Retired)] {
        fixture
            .request(Request::SetPolicy {
                enabled,
                limits: Limits::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            fixture
                .request(Request::AcquireStorageKey {
                    binding: binding.clone()
                })
                .await
                .unwrap_err(),
            expected
        );
    }
    fixture.reopen_ledger();
    assert_eq!(
        fixture
            .request(Request::RegisterStorageKey {
                binding: binding.clone(),
                key: TaskKey(key.clone()),
                write_origin: true,
            })
            .await
            .unwrap_err(),
        BrokerError::Retired
    );
    assert_eq!(
        fixture
            .request(Request::RegisterStorageKey {
                binding: binding.clone(),
                key: TaskKey(vec![0x42; 32]),
                write_origin: false,
            })
            .await
            .unwrap_err(),
        BrokerError::Integrity
    );
    fixture
        .request(Request::RegisterStorageKey {
            binding: binding.clone(),
            key: TaskKey(key.clone()),
            write_origin: false,
        })
        .await
        .expect("an authenticated runtime may restore the exact original key");
    let Response::StorageKey(restored) = fixture
        .request(Request::AcquireStorageKey {
            binding: binding.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected restored domain grant");
    };
    assert!(restored.write_origin);
    assert_eq!(&restored.key.0, key);
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
    // Caller-cache verification now rejects a mismatched SID before dispatch.
    // Exercise the independent DPAPI token guard directly on a real pipe so
    // this regression still reaches impersonation and transaction rollback.
    let name = format!(r"\\.\pipe\CarbonPaper.DpapiSidTest.{}", random_id());
    let mut pipe = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)
        .unwrap();
    let ledger = fixture.ledger.as_ref().unwrap().clone();
    let mut who = fixture.principal.clone();
    who.sid = other_sid.into();
    let dataset = fixture.dataset.clone();
    let server = tokio::spawn(async move {
        pipe.connect().await.unwrap();
        pipe.read_u8().await.unwrap();
        run_ledger_blocking(
            ledger,
            move |ledger| {
                let protector = DpapiProtector {
                    pipe: HANDLE(pipe.as_raw_handle()),
                    sid: &who.sid,
                };
                ledger.handle(
                    &who,
                    Request::AttachDataset {
                        dataset_id: dataset.clone(),
                    },
                    now_secs(),
                    &protector,
                )?;
                assert_eq!(
                    ledger
                        .handle(
                            &who,
                            Request::PrepareTask {
                                dataset_id: dataset,
                                screenshot_id: 1,
                                source_revision: 0,
                                consumers: 1,
                                payload_bytes: 40
                            },
                            now_secs(),
                            &protector
                        )
                        .unwrap_err(),
                    BrokerError::AccessDenied
                );
                let response = ledger.handle(&who, Request::Status {}, now_secs(), &protector)?;
                Ok((pipe, response))
            },
            None,
            None,
        )
        .await
    });
    let client = tokio::task::spawn_blocking(move || {
        use std::{io::Write, os::windows::fs::OpenOptionsExt};
        use windows::Win32::Storage::FileSystem::{SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT};
        let mut client = std::fs::OpenOptions::new()
            .access_mode(transport::PIPE_CLIENT_ACCESS)
            .custom_flags((SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION).0)
            .open(&name)
            .unwrap();
        client.write_all(&[1]).unwrap();
        client
    })
    .await
    .unwrap();
    let (_pipe, Response::Status(status)) = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    else {
        panic!("expected status after rejected protection")
    };
    drop(client);
    assert_eq!(status.active_tasks, 0);
}
