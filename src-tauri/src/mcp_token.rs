use crate::credential_manager::{
    self, decrypt_with_master_key, encrypt_with_master_key, CredentialManagerState,
};
use sha2::{Digest, Sha256};

const TOKEN_V2_PREFIX: &str = "v2:";

fn derive_legacy_mcp_key(credential_state: &CredentialManagerState) -> Result<[u8; 32], String> {
    let public_key = credential_manager::get_cached_public_key(credential_state)
        .or_else(|| credential_manager::load_public_key_from_file(credential_state).ok())
        .ok_or("Public key not available")?;
    let mut hasher = Sha256::new();
    hasher.update(&public_key);
    hasher.update(b"CarbonPaper-MCP-Token-Key-v1");
    Ok(hasher.finalize().into())
}

fn derive_mcp_key(credential_state: &CredentialManagerState) -> Result<[u8; 32], String> {
    let master_key = credential_manager::get_cached_master_key(credential_state)
        .ok_or_else(|| "AUTH_REQUIRED".to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&master_key);
    hasher.update(b"CarbonPaper-MCP-Token-Key-v2");
    Ok(hasher.finalize().into())
}

/// Generate a random 64-character hex token.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

/// Encrypt a token with a master-key-derived MCP key and return versioned base64.
pub fn encrypt_token(
    credential_state: &CredentialManagerState,
    token: &str,
) -> Result<String, String> {
    let key = derive_mcp_key(credential_state)?;
    let encrypted = encrypt_with_master_key(&key, token.as_bytes())
        .map_err(|e| format!("Token encryption failed: {}", e))?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted);
    Ok(format!("{}{}", TOKEN_V2_PREFIX, encoded))
}

fn decrypt_token_with_key(encrypted_b64: &str, key: &[u8; 32]) -> Result<String, String> {
    let encrypted =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_b64)
            .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let decrypted = decrypt_with_master_key(key, &encrypted)
        .map_err(|e| format!("Token decryption failed: {}", e))?;
    String::from_utf8(decrypted).map_err(|e| format!("Invalid UTF-8: {}", e))
}

pub fn is_current_format(encrypted_token: &str) -> bool {
    encrypted_token.starts_with(TOKEN_V2_PREFIX)
}

/// Decrypt a versioned token. Legacy public-key-derived tokens remain readable
/// so they can be migrated to v2 when the user unlocks the app.
pub fn decrypt_token(
    credential_state: &CredentialManagerState,
    encrypted_token: &str,
) -> Result<String, String> {
    if let Some(encrypted_b64) = encrypted_token.strip_prefix(TOKEN_V2_PREFIX) {
        let key = derive_mcp_key(credential_state)?;
        return decrypt_token_with_key(encrypted_b64, &key);
    }

    let key = derive_legacy_mcp_key(credential_state)?;
    decrypt_token_with_key(encrypted_token, &key)
}

/// Compute SHA-256 hash of a token string.
pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
