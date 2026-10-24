use crate::protocol::{BrokerError, Result, TaskBinding, TaskKey, MAX_INPUT_BYTES};
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub fn new_key() -> TaskKey {
    let mut key = vec![0; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    TaskKey(key)
}

/// Storage format: a fresh 12-byte nonce, ciphertext, and a 16-byte GCM tag.
/// Keep the concrete AES hot path in this crate's optimized compilation unit.
#[inline(never)]
pub fn encrypt_storage_bytes(key: &[u8], plaintext: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|error| format!("Failed to create cipher: {error}"))?;
    let nonce_bytes: [u8; 12] = rand::random();
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|error| format!("Encryption failed: {error}"))?;
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

#[inline(never)]
pub fn decrypt_storage_bytes(key: &[u8], encrypted: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if encrypted.len() < 28 {
        return Err("Invalid encrypted data".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|error| format!("Failed to create cipher: {error}"))?;
    cipher
        .decrypt(Nonce::from_slice(&encrypted[..12]), &encrypted[12..])
        .map_err(|error| format!("Decryption failed: {error}"))
}

pub fn encrypt_input(key: &TaskKey, binding: &TaskBinding, input: &[u8]) -> Result<Vec<u8>> {
    if input.len() as u64 > MAX_INPUT_BYTES || binding.payload_bytes != input.len() as u64 + 28 {
        return Err(BrokerError::LimitExceeded);
    }
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| BrokerError::Integrity)?;
    let mut nonce = [0; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let aad = binding.aad()?;
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: input,
                aad: &aad,
            },
        )
        .map_err(|_| BrokerError::Protection)?;
    let mut output = nonce.to_vec();
    output.extend_from_slice(&encrypted);
    Ok(output)
}

pub fn decrypt_input(
    key: &TaskKey,
    binding: &TaskBinding,
    encrypted: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if encrypted.len() < 28
        || encrypted.len() as u64 != binding.payload_bytes
        || encrypted.len() as u64 > MAX_INPUT_BYTES + 28
    {
        return Err(BrokerError::Integrity);
    }
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| BrokerError::Integrity)?;
    let aad = binding.aad()?;
    cipher
        .decrypt(
            Nonce::from_slice(&encrypted[..12]),
            Payload {
                msg: &encrypted[12..],
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| BrokerError::Integrity)
}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn encrypt_recap(
    key: &TaskKey,
    binding: &crate::protocol::RecapBinding,
    input: &[u8],
) -> Result<Vec<u8>> {
    if input.len() as u64 > MAX_INPUT_BYTES || binding.payload_bytes != input.len() as u64 + 28 {
        return Err(BrokerError::LimitExceeded);
    }
    let aad = serde_json::to_vec(binding).map_err(|_| BrokerError::Integrity)?;
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| BrokerError::Integrity)?;
    let mut nonce = [0; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: input,
                aad: &aad,
            },
        )
        .map_err(|_| BrokerError::Protection)?;
    Ok([nonce.as_slice(), &encrypted].concat())
}

pub fn decrypt_recap(
    key: &TaskKey,
    binding: &crate::protocol::RecapBinding,
    encrypted: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if encrypted.len() < 28
        || encrypted.len() as u64 != binding.payload_bytes
        || encrypted.len() as u64 > MAX_INPUT_BYTES + 28
    {
        return Err(BrokerError::Integrity);
    }
    let aad = serde_json::to_vec(binding).map_err(|_| BrokerError::Integrity)?;
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_| BrokerError::Integrity)?;
    cipher
        .decrypt(
            Nonce::from_slice(&encrypted[..12]),
            Payload {
                msg: &encrypted[12..],
                aad: &aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| BrokerError::Integrity)
}

/// Implemented by the service using user-context DPAPI followed by SYSTEM DPAPI.
/// There is no exported interface for unwrapping a caller-supplied blob.
pub trait KeyProtector {
    fn protect(&self, key: &TaskKey) -> Result<Vec<u8>>;
    fn unprotect(&self, blob: &[u8]) -> Result<TaskKey>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_decrypts_the_existing_nonce_ciphertext_tag_format() {
        // AES-256-GCM known-answer vector: zero key/nonce and 16 zero bytes.
        let encrypted = hex::decode(concat!(
            "000000000000000000000000",
            "cea7403d4d606b6e074ec5d3baf39d18",
            "d0d1c8a799996bf0265b98b5d48ab919"
        ))
        .unwrap();
        assert_eq!(
            decrypt_storage_bytes(&[0; 32], &encrypted).unwrap(),
            [0; 16]
        );
        assert!(decrypt_storage_bytes(&[1; 32], &encrypted).is_err());
        for index in [0, 12, encrypted.len() - 1] {
            let mut changed = encrypted.clone();
            changed[index] ^= 1;
            assert!(decrypt_storage_bytes(&[0; 32], &changed).is_err());
        }
        for length in 0..28 {
            assert!(decrypt_storage_bytes(&[0; 32], &encrypted[..length]).is_err());
        }
        assert!(decrypt_storage_bytes(&[0; 31], &encrypted).is_err());
    }

    #[test]
    fn storage_encrypts_with_fresh_nonces_and_legacy_reader_compatibility() {
        let key = [7; 32];
        let legacy_cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        for plaintext in [b"".as_slice(), b"screenshot bytes".as_slice()] {
            let first = encrypt_storage_bytes(&key, plaintext).unwrap();
            let second = encrypt_storage_bytes(&key, plaintext).unwrap();
            assert_eq!(first.len(), plaintext.len() + 28);
            assert_ne!(&first[..12], &second[..12]);
            assert_eq!(
                legacy_cipher
                    .decrypt(Nonce::from_slice(&first[..12]), &first[12..])
                    .unwrap(),
                plaintext
            );
            assert_eq!(decrypt_storage_bytes(&key, &first).unwrap(), plaintext);
        }
        assert!(encrypt_storage_bytes(&[0; 31], b"bytes").is_err());
    }

    #[test]
    fn input_is_bound_to_the_exact_task_and_dataset() {
        let key = new_key();
        let mut task = TaskBinding {
            task_id: crate::protocol::random_id(),
            dataset_id: crate::protocol::random_id(),
            screenshot_id: 7,
            input_version: 1,
            consumers: 7,
            payload_bytes: 34,
            expires_at: 99,
        };
        let encrypted = encrypt_input(&key, &task, b"secret").unwrap();
        assert_eq!(
            &**decrypt_input(&key, &task, &encrypted).unwrap(),
            b"secret"
        );
        task.screenshot_id = 8;
        assert!(decrypt_input(&key, &task, &encrypted).is_err());
        task.screenshot_id = 7;
        task.dataset_id = crate::protocol::random_id();
        assert!(decrypt_input(&key, &task, &encrypted).is_err());
        assert!(decrypt_input(&new_key(), &task, &encrypted).is_err());
    }

    #[test]
    fn recap_ciphertext_authenticates_its_slot_dataset_and_original_expiry() {
        let key = new_key();
        let binding = crate::protocol::RecapBinding {
            object_id: crate::protocol::random_id(),
            dataset_id: crate::protocol::random_id(),
            slot: "config".into(),
            payload_bytes: 34,
            expires_at: 99,
        };
        let encrypted = encrypt_recap(&key, &binding, b"secret").unwrap();
        assert_eq!(
            &**decrypt_recap(&key, &binding, &encrypted).unwrap(),
            b"secret"
        );
        let mut altered = binding.clone();
        altered.slot = "context".into();
        assert!(decrypt_recap(&key, &altered, &encrypted).is_err());
        altered = binding.clone();
        altered.expires_at = 100;
        assert!(decrypt_recap(&key, &altered, &encrypted).is_err());
        altered = binding;
        altered.dataset_id = crate::protocol::random_id();
        assert!(decrypt_recap(&key, &altered, &encrypted).is_err());
    }
}
