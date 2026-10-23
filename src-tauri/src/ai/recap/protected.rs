//! Capture-only recap generation and cross-period contexts, encrypted with
//! independent, service-owned keys. Archive private-key access is never granted.
use super::{runner, types::*, PrivacyFingerprint};
use crate::{
    ai::config::AiSettings,
    credential_manager::CredentialManagerState,
    processing_stage::recap::{self, CaptureMetadata, RecapConfig, SavedEvidence, SourceVersion},
    sensitive_filter::SensitiveFilterState,
    storage::{OcrResult, StorageState},
};
use carbonpaper_app_bound::protocol::Consumer;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Deserialize)]
struct Payload {
    revision: i64,
    privacy: String,
    sources: Vec<SourceVersion>,
    value: serde_json::Value,
    #[serde(default)]
    correction_version: Option<i64>,
}

#[derive(Clone)]
pub(crate) struct ProtectedRun {
    pub(crate) date: String,
    pub(crate) config: RecapConfig,
    pub(crate) config_hash: String,
    pub(crate) generation: u64,
    pub(crate) correction_version: i64,
    pub(crate) evidence: Vec<SavedEvidence>,
    pub(crate) sources: Vec<SourceVersion>,
    pub(crate) input_ids: HashSet<i64>,
}

impl ProtectedRun {
    pub(crate) fn covers_period(
        &self,
        storage: &StorageState,
        start: i64,
        end: i64,
    ) -> Result<bool, String> {
        let mut cursor = (String::new(), 0);
        loop {
            let page = storage.recap_source_page(start, end, &cursor)?;
            let Some((id, time)) = page.last() else {
                return Ok(true);
            };
            if page.iter().any(|(id, _)| !self.input_ids.contains(id)) {
                return Ok(false);
            }
            cursor = (time.clone(), *id);
        }
    }
    pub(crate) fn check(&self, storage: &StorageState) -> Result<(), String> {
        if self.generation != storage.db_generation()
            || !storage.background_processing_enabled()
            || !storage.processing_stage.supports(Consumer::Recap)
        {
            return Err("AUTH_REQUIRED".into());
        }
        if storage
            .processing_stage
            .recap_content_hash("config")?
            .as_deref()
            != Some(&self.config_hash)
            || recap::ai_hash(&AiSettings::load(storage)?)? != self.config.ai_hash
            || storage.recap_config_revision()? != self.config.revision
            || storage.recap_correction_version(&self.date)? != self.correction_version
        {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        // Reacquire from the authoritative service: a copied user queue, an
        // externally disabled service or retired grant cannot authorize a run.
        if storage
            .processing_stage
            .get_recap::<RecapConfig>(storage, "config")?
            .is_none()
        {
            return Err("AUTH_REQUIRED".into());
        }
        if !storage.recap_sources_current(&self.sources)? {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(())
    }

    pub(crate) fn check_sources_on_connection(
        &self,
        storage: &StorageState,
        conn: &rusqlite::Connection,
    ) -> Result<(), String> {
        if self.generation != storage.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        storage.recap_check_run_versions_on_connection(
            conn,
            &self.date,
            self.config.revision,
            self.correction_version,
        )?;
        for source in &self.sources {
            let current:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM screenshots s LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
                WHERE s.id=?1 AND s.is_deleted=0 AND s.status='committed' AND COALESCE(r.revision,0)=?2)",
                rusqlite::params![source.id,source.revision],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !current {
                return Err("RECAP_SOURCE_CHANGED".into());
            }
        }
        Ok(())
    }

    pub(crate) fn read<T: DeserializeOwned>(
        &self,
        storage: &StorageState,
        kind: &str,
        key: &str,
        revision: Option<i64>,
        privacy: &str,
    ) -> Result<Option<T>, String> {
        let correction_version = if kind == "corrections" {
            Some(storage.recap_correction_version(&self.date)?)
        } else {
            None
        };
        let slot = payload_slot(&self.date, kind, key);
        let Some(saved) = storage
            .processing_stage
            .get_recap::<Payload>(storage, &slot)?
        else {
            if correction_version.is_some_and(|version| version > 0) {
                return Err("RECAP_SOURCE_CHANGED".into());
            }
            return Ok(None);
        };
        if kind == "corrections"
            && (saved.correction_version != correction_version || saved.privacy != privacy)
        {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        if saved.privacy != privacy
            || revision.is_some_and(|r| r != saved.revision)
            || !storage.recap_sources_current(&saved.sources)?
        {
            if kind == "corrections" {
                return Err("RECAP_SOURCE_CHANGED".into());
            }
            return Ok(None);
        }
        serde_json::from_value(saved.value)
            .map(Some)
            .map_err(|_| "RECAP_INVALID_CACHE".into())
    }

    pub(super) fn write<T: Serialize>(
        &self,
        storage: &StorageState,
        kind: &str,
        key: &str,
        revision: i64,
        privacy: &str,
        value: &T,
    ) -> Result<(), String> {
        self.check(storage)?;
        mirror(
            storage,
            &self.date,
            kind,
            key,
            revision,
            privacy,
            &self.sources,
            value,
        )
    }
}

fn payload_slot(date: &str, kind: &str, key: &str) -> String {
    format!("day:{date}:payload:{kind}:{}", digest(key))
}

pub(crate) fn mirror<T: Serialize>(
    storage: &StorageState,
    date: &str,
    kind: &str,
    key: &str,
    revision: i64,
    privacy: &str,
    sources: &[SourceVersion],
    value: &T,
) -> Result<(), String> {
    let correction_version = if kind == "corrections" {
        Some(storage.recap_correction_version(date)?)
    } else {
        None
    };
    mirror_with_correction_version(
        storage,
        date,
        kind,
        key,
        revision,
        privacy,
        sources,
        value,
        correction_version,
    )
}

fn mirror_with_correction_version<T: Serialize>(
    storage: &StorageState,
    date: &str,
    kind: &str,
    key: &str,
    revision: i64,
    privacy: &str,
    sources: &[SourceVersion],
    value: &T,
    correction_version: Option<i64>,
) -> Result<(), String> {
    if !storage.processing_stage.supports(Consumer::Recap) {
        return Ok(());
    }
    let saved = Payload {
        revision,
        privacy: privacy.into(),
        sources: sources.to_vec(),
        value: serde_json::to_value(value).map_err(|_| "RECAP_INVALID_CACHE")?,
        correction_version,
    };
    storage.processing_stage.put_recap(
        storage,
        &payload_slot(date, kind, key),
        &saved,
        &sources.iter().map(|s| s.id).collect::<Vec<_>>(),
    )
}

fn filter_capture(
    filter: &SensitiveFilterState,
    id: i64,
    input: &crate::processing_stage::ProcessingInput,
) -> Result<Option<Evidence>, String> {
    let mode = filter.mode();
    let capture = input.recap.clone().unwrap_or_default();
    let blocks = if capture.blocks.is_empty() {
        vec![OcrResult {
            id: 0,
            screenshot_id: id,
            text: input.ocr_text.clone(),
            confidence: 1.,
            box_coords: vec![],
            created_at: String::new(),
        }]
    } else {
        capture
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| OcrResult {
                id: i as i64,
                screenshot_id: id,
                text: b.text.clone(),
                confidence: b.confidence,
                box_coords: b.box_coords.clone(),
                created_at: String::new(),
            })
            .collect()
    };
    let filtered = (|| {
        Ok::<_, crate::mcp_server::Rejected>(Evidence {
            id,
            timestamp_ms: input.timestamp_ms,
            process_name: crate::mcp_server::filter_identity(&filter, mode, &input.process_name)?,
            window_title: crate::mcp_server::filter_identity(&filter, mode, &input.window_title)?,
            page_url: crate::mcp_server::filter_url(&filter, mode, &capture.page_url)?,
            text: crate::mcp_server::filter_ocr_blocks(&filter, mode, blocks)?
                .into_iter()
                .map(|b| b.text)
                .collect::<Vec<_>>()
                .join("\n"),
            context: capture.document_identity.clone(),
            segment: String::new(),
            novelty: 0.,
            span_ms: 0,
            screening: Default::default(),
        })
    })();
    Ok(filtered.ok())
}

/// Save the recap input durably before acknowledging its independent consumer.
/// Only one screenshot lease is held at a time, even for a very busy period.
pub(super) fn ingest(app: &AppHandle) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>();
    if !storage.background_processing_enabled()
        || !storage.processing_stage.supports(Consumer::Recap)
    {
        return Ok(());
    }
    let _ingest = storage.processing_stage.lock_recap_ingest()?;
    if !storage
        .processing_stage
        .get_recap::<RecapConfig>(&storage, "config")?
        .is_some_and(|c| c.settings.enabled)
    {
        return Ok(());
    }
    for _ in 0..128 {
        let Some(work) = storage.processing_stage.claim(&storage, Consumer::Recap)? else {
            break;
        };
        let receipt = &work.receipt;
        let result = (|| {
            let date = work
                .input
                .recap
                .as_ref()
                .map(|c| c.date.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or(
                    chrono::DateTime::from_timestamp_millis(work.input.timestamp_ms)
                        .ok_or("RECAP_INVALID_DATE")?
                        .with_timezone(&chrono::Local)
                        .date_naive()
                        .to_string(),
                );
            if storage.recap_is_deleted(&date)? {
                storage.recap_record_input_receipt(receipt)?;
                return storage.processing_stage.finish(&storage, receipt);
            }
            let mut capture = work.input.recap.clone().unwrap_or_default();
            if let Some(metadata) = storage.processing_stage.get_recap::<CaptureMetadata>(
                &storage,
                &format!("metadata:{}", receipt.screenshot_id),
            )? {
                capture.page_url = metadata.page_url;
                capture.document_identity = metadata.document_identity;
            }
            let saved = SavedEvidence {
                evidence: Evidence {
                    id: receipt.screenshot_id,
                    timestamp_ms: work.input.timestamp_ms,
                    process_name: work.input.process_name.clone(),
                    window_title: work.input.window_title.clone(),
                    page_url: capture.page_url.clone(),
                    text: work.input.ocr_text.clone(),
                    context: capture.document_identity.clone(),
                    segment: String::new(),
                    novelty: 0.,
                    span_ms: 0,
                    screening: Default::default(),
                },
                source: SourceVersion {
                    id: receipt.screenshot_id,
                    revision: receipt.source_revision,
                },
                capture,
            };
            storage.processing_stage.put_recap(
                &storage,
                &format!("day:{date}:evidence:{}", receipt.screenshot_id),
                &saved,
                &[receipt.screenshot_id],
            )?;
            storage.recap_record_input_receipt(receipt)?;
            storage.processing_stage.finish(&storage, receipt)
        })();
        if let Err(error) = result {
            // A service acknowledgement can be recovered from the receipt. A
            // temporary failure must never abandon another consumer's input.
            let _ = storage.processing_stage.release(receipt, false);
            return Err(error);
        }
    }
    Ok(())
}

pub(super) fn prepare(app: &AppHandle, date: &str) -> Result<Option<ProtectedRun>, String> {
    let storage = app.state::<Arc<StorageState>>();
    let filter = app.state::<Arc<SensitiveFilterState>>();
    prepare_with_filter(&storage, date, &filter)
}

pub(crate) fn prepare_with_filter(
    storage: &StorageState,
    date: &str,
    filter: &SensitiveFilterState,
) -> Result<Option<ProtectedRun>, String> {
    if !storage.background_processing_enabled()
        || !storage.processing_stage.supports(Consumer::Recap)
    {
        return Ok(None);
    }
    storage.processing_stage.reconcile_recap()?;
    let Some(config) = storage
        .processing_stage
        .get_recap::<RecapConfig>(&storage, "config")?
    else {
        return Ok(None);
    };
    if !config.settings.enabled
        || recap::ai_hash(&AiSettings::load(&storage)?)? != config.ai_hash
        || storage.recap_config_revision()? != config.revision
    {
        return Ok(None);
    }
    let config_hash = storage
        .processing_stage
        .recap_content_hash("config")?
        .ok_or("AUTH_REQUIRED")?;
    let correction_version = storage.recap_correction_version(date)?;
    let mut evidence = vec![];
    let mut input_ids = HashSet::new();
    let mut input_sources = vec![];
    for slot in storage
        .processing_stage
        .recap_slots(&format!("day:{date}:evidence:"))?
    {
        if let Some(mut saved) = storage
            .processing_stage
            .get_recap::<SavedEvidence>(&storage, &slot)?
        {
            if storage.recap_sources_current(std::slice::from_ref(&saved.source))? {
                input_ids.insert(saved.source.id);
                input_sources.push(saved.source.clone());
                if let Some(metadata) = storage.processing_stage.get_recap::<CaptureMetadata>(
                    &storage,
                    &format!("metadata:{}", saved.source.id),
                )? {
                    saved.capture.page_url = metadata.page_url;
                    saved.capture.document_identity = metadata.document_identity;
                }
                let input = crate::processing_stage::ProcessingInput {
                    image_hash: String::new(),
                    window_title: saved.evidence.window_title.clone(),
                    process_name: saved.evidence.process_name.clone(),
                    timestamp_ms: saved.evidence.timestamp_ms,
                    ocr_text: saved.evidence.text.clone(),
                    source_revision: saved.source.revision,
                    clip: None,
                    recap: Some(saved.capture.clone()),
                };
                if let Some(filtered) = filter_capture(filter, saved.evidence.id, &input)? {
                    saved.evidence = filtered;
                    evidence.push(saved);
                }
            }
        }
    }
    evidence.sort_by_key(|s| (s.evidence.timestamp_ms, s.evidence.id));
    let mut sources = input_sources
        .into_iter()
        .map(|s| (s.id, s))
        .collect::<BTreeMap<_, _>>();
    let privacy = PrivacyFingerprint::new(filter.get_config())?;
    // Include authorized previous batches/corrections even after restart. Their
    // immutable dependency manifests extend the run's source checks.
    for slot in storage
        .processing_stage
        .recap_slots(&format!("day:{date}:payload:"))?
    {
        if let Some(saved) = storage
            .processing_stage
            .get_recap::<Payload>(&storage, &slot)?
        {
            if saved.privacy == privacy.as_str() && storage.recap_sources_current(&saved.sources)? {
                sources.extend(saved.sources.into_iter().map(|s| (s.id, s)));
            }
        }
    }
    Ok(Some(ProtectedRun {
        date: date.into(),
        config,
        config_hash,
        generation: storage.db_generation(),
        correction_version,
        evidence,
        sources: sources.into_values().collect(),
        input_ids,
    }))
}

pub(super) fn dates(storage: &StorageState) -> Result<Vec<String>, String> {
    let mut dates = HashSet::new();
    if storage.processing_stage.supports(Consumer::Recap) {
        for slot in storage.processing_stage.recap_slots("day:")? {
            if let Some(date) = slot.split(':').nth(1) {
                dates.insert(date.to_owned());
            }
        }
    }
    let mut dates = dates.into_iter().collect::<Vec<_>>();
    dates.sort();
    Ok(dates)
}

/// Seed already generated periods/corrections on an authorized unlock, so later
/// protected periods reuse exactly the user's existing task identities.
pub(crate) fn sync_after_unlock(app: &AppHandle) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>();
    let credentials = app.state::<Arc<CredentialManagerState>>();
    recap::sync_config(&storage, &credentials)?;
    if !storage.processing_stage.supports(Consumer::Recap) {
        return Ok(());
    }
    let settings = storage
        .recap_read_background::<RecapSettings>("settings", "settings", None)?
        .unwrap_or_default();
    if !settings.enabled {
        return Ok(());
    }
    let today = chrono::Local::now().date_naive();
    for date in [
        today.pred_opt().unwrap_or(today).to_string(),
        today.to_string(),
    ] {
        sync_day(app, &date)?;
    }
    Ok(())
}

pub(crate) fn sync_day(app: &AppHandle, date: &str) -> Result<(), String> {
    let storage = app.state::<Arc<StorageState>>();
    if !storage.processing_stage.supports(Consumer::Recap)
        || !storage.is_silent_read_authorized()
        || storage.recap_is_deleted(date)?
    {
        return Ok(());
    }
    let correction_version = storage.recap_correction_version(date)?;
    let day = runner::read_day_background(app, date)?;
    let privacy = runner::privacy_hash(app)?;
    let (start, end) = day_bounds(date)?;
    let (revision, _, _) =
        storage.recap_day_revision(date, start, end, &runner::privacy_fingerprint(app)?)?;
    let mut sources = day
        .batches
        .iter()
        .flat_map(|b| {
            b.records
                .iter()
                .map(|s| s.id)
                .chain(b.activities.iter().flat_map(|a| {
                    a.sources
                        .iter()
                        .map(|s| s.id)
                        .chain(a.member_ids.iter().copied())
                }))
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|id| {
            storage
                .staged_source_revision(id)
                .map(|revision| SourceVersion { id, revision })
        })
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_by_key(|source| source.id);
    for batch in &day.batches {
        let key = format!("{date}:{}", batch.start_ms);
        if batch.status != "pending" {
            let raw = storage.recap_read_background::<RecapBatch>("batch", &key, Some(revision))?;
            if let Some(raw) = raw {
                mirror(
                    &storage, date, "batch", &key, revision, &privacy, &sources, &raw,
                )?;
            }
        }
        if let Some(summary) = storage.recap_read_background::<super::summary::CachedSummary>(
            "summary",
            &key,
            Some(revision),
        )? {
            mirror(
                &storage, date, "summary", &key, revision, &privacy, &sources, &summary,
            )?;
        }
    }
    let corrections = storage
        .recap_read_background::<CorrectionHistory>("corrections", date, None)?
        .unwrap_or_default();
    if correction_version != storage.recap_correction_version(date)? {
        return Err("RECAP_SOURCE_CHANGED".into());
    }
    mirror_with_correction_version(
        &storage,
        date,
        "corrections",
        date,
        -1,
        &privacy,
        &sources,
        &corrections,
        Some(correction_version),
    )
}
