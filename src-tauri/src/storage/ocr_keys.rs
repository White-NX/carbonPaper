//! One fresh OCR-only key per capture batch, with authenticated row identities.
//! Read references are assembled from SQL metadata, never persisted in the
//! legacy RSA envelope column. They contain wrapped keys, not decrypted keys.
use super::{BackgroundReadError, StorageState};
use crate::credential_manager::{
    decrypt_row_key_with_cng_silent, decrypt_with_master_key, CredentialError,
    CredentialManagerState,
};
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rusqlite::{functions::FunctionFlags, params, Connection};
use sha2::Sha256;
use std::{collections::HashMap, sync::Mutex, time::Instant};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"CPORKEY1";
const DATASET: &str = "protected_content_dataset_id";
const MAX_CACHED_KEYS: usize = 64;

pub(super) mod backup;
mod migration;
pub(super) use migration::CURSOR as COMPACTION_CURSOR;

fn invalid() -> CredentialError {
    CredentialError::CryptoError("Invalid OCR key or payload binding".into())
}

fn scope(dataset: &str, screenshot: i64, group: i64) -> Result<Vec<u8>, CredentialError> {
    if !carbonpaper_app_bound::protocol::valid_id(dataset) || screenshot <= 0 || group <= 0 {
        return Err(invalid());
    }
    let mut bytes = b"carbonpaper/ocr-key/v1\0".to_vec();
    bytes.extend_from_slice(dataset.as_bytes());
    bytes.extend_from_slice(&screenshot.to_le_bytes());
    bytes.extend_from_slice(&group.to_le_bytes());
    Ok(bytes)
}

fn scope_tag(
    key: &[u8],
    dataset: &str,
    screenshot: i64,
    group: i64,
) -> Result<Vec<u8>, CredentialError> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| invalid())?;
    mac.update(&scope(dataset, screenshot, group)?);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn verify_scope(
    key: &[u8],
    dataset: &str,
    screenshot: i64,
    group: i64,
    tag: &[u8],
) -> Result<(), CredentialError> {
    if key.len() != 32 {
        return Err(invalid());
    }
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| invalid())?;
    mac.update(&scope(dataset, screenshot, group)?);
    mac.verify_slice(tag).map_err(|_| invalid())
}

fn aad(dataset: &str, screenshot: i64, group: i64, row: i64) -> Result<Vec<u8>, CredentialError> {
    if row <= 0 {
        return Err(invalid());
    }
    let mut data = scope(dataset, screenshot, group)?;
    data.extend_from_slice(b"/text/");
    data.extend_from_slice(&row.to_le_bytes());
    Ok(data)
}

fn seal(key: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, CredentialError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| invalid())?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut result = nonce.to_vec();
    result.extend(
        cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| invalid())?,
    );
    Ok(result)
}

fn open(key: &[u8], aad: &[u8], encrypted: &[u8]) -> Result<Vec<u8>, CredentialError> {
    if encrypted.len() < 28 {
        return Err(invalid());
    }
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| invalid())?
        .decrypt(
            Nonce::from_slice(&encrypted[..12]),
            Payload {
                msg: &encrypted[12..],
                aad,
            },
        )
        .map_err(|_| invalid())
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
}

pub(super) fn dataset(conn: &Connection) -> Result<String, String> {
    conn.query_row(
        "SELECT value FROM app_metadata WHERE key=?1",
        [DATASET],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

impl StorageState {
    pub(super) fn ocr_decoder(&self) -> Decoder<'_> {
        Decoder::new(&self.credential_state)
    }
    pub(super) fn new_ocr_write_key(&self) -> Result<WriteKey, String> {
        WriteKey::new(|key| self.wrap_row_key_for_storage(key))
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
        8,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            let group: Option<i64> = ctx.get(2)?;
            let mut result = MAGIC.to_vec();
            let Some(group) = group else {
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
            result.push(1);
            for id in [row, screenshot, group, owner] {
                result.extend_from_slice(&id.to_le_bytes());
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
        Some(1) if bytes.len() >= 74 => {
            let read = |start| i64::from_le_bytes(bytes[start..start + 8].try_into().unwrap());
            let row = read(9);
            let screenshot = read(17);
            let group = read(25);
            let owner = read(33);
            let length = bytes[41] as usize;
            if owner != screenshot || bytes.len() < 42 + length + 32 || length > 128 {
                return Err(invalid());
            }
            let dataset = std::str::from_utf8(&bytes[42..42 + length]).map_err(|_| invalid())?;
            scope(dataset, screenshot, group)?;
            Ok(Reference::Group {
                row,
                screenshot,
                group,
                dataset,
                tag: &bytes[42 + length..74 + length],
                wrapped: &bytes[74 + length..],
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
        } => {
            let key = Zeroizing::new(unwrap(wrapped)?);
            verify_scope(&key, dataset, screenshot, group, tag)?;
            open(&key, &aad(dataset, screenshot, group, row)?, data)
        }
    }
}

/// A bounded request-local cache. Every hit rechecks the admitted credential
/// session; dropped/revoked readers cannot keep using a previously unwrapped key.
pub(super) struct Decoder<'a> {
    credential: &'a CredentialManagerState,
    admitted: Option<Instant>,
    keys: Mutex<HashMap<Vec<u8>, Zeroizing<Vec<u8>>>>,
}

impl<'a> Decoder<'a> {
    pub(super) fn new(credential: &'a CredentialManagerState) -> Self {
        Self {
            credential,
            admitted: credential.session_marker(),
            keys: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self) -> Result<(), CredentialError> {
        if !self.credential.protected_read_authorized()
            || self.credential.session_marker() != self.admitted
        {
            self.keys.lock().unwrap_or_else(|e| e.into_inner()).clear();
            Err(CredentialError::AuthRequired)
        } else {
            Ok(())
        }
    }

    pub(super) fn decrypt_with_unwrap(
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
    // Counts and ownership checks maintain the sparse group relation without
    // building another multi-million-row index during startup. Backups verify
    // all members in one sequential pass rather than querying each group.
    tx.execute_batch("CREATE VIEW IF NOT EXISTS ocr_results_read AS SELECT r.*,
        cp_ocr_key_ref(r.id,r.screenshot_id,r.ocr_key_id,r.text_key_encrypted,
            k.screenshot_id,k.scope_tag,k.wrapped_key,
            (SELECT value FROM app_metadata WHERE key='protected_content_dataset_id')) AS text_key_ref
        FROM ocr_results r LEFT JOIN ocr_keys k ON k.id=r.ocr_key_id;
        CREATE TRIGGER IF NOT EXISTS ocr_key_insert AFTER INSERT ON ocr_results
        WHEN NEW.ocr_key_id IS NOT NULL BEGIN
            SELECT CASE WHEN NOT EXISTS(SELECT 1 FROM ocr_keys WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id)
                THEN RAISE(ABORT,'Missing or mismatched OCR key') END;
            UPDATE ocr_keys SET live_rows=live_rows+1 WHERE id=NEW.ocr_key_id;
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_key_update AFTER UPDATE OF ocr_key_id,screenshot_id ON ocr_results
        WHEN OLD.ocr_key_id IS NOT NEW.ocr_key_id OR OLD.screenshot_id!=NEW.screenshot_id BEGIN
            SELECT CASE WHEN NEW.ocr_key_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM ocr_keys WHERE id=NEW.ocr_key_id AND screenshot_id=NEW.screenshot_id)
                THEN RAISE(ABORT,'Missing or mismatched OCR key') END;
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
