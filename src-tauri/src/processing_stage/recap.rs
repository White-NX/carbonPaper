//! Recap-only encrypted objects. The service owns keys, replacements and source
//! revocations; restoring the user-writable SQLite file cannot restore a grant.
use super::*;
use crate::ai::{
    config::{AiSettings, ResolvedProvider},
    recap::{Evidence, RecapSettings},
};
use crate::storage::OcrResultInput;
use serde::de::DeserializeOwned;

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct CaptureBlock {
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
}

#[derive(Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub(crate) struct CaptureEvidence {
    #[serde(default)]
    pub date: String,
    pub page_url: String,
    pub document_identity: String,
    pub blocks: Vec<CaptureBlock>,
}

impl CaptureEvidence {
    pub(crate) fn from_blocks(blocks: &[OcrResultInput]) -> Self {
        Self {
            blocks: blocks
                .iter()
                .map(|b| CaptureBlock {
                    text: b.text.clone(),
                    confidence: b.confidence,
                    box_coords: b.box_coords.clone(),
                })
                .collect(),
            date: String::new(),
            page_url: String::new(),
            document_identity: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SourceVersion {
    pub id: i64,
    pub revision: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedEvidence {
    pub evidence: Evidence,
    pub source: SourceVersion,
    #[serde(default)]
    pub capture: CaptureEvidence,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RecapConfig {
    pub settings: RecapSettings,
    pub provider: ResolvedProvider,
    pub remote_consent: bool,
    pub ai_hash: String,
    pub revision: i64,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct CaptureMetadata {
    pub page_url: String,
    pub document_identity: String,
}

pub(super) fn initialize(conn: &Connection, dataset: &str) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recap_objects (
        object_id TEXT PRIMARY KEY,dataset_id TEXT NOT NULL,slot TEXT NOT NULL,
        binding TEXT NOT NULL,payload BLOB NOT NULL,digest TEXT NOT NULL,
        content_hash TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS recap_objects_slot ON recap_objects(slot,state,created);",
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM recap_objects WHERE dataset_id!=?1", [dataset])
        .map_err(|e| e.to_string())?;
    Ok(())
}

impl ProcessingStaging {
    pub(crate) fn lock_recap_ingest(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.recap_ingest_lock
            .lock()
            .map_err(|_| "staging lock poisoned".into())
    }
    /// Called while new browser/Office metadata is still plaintext. It never
    /// decrypts the historical archive, including after a locked restart.
    pub(crate) fn put_recap_metadata(
        &self,
        storage: &StorageState,
        id: i64,
        expected_generation: u64,
        page_url: Option<&str>,
        document_identity: Option<&str>,
    ) -> Result<(), String> {
        if expected_generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        if !self.supports(Consumer::Recap) {
            return Ok(());
        }
        let Some(config) = self.get_recap::<RecapConfig>(storage, "config")? else {
            return Ok(());
        };
        if !config.settings.enabled {
            return Ok(());
        }
        let slot = format!("metadata:{id}");
        let mut metadata = self
            .get_recap::<CaptureMetadata>(storage, &slot)?
            .unwrap_or_default();
        if let Some(url) = page_url {
            metadata.page_url = url.into();
        }
        if let Some(identity) = document_identity {
            metadata.document_identity = identity.into();
        }
        self.put_recap_for_generation(storage, &slot, &metadata, &[id], expected_generation)
    }
    pub(crate) fn put_recap<T: Serialize>(
        &self,
        storage: &StorageState,
        slot: &str,
        value: &T,
        sources: &[i64],
    ) -> Result<(), String> {
        self.put_recap_for_generation(storage, slot, value, sources, storage.db_generation())
    }

    fn put_recap_for_generation<T: Serialize>(
        &self,
        storage: &StorageState,
        slot: &str,
        value: &T,
        sources: &[i64],
        expected_generation: u64,
    ) -> Result<(), String> {
        let _serial = self
            .capture_lock
            .lock()
            .map_err(|_| "staging lock poisoned")?;
        if expected_generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        if !self.supports(Consumer::Recap) {
            return Err("APP_BOUND_RECAP_UNAVAILABLE".into());
        }
        storage.finish_staged_deletions()?;
        let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value).map_err(|e| e.to_string())?);
        let hash = crypto::digest(&bytes);
        if self.recap_content_hash(slot)?.as_deref() == Some(&hash)
            && self
                .get_recap::<serde_json::Value>(storage, slot)?
                .is_some()
        {
            return Ok(());
        }
        // A later value must supersede any interrupted earlier prepare.
        self.with_store(|s| {
            s.connection
                .execute(
                    "DELETE FROM recap_objects WHERE slot=?1 AND state='preparing'",
                    [slot],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
        })?;
        let dataset = self.with_store(|s| Ok(s.dataset_id.clone()))?;
        let Response::RecapKey(prepared) = self
            .call_broker_logged(
                Request::PrepareRecapObject {
                    dataset_id: dataset.clone(),
                    slot: slot.into(),
                    payload_bytes: bytes.len() as u64 + 28,
                },
                Some(Consumer::Recap),
            )
            .map_err(|e| e.to_string())?
        else {
            return Err("app_bound_integrity_failure".into());
        };
        let binding = &prepared.binding;
        if binding.dataset_id != dataset
            || binding.slot != slot
            || binding.payload_bytes != bytes.len() as u64 + 28
        {
            return Err("app_bound_integrity_failure".into());
        }
        for chunk in sources.chunks(128) {
            self.call_broker_logged(
                Request::AddRecapSources {
                    object_id: binding.object_id.clone(),
                    screenshot_ids: chunk.to_vec(),
                },
                Some(Consumer::Recap),
            )
            .map_err(|e| e.to_string())?;
        }
        let encrypted =
            crypto::encrypt_recap(&prepared.key, binding, &bytes).map_err(|e| e.to_string())?;
        if !self.make_recap_room(bytes.len() as u64 + 28)? {
            return Err(BrokerError::LimitExceeded.to_string());
        }
        let digest = crypto::digest(&encrypted);
        self.with_store(|s| {
            if s.dataset_id != dataset
                || s.generation != expected_generation
                || expected_generation != storage.db_generation()
            {
                return Err("RECAP_SOURCE_CHANGED".into());
            }
            s.connection
                .execute(
                    "INSERT INTO recap_objects VALUES(?1,?2,?3,?4,?5,?6,?7,'preparing',?8)",
                    params![
                        binding.object_id,
                        dataset,
                        slot,
                        serde_json::to_string(binding).map_err(|e| e.to_string())?,
                        encrypted,
                        digest,
                        hash,
                        protocol::now_secs()
                    ],
                )
                .map_err(|e| e.to_string())?;
            Ok(())
        })?;
        self.activate_recap(&binding.object_id, &digest, slot)
    }

    pub(super) fn make_recap_room(&self, incoming: u64) -> Result<bool, String> {
        let capacity = self.status().limits.capacity_bytes;
        loop {
            let (used,oldest)=self.with_store(|s| {
                let used:i64=s.connection.query_row("SELECT COALESCE(SUM(length(payload)),0) FROM (
                    SELECT payload FROM staged_inputs UNION ALL SELECT payload FROM recap_objects)",[],|r|r.get(0)).map_err(|e|e.to_string())?;
                let oldest=s.connection.query_row("SELECT object_id FROM recap_objects WHERE slot!='config' ORDER BY created,object_id LIMIT 1",[],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?;
                Ok((used as u64,oldest))
            })?;
            if used.saturating_add(incoming) <= capacity {
                return Ok(true);
            }
            let Some(id) = oldest else {
                return Ok(false);
            };
            self.call_broker_logged(
                Request::RevokeRecapObjects {
                    object_ids: vec![id.clone()],
                },
                Some(Consumer::Recap),
            )
            .map_err(|e| e.to_string())?;
            self.with_store(|s| {
                s.connection
                    .execute("DELETE FROM recap_objects WHERE object_id=?1", [id])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?;
        }
    }

    fn activate_recap(&self, id: &str, digest: &str, slot: &str) -> Result<(), String> {
        self.call_broker_logged(
            Request::ActivateRecapObject {
                object_id: id.into(),
                ciphertext_digest: digest.into(),
            },
            Some(Consumer::Recap),
        )
        .map_err(|e| e.to_string())?;
        self.with_store(|s| {
            let tx = s
                .connection
                .unchecked_transaction()
                .map_err(|e| e.to_string())?;
            tx.execute(
                "UPDATE recap_objects SET state='active' WHERE object_id=?1",
                [id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM recap_objects WHERE slot=?1 AND object_id!=?2",
                params![slot, id],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
    }

    pub(crate) fn recap_content_hash(&self, slot: &str) -> Result<Option<String>, String> {
        self.with_store(|s|s.connection.query_row("SELECT content_hash FROM recap_objects WHERE slot=?1 AND state='active' ORDER BY created DESC LIMIT 1",[slot],|r|r.get(0)).optional().map_err(|e|e.to_string()))
    }

    pub(crate) fn get_recap<T: DeserializeOwned>(
        &self,
        storage: &StorageState,
        slot: &str,
    ) -> Result<Option<T>, String> {
        if !self.supports(Consumer::Recap) {
            return Ok(None);
        }
        storage.finish_staged_deletions()?;
        let row=self.with_store(|s| {
            if s.generation!=storage.db_generation() { return Err("RECAP_SOURCE_CHANGED".into()); }
            s.connection.query_row("SELECT object_id,binding,payload,digest FROM recap_objects WHERE slot=?1 AND state='active' ORDER BY created DESC LIMIT 1",[slot],
                |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,String>(3)?))).optional().map_err(|e|e.to_string())
        })?;
        let Some((id, binding, encrypted, digest)) = row else {
            return Ok(None);
        };
        let key = match self.call_broker_logged(
            Request::AcquireRecapObject {
                object_id: id.clone(),
                ciphertext_digest: digest.clone(),
            },
            Some(Consumer::Recap),
        ) {
            Ok(Response::RecapKey(key)) => key,
            Err(BrokerError::Retired) => {
                self.with_store(|s| {
                    s.connection
                        .execute("DELETE FROM recap_objects WHERE object_id=?1", [&id])
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })?;
                return Ok(None);
            }
            Err(e) => return Err(e.to_string()),
            _ => return Err("app_bound_integrity_failure".into()),
        };
        let local: protocol::RecapBinding =
            serde_json::from_str(&binding).map_err(|_| "RECAP_INVALID_CACHE")?;
        if local != key.binding || local.slot != slot || crypto::digest(&encrypted) != digest {
            return Err("app_bound_integrity_failure".into());
        }
        let bytes =
            crypto::decrypt_recap(&key.key, &local, &encrypted).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "RECAP_INVALID_CACHE".into())
    }

    pub(crate) fn recap_slots(&self, prefix: &str) -> Result<Vec<String>, String> {
        self.with_store(|s| {
            let mut stmt=s.connection.prepare("SELECT slot FROM recap_objects WHERE state='active' AND slot LIKE ?1 ORDER BY slot").map_err(|e|e.to_string())?;
            let rows=stmt.query_map([format!("{prefix}%")],|r|r.get(0)).map_err(|e|e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
        })
    }

    pub(crate) fn revoke_recap(&self) -> Result<(), String> {
        if self.supports(Consumer::Recap) {
            self.call_broker_logged(Request::RevokeRecap {}, Some(Consumer::Recap))
                .map_err(|e| e.to_string())?;
        }
        if self.initialized() {
            self.with_store(|s| {
                s.connection
                    .execute("DELETE FROM recap_objects", [])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?;
        }
        Ok(())
    }

    pub(crate) fn revoke_recap_day(&self, date: &str) -> Result<(), String> {
        if self.supports(Consumer::Recap) {
            self.call_broker_logged(
                Request::RevokeRecapDay { date: date.into() },
                Some(Consumer::Recap),
            )
            .map_err(|e| e.to_string())?;
        }
        if self.initialized() {
            self.with_store(|s| {
                s.connection
                    .execute(
                        "DELETE FROM recap_objects WHERE slot LIKE ?1",
                        [format!("day:{date}:%")],
                    )
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?;
        }
        Ok(())
    }

    pub(crate) fn reconcile_recap(&self) -> Result<(), String> {
        let _serial = self
            .capture_lock
            .lock()
            .map_err(|_| "staging lock poisoned")?;
        let rows=self.with_store(|s| {
            let mut stmt=s.connection.prepare("SELECT object_id,digest,slot FROM recap_objects WHERE state='preparing' ORDER BY created LIMIT 32").map_err(|e|e.to_string())?;
            let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).map_err(|e|e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())
        })?;
        for (id, digest, slot) in rows {
            match self.activate_recap(&id, &digest, &slot) {
                Ok(()) => {}
                Err(e) if e == BrokerError::Retired.to_string() => {
                    self.with_store(|s| {
                        s.connection
                            .execute("DELETE FROM recap_objects WHERE object_id=?1", [id])
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Bound local ciphertext too. Service retirement is checked before any
    /// decrypt; local cleanup simply removes ciphertext that has lost its key.
    pub(crate) fn reap_recap_objects(&self, storage: &StorageState) -> Result<(), String> {
        if !self.supports(Consumer::Recap) {
            return Ok(());
        }
        let rows = self.with_store(|s| {
            let mut stmt = s
                .connection
                .prepare("SELECT object_id,digest FROM recap_objects WHERE state='active'")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())
        })?;
        storage.finish_staged_deletions()?;
        for (id, digest) in rows {
            match self.call_broker_logged(
                Request::AcquireRecapObject {
                    object_id: id.clone(),
                    ciphertext_digest: digest,
                },
                Some(Consumer::Recap),
            ) {
                Ok(Response::RecapKey(_)) => {}
                Err(BrokerError::Retired) => {
                    self.with_store(|s| {
                        s.connection
                            .execute("DELETE FROM recap_objects WHERE object_id=?1", [id])
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })?;
                }
                Err(e) => return Err(e.to_string()),
                _ => return Err("app_bound_integrity_failure".into()),
            }
        }
        Ok(())
    }
}

pub(crate) fn ai_hash(ai: &AiSettings) -> Result<String, String> {
    serde_json::to_vec(ai)
        .map(|bytes| crypto::digest(&bytes))
        .map_err(|e| e.to_string())
}

/// Called only after an authorized unlock/settings change, or with the same
/// process's existing background lease. No service ever receives archive keys.
pub(crate) fn sync_config(
    storage: &StorageState,
    credentials: &crate::credential_manager::CredentialManagerState,
) -> Result<(), String> {
    if !storage.processing_stage.supports(Consumer::Recap) {
        return Ok(());
    }
    let revision = storage.recap_config_revision()?;
    let settings = storage
        .recap_read_background::<RecapSettings>("settings", "settings", None)?
        .unwrap_or_default()
        .current_defaults()
        .with_app_language();
    if !settings.enabled || !storage.background_processing_enabled() {
        return storage.processing_stage.revoke_recap();
    }
    let ai = AiSettings::load(storage)?;
    let id = settings
        .provider_id
        .as_deref()
        .or_else(|| ai.effective_default_id())
        .ok_or("AI_NO_PROVIDER")?;
    let config = RecapConfig {
        provider: ai.resolve(credentials, id)?,
        settings,
        remote_consent: ai.remote_consent,
        ai_hash: ai_hash(&ai)?,
        revision,
    };
    if revision != storage.recap_config_revision()? {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    storage
        .processing_stage
        .put_recap(storage, "config", &config, &[])
}
