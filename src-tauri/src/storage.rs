//! 存储管理模块 - SQLCipher 加密数据库和截图文件管理
//!
//! 该模块提供：
//! 1. 加密的 SQLite 数据库存储（使用 SQLCipher）
//! 2. 截图文件的存储和检索
//! 3. OCR 数据的存储和搜索

use crate::credential_manager::{
    decrypt_row_key_with_cng, decrypt_with_master_key, derive_db_key_from_public_key,
    encrypt_row_key_with_cng, encrypt_with_master_key, get_cached_public_key,
    load_public_key_from_file, CredentialManagerState,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jieba_rs::Jieba;
use once_cell::sync::Lazy;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
// removed unused import: std::hash::Hash
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 存储管理器状态
pub struct StorageState {
    /// 数据库连接
    db: Mutex<Option<Connection>>,
    /// 测试数据库连接
    db_test: Mutex<Option<Connection>>,
    /// 数据目录
    pub data_dir: PathBuf,
    /// 截图目录
    pub screenshot_dir: PathBuf,
    /// 凭证管理器状态引用
    credential_state: Arc<CredentialManagerState>,
    /// 是否已初始化
    initialized: Mutex<bool>,
}

/// 截图记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRecord {
    pub id: i64,
    pub image_path: String,
    pub image_hash: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub created_at: String,
    pub metadata: Option<String>,
    pub timestamp: Option<i64>,
}

/// OCR 结果记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResult {
    pub id: i64,
    pub screenshot_id: i64,
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
    pub created_at: String,
}

/// 搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: i64,
    pub screenshot_id: i64,
    pub text: String,
    pub confidence: f64,
    pub box_coords: Vec<Vec<f64>>,
    pub image_path: String,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub created_at: String,
    pub screenshot_created_at: String,
}

/// 存储保存请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveScreenshotRequest {
    pub image_data: String, // Base64 编码的图片数据
    pub image_hash: String,
    pub width: i32,
    pub height: i32,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub ocr_results: Option<Vec<OcrResultInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResultInput {
    pub text: String,
    pub confidence: f64,
    #[serde(rename = "box")]
    pub box_coords: Vec<Vec<f64>>,
}

/// 存储响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveScreenshotResponse {
    pub status: String,
    pub screenshot_id: Option<i64>,
    pub image_path: Option<String>,
    pub added: i32,
    pub skipped: i32,
}

impl StorageState {
    pub fn new(data_dir: PathBuf, credential_state: Arc<CredentialManagerState>) -> Self {
        let screenshot_dir = data_dir.join("screenshots");

        Self {
            db: Mutex::new(None),
            db_test: Mutex::new(None),
            data_dir,
            screenshot_dir,
            credential_state,
            initialized: Mutex::new(false),
        }
    }

    /// 初始化存储（创建目录和数据库）
    pub fn initialize(&self) -> Result<(), String> {
        let mut initialized = self.initialized.lock().unwrap();
        if *initialized {
            return Ok(());
        }

        // 创建目录
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;
        std::fs::create_dir_all(&self.screenshot_dir)
            .map_err(|e| format!("Failed to create screenshot directory: {}", e))?;

        // 使用公钥派生弱数据库密钥（无需用户认证）
        let public_key = get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())?;
        let db_key = derive_db_key_from_public_key(&public_key);

        // 打开 SQLCipher 加密数据库
        let db_path = self.data_dir.join("screenshots.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;

        // 设置 SQLCipher 密钥（使用 hex 格式）
        let key_hex = hex::encode(&db_key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex))
            .map_err(|e| format!("Failed to set database key: {}", e))?;

        // 验证密钥是否正确
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|e| format!("Database key verification failed: {}", e))?;

        // 初始化表结构
        self.init_tables(&conn)?;

        *self.db.lock().unwrap() = Some(conn);

        // 测试性：采用盲三元组索引+压缩位图存储的数据库，暂时不加密方便调试
        // @TODO 后续改为加密存储
        let testing_db_path = self.data_dir.join("screenshots_test.db");
        let testing_conn = Connection::open(&testing_db_path)
            .map_err(|e| format!("Failed to open testing database: {}", e))?;

        self.init_blind_triple_index_db(&testing_conn)?;

        println!("[storage] Testing blind triple index database initialized");
        *self.db_test.lock().unwrap() = Some(testing_conn);

        *initialized = true;

        println!("[storage] SQLCipher weakly encrypted database initialized");

        Ok(())
    }

    fn init_blind_triple_index_db(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            -- 1. 截图记录表 (保持不变)
            CREATE TABLE IF NOT EXISTS screenshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                image_path TEXT NOT NULL,
                image_hash TEXT UNIQUE NOT NULL, -- 防止重复录制
                width INTEGER,
                height INTEGER,
                window_title TEXT,    -- 可选：仅用于调试，实际敏感数据存 encrypted
                process_name TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                -- 加密字段
                window_title_enc BLOB,
                process_name_enc BLOB,
                metadata_enc BLOB,
                content_key_encrypted BLOB
            );

            -- 2. OCR 结果表 (保持不变)
            CREATE TABLE IF NOT EXISTS ocr_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                screenshot_id INTEGER NOT NULL,
                text TEXT,            -- 可选：存部分明文或空，视隐私策略而定
                text_hash TEXT NOT NULL,
                -- 加密字段
                text_enc BLOB,        -- 核心密文
                text_key_encrypted BLOB,
                confidence REAL,
                box_x1 REAL, box_y1 REAL, box_x2 REAL, box_y2 REAL,
                box_x3 REAL, box_y3 REAL, box_x4 REAL, box_y4 REAL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            );

            -- 【修改】Bitmap 专用索引表
            CREATE TABLE IF NOT EXISTS blind_bitmap_index (
                token_hash TEXT PRIMARY KEY,  -- 三元组哈希 (如 "a1b2c3...")
                postings_blob BLOB NOT NULL   -- 核心：序列化后的 RoaringBitmap
            );
            -- 这里不需要 WITHOUT ROWID，因为 BLOB 可能较大，标准 B-Tree 管理大页面更好
        "#,
        )
        .map_err(|e| format!("Failed to init triple index db: {}", e))?;

        Ok(())
    }

    /// 初始化数据库表结构
    fn init_tables(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            -- 截图记录表
            CREATE TABLE IF NOT EXISTS screenshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                image_path TEXT NOT NULL,
                image_hash TEXT UNIQUE NOT NULL,
                width INTEGER,
                height INTEGER,
                window_title TEXT,
                process_name TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                metadata TEXT,
                -- 新增字段级加密列
                window_title_enc BLOB,
                process_name_enc BLOB,
                metadata_enc BLOB,
                content_key_encrypted BLOB
            );
            
            -- OCR 结果表
            CREATE TABLE IF NOT EXISTS ocr_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                screenshot_id INTEGER NOT NULL,
                text TEXT,
                text_hash TEXT NOT NULL,
                text_enc BLOB,
                text_key_encrypted BLOB,
                confidence REAL,
                box_x1 REAL, box_y1 REAL,
                box_x2 REAL, box_y2 REAL,
                box_x3 REAL, box_y3 REAL,
                box_x4 REAL, box_y4 REAL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            );

            -- 盲三元组索引表
            CREATE TABLE IF NOT EXISTS search_index (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                token_hash TEXT NOT NULL,
                ocr_result_id INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (ocr_result_id) REFERENCES ocr_results(id) ON DELETE CASCADE
            );
            
            -- 索引
            CREATE INDEX IF NOT EXISTS idx_image_hash ON screenshots(image_hash);
            CREATE INDEX IF NOT EXISTS idx_text_hash ON ocr_results(text_hash);
            CREATE INDEX IF NOT EXISTS idx_screenshot_id ON ocr_results(screenshot_id);
            CREATE INDEX IF NOT EXISTS idx_created_at ON screenshots(created_at);
            CREATE INDEX IF NOT EXISTS idx_process_name ON screenshots(process_name);
            CREATE INDEX IF NOT EXISTS idx_search_token ON search_index(token_hash);
            CREATE INDEX IF NOT EXISTS idx_search_ocr_result ON search_index(ocr_result_id);
            
            -- 启用外键约束
            PRAGMA foreign_keys = ON;
            "#,
        )
        .map_err(|e| format!("Failed to initialize tables: {}", e))?;

        self.ensure_schema(conn)?;

        Ok(())
    }

    /// 兼容旧库结构，补齐新增列
    fn ensure_schema(&self, conn: &Connection) -> Result<(), String> {
        Self::add_column_if_missing(conn, "screenshots", "window_title_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "process_name_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "metadata_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "content_key_encrypted", "BLOB")?;
        // Add status and committed_at for two-phase screenshot lifecycle
        Self::add_column_if_missing(conn, "screenshots", "status", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "committed_at", "TIMESTAMP")?;

        Self::add_column_if_missing(conn, "ocr_results", "text_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "ocr_results", "text_key_encrypted", "BLOB")?;

        Ok(())
    }

    fn add_column_if_missing(
        conn: &Connection,
        table: &str,
        column: &str,
        column_type: &str,
    ) -> Result<(), String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .map_err(|e| format!("Failed to read table info: {}", e))?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("Failed to query table info: {}", e))?
            .filter_map(|r| r.ok())
            .any(|name| name == column);

        if !exists {
            conn.execute_batch(&format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                table, column, column_type
            ))
            .map_err(|e| format!("Failed to add column {}.{}: {}", table, column, e))?;
        }

        Ok(())
    }

    /// 获取数据库连接
    fn get_connection(&self) -> Result<std::sync::MutexGuard<'_, Option<Connection>>, String> {
        let guard = self.db.lock().unwrap();
        if guard.is_none() {
            return Err("Database not initialized".to_string());
        }
        Ok(guard)
    }

    /// 获取测试数据库连接
    fn get_test_connection(&self) -> Result<std::sync::MutexGuard<'_, Option<Connection>>, String> {
        let guard = self.db_test.lock().unwrap();
        if guard.is_none() {
            return Err("Test database not initialized".to_string());
        }
        Ok(guard)
    }

    /// 计算数据的 MD5 哈希
    fn compute_hash(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let result = Sha256::digest(data);
        hex::encode(result)
    }

    /// HMAC 用于盲索引（安全折中，内置 Key）
    fn compute_hmac_hash(text: &str) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;
        const HMAC_KEY: &[u8] = b"CarbonPaper-Search-HMAC-Key-v1";

        let mut mac =
            HmacSha256::new_from_slice(HMAC_KEY).expect("HMAC key length should be valid");
        mac.update(text.as_bytes());
        let result = mac.finalize().into_bytes();
        hex::encode(result)
    }

    fn tokenize_text(text: &str) -> Vec<String> {
        static JIEBA: Lazy<Jieba> = Lazy::new(Jieba::new);

        // 使用 HashSet 进行去重
        let mut unique_tokens = HashSet::new();

        let keywords = JIEBA.cut(text, false);

        for token in keywords {
            let normalized = token
                .trim_matches(|c: char| !c.is_alphanumeric() && !Self::is_cjk(c))
                .to_lowercase();

            if normalized.is_empty() {
                continue;
            }

            // 过滤掉纯标点符号或特殊字符（虽然上面的 trim 处理了一部分，但以防万一）
            // 检查是否包含至少一个有效字符 (CJK 或 字母数字)
            let has_valid_char = normalized
                .chars()
                .any(|c| c.is_ascii_alphanumeric() || Self::is_cjk(c));

            if !has_valid_char {
                continue;
            }

            //    过滤掉单字符的英文/数字 (如 "a", "1")，保留单字符的中文 (如 "税") 视需求而定
            //    这里演示：如果是 ASCII 且长度为 1，则丢弃
            if normalized.len() == 1 && normalized.chars().next().unwrap().is_ascii() {
                continue;
            }

            unique_tokens.insert(normalized);
        }

        // 转回 Vec
        unique_tokens.into_iter().collect()
    }

    /// 三元组分词
    fn triplet_tokenize(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() < 3 {
            return HashSet::new(); // 忽略过短的文本
        }

        chars.windows(3).map(|w| w.iter().collect()).collect()
    }

    fn is_cjk(ch: char) -> bool {
        let code = ch as u32;
        matches!(
            code,
            0x4E00..=0x9FFF |  // CJK Unified Ideographs
            0x3400..=0x4DBF |  // CJK Unified Ideographs Extension A
            0x20000..=0x2A6DF | // Extension B
            0x2A700..=0x2B73F | // Extension C
            0x2B740..=0x2B81F | // Extension D
            0x2B820..=0x2CEAF | // Extension E/F
            0xF900..=0xFAFF |  // CJK Compatibility Ideographs
            0x2F800..=0x2FA1F   // CJK Compatibility Ideographs Supplement
        )
    }

    fn zeroize_bytes(bytes: &mut [u8]) {
        use std::sync::atomic::{compiler_fence, Ordering};
        for b in bytes.iter_mut() {
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }

    fn encrypt_payload_with_row_key(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_data = encrypt_with_master_key(&row_key, plaintext)
            .map_err(|e| format!("Failed to encrypt payload: {}", e))?;

        let encrypted_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap row key: {}", e))?;

        Self::zeroize_bytes(&mut row_key);
        Ok((encrypted_data, encrypted_key))
    }

    fn decrypt_payload_with_row_key(
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

    /// 为 ChromaDB 加密文本（公开 API）
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

    /// 解密来自 ChromaDB 的文本（公开 API）
    pub fn decrypt_from_chromadb(&self, encrypted: &str) -> Result<String, String> {
        if encrypted.is_empty()
            || (!encrypted.starts_with("ENC2:") && !encrypted.starts_with("ENC:"))
        {
            return Ok(encrypted.to_string());
        }

        if encrypted.starts_with("ENC:") {
            // 兼容旧格式：直接返回错误提示需要迁移
            return Err(
                "Legacy ENC format is no longer supported. Please migrate data.".to_string(),
            );
        }

        let data = &encrypted[5..]; // 移除 "ENC2:" 前缀
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

    /// 获取公钥（兼容旧 IPC/接口）
    pub fn get_public_key(&self) -> Result<Vec<u8>, String> {
        get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())
    }

    /// 检查截图是否已存在
    pub fn screenshot_exists(&self, image_hash: &str) -> Result<bool, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ?)",
                [image_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check screenshot: {}", e))?;

        Ok(count > 0)
    }

    /// 保存截图（包括 OCR 结果）
    pub fn save_screenshot(
        &self,
        request: &SaveScreenshotRequest,
    ) -> Result<SaveScreenshotResponse, String> {
        // 检查是否已存在
        if self.screenshot_exists(&request.image_hash)? {
            return Ok(SaveScreenshotResponse {
                status: "duplicate".to_string(),
                screenshot_id: None,
                image_path: None,
                added: 0,
                skipped: request
                    .ocr_results
                    .as_ref()
                    .map(|v| v.len() as i32)
                    .unwrap_or(0),
            });
        }

        // 解码图片数据
        let image_data = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &request.image_data,
        )
        .map_err(|e| format!("Failed to decode image data: {}", e))?;

        // 生成截图级 RowKey（用于图片与元数据加密）
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        // 加密图片数据
        let encrypted_image = encrypt_with_master_key(&row_key, &image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap image row key: {}", e))?;

        // 生成文件名（使用 .enc 扩展名标识加密文件）
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        let filename = format!("screenshot_{}.png.enc", timestamp);
        let image_path = self.screenshot_dir.join(&filename);

        // 保存加密后的图片文件
        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = image_path.to_string_lossy().to_string();

        // 保存到数据库（SQLCipher 整库加密）
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let metadata_json = request
            .metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).ok())
            .flatten();
        let window_title_enc = match &request.window_title {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt window title: {}", e))?,
            ),
            None => None,
        };
        let process_name_enc = match &request.process_name {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt process name: {}", e))?,
            ),
            None => None,
        };
        let metadata_enc = match &metadata_json {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt metadata: {}", e))?,
            ),
            None => None,
        };

        Self::zeroize_bytes(&mut row_key);

        conn.execute(
            "INSERT INTO screenshots (
                image_path, image_hash, width, height,
                window_title, process_name, metadata,
                window_title_enc, process_name_enc, metadata_enc,
                content_key_encrypted
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &image_path_str,
                &request.image_hash,
                request.width,
                request.height,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                window_title_enc,
                process_name_enc,
                metadata_enc,
                encrypted_row_key,
            ],
        )
        .map_err(|e| format!("Failed to insert screenshot: {}", e))?;

        let screenshot_id = conn.last_insert_rowid();

        // 保存 OCR 结果
        let mut added = 0;
        let mut skipped = 0;

        // 保存数据到测试性数据库
        let mut test_guard = self.get_test_connection()?;
        let test_conn = test_guard.as_mut().expect("Error: test database connection missing.");

        // 确保在测试数据库中也插入一条 screenshots 记录，这样后续向 testing ocr_results 插入时
        // 外键 (screenshot_id) 才有对应的父记录，避免 FOREIGN KEY 约束失败。
        test_conn.execute(
            "INSERT OR IGNORE INTO screenshots (id, image_path, image_hash, width, height, created_at) VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
            params![screenshot_id, &image_path_str, &request.image_hash, request.width, request.height],
        ).map_err(|e| format!("Failed to insert screenshot to testing database: {}", e))?;

        if let Some(ocr_results) = &request.ocr_results {
            for result in ocr_results {
                let text_hash = Self::compute_hmac_hash(&result.text);
                let (text_enc, text_key_encrypted) =
                    self.encrypt_payload_with_row_key(result.text.as_bytes())?;

                // 检查是否重复
                let box_coords = &result.box_coords;
                if box_coords.len() >= 4 {
                    let existing: i64 = conn
                        .query_row(
                            "SELECT COUNT(*) FROM ocr_results
                             WHERE screenshot_id = ? AND text_hash = ?
                             AND ABS(box_x1 - ?) < 10 AND ABS(box_y1 - ?) < 10",
                            params![
                                screenshot_id,
                                &text_hash,
                                box_coords[0][0],
                                box_coords[0][1],
                            ],
                            |row| row.get(0),
                        )
                        .unwrap_or(0);

                    if existing > 0 {
                        skipped += 1;
                        continue;
                    }

                    // 插入 OCR 结果（文本保留明文用于搜索）
                    conn.execute(
                        "INSERT INTO ocr_results (
                            screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                            box_x1, box_y1, box_x2, box_y2,
                            box_x3, box_y3, box_x4, box_y4
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            screenshot_id,
                            Option::<String>::None,
                            &text_hash,
                            text_enc,
                            text_key_encrypted,
                            result.confidence,
                            box_coords[0][0], box_coords[0][1],
                            box_coords[1][0], box_coords[1][1],
                            box_coords[2][0], box_coords[2][1],
                            box_coords[3][0], box_coords[3][1],
                        ],
                    )
                    .map_err(|e| format!("Failed to insert OCR result: {}", e))?;

                    test_conn.execute(
                        "INSERT INTO ocr_results (
                            screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                            box_x1, box_y1, box_x2, box_y2,
                            box_x3, box_y3, box_x4, box_y4
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            screenshot_id,
                            Option::<String>::None,
                            &text_hash,
                            text_enc,
                            text_key_encrypted,
                            result.confidence,
                            box_coords[0][0], box_coords[0][1],
                            box_coords[1][0], box_coords[1][1],
                            box_coords[2][0], box_coords[2][1],
                            box_coords[3][0], box_coords[3][1],
                        ],
                    ).map_err(|e| format!("Failed to insert OCR result to testing database: {}", e))?;

                    let ocr_id = test_conn.last_insert_rowid();
                    let tokens = Self::tokenize_text(&result.text);
                    for token in tokens {
                        let token_hash = Self::compute_hmac_hash(&token);
                        let _ = conn.execute(
                            "INSERT INTO search_index (token_hash, ocr_result_id) VALUES (?, ?)",
                            params![token_hash, ocr_id],
                        );
                    }

                    let triple_tokens = Self::triplet_tokenize(&result.text);
                    let tx = test_conn
                        .transaction()
                        .map_err(|e| format!("Failed to start transaction: {}", e))?;
                    // 预编译 SQL 语句
                    let mut get_stmt = tx.prepare_cached(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                    ).map_err(|e| format!("Failed to prepare get statement: {}", e))?;
                    let mut put_stmt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)"
                    ).map_err(|e| format!("Failed to prepare put statement: {}", e))?;
                    for token in triple_tokens {
                        let token_hash = Self::compute_hmac_hash(&token);
                        // 获取现有的 postings_blob
                        let existing_blob: Option<Vec<u8>> =
                            get_stmt.query_row(params![&token_hash], |row| row.get(0)).optional().map_err(|e| format!("Failed to query postings_blob: {}", e))?;
                        let mut bitmap = if let Some(blob) = existing_blob {
                            let rb = roaring::RoaringBitmap::deserialize_from(&blob[..])
                                .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
                            rb
                        } else {
                            roaring::RoaringBitmap::new()
                        };

                        bitmap.insert(ocr_id as u32);

                        let mut serialized_blob = Vec::new();
                        bitmap
                            .serialize_into(&mut serialized_blob)
                            .map_err(|e| format!("Failed to serialize bitmap: {}", e))?;

                        // 插入或更新 postings_blob
                        put_stmt.execute(params![&token_hash, &serialized_blob]).map_err(|e| format!("Failed to update blind bitmap index: {}", e))?;
                    }

                    // 释放对 transaction 的借用
                    drop(put_stmt);
                    drop(get_stmt);

                    tx.commit()
                        .map_err(|e| format!("Failed to commit bitmap index: {}", e))?;

                    added += 1;
                }
            }
        }

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added,
            skipped,
        })
    }

    /// 临时保存（pending）截图：加密写文件并插入 screenshots 记录，状态为 'pending'
    pub fn save_screenshot_temp(
        &self,
        request: &SaveScreenshotRequest,
    ) -> Result<SaveScreenshotResponse, String> {
        // 如果已存在则返回 duplicate
        if self.screenshot_exists(&request.image_hash)? {
            return Ok(SaveScreenshotResponse {
                status: "duplicate".to_string(),
                screenshot_id: None,
                image_path: None,
                added: 0,
                skipped: 0,
            });
        }

        // 解码图片
        let image_data = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &request.image_data,
        )
        .map_err(|e| format!("Failed to decode image data: {}", e))?;

        // 生成 row key 并加密图片
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_image = encrypt_with_master_key(&row_key, &image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap image row key: {}", e))?;

        // 使用 .pending 后缀标记临时文件
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        let filename = format!("screenshot_{}.png.enc.pending", timestamp);
        let image_path = self.screenshot_dir.join(&filename);

        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = image_path.to_string_lossy().to_string();

        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let metadata_json = request
            .metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).ok())
            .flatten();

        let window_title_enc = match &request.window_title {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt window title: {}", e))?,
            ),
            None => None,
        };
        let process_name_enc = match &request.process_name {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt process name: {}", e))?,
            ),
            None => None,
        };
        let metadata_enc = match &metadata_json {
            Some(value) => Some(
                encrypt_with_master_key(&row_key, value.as_bytes())
                    .map_err(|e| format!("Failed to encrypt metadata: {}", e))?,
            ),
            None => None,
        };

        Self::zeroize_bytes(&mut row_key);

        conn.execute(
            "INSERT INTO screenshots (
                image_path, image_hash, width, height,
                window_title, process_name, metadata,
                window_title_enc, process_name_enc, metadata_enc,
                content_key_encrypted, status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &image_path_str,
                &request.image_hash,
                request.width,
                request.height,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                window_title_enc,
                process_name_enc,
                metadata_enc,
                encrypted_row_key,
                "pending",
            ],
        )
        .map_err(|e| format!("Failed to insert screenshot: {}", e))?;

        let screenshot_id = conn.last_insert_rowid();

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added: 0,
            skipped: 0,
        })
    }

    /// Commit pending screenshot: attach OCR results, update index and mark as committed
    pub fn commit_screenshot(
        &self,
        screenshot_id: i64,
        ocr_results: Option<&Vec<OcrResultInput>>,
    ) -> Result<SaveScreenshotResponse, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        // 查找截图记录
        let rec: Option<(String, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT image_path, content_key_encrypted FROM screenshots WHERE id = ?",
                params![screenshot_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("Failed to query screenshot: {}", e))?;

        if rec.is_none() {
            return Err("Screenshot not found".to_string());
        }

        let (image_path_str, _content_key_enc) = rec.unwrap();
        let image_path = std::path::PathBuf::from(&image_path_str);

        // 如果文件名以 .pending 结尾则重命名为正式名称，并更新数据库中的 image_path
        let mut final_image_path_str = image_path_str.clone();
        if let Some(fname) = image_path.file_name().and_then(|s| s.to_str()) {
            if fname.ends_with(".pending") {
                let new_name = fname.trim_end_matches(".pending");
                let new_path = image_path.with_file_name(new_name);
                if let Err(e) = std::fs::rename(&image_path, &new_path) {
                    // 如果重命名失败，记录但继续尝试插入 OCR 结果
                    eprintln!("Failed to rename pending image file: {}", e);
                } else {
                    final_image_path_str = new_path.to_string_lossy().to_string();
                }
            }
        }

        let mut added = 0;
                    let skipped = 0;

        // 插入 OCR 结果（如果有）
        if let Some(results) = ocr_results {
            for result in results {
                let text_hash = Self::compute_hmac_hash(&result.text);
                let (text_enc, text_key_encrypted) = self.encrypt_payload_with_row_key(result.text.as_bytes())?;

                // TODO: 复制 save_screenshot 的去重逻辑与索引插入，简化实现：直接插入
                conn.execute(
                    "INSERT INTO ocr_results (
                        screenshot_id, text, text_hash, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        screenshot_id,
                        Option::<String>::None,
                        &text_hash,
                        text_enc,
                        text_key_encrypted,
                        result.confidence,
                        result.box_coords[0][0], result.box_coords[0][1],
                        result.box_coords[1][0], result.box_coords[1][1],
                        result.box_coords[2][0], result.box_coords[2][1],
                        result.box_coords[3][0], result.box_coords[3][1],
                    ],
                )
                .map_err(|e| format!("Failed to insert OCR result: {}", e))?;

                added += 1;
            }

        }

        // 标记为 committed 并设置 committed_at，同时更新 image_path 为重命名后的路径
        conn.execute(
            "UPDATE screenshots SET image_path = ?, status = ?, committed_at = CURRENT_TIMESTAMP WHERE id = ?",
            params![final_image_path_str, "committed", screenshot_id],
        )
        .map_err(|e| format!("Failed to mark screenshot committed: {}", e))?;

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(final_image_path_str),
            added,
            skipped,
        })
    }

    /// Abort pending screenshot: delete encrypted file and mark DB record as aborted
    pub fn abort_screenshot(&self, screenshot_id: i64, _reason: Option<&str>) -> Result<SaveScreenshotResponse, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let rec: Option<String> = conn
            .query_row(
                "SELECT image_path FROM screenshots WHERE id = ?",
                params![screenshot_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Failed to query screenshot: {}", e))?;

        if rec.is_none() {
            return Err("Screenshot not found".to_string());
        }

        let image_path_str = rec.unwrap();
        let image_path = std::path::PathBuf::from(&image_path_str);

        // 删除文件（容错）
        if image_path.exists() {
            let _ = std::fs::remove_file(&image_path);
        }

        // 标记为 aborted
        conn.execute(
            "UPDATE screenshots SET status = ? WHERE id = ?",
            params!["aborted", screenshot_id],
        )
        .map_err(|e| format!("Failed to mark screenshot aborted: {}", e))?;

        Ok(SaveScreenshotResponse {
            status: "success".to_string(),
            screenshot_id: Some(screenshot_id),
            image_path: Some(image_path_str),
            added: 0,
            skipped: 0,
        })
    }

    /// 获取时间范围内的截图
    pub fn get_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<Vec<ScreenshotRecord>, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        // 转换时间戳（秒）为 UTC 时间的日期时间字符串
        // SQLite CURRENT_TIMESTAMP 存储的是 UTC 时间
        let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        // 使用直接 SQL（参数绑定在 SQLCipher 中可能有问题）
        // 注意：start_dt 和 end_dt 是我们生成的固定格式字符串，不存在 SQL 注入风险
        let sql = format!(
            "SELECT id, image_path, image_hash, width, height,
                    window_title, process_name, metadata,
                    window_title_enc, process_name_enc, metadata_enc,
                    content_key_encrypted,
                    strftime('%s', created_at) as timestamp, created_at
             FROM screenshots
             WHERE created_at BETWEEN '{}' AND '{}'
             ORDER BY created_at ASC",
            start_dt, end_dt
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let records: Vec<ScreenshotRecord> = stmt
            .query_map([], |row| {
                // timestamp 列是 strftime 返回的文本，需要解析为 i64
                let timestamp_str: Option<String> = row.get(12)?;
                let timestamp = timestamp_str.and_then(|s| s.parse::<i64>().ok());

                let window_title_plain: Option<String> = row.get(5)?;
                let process_name_plain: Option<String> = row.get(6)?;
                let metadata_plain: Option<String> = row.get(7)?;

                let window_title_enc: Option<Vec<u8>> = row.get(8)?;
                let process_name_enc: Option<Vec<u8>> = row.get(9)?;
                let metadata_enc: Option<Vec<u8>> = row.get(10)?;
                let content_key_enc: Option<Vec<u8>> = row.get(11)?;

                let mut row_key = content_key_enc
                    .as_ref()
                    .and_then(|enc| decrypt_row_key_with_cng(enc).ok());

                let window_title = match (window_title_enc.as_ref(), row_key.as_ref()) {
                    (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => window_title_plain,
                };
                let process_name = match (process_name_enc.as_ref(), row_key.as_ref()) {
                    (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => process_name_plain,
                };
                let metadata = match (metadata_enc.as_ref(), row_key.as_ref()) {
                    (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => metadata_plain,
                };

                if let Some(ref mut key) = row_key {
                    Self::zeroize_bytes(key);
                }

                Ok(ScreenshotRecord {
                    id: row.get(0)?,
                    image_path: row.get(1)?,
                    image_hash: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    window_title,
                    process_name,
                    timestamp,
                    metadata,
                    created_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .enumerate()
            .filter_map(|(_, r)| r.ok())
            .collect();

        // SQLCipher 整库加密，数据自动解密
        Ok(records)
    }

    /// 搜索文本
    pub fn search_text(
        &self,
        query: &str,
        limit: i32,
        offset: i32,
        fuzzy: bool,
        process_names: Option<Vec<String>>,
        start_time: Option<f64>,
        end_time: Option<f64>,
    ) -> Result<Vec<SearchResult>, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let tokens = if query.is_empty() {
            Vec::new()
        } else {
            Self::tokenize_text(query)
        };
        let token_hashes: Vec<String> = tokens.iter().map(|t| Self::compute_hmac_hash(t)).collect();

        let mut sql = String::from(
            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                    s.image_path, s.window_title_enc, s.process_name_enc,
                    s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
             FROM ocr_results r
             JOIN screenshots s ON r.screenshot_id = s.id",
        );

        let mut where_clauses: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if !token_hashes.is_empty() {
            sql.push_str(" JOIN search_index si ON si.ocr_result_id = r.id");
            let placeholders: Vec<&str> = token_hashes.iter().map(|_| "?").collect();
            where_clauses.push(format!("si.token_hash IN ({})", placeholders.join(",")));
            for h in &token_hashes {
                params.push(Box::new(h.clone()));
            }
        }

        // 添加进程名过滤（加密后无法直接 SQL 过滤，改为后处理）

        // 添加时间范围过滤
        if let Some(start) = start_time {
            let start_dt = DateTime::<Utc>::from_timestamp(start as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            where_clauses.push("s.created_at >= ?".to_string());
            params.push(Box::new(start_dt));
        }

        if let Some(end) = end_time {
            let end_dt = DateTime::<Utc>::from_timestamp(end as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            where_clauses.push("s.created_at <= ?".to_string());
            params.push(Box::new(end_dt));
        }

        if !where_clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&where_clauses.join(" AND "));
        }

        if !token_hashes.is_empty() && !fuzzy {
            sql.push_str(" GROUP BY r.id HAVING COUNT(DISTINCT si.token_hash) = ?");
            params.push(Box::new(token_hashes.len() as i64));
        }

        sql.push_str(" ORDER BY s.created_at DESC, r.id DESC LIMIT ? OFFSET ?");
        params.push(Box::new(limit));
        params.push(Box::new(offset));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare search query: {}", e))?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let mut screenshot_key_cache: std::collections::HashMap<i64, Vec<u8>> =
            std::collections::HashMap::new();

        let results: Vec<SearchResult> = stmt
            .query_map(param_refs.as_slice(), |row| {
                let screenshot_id: i64 = row.get(1)?;
                let text_enc: Option<Vec<u8>> = row.get(2)?;
                let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                let window_title_enc: Option<Vec<u8>> = row.get(14)?;
                let process_name_enc: Option<Vec<u8>> = row.get(15)?;
                let screenshot_key_enc: Option<Vec<u8>> = row.get(16)?;

                Ok((
                    screenshot_id,
                    row.get::<_, i64>(0)?,
                    text_enc,
                    text_key_enc,
                    row.get::<_, f64>(4)?,
                    vec![
                        vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                        vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                        vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                        vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                    ],
                    row.get::<_, String>(13)?,
                    window_title_enc,
                    process_name_enc,
                    screenshot_key_enc,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                ))
            })
            .map_err(|e| format!("Failed to execute search query: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(
                |(
                    screenshot_id,
                    id,
                    text_enc,
                    text_key_enc,
                    confidence,
                    box_coords,
                    image_path,
                    window_title_enc,
                    process_name_enc,
                    screenshot_key_enc,
                    created_at,
                    screenshot_created_at,
                )| {
                    let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                        (Some(data), Some(key)) => self
                            .decrypt_payload_with_row_key(data, key)
                            .ok()
                            .and_then(|v| String::from_utf8(v).ok()),
                        _ => None,
                    };

                    let screenshot_key = match screenshot_key_cache.get(&screenshot_id) {
                        Some(key) => Some(key.clone()),
                        None => match screenshot_key_enc.as_ref() {
                            Some(enc) => {
                                let key = decrypt_row_key_with_cng(enc).ok();
                                if let Some(ref k) = key {
                                    screenshot_key_cache.insert(screenshot_id, k.clone());
                                }
                                key
                            }
                            None => None,
                        },
                    };

                    let window_title = match (window_title_enc.as_ref(), screenshot_key.as_ref()) {
                        (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                            .ok()
                            .and_then(|v| String::from_utf8(v).ok()),
                        _ => None,
                    };
                    let process_name = match (process_name_enc.as_ref(), screenshot_key.as_ref()) {
                        (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                            .ok()
                            .and_then(|v| String::from_utf8(v).ok()),
                        _ => None,
                    };

                    let result = SearchResult {
                        id,
                        screenshot_id,
                        text: text.unwrap_or_default(),
                        confidence,
                        box_coords,
                        image_path,
                        window_title,
                        process_name,
                        created_at,
                        screenshot_created_at,
                    };

                    Some(result)
                },
            )
            .collect();

        for (_, mut key) in screenshot_key_cache.into_iter() {
            Self::zeroize_bytes(&mut key);
        }

        // 后处理：进程名过滤
        let filtered = if let Some(names) = process_names {
            if names.is_empty() {
                results
            } else {
                results
                    .into_iter()
                    .filter(|r| {
                        r.process_name
                            .as_ref()
                            .map(|p| names.contains(p))
                            .unwrap_or(false)
                    })
                    .collect()
            }
        } else {
            results
        };

        Ok(filtered)
    }

    /// 获取截图详情
    pub fn get_screenshot_by_id(&self, id: i64) -> Result<Option<ScreenshotRecord>, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        println!("[Storage] get_screenshot_by_id called with id={}", id);

        // 使用直接 SQL（SQLCipher 参数绑定可能有问题）
        let sql = format!(
            "SELECT id, image_path, image_hash, width, height,
                    window_title, process_name, metadata,
                    window_title_enc, process_name_enc, metadata_enc,
                    content_key_encrypted,
                    strftime('%s', created_at) as timestamp, created_at
             FROM screenshots WHERE id = {}",
            id
        );

        let result = conn.query_row(&sql, [], |row| {
            // timestamp 列是 strftime 返回的文本，需要解析为 i64
            let timestamp_str: Option<String> = row.get(7)?;
            let timestamp = timestamp_str.and_then(|s| s.parse::<i64>().ok());

            let window_title_plain: Option<String> = row.get(5)?;
            let process_name_plain: Option<String> = row.get(6)?;
            let metadata_plain: Option<String> = row.get(7)?;

            let window_title_enc: Option<Vec<u8>> = row.get(8)?;
            let process_name_enc: Option<Vec<u8>> = row.get(9)?;
            let metadata_enc: Option<Vec<u8>> = row.get(10)?;
            let content_key_enc: Option<Vec<u8>> = row.get(11)?;

            let mut row_key = content_key_enc
                .as_ref()
                .and_then(|enc| decrypt_row_key_with_cng(enc).ok());

            let window_title = match (window_title_enc.as_ref(), row_key.as_ref()) {
                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                    .ok()
                    .and_then(|v| String::from_utf8(v).ok()),
                _ => window_title_plain,
            };
            let process_name = match (process_name_enc.as_ref(), row_key.as_ref()) {
                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                    .ok()
                    .and_then(|v| String::from_utf8(v).ok()),
                _ => process_name_plain,
            };
            let metadata = match (metadata_enc.as_ref(), row_key.as_ref()) {
                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                    .ok()
                    .and_then(|v| String::from_utf8(v).ok()),
                _ => metadata_plain,
            };

            if let Some(ref mut key) = row_key {
                Self::zeroize_bytes(key);
            }

            Ok(ScreenshotRecord {
                id: row.get(0)?,
                image_path: row.get(1)?,
                image_hash: row.get(2)?,
                width: row.get(3)?,
                height: row.get(4)?,
                window_title,
                process_name,
                timestamp,
                metadata,
                created_at: row.get(13)?,
            })
        });

        match result {
            Ok(record) => {
                println!(
                    "[Storage] Found record id={}, image_path={}",
                    record.id, record.image_path
                );
                Ok(Some(record))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                println!("[Storage] No record found for id={}", id);
                Ok(None)
            }
            Err(e) => {
                println!("[Storage] Query error for id={}: {}", id, e);
                Err(format!("Failed to get screenshot: {}", e))
            }
        }
    }

    /// 获取截图的 OCR 结果
    pub fn get_screenshot_ocr_results(&self, screenshot_id: i64) -> Result<Vec<OcrResult>, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT id, screenshot_id, text_enc, text_key_encrypted, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4, created_at
                 FROM ocr_results WHERE screenshot_id = ?
                 ORDER BY box_y1, box_x1",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map([screenshot_id], |row| {
                let text_enc: Option<Vec<u8>> = row.get(2)?;
                let text_key_enc: Option<Vec<u8>> = row.get(3)?;
                let text = match (text_enc.as_ref(), text_key_enc.as_ref()) {
                    (Some(data), Some(key)) => self
                        .decrypt_payload_with_row_key(data, key)
                        .ok()
                        .and_then(|v| String::from_utf8(v).ok()),
                    _ => None,
                };

                Ok(OcrResult {
                    id: row.get(0)?,
                    screenshot_id: row.get(1)?,
                    text: text.unwrap_or_default(),
                    confidence: row.get(4)?,
                    box_coords: vec![
                        vec![row.get::<_, f64>(5)?, row.get::<_, f64>(6)?],
                        vec![row.get::<_, f64>(7)?, row.get::<_, f64>(8)?],
                        vec![row.get::<_, f64>(9)?, row.get::<_, f64>(10)?],
                        vec![row.get::<_, f64>(11)?, row.get::<_, f64>(12)?],
                    ],
                    created_at: row.get(13)?,
                })
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// 删除截图
    pub fn delete_screenshot(&self, id: i64) -> Result<bool, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        // 先获取图片路径
        let image_path: Option<String> = conn
            .query_row(
                "SELECT image_path FROM screenshots WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .ok();

        // 删除数据库记录
        let deleted = conn
            .execute("DELETE FROM screenshots WHERE id = ?", [id])
            .map_err(|e| format!("Failed to delete screenshot: {}", e))?;

        // 尝试删除图片文件
        if deleted > 0 {
            if let Some(path) = image_path {
                let _ = std::fs::remove_file(&path);
            }
        }

        Ok(deleted > 0)
    }

    /// 删除时间范围内的截图
    pub fn delete_screenshots_by_time_range(
        &self,
        start_ts: f64,
        end_ts: f64,
    ) -> Result<i32, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        // 转换时间戳（毫秒）为 SQLite 日期时间
        let start_dt = DateTime::<Utc>::from_timestamp((start_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp((end_ts / 1000.0) as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        // 先获取所有要删除的图片路径
        let mut stmt = conn
            .prepare("SELECT image_path FROM screenshots WHERE created_at BETWEEN ? AND ?")
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let paths: Vec<String> = stmt
            .query_map([&start_dt, &end_dt], |row| row.get(0))
            .map_err(|e| format!("Failed to execute query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        // 删除数据库记录
        let deleted = conn
            .execute(
                "DELETE FROM screenshots WHERE created_at BETWEEN ? AND ?",
                [&start_dt, &end_dt],
            )
            .map_err(|e| format!("Failed to delete screenshots: {}", e))?;

        // 尝试删除图片文件
        for path in paths {
            let _ = std::fs::remove_file(&path);
        }

        Ok(deleted as i32)
    }

    /// 列出不同的进程名
    pub fn list_distinct_processes(&self) -> Result<Vec<(String, i64)>, String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT process_name, process_name_enc, content_key_encrypted
                 FROM screenshots",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

        let rows = stmt
            .query_map([], |row| {
                let process_plain: Option<String> = row.get(0)?;
                let process_enc: Option<Vec<u8>> = row.get(1)?;
                let key_enc: Option<Vec<u8>> = row.get(2)?;
                Ok((process_plain, process_enc, key_enc))
            })
            .map_err(|e| format!("Failed to execute query: {}", e))?;

        for row in rows.filter_map(|r| r.ok()) {
            let (process_plain, process_enc, key_enc) = row;
            let mut row_key = key_enc
                .as_ref()
                .and_then(|enc| decrypt_row_key_with_cng(enc).ok());
            let process_name = match (process_enc.as_ref(), row_key.as_ref()) {
                (Some(data), Some(key)) => decrypt_with_master_key(key, data)
                    .ok()
                    .and_then(|v| String::from_utf8(v).ok()),
                _ => process_plain,
            };

            if let Some(name) = process_name {
                if !name.trim().is_empty() {
                    *counts.entry(name).or_insert(0) += 1;
                }
            }

            if let Some(ref mut key) = row_key {
                Self::zeroize_bytes(key);
            }
        }

        let mut results: Vec<(String, i64)> = counts.into_iter().collect();
        results.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        Ok(results)
    }

    /// 读取加密图片文件并返回 Base64 编码
    pub fn read_image(&self, path: &str) -> Result<(String, String), String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        let key_enc: Option<Vec<u8>> = conn
            .query_row(
                "SELECT content_key_encrypted FROM screenshots WHERE image_path = ?",
                [path],
                |row| row.get(0),
            )
            .ok();

        let mut row_key = key_enc
            .as_ref()
            .and_then(|enc| decrypt_row_key_with_cng(enc).ok())
            .ok_or_else(|| "Failed to unwrap image row key".to_string())?;

        let result = read_encrypted_image_as_base64(path, &row_key);
        Self::zeroize_bytes(&mut row_key);
        result
    }
}

/// 读取图片文件并返回 Base64 编码（支持加密文件）
#[allow(dead_code)]
pub fn read_image_as_base64(path: &str) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;

    // 更稳健地检测是否为加密文件：文件名中包含 ".enc" 即视为加密（兼容 .enc.pending）
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // 获取实际的 MIME 类型：尝试从文件名（去掉 .enc/.pending 后缀）推断
    let base_name = if is_encrypted {
        // 例如：screenshot_xxx.png.enc 或 screenshot_xxx.png.enc.pending
        if let Some(pos) = fname.find(".enc") {
            &fname[..pos]
        } else {
            fname
        }
    } else {
        fname
    };

    let mime_type = if base_name.ends_with(".png") {
        "image/png"
    } else if base_name.ends_with(".jpg") || base_name.ends_with(".jpeg") {
        "image/jpeg"
    } else if base_name.ends_with(".gif") {
        "image/gif"
    } else if base_name.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };

    // 加密文件需要解密密钥，这里只能返回错误，需要使用带密钥的方法
    if is_encrypted {
        return Err(
            "Encrypted image requires decryption key. Use read_encrypted_image_as_base64 instead."
                .to_string(),
        );
    }

    let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    Ok((base64_data, mime_type.to_string()))
}

/// 读取加密图片文件并返回 Base64 编码（带解密）
pub fn read_encrypted_image_as_base64(
    path: &str,
    row_key: &[u8],
) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;
    // 更稳健地检测是否为加密文件：文件名中包含 ".enc" 即视为加密（兼容 .enc.pending）
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // 获取实际的 MIME 类型：尝试从文件名（去掉 .enc/.pending 后缀）推断
    let base_name = if is_encrypted {
        if let Some(pos) = fname.find(".enc") {
            &fname[..pos]
        } else {
            fname
        }
    } else {
        fname
    };

    let mime_type = if base_name.ends_with(".png") {
        "image/png"
    } else if base_name.ends_with(".jpg") || base_name.ends_with(".jpeg") {
        "image/jpeg"
    } else if base_name.ends_with(".gif") {
        "image/gif"
    } else if base_name.ends_with(".webp") {
        "image/webp"
    } else {
        "image/png"
    };

    let image_data = if is_encrypted {
        // 解密文件内容
        decrypt_with_master_key(row_key, &data)
            .map_err(|e| format!("Failed to decrypt image: {}", e))?
    } else {
        data
    };

    let base64_data =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image_data);

    Ok((base64_data, mime_type.to_string()))
}

/// 迁移统计结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub total_files: usize,
    pub migrated: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

impl StorageState {
    /// 扫描并加密所有明文截图文件
    /// 这会：
    /// 1. 扫描 screenshots 目录中的所有非 .enc 文件
    /// 2. 加密每个文件并保存为 .enc 格式
    /// 3. 更新数据库中的路径
    /// 4. 删除原始明文文件
    pub fn migrate_plaintext_screenshots(&self) -> Result<MigrationResult, String> {
        let mut result = MigrationResult {
            total_files: 0,
            migrated: 0,
            skipped: 0,
            errors: Vec::new(),
        };

        // 扫描 screenshots 目录
        let entries = std::fs::read_dir(&self.screenshot_dir)
            .map_err(|e| format!("Failed to read screenshot directory: {}", e))?;

        let plaintext_files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                let ext = path.extension().and_then(|e| e.to_str());
                // 只处理非加密的图片文件
                matches!(
                    ext,
                    Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp")
                )
            })
            .collect();

        result.total_files = plaintext_files.len();

        for entry in plaintext_files {
            let path = entry.path();
            let path_str = path.to_string_lossy().to_string();

            match self.encrypt_single_file(&path) {
                Ok((new_path, encrypted_key)) => {
                    // 更新数据库中的路径
                    if let Err(e) =
                        self.update_screenshot_path(&path_str, &new_path, &encrypted_key)
                    {
                        result
                            .errors
                            .push(format!("Failed to update DB for {}: {}", path_str, e));
                    }

                    // 删除原始文件
                    if let Err(e) = std::fs::remove_file(&path) {
                        result
                            .errors
                            .push(format!("Failed to delete {}: {}", path_str, e));
                    } else {
                        result.migrated += 1;
                        println!("[storage] Migrated: {} -> {}", path_str, new_path);
                    }
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to encrypt {}: {}", path_str, e));
                }
            }
        }

        result.skipped = result.total_files - result.migrated - result.errors.len();

        Ok(result)
    }

    /// 加密单个文件
    fn encrypt_single_file(&self, path: &Path) -> Result<(String, Vec<u8>), String> {
        // 读取文件
        let data = std::fs::read(path).map_err(|e| format!("Failed to read file: {}", e))?;

        // 使用行级密钥加密
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);
        let encrypted = encrypt_with_master_key(&row_key, &data)
            .map_err(|e| format!("Failed to encrypt: {}", e))?;
        let encrypted_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap row key: {}", e))?;
        Self::zeroize_bytes(&mut row_key);

        // 生成新文件名
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let new_file_name = format!("{}.enc", file_name);
        let new_path = self.screenshot_dir.join(&new_file_name);

        // 保存加密文件
        std::fs::write(&new_path, &encrypted)
            .map_err(|e| format!("Failed to write encrypted file: {}", e))?;

        Ok((new_path.to_string_lossy().to_string(), encrypted_key))
    }

    /// 更新数据库中的截图路径
    fn update_screenshot_path(
        &self,
        old_path: &str,
        new_path: &str,
        encrypted_key: &[u8],
    ) -> Result<(), String> {
        let guard = self.get_connection()?;
        let conn = guard.as_ref().unwrap();

        conn.execute(
            "UPDATE screenshots SET image_path = ?, content_key_encrypted = ? WHERE image_path = ?",
            params![new_path, encrypted_key, old_path],
        )
        .map_err(|e| format!("Failed to update screenshot path: {}", e))?;

        Ok(())
    }

    /// 列出所有明文（未加密）的截图文件
    pub fn list_plaintext_screenshots(&self) -> Result<Vec<String>, String> {
        let entries = std::fs::read_dir(&self.screenshot_dir)
            .map_err(|e| format!("Failed to read screenshot directory: {}", e))?;

        let files: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                let ext = path.extension().and_then(|e| e.to_str());
                matches!(
                    ext,
                    Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp")
                )
            })
            .map(|e| e.path().to_string_lossy().to_string())
            .collect();

        Ok(files)
    }

    /// 安全删除所有明文截图文件（不迁移，直接删除）
    pub fn delete_plaintext_screenshots(&self) -> Result<usize, String> {
        let files = self.list_plaintext_screenshots()?;
        let mut deleted = 0;

        for file in &files {
            if let Err(e) = std::fs::remove_file(file) {
                eprintln!("Failed to delete {}: {}", file, e);
            } else {
                deleted += 1;
            }
        }

        Ok(deleted)
    }
}
