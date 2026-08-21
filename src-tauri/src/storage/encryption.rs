//! Row-level and ChromaDB encryption/decryption helpers.

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_row_key_with_cng_silent, decrypt_with_master_key,
    encrypt_with_exported_public_key, encrypt_with_master_key, get_cached_public_key,
    load_public_key_from_file, CredentialError,
};
use rand::RngCore;

use super::{BackgroundReadError, StorageState};

impl StorageState {
    /// Zeroize sensitive data in memory to reduce risk of leakage.
    pub(crate) fn zeroize_bytes(bytes: &mut [u8]) {
        use std::sync::atomic::{compiler_fence, Ordering};
        for b in bytes.iter_mut() {
            // SAFETY: `b` is a unique, valid mutable reference into `bytes`; volatile
            // writes prevent this best-effort zeroization from being optimized away.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }

    pub(crate) fn wrap_row_key_for_storage(&self, row_key: &[u8]) -> Result<Vec<u8>, String> {
        let public_key = self.get_public_key()?;
        encrypt_with_exported_public_key(&public_key, row_key)
            .map_err(|e| format!("Failed to wrap row key with public key: {}", e))
    }

    pub(super) fn encrypt_payload_with_row_key(
        &self,
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_data = encrypt_with_master_key(&row_key, plaintext)
            .map_err(|e| format!("Failed to encrypt payload: {}", e))?;

        let encrypted_key = self.wrap_row_key_for_storage(&row_key)?;

        Self::zeroize_bytes(&mut row_key);
        Ok((encrypted_data, encrypted_key))
    }

    pub(crate) fn decrypt_payload_with_row_key(
        &self,
        encrypted_data: &[u8],
        encrypted_key: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mut row_key = decrypt_row_key_with_cng(encrypted_key)
            .map_err(|e| format!("Failed to unwrap row key: {}", e))?;

        let decrypted = decrypt_with_master_key(&row_key, encrypted_data)
            .map_err(|e| format!("Failed to decrypt payload: {}", e))?;

        Self::zeroize_bytes(&mut row_key);
        Ok(decrypted)
    }

    pub(crate) fn decrypt_payload_with_row_key_silent(
        &self,
        encrypted_data: &[u8],
        encrypted_key: &[u8],
    ) -> Result<Vec<u8>, BackgroundReadError> {
        // Re-check immediately before the CNG unwrap. This closes the small
        // window in which a caller can be admitted and the background lease
        // can then be disabled before protected bytes are decrypted.
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        Self::decrypt_payload_with_unwrap(encrypted_data, encrypted_key, &|ciphertext| {
            decrypt_row_key_with_cng_silent(ciphertext)
        })
    }

    /// Row-payload decryption with an injected row-key unwrap, so batch
    /// callers can reuse one `CngKeySession` handle instead of paying a CNG
    /// open/free round-trip for every row.
    pub(crate) fn decrypt_payload_with_unwrap(
        encrypted_data: &[u8],
        encrypted_key: &[u8],
        unwrap_row_key: &dyn Fn(&[u8]) -> Result<Vec<u8>, CredentialError>,
    ) -> Result<Vec<u8>, BackgroundReadError> {
        let mut row_key = unwrap_row_key(encrypted_key).map_err(|error| match error {
            CredentialError::AuthRequired => BackgroundReadError::AuthRequired,
            other => BackgroundReadError::Other(format!("Failed to unwrap row key: {}", other)),
        })?;

        let decrypted = decrypt_with_master_key(&row_key, encrypted_data)
            .map_err(|e| BackgroundReadError::Other(format!("Failed to decrypt payload: {}", e)));

        Self::zeroize_bytes(&mut row_key);
        decrypted
    }

    /// Encrypt text for ChromaDB storage.
    pub fn encrypt_for_chromadb(&self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Ok(text.to_string());
        }

        let (encrypted_data, encrypted_key) = self.encrypt_payload_with_row_key(text.as_bytes())?;
        let payload = serde_json::json!({
            "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted_data),
            "key": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted_key)
        });

        Ok(format!("ENC2:{}", payload.to_string()))
    }

    /// Decrypt text from ChromaDB storage.
    pub fn decrypt_from_chromadb(&self, encrypted: &str) -> Result<String, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".to_string());
        }
        self.decrypt_from_chromadb_with_mode(encrypted, false)
            .map_err(|error| error.to_string())
    }

    /// Decrypt Chroma text for unattended work without ever allowing CNG UI.
    pub(crate) fn decrypt_from_chromadb_silent(
        &self,
        encrypted: &str,
    ) -> Result<String, BackgroundReadError> {
        if !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        self.decrypt_from_chromadb_with_mode(encrypted, true)
    }

    /// Batch variant used by Python HDBSCAN metadata hydration. A single
    /// authorization/CNG failure aborts the batch so callers retain the task
    /// instead of silently clustering ciphertext.
    pub(crate) fn decrypt_many_from_chromadb_silent(
        &self,
        encrypted_list: &[String],
    ) -> Result<Vec<String>, BackgroundReadError> {
        if !encrypted_list.is_empty() && !self.is_silent_read_authorized() {
            return Err(BackgroundReadError::AuthRequired);
        }
        encrypted_list
            .iter()
            .map(|encrypted| self.decrypt_from_chromadb_with_mode(encrypted, true))
            .collect()
    }

    fn decrypt_from_chromadb_with_mode(
        &self,
        encrypted: &str,
        silent: bool,
    ) -> Result<String, BackgroundReadError> {
        if encrypted.is_empty()
            || (!encrypted.starts_with("ENC2:") && !encrypted.starts_with("ENC:"))
        {
            return Ok(encrypted.to_string());
        }

        if encrypted.starts_with("ENC:") {
            return Err(BackgroundReadError::Other(
                "Legacy ENC format is no longer supported. Please migrate data.".to_string(),
            ));
        }

        let data = &encrypted[5..]; // Remove "ENC2:" prefix
        let payload: serde_json::Value = serde_json::from_str(data).map_err(|e| {
            BackgroundReadError::Other(format!("Failed to parse encrypted payload: {}", e))
        })?;
        let enc_data_b64 = payload
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackgroundReadError::Other("Missing data field".to_string()))?;
        let enc_key_b64 = payload
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BackgroundReadError::Other("Missing key field".to_string()))?;

        let encrypted_data =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc_data_b64)
                .map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to decode encrypted data: {}", e))
                })?;
        let encrypted_key =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc_key_b64)
                .map_err(|e| {
                    BackgroundReadError::Other(format!("Failed to decode encrypted key: {}", e))
                })?;

        let decrypted = if silent {
            self.decrypt_payload_with_row_key_silent(&encrypted_data, &encrypted_key)?
        } else {
            self.decrypt_payload_with_row_key(&encrypted_data, &encrypted_key)
                .map_err(BackgroundReadError::Other)?
        };
        String::from_utf8(decrypted).map_err(|e| {
            BackgroundReadError::Other(format!("Invalid UTF-8 in decrypted data: {}", e))
        })
    }

    /// Get public key (for backward-compatible IPC/interface).
    pub fn get_public_key(&self) -> Result<Vec<u8>, String> {
        get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())
    }
}
