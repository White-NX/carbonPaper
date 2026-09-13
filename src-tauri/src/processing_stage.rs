//! Persistent inputs for newly captured work. This module never unwraps archive
//! row keys and never changes CredentialManagerState's archive authorization.
use crate::{
    background_scheduler::{AutomaticSliceContext, ScheduledSliceResult},
    ml_protocol::{MlImageInput, MlSemanticModel},
    semantic_runtime::SemanticRuntimeState,
    storage::StorageState,
};
use carbonpaper_app_bound::{
    crypto,
    protocol::{self, BrokerError, BrokerStatus, Consumer, Limits, Request, Response, TaskBinding},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{AppHandle, Manager};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const STAGING_FILE: &str = "processing-staging.db";
const CLIP_SIZE: u32 = 224;
const MAX_ATTEMPTS: i64 = 5;

pub(crate) trait Broker: Send + Sync {
    fn call(&self, request: Request) -> protocol::Result<Response>;
    fn installed(&self) -> protocol::Result<bool>;
    fn supported(&self) -> bool;
}
struct NativeBroker;
impl Broker for NativeBroker {
    fn installed(&self) -> protocol::Result<bool> {
        carbonpaper_app_bound::windows::identity::active_runtime().map(|runtime| runtime.is_some())
    }
    fn supported(&self) -> bool {
        crate::app_bound::supported_build()
    }
    fn call(&self, request: Request) -> protocol::Result<Response> {
        // Production trust never has a developer directory or environment-key
        // override. Unit tests inject an in-process broker explicitly.
        if !crate::app_bound::supported_build() {
            return Err(BrokerError::VersionMismatch);
        }
        if !crate::app_bound::protected_environment_ready() {
            return Err(BrokerError::AccessDenied);
        }
        carbonpaper_app_bound::windows::call(request)
    }
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ClipInput {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub model_revision: String,
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct ProcessingInput {
    pub image_hash: String,
    pub window_title: String,
    pub process_name: String,
    pub timestamp_ms: i64,
    pub ocr_text: String,
    pub source_revision: i64,
    pub clip: Option<ClipInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskReceipt {
    pub task_id: String,
    pub dataset_id: String,
    pub screenshot_id: i64,
    pub consumer: Consumer,
    pub lease_id: String,
    pub db_generation: u64,
    pub source_revision: i64,
}

pub(crate) struct StagedWork {
    pub input: ProcessingInput,
    pub receipt: TaskReceipt,
}

struct Store {
    connection: Connection,
    dataset_id: String,
    generation: u64,
}

struct IssuedReceipt {
    receipt: TaskReceipt,
    deadline: i64,
    max_deadline: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessingStatus {
    pub installed: bool,
    pub supported: bool,
    pub enabled: bool,
    pub available: bool,
    pub reason: Option<String>,
    pub limits: Limits,
    pub active_tasks: u64,
    pub active_bytes: u64,
    pub waiting_for_unlock: u64,
}

impl Default for ProcessingStatus {
    fn default() -> Self {
        Self {
            installed: false,
            supported: crate::app_bound::supported_build(),
            enabled: false,
            available: false,
            reason: None,
            limits: Limits::default(),
            active_tasks: 0,
            active_bytes: 0,
            waiting_for_unlock: 0,
        }
    }
}

pub(crate) struct ProcessingStaging {
    store: Mutex<Option<Store>>,
    status: Mutex<ProcessingStatus>,
    available: AtomicBool,
    broker: Arc<dyn Broker>,
    // Only receipts issued by this process can authorize a new archive write.
    // Python and user-writable SQLite rows cannot change their screenshot scope.
    issued: Mutex<HashMap<String, IssuedReceipt>>,
    scan_cursor: Mutex<String>,
    capture_lock: Mutex<()>,
}

impl ProcessingStaging {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(None),
            status: Mutex::new(ProcessingStatus::default()),
            available: AtomicBool::new(false),
            broker: Arc::new(NativeBroker),
            issued: Mutex::new(HashMap::new()),
            scan_cursor: Mutex::new(String::new()),
            capture_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_broker(broker: Arc<dyn Broker>) -> Self {
        Self {
            broker,
            ..Self::new()
        }
    }

    pub fn initialize(
        &self,
        directory: &Path,
        dataset_id: String,
        generation: u64,
    ) -> Result<(), String> {
        self.available.store(false, Ordering::Release);
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .clear();
        self.scan_cursor
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .clear();
        let connection =
            Connection::open(directory.join(STAGING_FILE)).map_err(|e| e.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE;
            PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA auto_vacuum=INCREMENTAL;
            CREATE TABLE IF NOT EXISTS staged_inputs (
                task_id TEXT PRIMARY KEY,dataset_id TEXT NOT NULL,screenshot_id INTEGER NOT NULL,
                binding TEXT NOT NULL,payload BLOB,digest TEXT NOT NULL,expires INTEGER NOT NULL,
                state TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(dataset_id,screenshot_id)
            );
            CREATE TABLE IF NOT EXISTS staged_work (
                task_id TEXT NOT NULL REFERENCES staged_inputs(task_id) ON DELETE CASCADE,
                consumer INTEGER NOT NULL,state TEXT NOT NULL DEFAULT 'pending',lease_id TEXT,
                deadline INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt INTEGER NOT NULL DEFAULT 0,error_code TEXT,
                PRIMARY KEY(task_id,consumer)
            );
            CREATE INDEX IF NOT EXISTS staged_work_ready ON staged_work(consumer,state,next_attempt);")
            .map_err(|e|e.to_string())?;
        // A restored archive cannot attach inputs from the previous dataset.
        connection
            .execute(
                "DELETE FROM staged_inputs WHERE dataset_id!=?1",
                [&dataset_id],
            )
            .map_err(|e| e.to_string())?;
        connection
            .execute(
                "UPDATE staged_work SET state='pending',lease_id=NULL WHERE state='processing'",
                [],
            )
            .map_err(|e| e.to_string())?;
        *self.store.lock().map_err(|_| "staging lock poisoned")? = Some(Store {
            connection,
            dataset_id,
            generation,
        });
        Ok(())
    }

    pub fn shutdown(&self) {
        self.available.store(false, Ordering::Release);
        if let Ok(mut issued) = self.issued.lock() {
            issued.clear();
        }
        if let Ok(mut store) = self.store.lock() {
            *store = None;
        }
    }

    pub fn initialized(&self) -> bool {
        self.store.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    fn with_store<T>(&self, read: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let guard = self.store.lock().map_err(|_| "staging lock poisoned")?;
        read(guard.as_ref().ok_or("staging is not initialized")?)
    }

    fn call_broker_logged(
        &self,
        request: Request,
        consumer: Option<Consumer>,
    ) -> protocol::Result<Response> {
        let operation = request.operation();
        let result = self.broker.call(request);
        if let Err(error) = &result {
            if operation != "status" || *error != BrokerError::Unavailable {
                tracing::warn!(
                    "[APP_BOUND] broker operation={} consumer={} result=error code={}",
                    operation,
                    consumer.map_or("none", Consumer::name),
                    error.code(),
                );
            }
        }
        result
    }

    pub fn status(&self) -> ProcessingStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    pub fn refresh(&self) -> Result<(), String> {
        let supported = self.broker.supported();
        let installed = match self.broker.installed() {
            Ok(value) => value,
            Err(error) => {
                self.available.store(false, Ordering::Release);
                let mut status = self.status.lock().map_err(|_| "staging lock poisoned")?;
                status.installed = true;
                status.available = false;
                status.reason = Some(error.to_string());
                return Err(error.to_string());
            }
        };
        if !installed || !supported {
            self.available.store(false, Ordering::Release);
            let mut status = self.status.lock().map_err(|_| "staging lock poisoned")?;
            status.installed = installed;
            status.supported = supported;
            status.enabled = false;
            status.available = false;
            status.active_tasks = 0;
            status.active_bytes = 0;
            status.reason = Some(
                if supported {
                    "not_installed"
                } else {
                    "development_build"
                }
                .into(),
            );
            return Ok(());
        }
        let result = (|| -> protocol::Result<BrokerStatus> {
            let Response::Status(status) = self.call_broker_logged(Request::Status {}, None)?
            else {
                return Err(BrokerError::Integrity);
            };
            let dataset = self
                .with_store(|s| Ok(s.dataset_id.clone()))
                .map_err(|_| BrokerError::Storage)?;
            if status.dataset_id.as_deref() != Some(&dataset) {
                self.call_broker_logged(
                    Request::AttachDataset {
                        dataset_id: dataset,
                    },
                    None,
                )?;
            }
            Ok(status)
        })();
        match result {
            Ok(broker) => {
                self.available.store(broker.enabled, Ordering::Release);
                let waiting = self
                    .with_store(|s| {
                        s.connection
                            .query_row(
                                "SELECT COUNT(*) FROM staged_work WHERE state='waiting_for_auth'",
                                [],
                                |r| r.get::<_, i64>(0),
                            )
                            .map(|v| v as u64)
                            .map_err(|e| e.to_string())
                    })
                    .unwrap_or(0);
                *self.status.lock().map_err(|_| "staging lock poisoned")? = ProcessingStatus {
                    installed: true,
                    supported,
                    enabled: broker.enabled,
                    available: broker.enabled,
                    reason: if broker.enabled {
                        None
                    } else {
                        Some("disabled".into())
                    },
                    limits: broker.limits,
                    active_tasks: broker.active_tasks,
                    active_bytes: broker.active_bytes,
                    waiting_for_unlock: waiting,
                };
                Ok(())
            }
            Err(error) => {
                self.available.store(false, Ordering::Release);
                let mut status = self.status.lock().map_err(|_| "staging lock poisoned")?;
                status.installed = true;
                status.available = false;
                status.reason = Some(error.to_string());
                Err(error.to_string())
            }
        }
    }

    pub fn set_policy(&self, enabled: bool, limits: Limits) -> Result<(), String> {
        limits.validate().map_err(|e| e.to_string())?;
        if !enabled {
            self.available.store(false, Ordering::Release);
        }
        self.call_broker_logged(Request::SetPolicy { enabled, limits }, None)
            .map_err(|e| e.to_string())?;
        if !enabled {
            self.issued
                .lock()
                .map_err(|_| "staging lock poisoned")?
                .clear();
            self.with_store(|s|s.connection.execute_batch("UPDATE staged_work SET state='waiting_for_auth',lease_id=NULL WHERE state!='completed';
                UPDATE staged_inputs SET payload=NULL,state='retired';").map_err(|e|e.to_string()))?;
        }
        self.refresh()?;
        if enabled {
            self.make_local_room(0)?;
        }
        Ok(())
    }

    pub fn disable_if_installed(&self) -> Result<(), String> {
        self.available.store(false, Ordering::Release);
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .clear();
        if self.broker.supported() && self.broker.installed().map_err(|e| e.to_string())? {
            let Response::Status(status) = self
                .call_broker_logged(Request::Status {}, None)
                .map_err(|e| e.to_string())?
            else {
                return Err(BrokerError::Integrity.to_string());
            };
            self.set_policy(false, status.limits)?;
        }
        Ok(())
    }

    pub fn retire_dataset(&self) -> Result<(), String> {
        self.available.store(false, Ordering::Release);
        if self.broker.supported() && self.broker.installed().map_err(|e| e.to_string())? {
            // Restore/switch must retire the service's actual dataset even if
            // the local staging DB is corrupt, absent, or from an old backup.
            let Response::Status(status) = self
                .call_broker_logged(Request::Status {}, None)
                .map_err(|e| e.to_string())?
            else {
                return Err(BrokerError::Integrity.to_string());
            };
            if let Some(dataset_id) = status.dataset_id {
                self.call_broker_logged(Request::RetireDataset { dataset_id }, None)
                    .map_err(|e| e.to_string())?;
            }
        }
        self.discard_revoked_inputs()
    }

    pub fn stage(
        &self,
        screenshot_id: i64,
        input: &ProcessingInput,
        consumers: u8,
    ) -> Result<bool, String> {
        let _capture = self
            .capture_lock
            .lock()
            .map_err(|_| "staging lock poisoned")?;
        if !self.available() || consumers == 0 {
            return Ok(false);
        }
        let (dataset, generation) =
            self.with_store(|s| Ok((s.dataset_id.clone(), s.generation)))?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(input).map_err(|e| e.to_string())?);
        if bytes.len() as u64 > protocol::MAX_INPUT_BYTES {
            return Err(BrokerError::LimitExceeded.to_string());
        }
        self.make_local_room(bytes.len() as u64 + 28)?;
        let Response::Prepared(prepared) = self
            .call_broker_logged(
                Request::PrepareTask {
                    dataset_id: dataset.clone(),
                    screenshot_id,
                    consumers,
                    payload_bytes: bytes.len() as u64 + 28,
                },
                None,
            )
            .map_err(|e| e.to_string())?
        else {
            return Err(BrokerError::Integrity.to_string());
        };
        let encrypted = crypto::encrypt_input(&prepared.key, &prepared.task, &bytes)
            .map_err(|e| e.to_string())?;
        let digest = crypto::digest(&encrypted);
        let binding = serde_json::to_string(&prepared.task).map_err(|e| e.to_string())?;
        let stored=self.with_store(|s| {
            if s.generation!=generation || s.dataset_id!=dataset { return Err("staging dataset changed".into()); }
            let tx=s.connection.unchecked_transaction().map_err(|e|e.to_string())?;
            tx.execute("INSERT INTO staged_inputs(task_id,dataset_id,screenshot_id,binding,payload,digest,expires,state,created)
                VALUES(?1,?2,?3,?4,?5,?6,?7,'preparing',?8)",params![prepared.task.task_id,dataset,screenshot_id,binding,encrypted,digest,prepared.task.expires_at,protocol::now_secs()]).map_err(|e|e.to_string())?;
            for consumer in Consumer::ALL { if consumers&consumer.bit()!=0 {
                tx.execute("INSERT INTO staged_work(task_id,consumer) VALUES(?1,?2)",params![prepared.task.task_id,consumer.bit()]).map_err(|e|e.to_string())?;
            }}
            tx.commit().map_err(|e|e.to_string())
        });
        if let Err(error) = stored {
            let _ = self.call_broker_logged(
                Request::RevokeTask {
                    task_id: prepared.task.task_id,
                },
                None,
            );
            return Err(error);
        }
        // A durable prepare can be activated again after an interrupted reply.
        // Keep it owned by staging so the legacy queue does not duplicate work.
        let _ = self.activate(&prepared.task.task_id, &digest);
        Ok(true)
    }

    fn make_local_room(&self, incoming: u64) -> Result<(), String> {
        let capacity = self.status().limits.capacity_bytes;
        loop {
            let usage = self.with_store(|s| {
                s.connection
                    .query_row(
                        "SELECT COALESCE(SUM(length(payload)),0) FROM staged_inputs",
                        [],
                        |r| r.get::<_, i64>(0).map(|v| v as u64),
                    )
                    .map_err(|e| e.to_string())
            })?;
            if usage.saturating_add(incoming) <= capacity {
                return Ok(());
            }
            let tasks=self.with_store(|s| {
                let mut stmt=s.connection.prepare("SELECT task_id,length(payload) FROM staged_inputs WHERE payload IS NOT NULL ORDER BY created,task_id LIMIT 128").map_err(|e|e.to_string())?;
                let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)? as u64))).map_err(|e|e.to_string())?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
            })?;
            if tasks.is_empty() {
                return Err(BrokerError::LimitExceeded.to_string());
            }
            let mut remaining = usage.saturating_add(incoming);
            let mut revoke = Vec::new();
            for (id, bytes) in tasks {
                if remaining <= capacity {
                    break;
                }
                remaining = remaining.saturating_sub(bytes);
                revoke.push(id);
            }
            self.retire_tasks(&revoke)?;
        }
    }

    fn activate(&self, id: &str, digest: &str) -> Result<(), String> {
        self.call_broker_logged(
            Request::ActivateTask {
                task_id: id.into(),
                ciphertext_digest: digest.into(),
            },
            None,
        )
        .map_err(|e| e.to_string())?;
        self.with_store(|s| {
            s.connection
                .execute(
                    "UPDATE staged_inputs SET state='active' WHERE task_id=?1",
                    [id],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    pub fn has_ready(&self, consumer: Consumer) -> bool {
        self.available() && self.with_store(|s|s.connection.query_row("SELECT EXISTS(SELECT 1 FROM staged_work w JOIN staged_inputs i USING(task_id)
            WHERE w.consumer=?1 AND w.state='pending' AND w.next_attempt<=?2 AND i.state='active' AND i.payload IS NOT NULL)",
            params![consumer.bit(),protocol::now_secs()],|r|r.get(0)).map_err(|e|e.to_string())).unwrap_or(false)
    }

    pub fn owns_screenshot(&self, id: i64, consumer: Consumer) -> bool {
        self.available() && self.with_store(|s|s.connection.query_row("SELECT EXISTS(SELECT 1 FROM staged_inputs i JOIN staged_work w USING(task_id)
            WHERE i.screenshot_id=?1 AND w.consumer=?2 AND w.state IN ('pending','processing') AND i.state IN ('active','preparing'))",
            params![id,consumer.bit()],|r|r.get(0)).map_err(|e|e.to_string())).unwrap_or(false)
    }

    pub(crate) fn classification_in_flight(&self) -> bool {
        self.with_store(|s|s.connection.query_row("SELECT EXISTS(SELECT 1 FROM staged_work WHERE consumer=1 AND state='processing' AND deadline>?1)",
            [protocol::now_secs()],|r|r.get(0)).map_err(|e|e.to_string())).unwrap_or(true)
    }

    pub fn claim(
        &self,
        storage: &StorageState,
        consumer: Consumer,
    ) -> Result<Option<StagedWork>, String> {
        if !self.available() {
            return Ok(None);
        }
        // Drain deletion intent before any key request, including after a crash.
        storage.finish_staged_deletions()?;
        for _ in 0..8 {
            let (row,dataset,generation)=self.with_store(|s| {
            let row=s.connection.query_row("SELECT i.task_id,CASE WHEN length(i.binding)<=4096 THEN i.binding ELSE '' END,
                CASE WHEN length(i.payload) BETWEEN 28 AND ?3 THEN i.payload ELSE NULL END,i.digest,i.screenshot_id
                FROM staged_work w JOIN staged_inputs i USING(task_id)
                WHERE w.consumer=?1 AND w.state='pending' AND w.next_attempt<=?2 AND i.state='active'
                ORDER BY i.created,i.task_id LIMIT 1",params![consumer.bit(),protocol::now_secs(),protocol::MAX_INPUT_BYTES as i64+28],
                |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<Vec<u8>>>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))
                .optional().map_err(|e|e.to_string())?;
            Ok((row,s.dataset_id.clone(),s.generation))
        })?;
            let Some((id, binding, encrypted, digest, screenshot_id)) = row else {
                return Ok(None);
            };
            let local = serde_json::from_str::<TaskBinding>(&binding).ok();
            let valid = local.as_ref().is_some_and(|binding| {
                binding.task_id == id
                    && protocol::valid_id(&id)
                    && binding.dataset_id == dataset
                    && binding.screenshot_id == screenshot_id
                    && binding.input_version == protocol::INPUT_VERSION
                    && binding.consumers & consumer.bit() != 0
                    && encrypted
                        .as_ref()
                        .is_some_and(|bytes| bytes.len() as u64 == binding.payload_bytes)
                    && protocol::valid_id(&digest)
            });
            if !valid
                || generation != storage.db_generation()
                || !storage.staged_screenshot_active(screenshot_id)?
            {
                self.retire_task(&id)?;
                continue;
            }
            let local = local.unwrap();
            let encrypted = encrypted.unwrap();
            if crypto::digest(&encrypted) != digest {
                self.retire_task(&id)?;
                continue;
            }
            let lease = match self.call_broker_logged(
                Request::AcquireTask {
                    task_id: id.clone(),
                    consumer,
                    ciphertext_digest: digest,
                },
                Some(consumer),
            ) {
                Ok(Response::Lease(lease)) => lease,
                Err(error) => {
                    self.defer_broker_error(&id, consumer, error)?;
                    continue;
                }
                _ => return Err(BrokerError::Integrity.to_string()),
            };
            let decoded = (|| {
                if local != lease.task
                    || consumer != lease.consumer
                    || !protocol::valid_id(&lease.lease_id)
                    || lease.deadline <= protocol::now_secs()
                {
                    return Err(BrokerError::Integrity);
                }
                let bytes = crypto::decrypt_input(&lease.key, &lease.task, &encrypted)?;
                serde_json::from_slice::<ProcessingInput>(&bytes)
                    .map_err(|_| BrokerError::Integrity)
            })();
            let input = match decoded {
                Ok(input) => input,
                Err(_) => {
                    let _ = self.broker.call(Request::ReleaseLease {
                        task_id: id.clone(),
                        consumer,
                        lease_id: lease.lease_id.clone(),
                    });
                    self.retire_task(&id)?;
                    continue;
                }
            };
            let receipt = TaskReceipt {
                task_id: id.clone(),
                dataset_id: lease.task.dataset_id.clone(),
                screenshot_id: lease.task.screenshot_id,
                consumer,
                lease_id: lease.lease_id.clone(),
                db_generation: generation,
                source_revision: input.source_revision,
            };
            let claimed = (|| {
                if !storage.staged_source_is_current(&receipt)? {
                    return Err("staged source changed".into());
                }
                self.with_store(|s| {
            if s.generation!=generation || s.dataset_id!=receipt.dataset_id { return Err("staging dataset changed".into()); }
            let changed=s.connection.execute("UPDATE staged_work SET state='processing',lease_id=?3,deadline=?4 WHERE task_id=?1 AND consumer=?2 AND state='pending'
                AND EXISTS(SELECT 1 FROM staged_inputs WHERE task_id=?1 AND state='active')",
                params![id,consumer.bit(),lease.lease_id,lease.deadline]).map_err(|e|e.to_string())?;
            if changed!=1 { return Err("staging claim lost".into()); } Ok(())
        })?;
                let mut issued = self.issued.lock().map_err(|_| "staging lock poisoned")?;
                issued.retain(|_, value| value.deadline > protocol::now_secs());
                if issued.len() >= 128 {
                    return Err("too many staged leases".into());
                }
                issued.insert(
                    receipt.lease_id.clone(),
                    IssuedReceipt {
                        receipt: receipt.clone(),
                        deadline: lease.deadline,
                        max_deadline: protocol::now_secs() + 15 * 60,
                    },
                );
                Ok::<(), String>(())
            })();
            if claimed.is_err() {
                let _ = self.broker.call(Request::ReleaseLease {
                    task_id: id.clone(),
                    consumer,
                    lease_id: lease.lease_id.clone(),
                });
                self.retire_task(&id)?;
                continue;
            }
            // TaskKey is erased here; only this one task's input reaches inference.
            return Ok(Some(StagedWork { input, receipt }));
        }
        Ok(None)
    }

    fn defer_broker_error(
        &self,
        id: &str,
        consumer: Consumer,
        error: BrokerError,
    ) -> Result<(), String> {
        if error == BrokerError::Retired {
            if let Ok(Response::TaskState(state)) =
                self.call_broker_logged(Request::InspectTask { task_id: id.into() }, Some(consumer))
            {
                let terminal = state.retired
                    || (state.finished_consumers | state.abandoned_consumers) & consumer.bit() != 0;
                self.apply_task_state(id, &state)?;
                if terminal {
                    return Ok(());
                }
            }
        }
        let terminal = matches!(
            error,
            BrokerError::Retired
                | BrokerError::DatasetMismatch
                | BrokerError::Integrity
                | BrokerError::Disabled
        );
        self.with_store(|s| {
            s.connection.execute("UPDATE staged_work SET state=?3,next_attempt=?4,error_code=?5 WHERE task_id=?1 AND consumer=?2",
                params![id,consumer.bit(),if terminal { "waiting_for_auth" } else { "pending" },protocol::now_secs()+30,error.to_string()]).map_err(|e|e.to_string())?;
            Ok(())
        })?;
        if terminal {
            self.retire_task(id)?;
        }
        Ok(())
    }

    pub fn release(&self, receipt: &TaskReceipt, failed: bool) -> Result<(), String> {
        self.check_receipt(receipt)?;
        let exhausted=self.with_store(|s|s.connection.query_row("SELECT attempts FROM staged_work WHERE task_id=?1 AND consumer=?2 AND lease_id=?3",
            params![receipt.task_id,receipt.consumer.bit(),receipt.lease_id],|r|r.get::<_,i64>(0)).map(|attempts|attempts+i64::from(failed)>=MAX_ATTEMPTS).map_err(|e|e.to_string()))?;
        if exhausted {
            self.call_broker_logged(
                Request::AbandonConsumer {
                    task_id: receipt.task_id.clone(),
                    consumer: receipt.consumer,
                },
                Some(receipt.consumer),
            )
            .map_err(|e| e.to_string())?;
        }
        let _ = self.broker.call(Request::ReleaseLease {
            task_id: receipt.task_id.clone(),
            consumer: receipt.consumer,
            lease_id: receipt.lease_id.clone(),
        });
        self.with_store(|s| {
            let attempts:i64=s.connection.query_row("SELECT attempts FROM staged_work WHERE task_id=?1 AND consumer=?2",
                params![receipt.task_id,receipt.consumer.bit()],|r|r.get(0)).map_err(|e|e.to_string())?;
            let attempts=attempts+i64::from(failed);
            s.connection.execute("UPDATE staged_work SET state=?4,lease_id=NULL,attempts=?5,next_attempt=?6,error_code=?7
                WHERE task_id=?1 AND consumer=?2 AND lease_id=?3",params![receipt.task_id,receipt.consumer.bit(),receipt.lease_id,
                if attempts>=MAX_ATTEMPTS { "waiting_for_auth" } else { "pending" },attempts,
                protocol::now_secs()+if failed { 60*(1i64<<attempts.min(6)) } else { 5 },if failed { "processing_failed" } else { "deferred" }]).map_err(|e|e.to_string())?;
            Ok(())
        })?;
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .remove(&receipt.lease_id);
        Ok(())
    }

    pub fn check_receipt(&self, receipt: &TaskReceipt) -> Result<(), String> {
        if !self.available() {
            return Err(BrokerError::Disabled.to_string());
        }
        {
            let issued = self.issued.lock().map_err(|_| "staging lock poisoned")?;
            let Some(original) = issued.get(&receipt.lease_id) else {
                return Err(BrokerError::LeaseExpired.to_string());
            };
            if original.receipt != *receipt {
                return Err(BrokerError::AccessDenied.to_string());
            }
            if original.deadline <= protocol::now_secs() {
                return Err(BrokerError::LeaseExpired.to_string());
            }
        }
        self.with_store(|s| {
            if s.generation!=receipt.db_generation || s.dataset_id!=receipt.dataset_id { return Err("staging generation changed".into()); }
            let valid:bool=s.connection.query_row("SELECT EXISTS(SELECT 1 FROM staged_work w JOIN staged_inputs i USING(task_id) WHERE w.task_id=?1 AND w.consumer=?2
                AND w.state='processing' AND w.lease_id=?3 AND w.deadline>?4 AND i.state='active')",params![receipt.task_id,receipt.consumer.bit(),receipt.lease_id,protocol::now_secs()],|r|r.get(0)).map_err(|e|e.to_string())?;
            if valid { Ok(()) } else { Err(BrokerError::LeaseExpired.to_string()) }
        })
    }

    fn renew_live_leases(&self) -> Result<(), String> {
        let now = protocol::now_secs();
        let renew = self
            .issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .values()
            .filter(|v| v.deadline > now && v.deadline <= now + 60 && now < v.max_deadline)
            .map(|v| (v.receipt.clone(), v.max_deadline))
            .collect::<Vec<_>>();
        for (receipt, maximum) in renew {
            match self.broker.call(Request::RenewLease {
                task_id: receipt.task_id.clone(),
                consumer: receipt.consumer,
                lease_id: receipt.lease_id.clone(),
            }) {
                Ok(Response::Deadline(deadline)) => {
                    let deadline = deadline.min(maximum);
                    self.with_store(|s|s.connection.execute("UPDATE staged_work SET deadline=?4 WHERE task_id=?1 AND consumer=?2 AND lease_id=?3 AND state='processing'",
                        params![receipt.task_id,receipt.consumer.bit(),receipt.lease_id,deadline]).map(|_|()).map_err(|e|e.to_string()))?;
                    if let Some(issued) = self
                        .issued
                        .lock()
                        .map_err(|_| "staging lock poisoned")?
                        .get_mut(&receipt.lease_id)
                    {
                        issued.deadline = deadline;
                    }
                }
                Err(
                    BrokerError::LeaseExpired | BrokerError::Retired | BrokerError::DatasetMismatch,
                ) => {
                    self.issued
                        .lock()
                        .map_err(|_| "staging lock poisoned")?
                        .remove(&receipt.lease_id);
                }
                Err(error) => return Err(error.to_string()),
                _ => return Err(BrokerError::Integrity.to_string()),
            }
        }
        Ok(())
    }

    pub fn discard_revoked_inputs(&self) -> Result<(), String> {
        self.available.store(false, Ordering::Release);
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .clear();
        if self.initialized() {
            self.with_store(|s| {
                s.connection
                    .execute("DELETE FROM staged_inputs", [])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?;
        }
        Ok(())
    }

    pub fn finish(&self, storage: &StorageState, receipt: &TaskReceipt) -> Result<(), String> {
        // The archive receipt is already durable. It is authoritative for retry
        // intent, but only the broker decides whether a key remains available.
        self.call_broker_logged(
            Request::FinishConsumer {
                task_id: receipt.task_id.clone(),
                consumer: receipt.consumer,
                lease_id: receipt.lease_id.clone(),
            },
            Some(receipt.consumer),
        )
        .map_err(|e| e.to_string())?;
        self.with_store(|s| {
            s.connection.execute("UPDATE staged_work SET state='completed',lease_id=NULL WHERE task_id=?1 AND consumer=?2",
                params![receipt.task_id,receipt.consumer.bit()]).map_err(|e|e.to_string())?;
            s.connection.execute("UPDATE staged_inputs SET payload=NULL,state='retired' WHERE task_id=?1 AND NOT EXISTS
                (SELECT 1 FROM staged_work WHERE task_id=?1 AND state NOT IN ('completed','waiting_for_auth'))",[&receipt.task_id]).map_err(|e|e.to_string())?;
            Ok(())
        })?;
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .remove(&receipt.lease_id);
        storage.clear_staged_receipt(receipt)
    }

    pub fn retire_task(&self, id: &str) -> Result<(), String> {
        self.retire_tasks(&[id.into()])
    }

    fn retire_tasks(&self, ids: &[String]) -> Result<(), String> {
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .retain(|_, value| !ids.contains(&value.receipt.task_id));
        self.with_store(|s| {
            for id in ids {
                s.connection
                    .execute(
                        "UPDATE staged_inputs SET state='revoke_pending' WHERE task_id=?1",
                        [id],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })?;
        for chunk in ids.chunks(128) {
            let valid = chunk
                .iter()
                .filter(|id| protocol::valid_id(id))
                .cloned()
                .collect::<Vec<_>>();
            if !valid.is_empty() {
                self.call_broker_logged(Request::RevokeTasks { task_ids: valid }, None)
                    .map_err(|e| e.to_string())?;
            }
            self.with_store(|s| {
                let tx=s.connection.unchecked_transaction().map_err(|e|e.to_string())?;
                for id in chunk {
                    tx.execute("UPDATE staged_inputs SET state='retired',payload=NULL WHERE task_id=?1",[id]).map_err(|e|e.to_string())?;
                    tx.execute("UPDATE staged_work SET state='waiting_for_auth',lease_id=NULL WHERE task_id=?1 AND state!='completed'",[id]).map_err(|e|e.to_string())?;
                }
                tx.commit().map_err(|e|e.to_string())
            })?;
        }
        Ok(())
    }

    fn apply_task_state(&self, id: &str, state: &protocol::TaskState) -> Result<(), String> {
        self.with_store(|s| {
            let tx=s.connection.unchecked_transaction().map_err(|e|e.to_string())?;
            for consumer in Consumer::ALL {
                let next=if state.finished_consumers&consumer.bit()!=0 { Some("completed") }
                    else if state.retired || state.abandoned_consumers&consumer.bit()!=0 { Some("waiting_for_auth") } else { None };
                if let Some(next)=next {
                    tx.execute("UPDATE staged_work SET state=?3,lease_id=NULL WHERE task_id=?1 AND consumer=?2 AND state!='completed'",
                        params![id,consumer.bit(),next]).map_err(|e|e.to_string())?;
                }
            }
            tx.execute("UPDATE staged_inputs SET expires=MIN(expires,?2) WHERE task_id=?1",params![id,state.expires_at]).map_err(|e|e.to_string())?;
            if state.retired { tx.execute("UPDATE staged_inputs SET state='retired',payload=NULL WHERE task_id=?1",[id]).map_err(|e|e.to_string())?; }
            tx.commit().map_err(|e|e.to_string())
        })?;
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .retain(|_, value| {
                value.receipt.task_id != id
                    || (!state.retired
                        && (state.finished_consumers | state.abandoned_consumers)
                            & value.receipt.consumer.bit()
                            == 0)
            });
        Ok(())
    }

    pub fn revoke_screenshots(&self, ids: &[i64]) -> Result<(), String> {
        if !self.initialized() || ids.is_empty() {
            return Ok(());
        }
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .retain(|_, value| !ids.contains(&value.receipt.screenshot_id));
        if self.broker.supported() && self.broker.installed().map_err(|e| e.to_string())? {
            let dataset = self.with_store(|s| Ok(s.dataset_id.clone()))?;
            for chunk in ids.chunks(128) {
                self.call_broker_logged(
                    Request::RevokeScreenshots {
                        dataset_id: dataset.clone(),
                        screenshot_ids: chunk.to_vec(),
                    },
                    None,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let tasks=self.with_store(|s| {
            let placeholders=ids.iter().map(|_|"?").collect::<Vec<_>>().join(",");
            let mut stmt=s.connection.prepare(&format!("SELECT task_id FROM staged_inputs WHERE state!='retired' AND screenshot_id IN ({placeholders})")).map_err(|e|e.to_string())?;
            let rows=stmt.query_map(rusqlite::params_from_iter(ids),|r|r.get::<_,String>(0)).map_err(|e|e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
        })?;
        // Service revocation above already covered tasks absent from this DB.
        self.with_store(|s| {
            let tx=s.connection.unchecked_transaction().map_err(|e|e.to_string())?;
            for id in tasks {
                tx.execute("UPDATE staged_inputs SET state='retired',payload=NULL WHERE task_id=?1",[&id]).map_err(|e|e.to_string())?;
                tx.execute("UPDATE staged_work SET state='waiting_for_auth',lease_id=NULL WHERE task_id=?1 AND state!='completed'",[&id]).map_err(|e|e.to_string())?;
            }
            tx.commit().map_err(|e|e.to_string())
        })
    }

    pub fn reconcile(&self, storage: &StorageState) -> Result<u64, String> {
        if !self.available() {
            return Ok(0);
        }
        let mut processed = 0u64;
        storage.finish_staged_deletions()?;
        self.renew_live_leases()?;
        let preparing = self.with_store(|s| {
            let mut stmt = s
                .connection
                .prepare(
                    "SELECT task_id,digest FROM staged_inputs WHERE state='preparing' LIMIT 32",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())
        })?;
        for (id, digest) in preparing {
            // Temporary transport failure must not discard an intact prepare.
            if let Err(error) = self.activate(&id, &digest) {
                if error == BrokerError::Unavailable.to_string() {
                    return Err(error);
                }
                if error != BrokerError::Busy.to_string() {
                    self.retire_task(&id)?;
                }
            }
        }
        for receipt in storage.pending_staged_receipts()? {
            if self.finish(storage, &receipt).is_ok() {
                if receipt.consumer == Consumer::Classification {
                    crate::background_activity::classification_acknowledged(1);
                }
                processed = processed.saturating_add(1);
                continue;
            }
            match self.broker.call(Request::InspectTask {
                task_id: receipt.task_id.clone(),
            }) {
                Ok(Response::TaskState(state)) => {
                    self.apply_task_state(&receipt.task_id, &state)?;
                    if state.retired
                        || (state.finished_consumers | state.abandoned_consumers)
                            & receipt.consumer.bit()
                            != 0
                    {
                        storage.clear_staged_receipt(&receipt)?;
                        if receipt.consumer == Consumer::Classification {
                            crate::background_activity::classification_acknowledged(1);
                        }
                        processed = processed.saturating_add(1);
                        continue;
                    }
                    if state.task.dataset_id != receipt.dataset_id
                        || state.task.screenshot_id != receipt.screenshot_id
                    {
                        storage.clear_staged_receipt(&receipt)?;
                        if receipt.consumer == Consumer::Classification {
                            crate::background_activity::classification_acknowledged(1);
                        }
                        processed = processed.saturating_add(1);
                        continue;
                    }
                }
                Err(BrokerError::Retired | BrokerError::DatasetMismatch) => {
                    storage.clear_staged_receipt(&receipt)?;
                    if receipt.consumer == Consumer::Classification {
                        crate::background_activity::classification_acknowledged(1);
                    }
                    processed = processed.saturating_add(1);
                    continue;
                }
                Err(BrokerError::Unavailable) => return Err(BrokerError::Unavailable.to_string()),
                _ => continue,
            }
            // A crash may leave a committed result behind an expired process
            // lease. Acquire a new lease only to resend its completion.
            let digest = self.with_store(|s| {
                s.connection
                    .query_row(
                        "SELECT digest FROM staged_inputs WHERE task_id=?1",
                        [&receipt.task_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|e| e.to_string())
            });
            if let Ok(digest) = digest {
                match self.broker.call(Request::AcquireTask {
                    task_id: receipt.task_id.clone(),
                    consumer: receipt.consumer,
                    ciphertext_digest: digest,
                }) {
                    Ok(Response::Lease(lease)) => {
                        let mut recovered = receipt.clone();
                        recovered.lease_id = lease.lease_id;
                        if self.finish(storage, &recovered).is_ok() {
                            if receipt.consumer == Consumer::Classification {
                                crate::background_activity::classification_acknowledged(1);
                            }
                            processed = processed.saturating_add(1);
                        }
                    }
                    Err(BrokerError::Retired) => {
                        // Another consumer can still have a live grant.
                        self.defer_broker_error(
                            &receipt.task_id,
                            receipt.consumer,
                            BrokerError::Retired,
                        )?;
                        storage.clear_staged_receipt(&receipt)?;
                        if receipt.consumer == Consumer::Classification {
                            crate::background_activity::classification_acknowledged(1);
                        }
                        processed = processed.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        let expired=self.with_store(|s| {
            let mut stmt=s.connection.prepare("SELECT task_id FROM staged_inputs WHERE state='revoke_pending' OR (state!='retired' AND expires<=?1) ORDER BY expires,task_id LIMIT 128").map_err(|e|e.to_string())?;
            let rows=stmt.query_map([protocol::now_secs()],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
        })?;
        self.retire_tasks(&expired)?;
        let cursor = self
            .scan_cursor
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .clone();
        let entries=self.with_store(|s| {
            let mut stmt=s.connection.prepare("SELECT task_id,screenshot_id FROM staged_inputs WHERE state!='retired' AND task_id>?1 ORDER BY task_id LIMIT 64").map_err(|e|e.to_string())?;
            let rows=stmt.query_map([cursor],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).map_err(|e|e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
        })?;
        *self
            .scan_cursor
            .lock()
            .map_err(|_| "staging lock poisoned")? = if entries.len() == 64 {
            entries.last().unwrap().0.clone()
        } else {
            String::new()
        };
        for (id, screenshot) in entries {
            if !storage.staged_screenshot_active(screenshot)? {
                self.retire_task(&id)?;
            } else {
                match self.broker.call(Request::InspectTask {
                    task_id: id.clone(),
                }) {
                    Ok(Response::TaskState(state)) => self.apply_task_state(&id, &state)?,
                    Err(BrokerError::Retired | BrokerError::DatasetMismatch) => {
                        self.retire_task(&id)?
                    }
                    Err(error) => return Err(error.to_string()),
                    _ => return Err(BrokerError::Integrity.to_string()),
                }
            }
        }
        self.make_local_room(0)?;
        self.issued
            .lock()
            .map_err(|_| "staging lock poisoned")?
            .retain(|_, value| value.deadline > protocol::now_secs());
        self.with_store(|s| {
            s.connection.execute("UPDATE staged_work SET state='pending',lease_id=NULL WHERE state='processing' AND deadline<=?1",[protocol::now_secs()]).map_err(|e|e.to_string())?;
            s.connection.execute("DELETE FROM staged_inputs WHERE state='retired' AND expires<?1",[protocol::now_secs()-60*protocol::DAY_SECS]).map_err(|e|e.to_string())?;
            s.connection.execute_batch("PRAGMA incremental_vacuum(256);").map_err(|e|e.to_string())
        })?;
        Ok(processed)
    }
}

pub(crate) fn ready_for_kind(
    storage: &StorageState,
    kind: crate::background_scheduler::BackgroundTaskKind,
) -> bool {
    use crate::background_scheduler::BackgroundTaskKind;
    match kind {
        BackgroundTaskKind::SemanticIndex => storage.processing_stage.has_ready(Consumer::MiniLm),
        BackgroundTaskKind::ClipIndex => storage.processing_stage.has_ready(Consumer::Clip),
        _ => false,
    }
}

pub(crate) async fn dispatch_classification(app: &AppHandle) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    if !storage.background_processing_enabled() {
        crate::background_activity::clear_blocked("classification");
        return Ok(());
    }
    if let Some(reason) = crate::background_scheduler::environment_gate_reason(app, false) {
        if storage.processing_stage.has_ready(Consumer::Classification) {
            crate::background_activity::blocked("classification", reason);
        }
        return Ok(());
    }
    if !storage.processing_stage.has_ready(Consumer::Classification)
        || storage.processing_stage.classification_in_flight()
    {
        crate::background_activity::clear_classification_blocked_if_idle();
        return Ok(());
    }
    crate::background_activity::clear_blocked("classification");
    let claim_storage = storage.clone();
    let Some(work) = tokio::task::spawn_blocking(move || {
        claim_storage
            .processing_stage
            .claim(&claim_storage, Consumer::Classification)
    })
    .await
    .map_err(|e| e.to_string())??
    else {
        return Ok(());
    };
    let request = serde_json::json!({
        "command":"enqueue_ocr_postprocess","screenshot_id":work.receipt.screenshot_id,
        "window_title":work.input.window_title,"process_name":work.input.process_name,
        "ocr_text":work.input.ocr_text,"timestamp":work.input.timestamp_ms,"staged_receipt":work.receipt,
    });
    crate::background_activity::classification_started();
    tracing::debug!("[BACKGROUND] event=classification_dispatched source=staged");
    let monitor = app.state::<crate::monitor::MonitorState>();
    let accepted = crate::monitor::forward_command_to_python(&monitor, request)
        .await
        .ok()
        .and_then(|value| value.get("postprocess_enqueued").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    if !accepted {
        crate::background_activity::classification_dispatch_rejected();
        let receipt = work.receipt;
        tokio::task::spawn_blocking(move || storage.processing_stage.release(&receipt, false))
            .await
            .map_err(|e| e.to_string())??;
    }
    Ok(())
}

pub(crate) fn captured_input(
    screenshot_id: i64,
    storage: &StorageState,
    image_hash: &str,
    title: &str,
    process: &str,
    timestamp_ms: i64,
    ocr_text: String,
    image: &image::RgbImage,
) -> Result<(ProcessingInput, u8), String> {
    let semantic_enabled = crate::registry_config::get_bool("clustering_enabled").unwrap_or(true)
        || crate::registry_config::get_bool("smart_cluster_enabled").unwrap_or(false);
    let classification = crate::registry_config::get_bool("classification_enabled").unwrap_or(true);
    let has_ocr = !ocr_text.trim().is_empty();
    let semantic_text = crate::minilm_migration::build_minilm_task_text(process, title, &ocr_text);
    let clip = if has_ocr {
        let resized =
            crate::clip_preprocess::pillow_bicubic_resize_rgb(image, CLIP_SIZE, CLIP_SIZE);
        Some(ClipInput {
            width: CLIP_SIZE,
            height: CLIP_SIZE,
            rgb: resized.into_raw(),
            model_revision: crate::semantic_models::descriptor(MlSemanticModel::ChineseClip)
                .revision
                .into(),
        })
    } else {
        None
    };
    let mask = if classification {
        Consumer::Classification.bit()
    } else {
        0
    } | if semantic_enabled && !semantic_text.trim().is_empty() {
        Consumer::MiniLm.bit()
    } else {
        0
    } | if has_ocr { Consumer::Clip.bit() } else { 0 };
    Ok((
        ProcessingInput {
            image_hash: image_hash.into(),
            window_title: title.into(),
            process_name: process.into(),
            timestamp_ms,
            ocr_text,
            source_revision: storage.staged_source_revision(screenshot_id)?,
            clip,
        },
        mask,
    ))
}

/// A bounded single-model slice. It participates in the existing scheduler's
/// affinity quantum but only ever receives service-authorized staged inputs.
pub(crate) async fn run_model_slice(
    app: &AppHandle,
    consumer: Consumer,
    quantum: Option<&AutomaticSliceContext>,
) -> Result<ScheduledSliceResult, String> {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let semantic = app.state::<Arc<SemanticRuntimeState>>().inner().clone();
    let mut processed = 0u64;
    for _ in 0..16 {
        if crate::background_scheduler::environment_gate_reason(app, false).is_some()
            || !storage.background_processing_enabled()
        {
            break;
        }
        if quantum.is_some_and(|q| q.stop_reason(&semantic, processed > 0).is_some()) {
            break;
        }
        let claim_storage = storage.clone();
        let Some(work) = tokio::task::spawn_blocking(move || {
            claim_storage
                .processing_stage
                .claim(&claim_storage, consumer)
        })
        .await
        .map_err(|e| e.to_string())??
        else {
            break;
        };
        let result = encode_staged(app, &storage, &semantic, &work).await;
        match result {
            Ok(()) => {
                processed = processed.saturating_add(1);
                crate::background_activity::index_progress(1);
                let finish_storage = storage.clone();
                let receipt = work.receipt.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    finish_storage
                        .processing_stage
                        .finish(&finish_storage, &receipt)
                })
                .await;
            }
            Err(error) => {
                let deferred = error.starts_with("deferred:");
                let release_storage = storage.clone();
                let receipt = work.receipt.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    release_storage
                        .processing_stage
                        .release(&receipt, !deferred)
                })
                .await;
                if !deferred {
                    return Err("Staged model processing failed".into());
                }
                break;
            }
        }
    }
    Ok(
        ScheduledSliceResult::complete(storage.processing_stage.has_ready(consumer))
            .with_processed(processed),
    )
}

async fn encode_staged(
    app: &AppHandle,
    storage: &Arc<StorageState>,
    semantic: &Arc<SemanticRuntimeState>,
    work: &StagedWork,
) -> Result<(), String> {
    let input = &work.input;
    let (spec, text) = match work.receipt.consumer {
        Consumer::MiniLm => {
            let text = crate::minilm_migration::build_minilm_task_text(
                &input.process_name,
                &input.window_title,
                &input.ocr_text,
            );
            (
                crate::minilm_migration::minilm_job_spec(work.receipt.screenshot_id, &text),
                Some(text),
            )
        }
        Consumer::Clip => (
            crate::clip_migration::clip_job_spec(&input.image_hash),
            None,
        ),
        Consumer::Classification => return Err("invalid model consumer".into()),
    };
    if !storage.staged_source_is_current(&work.receipt)? {
        return Err("deferred: source changed".into());
    }
    if let Some(existing) =
        storage.get_query_visible_embedding(spec.index_kind, &spec.subject_key)?
    {
        if existing.job == spec {
            return storage.record_staged_receipt(&work.receipt);
        }
    }
    storage.ensure_derived_index_job(&spec)?;
    let lease_token = storage
        .mark_derived_index_job_processing(&spec)
        .map_err(|_| "deferred: derived job busy")?;
    let result = async {
        let _worker = crate::semantic_runtime::BACKGROUND_PASS_GUARD
            .try_lock()
            .map_err(|_| "deferred: model busy")?;
        if crate::background_scheduler::environment_gate_reason(app, false).is_some() {
            return Err("deferred: user active".into());
        }
        let embedded = if let Some(text) = &text {
            semantic
                .embed_text(
                    app.clone(),
                    MlSemanticModel::MinilmL12,
                    vec![text.clone()],
                    Duration::from_secs(120),
                    false,
                )
                .await?
        } else {
            let clip = input.clip.as_ref().ok_or("staged CLIP input is missing")?;
            if clip.model_revision
                != crate::semantic_models::descriptor(MlSemanticModel::ChineseClip).revision
                || clip.width != CLIP_SIZE
                || clip.height != CLIP_SIZE
                || clip.rgb.len() != (CLIP_SIZE * CLIP_SIZE * 3) as usize
            {
                return Err("staged CLIP contract changed".into());
            }
            semantic
                .embed_image(
                    app.clone(),
                    MlSemanticModel::ChineseClip,
                    vec![MlImageInput {
                        width: clip.width,
                        height: clip.height,
                        stride: clip.width as usize * 3,
                        offset: 0,
                        body_len: clip.rgb.len(),
                    }],
                    clip.rgb.clone(),
                    Duration::from_secs(120),
                    false,
                )
                .await?
        };
        let vector = embedded
            .vectors
            .into_iter()
            .next()
            .ok_or("model returned no vector")?;
        match work.receipt.consumer {
            Consumer::MiniLm => crate::minilm_migration::validate_minilm_vector(&vector)?,
            Consumer::Clip => crate::clip_migration::validate_clip_vector(&vector)?,
            _ => unreachable!(),
        }
        storage.processing_stage.check_receipt(&work.receipt)?;
        storage.commit_staged_embedding(
            &crate::storage::DerivedEmbeddingWrite {
                job: spec.clone(),
                lease_token: lease_token.clone(),
                vector: vector.clone(),
            },
            &work.receipt,
        )?;
        Ok::<_, String>(vector)
    }
    .await;
    match result {
        Ok(vector) => {
            if work.receipt.consumer == Consumer::MiniLm {
                crate::minilm_index::mirror_staged_result(
                    app,
                    work.receipt.screenshot_id,
                    input,
                    text.unwrap_or_default(),
                    vector,
                )
                .await;
            }
            Ok(())
        }
        Err(error) => {
            let _ = storage.requeue_derived_index_job(
                &spec,
                &lease_token,
                "staged_retry",
                "Waiting for staged processing retry",
            );
            Err(error)
        }
    }
}
