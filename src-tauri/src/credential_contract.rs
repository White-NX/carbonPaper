//! The persisted credential format shared by CarbonPaper and offline data tools.
//! These identifiers contain no private key material. Opening an existing key
//! must never create, replace, or change its Windows protection policy.

use sha2::{Digest, Sha256};

pub const CNG_KEY_NAME: &str = "CarbonPaperMasterKeyV3";
pub const CNG_PROVIDER_NAME: &str = "Microsoft Software Key Storage Provider";
pub const MASTER_KEY_FILE_MAGIC: &[u8; 5] = b"CPMK3";
pub const MASTER_KEY_FILE_NAME: &str = "credential_master_key.bin";
pub const PUBLIC_KEY_FILE_NAME: &str = "credential_public_key.bin";

/// The bootstrap SQLCipher key. Protected image/row payloads still require
/// their separately wrapped content keys and an authenticated CNG session.
pub fn weak_database_key(public_key: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    hasher.update(b"CarbonPaper-Weak-DB-Key-v1");
    hasher.finalize().into()
}
