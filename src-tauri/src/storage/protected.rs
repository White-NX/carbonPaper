//! Separate encryption domains inside the public bootstrap database. A process
//! always creates fresh write keys; persisted, caller-writable key envelopes are
//! used only for authenticated reads, never selected as future encryption keys.

use super::StorageState;
use crate::credential_manager::{
    load_public_key_from_file, unwrap_storage_domain_key, wrap_storage_domain_key,
    CredentialManagerState,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use carbonpaper_app_bound::{
    payload::{payload_key_id, CipherKey},
    protocol::{BrokerError, Request, Response, StorageKeyBinding, TaskKey},
};
use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock, Weak,
};

const TEXT_PREFIX: &str = "cpe1:";
const PROCESS_PREFIX: &str = "cpp1:";

#[cfg(test)]
mod tests;

#[derive(Clone)]
struct KeyRecord {
    binding: StorageKeyBinding,
    wrapped: Vec<u8>,
}
#[derive(Default)]
struct State {
    dataset: Option<String>,
    service_dataset: Option<String>,
    records: HashMap<String, KeyRecord>,
    keys: HashMap<String, Arc<CipherKey>>,
    writes: HashMap<String, String>,
    published: HashSet<String>,
    queued_reads: HashSet<String>,
    publication_retry_at: Option<std::time::Instant>,
    processes: HashMap<String, Vec<u8>>,
    grant_checked_at: HashMap<String, std::time::Instant>,
}

pub(super) struct ProtectedStorage {
    credentials: Arc<CredentialManagerState>,
    staging: RwLock<Arc<crate::processing_stage::ProcessingStaging>>,
    state: Mutex<State>,
    weak_self: Weak<Self>,
    publication_running: AtomicBool,
    publication_lock: Mutex<()>,
    #[cfg(test)]
    allow_plaintext: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    automatic_publication_for_tests: AtomicBool,
}

impl ProtectedStorage {
    pub(super) fn new(
        credentials: Arc<CredentialManagerState>,
        staging: Arc<crate::processing_stage::ProcessingStaging>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            credentials,
            staging: RwLock::new(staging),
            state: Mutex::new(State::default()),
            weak_self: weak.clone(),
            publication_running: AtomicBool::new(false),
            publication_lock: Mutex::new(()),
            #[cfg(test)]
            allow_plaintext: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            automatic_publication_for_tests: AtomicBool::new(false),
        })
    }
    pub(super) fn reset(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = State::default();
    }

    #[cfg(test)]
    pub(super) fn allow_plaintext_fixtures(&self, allow: bool) {
        self.allow_plaintext
            .store(allow, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn clear_read_keys(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let writes: HashSet<_> = state.writes.values().cloned().collect();
        state.keys.retain(|id, _| writes.contains(id));
        state.published.clear();
        state.queued_reads.clear();
        state.publication_retry_at = None;
        state.grant_checked_at.clear();
    }

    fn broker_request(&self, request: Request) -> Result<Response, BrokerError> {
        self.staging
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .storage_key_request(request)
    }

    pub(super) fn initialize(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS protected_storage_keys (
            key_id TEXT PRIMARY KEY,purpose TEXT NOT NULL,dataset_id TEXT NOT NULL,
            public_key_fingerprint TEXT NOT NULL,wrapped_key BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS protected_process_names(token TEXT PRIMARY KEY,name_enc BLOB NOT NULL);")
            .map_err(|e|e.to_string())?;
        let service_dataset: String = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='app_bound_dataset_id'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        conn.execute("INSERT OR IGNORE INTO app_metadata(key,value) VALUES('protected_content_dataset_id',?1)", [&service_dataset]).map_err(|e|e.to_string())?;
        let dataset: String = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let public = load_public_key_from_file(&self.credentials).map_err(|e| e.to_string())?;
        if !carbonpaper_app_bound::protocol::valid_id(&dataset)
            || !carbonpaper_app_bound::protocol::valid_id(&service_dataset)
        {
            return Err("Invalid protected dataset identity".into());
        }
        let fingerprint = hex::encode(Sha256::digest(&public));
        let previous: Option<String> = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='protected_public_key_fingerprint'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if previous
            .as_deref()
            .is_some_and(|value| value != fingerprint)
        {
            return Err("Dataset public key mismatch; recovery is required".into());
        }
        conn.execute(
            "INSERT OR IGNORE INTO app_metadata(key,value) VALUES('protected_public_key_fingerprint',?1)",
            [&fingerprint],
        )
        .map_err(|e| e.to_string())?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.dataset.as_deref() == Some(&dataset)
            && state.service_dataset.as_deref() == Some(&service_dataset)
        {
            return Ok(());
        }
        *state = State {
            dataset: Some(dataset.clone()),
            service_dataset: Some(service_dataset.clone()),
            ..State::default()
        };
        let mut query=conn.prepare("SELECT key_id,purpose,dataset_id,public_key_fingerprint,wrapped_key FROM protected_storage_keys LIMIT 100001").map_err(|e|e.to_string())?;
        let records = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in records {
            let (id, purpose, stored_dataset, fp, wrapped) = row.map_err(|e| e.to_string())?;
            if state.records.len() >= 100000
                || id.len() != 32
                || !id
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                || wrapped.len() > 4096
                || stored_dataset != dataset
                || fp != fingerprint
                || !matches!(purpose.as_str(), "metadata" | "vectors" | "ann")
            {
                return Err("Invalid protected key registry".into());
            }
            let binding = StorageKeyBinding {
                key_id: id.clone(),
                dataset_id: service_dataset.clone(),
                content_dataset_id: dataset.clone(),
                public_key_fingerprint: fp,
                purpose,
                wrapped_key_digest: hex::encode(Sha256::digest(&wrapped)),
            };
            state.records.insert(id, KeyRecord { binding, wrapped });
        }
        drop(query);
        self.create_write_keys(conn, &mut state, &dataset, &service_dataset, &fingerprint)?;
        let mut query = conn
            .prepare("SELECT token,name_enc FROM protected_process_names")
            .map_err(|e| e.to_string())?;
        for row in query
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| e.to_string())?
        {
            let (token, encrypted) = row.map_err(|e| e.to_string())?;
            if state.processes.len() >= 100_000 || encrypted.len() > 8192 || token.len() != 102 {
                return Err("Invalid protected process catalog".into());
            }
            state.processes.insert(token, encrypted);
        }
        Ok(())
    }

    fn create_write_keys(
        &self,
        conn: &Connection,
        state: &mut State,
        dataset: &str,
        service_dataset: &str,
        fingerprint: &str,
    ) -> Result<(), String> {
        for purpose in ["metadata", "vectors", "ann"] {
            let id = hex::encode(rand::random::<[u8; 16]>());
            let bytes = zeroize::Zeroizing::new(rand::random::<[u8; 32]>());
            let wrapped =
                wrap_storage_domain_key(&self.credentials, &*bytes).map_err(|e| e.to_string())?;
            let key = Arc::new(CipherKey::new(
                id.clone(),
                dataset.into(),
                purpose.into(),
                &*bytes,
            )?);
            conn.execute(
                "INSERT INTO protected_storage_keys VALUES(?1,?2,?3,?4,?5)",
                params![id, purpose, dataset, fingerprint, wrapped],
            )
            .map_err(|e| e.to_string())?;
            let binding = StorageKeyBinding {
                key_id: id.clone(),
                dataset_id: service_dataset.into(),
                content_dataset_id: dataset.into(),
                public_key_fingerprint: fingerprint.into(),
                purpose: purpose.into(),
                wrapped_key_digest: hex::encode(Sha256::digest(&wrapped)),
            };
            state
                .records
                .insert(id.clone(), KeyRecord { binding, wrapped });
            state.writes.insert(purpose.into(), id.clone());
            state.keys.insert(id, key);
        }
        Ok(())
    }

    pub(super) fn rotate_write_keys(&self, conn: &Connection) -> Result<(), String> {
        let public = load_public_key_from_file(&self.credentials).map_err(|e| e.to_string())?;
        let fingerprint = hex::encode(Sha256::digest(public));
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let dataset = state
            .dataset
            .clone()
            .ok_or("Protected storage is not initialized")?;
        let service_dataset = state
            .service_dataset
            .clone()
            .ok_or("Protected storage is not initialized")?;
        self.create_write_keys(conn, &mut state, &dataset, &service_dataset, &fingerprint)
    }

    pub(super) fn write_key(&self, purpose: &str) -> Result<Arc<CipherKey>, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = state
            .writes
            .get(purpose)
            .ok_or("Protected storage is not initialized")?;
        state
            .keys
            .get(id)
            .cloned()
            .ok_or("Protected write key is unavailable".into())
    }

    fn read_key(&self, id: &str, purpose: &str) -> Result<Arc<CipherKey>, String> {
        let (record, cached, fresh, published, checked) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let record = state
                .records
                .get(id)
                .cloned()
                .ok_or("Unknown protected content key")?;
            if record.binding.purpose != purpose {
                return Err("Protected content domain mismatch".into());
            }
            (
                record,
                state.keys.get(id).cloned(),
                state.writes.values().any(|value| value == id),
                state.published.contains(id),
                state.grant_checked_at.get(id).copied(),
            )
        };
        let authorized = self.credentials.protected_read_authorized();
        #[cfg(test)]
        let authorized = authorized || self.credentials.storage_fixture_for_tests();
        if authorized {
            let key = if let Some(key) = cached {
                key
            } else {
                let bytes = zeroize::Zeroizing::new(
                    unwrap_storage_domain_key(&self.credentials, &record.wrapped)
                        .map_err(|e| e.to_string())?,
                );
                let key = Arc::new(CipherKey::new(
                    id.into(),
                    record.binding.content_dataset_id.clone(),
                    purpose.into(),
                    &bytes,
                )?);
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .keys
                    .insert(id.into(), key.clone());
                key
            };
            if !published && self.credentials.background_processing_enabled() {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .queued_reads
                    .insert(id.into());
                self.schedule_publication();
            }
            return Ok(key);
        }
        if !self.credentials.background_processing_enabled() {
            return Err("AUTH_REQUIRED".into());
        }
        // Cache only a recently verified grant for this exact key. A generic
        // "service enabled" status cannot prove a previously revoked key live.
        if published
            && checked.is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(1))
            && self
                .staging
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .storage_keys_available()
        {
            if let Some(key) = cached {
                return Ok(key);
            }
        }
        if fresh {
            let key = cached.ok_or("Protected write key is unavailable")?;
            self.publish(&record, &key, true)?;
            return Ok(key);
        }
        let response = self
            .broker_request(Request::AcquireStorageKey {
                binding: record.binding.clone(),
            })
            .map_err(|error| {
                if matches!(error, BrokerError::Integrity | BrokerError::DatasetMismatch) {
                    if self.record_is_current(&record) {
                        self.staging
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .quarantine_broker(error);
                    }
                    error.to_string()
                } else {
                    "AUTH_REQUIRED".into()
                }
            })?;
        let Response::StorageKey(grant) = response else {
            self.staging
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .quarantine_broker(BrokerError::Integrity);
            return Err("Invalid protected key grant".into());
        };
        if grant.binding != record.binding {
            self.staging
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .quarantine_broker(BrokerError::Integrity);
            return Err("Protected key grant identity mismatch".into());
        }
        let key = Arc::new(CipherKey::new(
            id.into(),
            grant.binding.content_dataset_id,
            purpose.into(),
            &grant.key.0,
        )?);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.keys.insert(id.into(), key.clone());
        state.published.insert(id.into());
        state
            .grant_checked_at
            .insert(id.into(), std::time::Instant::now());
        Ok(key)
    }

    fn publish(
        &self,
        record: &KeyRecord,
        key: &CipherKey,
        write_origin: bool,
    ) -> Result<(), String> {
        let response = self
            .broker_request(Request::RegisterStorageKey {
                binding: record.binding.clone(),
                key: TaskKey(key.bytes().to_vec()),
                write_origin,
            })
            .map_err(|error| {
                if matches!(error, BrokerError::Integrity | BrokerError::DatasetMismatch) {
                    if self.record_is_current(record) {
                        self.staging
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .quarantine_broker(error);
                    }
                    error.to_string()
                } else {
                    "AUTH_REQUIRED".into()
                }
            })?;
        if !matches!(response, Response::Ok) {
            if self.record_is_current(record) {
                self.staging
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .quarantine_broker(BrokerError::Integrity);
            }
            return Err("Invalid protected key acknowledgement".into());
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .records
            .get(&key.id)
            .is_none_or(|current| current.binding != record.binding)
        {
            return Ok(());
        }
        state.published.insert(key.id.clone());
        state.queued_reads.remove(&key.id);
        state
            .grant_checked_at
            .insert(key.id.clone(), std::time::Instant::now());
        Ok(())
    }

    fn record_is_current(&self, record: &KeyRecord) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .get(&record.binding.key_id)
            .is_some_and(|current| current.binding == record.binding)
    }

    pub(super) fn schedule_publication(&self) {
        // Ordinary fixtures own short-lived temporary archives. Tests that
        // exercise this lane explicitly enable it and wait for its completion.
        #[cfg(test)]
        if !self.automatic_publication_for_tests.load(Ordering::Acquire) {
            return;
        }
        if !self.credentials.background_processing_enabled() {
            return;
        }
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state
                .publication_retry_at
                .is_some_and(|retry| retry > std::time::Instant::now())
                || (state.queued_reads.is_empty()
                    && state.writes.values().all(|id| state.published.contains(id)))
            {
                return;
            }
        }
        if self.publication_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = self.weak_self.clone();
        let spawned = std::thread::Builder::new()
            .name("protected-key-publication".into())
            .spawn(move || {
                let Some(storage) = weak.upgrade() else {
                    return;
                };
                struct Running(Arc<ProtectedStorage>);
                impl Drop for Running {
                    fn drop(&mut self) {
                        self.0.publication_running.store(false, Ordering::Release);
                    }
                }
                let _running = Running(storage.clone());
                if let Err(error) = storage.publish_pending_keys() {
                    storage
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .publication_retry_at =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
                    tracing::debug!("[APP_BOUND] Background key publication deferred: {error}");
                }
            });
        if spawned.is_err() {
            self.publication_running.store(false, Ordering::Release);
            tracing::warn!("[APP_BOUND] Could not schedule background key publication");
        }
    }

    pub(super) fn publish_pending_keys(&self) -> Result<(), String> {
        if !self.credentials.background_processing_enabled() {
            return Ok(());
        }
        let _publication = self
            .publication_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let staging = self
            .staging
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !staging.available() {
            staging.refresh().map_err(|_| "AUTH_REQUIRED")?;
        }
        if !staging.storage_keys_available() {
            return Err("AUTH_REQUIRED".into());
        }
        let pending = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut ids: Vec<_> = state
                .writes
                .values()
                .filter(|id| !state.published.contains(*id))
                .cloned()
                .collect();
            for id in &state.queued_reads {
                if ids.len() >= 64 {
                    break;
                }
                if !state.published.contains(id) && !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
            let mut pending = Vec::new();
            for id in ids.into_iter().take(64) {
                if let (Some(record), Some(key)) = (state.records.get(&id), state.keys.get(&id)) {
                    pending.push((
                        record.clone(),
                        key.clone(),
                        state.writes.values().any(|current| *current == id),
                    ));
                }
            }
            pending
        };
        for (record, key, fresh) in pending {
            if !self.credentials.background_processing_enabled() {
                return Err("AUTH_REQUIRED".into());
            }
            if !self.record_is_current(&record) {
                continue;
            }
            // Stop at the first outage. Every key does not need a fresh status
            // probe or another full pipe timeout in the same publication pass.
            self.publish(&record, &key, fresh)?;
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .publication_retry_at = None;
        Ok(())
    }

    pub(super) fn seal(
        &self,
        purpose: &str,
        identity: &str,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.write_key(purpose)?.seal(identity, bytes)
    }

    fn resume_ann_key(&self, id: &str) -> Result<Arc<CipherKey>, String> {
        let record = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.writes.get("ann").is_some_and(|current| current == id) {
                return state
                    .keys
                    .get(id)
                    .cloned()
                    .ok_or("Protected ANN write key is unavailable".into());
            }
            state
                .records
                .get(id)
                .cloned()
                .ok_or("Unknown ANN checkpoint key")?
        };
        if record.binding.purpose != "ann" {
            return Err("ANN checkpoint key domain mismatch".into());
        }
        // A CNG unwrap proves only the recipient. It does not prove that this
        // caller-writable envelope was generated by an earlier trusted writer.
        let request = || {
            self.broker_request(Request::AcquireStorageKey {
                binding: record.binding.clone(),
            })
        };
        let response = match request() {
            Ok(response) => Ok(response),
            Err(_) if self.credentials.protected_read_authorized() => {
                let key = self.read_key(id, "ann")?;
                // Resuming encryption is an explicit background operation and
                // still requires durable write provenance, even after Hello.
                self.publish(&record, &key, false)?;
                request()
            }
            Err(error) => Err(error),
        };
        let Response::StorageKey(grant) =
            response.map_err(|_| "AUTH_REQUIRED: ANN checkpoint needs a registered write key")?
        else {
            return Err("Invalid ANN checkpoint key grant".into());
        };
        if grant.binding != record.binding || !grant.write_origin {
            return Err("ANN checkpoint key identity mismatch".into());
        }
        CipherKey::new(
            id.into(),
            grant.binding.content_dataset_id,
            "ann".into(),
            &grant.key.0,
        )
        .map(Arc::new)
    }
    pub(super) fn text(&self, identity: &str, text: &str) -> Result<String, String> {
        Ok(format!(
            "{TEXT_PREFIX}{}",
            STANDARD.encode(self.seal("metadata", identity, text.as_bytes())?)
        ))
    }
    fn open(&self, purpose: &str, identity: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if self
            .allow_plaintext
            .load(std::sync::atomic::Ordering::Relaxed)
            && !carbonpaper_app_bound::payload::is_sealed(bytes)
        {
            return Ok(bytes.to_vec());
        }
        let id = payload_key_id(bytes)?;
        Ok(self.read_key(&id, purpose)?.open(identity, bytes)?.to_vec())
    }
    fn open_text(&self, identity: &str, text: &str) -> Result<String, String> {
        #[cfg(test)]
        if self
            .allow_plaintext
            .load(std::sync::atomic::Ordering::Relaxed)
            && !text.starts_with(TEXT_PREFIX)
        {
            return Ok(text.into());
        }
        let encoded = text
            .strip_prefix(TEXT_PREFIX)
            .ok_or("Encrypted text format is required")?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "Invalid protected text")?;
        String::from_utf8(self.open("metadata", identity, &bytes)?)
            .map_err(|_| "Invalid protected text encoding".into())
    }

    pub(super) fn process_token(&self, conn: &Connection, name: &str) -> Result<String, String> {
        use hmac::{Hmac, Mac};
        let key = self.write_key("metadata")?;
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.bytes())
            .map_err(|_| "Invalid process key")?;
        mac.update(b"CarbonPaper-Process-v1:");
        mac.update(name.as_bytes());
        let token = format!(
            "{PROCESS_PREFIX}{}:{}",
            key.id,
            hex::encode(mac.finalize().into_bytes())
        );
        let encrypted = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .processes
            .get(&token)
            .cloned();
        let encrypted = match encrypted {
            Some(bytes) => bytes,
            None => key.seal(&format!("process:{token}"), name.as_bytes())?,
        };
        // Reinsert on cache hits as well: an earlier caller's transaction may
        // have rolled back after creating the encrypted catalog entry.
        conn.execute(
            "INSERT OR IGNORE INTO protected_process_names VALUES(?1,?2)",
            params![token, encrypted],
        )
        .map_err(|e| e.to_string())?;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .processes
            .insert(token.clone(), encrypted);
        Ok(token)
    }
    fn process_name(&self, token: &str) -> Result<String, String> {
        #[cfg(test)]
        if self
            .allow_plaintext
            .load(std::sync::atomic::Ordering::Relaxed)
            && !token.starts_with(PROCESS_PREFIX)
        {
            return Ok(token.into());
        }
        let encrypted = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .processes
            .get(token)
            .cloned()
            .ok_or("Unknown protected process")?;
        String::from_utf8(self.open("metadata", &format!("process:{token}"), &encrypted)?)
            .map_err(|_| "Invalid process name encoding".into())
    }
    pub(super) fn process_tokens(&self, names: &[String]) -> Result<Vec<String>, String> {
        let tokens: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .processes
            .keys()
            .cloned()
            .collect();
        let mut result = Vec::new();
        for token in tokens {
            if names.contains(&self.process_name(&token)?) {
                result.push(token);
            }
        }
        #[cfg(test)]
        if self
            .allow_plaintext
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            result.extend(names.iter().cloned());
        }
        if result.is_empty() {
            result.push("cpp1:absent".into());
        }
        Ok(result)
    }

    pub(super) fn register_functions(self: &Arc<Self>, conn: &Connection) -> Result<(), String> {
        let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY;
        let context = self.clone();
        conn.create_scalar_function("cp_seal_text", 2, flags, move |args| {
            let text: Option<String> = args.get(0)?;
            text.map(|text| {
                context
                    .text(&args.get::<String>(1)?, &text)
                    .map_err(sql_error)
            })
            .transpose()
        })
        .map_err(|e| e.to_string())?;
        let context = self.clone();
        conn.create_scalar_function("cp_unseal", 3, flags, move |args| {
            let bytes: Option<Vec<u8>> = args.get(0)?;
            bytes
                .map(|bytes| {
                    context
                        .open(&args.get::<String>(1)?, &args.get::<String>(2)?, &bytes)
                        .map_err(sql_error)
                })
                .transpose()
        })
        .map_err(|e| e.to_string())?;
        let context = self.clone();
        conn.create_scalar_function("cp_text", 2, flags, move |args| {
            let text: Option<String> = args.get(0)?;
            text.map(|text| {
                context
                    .open_text(&args.get::<String>(1)?, &text)
                    .map_err(sql_error)
            })
            .transpose()
        })
        .map_err(|e| e.to_string())?;
        let context = self.clone();
        conn.create_scalar_function("cp_process", 1, flags, move |args| {
            let token: Option<String> = args.get(0)?;
            token
                .map(|token| context.process_name(&token).map_err(sql_error))
                .transpose()
        })
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn sql_error(message: String) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        message,
    )))
}

impl StorageState {
    pub(crate) fn protected_query_session(
        &self,
    ) -> Result<Option<(std::sync::Weak<CredentialManagerState>, std::time::Instant)>, String> {
        #[cfg(test)]
        if self.credential_state.storage_fixture_for_tests() {
            return Ok(None);
        }
        if !self.credential_state.is_session_valid()
            || self.credential_state.silent_read_auth_required()
        {
            return Err("AUTH_REQUIRED".into());
        }
        let marker = self
            .credential_state
            .session_marker()
            .ok_or("AUTH_REQUIRED")?;
        Ok(Some((Arc::downgrade(&self.credential_state), marker)))
    }

    pub(crate) fn protected_ann_write_key(&self) -> Result<Arc<CipherKey>, String> {
        self.protected.write_key("ann")
    }
    pub(crate) fn protected_ann_read_key(&self, id: &str) -> Result<Arc<CipherKey>, String> {
        self.protected.read_key(id, "ann")
    }
    pub(crate) fn protected_ann_resume_key(&self, id: &str) -> Result<Arc<CipherKey>, String> {
        self.protected.resume_ann_key(id)
    }
    #[cfg(test)]
    pub(crate) fn publish_protected_keys(&self) -> Result<(), String> {
        match self.protected.publish_pending_keys() {
            Err(error) if error == "AUTH_REQUIRED" => Ok(()),
            result => result,
        }
    }

    pub(crate) fn request_protected_key_publication(&self) {
        self.protected.schedule_publication();
    }

    pub(crate) fn rotate_protected_write_keys(&self) -> Result<(), String> {
        let guard = self.get_connection_named("rotate_protected_write_keys")?;
        self.protected
            .rotate_write_keys(guard.as_ref().ok_or("Database not initialized")?)
    }

    pub(crate) fn clear_protected_read_cache(&self) {
        self.protected.clear_read_keys();
        self.reset_semantic_vector_cache();
    }

    #[cfg(test)]
    pub(super) fn replace_processing_stage_for_tests(
        &mut self,
        staging: crate::processing_stage::ProcessingStaging,
    ) {
        self.processing_stage = Arc::new(staging);
        *self.protected.staging.write().unwrap() = self.processing_stage.clone();
    }
}
