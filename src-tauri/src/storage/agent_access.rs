//! Desktop-only typed exporter. Archive keys stay here; the reader links only
//! the independent agent-access crate. Every source trigger fences reads before
//! committing its independently encrypted outbox event.
use super::StorageState;
use crate::credential_manager::{decrypt_with_master_key, encrypt_with_master_key};
use carbonpaper_agent_access::{
    repository::{open_event, seal_event, AgentRepository},
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
    time::Instant,
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
struct WriterKeys {
    content: Zeroizing<Vec<u8>>,
    outbox: Zeroizing<Vec<u8>>,
}
#[derive(Default)]
pub(crate) struct AgentWriter {
    keys: Mutex<Option<WriterKeys>>,
    configured_binding: Mutex<Option<AgentViewBinding>>,
    pending: Mutex<HashMap<[u8; 32], (Instant, Vec<u8>)>>,
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
    fn progress_phase(&self, phase: &str, error: Option<String>) {
        let mut state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = phase.into();
        state.error = error;
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
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
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
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        let keys = keys.as_ref()?;
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (time, bytes) = pending.get(&<[u8; 32]>::from(Sha256::digest(ciphertext)))?;
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
    fn prepare_live_projection(&self, kind: &str, value: &mut Value) -> Result<(), String> {
        match kind {
            "snapshot" | "snapshot_state" | "assignment" | "calendar" | "day_delete" => {}
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
            "ocr" | "recap" | "vector" => {
                let (encoded, target) = match kind {
                    "ocr" => ("text_hex", "text"),
                    "recap" => ("content_hex", "content"),
                    _ => ("vector_hex", "vector_hex"),
                };
                if let Some(hex) = value[encoded].as_str().filter(|s| !s.is_empty()) {
                    let bytes = hex::decode(hex).map_err(|_| "AGENT_INVALID_RECORD")?;
                    let plain = self.staged(&bytes);
                    value[target] = if kind == "vector" {
                        json!(plain.map(|p| hex::encode(&*p)))
                    } else {
                        json!(plain.and_then(|p| String::from_utf8(p.to_vec()).ok()))
                    };
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
            };
            Ok(
                json!({"gap_kind":kind,"gap_key":key,"revision":revision,"operation":operation})
                    .to_string(),
            )
        }
    }
    pub(super) fn register(self: &Arc<Self>, conn: &Connection) -> Result<(), String> {
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
                        | "vector"
                ) {
                    return Err(sql_error("AGENT_INVALID_RECORD_KIND".into()));
                }
                let change = live_projection(&kind, &key, &action, value).map_err(sql_error)?;
                writer.queue_event(change).map(Some).map_err(sql_error)
            }),
        )?;
        Ok(())
    }
}

fn live_projection(
    kind: &str,
    key: &str,
    action: &str,
    mut value: Value,
) -> Result<TextChange, String> {
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
        "ocr" => value["text"].is_null(),
        "cluster" => {
            value["anchor_text"].is_null() || value["missing_name"].as_bool() == Some(true)
        }
        "summary" => value["unavailable"].as_bool() == Some(true),
        "recap" => value["content"].is_null(),
        "vector" => value["vector_hex"].is_null(),
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
        "ocr" => {
            let coords: Vec<Vec<f64>> = value["box_coords"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|point| {
                    point
                        .as_array()
                        .filter(|v| v.len() == 2)
                        .and_then(|v| v.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
                })
                .collect();
            value["box_coords"] = json!(coords);
            value["confidence"] = json!(value["confidence"].as_f64().unwrap_or(0.0));
            serde_json::from_value(value)
                .map(TextRecord::Ocr)
                .map_err(|e| e.to_string())
        }
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
            if part == "search" {
                if content.version != crate::recap_text_contract::INDEX_VERSION
                    || content.model_revision
                        != crate::minilm_contract::MINILM_VECTOR_SPACE_REVISION
                {
                    content.vectors.clear();
                } else {
                    content.vectors.retain(|key, vector| {
                        carbonpaper_app_bound::protocol::valid_id(key)
                            && crate::minilm_contract::validate_minilm_vector(vector).is_ok()
                    });
                }
            }
            value["content"] = json!(content);
            serde_json::from_value(value)
                .map(TextRecord::Recap)
                .map_err(|e| e.to_string())
        }
        "vector" => {
            let bytes = hex::decode(value["vector_hex"].as_str().ok_or("AGENT_INVALID_RECORD")?)
                .map_err(|_| "AGENT_INVALID_VECTOR")?;
            if bytes.is_empty() || bytes.len() % 4 != 0 || bytes.len() > 4096 * 4 {
                return Err("AGENT_INVALID_VECTOR".into());
            }
            let vector: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect();
            if vector.iter().any(|v| !v.is_finite()) {
                return Err("AGENT_INVALID_VECTOR".into());
            }
            value["vector"] = json!(vector);
            serde_json::from_value(value)
                .map(TextRecord::Vector)
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
            self.agent_writer
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
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
            let repo = AgentRepository::open(&path, &keys.content_key.0, &binding, false)?;
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
            self.agent_writer
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        result
    }
    pub(crate) fn disable_agent_access(&self) -> Result<(), String> {
        self.agent_writer.request(Request::AgentDisable {})?;
        self.agent_writer.cancelled.store(true, Ordering::Release);
        let _sync = self
            .agent_writer
            .sync_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let binding = self.agent_binding();
        let guard = self.get_connection_named("agent_disable")?;
        db(guard
            .as_ref()
            .unwrap()
            .execute_batch("DELETE FROM agent_access_state;DELETE FROM agent_outbox;DELETE FROM agent_projection_gaps;"))?;
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
        self.agent_writer
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.agent_writer.progress_phase("disabled", None);
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
                if storage.agent_access_enabled() {
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
            while checkpoint.phase < SOURCES.len() {
                if self.agent_writer.cancelled.load(Ordering::Acquire) {
                    return Err("AGENT_CANCELLED".into());
                }
                if !self.is_session_valid() {
                    return Err("AUTH_REQUIRED".into());
                }
                let (kind, sql) = SOURCES[checkpoint.phase];
                let rows = self.agent_source_page(sql, &checkpoint.cursor, None)?;
                if rows.is_empty() {
                    checkpoint.phase += 1;
                    checkpoint.cursor.clear();
                } else {
                    for (key, value) in rows {
                        let records = self.hydrate_agent_record(kind, value)?;
                        for record in records {
                            repo.apply(&ExportEvent {
                                dataset_id: binding.dataset_id.clone(),
                                generation: binding.generation.clone(),
                                revision: checkpoint.baseline,
                                change: TextChange::Put { record },
                            })?;
                        }
                        checkpoint.cursor = key;
                        self.agent_writer
                            .progress
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .processed += 1;
                    }
                }
                self.save_agent_checkpoint(&checkpoint)?;
                // Keep source updates moving between snapshot pages.
                self.apply_agent_outbox(&binding, &mut repo)?;
            }
            checkpoint.complete = true;
            self.save_agent_checkpoint(&checkpoint)?;
        }
        self.apply_agent_outbox(&binding, &mut repo)?;
        if self.is_session_valid() {
            for (kind, key, revision) in repo.gaps()? {
                if kind == "privacy" {
                    continue;
                }
                let Some((_, sql)) = SOURCES.iter().find(|(k, _)| *k == kind) else {
                    return Err("AGENT_INVALID_GAP".into());
                };
                let rows = self.agent_source_page(sql, "", Some(&key))?;
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
                        for record in self.hydrate_agent_record(&kind, value)? {
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
            if self.agent_writer.cancelled.load(Ordering::Acquire) {
                return Err("AGENT_CANCELLED".into());
            }
            let change = match operation.as_str() {
                "delete" => TextChange::Delete {
                    kind: kind.clone(),
                    key: key.clone(),
                },
                "delete_day" => TextChange::DeleteDay { date: key.clone() },
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
            let mut last = 0;
            for (id, bytes) in rows {
                if self.agent_writer.cancelled.load(Ordering::Acquire) {
                    return Err("AGENT_CANCELLED".into());
                }
                let event = {
                    let keys = self
                        .agent_writer
                        .keys
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let keys = keys.as_ref().ok_or("AGENT_WRITE_KEY_UNAVAILABLE")?;
                    open_event(&keys.outbox, binding, &bytes)?
                };
                repo.apply(&event)?;
                repo.set_checkpoint(id)?;
                last = id;
            }
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
        sql: &str,
        cursor: &str,
        key: Option<&str>,
    ) -> Result<Vec<(String, Value)>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let guard = self.get_connection_named("agent_export_source")?;
        let conn = guard.as_ref().unwrap();
        let sql=format!("SELECT source_key,payload FROM ({sql}) WHERE (?2 IS NULL AND source_key>?1) OR source_key=?2 ORDER BY source_key LIMIT 128");
        let mut statement = db(conn.prepare(&sql))?;
        let rows = db(statement.query_map(params![cursor, key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }))?;
        rows.map(|r| {
            let (key, payload) = db(r)?;
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
    ) -> Result<Vec<TextRecord>, String> {
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
                        row_key = Some(Zeroizing::new(
                            crate::credential_manager::decrypt_row_key_with_cng(
                                &self.credential_state,
                                key.as_deref().ok_or("AGENT_INVALID_SOURCE")?,
                            )
                            .map_err(|e| e.to_string())?,
                        ));
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
                let plain = Zeroizing::new(self.decrypt_payload_with_row_key(
                    &hex::decode(data).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    &hex::decode(key).map_err(|_| "AGENT_INVALID_SOURCE")?,
                )?);
                let links: Vec<TextLink> =
                    serde_json::from_slice(&plain).map_err(|_| "AGENT_INVALID_SOURCE")?;
                value["visible_links"] = json!(links);
            }
        } else if kind == "ocr" || kind == "recap" {
            let data = value["data_enc"].as_str().filter(|s| !s.is_empty());
            let key = value["key_enc"].as_str().filter(|s| !s.is_empty());
            if let (Some(data), Some(key)) = (data, key) {
                let plain = Zeroizing::new(self.decrypt_payload_with_row_key(
                    &hex::decode(data).map_err(|_| "AGENT_INVALID_SOURCE")?,
                    &hex::decode(key).map_err(|_| "AGENT_INVALID_SOURCE")?,
                )?);
                value[if kind == "ocr" { "text" } else { "content" }] =
                    json!(String::from_utf8(plain.to_vec()).map_err(|_| "AGENT_INVALID_SOURCE")?);
            }
        }
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        Ok(vec![decode_projection(kind, value)?])
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
        self.agent_writer
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
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
