//! 凭证管理模块 - 使用 CNG (NCrypt) API 实现安全密钥管理
//!
//! 安全模型：
//! - 加密阶段（使用公钥）：无感，后台服务可随时执行
//! - 解密阶段（使用私钥）：受 OS 强制 UI 保护，需用户 PIN 认证
//!
//! 实现原理：
//! 1. 使用 Microsoft Software Key Storage Provider 创建 RSA 密钥对
//! 2. 设置 NCRYPT_UI_POLICY 强制高保护级别
//! 3. 加密使用公钥（NCryptEncrypt 不触发 UI）
//! 4. 解密使用私钥（NCryptDecrypt 触发系统级安全对话框）

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(windows)]
#[allow(unused_imports)]
use windows::{
    core::HSTRING,
    Security::Credentials::{
        KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialRetrievalResult,
        KeyCredentialStatus,
    },
    Security::Cryptography::Core::CryptographicPublicKeyBlobType,
    Security::Cryptography::CryptographicBuffer,
    Storage::Streams::IBuffer,
};

/// 凭证管理器错误类型
#[derive(Debug)]
#[allow(dead_code)]
pub enum CredentialError {
    /// Windows Hello 不可用
    WindowsHelloNotAvailable,
    /// 用户取消了认证
    UserCancelled,
    /// 密钥不存在
    KeyNotFound,
    /// 需要用户认证
    AuthRequired,
    /// 加密/解密失败
    CryptoError(String),
    /// 系统错误
    SystemError(String),
    /// 密钥已存在
    KeyAlreadyExists,
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WindowsHelloNotAvailable => write!(f, "Windows Hello is not available"),
            Self::UserCancelled => write!(f, "User cancelled authentication"),
            Self::KeyNotFound => write!(f, "Credential key not found"),
            Self::AuthRequired => write!(f, "Authentication required"),
            Self::CryptoError(msg) => write!(f, "Crypto error: {}", msg),
            Self::SystemError(msg) => write!(f, "System error: {}", msg),
            Self::KeyAlreadyExists => write!(f, "Credential key already exists"),
        }
    }
}

impl std::error::Error for CredentialError {}

/// 默认认证会话超时时间（秒）
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 15 * 60; // 15 分钟
const MASTER_KEY_FILE_NAME: &str = "credential_master_key.bin";
const MASTER_KEY_LEN: usize = 32;
const MASTER_KEY_FILE_MAGIC: &[u8; 5] = b"CPMK3"; // 版本升级
const CNG_KEY_NAME: &str = "CarbonPaperMasterKeyV3";
// 使用 Software KSP，它支持 RSA 加密和 UI Policy
const CNG_PROVIDER_NAME: &str = "Microsoft Software Key Storage Provider";

/// 凭证管理器状态
pub struct CredentialManagerState {
    /// 应用名称（用于 KeyCredentialManager）
    app_name: String,
    /// 缓存的加密密钥（用于 SQLCipher）
    cached_db_key: Mutex<Option<Vec<u8>>>,
    /// 缓存的公钥（用于加密新数据）
    cached_public_key: Mutex<Option<Vec<u8>>>,
    /// 缓存的主密钥（用于数据加密）
    cached_master_key: Mutex<Option<Vec<u8>>>,
    /// 数据目录路径
    data_dir: PathBuf,
    /// 上次认证成功的时间戳（用于会话超时）
    last_auth_time: Mutex<Option<std::time::Instant>>,
    /// 应用是否在前台（用于会话管理）
    app_in_foreground: Mutex<bool>,
    /// 会话超时时间（秒），-1 表示永不超时
    session_timeout_secs: Mutex<i64>,
}

impl CredentialManagerState {
    pub fn new(app_name: &str, data_dir: PathBuf) -> Self {
        // 默认值
        let default = DEFAULT_SESSION_TIMEOUT_SECS as i64;
        let mut initial_timeout = default;

        // 尝试从注册表读取持久化的超时（以秒为单位）
        if let Some(s) = crate::registry_config::get_string("session_timeout_secs") {
            if let Ok(parsed) = s.parse::<i64>() {
                initial_timeout = parsed;
            } else {
                eprintln!("[credential_manager] Failed to parse session_timeout_secs from registry: {}", s);
            }
        }

        Self {
            app_name: app_name.to_string(),
            cached_db_key: Mutex::new(None),
            cached_public_key: Mutex::new(None),
            cached_master_key: Mutex::new(None),
            data_dir,
            last_auth_time: Mutex::new(None),
            app_in_foreground: Mutex::new(true),
            session_timeout_secs: Mutex::new(initial_timeout),
        }
    }
    
    /// 设置会话超时时间（秒），-1 表示永不超时
    #[allow(dead_code)]
    pub fn set_session_timeout(&self, timeout_secs: i64) {
        let mut timeout = self.session_timeout_secs.lock().unwrap();
        *timeout = timeout_secs;
    }
    
    /// 获取当前会话超时时间设置
    #[allow(dead_code)]
    pub fn get_session_timeout(&self) -> i64 {
        *self.session_timeout_secs.lock().unwrap()
    }
    
    /// 检查当前认证会话是否有效
    pub fn is_session_valid(&self) -> bool {
        let last_auth = self.last_auth_time.lock().unwrap();
        let in_foreground = *self.app_in_foreground.lock().unwrap();
        let timeout_secs = *self.session_timeout_secs.lock().unwrap();
        
        match *last_auth {
            Some(auth_time) => {
                // 如果应用在后台，会话立即失效（除非设置为永不超时）
                if !in_foreground && timeout_secs != -1 {
                    return false;
                }
                // 如果超时设置为 -1，永不超时
                if timeout_secs == -1 {
                    return true;
                }
                // 检查是否超时
                auth_time.elapsed().as_secs() < timeout_secs as u64
            }
            None => false,
        }
    }
    
    /// 更新认证时间戳
    pub fn update_auth_time(&self) {
        let mut last_auth = self.last_auth_time.lock().unwrap();
        *last_auth = Some(std::time::Instant::now());
    }
    
    /// 清除认证会话（锁定 UI 访问权限）
    /// 注意：不清除 cached_master_key，以允许后台继续加密数据
    pub fn invalidate_session(&self) {
        let mut last_auth = self.last_auth_time.lock().unwrap();
        *last_auth = None;

        // 只清除 db_key 缓存（用于 UI 层的数据库访问）
        // 保留 master_key 缓存，允许后台服务继续加密新数据
        {
            let mut cached_db = self.cached_db_key.lock().unwrap();
            *cached_db = None;
        }
        // 不清除 master_key - 后台加密需要它
        // {
        //     let mut cached_master = self.cached_master_key.lock().unwrap();
        //     *cached_master = None;
        // }
    }
    
    /// 完全清除所有缓存（应用退出或重置时调用）
    pub fn clear_all_cached_keys(&self) {
        {
            let mut cached_db = self.cached_db_key.lock().unwrap();
            *cached_db = None;
        }
        {
            let mut cached_master = self.cached_master_key.lock().unwrap();
            *cached_master = None;
        }
        {
            let mut cached_pub = self.cached_public_key.lock().unwrap();
            *cached_pub = None;
        }
    }
    
    /// 设置应用前台/后台状态
    pub fn set_foreground_state(&self, in_foreground: bool) {
        let mut state = self.app_in_foreground.lock().unwrap();
        *state = in_foreground;
        
        // 如果进入后台，立即使会话失效（除非设置为永不超时）
        let timeout = *self.session_timeout_secs.lock().unwrap();
        if !in_foreground && timeout != -1 {
            self.invalidate_session();
        }
    }
    
    /// 获取应用名称
    #[allow(dead_code)]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

/// 使用主密钥加密数据（AES-GCM）
/// 返回格式: nonce(12字节) + 密文 + tag(16字节)
pub fn encrypt_with_master_key(master_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| CredentialError::CryptoError(format!("Failed to create cipher: {}", e)))?;
    
    // 生成随机 nonce
    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    // 加密数据
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CredentialError::CryptoError(format!("Encryption failed: {}", e)))?;
    
    // 组合结果: nonce + ciphertext
    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    
    Ok(result)
}

/// 使用主密钥解密数据
pub fn decrypt_with_master_key(master_key: &[u8], encrypted: &[u8]) -> Result<Vec<u8>, CredentialError> {
    if encrypted.len() < 12 + 16 {
        return Err(CredentialError::CryptoError("Invalid encrypted data".to_string()));
    }

    let cipher = Aes256Gcm::new_from_slice(master_key)
        .map_err(|e| CredentialError::CryptoError(format!("Failed to create cipher: {}", e)))?;
    
    // 提取 nonce 和密文
    let nonce = Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];
    
    // 解密数据
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CredentialError::CryptoError(format!("Decryption failed: {}", e)))
}

/// 将加密数据编码为 Base64（用于存储在 ChromaDB 等文本字段）
#[allow(dead_code)]
pub fn encrypt_to_base64_with_master_key(master_key: &[u8], plaintext: &str) -> Result<String, CredentialError> {
    let encrypted = encrypt_with_master_key(master_key, plaintext.as_bytes())?;
    Ok(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encrypted))
}

/// 从 Base64 解密数据
#[allow(dead_code)]
pub fn decrypt_from_base64_with_master_key(master_key: &[u8], encrypted_base64: &str) -> Result<String, CredentialError> {
    let encrypted = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted_base64)
        .map_err(|e| CredentialError::CryptoError(format!("Invalid base64: {}", e)))?;

    let decrypted = decrypt_with_master_key(master_key, &encrypted)?;
    
    String::from_utf8(decrypted)
        .map_err(|e| CredentialError::CryptoError(format!("Invalid UTF-8: {}", e)))
}

/// 生成用于 SQLCipher 的数据库密钥
pub fn derive_db_key_from_master(master_key: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(master_key);
    hasher.update(b"CarbonPaper-SQLCipher-Key-v1");
    hasher.finalize().to_vec()
}

/// 使用公钥派生弱数据库密钥（不需要用户认证）
pub fn derive_db_key_from_public_key(public_key: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(public_key);
    hasher.update(b"CarbonPaper-Weak-DB-Key-v1");
    hasher.finalize().to_vec()
}

/// 将数据库密钥转换为 SQLCipher 可用的十六进制格式
#[allow(dead_code)]
pub fn db_key_to_hex(key: &[u8]) -> String {
    format!("x'{}'", hex::encode(key))
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    
    /// 检查 Windows Hello 是否可用
    pub async fn is_windows_hello_available() -> Result<bool, CredentialError> {
        let result = KeyCredentialManager::IsSupportedAsync()
            .map_err(|e| CredentialError::SystemError(format!("Failed to check Windows Hello: {}", e)))?
            .get()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get result: {}", e)))?;
        
        Ok(result)
    }
    
    /// 创建或获取凭证密钥
    /// 如果密钥已存在，返回现有密钥；否则创建新密钥
    pub async fn create_or_get_credential(
        state: &CredentialManagerState,
    ) -> Result<Vec<u8>, CredentialError> {
        // 首先检查是否有缓存的公钥
        {
            let cached = state.cached_public_key.lock().unwrap();
            if let Some(ref key) = *cached {
                return Ok(key.clone());
            }
        }
        
        // 检查 Windows Hello 是否可用
        if !is_windows_hello_available().await? {
            return Err(CredentialError::WindowsHelloNotAvailable);
        }
        
        let app_name = HSTRING::from(&state.app_name);
        
        // 尝试获取现有密钥
        let result: KeyCredentialRetrievalResult = KeyCredentialManager::OpenAsync(&app_name)
            .map_err(|e| CredentialError::SystemError(format!("Failed to open credential: {}", e)))?
            .get()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get credential: {}", e)))?;
        
        let status = result.Status()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get status: {}", e)))?;
        
        let credential = match status {
            KeyCredentialStatus::Success => {
                result.Credential()
                    .map_err(|e| CredentialError::SystemError(format!("Failed to get credential object: {}", e)))?
            }
            KeyCredentialStatus::NotFound => {
                // 密钥不存在，创建新密钥
                let create_result = KeyCredentialManager::RequestCreateAsync(
                    &app_name,
                    KeyCredentialCreationOption::ReplaceExisting,
                )
                .map_err(|e| CredentialError::SystemError(format!("Failed to create credential: {}", e)))?
                .get()
                .map_err(|e| CredentialError::SystemError(format!("Failed to get create result: {}", e)))?;
                
                let create_status = create_result.Status()
                    .map_err(|e| CredentialError::SystemError(format!("Failed to get create status: {}", e)))?;
                
                match create_status {
                    KeyCredentialStatus::Success => {
                        create_result.Credential()
                            .map_err(|e| CredentialError::SystemError(format!("Failed to get new credential: {}", e)))?
                    }
                    KeyCredentialStatus::UserCanceled => {
                        return Err(CredentialError::UserCancelled);
                    }
                    _ => {
                        return Err(CredentialError::SystemError(format!(
                            "Failed to create credential: {:?}",
                            create_status
                        )));
                    }
                }
            }
            KeyCredentialStatus::UserCanceled => {
                return Err(CredentialError::UserCancelled);
            }
            _ => {
                return Err(CredentialError::SystemError(format!(
                    "Unexpected credential status: {:?}",
                    status
                )));
            }
        };
        
        // 获取公钥
        let public_key_buffer: IBuffer = credential
            .RetrievePublicKeyWithBlobType(CryptographicPublicKeyBlobType::X509SubjectPublicKeyInfo)
            .map_err(|e| CredentialError::SystemError(format!("Failed to get public key: {}", e)))?;
        
        // 转换为字节数组
        let length = public_key_buffer.Length()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get buffer length: {}", e)))? as usize;
        
        let mut public_key = vec![0u8; length];
        
        let data_reader = windows::Storage::Streams::DataReader::FromBuffer(&public_key_buffer)
            .map_err(|e| CredentialError::SystemError(format!("Failed to create data reader: {}", e)))?;
        
        data_reader.ReadBytes(&mut public_key)
            .map_err(|e| CredentialError::SystemError(format!("Failed to read bytes: {}", e)))?;
        
        // 缓存公钥
        {
            let mut cached = state.cached_public_key.lock().unwrap();
            *cached = Some(public_key.clone());
        }
        
        Ok(public_key)
    }
    
    /// 请求用户通过 Windows Hello 认证以获取私钥签名
    /// 这用于验证用户身份
    #[allow(dead_code)]
    pub async fn request_user_verification(
        state: &CredentialManagerState,
    ) -> Result<bool, CredentialError> {
        let app_name = HSTRING::from(&state.app_name);

        let result = KeyCredentialManager::OpenAsync(&app_name)
            .map_err(|e| CredentialError::SystemError(format!("Failed to open credential: {}", e)))?
            .get()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get credential: {}", e)))?;

        let status = result.Status()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get status: {}", e)))?;

        if status != KeyCredentialStatus::Success {
            return Err(CredentialError::KeyNotFound);
        }

        let credential = result.Credential()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get credential: {}", e)))?;

        let challenge_buffer = CryptographicBuffer::CreateFromByteArray(b"CarbonPaper-Auth-Challenge")
            .map_err(|e| CredentialError::SystemError(format!("Failed to create challenge: {}", e)))?;

        let sign_result = credential.RequestSignAsync(&challenge_buffer)
            .map_err(|e| CredentialError::SystemError(format!("Failed to request sign: {}", e)))?
            .get()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get sign result: {}", e)))?;

        let sign_status = sign_result.Status()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get sign status: {}", e)))?;

        match sign_status {
            KeyCredentialStatus::Success => Ok(true),
            KeyCredentialStatus::UserCanceled => Err(CredentialError::UserCancelled),
            _ => Err(CredentialError::SystemError(format!(
                "Sign failed: {:?}",
                sign_status
            ))),
        }
    }
    
    /// 删除凭证密钥
    #[allow(dead_code)]
    pub async fn delete_credential(state: &CredentialManagerState) -> Result<(), CredentialError> {
        let app_name = HSTRING::from(&state.app_name);
        
        KeyCredentialManager::DeleteAsync(&app_name)
            .map_err(|e| CredentialError::SystemError(format!("Failed to delete credential: {}", e)))?
            .get()
            .map_err(|e| CredentialError::SystemError(format!("Failed to complete deletion: {}", e)))?;
        
        // 清除缓存
        {
            let mut cached = state.cached_public_key.lock().unwrap();
            *cached = None;
        }
        {
            let mut cached_db = state.cached_db_key.lock().unwrap();
            *cached_db = None;
        }
        {
            let mut cached_master = state.cached_master_key.lock().unwrap();
            *cached_master = None;
        }

        // 删除本地主密钥文件
        let key_file = state.data_dir.join(MASTER_KEY_FILE_NAME);
        let _ = std::fs::remove_file(&key_file);
        
        Ok(())
    }

    /// 强制验证用户身份并解锁主密钥
    ///
    /// 策略：
    /// - 冷启动（master key 未缓存）：主进程内直接 CNG 解密
    ///   → 一次弹窗，且主进程 CNG PIN 缓存生效，后续 row key 解密无需再弹
    /// - 锁定后解锁（master key 已缓存）：spawn 子进程执行 CNG 解密
    ///   → 子进程无 PIN 缓存，强制弹窗；主进程 CNG 缓存仍在，row key 解密无额外弹窗
    pub fn force_verify_and_unlock_master_key(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
        let key_file = state.data_dir.join(MASTER_KEY_FILE_NAME);
        if !key_file.exists() {
            return Err(CredentialError::KeyNotFound);
        }

        let already_cached = get_cached_master_key(state).is_some();

        let master_key = if already_cached {
            // 锁定后解锁：主进程 CNG 已缓存不会弹窗，用子进程强制验证
            verify_via_subprocess(&key_file)?
        } else {
            // 冷启动：主进程内直接解密，让 CNG PIN 缓存留在主进程中
            let file_data = std::fs::read(&key_file)
                .map_err(|e| CredentialError::SystemError(format!("Failed to read master key file: {}", e)))?;
            let ciphertext = decode_master_key_file(&file_data)?;
            decrypt_master_key_with_cng(&ciphertext)?
        };

        {
            let mut cached = state.cached_master_key.lock().unwrap();
            *cached = Some(master_key.clone());
        }

        Ok(master_key)
    }

    /// 通过独立子进程执行 CNG 解密，绕过主进程的 PIN 缓存
    fn verify_via_subprocess(key_file: &std::path::Path) -> Result<Vec<u8>, CredentialError> {
        let exe_path = std::env::current_exe()
            .map_err(|e| CredentialError::SystemError(format!("Failed to get current exe: {}", e)))?;

        let output = std::process::Command::new(&exe_path)
            .arg("--cng-unlock")
            .arg(key_file)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| CredentialError::SystemError(format!("Failed to spawn CNG unlock process: {}", e)))?
            .wait_with_output()
            .map_err(|e| CredentialError::SystemError(format!("Failed to wait for CNG unlock process: {}", e)))?;

        match output.status.code() {
            Some(0) => {
                let hex_str = String::from_utf8_lossy(&output.stdout);
                let master_key = hex::decode(hex_str.trim())
                    .map_err(|e| CredentialError::CryptoError(format!("Invalid hex from subprocess: {}", e)))?;

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

    /// 通过 Windows Hello 解锁并缓存主密钥
    pub async fn unlock_master_key(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
        if let Some(key) = get_cached_master_key(state) {
            return Ok(key);
        }

        let key_file = state.data_dir.join(MASTER_KEY_FILE_NAME);
        if !key_file.exists() {
            return Err(CredentialError::KeyNotFound);
        }

        let file_data = std::fs::read(&key_file)
            .map_err(|e| CredentialError::SystemError(format!("Failed to read master key file: {}", e)))?;

        let ciphertext = decode_master_key_file(&file_data)?;
        let master_key = decrypt_master_key_with_cng(&ciphertext)?;

        {
            let mut cached = state.cached_master_key.lock().unwrap();
            *cached = Some(master_key.clone());
        }

        Ok(master_key)
    }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub async fn create_or_get_credential(
    _state: &CredentialManagerState,
) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError("Windows Hello is only available on Windows".to_string()))
}

/// 仅在首次使用时生成主密钥（不触发 Windows Hello）
/// 如果主密钥已存在（缓存或文件），不做任何操作
/// 用于 credential_initialize，避免冷启动时触发两次 Windows Hello
#[cfg(windows)]
pub fn ensure_master_key_created(state: &CredentialManagerState) -> Result<(), CredentialError> {
    // 已缓存，无需操作
    if get_cached_master_key(state).is_some() {
        return Ok(());
    }

    let key_file = state.data_dir.join(MASTER_KEY_FILE_NAME);
    if key_file.exists() {
        // 主密钥文件已存在，稍后由 credential_verify_user 解锁
        return Ok(());
    }

    // 首次使用：生成新主密钥并用 CNG 公钥封装（不弹窗）
    let mut master_key = vec![0u8; MASTER_KEY_LEN];
    rand::thread_rng().fill_bytes(&mut master_key);

    let ciphertext = encrypt_master_key_with_cng(&master_key)?;
    let file_data = encode_master_key_file(&ciphertext);

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CredentialError::SystemError(format!("Failed to create directory: {}", e)))?;
    }

    std::fs::write(&key_file, file_data)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save master key: {}", e)))?;

    {
        let mut cached = state.cached_master_key.lock().unwrap();
        *cached = Some(master_key.clone());
    }

    Ok(())
}

#[cfg(windows)]
pub async fn ensure_master_key_ready(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_master_key(state) {
        return Ok(key);
    }

    let key_file = state.data_dir.join(MASTER_KEY_FILE_NAME);
    if key_file.exists() {
        let master_key = unlock_master_key(state).await?;
        return Ok(master_key);
    }

    // 生成新主密钥并用 CNG/NCrypt 公钥封装（不弹窗）
    let mut master_key = vec![0u8; MASTER_KEY_LEN];
    rand::thread_rng().fill_bytes(&mut master_key);

    let ciphertext = encrypt_master_key_with_cng(&master_key)?;
    let file_data = encode_master_key_file(&ciphertext);

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CredentialError::SystemError(format!("Failed to create directory: {}", e)))?;
    }

    std::fs::write(&key_file, file_data)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save master key: {}", e)))?;

    {
        let mut cached = state.cached_master_key.lock().unwrap();
        *cached = Some(master_key.clone());
    }

    Ok(master_key)
}

/// 检查是否需要认证
fn ensure_session_valid(state: &CredentialManagerState) -> Result<(), CredentialError> {
    if !state.is_session_valid() {
        return Err(CredentialError::AuthRequired);
    }
    Ok(())
}

/// 获取缓存的主密钥
pub fn get_cached_master_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state.cached_master_key.lock().unwrap().clone()
}

/// 获取缓存的数据库密钥
pub fn get_cached_db_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state.cached_db_key.lock().unwrap().clone()
}

/// 获取缓存的公钥
pub fn get_cached_public_key(state: &CredentialManagerState) -> Option<Vec<u8>> {
    state.cached_public_key.lock().unwrap().clone()
}

/// 获取或创建数据库密钥（同步版本，用于数据库初始化）
pub fn get_or_create_db_key_sync(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
    // 先检查缓存
    if let Some(key) = get_cached_db_key(state) {
        return Ok(key);
    }

    // 需要有效认证会话
    ensure_session_valid(state)?;

    // 获取主密钥（必须已解锁缓存）
    let master_key = get_or_create_master_key_sync(state)?;
    let db_key = derive_db_key_from_master(&master_key);

    // 更新缓存
    {
        let mut cached_db = state.cached_db_key.lock().unwrap();
        *cached_db = Some(db_key.clone());
    }

    Ok(db_key)
}

/// 获取或创建主密钥（同步版本）
pub fn get_or_create_master_key_sync(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_master_key(state) {
        return Ok(key);
    }

    // 未解锁，强制要求认证
    Err(CredentialError::AuthRequired)
}

#[cfg(windows)]
fn open_or_create_cng_key() -> Result<windows::Win32::Security::Cryptography::NCRYPT_KEY_HANDLE, CredentialError> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Security::Cryptography::{
        NCryptCreatePersistedKey, NCryptFinalizeKey, NCryptOpenKey, NCryptOpenStorageProvider,
        NCryptSetProperty, NCryptFreeObject, CERT_KEY_SPEC, NCRYPT_FLAGS, NCRYPT_KEY_HANDLE, 
        NCRYPT_PROV_HANDLE, NCRYPT_RSA_ALGORITHM, NCRYPT_OVERWRITE_KEY_FLAG,
        NCRYPT_UI_POLICY, NCRYPT_UI_FORCE_HIGH_PROTECTION_FLAG, NCRYPT_HANDLE,
    };

    // 属性名称常量
    const LENGTH_PROP: &str = "Length";
    const UI_POLICY_PROP: &str = "UI Policy";

    // 打开 Software KSP
    let mut provider = NCRYPT_PROV_HANDLE::default();
    let provider_name = HSTRING::from(CNG_PROVIDER_NAME);
    let provider_pcwstr = PCWSTR::from_raw(provider_name.as_ptr());
    unsafe { NCryptOpenStorageProvider(&mut provider, provider_pcwstr, 0) }
        .map_err(|e| CredentialError::SystemError(format!("Failed to open CNG provider: {}", e)))?;

    // 尝试打开已存在的密钥
    let mut key = NCRYPT_KEY_HANDLE::default();
    let key_name = HSTRING::from(CNG_KEY_NAME);
    let key_pcwstr = PCWSTR::from_raw(key_name.as_ptr());
    let open_result = unsafe {
        NCryptOpenKey(provider, &mut key, key_pcwstr, CERT_KEY_SPEC(0), NCRYPT_FLAGS(0))
    };

    if open_result.is_ok() {
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        return Ok(key);
    }

    // 密钥不存在，创建新密钥
    let mut new_key = NCRYPT_KEY_HANDLE::default();
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
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to create CNG key: {}", e))
    })?;

    // 设置 RSA 密钥长度为 2048 位
    let key_length: u32 = 2048;
    let length_name = HSTRING::from(LENGTH_PROP);
    let length_pcwstr = PCWSTR::from_raw(length_name.as_ptr());
    unsafe {
        NCryptSetProperty(
            new_key,
            length_pcwstr,
            &key_length.to_le_bytes(),
            NCRYPT_FLAGS(0),
        )
    }
    .map_err(|e| {
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to set key length: {}", e))
    })?;

    // 设置 UI Policy - 强制高保护（解密时弹出系统级对话框）
    // 注意：这是关键步骤，确保私钥操作需要用户确认
    let ui_policy = NCRYPT_UI_POLICY {
        dwVersion: 1,
        dwFlags: NCRYPT_UI_FORCE_HIGH_PROTECTION_FLAG,
        pszCreationTitle: PCWSTR::null(),
        pszFriendlyName: PCWSTR::null(),
        pszDescription: PCWSTR::null(),
    };

    let policy_name = HSTRING::from(UI_POLICY_PROP);
    let policy_pcwstr = PCWSTR::from_raw(policy_name.as_ptr());
    let policy_bytes = unsafe {
        std::slice::from_raw_parts(
            &ui_policy as *const NCRYPT_UI_POLICY as *const u8,
            std::mem::size_of::<NCRYPT_UI_POLICY>(),
        )
    };

    unsafe {
        NCryptSetProperty(
            new_key,
            policy_pcwstr,
            policy_bytes,
            NCRYPT_FLAGS(0),
        )
    }
    .map_err(|e| {
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
        CredentialError::SystemError(format!("Failed to set UI policy: {}", e))
    })?;

    // 最终确定密钥
    unsafe { NCryptFinalizeKey(new_key, NCRYPT_FLAGS(0)) }
        .map_err(|e| {
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(new_key.0)) };
            let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
            CredentialError::SystemError(format!("Failed to finalize CNG key: {}", e))
        })?;

    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(provider.0)) };
    Ok(new_key)
}

#[cfg(windows)]
fn encrypt_master_key_with_cng(master_key: &[u8]) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Security::Cryptography::{
        NCryptEncrypt, NCryptFreeObject, NCRYPT_PAD_PKCS1_FLAG, NCRYPT_HANDLE,
    };

    let key = open_or_create_cng_key()?;
    
    // 使用 PKCS1 padding（更广泛支持）
    // 第一次调用获取输出大小
    let mut out_len: u32 = 0;
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
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptEncrypt size failed: {}", e))
    })?;

    let mut output = vec![0u8; out_len as usize];
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
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptEncrypt failed: {}", e))
    })?;

    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
    output.truncate(out_len as usize);
    Ok(output)
}

/// 使用 CNG 公钥封装任意行密钥（无 UI）
pub fn encrypt_row_key_with_cng(row_key: &[u8]) -> Result<Vec<u8>, CredentialError> {
    encrypt_master_key_with_cng(row_key)
}

#[cfg(windows)]
pub fn decrypt_master_key_with_cng(ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    use windows::Win32::Security::Cryptography::{
        NCryptDecrypt, NCryptFreeObject, NCRYPT_PAD_PKCS1_FLAG, NCRYPT_HANDLE,
    };

    let key = open_or_create_cng_key()?;
    
    // 第一次调用获取输出大小
    let mut out_len: u32 = 0;
    unsafe {
        NCryptDecrypt(
            key,
            Some(ciphertext),
            None,
            None,
            &mut out_len,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .map_err(|e| {
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptDecrypt size failed: {}", e))
    })?;

    let mut output = vec![0u8; out_len as usize];
    unsafe {
        NCryptDecrypt(
            key,
            Some(ciphertext),
            None,
            Some(output.as_mut_slice()),
            &mut out_len,
            NCRYPT_PAD_PKCS1_FLAG,
        )
    }
    .map_err(|e| {
        let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
        CredentialError::SystemError(format!("NCryptDecrypt failed: {}", e))
    })?;

    let _ = unsafe { NCryptFreeObject(NCRYPT_HANDLE(key.0)) };
    output.truncate(out_len as usize);
    Ok(output)
}

/// 使用 CNG 私钥解封装行密钥（触发系统级 UI）
pub fn decrypt_row_key_with_cng(ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    decrypt_master_key_with_cng(ciphertext)
}

fn encode_master_key_file(ciphertext: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(MASTER_KEY_FILE_MAGIC.len() + ciphertext.len());
    data.extend_from_slice(MASTER_KEY_FILE_MAGIC);
    data.extend_from_slice(ciphertext);
    data
}

pub fn decode_master_key_file(data: &[u8]) -> Result<Vec<u8>, CredentialError> {
    if data.len() <= MASTER_KEY_FILE_MAGIC.len() {
        return Err(CredentialError::CryptoError("Invalid master key file".to_string()));
    }

    if &data[..MASTER_KEY_FILE_MAGIC.len()] != MASTER_KEY_FILE_MAGIC {
        return Err(CredentialError::CryptoError("Invalid master key file magic".to_string()));
    }

    Ok(data[MASTER_KEY_FILE_MAGIC.len()..].to_vec())
}

/// 从文件加载公钥并缓存（无需用户交互）
pub fn load_public_key_from_file(state: &CredentialManagerState) -> Result<Vec<u8>, CredentialError> {
    if let Some(key) = get_cached_public_key(state) {
        return Ok(key);
    }

    let key_file = state.data_dir.join("credential_public_key.bin");
    if !key_file.exists() {
        return Err(CredentialError::KeyNotFound);
    }

    let public_key = std::fs::read(&key_file)
        .map_err(|e| CredentialError::SystemError(format!("Failed to read public key: {}", e)))?;

    {
        let mut cached = state.cached_public_key.lock().unwrap();
        *cached = Some(public_key.clone());
    }

    Ok(public_key)
}

/// 保存公钥到文件（用于后续无需交互的访问）
pub fn save_public_key_to_file(state: &CredentialManagerState, public_key: &[u8]) -> Result<(), CredentialError> {
    let key_file = state.data_dir.join("credential_public_key.bin");
    
    // 确保目录存在
    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CredentialError::SystemError(format!("Failed to create directory: {}", e)))?;
    }
    
    std::fs::write(&key_file, public_key)
        .map_err(|e| CredentialError::SystemError(format!("Failed to save public key: {}", e)))?;
    
    Ok(())
}

#[cfg(not(windows))]
pub fn encrypt_row_key_with_cng(_row_key: &[u8]) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError("CNG is only available on Windows".to_string()))
}

#[cfg(not(windows))]
pub fn decrypt_row_key_with_cng(_ciphertext: &[u8]) -> Result<Vec<u8>, CredentialError> {
    Err(CredentialError::SystemError("CNG is only available on Windows".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    
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
}
