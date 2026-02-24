//! Row-level and ChromaDB encryption/decryption helpers.

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_with_master_key, encrypt_row_key_with_cng,
    encrypt_with_master_key, get_cached_public_key, load_public_key_from_file,
};
use rand::RngCore;

use super::StorageState;

impl StorageState {
    /// Zeroize sensitive data in memory to reduce risk of leakage.
    pub(crate) fn zeroize_bytes(bytes: &mut [u8]) {
        use std::sync::atomic::{compiler_fence, Ordering};
        for b in bytes.iter_mut() {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }

    pub(super) fn encrypt_payload_with_row_key(
        &self,
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_data = encrypt_with_master_key(&row_key, plaintext)
            .map_err(|e| format!("Failed to encrypt payload: {}", e))?;

        let encrypted_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap row key: {}", e))?;

        Self::zeroize_bytes(&mut row_key);
        Ok((encrypted_data, encrypted_key))
    }

    pub(super) fn decrypt_payload_with_row_key(
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
        if encrypted.is_empty()
            || (!encrypted.starts_with("ENC2:") && !encrypted.starts_with("ENC:"))
        {
            return Ok(encrypted.to_string());
        }

        if encrypted.starts_with("ENC:") {
            return Err(
                "Legacy ENC format is no longer supported. Please migrate data.".to_string(),
            );
        }

        let data = &encrypted[5..]; // Remove "ENC2:" prefix
        let payload: serde_json::Value = serde_json::from_str(data)
            .map_err(|e| format!("Failed to parse encrypted payload: {}", e))?;
        let enc_data_b64 = payload
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing data field".to_string())?;
        let enc_key_b64 = payload
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing key field".to_string())?;

        let encrypted_data =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc_data_b64)
                .map_err(|e| format!("Failed to decode encrypted data: {}", e))?;
        let encrypted_key =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, enc_key_b64)
                .map_err(|e| format!("Failed to decode encrypted key: {}", e))?;

        let decrypted = self.decrypt_payload_with_row_key(&encrypted_data, &encrypted_key)?;
        String::from_utf8(decrypted).map_err(|e| format!("Invalid UTF-8 in decrypted data: {}", e))
    }

    /// Get public key (for backward-compatible IPC/interface).
    pub fn get_public_key(&self) -> Result<Vec<u8>, String> {
        get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())
    }
}
