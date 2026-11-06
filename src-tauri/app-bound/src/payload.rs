//! Versioned authenticated envelopes for derived metadata and index files.
//! Keys are supplied by a CNG session or the verified, dataset-scoped broker.

use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 8] = b"CPENC001";
pub const KEY_ID_BYTES: usize = 16;
pub const OVERHEAD: usize = 8 + KEY_ID_BYTES + 12 + 16;

pub struct CipherKey {
    pub id: String,
    pub dataset: String,
    pub purpose: String,
    key: Zeroizing<[u8; 32]>,
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for CipherKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CipherKey")
            .field("id", &self.id)
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

impl CipherKey {
    pub fn new(id: String, dataset: String, purpose: String, bytes: &[u8]) -> Result<Self, String> {
        let decoded = hex::decode(&id).map_err(|_| "Invalid content key identity")?;
        if decoded.len() != KEY_ID_BYTES || bytes.len() != 32 {
            return Err("Invalid content key".into());
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(bytes);
        let cipher = Aes256Gcm::new_from_slice(&*key).map_err(|_| "Invalid content key")?;
        Ok(Self {
            id,
            dataset,
            purpose,
            key,
            cipher,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &*self.key
    }

    fn aad(&self, identity: &str) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&(
            "CarbonPaper-Derived-v1",
            &self.dataset,
            &self.purpose,
            &self.id,
            identity,
        ))
        .map_err(|e| e.to_string())
    }

    pub fn seal(&self, identity: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let nonce: [u8; 12] = rand::random();
        let aad = self.aad(identity)?;
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| "Content encryption failed")?;
        let mut output = Vec::with_capacity(OVERHEAD + plaintext.len());
        output.extend_from_slice(MAGIC);
        output.extend(hex::decode(&self.id).map_err(|_| "Invalid content key identity")?);
        output.extend_from_slice(&nonce);
        output.extend(ciphertext);
        Ok(output)
    }

    pub fn open(&self, identity: &str, encrypted: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
        if payload_key_id(encrypted)? != self.id {
            return Err("Content key identity mismatch".into());
        }
        let aad = self.aad(identity)?;
        self.cipher
            .decrypt(
                Nonce::from_slice(&encrypted[24..36]),
                Payload {
                    msg: &encrypted[36..],
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| "Content authentication failed".into())
    }
}

pub fn is_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

pub fn payload_key_id(bytes: &[u8]) -> Result<String, String> {
    if !is_sealed(bytes) || bytes.len() < OVERHEAD {
        return Err("Encrypted content format is required".into());
    }
    Ok(hex::encode(&bytes[8..24]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(dataset: &str, purpose: &str) -> CipherKey {
        CipherKey::new("01".repeat(16), dataset.into(), purpose.into(), &[7; 32]).unwrap()
    }
    #[test]
    fn payloads_authenticate_dataset_purpose_record_and_key_identity() {
        let key = key("dataset", "vectors");
        let encrypted = key.seal("clip_image:one", b"private vector").unwrap();
        assert_eq!(
            &**key.open("clip_image:one", &encrypted).unwrap(),
            b"private vector"
        );
        assert!(key.open("clip_image:two", &encrypted).is_err());
        assert!(super::tests::key("other", "vectors")
            .open("clip_image:one", &encrypted)
            .is_err());
        assert!(super::tests::key("dataset", "metadata")
            .open("clip_image:one", &encrypted)
            .is_err());
        for index in [8, 24, 36, encrypted.len() - 1] {
            let mut corrupt = encrypted.clone();
            corrupt[index] ^= 1;
            assert!(key.open("clip_image:one", &corrupt).is_err());
        }
    }
    #[test]
    fn writes_have_fresh_nonces_and_legacy_plaintext_is_rejected() {
        let key = key("dataset", "metadata");
        assert_ne!(
            key.seal("summary:1", b"secret").unwrap(),
            key.seal("summary:1", b"secret").unwrap()
        );
        assert!(key.open("summary:1", b"legacy plaintext").is_err());
    }
}
