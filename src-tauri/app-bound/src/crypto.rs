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
}
