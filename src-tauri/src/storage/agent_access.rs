//! Desktop writer for scoped OCR/vector keys and small text projections.
//! CNG and image keys stay here. Source triggers fence reads before committing
//! independently encrypted authorization and projection events.
use super::StorageState;
use crate::credential_manager::{
    decrypt_with_master_key, encrypt_with_master_key, CredentialError, RowKeyDecryptTimings,
};
use carbonpaper_agent_access::{
    repository::{open_event, seal_access_key, seal_event, AgentRepository},
    types::*,
};
use carbonpaper_app_bound::protocol::{AgentViewBinding, AgentViewKeys, Request, Response};
use rusqlite::{functions::FunctionFlags, params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

fn broker(request: Request) -> Result<Response, String> {
    carbonpaper_app_bound::windows::transport::call(request)
        .map_err(|e| format!("AGENT_SERVICE_UNAVAILABLE: {e}"))
}
fn sql_error(message: String) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        message,
    )))
}
fn db<T>(value: rusqlite::Result<T>) -> Result<T, String> {
    value.map_err(|e| e.to_string())
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentPreparationProgress {
    pub phase: String,
    pub processed: u64,
    pub total: u64,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Checkpoint {
    generation: String,
    phase: usize,
    cursor: String,
    baseline: i64,
    complete: bool,
}

struct PreparationTimings {
    kind: &'static str,
    started: Instant,
    last_report: Instant,
    complete: bool,
    processed: u64,
    read: Duration,
    hydrate: Duration,
    write: Duration,
    checkpoint: Duration,
    outbox: Duration,
    row_keys: RowKeyDecryptTimings,
}

impl PreparationTimings {
    fn new(kind: &'static str) -> Self {
        Self {
            kind,
            started: Instant::now(),
            last_report: Instant::now(),
            complete: false,
            processed: 0,
            read: Duration::ZERO,
            hydrate: Duration::ZERO,
            write: Duration::ZERO,
            checkpoint: Duration::ZERO,
            outbox: Duration::ZERO,
            row_keys: RowKeyDecryptTimings::default(),
        }
    }

    fn report(&mut self, outcome: &str) {
        if self.kind.is_empty() {
            return;
        }
        tracing::info!(
            "[AGENT_ACCESS] preparation phase={} outcome={} processed={} elapsed_ms={} read_ms={} hydrate_ms={} write_ms={} checkpoint_ms={} outbox_ms={} row_key_calls={} row_key_decrypt_ms={} row_key_lock_wait_ms={}",
            self.kind, outcome, self.processed, self.started.elapsed().as_millis(),
            self.read.as_millis(), self.hydrate.as_millis(), self.write.as_millis(),
            self.checkpoint.as_millis(), self.outbox.as_millis(), self.row_keys.calls,
            self.row_keys.decrypt.as_millis(), self.row_keys.lock_wait.as_millis(),
        );
        self.last_report = Instant::now();
    }

    fn page_finished(&mut self, complete: bool) {
        if complete {
            self.complete = true;
            self.report("complete");
        } else if self.last_report.elapsed() >= Duration::from_secs(30) {
            self.report("running");
        }
    }
}

impl Drop for PreparationTimings {
    fn drop(&mut self) {
        if !self.complete {
            self.report("interrupted");
        }
    }
}

struct WriterKeys {
    content: Zeroizing<Vec<u8>>,
    outbox: Zeroizing<Vec<u8>>,
}
#[derive(Default)]
pub(crate) struct AgentWriter {
    keys: Mutex<Option<WriterKeys>>,
    configured_binding: Mutex<Option<AgentViewBinding>>,
    pending: Mutex<HashMap<[u8; 32], (Instant, Vec<u8>)>>,
    pending_keys: Mutex<HashMap<[u8; 32], (Instant, Vec<u8>)>>,
    vector_keys: Mutex<HashMap<String, VectorAccess>>,
    progress: Mutex<AgentPreparationProgress>,
    pub(super) sync_lock: Mutex<()>,
    started: AtomicBool,
    stopped: AtomicBool,
    cancelled: AtomicBool,
    configured: AtomicBool,
    #[cfg(test)]
    authority: Mutex<Option<Arc<dyn Fn(Request) -> Result<Response, String> + Send + Sync>>>,
}
thread_local! {static CAPTURE:RefCell<Option<SnapshotText>>=const {RefCell::new(None)};}
pub(super) struct CaptureTextGuard(Option<SnapshotText>);
impl Drop for CaptureTextGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        CAPTURE.with(|v| {
            *v.borrow_mut() = previous;
        });
    }
}

impl AgentWriter {
    fn request(&self, request: Request) -> Result<Response, String> {
        #[cfg(test)]
        if let Some(authority) = self
            .authority
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return authority(request);
        }
        broker(request)
    }
    pub(super) fn enabled(&self) -> bool {
        self.configured.load(Ordering::Acquire)
    }
    pub(super) fn binding(&self) -> Option<AgentViewBinding> {
        self.configured_binding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn progress(&self) -> AgentPreparationProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn clear_staging(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.pending_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.vector_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    fn progress_phase(&self, phase: &str, error: Option<String>) {
        let mut state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = phase.into();
        state.error = error;
    }
    /// Called only after both database activity gates have drained. The
    /// exporter cannot publish readiness again until maintenance releases them.
    pub(super) fn suspend_for_maintenance(&self) {
        if !self.enabled() {
            return;
        }
        let Some(binding) = self.binding() else {
            return;
        };
        self.progress_phase("syncing", None);
        let result = self.request(Request::AgentFence {
            generation: binding.generation,
        });
        let error = match result {
            Ok(Response::AgentRevision(_)) => return,
            Ok(_) => "AGENT_INVALID_REVISION".to_string(),
            Err(error) => error,
        };
        self.progress_phase("error", Some(error.clone()));
        tracing::warn!("Agent access maintenance reconciliation required: {error}");
    }
    fn install_keys(&self, keys: AgentViewKeys) -> Result<(), String> {
        self.cancelled.store(false, Ordering::Release);
        let outbox = keys.outbox_key.ok_or("AGENT_WRITE_KEY_UNAVAILABLE")?;
        if keys.content_key.0.len() != 32 || outbox.0.len() != 32 {
            return Err("AGENT_INVALID_KEY".into());
        }
        *self
            .configured_binding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(keys.binding.clone());
        *self.keys.lock().unwrap_or_else(|e| e.into_inner()) = Some(WriterKeys {
            content: Zeroizing::new(keys.content_key.0.clone()),
            outbox: Zeroizing::new(outbox.0.clone()),
        });
        self.clear_staging();
        Ok(())
    }
    pub(super) fn stage_capture(&self, request: &super::SaveScreenshotRequest) -> CaptureTextGuard {
        let value = self.enabled().then(|| SnapshotText {
            window_title: request.window_title.clone(),
            process_name: request.process_name.clone(),
            source: request.source.clone(),
            page_url: request.page_url.clone(),
            width: Some(request.width),
            height: Some(request.height),
            visible_links: request
                .visible_links
                .as_ref()
                .map(|v| {
                    v.iter()
                        .map(|l| TextLink {
                            text: l.text.clone(),
                            url: l.url.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        });
        CaptureTextGuard(CAPTURE.with(|v| v.replace(value)))
    }
    /// Capture/processing has already obtained this plaintext. Retain only an
    /// Agent-key-encrypted, bounded, short-lived copy for the SQL trigger.
    pub(super) fn stage_payload(&self, ciphertext: &[u8], plaintext: &[u8]) {
        if plaintext.len() > 8 * 1024 * 1024 {
            return;
        }
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let Some(keys) = keys.as_ref() else {
            return;
        };
        let Ok(sealed) = encrypt_with_master_key(&keys.outbox, plaintext) else {
            return;
        };
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, (time, _)| time.elapsed().as_secs() < 30);
        if pending.values().map(|(_, b)| b.len()).sum::<usize>() + sealed.len() > 16 * 1024 * 1024 {
            pending.clear();
        }
        pending.insert(Sha256::digest(ciphertext).into(), (Instant::now(), sealed));
    }
    fn staged(&self, ciphertext: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        self.staged_plain(&Sha256::digest(ciphertext).into())
    }
    fn staged_plain(&self, digest: &[u8; 32]) -> Option<Zeroizing<Vec<u8>>> {
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let keys = keys.as_ref()?;
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (time, bytes) = pending.get(digest)?;
        if time.elapsed().as_secs() >= 30 {
            return None;
        }
        decrypt_with_master_key(&keys.outbox, bytes)
            .ok()
            .map(Zeroizing::new)
    }
    fn stage_text(&self, value: rusqlite::types::ValueRef<'_>) -> Option<String> {
        match value {
            rusqlite::types::ValueRef::Null => None,
            rusqlite::types::ValueRef::Blob(v) => self
                .staged(v)
                .and_then(|v| String::from_utf8(v.to_vec()).ok()),
            rusqlite::types::ValueRef::Text(v) => {
                let text = std::str::from_utf8(v).ok()?;
                if let Some(encoded) = text.strip_prefix("cpe1:") {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .ok()?;
                    self.staged(&bytes)
                        .and_then(|v| String::from_utf8(v.to_vec()).ok())
                } else {
                    Some(text.into())
                }
            }
            _ => None,
        }
    }
    pub(super) fn stage_row_key(&self, ciphertext: &[u8], row_key: &[u8]) {
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let Some(keys) = keys.as_ref() else {
            return;
        };
        let Ok(sealed) = encrypt_with_master_key(&keys.outbox, row_key) else {
            return;
        };
        let mut pending = self.pending_keys.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, (at, _)| at.elapsed().as_secs() < 30);
        if pending.len() >= 65_536 {
            pending.clear();
        }
        pending.insert(Sha256::digest(ciphertext).into(), (Instant::now(), sealed));
    }
    fn staged_row_key(&self, digest: &[u8; 32]) -> Option<Zeroizing<Vec<u8>>> {
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let keys = keys.as_ref()?;
        let pending = self.pending_keys.lock().unwrap_or_else(|e| e.into_inner());
        let (at, sealed) = pending.get(digest)?;
        if at.elapsed().as_secs() >= 30 {
            return None;
        }
        decrypt_with_master_key(&keys.outbox, sealed)
            .ok()
            .map(Zeroizing::new)
    }
    fn wrap_access_key(
        &self,
        domain: &str,
        identity: &str,
        digest: &[u8],
        key: &[u8],
    ) -> Result<Vec<u8>, String> {
        let binding = self.binding().ok_or("AGENT_ACCESS_DISABLED")?;
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let keys = keys.as_ref().ok_or("AGENT_WRITE_KEY_UNAVAILABLE")?;
        seal_access_key(&keys.content, &binding, domain, identity, digest, key)
    }
    pub(super) fn stage_vector_key(&self, key: &carbonpaper_app_bound::payload::CipherKey) {
        let Some(binding) = self.binding() else {
            return;
        };
        if key.purpose != "vectors" || key.dataset != binding.dataset_id {
            return;
        }
        if self
            .vector_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key.id)
        {
            return;
        }
        let Ok(sealed) = self.wrap_access_key("vectors", &key.id, &[], key.bytes()) else {
            return;
        };
        self.vector_keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.id.clone(),
                VectorAccess {
                    key_id: key.id.clone(),
                    key_envelope: sealed,
                },
            );
    }
    fn prepare_live_projection(&self, kind: &str, value: &mut Value) -> Result<(), String> {
        match kind {
            "snapshot" | "snapshot_state" | "assignment" | "calendar" | "day_delete" | "source" => {
            }
            "cluster" | "summary" => {
                let fields: &[&str] = if kind == "cluster" {
                    &["anchor_text", "display_name"]
                } else {
                    &[
                        "title",
                        "summary",
                        "ocr_summary",
                        "key_points_json",
                        "evidence_json",
                    ]
                };
                for field in fields {
                    if let Some(text) = value[*field].as_str().map(str::to_string) {
                        let decoded =
                            self.stage_text(rusqlite::types::ValueRef::Text(text.as_bytes()));
                        if decoded.is_none() {
                            value[if kind == "cluster" && *field == "display_name" {
                                "missing_name"
                            } else {
                                "unavailable"
                            }] = json!(true);
                        }
                        value[*field] = json!(decoded);
                    }
                }
            }
            "vector_key" => {
                let id = value["key_id"]
                    .as_str()
                    .ok_or("AGENT_INVALID_VECTOR_ACCESS")?
                    .to_ascii_lowercase();
                let keys = self.vector_keys.lock().unwrap_or_else(|e| e.into_inner());
                value["key_envelope"] = json!(keys.get(&id).map(|v| hex::encode(&v.key_envelope)));
                value["key_id"] = json!(id);
            }
            "ocr" | "recap" => {
                if let Some(encoded) = value["payload_digest"].as_str() {
                    let digest: [u8; 32] = hex::decode(encoded)
                        .map_err(|_| "AGENT_INVALID_RECORD")?
                        .try_into()
                        .map_err(|_| "AGENT_INVALID_RECORD")?;
                    if kind == "ocr" || value["part"] == "search" {
                        let key = self.staged_row_key(&digest);
                        let (domain, id) = if kind == "ocr" {
                            (
                                "ocr",
                                value["ocr_id"]
                                    .as_i64()
                                    .ok_or("AGENT_INVALID_RECORD")?
                                    .to_string(),
                            )
                        } else {
                            (
                                "recap_search",
                                value["key"]
                                    .as_str()
                                    .ok_or("AGENT_INVALID_RECORD")?
                                    .to_string(),
                            )
                        };
                        let sealed = key
                            .map(|key| self.wrap_access_key(domain, &id, &digest, &key))
                            .transpose()?;
                        value["payload_digest"] = json!(digest.to_vec());
                        value["key_envelope"] = json!(sealed.map(hex::encode));
                    } else {
                        value["content"] = json!(self
                            .staged_plain(&digest)
                            .and_then(|p| String::from_utf8(p.to_vec()).ok()));
                    }
                }
            }
            _ => return Err("AGENT_INVALID_RECORD_KIND".into()),
        }
        Ok(())
    }
    fn queue_event(&self, change: TextChange) -> Result<String, String> {
        let binding = self.binding().ok_or("AGENT_ACCESS_DISABLED")?;
        let Response::AgentRevision(revision) = self.request(Request::AgentFence {
            generation: binding.generation.clone(),
        })?
        else {
            return Err("AGENT_INVALID_REVISION".into());
        };
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(keys) = keys.as_ref() {
            let event = ExportEvent {
                dataset_id: binding.dataset_id,
                generation: binding.generation,
                revision,
                change,
            };
            Ok(json!({"encrypted":hex::encode(seal_event(&keys.outbox,&event)?)}).to_string())
        } else {
            // Durable gaps store identifiers only, never a plaintext substitute
            // for unavailable projection write material.
            let (kind, key, operation) = match change {
                TextChange::Put { record } => {
                    let (kind, key) = record.identity();
                    (kind.to_string(), key, "gap")
                }
                TextChange::Gap { kind, key } => (kind, key, "gap"),
                TextChange::Delete { kind, key } => (kind, key, "delete"),
                TextChange::SnapshotState {
                    screenshot_id,
                    deleted,
                    ..
                } => (
                    "snapshot".into(),
                    screenshot_id.to_string(),
                    if deleted { "delete" } else { "gap" },
                ),
                TextChange::DeleteDay { date } => ("calendar".into(), date, "delete_day"),
                TextChange::SourceChanged => ("source".into(), "revision".into(), "source_changed"),
            };
            Ok(
                json!({"gap_kind":kind,"gap_key":key,"revision":revision,"operation":operation})
                    .to_string(),
            )
        }
    }
    pub(super) fn register(self: &Arc<Self>, conn: &Connection) -> Result<(), String> {
        Self::register_digest(conn)?;
        // Trigger-callable functions return only a flag, decoded ciphertext or
        // an independently encrypted envelope. Plaintext decryption functions
        // remain DIRECTONLY and are never made available to an untrusted schema.
        let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS;
        db(
            conn.create_scalar_function("cp_agent_bytes", 1, flags, move |args| {
                let encoded: String = args.get(0)?;
                hex::decode(encoded).map_err(|e| sql_error(e.to_string()))
            }),
        )?;
        let writer = self.clone();
        db(conn
            .create_scalar_function("cp_agent_enabled", 0, flags, move |_| Ok(writer.enabled())))?;
        let writer = self.clone();
        db(
            conn.create_scalar_function("cp_agent_emit", 4, flags, move |args| {
                if !writer.enabled() {
                    return Ok(None::<String>);
                }
                let kind: String = args.get(0)?;
                let key: String = args.get(1)?;
                let action: String = args.get(2)?;
                let payload: String = args.get(3)?;
                if payload.len() > 32 * 1024 * 1024 {
                    return Err(sql_error("AGENT_INVALID_RECORD".into()));
                }
                let mut value: Value =
                    serde_json::from_str(&payload).map_err(|e| sql_error(e.to_string()))?;
                if action != "delete" {
                    writer
                        .prepare_live_projection(&kind, &mut value)
                        .map_err(sql_error)?;
                } else if !matches!(
                    kind.as_str(),
                    "snapshot"
                        | "ocr"
                        | "cluster"
                        | "summary"
                        | "assignment"
                        | "recap"
                        | "calendar"
                        | "vector_key"
                ) {
                    return Err(sql_error("AGENT_INVALID_RECORD_KIND".into()));
                }
                let change = live_projection(&kind, &key, &action, value).map_err(sql_error)?;
                writer.queue_event(change).map(Some).map_err(sql_error)
            }),
        )?;
        Ok(())
    }
    fn register_digest(conn: &Connection) -> Result<(), String> {
        let flags = FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_INNOCUOUS
            | FunctionFlags::SQLITE_DETERMINISTIC;
        db(
            conn.create_scalar_function("cp_agent_digest", 1, flags, |args| {
                match args.get_raw(0) {
                    rusqlite::types::ValueRef::Null => Ok(None::<String>),
                    rusqlite::types::ValueRef::Blob(bytes) => {
                        Ok(Some(hex::encode(Sha256::digest(bytes))))
                    }
                    _ => Err(sql_error("AGENT_INVALID_SOURCE".into())),
                }
            }),
        )
    }
}

fn live_projection(
    kind: &str,
    key: &str,
    action: &str,
    mut value: Value,
) -> Result<TextChange, String> {
    if kind == "source" {
        return Ok(TextChange::SourceChanged);
    }
    if action == "delete" {
        return Ok(TextChange::Delete {
            kind: kind.into(),
            key: key.into(),
        });
    }
    if kind == "day_delete" {
        return Ok(TextChange::DeleteDay { date: key.into() });
    }
    if kind == "snapshot_state" {
        if value["requires_projection"].as_bool() == Some(true) {
            return Ok(TextChange::Gap {
                kind: "snapshot".into(),
                key: key.into(),
            });
        }
        return Ok(TextChange::SnapshotState {
            screenshot_id: value["screenshot_id"]
                .as_i64()
                .ok_or("AGENT_INVALID_RECORD")?,
            category: value["category"].as_str().map(str::to_string),
            status: value["status"].as_str().unwrap_or_default().into(),
            deleted: value["deleted"].as_bool() == Some(true),
        });
    }
    if kind == "snapshot" {
        if let Some(mut capture) = CAPTURE.with(|c| c.borrow().clone()) {
            capture.screenshot_id = value["screenshot_id"]
                .as_i64()
                .ok_or("AGENT_INVALID_RECORD")?;
            capture.captured_at_ms = value["captured_at_ms"].as_i64();
            capture.status = value["status"].as_str().unwrap_or_default().into();
            capture.category = value["category"].as_str().map(str::to_string);
            return Ok(TextChange::Put {
                record: TextRecord::Snapshot(capture),
            });
        }
        if value["encrypted"].as_bool() == Some(true) {
            return Ok(TextChange::Gap {
                kind: kind.into(),
                key: key.into(),
            });
        }
        value["visible_links"] = json!([]);
    }
    let unavailable = match kind {
        "ocr" | "vector_key" => value["key_envelope"].is_null(),
        "cluster" => {
            value["anchor_text"].is_null() || value["missing_name"].as_bool() == Some(true)
        }
        "summary" => value["unavailable"].as_bool() == Some(true),
        "recap" => {
            if value["part"] == "search" {
                value["key_envelope"].is_null()
            } else {
                value["content"].is_null()
            }
        }
        _ => false,
    };
    if unavailable {
        return Ok(TextChange::Gap {
            kind: kind.into(),
            key: key.into(),
        });
    }
    let record = decode_projection(kind, value)?;
    Ok(TextChange::Put { record })
}
fn decode_projection(kind: &str, mut value: Value) -> Result<TextRecord, String> {
    let decode =
        |value| serde_json::from_value(value).map_err(|e| format!("AGENT_INVALID_RECORD: {e}"));
    match kind {
        "snapshot" => serde_json::from_value(value)
            .map(TextRecord::Snapshot)
            .map_err(|e| e.to_string()),
        "ocr" => Ok(TextRecord::OcrAccess(OcrAccess {
            ocr_id: value["ocr_id"].as_i64().ok_or("AGENT_INVALID_RECORD")?,
            screenshot_id: value["screenshot_id"]
                .as_i64()
                .ok_or("AGENT_INVALID_RECORD")?,
            payload_digest: serde_json::from_value(value["payload_digest"].clone())
                .map_err(|_| "AGENT_INVALID_RECORD")?,
            key_envelope: hex::decode(
                value["key_envelope"]
                    .as_str()
                    .ok_or("AGENT_INVALID_RECORD")?,
            )
            .map_err(|_| "AGENT_INVALID_RECORD")?,
        })),
        "vector_key" => Ok(TextRecord::VectorAccess(VectorAccess {
            key_id: value["key_id"]
                .as_str()
                .ok_or("AGENT_INVALID_RECORD")?
                .into(),
            key_envelope: hex::decode(
                value["key_envelope"]
                    .as_str()
                    .ok_or("AGENT_INVALID_RECORD")?,
            )
            .map_err(|_| "AGENT_INVALID_RECORD")?,
        })),
        "cluster" => serde_json::from_value(value)
            .map(TextRecord::Cluster)
            .map_err(|e| e.to_string()),
        "summary" => {
            let points = value["key_points_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null);
            value["key_points"] = json!(points
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>());
            let evidence = value["evidence_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null);
            value["evidence"] = json!(evidence
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| serde_json::from_value::<TextEvidence>(v.clone()).ok())
                .collect::<Vec<_>>());
            serde_json::from_value(value)
                .map(TextRecord::Summary)
                .map_err(|e| e.to_string())
        }
        "assignment" => serde_json::from_value(value)
            .map(TextRecord::Assignment)
            .map_err(|e| e.to_string()),
        "calendar" => serde_json::from_value(value)
            .map(TextRecord::Calendar)
            .map_err(|e| e.to_string()),
        "recap" => {
            let part = value["part"].as_str().unwrap_or_default();
            if part == "search" {
                return Ok(TextRecord::RecapSearchAccess(RecapSearchAccess {
                    date: value["date"].as_str().ok_or("AGENT_INVALID_RECORD")?.into(),
                    key: value["key"].as_str().ok_or("AGENT_INVALID_RECORD")?.into(),
                    revision: value["revision"].as_i64().ok_or("AGENT_INVALID_RECORD")?,
                    payload_digest: serde_json::from_value(value["payload_digest"].clone())
                        .map_err(|_| "AGENT_INVALID_RECORD")?,
                    key_envelope: hex::decode(
                        value["key_envelope"]
                            .as_str()
                            .ok_or("AGENT_INVALID_RECORD")?,
                    )
                    .map_err(|_| "AGENT_INVALID_RECORD")?,
                }));
            }
            let raw = value["content"].as_str().ok_or("AGENT_INVALID_RECORD")?;
            let mut content: Value =
                serde_json::from_str(raw).map_err(|_| "AGENT_INVALID_RECORD")?;
            if part == "corrections" {
                content = content["current"].take();
            } else if part == "summary" {
                // CachedSummary also contains execution diagnostics. Export
                // only the typed user-facing recap summary.
                content = content["summary"].take();
                if content.is_null() {
                    content = json!({});
                }
            }
            if content.is_array() {
                content = json!({"threads":content});
            }
            let mut content = serde_json::from_value::<RecapContent>(content)
                .map_err(|_| "AGENT_INVALID_RECORD")?;
            content.vectors.clear();
            value["content"] = json!(content);
            serde_json::from_value(value)
                .map(TextRecord::Recap)
                .map_err(|e| e.to_string())
        }
        "privacy" => decode(value).map(TextRecord::Privacy),
        _ => Err("AGENT_INVALID_RECORD_KIND".into()),
    }
}

impl StorageState {
    pub(super) fn init_agent_access_tables(&self, conn: &Connection) -> Result<(), String> {
        self.agent_writer.register(conn)?;
        db(conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_access_state(id INTEGER PRIMARY KEY CHECK(id=1),binding TEXT NOT NULL,checkpoint TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,envelope BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_projection_gaps(kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,operation TEXT NOT NULL,PRIMARY KEY(kind,key));
            CREATE VIEW IF NOT EXISTS agent_enqueue AS SELECT NULL AS envelope WHERE 0;
            DROP TRIGGER IF EXISTS agent_enqueue_route;
            CREATE TRIGGER agent_enqueue_route INSTEAD OF INSERT ON agent_enqueue BEGIN
                INSERT INTO agent_outbox(envelope) SELECT cp_agent_bytes(json_extract(NEW.envelope,'$.encrypted')) WHERE json_type(NEW.envelope,'$.encrypted')='text';
                INSERT INTO agent_projection_gaps(kind,key,revision,operation)
                    SELECT json_extract(NEW.envelope,'$.gap_kind'),json_extract(NEW.envelope,'$.gap_key'),json_extract(NEW.envelope,'$.revision'),json_extract(NEW.envelope,'$.operation')
                    WHERE json_type(NEW.envelope,'$.gap_kind')='text'
                    ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,operation=excluded.operation WHERE agent_projection_gaps.revision<excluded.revision;
            END;"))?;
        install_triggers(conn)
    }
    pub(super) fn restore_agent_access(&self, conn: &Connection) -> Result<(), String> {
        let binding: Option<String> = db(conn
            .query_row(
                "SELECT binding FROM agent_access_state WHERE id=1",
                [],
                |r| r.get(0),
            )
            .optional())?;
        let Some(binding) = binding else {
            self.agent_writer.configured.store(false, Ordering::Release);
            *self
                .agent_writer
                .keys
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .agent_writer
                .configured_binding
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            return Ok(());
        };
        let binding: AgentViewBinding =
            serde_json::from_str(&binding).map_err(|_| "AGENT_INVALID_BINDING")?;
        self.agent_writer.configured.store(true, Ordering::Release);
        *self
            .agent_writer
            .configured_binding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(binding.clone());
        let dataset: String = db(conn.query_row(
            "SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'",
            [],
            |r| r.get(0),
        ))?;
        let Response::AgentStatus(status) = self.agent_writer.request(Request::AgentStatus {})?
        else {
            return Err("AGENT_INVALID_STATUS".into());
        };
        if !status.enabled
            || status.binding.as_ref() != Some(&binding)
            || dataset != binding.dataset_id
        {
            self.agent_writer.request(Request::AgentDisable {})?;
            db(conn.execute_batch("DELETE FROM agent_access_state; DELETE FROM agent_outbox;DELETE FROM agent_projection_gaps;"))?;
            self.agent_writer.configured.store(false, Ordering::Release);
            *self
                .agent_writer
                .keys
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.agent_writer.clear_staging();
            *self
                .agent_writer
                .configured_binding
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.agent_writer.progress_phase("disabled", None);
            return Ok(());
        }
        let Response::AgentKeys(keys) = self.agent_writer.request(Request::AgentWriterKeys {
            generation: binding.generation,
        })?
        else {
            return Err("AGENT_WRITE_KEY_UNAVAILABLE".into());
        };
        self.agent_writer.install_keys(keys)?;
        self.agent_writer.progress_phase("syncing", None);
        Ok(())
    }
    pub(crate) fn agent_access_enabled(&self) -> bool {
        self.agent_writer.enabled()
    }
    pub(crate) fn agent_preparation(&self) -> AgentPreparationProgress {
        self.agent_writer.progress()
    }
    pub(crate) fn agent_binding(&self) -> Option<AgentViewBinding> {
        self.agent_writer.binding()
    }
    pub(crate) fn begin_agent_access(&self) -> Result<AgentViewBinding, String> {
        let _sync = self
            .agent_writer
            .sync_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        if let Some(binding) = self.agent_binding() {
            return Ok(binding);
        }
        let dataset = {
            let guard = self.get_connection_named("agent_prepare_dataset")?;
            db(guard.as_ref().unwrap().query_row(
                "SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'",
                [],
                |r| r.get::<_, String>(0),
            ))?
        };
        let path = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .join("agent-memory.db");
        let Response::AgentKeys(keys) = self.agent_writer.request(Request::AgentPrepare {
            dataset_id: dataset,
            view_path: path.to_string_lossy().into_owned(),
        })?
        else {
            return Err("AGENT_WRITE_KEY_UNAVAILABLE".into());
        };
        let binding = keys.binding.clone();
        let result = (|| {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let file = PathBuf::from(format!("{}{}", path.display(), suffix));
                if file.exists() {
                    std::fs::remove_file(file).map_err(|e| e.to_string())?;
                }
            }
            let mut repo = AgentRepository::open(&path, &keys.content_key.0, &binding, false)?;
            repo.configure_source(
                &self.database_key()?,
                &self.credential_state.get_hmac_key()?,
            )?;
            let Response::AgentStatus(status) =
                self.agent_writer.request(Request::AgentStatus {})?
            else {
                return Err("AGENT_INVALID_STATUS".into());
            };
            let checkpoint = Checkpoint {
                generation: binding.generation.clone(),
                phase: 0,
                cursor: String::new(),
                baseline: status.source_revision,
                complete: false,
            };
            let guard = self.get_connection_named("agent_prepare_state")?;
            let conn = guard.as_ref().unwrap();
            db(conn.execute(
                "INSERT INTO agent_access_state VALUES(1,?1,?2)",
                params![
                    serde_json::to_string(&binding).map_err(|e| e.to_string())?,
                    serde_json::to_string(&checkpoint).map_err(|e| e.to_string())?
                ],
            ))?;
            let total=db(conn.query_row("SELECT (SELECT COUNT(*) FROM screenshots WHERE is_deleted=0)+(SELECT COUNT(*) FROM ocr_results WHERE is_deleted=0)+(SELECT COUNT(*) FROM smart_clusters)+(SELECT COUNT(*) FROM smart_cluster_summaries)+(SELECT COUNT(*) FROM smart_cluster_assignments)+(SELECT COUNT(*) FROM recap_days)+(SELECT COUNT(*) FROM recap_payloads WHERE kind IN ('batch','summary','corrections','threads','search'))+(SELECT COUNT(*) FROM derived_embeddings)",[],|r|r.get::<_,i64>(0)))?.max(0) as u64;
            *self
                .agent_writer
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = AgentPreparationProgress {
                phase: "preparing".into(),
                processed: 0,
                total,
                error: None,
            };
            drop(guard);
            drop(repo);
            self.agent_writer.install_keys(keys)?;
            self.agent_writer.configured.store(true, Ordering::Release);
            self.agent_writer.progress_phase("preparing", None);
            Ok(binding.clone())
        })();
        if result.is_err() {
            let _ = self.agent_writer.request(Request::AgentDisable {});
            self.agent_writer.configured.store(false, Ordering::Release);
            *self
                .agent_writer
                .keys
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *self
                .agent_writer
                .configured_binding
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.agent_writer.clear_staging();
        }
        result
    }
    pub(crate) fn disable_agent_access(&self) -> Result<(), String> {
        self.agent_writer.request(Request::AgentDisable {})?;
        self.finish_agent_access_disable()
    }
    pub(crate) fn finish_agent_access_disable(&self) -> Result<(), String> {
        self.agent_writer.cancelled.store(true, Ordering::Release);
        self.agent_writer.configured.store(false, Ordering::Release);
        let _sync = self
            .agent_writer
            .sync_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let binding = self.agent_binding();
        *self
            .agent_writer
            .keys
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.agent_writer.configured.store(false, Ordering::Release);
        *self
            .agent_writer
            .configured_binding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.agent_writer.clear_staging();
        self.agent_writer.progress_phase("disabled", None);
        let guard = self.get_connection_named("agent_disable")?;
        db(guard.as_ref().unwrap().execute_batch("DELETE FROM agent_access_state;DELETE FROM agent_outbox;DELETE FROM agent_projection_gaps;"))?;
        drop(guard);
        if let Some(binding) = binding {
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let path = PathBuf::from(format!("{}{}", binding.view_path, suffix));
                if path.exists() {
                    if let Err(e) = std::fs::remove_file(path) {
                        tracing::warn!("Encrypted Agent view removal deferred: {e}");
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn start_agent_exporter(self: &Arc<Self>) {
        if self.agent_writer.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let storage = self.clone();
        std::thread::spawn(move || {
            while !storage.agent_writer.stopped.load(Ordering::Acquire) {
                if storage.agent_access_enabled() && !storage.agent_maintenance_pending() {
                    if let Err(error) = storage.sync_agent_access() {
                        let phase = if error.contains("AUTH_REQUIRED") {
                            "waiting_for_unlock"
                        } else {
                            "error"
                        };
                        storage.agent_writer.progress_phase(phase, Some(error));
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
    }
    pub(crate) fn sync_agent_access(&self) -> Result<(), String> {
        let _activity = self.foreground_db_activity.read("agent_export");
        if self.agent_maintenance_pending() {
            return Ok(());
        }
        let _sync = self
            .agent_writer
            .sync_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.agent_access_enabled()
            && self
                .agent_writer
                .keys
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            let guard = self.get_connection_named("agent_restore_keys")?;
            self.restore_agent_access(guard.as_ref().unwrap())?;
        }
        let Some(binding) = self.agent_binding() else {
            return Ok(());
        };
        if self.agent_preparation().phase == "ready" {
            let pending = {
                let guard = self.get_connection_named("agent_sync_pending")?;
                db(guard.as_ref().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM agent_outbox UNION ALL SELECT 1 FROM agent_projection_gaps)",[],|r|r.get::<_,bool>(0)))?
            };
            if !pending {
                if let Response::AgentStatus(status) =
                    self.agent_writer.request(Request::AgentStatus {})?
                {
                    if status.enabled
                        && status.binding.as_ref() == Some(&binding)
                        && status.phase == "ready"
                        && status.source_revision == status.applied_revision
                    {
                        return Ok(());
                    }
                }
            }
        }
        let mut repo = {
            let keys = self
                .agent_writer
                .keys
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let keys = keys.as_ref().ok_or("AGENT_WRITE_KEY_UNAVAILABLE")?;
            AgentRepository::open(
                Path::new(&binding.view_path),
                &keys.content,
                &binding,
                false,
            )?
        };
        let mut checkpoint: Checkpoint = {
            let guard = self.get_connection_named("agent_checkpoint")?;
            let value: String = db(guard.as_ref().unwrap().query_row(
                "SELECT checkpoint FROM agent_access_state WHERE id=1",
                [],
                |r| r.get(0),
            ))?;
            serde_json::from_str(&value).map_err(|_| "AGENT_INVALID_CHECKPOINT")?
        };
        if checkpoint.generation != binding.generation {
            return Err("AGENT_DATASET_MISMATCH".into());
        }
        if !checkpoint.complete {
            if !self.is_session_valid() {
                return Err("AUTH_REQUIRED".into());
            }
            self.agent_writer.progress_phase("preparing", None);
            let mut timings = PreparationTimings::new("");
            while checkpoint.phase < SOURCES.len() {
                if self.agent_maintenance_pending() {
                    return Ok(());
                }
                if self.agent_writer.cancelled.load(Ordering::Acquire) {
                    return Err("AGENT_CANCELLED".into());
                }
                if !self.is_session_valid() {
                    return Err("AUTH_REQUIRED".into());
                }
                let source = &SOURCES[checkpoint.phase];
                let kind = source.kind;
                if timings.kind != kind {
                    timings = PreparationTimings::new(kind);
                }
                let read_started = Instant::now();
                let rows = self.agent_source_page(source, &checkpoint.cursor, None)?;
                timings.read += read_started.elapsed();
                let complete = rows.is_empty();
                if complete {
                    checkpoint.phase += 1;
                    checkpoint.cursor.clear();
                } else {
                    let processed = rows.len() as u64;
                    let cursor = rows.last().unwrap().0.clone();
                    let mut events = Vec::with_capacity(rows.len());
                    let hydrate_started = Instant::now();
                    for (_, value) in rows {
                        let records =
                            self.hydrate_agent_record(kind, value, &mut timings.row_keys)?;
                        for record in records {
                            events.push(ExportEvent {
                                dataset_id: binding.dataset_id.clone(),
                                generation: binding.generation.clone(),
                                revision: checkpoint.baseline,
                                change: TextChange::Put { record },
                            });
                        }
                    }
                    timings.hydrate += hydrate_started.elapsed();
                    if self.agent_writer.cancelled.load(Ordering::Acquire) {
                        return Err("AGENT_CANCELLED".into());
                    }
                    if !self.is_session_valid() {
                        return Err("AUTH_REQUIRED".into());
                    }
                    let write_started = Instant::now();
                    repo.apply_batch(&events)?;
                    timings.write += write_started.elapsed();
                    timings.processed += processed;
                    checkpoint.cursor = cursor;
                    self.agent_writer
                        .progress
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .processed += processed;
                }
                let checkpoint_started = Instant::now();
                self.save_agent_checkpoint(&checkpoint)?;
                timings.checkpoint += checkpoint_started.elapsed();
                // Keep source updates moving between snapshot pages.
                let outbox_started = Instant::now();
                self.apply_agent_outbox(&binding, &mut repo)?;
                timings.outbox += outbox_started.elapsed();
                timings.page_finished(complete);
            }
            checkpoint.complete = true;
            self.save_agent_checkpoint(&checkpoint)?;
        }
        self.apply_agent_outbox(&binding, &mut repo)?;
        if self.is_session_valid() {
            let mut row_key_timings = RowKeyDecryptTimings::default();
            for (kind, key, revision) in repo.gaps()? {
                if self.agent_maintenance_pending() {
                    return Ok(());
                }
                if kind == "privacy" {
                    continue;
                }
                let Some(source) = SOURCES.iter().find(|source| source.kind == kind) else {
                    return Err("AGENT_INVALID_GAP".into());
                };
                let rows = self.agent_source_page(source, "", Some(&key))?;
                if rows.is_empty() {
                    repo.apply(&ExportEvent {
                        dataset_id: binding.dataset_id.clone(),
                        generation: binding.generation.clone(),
                        revision,
                        change: TextChange::Delete {
                            kind: kind.clone(),
                            key: key.clone(),
                        },
                    })?;
                } else {
                    for (_, value) in rows {
                        for record in
                            self.hydrate_agent_record(&kind, value, &mut row_key_timings)?
                        {
                            repo.apply(&ExportEvent {
                                dataset_id: binding.dataset_id.clone(),
                                generation: binding.generation.clone(),
                                revision,
                                change: TextChange::Put { record },
                            })?;
                        }
                    }
                }
                repo.clear_gap(&kind, &key, revision)?;
            }
        }
        if !repo.has_gaps()? {
            self.sync_agent_ocr_index(&repo)?;
        }
        if self.agent_maintenance_pending() {
            return Ok(());
        }
        let config = self
            .load_policy()?
            .get("sensitive_filter")
            .cloned()
            .unwrap_or_else(|| json!(crate::sensitive_filter::SensitiveFilterConfig::default()));
        let privacy = decode_projection("privacy", config)?;
        let guard = self.get_connection_named("agent_publish")?;
        let conn = guard.as_ref().unwrap();
        let pending: bool = db(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_outbox UNION ALL SELECT 1 FROM agent_projection_gaps)",
            [],
            |r| r.get(0),
        ))?;
        if pending {
            return Ok(());
        }
        let Response::AgentStatus(status) = self.agent_writer.request(Request::AgentStatus {})?
        else {
            return Err("AGENT_INVALID_STATUS".into());
        };
        if status.binding.as_ref() != Some(&binding) || !status.enabled {
            return Err("AGENT_DATASET_MISMATCH".into());
        }
        repo.apply(&ExportEvent {
            dataset_id: binding.dataset_id.clone(),
            generation: binding.generation.clone(),
            revision: status.source_revision,
            change: TextChange::Put { record: privacy },
        })?;
        let complete = !repo.has_gaps()?;
        self.agent_writer.request(Request::AgentPublish {
            generation: binding.generation,
            revision: status.source_revision,
            complete,
        })?;
        self.agent_writer.progress_phase(
            if complete {
                "ready"
            } else {
                "waiting_for_unlock"
            },
            None,
        );
        Ok(())
    }
    fn agent_maintenance_pending(&self) -> bool {
        self.is_database_maintenance_pending() || crate::maintenance::is_active()
    }
    fn sync_agent_ocr_index(&self, repo: &AgentRepository) -> Result<(), String> {
        use super::blind_index::{BlindIndex, MutationStats};
        use std::collections::BTreeMap;
        // Decryption and token hashing run outside the main database mutex.
        // Only bounded posting updates enter its write transaction.
        loop {
            if self.agent_maintenance_pending() {
                return Ok(());
            }
            if self.agent_writer.cancelled.load(Ordering::Acquire) {
                return Err("AGENT_CANCELLED".into());
            }
            let rows = repo.unindexed_ocr(0, 32)?;
            if rows.is_empty() {
                return Ok(());
            }
            let hmac = repo.source_hmac_key()?;
            let mut row_hashes = Vec::new();
            let mut tokens = BTreeMap::<String, roaring::RoaringBitmap>::new();
            for mut row in rows {
                let id = u32::try_from(row.ocr_id).map_err(|_| "AGENT_INDEX_ID_UNSUPPORTED")?;
                let plaintext = Zeroizing::new(std::mem::take(&mut row.text));
                let hash = Self::compute_hmac_hash(&plaintext, hmac);
                for term in Self::bigram_tokenize(&plaintext) {
                    tokens
                        .entry(Self::compute_hmac_hash(&term, hmac))
                        .or_default()
                        .insert(id);
                }
                row_hashes.push((row.ocr_id, hash, repo.ocr_payload_digest(row.ocr_id)?));
            }
            let guard = self.get_connection_named("agent_index_write")?;
            let conn = guard.as_ref().unwrap();
            let tx = db(conn.unchecked_transaction())?;
            let mut active = roaring::RoaringBitmap::new();
            {
                let mut read = db(tx.prepare("SELECT o.text_enc FROM ocr_results o JOIN screenshots s ON s.id=o.screenshot_id
                    WHERE o.id=?1 AND o.text_hash='' AND o.is_deleted=0 AND s.is_deleted=0"))?;
                let mut update = db(tx.prepare("UPDATE ocr_results SET text_hash=?2 WHERE id=?1 AND text_hash='' AND is_deleted=0"))?;
                for (id, hash, digest) in row_hashes {
                    let encrypted: Option<Vec<u8>> =
                        db(read.query_row([id], |r| r.get(0)).optional())?;
                    if encrypted.is_some_and(|v| Sha256::digest(&v).as_slice() == digest.as_slice())
                    {
                        db(update.execute(params![id, hash]))?;
                        active.insert(id as u32);
                    }
                }
            }
            let index = BlindIndex::open(&tx)?;
            let mut stats = MutationStats::default();
            for (hash, posting) in tokens {
                let posting = posting & &active;
                if !posting.is_empty() {
                    index.add(&hash, &posting, &mut stats)?;
                }
            }
            db(tx.commit())?;
        }
    }
    fn save_agent_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), String> {
        let guard = self.get_connection_named("agent_save_checkpoint")?;
        db(guard.as_ref().unwrap().execute(
            "UPDATE agent_access_state SET checkpoint=?1 WHERE id=1",
            [serde_json::to_string(checkpoint).map_err(|e| e.to_string())?],
        ))?;
        Ok(())
    }
    fn apply_agent_outbox(
        &self,
        binding: &AgentViewBinding,
        repo: &mut AgentRepository,
    ) -> Result<(), String> {
        let gaps: Vec<(String, String, i64, String)> = {
            let guard = self.get_connection_named("agent_read_gaps")?;
            let mut query = db(guard.as_ref().unwrap().prepare(
                "SELECT kind,key,revision,operation FROM agent_projection_gaps ORDER BY revision",
            ))?;
            let rows =
                db(query.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))))?;
            db(rows.collect())?
        };
        for (kind, key, revision, operation) in gaps {
            if self.agent_maintenance_pending() {
                return Ok(());
            }
            if self.agent_writer.cancelled.load(Ordering::Acquire) {
                return Err("AGENT_CANCELLED".into());
            }
            let change = match operation.as_str() {
                "delete" => TextChange::Delete {
                    kind: kind.clone(),
                    key: key.clone(),
                },
                "delete_day" => TextChange::DeleteDay { date: key.clone() },
                "source_changed" => TextChange::SourceChanged,
                _ => TextChange::Gap {
                    kind: kind.clone(),
                    key: key.clone(),
                },
            };
            repo.apply(&ExportEvent {
                dataset_id: binding.dataset_id.clone(),
                generation: binding.generation.clone(),
                revision,
                change,
            })?;
            let guard = self.get_connection_named("agent_ack_gap")?;
            db(guard.as_ref().unwrap().execute(
                "DELETE FROM agent_projection_gaps WHERE kind=?1 AND key=?2 AND revision<=?3",
                params![kind, key, revision],
            ))?;
        }
        loop {
            if self.agent_maintenance_pending() {
                return Ok(());
            }
            if self.agent_writer.cancelled.load(Ordering::Acquire) {
                return Err("AGENT_CANCELLED".into());
            }
            let rows: Vec<(i64, Vec<u8>)> = {
                let guard = self.get_connection_named("agent_read_outbox")?;
                let mut statement = db(guard
                    .as_ref()
                    .unwrap()
                    .prepare("SELECT id,envelope FROM agent_outbox ORDER BY id LIMIT 256"))?;
                let rows = db(statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))))?;
                db(rows.collect())?
            };
            if rows.is_empty() {
                break;
            }
            let last = rows.last().unwrap().0;
            let outbox_key = {
                let keys = self
                    .agent_writer
                    .keys
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                keys.as_ref()
                    .ok_or("AGENT_WRITE_KEY_UNAVAILABLE")?
                    .outbox
                    .clone()
            };
            let mut events = Vec::with_capacity(rows.len());
            for (_, bytes) in rows {
                if self.agent_writer.cancelled.load(Ordering::Acquire) {
                    return Err("AGENT_CANCELLED".into());
                }
                events.push(open_event(&outbox_key, binding, &bytes)?);
            }
            if self.agent_maintenance_pending() {
                return Ok(());
            }
            if self.agent_writer.cancelled.load(Ordering::Acquire) {
                return Err("AGENT_CANCELLED".into());
            }
            repo.apply_outbox_batch(&events, last)?;
            let guard = self.get_connection_named("agent_ack_outbox")?;
            db(guard
                .as_ref()
                .unwrap()
                .execute("DELETE FROM agent_outbox WHERE id<=?1", [last]))?;
        }
        Ok(())
    }
    fn agent_source_page(
        &self,
        source: &schema::Source,
        cursor: &str,
        key: Option<&str>,
    ) -> Result<Vec<(String, Value)>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let (sql, parameters) = source.query(cursor, key)?;
        let rows: Vec<(String, String)> =
            self.with_read_connection_named("agent_export_source", |conn| {
                AgentWriter::register_digest(conn)?;
                if !self.is_session_valid() {
                    return Err("AUTH_REQUIRED".into());
                }
                let mut statement = db(conn.prepare(&sql))?;
                let rows = db(
                    statement.query_map(rusqlite::params_from_iter(parameters), |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    }),
                )?;
                db(rows.collect())
            })?;
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        rows.into_iter()
            .map(|(key, payload)| {
                Ok((
                    key,
                    serde_json::from_str(&payload).map_err(|e| e.to_string())?,
                ))
            })
            .collect()
    }
    fn hydrate_agent_record(
        &self,
        kind: &str,
        mut value: Value,
        timings: &mut RowKeyDecryptTimings,
    ) -> Result<Vec<TextRecord>, String> {
        if kind == "ocr" || kind == "vector_key" || (kind == "recap" && value["part"] == "search") {
            if !self.is_session_valid() {
                return Err("AUTH_REQUIRED".into());
            }
            let encoded = value["key_enc"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("AGENT_INVALID_SOURCE")?;
            let wrapped = hex::decode(encoded).map_err(|_| "AGENT_INVALID_SOURCE")?;
            let raw_key = Zeroizing::new(if kind == "vector_key" {
                crate::credential_manager::unwrap_storage_domain_key(
                    &self.credential_state,
                    &wrapped,
                )
                .map_err(|e| e.to_string())?
            } else {
                self.unwrap_agent_history_key(&wrapped, timings)?
            });
            let (domain, id, digest) = if kind == "vector_key" {
                (
                    "vectors",
                    value["key_id"]
                        .as_str()
                        .ok_or("AGENT_INVALID_SOURCE")?
                        .to_string(),
                    Vec::new(),
                )
            } else {
                let digest = hex::decode(
                    value["payload_digest"]
                        .as_str()
                        .ok_or("AGENT_INVALID_SOURCE")?,
                )
                .map_err(|_| "AGENT_INVALID_SOURCE")?;
                let id = if kind == "ocr" {
                    value["ocr_id"]
                        .as_i64()
                        .ok_or("AGENT_INVALID_SOURCE")?
                        .to_string()
                } else {
                    value["key"]
                        .as_str()
                        .ok_or("AGENT_INVALID_SOURCE")?
                        .to_string()
                };
                (
                    if kind == "ocr" { "ocr" } else { "recap_search" },
                    id,
                    digest,
                )
            };
            value["key_envelope"] = json!(hex::encode(
                self.agent_writer
                    .wrap_access_key(domain, &id, &digest, &raw_key)?
            ));
            value["payload_digest"] = json!(digest);
            if !self.is_session_valid() {
                return Err("AUTH_REQUIRED".into());
            }
            return Ok(vec![decode_projection(kind, value)?]);
        }
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        if kind == "snapshot" {
            let key = value["key_enc"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(hex::decode)
                .transpose()
                .map_err(|_| "AGENT_INVALID_SOURCE")?;
            let mut row_key = None;
            for field in ["window_title", "process_name", "page_url"] {
                let encoded = value[format!("{field}_enc")]
                    .as_str()
                    .filter(|s| !s.is_empty());
                if let Some(encoded) = encoded {
                    if row_key.is_none() {
                        row_key = Some(Zeroizing::new(self.unwrap_agent_history_key(
                            key.as_deref().ok_or("AGENT_INVALID_SOURCE")?,
                            timings,
                        )?));
                    }
                    let bytes = hex::decode(encoded).map_err(|_| "AGENT_INVALID_SOURCE")?;
                    let plain = Zeroizing::new(
                        decrypt_with_master_key(row_key.as_ref().unwrap(), &bytes)
                            .map_err(|e| e.to_string())?,
                    );
                    value[field] =
                        json!(String::from_utf8(plain.to_vec())
                            .map_err(|_| "AGENT_INVALID_SOURCE")?);
                }
            }
            value["visible_links"] = json!([]);
            if let (Some(data), Some(key)) = (
                value["links_enc"].as_str().filter(|s| !s.is_empty()),
                value["links_key"].as_str().filter(|s| !s.is_empty()),
            ) {
                let plain = self.decrypt_agent_history_payload(
                    &hex::decode(data).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    &hex::decode(key).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    timings,
                )?;
                let links: Vec<TextLink> =
                    serde_json::from_slice(&plain).map_err(|_| "AGENT_INVALID_SOURCE")?;
                value["visible_links"] = json!(links);
            }
        } else if kind == "recap" {
            let data = value["data_enc"].as_str().filter(|s| !s.is_empty());
            let key = value["key_enc"].as_str().filter(|s| !s.is_empty());
            if let (Some(data), Some(key)) = (data, key) {
                let plain = self.decrypt_agent_history_payload(
                    &hex::decode(data).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    &hex::decode(key).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    timings,
                )?;
                value[if kind == "ocr" { "text" } else { "content" }] =
                    json!(String::from_utf8(plain.to_vec()).map_err(|_| "AGENT_INVALID_SOURCE")?);
            }
        }
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        Ok(vec![decode_projection(kind, value)?])
    }

    fn unwrap_agent_history_key(
        &self,
        wrapped: &[u8],
        timings: &mut RowKeyDecryptTimings,
    ) -> Result<Vec<u8>, String> {
        crate::credential_manager::decrypt_row_key_with_cng_measured(
            &self.credential_state,
            wrapped,
            timings,
            &|| self.agent_writer.cancelled.load(Ordering::Acquire),
        )
        .map_err(|error| match error {
            CredentialError::AuthRequired => "AUTH_REQUIRED".into(),
            _ => error.to_string(),
        })?
        .ok_or_else(|| "AGENT_CANCELLED".into())
    }

    fn decrypt_agent_history_payload(
        &self,
        encrypted: &[u8],
        wrapped: &[u8],
        timings: &mut RowKeyDecryptTimings,
    ) -> Result<Zeroizing<Vec<u8>>, String> {
        let key = Zeroizing::new(self.unwrap_agent_history_key(wrapped, timings)?);
        decrypt_with_master_key(&key, encrypted)
            .map(Zeroizing::new)
            .map_err(|error| error.to_string())
    }
    pub(super) fn write_agent_policy(
        &self,
        path: &Path,
        bytes: &[u8],
        privacy: Value,
    ) -> Result<(), String> {
        if !self.agent_access_enabled() {
            return std::fs::write(path, bytes).map_err(|e| e.to_string());
        }
        let guard = self.get_connection_named("agent_privacy_policy")?;
        let change = TextChange::Put {
            record: decode_projection("privacy", privacy)?,
        };
        let envelope = self.agent_writer.queue_event(change)?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
        db(guard
            .as_ref()
            .unwrap()
            .execute("INSERT INTO agent_enqueue(envelope) VALUES(?1)", [envelope]))?;
        Ok(())
    }

    /// Dataset replacement and directory changes revoke grants before any
    /// storage files move. Ordinary shutdown preserves the service authority.
    pub(crate) fn retire_agent_access_for_dataset_change(&self) -> Result<(), String> {
        if !self.agent_access_enabled() {
            return Ok(());
        }
        self.agent_writer.request(Request::AgentDisable {})?;
        self.agent_writer.cancelled.store(true, Ordering::Release);
        let _sync = self
            .agent_writer
            .sync_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *self
            .agent_writer
            .keys
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.agent_writer.configured.store(false, Ordering::Release);
        *self
            .agent_writer
            .configured_binding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.agent_writer.clear_staging();
        let guard = self.get_connection_named("agent_retire_dataset")?;
        db(guard
            .as_ref()
            .unwrap()
            .execute_batch("DELETE FROM agent_access_state;DELETE FROM agent_outbox;DELETE FROM agent_projection_gaps;"))?;
        self.agent_writer.progress_phase("disabled", None);
        Ok(())
    }
}

mod schema;
use schema::{install_triggers, SOURCES};
#[cfg(test)]
mod tests;
