//! 存储管理模块 - SQLCipher 加密数据库和截图文件管理
//!
//! 该模块提供：
//! 1. 加密SQLite 数据库存储（使用 SQLCipher）
//! 2. 截图文件的存储和管理
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
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri::Emitter;
use walkdir::WalkDir;
use serde_json::json;

struct MigrationRunGuard<'a> {
    in_progress: &'a AtomicBool,
    cancel_requested: &'a AtomicBool,
}

impl<'a> MigrationRunGuard<'a> {
    fn new(in_progress: &'a AtomicBool, cancel_requested: &'a AtomicBool) -> Self {
        Self {
            in_progress,
            cancel_requested,
        }
    }
}

impl Drop for MigrationRunGuard<'_> {
    fn drop(&mut self) {
        self.in_progress.store(false, Ordering::SeqCst);
        self.cancel_requested.store(false, Ordering::SeqCst);
    }
}

/// 存储管理器状
pub struct StorageState {
    /// 数据库连
    db: Mutex<Option<Connection>>,
    /// 数据目录
    pub data_dir: Mutex<PathBuf>,
    /// 截图目录
    pub screenshot_dir: Mutex<PathBuf>,
    /// 凭证管理器状态引
    credential_state: Arc<CredentialManagerState>,
    /// 是否已初始化
    initialized: Mutex<bool>,
    migration_cancel_requested: AtomicBool,
    migration_in_progress: AtomicBool,
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
    pub image_data: String, // Base64 编码的图片数
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
            data_dir: Mutex::new(data_dir),
            screenshot_dir: Mutex::new(screenshot_dir),
            credential_state,
            initialized: Mutex::new(false),
            migration_cancel_requested: AtomicBool::new(false),
            migration_in_progress: AtomicBool::new(false),
        }
    }

    /// 安全关闭存储，释放数据库连接和其他句
    pub fn request_migration_cancel(&self) -> bool {
        self.migration_cancel_requested.store(true, Ordering::SeqCst);
        self.migration_in_progress.load(Ordering::SeqCst)
    }

    fn is_migration_cancel_requested(&self) -> bool {
        self.migration_cancel_requested.load(Ordering::SeqCst)
    }

    fn rollback_partial_migration(copied_files: &[PathBuf], created_dirs: &mut Vec<PathBuf>) {
        for file in copied_files.iter().rev() {
            let _ = std::fs::remove_file(file);
        }

        created_dirs.sort();
        created_dirs.dedup();
        created_dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));
        for dir in created_dirs.iter() {
            let _ = std::fs::remove_dir(dir);
        }
    }

    fn restore_source_and_reinitialize(
        &self,
        app_handle: &AppHandle,
        src: &PathBuf,
        message: String,
        cancelled: bool,
    ) -> Result<serde_json::Value, String> {
        {
            let mut data_guard = self.data_dir.lock().unwrap();
            *data_guard = src.clone();
            let mut ss_guard = self.screenshot_dir.lock().unwrap();
            *ss_guard = src.join("screenshots");
        }

        if let Err(e) = self.initialize() {
            let msg = format!("{}; failed to reinitialize source storage: {}", message, e);
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false, "cancelled": cancelled }),
            );
            return Err(msg);
        }

        let _ = app_handle.emit(
            "storage-migration-error",
            json!({ "message": message.clone(), "recoverable": true, "cancelled": cancelled }),
        );

        Err(message)
    }

    pub fn shutdown(&self) -> Result<(), String> {
        // take and drop connection
        let mut db_guard = self.db.lock().map_err(|e| format!("lock error: {}", e))?;
        if db_guard.is_some() {
            *db_guard = None;
        }
        let mut init = self.initialized.lock().map_err(|e| format!("lock error: {}", e))?;
        *init = false;
        Ok(())
    }

    /// 更新 data 目录。可选执行完整迁移（copy + remove），并过 app_handle 发出进度事件    /// 返回 JSON 对象 { target: String, migrated: bool }
    pub fn migrate_data_dir_blocking(
        &self,
        app_handle: AppHandle,
        target: String,
        migrate_data_files: bool,
    ) -> Result<serde_json::Value, String> {
        if self.migration_in_progress.swap(true, Ordering::SeqCst) {
            return Err("A storage migration is already in progress".to_string());
        }
        self.migration_cancel_requested.store(false, Ordering::SeqCst);
        let _migration_guard =
            MigrationRunGuard::new(&self.migration_in_progress, &self.migration_cancel_requested);

        let src = self.data_dir.lock().unwrap().clone();
        let dst = PathBuf::from(&target);
        let mut copied_files: Vec<PathBuf> = Vec::new();
        let mut created_dirs: Vec<PathBuf> = Vec::new();
        let mut source_removed = false;

        if let Err(e) = self.shutdown() {
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": format!("Failed to shutdown storage: {}", e), "recoverable": false }),
            );
            return Err(format!("Failed to shutdown storage: {}", e));
        }

        if self.is_migration_cancel_requested() {
            return self.restore_source_and_reinitialize(
                &app_handle,
                &src,
                "Migration cancelled by user".to_string(),
                true,
            );
        }

        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                let msg = format!("Failed to create target parent dirs: {}", e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
            }
        }

        let should_migrate_files = migrate_data_files && src != dst;

        if should_migrate_files {
            let mut total_files: usize = 0;
            for entry in WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    total_files += 1;
                }
            }

            if total_files == 0 {
                let existed = dst.exists();
                if let Err(e) = std::fs::create_dir_all(&dst) {
                    let msg = format!("Failed to create target dir: {}", e);
                    let _ = app_handle.emit(
                        "storage-migration-error",
                        json!({ "message": msg.clone(), "recoverable": false }),
                    );
                    return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
                }
                if !existed {
                    created_dirs.push(dst.clone());
                }
            }

            let mut copied: usize = 0;

            for entry in WalkDir::new(&src).into_iter().filter_map(|e| e.ok()) {
                if self.is_migration_cancel_requested() {
                    Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                    return self.restore_source_and_reinitialize(
                        &app_handle,
                        &src,
                        "Migration cancelled by user".to_string(),
                        true,
                    );
                }

                let rel_path = match entry.path().strip_prefix(&src) {
                    Ok(p) => p.to_path_buf(),
                    Err(_) => continue,
                };
                let target_path = dst.join(&rel_path);

                if entry.file_type().is_dir() {
                    let existed = target_path.exists();
                    if let Err(e) = std::fs::create_dir_all(&target_path) {
                        let msg = format!("Failed to create dir {}: {}", target_path.display(), e);
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                    if !existed {
                        created_dirs.push(target_path.clone());
                    }
                    continue;
                }

                if let Some(parent) = target_path.parent() {
                    let existed = parent.exists();
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        let msg = format!(
                            "Failed to create parent for file {}: {}",
                            target_path.display(),
                            e
                        );
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                    if !existed {
                        created_dirs.push(parent.to_path_buf());
                    }
                }

                match std::fs::copy(entry.path(), &target_path) {
                    Ok(_) => {
                        copied += 1;
                        copied_files.push(target_path.clone());
                        let _ = app_handle.emit(
                            "storage-migration-progress",
                            json!({
                                "total_files": total_files,
                                "copied_files": copied,
                                "current_file": entry.path().to_string_lossy()
                            }),
                        );
                    }
                    Err(e) => {
                        let msg = format!("Failed to copy {}: {}", entry.path().display(), e);
                        Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                        let _ = app_handle.emit(
                            "storage-migration-error",
                            json!({ "message": msg.clone(), "recoverable": false }),
                        );
                        return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
                    }
                }
            }

            if self.is_migration_cancel_requested() {
                Self::rollback_partial_migration(&copied_files, &mut created_dirs);
                return self.restore_source_and_reinitialize(
                    &app_handle,
                    &src,
                    "Migration cancelled by user".to_string(),
                    true,
                );
            }

            if let Err(e) = std::fs::remove_dir_all(&src) {
                let msg = format!("Failed to remove source dir {}: {}", src.display(), e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return Err(msg);
            }
            source_removed = true;
        } else {
            let existed = dst.exists();
            if let Err(e) = std::fs::create_dir_all(&dst) {
                let msg = format!("Failed to create target dir: {}", e);
                let _ = app_handle.emit(
                    "storage-migration-error",
                    json!({ "message": msg.clone(), "recoverable": false }),
                );
                return self.restore_source_and_reinitialize(&app_handle, &src, msg, false);
            }
            if !existed {
                created_dirs.push(dst.clone());
            }
        }

        if self.is_migration_cancel_requested() && !source_removed {
            Self::rollback_partial_migration(&copied_files, &mut created_dirs);
            return self.restore_source_and_reinitialize(
                &app_handle,
                &src,
                "Migration cancelled by user".to_string(),
                true,
            );
        }

        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
        let cfg_path = PathBuf::from(local_appdata).join("CarbonPaper").join("config.json");
        if let Some(parent) = cfg_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let cfg = json!({ "data_dir": target });
        if let Err(e) = std::fs::write(
            &cfg_path,
            serde_json::to_string_pretty(&cfg).unwrap_or_default(),
        ) {
            let msg = format!("Failed to write config file {}: {}", cfg_path.display(), e);
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false }),
            );
            return Err(msg);
        }

        {
            let mut data_guard = self.data_dir.lock().unwrap();
            *data_guard = dst.clone();
            let mut ss_guard = self.screenshot_dir.lock().unwrap();
            *ss_guard = dst.join("screenshots");
        }

        if let Err(e) = self.initialize() {
            let msg = format!("Failed to reinitialize storage after migration: {}", e);
            let _ = app_handle.emit(
                "storage-migration-error",
                json!({ "message": msg.clone(), "recoverable": false }),
            );
            return Err(msg);
        }

        let _ = app_handle.emit(
            "storage-migration-done",
            json!({ "target": target, "migrated": should_migrate_files }),
        );

        Ok(json!({ "target": dst.to_string_lossy(), "migrated": should_migrate_files }))
    }

    /// 初始化存储（创建目录和数据库
    pub fn initialize(&self) -> Result<(), String> {
        let mut initialized = self.initialized.lock().unwrap();
        if *initialized {
            return Ok(());
        }

        // 创建目录
        let data_dir = self.data_dir.lock().unwrap().clone();
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;
        std::fs::create_dir_all(&screenshot_dir)
            .map_err(|e| format!("Failed to create screenshot directory: {}", e))?;

        // 使用公钥派生弱数据库密钥（无霢用户认证
        let public_key = get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())?;
        let db_key = derive_db_key_from_public_key(&public_key);

        // 打开 SQLCipher 加密数据
        let db_path = data_dir.join("screenshots.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;

        // 设置 SQLCipher 密钥（使hex 格式
        let key_hex = hex::encode(&db_key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex))
            .map_err(|e| format!("Failed to set database key: {}", e))?;

        // 验证密钥是否正确
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|e| format!("Database key verification failed: {}", e))?;

        // 初始化表结构
        self.init_tables(&conn)?;

        *self.db.lock().unwrap() = Some(conn);

        // 初始化盲三元组位图索引表到主数据库（取代原来的测试数据库
        // init_tables() 已包blind_bitmap_index 的表创建

        *initialized = true;

        println!("[storage] SQLCipher weakly encrypted database initialized");

        Ok(())
    }

    /// 将存储策略写入应用目录下storage_policy.json
    pub fn save_policy(&self, policy: &JsonValue) -> Result<(), String> {
        // policy file placed at <data_dir_parent>/storage_policy.json
        let mut cfg_dir = self.data_dir.lock().unwrap().clone();
        if let Some(parent) = cfg_dir.parent() {
            cfg_dir = parent.to_path_buf();
        }
        let policy_path = cfg_dir.join("storage_policy.json");

        let s = serde_json::to_string_pretty(policy).map_err(|e| format!("serde json error: {}", e))?;
        std::fs::write(&policy_path, s).map_err(|e| format!("failed to write policy file: {}", e))
    }

    /// 从应用目录读storage_policy.json，如果不存在返回空对
    pub fn load_policy(&self) -> Result<JsonValue, String> {
        let mut cfg_dir = self.data_dir.lock().unwrap().clone();
        if let Some(parent) = cfg_dir.parent() {
            cfg_dir = parent.to_path_buf();
        }
        let policy_path = cfg_dir.join("storage_policy.json");

        if !policy_path.exists() {
            return Ok(serde_json::json!({}));
        }

        let content = std::fs::read_to_string(&policy_path).map_err(|e| format!("failed to read policy file: {}", e))?;
        let v: JsonValue = serde_json::from_str(&content).map_err(|e| format!("failed to parse policy json: {}", e))?;
        Ok(v)
    }

    /// 初始化数据库表结
    fn init_tables(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            -- 截图记录
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
            
            -- OCR 结果
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

            -- 盲三元组位图索引表（使用 RoaringBitmap 存储 postings
            CREATE TABLE IF NOT EXISTS blind_bitmap_index (
                token_hash TEXT PRIMARY KEY,
                postings_blob BLOB NOT NULL
            );
            
            -- 索引
            CREATE INDEX IF NOT EXISTS idx_image_hash ON screenshots(image_hash);
            CREATE INDEX IF NOT EXISTS idx_text_hash ON ocr_results(text_hash);
            CREATE INDEX IF NOT EXISTS idx_screenshot_id ON ocr_results(screenshot_id);
            CREATE INDEX IF NOT EXISTS idx_created_at ON screenshots(created_at);
            CREATE INDEX IF NOT EXISTS idx_process_name ON screenshots(process_name);
            
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

    /// 计算数据 MD5 哈希
    fn compute_hash(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let result = Sha256::digest(data);
        hex::encode(result)
    }

    /// HMAC 用于盲索引
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

            // 过滤掉纯标点符号或特殊字符（虽然上面trim 处理了一部分，但以防万一
            // 棢查是否包含至少一个有效字(CJK 字母数字)
            let has_valid_char = normalized
                .chars()
                .any(|c| c.is_ascii_alphanumeric() || Self::is_cjk(c));

            if !has_valid_char {
                continue;
            }

            //    过滤掉单字符的英数字 ("a", "1")，保留单字符的中(") 视需求定
            //    这里演示：如果是 ASCII 且长度为 1，则丢弃
            if normalized.len() == 1 && normalized.chars().next().unwrap().is_ascii() {
                continue;
            }

            unique_tokens.insert(normalized);
        }

        // 转回 Vec
        unique_tokens.into_iter().collect()
    }

    /// 二元组分
    fn bigram_tokenize(text: &str) -> HashSet<String> {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() < 2 {
            return HashSet::new(); // 忽略过短的文
        }

        chars.windows(2).map(|w| w.iter().collect()).collect()
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

    /// ChromaDB 加密文本
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

    /// 解密来自 ChromaDB 的文本
    pub fn decrypt_from_chromadb(&self, encrypted: &str) -> Result<String, String> {
        if encrypted.is_empty()
            || (!encrypted.starts_with("ENC2:") && !encrypted.starts_with("ENC:"))
        {
            return Ok(encrypted.to_string());
        }

        if encrypted.starts_with("ENC:") {
            // 如果是奇怪的旧格式，直接返回错误提示
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

    /// 获取公钥（兼容旧 IPC/接口
    pub fn get_public_key(&self) -> Result<Vec<u8>, String> {
        get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())
    }

    /// 棢查截图是否已存在
    pub fn screenshot_exists(&self, image_hash: &str) -> Result<bool, String> {
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshots WHERE image_hash = ?)",
                [image_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check screenshot: {}", e))?;

        Ok(count > 0)
    }

    /// 保存截图和 OCR 结果
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

        // 生成截图RowKey（用于图片与元数据加密）
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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let image_path = screenshot_dir.join(&filename);

        // 保存加密后的图片文件
        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = image_path.to_string_lossy().to_string();

        // 保存到数据库（SQLCipher 整库加密
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

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

                    // 使用主数据库维护盲三元组位图索引
                    let ocr_id = conn.last_insert_rowid();
                    let triple_tokens = Self::bigram_tokenize(&result.text);
                    let tx = conn
                        .transaction()
                        .map_err(|e| format!("Failed to start transaction: {}", e))?;

                    let mut get_stmt = tx.prepare_cached(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                    ).map_err(|e| format!("Failed to prepare get statement: {}", e))?;
                    let mut put_stmt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)"
                    ).map_err(|e| format!("Failed to prepare put statement: {}", e))?;

                    for token in triple_tokens {
                        let token_hash = Self::compute_hmac_hash(&token);
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

                        put_stmt.execute(params![&token_hash, &serialized_blob]).map_err(|e| format!("Failed to update blind bitmap index: {}", e))?;
                    }

                    // 释放 transaction 资源
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

        // 生成 row key 并加密图
        let mut row_key = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut row_key);

        let encrypted_image = encrypt_with_master_key(&row_key, &image_data)
            .map_err(|e| format!("Failed to encrypt image: {}", e))?;
        let encrypted_row_key = encrypt_row_key_with_cng(&row_key)
            .map_err(|e| format!("Failed to wrap image row key: {}", e))?;

        // 使用 .pending 后缀标记临时文件
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
        let filename = format!("screenshot_{}.png.enc.pending", timestamp);
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let image_path = screenshot_dir.join(&filename);

        std::fs::write(&image_path, &encrypted_image)
            .map_err(|e| format!("Failed to save encrypted image file: {}", e))?;

        let image_path_str = image_path.to_string_lossy().to_string();

        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

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
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

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
                    // 如果重命名失败，记录但继续尝试插OCR 结果
                    eprintln!("Failed to rename pending image file: {}", e);
                } else {
                    final_image_path_str = new_path.to_string_lossy().to_string();
                }
            }
        }

        let mut added = 0;
                    let skipped = 0;

        // 插入 OCR 结果（如果有），并更新盲三元组位图索
        if let Some(results) = ocr_results {
            for result in results {
                let text_hash = Self::compute_hmac_hash(&result.text);
                let (text_enc, text_key_encrypted) = self.encrypt_payload_with_row_key(result.text.as_bytes())?;

                // 插入 OCR 结果
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

                let ocr_id = conn.last_insert_rowid();
                let triple_tokens = Self::bigram_tokenize(&result.text);
                let tx = conn
                    .transaction()
                    .map_err(|e| format!("Failed to start transaction: {}", e))?;

                let mut get_stmt = tx.prepare_cached(
                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?1",
                ).map_err(|e| format!("Failed to prepare get statement: {}", e))?;
                let mut put_stmt = tx.prepare_cached(
                    "INSERT OR REPLACE INTO blind_bitmap_index (token_hash, postings_blob) VALUES (?1, ?2)"
                ).map_err(|e| format!("Failed to prepare put statement: {}", e))?;

                for token in triple_tokens {
                    let token_hash = Self::compute_hmac_hash(&token);
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

                    put_stmt.execute(params![&token_hash, &serialized_blob]).map_err(|e| format!("Failed to update blind bitmap index: {}", e))?;
                }

                drop(put_stmt);
                drop(get_stmt);

                tx.commit()
                    .map_err(|e| format!("Failed to commit bitmap index: {}", e))?;

                added += 1;
            }
        }

        // 标记 committed 并设 committed_at，同时更改 image_path 为重命名后的路径
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
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

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

        if image_path.exists() {
            let _ = std::fs::remove_file(&image_path);
        }

        // 标记 aborted
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
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

        // 转换时间戳（秒）UTC 时间的日期时间字符串
        // SQLite CURRENT_TIMESTAMP 存储的是 UTC 时间
        let start_dt = DateTime::<Utc>::from_timestamp(start_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();

        // 使用直接 SQL（参数绑定在 SQLCipher 中可能有问题
        // start_dt end_dt 是我们生成的固定格式字符串，不存在 SQL 注入风险
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
        let mut guard = self.get_connection()?;
        let conn = guard.as_mut().unwrap();

        // 使用盲三元组位图索引进行搜索。如果 query 为空则化为按时间序的全文扫描（带时间进程过滤）
        let triple_tokens: Vec<String> = if query.is_empty() {
            Vec::new()
        } else {
            Self::bigram_tokenize(query).into_iter().collect()
        };

        // 如果没有三元token，则尝试对短查询使用基于词元的位图索引
        // 若词元也为空则化为简单的 SQL 查询（按时间排序）
        if triple_tokens.is_empty() {
            if !query.is_empty() {
                // 使用分词（短查询策略
                let tokens = Self::tokenize_text(query);
                if !tokens.is_empty() {
                    // blind_bitmap_index 获取 postings 并交
                    let mut bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
                    for token in &tokens {
                        let token_hash = Self::compute_hmac_hash(token);
                        let blob: Option<Vec<u8>> = conn
                            .query_row(
                                "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                                params![&token_hash],
                                |row| row.get(0),
                            )
                            .optional()
                            .map_err(|e| format!("Failed to query bitmap: {}", e))?;

                        if let Some(b) = blob {
                            let rb = roaring::RoaringBitmap::deserialize_from(&b[..])
                                .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
                            bitmaps.push(rb);
                        } else {
                            // 某个 token 没有 posting => 无匹配
                            return Ok(vec![]);
                        }
                    }

                    // 交集
                    let mut iter = bitmaps.into_iter();
                    let mut intersection = if let Some(first) = iter.next() {
                        first
                    } else {
                        roaring::RoaringBitmap::new()
                    };
                    for bm in iter {
                        intersection &= &bm;
                    }

                    if intersection.is_empty() {
                        return Ok(vec![]);
                    }

                    let mut ids: Vec<i64> = intersection.into_iter().map(|v| v as i64).collect();
                    ids.sort_unstable_by(|a, b| b.cmp(a));

                    // 分页
                    let start = offset as usize;
                    let end = std::cmp::min(ids.len(), (offset + limit) as usize);
                    let page_ids = if start < end { ids[start..end].to_vec() } else { Vec::new() };

                    if page_ids.is_empty() {
                        return Ok(vec![]);
                    }

                    // 构建 SQL 查询这些 ocr_result ids（复用后续解后处理辑
                    let placeholders: Vec<&str> = page_ids.iter().map(|_| "?").collect();
                    let sql = format!(
                        "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                                r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                                r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                                s.image_path, s.window_title_enc, s.process_name_enc,
                                s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
                         FROM ocr_results r
                         JOIN screenshots s ON r.screenshot_id = s.id
                         WHERE r.id IN ({})
                         ORDER BY s.created_at DESC, r.id DESC",
                        placeholders.join(",")
                    );

                    let param_refs: Vec<&dyn rusqlite::ToSql> = page_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
                    let mut stmt = conn.prepare(&sql).map_err(|e| format!("Failed to prepare query: {}", e))?;

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

                    // 后处理：进程名和时间过滤
                    let filtered: Vec<SearchResult> = results
                        .into_iter()
                        .filter(|r| {
                            if let Some(ref names) = process_names {
                                if !names.is_empty() {
                                    if let Some(p) = &r.process_name {
                                        if !names.contains(p) {
                                            return false;
                                        }
                                    } else {
                                        return false;
                                    }
                                }
                            }

                            if let Some(start) = start_time {
                                if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(&r.screenshot_created_at, "%Y-%m-%d %H:%M:%S") {
                                    if (nd.timestamp() as f64) < start {
                                        return false;
                                    }
                                }
                            }
                            if let Some(end) = end_time {
                                if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(&r.screenshot_created_at, "%Y-%m-%d %H:%M:%S") {
                                    if (nd.timestamp() as f64) > end {
                                        return false;
                                    }
                                }
                            }

                            true
                        })
                        .collect();

                    return Ok(filtered);
                }
            }
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

            return Ok(filtered);
        }

        // 有三元组 tokens：从 blind_bitmap_index 获取 postings 并交集
        let mut bitmaps: Vec<roaring::RoaringBitmap> = Vec::new();
        for token in &triple_tokens {
            let token_hash = Self::compute_hmac_hash(token);
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash = ?",
                    params![&token_hash],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("Failed to query bitmap: {}", e))?;

            if let Some(b) = blob {
                let rb = roaring::RoaringBitmap::deserialize_from(&b[..])
                    .map_err(|e| format!("Failed to deserialize bitmap: {}", e))?;
                bitmaps.push(rb);
            } else {
                // 某个 token 没有 posting => 无匹
                return Ok(vec![]);
            }
        }

        // 交集
        let mut iter = bitmaps.into_iter();
        let mut intersection = if let Some(first) = iter.next() {
            first
        } else {
            roaring::RoaringBitmap::new()
        };
        for bm in iter {
            intersection &= &bm;
        }

        if intersection.is_empty() {
            return Ok(vec![]);
        }

        let mut ids: Vec<i64> = intersection.into_iter().map(|v| v as i64).collect();
        // id 降序（近似时间序列）
        ids.sort_unstable_by(|a, b| b.cmp(a));

        // 分页
        let start = offset as usize;
        let end = std::cmp::min(ids.len(), (offset + limit) as usize);
        let page_ids = if start < end { ids[start..end].to_vec() } else { Vec::new() };

        if page_ids.is_empty() {
            return Ok(vec![]);
        }

        // 构建 SQL 查询这些 ocr_result ids
        let placeholders: Vec<&str> = page_ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT r.id, r.screenshot_id, r.text_enc, r.text_key_encrypted, r.confidence,
                    r.box_x1, r.box_y1, r.box_x2, r.box_y2,
                    r.box_x3, r.box_y3, r.box_x4, r.box_y4,
                    s.image_path, s.window_title_enc, s.process_name_enc,
                    s.content_key_encrypted, r.created_at, s.created_at as screenshot_created_at
             FROM ocr_results r
             JOIN screenshots s ON r.screenshot_id = s.id
             WHERE r.id IN ({})
             ORDER BY s.created_at DESC, r.id DESC",
            placeholders.join(",")
        );

        let param_refs: Vec<&dyn rusqlite::ToSql> = page_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

        let mut stmt = conn.prepare(&sql).map_err(|e| format!("Failed to prepare query: {}", e))?;

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

        // 后处理：进程名和时间过滤
        let filtered: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| {
                if let Some(ref names) = process_names {
                    if !names.is_empty() {
                        if let Some(p) = &r.process_name {
                            if !names.contains(p) {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    }
                }

                if let Some(start) = start_time {
                    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(&r.screenshot_created_at, "%Y-%m-%d %H:%M:%S") {
                        if (nd.timestamp() as f64) < start {
                            return false;
                        }
                    }
                }
                if let Some(end) = end_time {
                    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(&r.screenshot_created_at, "%Y-%m-%d %H:%M:%S") {
                        if (nd.timestamp() as f64) > end {
                            return false;
                        }
                    }
                }

                true
            })
            .collect();

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

    /// 获取截图 OCR 结果
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

    /// 读取加密图片文件并返Base64 编码
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

/// 读取图片文件并返Base64 编码（支持加密文件）
#[allow(dead_code)]
pub fn read_image_as_base64(path: &str) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;

    // 更稳健地棢测是否为加密文件：文件名中包".enc" 即视为加密（兼容 .enc.pending
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // 获取实际MIME 类型：尝试从文件名（去掉 .enc/.pending 后缀）推
    let base_name = if is_encrypted {
        // 例如：screenshot_xxx.png.enc screenshot_xxx.png.enc.pending
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

    // 加密文件需要解密密钥，这里只能返回错误，需要使用带密钥的方
    if is_encrypted {
        return Err(
            "Encrypted image requires decryption key. Use read_encrypted_image_as_base64 instead."
                .to_string(),
        );
    }

    let base64_data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);

    Ok((base64_data, mime_type.to_string()))
}

/// 读取加密图片文件并返 Base64 编码（带解密）
pub fn read_encrypted_image_as_base64(
    path: &str,
    row_key: &[u8],
) -> Result<(String, String), String> {
    let path = Path::new(path);

    if !path.exists() {
        return Err(format!("Image file not found: {}", path.display()));
    }

    let data = std::fs::read(path).map_err(|e| format!("Failed to read image file: {}", e))?;
    // 更稳健地检测是否为加密文件：文件名中包含".enc" 即视为加密（兼容 .enc.pending）
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let is_encrypted = fname.contains(".enc");

    // 获取实际MIME 类型：尝试从文件名（去掉 .enc/.pending 后缀）推断
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
    /// 扫描并加密所有明文截图文
    /// 这会
    /// 1. 扫描 screenshots 目录中的扢有非 .enc 文件
    /// 2. 加密每个文件并保存为 .enc 格式
    /// 3. 更新数据库中的路
    /// 4. 删除原始明文文件
    pub fn migrate_plaintext_screenshots(&self) -> Result<MigrationResult, String> {
        let mut result = MigrationResult {
            total_files: 0,
            migrated: 0,
            skipped: 0,
            errors: Vec::new(),
        };

        // 扫描 screenshots 目录
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let entries = std::fs::read_dir(&screenshot_dir)
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
                    // 更新数据库中的截图路径
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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let new_path = screenshot_dir.join(&new_file_name);

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
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();
        let entries = std::fs::read_dir(&screenshot_dir)
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

    /// 安全删除所有明文截图文件（不迁移，直接删除
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
