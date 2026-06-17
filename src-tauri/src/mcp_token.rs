use crate::credential_manager::{
    self, decrypt_with_master_key, encrypt_with_master_key, CredentialManagerState,
};
use sha2::{Digest, Sha256};

/// Derive an AES-256 key from the public key for MCP token encryption.
/// This key is always available without Windows Hello authentication.
pub fn derive_mcp_key(credential_state: &CredentialManagerState) -> Result<[u8; 32], String> {
    let public_key = credential_manager::get_cached_public_key(credential_state)
        .or_else(|| credential_manager::load_public_key_from_file(credential_state).ok())
        .ok_or("Public key not available")?;
    let mut hasher = Sha256::new();
    hasher.update(&public_key);
    hasher.update(b"CarbonPaper-MCP-Token-Key-v1");
    Ok(hasher.finalize().into())
}

/// Generate a random 64-character hex token.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

/// Encrypt a token with the MCP-derived key and return base64.
pub fn encrypt_token(
    credential_state: &CredentialManagerState,
    token: &str,
) -> Result<String, String> {
    let key = derive_mcp_key(credential_state)?;
    let encrypted = encrypt_with_master_key(&key, token.as_bytes())
        .map_err(|e| format!("Token encryption failed: {}", e))?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &encrypted,
    ))
}

/// Decrypt a base64-encoded encrypted token.
pub fn decrypt_token(
    credential_state: &CredentialManagerState,
    encrypted_b64: &str,
) -> Result<String, String> {
    let key = derive_mcp_key(credential_state)?;
    let encrypted =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_b64)
            .map_err(|e| format!("Base64 decode failed: {}", e))?;
    let decrypted = decrypt_with_master_key(&key, &encrypted)
        .map_err(|e| format!("Token decryption failed: {}", e))?;
    String::from_utf8(decrypted).map_err(|e| format!("Invalid UTF-8: {}", e))
}

/// Compute SHA-256 hash of a token string.
pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
