//! Credential management backed by the Windows CNG (`NCrypt`) API.
//!
//! Security model:
//! - Encryption uses an exported public key and can run unattended in background work.
//! - Decryption uses the persisted private key, whose high-protection UI policy requires
//!   OS-mediated user verification before sensitive data becomes available.
//!
//! The Microsoft Software Key Storage Provider owns the RSA key pair. CarbonPaper sets
//! `NCRYPT_UI_POLICY` to high protection, exports only the public blob for writes, and
//! keeps decrypted master-key material in the bounded authenticated session cache.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;
use thiserror::Error;

/// Errors produced by credential and key-management operations.
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum CredentialError {
    /// Windows Hello or the backing credential provider is unavailable.
    #[error("Windows Hello is not available")]
    WindowsHelloNotAvailable,
    /// The user cancelled OS authentication.
    #[error("User cancelled authentication")]
    UserCancelled,
    /// The requested key does not exist.
    #[error("Credential key not found")]
    KeyNotFound,
    /// An authenticated session is required.
    #[error("Authentication required")]
    AuthRequired,
    /// Encryption or decryption failed.
    #[error("Crypto error: {0}")]
    CryptoError(String),
    /// A Windows or operating-system operation failed.
    #[error("System error: {0}")]
    SystemError(String),
    /// The key already exists.
    #[error("Credential key already exists")]
    KeyAlreadyExists,
}

/// Default authenticated-session timeout in seconds.
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 15 * 60; // 15 分钟
const MASTER_KEY_FILE_NAME: &str = "credential_master_key.bin";
const MASTER_KEY_LEN: usize = 32;
const MASTER_KEY_FILE_MAGIC: &[u8; 5] = b"CPMK3"; // 版本升级
const CNG_KEY_NAME: &str = "CarbonPaperMasterKeyV3";
// The Software KSP supports RSA encryption and protected UI policy.
const CNG_PROVIDER_NAME: &str = "Microsoft Software Key Storage Provider";

/// Shared credential-manager state.
pub struct CredentialManagerState {
    /// Cached SQLCipher database key, available only to an authenticated UI session.
    cached_db_key: Mutex<Option<Vec<u8>>>,
    /// Cached public key used to encrypt new data without user interaction.
    cached_public_key: Mutex<Option<Vec<u8>>>,
    /// Cached master key used by background data encryption.
    cached_master_key: Mutex<Option<Vec<u8>>>,
    /// Data directory containing persisted key material.
    data_dir: Mutex<PathBuf>,
    /// Time of the last successful authentication, used for session expiry.
    last_auth_time: Mutex<Option<std::time::Instant>>,
    /// Whether the application UI is currently foregrounded.
    app_in_foreground: Mutex<bool>,
    /// Session timeout in seconds; `-1` disables time-based expiry.
    session_timeout_secs: Mutex<i64>,
}

impl CredentialManagerState {
    pub fn get_hmac_key(&self) -> Result<Vec<u8>, String> {
        let guard = self.cached_master_key.lock().unwrap();
        if let Some(key) = &*guard {
            Ok(derive_hmac_key_from_master(key))
        } else {
            Err("Master key not unlocked".to_string())
        }
    }

    pub fn new(data_dir: PathBuf) -> Self {
        // Start with secure defaults.
        let default = DEFAULT_SESSION_TIMEOUT_SECS as i64;
        let mut initial_timeout = default;

        // Restore the persisted timeout, which is stored in seconds.
        if let Some(s) = crate::registry_config::get_string("session_timeout_secs") {
            if let Ok(parsed) = s.parse::<i64>() {
                initial_timeout = parsed;
            } else {
                tracing::error!("Failed to parse session_timeout_secs from registry: {}", s);
            }
        }

        Self {
            cached_db_key: Mutex::new(None),
            cached_public_key: Mutex::new(None),
            cached_master_key: Mutex::new(None),
            data_dir: Mutex::new(data_dir),
            last_auth_time: Mutex::new(None),
            app_in_foreground: Mutex::new(true),
            session_timeout_secs: Mutex::new(initial_timeout),
        }
    }

    /// Sets the session timeout in seconds; `-1` disables time-based expiry.
    #[allow(dead_code)]
    pub fn set_session_timeout(&self, timeout_secs: i64) {
        let mut timeout = self
            .session_timeout_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *timeout = timeout_secs;
    }

    /// Returns the current session timeout setting.
    #[allow(dead_code)]
    pub fn get_session_timeout(&self) -> i64 {
        *self
            .session_timeout_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Returns whether the authenticated UI session is still valid.
    pub fn is_session_valid(&self) -> bool {
        let last_auth = self
            .last_auth_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let in_foreground = *self
            .app_in_foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let timeout_secs = *self
            .session_timeout_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        match *last_auth {
            Some(auth_time) => {
                // Backgrounding invalidates the UI session unless expiry is disabled.
                if !in_foreground && timeout_secs != -1 {
                    return false;
                }
                // A timeout of -1 keeps the foreground session valid indefinitely.
                if timeout_secs == -1 {
                    return true;
                }
                // Enforce elapsed-time expiry for foreground sessions.
                auth_time.elapsed().as_secs() < timeout_secs as u64
            }
            None => false,
        }
    }

    /// Returns whether the user authenticated within the specified window.
    ///
    /// Reserve this for exceptional operations that explicitly require a fresh
    /// proof of user presence. Normal protected application actions should use
    /// `is_session_valid` so an unlocked session remains sufficient.
    pub fn is_recently_authenticated(&self, max_age_secs: u64) -> bool {
        let in_foreground = *self
            .app_in_foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !in_foreground {
            return false;
        }

        self.last_auth_time
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|auth_time| auth_time.elapsed().as_secs() < max_age_secs)
            .unwrap_or(false)
    }

    /// Records a successful authentication time.
    pub fn update_auth_time(&self) {
        let mut last_auth = self
            .last_auth_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *last_auth = Some(std::time::Instant::now());
    }

    /// Invalidates UI access while retaining the master key for background encryption.
    pub fn invalidate_session(&self) {
        let mut last_auth = self
            .last_auth_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *last_auth = None;

        // Clear only the database key used for authenticated UI reads.
        // Retain the master key so background services can continue encrypting new data.
        {
            let mut cached_db = self.cached_db_key.lock().unwrap_or_else(|e| e.into_inner());
            *cached_db = None;
        }
        // Do not clear the master key; background encryption depends on it.
        // {
        //     let mut cached_master = self.cached_master_key.lock().unwrap_or_else(|e| e.into_inner());
        //     *cached_master = None;
        // }
    }

    /// Clears every cached key during shutdown or credential reset.
    pub fn clear_all_cached_keys(&self) {
        {
            let mut cached_db = self.cached_db_key.lock().unwrap_or_else(|e| e.into_inner());
            *cached_db = None;
        }
        {
            let mut cached_master = self
                .cached_master_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cached_master = None;
        }
        {
            let mut cached_pub = self
                .cached_public_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cached_pub = None;
        }
    }

    /// Updates the foreground/background state used by session policy.
    pub fn set_foreground_state(&self, in_foreground: bool) {
        let mut state = self
            .app_in_foreground
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *state = in_foreground;
        // Do not hold app_in_foreground while invalidating the session.
        // is_session_valid acquires last_auth_time before app_in_foreground,
        // so retaining this guard here would create an AB-BA lock inversion.
        drop(state);

        // Moving to the background immediately expires ordinary UI sessions.
        let timeout = *self
            .session_timeout_secs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !in_foreground && timeout != -1 {
            self.invalidate_session();
        }
    }

    pub fn set_data_dir(&self, data_dir: PathBuf) {
        let mut guard = self.data_dir.lock().unwrap_or_else(|e| e.into_inner());
        *guard = data_dir;
    }

    fn file_path(&self, file_name: &str) -> PathBuf {
        self.data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .join(file_name)
    }

    fn master_key_file_path(&self) -> PathBuf {
        self.file_path(MASTER_KEY_FILE_NAME)
    }

    pub fn import_master_key(&self, master_key: &[u8]) -> Result<(), CredentialError> {
        if master_key.len() != MASTER_KEY_LEN {
            return Err(CredentialError::CryptoError(format!(
                "Invalid master key length: {} (expected {})",
                master_key.len(),
                MASTER_KEY_LEN
            )));
        }

        let ciphertext = encrypt_master_key_with_cng(master_key)?;
        let file_data = encode_master_key_file(&ciphertext);
        let key_file = self.master_key_file_path();

        std::fs::write(key_file, file_data).map_err(|e| {
            CredentialError::SystemError(format!("Failed to write master key file: {}", e))
        })?;

        // Update caches
        let mut cached_master = self
            .cached_master_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cached_master = Some(master_key.to_vec());

        let mut cached_db = self.cached_db_key.lock().unwrap_or_else(|e| e.into_inner());
        *cached_db = None; // Force re-derivation

        Ok(())
    }
}

/// Encrypts data with the master key using AES-GCM.
///
/// The result is `nonce (12 bytes) || ciphertext || tag (16 bytes)`.
pub fn encrypt_with_master_key(
    master_key: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CredentialError> {
    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| CredentialError::CryptoError(format!("Failed to create cipher: {}", e)))?;

    // Generate a fresh nonce for every encryption.
    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Authenticate and encrypt the plaintext.
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CredentialError::CryptoError(format!("Encryption failed: {}", e)))?;

    // Prefix the nonce so decryption can reconstruct the AEAD input.
    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);

    Ok(result)
}

/// Decrypts and authenticates data encrypted with the master key.
pub fn decrypt_with_master_key(
    master_key: &[u8],
    encrypted: &[u8],
) -> Result<Vec<u8>, CredentialError> {
    if encrypted.len() < 12 + 16 {
        return Err(CredentialError::CryptoError(
            "Invalid encrypted data".to_string(),
        ));
    }

    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| CredentialError::CryptoError(format!("Failed to create cipher: {}", e)))?;

    // Split the nonce prefix from ciphertext and authentication tag.
    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    // Reject modified data through AES-GCM authentication.
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CredentialError::CryptoError(format!("Decryption failed: {}", e)))
}

/// Encrypts data and encodes it as Base64 for text-only storage fields.
#[allow(dead_code)]
pub fn encrypt_to_base64_with_master_key(
    master_key: &[u8],
    plaintext: &str,
) -> Result<String, CredentialError> {
    let encrypted = encrypt_with_master_key(master_key, plaintext.as_bytes())?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &encrypted,
    ))
}

/// Decodes Base64 and decrypts data with the master key.
#[allow(dead_code)]
pub fn decrypt_from_base64_with_master_key(
    master_key: &[u8],
    encrypted_base64: &str,
) -> Result<String, CredentialError> {
    let encrypted =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_base64)
            .map_err(|e| CredentialError::CryptoError(format!("Invalid base64: {}", e)))?;

    let decrypted = decrypt_with_master_key(master_key, &encrypted)?;

    String::from_utf8(decrypted)
        .map_err(|e| CredentialError::CryptoError(format!("Invalid UTF-8: {}", e)))
}

/// Derives the SQLCipher database key from the master key.
#[allow(dead_code)]
pub fn derive_db_key_from_master(master_key: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(master_key);
    hasher.update(b"CarbonPaper-SQLCipher-Key-v1");
    hasher.finalize().to_vec()
}

/// Derives the high-entropy HMAC key used by the blind search index.
pub fn derive_hmac_key_from_master(master_key: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"CarbonPaper-HMAC-v2-");
    hasher.update(master_key);
    hasher.finalize().to_vec()
}

/// Derives the intentionally weak bootstrap database key from public material.
pub fn derive_db_key_from_public_key(public_key: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    hasher.update(b"CarbonPaper-Weak-DB-Key-v1");
    hasher.finalize().to_vec()
}

/// Formats a database key as a SQLCipher hexadecimal key literal.
#[allow(dead_code)]
pub fn db_key_to_hex(key: &[u8]) -> String {
    format!("x'{}'", hex::encode(key))
}

#[cfg(windows)]
mod windows_impl {
    use super::*;

    /// Exports the CNG RSA public key without invoking protected private-key UI.
    pub fn export_cng_public_key() -> Result<Vec<u8>, CredentialError> {
        use windows::core::HSTRING;
        use windows::Win32::Security::Cryptography::{
            NCryptExportKey, NCryptFreeObject, NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE,
        };

        let key = open_or_create_cng_key()?;

        let blob_type = HSTRING::from("RSAPUBLICBLOB");
        let blob_pcwstr = windows::core::PCWSTR::from_raw(blob_type.as_ptr());

        // Query the output size before allocating the public-key blob.
        let mut out_len: u32 = 0;
        // SAFETY: `key` is a live CNG key handle; the blob type string and output-length
        // pointer remain valid for the synchronous size query, which has no output buffer.
        unsafe {
            NCryptExportKey(
                key,
                NCRYPT_KEY_HANDLE::default(),
                blob_pcwstr,
                None,
                None,
                &mut out_len,
                NCRYPT_FLAGS(0),
            )
        }
        .map_err(|e| {
            // SAFETY: `key` is still owned by this function and has not been freed.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            CredentialError::SystemError(format!("NCryptExportKey size failed: {}", e))
        })?;

        let mut output = vec![0u8; out_len as usize];
        // SAFETY: the output slice owns at least the size reported by CNG and remains
        // live and uniquely mutable for the synchronous export call.
        unsafe {
            NCryptExportKey(
                key,
                NCRYPT_KEY_HANDLE::default(),
                blob_pcwstr,
                None,
                Some(output.as_mut_slice()),
                &mut out_len,
                NCRYPT_FLAGS(0),
            )
        }
        .map_err(|e| {
            // SAFETY: `key` is still owned by this function and has not been freed.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            CredentialError::SystemError(format!("NCryptExportKey failed: {}", e))
        })?;

        // SAFETY: this is the final use of the owned CNG key handle.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        output.truncate(out_len as usize);
        Ok(output)
    }

    /// Encrypt with an exported RSAPUBLICBLOB only.
    ///
    /// This keeps background writes off the protected persisted key handle. The
    /// persisted key has a high-protection UI policy, so even public operations
    /// through that handle can surface Windows security UI on some systems.
    pub fn encrypt_with_exported_public_key(
        public_key: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CredentialError> {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::Security::Cryptography::{
            NCryptEncrypt, NCryptFreeObject, NCryptImportKey, NCryptOpenStorageProvider,
            NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE, NCRYPT_PAD_PKCS1_FLAG,
            NCRYPT_PROV_HANDLE,
        };

        let mut provider = NCRYPT_PROV_HANDLE::default();
        let provider_name = HSTRING::from(CNG_PROVIDER_NAME);
        // SAFETY: `provider_name` is a live NUL-terminated HSTRING and `provider` points
        // to writable handle storage that CNG initializes synchronously.
        unsafe {
            NCryptOpenStorageProvider(&mut provider, PCWSTR::from_raw(provider_name.as_ptr()), 0)
        }
        .map_err(|e| CredentialError::SystemError(format!("Failed to open CNG provider: {}", e)))?;

        let blob_type = HSTRING::from("RSAPUBLICBLOB");
        let mut key = NCRYPT_KEY_HANDLE::default();
        // SAFETY: provider is live, the blob type and public-key slice remain valid, and
        // `key` points to writable storage for the imported handle.
        unsafe {
            NCryptImportKey(
                provider,
                NCRYPT_KEY_HANDLE::default(),
                PCWSTR::from_raw(blob_type.as_ptr()),
                None,
                &mut key,
                public_key,
                NCRYPT_FLAGS(0),
            )
        }
        .map_err(|e| {
            // SAFETY: provider is owned here and import did not transfer that ownership.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
            CredentialError::SystemError(format!("NCryptImportKey public key failed: {}", e))
        })?;

        let mut out_len: u32 = 0;
        // SAFETY: key and plaintext are live for the call; a null output buffer requests
        // only the required ciphertext length.
        unsafe {
            NCryptEncrypt(
                key,
                Some(plaintext),
                None,
                None,
                &mut out_len,
                NCRYPT_PAD_PKCS1_FLAG,
            )
        }
        .map_err(|e| {
            // SAFETY: both handles remain owned here and are freed once on this path.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            // SAFETY: provider remains live and owned after the key is released.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
            CredentialError::SystemError(format!("NCryptEncrypt public size failed: {}", e))
        })?;

        let mut output = vec![0u8; out_len as usize];
        // SAFETY: output is uniquely mutable and at least the size CNG reported; all
        // input slices and handles remain live until the synchronous call returns.
        unsafe {
            NCryptEncrypt(
                key,
                Some(plaintext),
                None,
                Some(output.as_mut_slice()),
                &mut out_len,
                NCRYPT_PAD_PKCS1_FLAG,
            )
        }
        .map_err(|e| {
            // SAFETY: both handles remain owned here and are freed once on this path.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            // SAFETY: provider remains live and owned after the key is released.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
            CredentialError::SystemError(format!("NCryptEncrypt public failed: {}", e))
        })?;

        // SAFETY: these are the final uses of the owned imported key and provider handles.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        // SAFETY: the provider is no longer needed after the imported key is released.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        output.truncate(out_len as usize);
        Ok(output)
    }

    /// Returns the cached exported public key without showing authentication UI.
    pub fn export_or_get_public_key(
        state: &CredentialManagerState,
    ) -> Result<Vec<u8>, CredentialError> {
        // Prefer the in-memory public key cache.
        {
            let cached = state
                .cached_public_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(ref key) = *cached {
                return Ok(key.clone());
            }
        }

        // Export from CNG only on a cache miss.
        let public_key = export_cng_public_key()?;

        // Cache the immutable public blob for background encryption.
        {
            let mut cached = state
                .cached_public_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cached = Some(public_key.clone());
        }

        Ok(public_key)
    }

    /// Forces user verification and unlocks the master key.
    ///
    /// Cold start decrypts in the main process so one prompt establishes the CNG PIN
    /// cache used by later row-key reads. Re-unlocking an already cached master key uses
    /// a short-lived child process with no PIN cache, forcing fresh user verification
    /// without disrupting the main process cache used for subsequent reads.
    pub fn force_verify_and_unlock_master_key(
        state: &CredentialManagerState,
        owner_hwnd: Option<isize>,
    ) -> Result<Vec<u8>, CredentialError> {
        let key_file = state.master_key_file_path();
        if !key_file.exists() {
            return Err(CredentialError::KeyNotFound);
        }

        let already_cached = get_cached_master_key(state).is_some();

        let master_key = if already_cached {
            // A child process bypasses the main process's existing CNG PIN cache.
            verify_via_subprocess(&key_file, owner_hwnd)?
        } else {
            // Cold start keeps the new CNG PIN cache in the main process.
            let file_data = std::fs::read(&key_file).map_err(|e| {
                CredentialError::SystemError(format!("Failed to read master key file: {}", e))
            })?;
            let ciphertext = decode_master_key_file(&file_data)?;
            decrypt_master_key_with_cng_for_window(&ciphertext, owner_hwnd)?
        };

        {
            let mut cached = state
                .cached_master_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cached = Some(master_key.clone());
        }

        Ok(master_key)
    }

    /// Runs CNG decryption in a child process to bypass the main process PIN cache.
    fn verify_via_subprocess(
        key_file: &std::path::Path,
        owner_hwnd: Option<isize>,
    ) -> Result<Vec<u8>, CredentialError> {
        let exe_path = std::env::current_exe().map_err(|e| {
            CredentialError::SystemError(format!("Failed to get current exe: {}", e))
        })?;

        let mut command = std::process::Command::new(&exe_path);
        command.arg("--cng-unlock").arg(key_file);
        if let Some(hwnd) = owner_hwnd {
            command.arg(hwnd.to_string());
        }
        let output = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                CredentialError::SystemError(format!("Failed to spawn CNG unlock process: {}", e))
            })?
            .wait_with_output()
            .map_err(|e| {
                CredentialError::SystemError(format!(
                    "Failed to wait for CNG unlock process: {}",
                    e
                ))
            })?;

        match output.status.code() {
            Some(0) => {
                let hex_str = String::from_utf8_lossy(&output.stdout);
                let master_key = hex::decode(hex_str.trim()).map_err(|e| {
                    CredentialError::CryptoError(format!("Invalid hex from subprocess: {}", e))
                })?;

                if master_key.len() != MASTER_KEY_LEN {
                    return Err(CredentialError::CryptoError(format!(
                        "Unexpected master key length: {} (expected {})",
                        master_key.len(),
                        MASTER_KEY_LEN
                    )));
                }

                Ok(master_key)
            }
            Some(2) => Err(CredentialError::UserCancelled),
            _ => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(CredentialError::SystemError(format!(
                    "CNG unlock subprocess failed: {}",
                    stderr.trim()
                )))
            }
        }
    }

    /// Unlocks and caches the master key through Windows Hello/CNG verification.
    #[allow(dead_code)]
    pub async fn unlock_master_key(
        state: &CredentialManagerState,
    ) -> Result<Vec<u8>, CredentialError> {
        if let Some(key) = get_cached_master_key(state) {
            return Ok(key);
        }

        let key_file = state.master_key_file_path();
        if !key_file.exists() {
            return Err(CredentialError::KeyNotFound);
        }

        let file_data = std::fs::read(&key_file).map_err(|e| {
            CredentialError::SystemError(format!("Failed to read master key file: {}", e))
        })?;

        let ciphertext = decode_master_key_file(&file_data)?;
        let master_key = decrypt_master_key_with_cng(&ciphertext)?;

        {
            let mut cached = state
                .cached_master_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cached = Some(master_key.clone());
        }

        Ok(master_key)
    }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub fn export_or_get_public_key(
    _state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError(
        "CNG is only available on Windows".to_string(),
    ))
}

#[cfg(not(windows))]
pub fn encrypt_with_exported_public_key(
    _public_key: &[u8],
    _plaintext: &[u8],
) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError(
        "CNG is only available on Windows".to_string(),
    ))
}

/// Creates the master key on first use without invoking Windows Hello.
///
/// Existing cached or persisted keys are left untouched. This bootstrap path prevents
/// `credential_initialize` from prompting twice during a cold start.
#[cfg(windows)]
pub fn ensure_master_key_created(state: &CredentialManagerState) -> Result<(), CredentialError> {
    // An existing cached master key needs no bootstrap work.
    if get_cached_master_key(state).is_some() {
        return Ok(());
    }

    let key_file = state.master_key_file_path();
    if key_file.exists() {
        // A persisted key will be unlocked later by `credential_verify_user`.
        return Ok(());
    }

    // First use: generate a master key and wrap it with the public key without UI.
    let mut master_key = vec![0u8; MASTER_KEY_LEN];
    rand::thread_rng().fill_bytes(&mut master_key);

    let ciphertext = encrypt_master_key_with_cng(&master_key)?;
    let file_data = encode_master_key_file(&ciphertext);

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CredentialError::SystemError(format!("Failed to create directory: {}", e))
        })?;
    }

    std::fs::write(&key_file, file_data)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save master key: {}", e)))?;

    {
        let mut cached = state
            .cached_master_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cached = Some(master_key.clone());
    }

    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
pub async fn ensure_master_key_ready(
    state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_master_key(state) {
        return Ok(key);
    }

    let key_file = state.master_key_file_path();
    if key_file.exists() {
        let master_key = unlock_master_key(state).await?;
        return Ok(master_key);
    }

    // Generate a new master key and wrap it with the exported CNG public key.
    let mut master_key = vec![0u8; MASTER_KEY_LEN];
    rand::thread_rng().fill_bytes(&mut master_key);

    let ciphertext = encrypt_master_key_with_cng(&master_key)?;
    let file_data = encode_master_key_file(&ciphertext);

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CredentialError::SystemError(format!("Failed to create directory: {}", e))
        })?;
    }

    std::fs::write(&key_file, file_data)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save master key: {}", e)))?;

    {
        let mut cached = state
            .cached_master_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cached = Some(master_key.clone());
    }

    Ok(master_key)
}

/// Returns whether protected UI access requires authentication.
#[allow(dead_code)]
fn ensure_session_valid(state: &CredentialManagerState) -> Result<(), CredentialError> {
    if !state.is_session_valid() {
        return Err(CredentialError::AuthRequired);
    }
    Ok(())
}

/// Returns a copy of the cached master key, if unlocked.
pub fn get_cached_master_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state
        .cached_master_key
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Returns a copy of the cached database key, if authenticated.
#[allow(dead_code)]
pub fn get_cached_db_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state
        .cached_db_key
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Returns a copy of the cached public key, if loaded.
pub fn get_cached_public_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state
        .cached_public_key
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Returns or derives the database key synchronously during storage initialization.
#[allow(dead_code)]
pub fn get_or_create_db_key_sync(
    state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    // Prefer the authenticated database-key cache.
    if let Some(key) = get_cached_db_key(state) {
        return Ok(key);
    }

    // Deriving a read key requires a valid UI session.
    ensure_session_valid(state)?;

    // The master key must already be unlocked and cached.
    let master_key = get_or_create_master_key_sync(state)?;
    let db_key = derive_db_key_from_master(&master_key);

    // Cache the derived database key for the authenticated session.
    {
        let mut cached_db = state
            .cached_db_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cached_db = Some(db_key.clone());
    }

    Ok(db_key)
}

/// Returns the cached master key or reports that authentication is required.
#[allow(dead_code)]
pub fn get_or_create_master_key_sync(
    state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_master_key(state) {
        return Ok(key);
    }

    // Refuse private-key access until the user explicitly unlocks the session.
    Err(CredentialError::AuthRequired)
}

#[cfg(windows)]
fn open_or_create_cng_key(
) -> Result<windows::Win32::Security::Cryptography::NCRYPT_KEY_HANDLE, CredentialError> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Security::Cryptography::{
        NCryptCreatePersistedKey, NCryptFinalizeKey, NCryptFreeObject, NCryptOpenKey,
        NCryptOpenStorageProvider, NCryptSetProperty, CERT_KEY_SPEC, NCRYPT_FLAGS, NCRYPT_HANDLE,
        NCRYPT_KEY_HANDLE, NCRYPT_OVERWRITE_KEY_FLAG, NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM,
        NCRYPT_UI_FORCE_HIGH_PROTECTION_FLAG, NCRYPT_UI_POLICY,
    };

    // CNG property names.
    const LENGTH_PROP: &str = "Length";
    const UI_POLICY_PROP: &str = "UI Policy";

    // Open the Microsoft Software Key Storage Provider.
    let mut provider = NCRYPT_PROV_HANDLE::default();
    let provider_name = HSTRING::from(CNG_PROVIDER_NAME);
    let provider_pcwstr = PCWSTR::from_raw(provider_name.as_ptr());
    // SAFETY: the provider name is a live NUL-terminated HSTRING and `provider` points
    // to writable storage initialized synchronously by CNG.
    unsafe { NCryptOpenStorageProvider(&mut provider, provider_pcwstr, 0) }
        .map_err(|e| CredentialError::SystemError(format!("Failed to open CNG provider: {}", e)))?;

    // Reuse the persisted key when it already exists.
    let mut key = NCRYPT_KEY_HANDLE::default();
    let key_name = HSTRING::from(CNG_KEY_NAME);
    let key_pcwstr = PCWSTR::from_raw(key_name.as_ptr());
    // SAFETY: provider and key-name storage remain live; `key` is valid writable output
    // storage and CNG does not retain any Rust pointer.
    let open_result = unsafe {
        NCryptOpenKey(
            provider,
            &mut key,
            key_pcwstr,
            CERT_KEY_SPEC(0),
            NCRYPT_FLAGS(0),
        )
    };

    if open_result.is_ok() {
        // SAFETY: the provider handle is owned here; the opened key remains independently
        // valid after the provider reference is released.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        return Ok(key);
    }

    // Create a new persisted key when the lookup failed.
    let mut new_key = NCRYPT_KEY_HANDLE::default();
    // SAFETY: provider and algorithm/key-name strings are live, and `new_key` is writable
    // handle storage. Ownership of the returned key remains with this function.
    unsafe {
        NCryptCreatePersistedKey(
            provider,
            &mut new_key,
            NCRYPT_RSA_ALGORITHM,
            key_pcwstr,
            CERT_KEY_SPEC(0),
            NCRYPT_OVERWRITE_KEY_FLAG,
        )
    }
    .map_err(|e| {
        // SAFETY: provider is still owned here because key creation failed.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to create CNG key: {}", e))
    })?;

    // Configure a 2048-bit RSA key before finalization.
    let key_length: u32 = 2048;
    let length_name = HSTRING::from(LENGTH_PROP);
    let length_pcwstr = PCWSTR::from_raw(length_name.as_ptr());
    // SAFETY: `new_key` is an unfinalized live key, the property name is live, and the
    // four-byte property value slice has the documented representation.
    unsafe {
        NCryptSetProperty(
            new_key,
            length_pcwstr,
            &key_length.to_le_bytes(),
            NCRYPT_FLAGS(0),
        )
    }
    .map_err(|e| {
        // SAFETY: both handles remain owned here and are released once on this path.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
        // SAFETY: provider remains owned after releasing the failed key.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to set key length: {}", e))
    })?;

    // Force high-protection UI so private-key operations require OS-mediated consent.
    let ui_policy = NCRYPT_UI_POLICY {
        dwVersion: 1,
        dwFlags: NCRYPT_UI_FORCE_HIGH_PROTECTION_FLAG,
        pszCreationTitle: PCWSTR::null(),
        pszFriendlyName: PCWSTR::null(),
        pszDescription: PCWSTR::null(),
    };

    let policy_name = HSTRING::from(UI_POLICY_PROP);
    let policy_pcwstr = PCWSTR::from_raw(policy_name.as_ptr());
    // SAFETY: `ui_policy` is a fully initialized plain C-layout structure; the byte slice
    // covers exactly that live value and is used only during the following synchronous call.
    let policy_bytes = unsafe {
        std::slice::from_raw_parts(
            &ui_policy as *const NCRYPT_UI_POLICY as *const u8,
            std::mem::size_of::<NCRYPT_UI_POLICY>(),
        )
    };

    // SAFETY: the key, property name, and policy byte slice are valid and live for the
    // synchronous property update.
    unsafe { NCryptSetProperty(new_key, policy_pcwstr, policy_bytes, NCRYPT_FLAGS(0)) }.map_err(
        |e| {
            // SAFETY: both handles remain owned here and are released once on this path.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
            // SAFETY: provider remains owned after releasing the failed key.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
            CredentialError::SystemError(format!("Failed to set UI policy: {}", e))
        },
    )?;

    // Finalize only after the length and UI policy are installed.
    // SAFETY: `new_key` is a live unfinalized key configured by the calls above.
    unsafe { NCryptFinalizeKey(new_key, NCRYPT_FLAGS(0)) }.map_err(|e| {
        // SAFETY: both handles remain owned here and are released once on this path.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
        // SAFETY: provider remains owned after releasing the failed key.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to finalize CNG key: {}", e))
    })?;

    // SAFETY: provider ownership ends here; the finalized persisted key handle remains valid.
    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
    Ok(new_key)
}

#[cfg(windows)]
fn encrypt_master_key_with_cng(master_key: &[u8]) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Security::Cryptography::{
        NCryptEncrypt, NCryptFreeObject, NCRYPT_HANDLE, NCRYPT_PAD_PKCS1_FLAG,
    };

    let key = open_or_create_cng_key()?;

    // Use PKCS#1 v1.5 padding for compatibility and query the output size first.
    let mut out_len: u32 = 0;
    // SAFETY: key and master-key input are live; the null output buffer requests only
    // the required ciphertext length.
    unsafe {
        NCryptEncrypt(
            key,
            Some(master_key),
            None,
            None,
            &mut out_len,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .map_err(|e| {
        // SAFETY: key ownership remains local when encryption fails.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptEncrypt size failed: {}", e))
    })?;

    let mut output = vec![0u8; out_len as usize];
    // SAFETY: output is uniquely mutable and sized from CNG's query; key and input remain
    // live for the synchronous encryption call.
    unsafe {
        NCryptEncrypt(
            key,
            Some(master_key),
            None,
            Some(output.as_mut_slice()),
            &mut out_len,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .map_err(|e| {
        // SAFETY: key ownership remains local when encryption fails.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptEncrypt failed: {}", e))
    })?;

    // SAFETY: this is the final use of the owned key handle.
    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
    output.truncate(out_len as usize);
    Ok(output)
}

#[cfg(windows)]
pub fn decrypt_master_key_with_cng(ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    decrypt_master_key_with_cng_for_window(ciphertext, None)
}

#[cfg(windows)]
pub fn decrypt_master_key_with_cng_for_window(
    ciphertext: &[u8],
    owner_hwnd: Option<isize>,
) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Security::Cryptography::NCRYPT_PAD_PKCS1_FLAG;

    decrypt_master_key_with_cng_flags(ciphertext, NCRYPT_PAD_PKCS1_FLAG, owner_hwnd)
}

#[cfg(windows)]
fn decrypt_master_key_with_cng_flags(
    ciphertext: &[u8],
    flags: windows::Win32::Security::Cryptography::NCRYPT_FLAGS,
    owner_hwnd: Option<isize>,
) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Foundation::NTE_SILENT_CONTEXT;
    use windows::Win32::Security::Cryptography::{NCryptDecrypt, NCryptFreeObject, NCRYPT_HANDLE};

    let key = open_or_create_cng_key()?;
    if let Some(hwnd) = owner_hwnd {
        use windows::Win32::Security::Cryptography::{
            NCryptSetProperty, NCRYPT_WINDOW_HANDLE_PROPERTY,
        };
        let hwnd_bytes = (hwnd as usize).to_ne_bytes();
        // SAFETY: key is live and `hwnd_bytes` has the native pointer width expected by
        // `NCRYPT_WINDOW_HANDLE_PROPERTY`; the slice is used synchronously.
        unsafe {
            NCryptSetProperty(
                key,
                NCRYPT_WINDOW_HANDLE_PROPERTY,
                &hwnd_bytes,
                windows::Win32::Security::Cryptography::NCRYPT_FLAGS(0),
            )
        }
        .map_err(|e| {
            // SAFETY: key ownership remains local when setting the property fails.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            CredentialError::SystemError(format!("Failed to set CNG owner window: {}", e))
        })?;
    }

    // Query plaintext size before allocating the output buffer.
    let mut out_len: u32 = 0;
    // SAFETY: key and ciphertext are live; the null output buffer requests only the
    // plaintext length and `out_len` points to writable stack storage.
    unsafe { NCryptDecrypt(key, Some(ciphertext), None, None, &mut out_len, flags) }.map_err(
        |e| {
            // SAFETY: key ownership remains local when decryption fails.
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
            if e.code() == NTE_SILENT_CONTEXT {
                CredentialError::AuthRequired
            } else {
                CredentialError::SystemError(format!("NCryptDecrypt size failed: {}", e))
            }
        },
    )?;

    let mut output = vec![0u8; out_len as usize];
    // SAFETY: output is uniquely mutable and sized from CNG's query; all handles and
    // input/output slices remain live until the synchronous call returns.
    unsafe {
        NCryptDecrypt(
            key,
            Some(ciphertext),
            None,
            Some(output.as_mut_slice()),
            &mut out_len,
            flags,
        )
    }
    .map_err(|e| {
        // SAFETY: key ownership remains local when decryption fails.
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        if e.code() == NTE_SILENT_CONTEXT {
            CredentialError::AuthRequired
        } else {
            CredentialError::SystemError(format!("NCryptDecrypt failed: {}", e))
        }
    })?;

    // SAFETY: this is the final use of the owned key handle.
    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
    output.truncate(out_len as usize);
    Ok(output)
}

/// Unwraps a row key with the protected CNG private key, allowing OS UI.
pub fn decrypt_row_key_with_cng(ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    decrypt_master_key_with_cng(ciphertext)
}

/// Silently unwraps a row key with CNG and never displays authentication UI.
///
/// Returns [`CredentialError::AuthRequired`] when user interaction would be needed so
/// background callers can wait for an explicit unlock.
#[cfg(windows)]
pub fn decrypt_row_key_with_cng_silent(ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Security::Cryptography::{NCRYPT_PAD_PKCS1_FLAG, NCRYPT_SILENT_FLAG};

    decrypt_master_key_with_cng_flags(ciphertext, NCRYPT_PAD_PKCS1_FLAG | NCRYPT_SILENT_FLAG, None)
}

fn encode_master_key_file(ciphertext: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(MASTER_KEY_FILE_MAGIC.len() + ciphertext.len());
    data.extend_from_slice(MASTER_KEY_FILE_MAGIC);
    data.extend_from_slice(ciphertext);
    data
}

pub fn decode_master_key_file(data: &[u8]) -> Result<Vec<u8>, CredentialError> {
    if data.len() <= MASTER_KEY_FILE_MAGIC.len() {
        return Err(CredentialError::CryptoError(
            "Invalid master key file".to_string(),
        ));
    }

    if &data[..MASTER_KEY_FILE_MAGIC.len()] != MASTER_KEY_FILE_MAGIC {
        return Err(CredentialError::CryptoError(
            "Invalid master key file magic".to_string(),
        ));
    }

    Ok(data[MASTER_KEY_FILE_MAGIC.len()..].to_vec())
}

/// Loads and caches the public key file without user interaction.
pub fn load_public_key_from_file(
    state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_public_key(state) {
        return Ok(key);
    }

    let key_file = state.file_path("credential_public_key.bin");
    if !key_file.exists() {
        return Err(CredentialError::KeyNotFound);
    }

    let public_key = std::fs::read(&key_file)
        .map_err(|e| CredentialError::SystemError(format!("Failed to read public key: {}", e)))?;

    {
        let mut cached = state
            .cached_public_key
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *cached = Some(public_key.clone());
    }

    Ok(public_key)
}

/// Persists the public key for future unattended encryption.
pub fn save_public_key_to_file(
    state: &CredentialManagerState,
    public_key: &[u8],
) -> Result<(), CredentialError> {
    let key_file = state.file_path("credential_public_key.bin");

    // Create the parent directory before atomically writing the public key.
    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CredentialError::SystemError(format!("Failed to create directory: {}", e))
        })?;
    }

    std::fs::write(&key_file, public_key)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save public key: {}", e)))?;

    Ok(())
}

#[cfg(not(windows))]
pub fn decrypt_row_key_with_cng(_ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError(
        "CNG is only available on Windows".to_string(),
    ))
}

#[cfg(not(windows))]
pub fn decrypt_row_key_with_cng_silent(_ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError(
        "CNG is only available on Windows".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locking_ui_session_preserves_background_master_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = CredentialManagerState::new(temp.path().to_path_buf());
        *state
            .cached_master_key
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(vec![9u8; MASTER_KEY_LEN]);
        state.update_auth_time();

        state.invalidate_session();

        assert!(!state.is_session_valid());
        assert_eq!(
            get_cached_master_key(&state),
            Some(vec![9u8; MASTER_KEY_LEN])
        );
    }

    #[test]
    fn test_encrypt_decrypt() {
        let public_key = b"12345678901234567890123456789012";
        let plaintext = b"Hello, World!";

        let encrypted = encrypt_with_master_key(public_key, plaintext).unwrap();
        let decrypted = decrypt_with_master_key(public_key, &encrypted).unwrap();

        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_base64_encrypt_decrypt() {
        let public_key = b"12345678901234567890123456789012";
        let plaintext = "测试中文文本";

        let encrypted = encrypt_to_base64_with_master_key(public_key, plaintext).unwrap();
        let decrypted = decrypt_from_base64_with_master_key(public_key, &encrypted).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn test_db_key_generation() {
        let public_key = b"12345678901234567890123456789012";
        let db_key = derive_db_key_from_master(public_key);

        assert_eq!(db_key.len(), 32); // SHA-256 outputs 32 bytes

        let hex_key = db_key_to_hex(&db_key);
        assert!(hex_key.starts_with("x'"));
        assert!(hex_key.ends_with("'"));
    }

    #[test]
    fn test_decrypt_invalid_data() {
        let key = b"12345678901234567890123456789012";
        // 5 bytes is too short (need at least 12 nonce + 16 tag = 28 bytes)
        let corrupt_data: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42];
        let result = decrypt_with_master_key(key, &corrupt_data);
        assert!(
            result.is_err(),
            "Decrypting corrupt/short ciphertext should return an error"
        );
    }

    #[test]
    fn test_encrypt_decrypt_empty() {
        let key = b"12345678901234567890123456789012";
        let plaintext: &[u8] = b"";
        let encrypted = encrypt_with_master_key(key, plaintext).unwrap();
        let decrypted = decrypt_with_master_key(key, &encrypted).unwrap();
        assert_eq!(
            decrypted,
            Vec::<u8>::new(),
            "Round-trip of empty plaintext should produce empty Vec"
        );
    }

    #[test]
    fn test_db_key_deterministic() {
        let master_key = b"some-master-key-for-testing-1234";
        let key1 = derive_db_key_from_master(master_key);
        let key2 = derive_db_key_from_master(master_key);
        assert_eq!(
            key1, key2,
            "derive_db_key_from_master should be deterministic"
        );
    }

    #[test]
    fn test_db_key_to_hex_format() {
        let key = b"12345678901234567890123456789012";
        let db_key = derive_db_key_from_master(key);
        let hex_str = db_key_to_hex(&db_key);
        assert!(hex_str.starts_with("x'"), "hex key should start with x'");
        assert!(hex_str.ends_with("'"), "hex key should end with '");
        // db_key is 32 bytes = 64 hex chars, plus "x'" prefix and "'" suffix = 67 chars total
        assert_eq!(
            hex_str.len(),
            67,
            "hex key should be x' + 64 hex chars + ' = 67 chars"
        );
    }
}
