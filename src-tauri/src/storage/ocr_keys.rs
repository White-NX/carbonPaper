//! One fresh OCR-only key per capture batch, with authenticated row identities.
//! Read references are assembled from SQL metadata, never persisted in the
//! legacy RSA envelope column. They contain wrapped keys, not decrypted keys.
use super::{BackgroundReadError, StorageState};
use crate::credential_manager::{
    decrypt_row_key_with_cng_silent, decrypt_with_master_key, CredentialError,
    CredentialManagerState,
};
use rand::RngCore;
use rusqlite::{functions::FunctionFlags, params, Connection};
use std::{collections::HashMap, sync::Mutex, time::Instant};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"CPORKEY1";
const DATASET: &str = "protected_content_dataset_id";
const MAX_CACHED_KEYS: usize = 64;

pub(super) mod backup;
mod wire;
use wire::{aad, open, scope, scope_tag, seal};
pub(super) use wire::{block_aad, verify_scope};

fn invalid() -> CredentialError {
    CredentialError::CryptoError("Invalid OCR key or payload binding".into())
}

impl From<wire::InvalidBinding> for CredentialError {
    fn from(_: wire::InvalidBinding) -> Self {
        invalid()
    }
}

pub(super) struct WriteKey {
    pub(super) id: i64,
    key: Zeroizing<Vec<u8>>,
    wrapped: Vec<u8>,
}

impl WriteKey {
    pub(super) fn new(wrap: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>) -> Result<Self, String> {
        let mut key = Zeroizing::new(vec![0; 32]);
        rand::thread_rng().fill_bytes(&mut key);
        // A collision aborts the transaction; a retry generates a fresh ID/key.
        // No persisted, caller-modifiable wrapped key is selected for writing.
        let id = (rand::random::<u64>() >> 1).max(1) as i64;
        let wrapped = wrap(&key)?;
        Ok(Self { id, key, wrapped })
    }

    pub(super) fn insert(
        &self,
        conn: &Connection,
        dataset: &str,
        screenshot: i64,
    ) -> Result<(), String> {
        let tag = scope_tag(&self.key, dataset, screenshot, self.id).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO ocr_keys(id,screenshot_id,wrapped_key,scope_tag) VALUES(?1,?2,?3,?4)",
            params![self.id, screenshot, self.wrapped, tag],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn encrypt(
        &self,
        dataset: &str,
        screenshot: i64,
        row: i64,
        text: &[u8],
    ) -> Result<Vec<u8>, String> {
        seal(
            &self.key,
            &aad(dataset, screenshot, self.id, row).map_err(|e| e.to_string())?,
            text,
        )
        .map_err(|e| e.to_string())
    }

    pub(super) fn encrypt_block(
        &self,
        dataset: &str,
        screenshot: i64,
        plain: &[u8],
    ) -> Result<Vec<u8>, String> {
        seal(
            &self.key,
            &block_aad(dataset, screenshot, self.id).map_err(|e| e.to_string())?,
            plain,
        )
        .map_err(|e| e.to_string())
    }
}

pub(super) fn dataset(conn: &Connection) -> Result<String, String> {
    conn.query_row(
        "SELECT value FROM app_metadata WHERE key=?1",
        [DATASET],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Keep locators, key descriptors and separately fetched block ciphertext in
/// one SQLite snapshot. Callers finish this read before entering CNG.
pub(super) fn read_snapshot(
    conn: &Connection,
) -> Result<Option<rusqlite::Transaction<'_>>, String> {
    if conn.is_autocommit() {
        conn.unchecked_transaction()
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        Ok(None)
    }
}

impl StorageState {
    pub(super) fn ocr_decoder(&self) -> Decoder<'_> {
        Decoder::new(&self.credential_state)
    }
    pub(super) fn new_ocr_write_key(&self) -> Result<WriteKey, String> {
        WriteKey::new(|key| {
            let wrapped = self.wrap_row_key_for_storage(key)?;
            self.agent_writer.stage_row_key(&wrapped, key);
            Ok(wrapped)
        })
    }
}

pub(super) fn next_row_id(conn: &Connection) -> Result<i64, String> {
    let last: i64 = conn
        .query_row(
            "SELECT max(COALESCE((SELECT seq FROM sqlite_sequence WHERE name='ocr_results'),0),
                    COALESCE((SELECT max(id) FROM ocr_results),0))",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    last.checked_add(1)
        .ok_or_else(|| "OCR row ID capacity exhausted".into())
}

// This innocuous function only serializes public bootstrap metadata. Content
// decryption stays in Rust behind the authenticated credential boundary.
pub(super) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "cp_ocr_key_ref",
        9,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            let group: Option<i64> = ctx.get(2)?;
            let slot: Option<i64> = ctx.get(8)?;
            let mut result = MAGIC.to_vec();
            let Some(group) = group else {
                if slot.is_some() {
                    return Err(rusqlite::Error::UserFunctionError(Box::new(invalid())));
                }
                let Some(wrapped) = ctx.get::<Option<Vec<u8>>>(3)? else {
                    return Ok(None);
                };
                result.push(0);
                result.extend(wrapped);
                return Ok(Some(result));
            };
            let failed = || rusqlite::Error::UserFunctionError(Box::new(invalid()));
            let row: i64 = ctx.get(0)?;
            let screenshot: i64 = ctx.get(1)?;
            let owner = ctx.get::<Option<i64>>(4)?.ok_or_else(failed)?;
            let tag = ctx.get::<Option<Vec<u8>>>(5)?.ok_or_else(failed)?;
            let wrapped = ctx.get::<Option<Vec<u8>>>(6)?.ok_or_else(failed)?;
            let dataset = ctx.get::<Option<String>>(7)?.ok_or_else(failed)?;
            if tag.len() != 32 || dataset.len() > 128 || wrapped.len() > 1024 {
                return Err(failed());
            }
            if slot
                .is_some_and(|slot| !(0..super::ocr_blocks::codec::MAX_ROWS as i64).contains(&slot))
            {
                return Err(failed());
            }
            result.push(if slot.is_some() { 2 } else { 1 });
            for id in [row, screenshot, group, owner] {
                result.extend_from_slice(&id.to_le_bytes());
            }
            if let Some(slot) = slot {
                result.extend_from_slice(&slot.to_le_bytes());
            }
            result.push(dataset.len() as u8);
            result.extend_from_slice(dataset.as_bytes());
            result.extend_from_slice(&tag);
            result.extend_from_slice(&wrapped);
            Ok(Some(result))
        },
    )
}

enum Reference<'a> {
    Legacy(&'a [u8]),
    Group {
        row: i64,
        screenshot: i64,
        group: i64,
        dataset: &'a str,
        tag: &'a [u8],
        wrapped: &'a [u8],
        slot: Option<usize>,
    },
}

fn parse(bytes: &[u8]) -> Result<Reference<'_>, CredentialError> {
    // Internal legacy index-repair callers can still supply a raw RSA envelope.
    // SQL readers always wrap legacy values once, so a forged descriptor stored
    // in text_key_encrypted is sent to RSA, never recursively interpreted.
    if !bytes.starts_with(MAGIC) {
        return Ok(Reference::Legacy(bytes));
    }
    match bytes.get(8) {
        Some(0) => Ok(Reference::Legacy(&bytes[9..])),
        Some(kind @ (1 | 2)) if bytes.len() >= if *kind == 1 { 74 } else { 82 } => {
            let read = |start| i64::from_le_bytes(bytes[start..start + 8].try_into().unwrap());
            let row = read(9);
            let screenshot = read(17);
            let group = read(25);
            let owner = read(33);
            let start = if *kind == 1 { 42 } else { 50 };
            let slot = if *kind == 2 {
                let slot = usize::try_from(read(41)).map_err(|_| invalid())?;
                if slot >= super::ocr_blocks::codec::MAX_ROWS {
                    return Err(invalid());
                }
                Some(slot)
            } else {
                None
            };
            let length = bytes[start - 1] as usize;
            if owner != screenshot || bytes.len() < start + length + 32 || length > 128 {
                return Err(invalid());
            }
            let dataset =
                std::str::from_utf8(&bytes[start..start + length]).map_err(|_| invalid())?;
            scope(dataset, screenshot, group)?;
            Ok(Reference::Group {
                row,
                screenshot,
                group,
                dataset,
                tag: &bytes[start + length..start + length + 32],
                wrapped: &bytes[start + length + 32..],
                slot,
            })
        }
        _ => Err(invalid()),
    }
}

fn decode(
    data: &[u8],
    reference: &[u8],
    unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
) -> Result<Vec<u8>, CredentialError> {
    match parse(reference)? {
        Reference::Legacy(wrapped) => {
            let key = Zeroizing::new(unwrap(wrapped)?);
            decrypt_with_master_key(&key, data)
        }
        Reference::Group {
            row,
            screenshot,
            group,
            dataset,
            tag,
            wrapped,
            slot,
        } => {
            if slot.is_some() {
                return Err(invalid());
            }
            let key = Zeroizing::new(unwrap(wrapped)?);
            verify_scope(&key, dataset, screenshot, group, tag)?;
            Ok(open(&key, &aad(dataset, screenshot, group, row)?, data)?)
        }
    }
}

/// A bounded request-local cache. Every hit rechecks the admitted credential
/// session; dropped/revoked readers cannot keep using a previously unwrapped key.
pub(super) struct Decoder<'a> {
    credential: &'a CredentialManagerState,
    admitted: Option<Instant>,
    keys: Mutex<HashMap<Vec<u8>, Zeroizing<Vec<u8>>>>,
    encrypted_blocks: Mutex<HashMap<i64, Vec<u8>>>,
    blocks: Mutex<HashMap<i64, super::ocr_blocks::Decoded>>,
}

#[derive(Default)]
pub(super) struct DecodedRecord {
    pub text: String,
    pub text_layout: Option<crate::ml_protocol::MlOcrTextLayout>,
    pub metadata: Option<super::ocr_blocks::Metadata>,
}

impl<'a> Decoder<'a> {
    pub(super) fn new(credential: &'a CredentialManagerState) -> Self {
        Self {
            credential,
            admitted: credential.session_marker(),
            keys: Mutex::new(HashMap::new()),
            encrypted_blocks: Mutex::new(HashMap::new()),
            blocks: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self) -> Result<(), CredentialError> {
        if !self.credential.protected_read_authorized()
            || self.credential.session_marker() != self.admitted
        {
            self.keys.lock().unwrap_or_else(|e| e.into_inner()).clear();
            self.blocks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            Err(CredentialError::AuthRequired)
        } else {
            Ok(())
        }
    }

    /// Fetch each ciphertext block once while the caller's read snapshot is
    /// open. SQL rows carry only empty payload placeholders and bound references.
    pub(super) fn load_blocks<'b>(
        &self,
        conn: &Connection,
        references: impl IntoIterator<Item = &'b [u8]>,
    ) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        for reference in references {
            if let Reference::Group {
                group,
                slot: Some(_),
                ..
            } = parse(reference).map_err(|e| e.to_string())?
            {
                ids.insert(group);
            }
        }
        let mut encrypted = self
            .encrypted_blocks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| !encrypted.contains_key(id))
            .collect();
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT id,content_enc FROM ocr_blocks WHERE id IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk), |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (id, data) = row.map_err(|e| e.to_string())?;
                if data.len() > super::ocr_blocks::codec::MAX_BYTES + 33 {
                    return Err("Oversized OCR block ciphertext".into());
                }
                encrypted.insert(id, data);
            }
        }
        Ok(())
    }

    fn cached_key(
        &self,
        wrapped: &[u8],
        unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
    ) -> Result<Vec<u8>, CredentialError> {
        self.check()?;
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = keys.get(wrapped) {
            return Ok(key.to_vec());
        }
        let key = Zeroizing::new(unwrap(wrapped)?);
        if self.credential.session_marker() != self.admitted
            || !self.credential.protected_read_authorized()
        {
            keys.clear();
            return Err(CredentialError::AuthRequired);
        }
        if keys.len() >= MAX_CACHED_KEYS {
            keys.clear();
        }
        let output = key.to_vec();
        keys.insert(wrapped.to_vec(), key);
        Ok(output)
    }

    fn read_block<T>(
        &self,
        reference: &[u8],
        unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
        read: impl FnOnce(&super::ocr_blocks::Decoded, usize, i64) -> Result<T, String>,
    ) -> Result<T, BackgroundReadError> {
        let operation = || {
            self.check()?;
            let Reference::Group {
                row,
                screenshot,
                group,
                dataset,
                tag,
                wrapped,
                slot: Some(slot),
            } = parse(reference)?
            else {
                return Err(invalid());
            };
            let key = Zeroizing::new(self.cached_key(wrapped, unwrap)?);
            verify_scope(&key, dataset, screenshot, group, tag)?;
            let result = {
                let mut blocks = self.blocks.lock().unwrap_or_else(|e| e.into_inner());
                if !blocks.contains_key(&group) {
                    let encrypted = self
                        .encrypted_blocks
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let data = encrypted.get(&group).ok_or_else(invalid)?;
                    let plain =
                        Zeroizing::new(open(&key, &block_aad(dataset, screenshot, group)?, data)?);
                    let decoded =
                        super::ocr_blocks::Decoded::open(&plain).map_err(|_| invalid())?;
                    if blocks.len() >= MAX_CACHED_KEYS
                        || blocks.values().map(|b| b.size()).sum::<usize>() + decoded.size()
                            > 4 * 1024 * 1024
                    {
                        blocks.clear();
                    }
                    blocks.insert(group, decoded);
                }
                read(blocks.get(&group).ok_or_else(invalid)?, slot, row).map_err(|_| invalid())?
            };
            self.check()?;
            Ok(result)
        };
        operation().map_err(|e| match e {
            CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
            other => BackgroundReadError::Other(other.to_string()),
        })
    }

    fn decrypt_payload_with_unwrap(
        &self,
        data: &[u8],
        reference: &[u8],
        unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
    ) -> Result<Vec<u8>, BackgroundReadError> {
        let operation = || {
            self.check()?;
            let grouped = matches!(parse(reference)?, Reference::Group { .. });
            let plain = decode(data, reference, &|wrapped| {
                self.check()?;
                if !grouped {
                    return unwrap(wrapped);
                }
                self.cached_key(wrapped, unwrap)
            })?;
            let plain = Zeroizing::new(plain);
            self.check()?;
            Ok(plain.to_vec())
        };
        operation().map_err(|error| match error {
            CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
            other => BackgroundReadError::Other(other.to_string()),
        })
    }

    pub(super) fn decrypt(
        &self,
        data: &[u8],
        reference: &[u8],
    ) -> Result<Vec<u8>, BackgroundReadError> {
        self.decrypt_with_unwrap(data, reference, &|key| {
            decrypt_row_key_with_cng_silent(self.credential, key)
        })
    }

    pub(super) fn decrypt_with_unwrap(
        &self,
        data: &[u8],
        reference: &[u8],
        unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
    ) -> Result<Vec<u8>, BackgroundReadError> {
        if matches!(
            parse(reference).map_err(|e| BackgroundReadError::Other(e.to_string()))?,
            Reference::Group { slot: Some(_), .. }
        ) {
            return self.read_block(reference, unwrap, |block, slot, row| {
                Ok(block.text(slot, row)?.as_bytes().to_vec())
            });
        }
        let plain = Zeroizing::new(self.decrypt_payload_with_unwrap(data, reference, unwrap)?);
        crate::ocr_layout::decode_text(&plain)
            .map(|text| text.as_bytes().to_vec())
            .map_err(BackgroundReadError::Other)
    }

    pub(super) fn decrypt_record(
        &self,
        data: &[u8],
        reference: &[u8],
    ) -> Result<DecodedRecord, BackgroundReadError> {
        let unwrap = |key: &[u8]| decrypt_row_key_with_cng_silent(self.credential, key);
        if matches!(
            parse(reference).map_err(|e| BackgroundReadError::Other(e.to_string()))?,
            Reference::Group { slot: Some(_), .. }
        ) {
            return self.read_block(reference, &unwrap, |block, slot, row| {
                let record = block.record(slot, row)?;
                Ok(DecodedRecord {
                    text: record.text,
                    text_layout: record.text_layout,
                    metadata: Some(super::ocr_blocks::Metadata {
                        confidence: record.confidence,
                        box_coords: record.box_coords,
                    }),
                })
            });
        }
        let plain = Zeroizing::new(self.decrypt_payload_with_unwrap(data, reference, &|key| {
            decrypt_row_key_with_cng_silent(self.credential, key)
        })?);
        let (text, text_layout) =
            crate::ocr_layout::decode(&plain).map_err(BackgroundReadError::Other)?;
        Ok(DecodedRecord {
            text,
            text_layout,
            metadata: None,
        })
    }

    pub(super) fn metadata_with_unwrap(
        &self,
        reference: &[u8],
        unwrap: &impl Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
    ) -> Result<Option<super::ocr_blocks::Metadata>, BackgroundReadError> {
        if matches!(
            parse(reference).map_err(|e| BackgroundReadError::Other(e.to_string()))?,
            Reference::Group { slot: Some(_), .. }
        ) {
            self.read_block(reference, unwrap, |block, slot, row| {
                block.metadata_for(slot, row)
            })
            .map(Some)
        } else {
            Ok(None)
        }
    }
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    register(conn).map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ocr_keys(
        id INTEGER PRIMARY KEY,screenshot_id INTEGER NOT NULL,wrapped_key BLOB NOT NULL,
        scope_tag BLOB NOT NULL,live_rows INTEGER NOT NULL DEFAULT 0 CHECK(live_rows>=0));",
    )
    .map_err(|e| e.to_string())?;
    StorageState::add_column_if_missing(&tx, "ocr_results", "ocr_key_id", "INTEGER")?;
    super::ocr_blocks::initialize(&tx)?;
    // Counts and ownership checks maintain the sparse group relation without
    // building another multi-million-row index during startup. Backups verify
    // all members in one sequential pass rather than querying each group.
    tx.execute_batch("DROP VIEW IF EXISTS ocr_results_read;
        CREATE VIEW ocr_results_read AS SELECT r.id,r.screenshot_id,r.text,r.text_hash,
        CASE WHEN r.ocr_block_slot IS NULL THEN r.text_enc ELSE X'' END AS text_enc,
        r.text_key_encrypted,r.confidence,r.box_x1,r.box_y1,r.box_x2,r.box_y2,r.box_x3,r.box_y3,r.box_x4,r.box_y4,
        r.text_enc AS row_text_enc,r.created_at AS row_created_at,
        COALESCE(r.created_at,b.created_at) AS created_at,r.is_deleted,r.ocr_key_id,r.ocr_block_slot,
        cp_ocr_key_ref(r.id,r.screenshot_id,r.ocr_key_id,r.text_key_encrypted,
            k.screenshot_id,k.scope_tag,k.wrapped_key,
            (SELECT value FROM app_metadata WHERE key='protected_content_dataset_id'),r.ocr_block_slot) AS text_key_ref
        FROM ocr_results r LEFT JOIN ocr_keys k ON k.id=r.ocr_key_id
        LEFT JOIN ocr_blocks b ON b.id=r.ocr_key_id AND r.ocr_block_slot IS NOT NULL;
        DROP TRIGGER IF EXISTS ocr_key_insert;
        DROP TRIGGER IF EXISTS ocr_key_update;
        CREATE TRIGGER ocr_key_insert AFTER INSERT ON ocr_results
        WHEN NEW.ocr_key_id IS NOT NULL BEGIN
            SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM ocr_keys WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id)
                THEN RAISE(ABORT,'Missing or mismatched OCR key') END;
            SELECT CASE WHEN NEW.ocr_block_slot IS NOT NULL AND
                (NEW.ocr_block_slot<0 OR NEW.ocr_block_slot>=512 OR NEW.text_enc IS NOT NULL OR NEW.text_key_encrypted IS NOT NULL OR
                 NOT EXISTS(SELECT 1 FROM ocr_blocks WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id))
                THEN RAISE(ABORT,'Invalid OCR block member') END;
            UPDATE ocr_keys SET live_rows=live_rows+1 WHERE id=NEW.ocr_key_id;
        END;
        CREATE TRIGGER ocr_key_update AFTER UPDATE OF ocr_key_id,screenshot_id,ocr_block_slot ON ocr_results
        WHEN OLD.ocr_key_id IS NOT NEW.ocr_key_id OR OLD.screenshot_id!=NEW.screenshot_id OR OLD.ocr_block_slot IS NOT NEW.ocr_block_slot BEGIN
            SELECT CASE WHEN NEW.ocr_key_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM ocr_keys WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id)
                THEN RAISE(ABORT,'Missing or mismatched OCR key') END;
            SELECT CASE WHEN NEW.ocr_block_slot IS NOT NULL AND
                (NEW.ocr_block_slot<0 OR NEW.ocr_block_slot>=512 OR NEW.text_enc IS NOT NULL OR NEW.text_key_encrypted IS NOT NULL OR
                 NOT EXISTS(SELECT 1 FROM ocr_blocks WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id))
                THEN RAISE(ABORT,'Invalid OCR block member') END;
            UPDATE ocr_keys SET live_rows=live_rows+1 WHERE id=NEW.ocr_key_id;
            UPDATE ocr_keys SET live_rows=live_rows-1 WHERE id=OLD.ocr_key_id;
            DELETE FROM ocr_keys WHERE id=OLD.ocr_key_id AND live_rows=0;
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_key_delete AFTER DELETE ON ocr_results BEGIN
            UPDATE ocr_keys SET live_rows=live_rows-1 WHERE id=OLD.ocr_key_id;
            DELETE FROM ocr_keys WHERE id=OLD.ocr_key_id AND live_rows=0;
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_key_in_use BEFORE DELETE ON ocr_keys
        WHEN OLD.live_rows>0 BEGIN SELECT RAISE(ABORT,'OCR key is still referenced'); END;")
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
