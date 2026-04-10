//! Database initialization, table creation, and schema migration.

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
};
use rusqlite::{params, Connection};
use std::sync::atomic::Ordering;

use super::StorageState;

impl StorageState {
    /// Initialize storage (create directories and database).
    pub fn initialize(&self) -> Result<(), String> {
        let init_start = std::time::Instant::now();
        let mut initialized = self.initialized.lock().unwrap_or_else(|e| e.into_inner());
        if *initialized {
            return Ok(());
        }

        // Create directories
        let data_dir = self.data_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let screenshot_dir = self.screenshot_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();

        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;
        std::fs::create_dir_all(&screenshot_dir)
            .map_err(|e| format!("Failed to create screenshot directory: {}", e))?;

        let t0 = std::time::Instant::now();
        // Derive weak database key from public key (no user authentication required)
        let public_key = get_cached_public_key(&self.credential_state)
            .or_else(|| load_public_key_from_file(&self.credential_state).ok())
            .ok_or_else(|| "Public key not initialized".to_string())?;
        let db_key = derive_db_key_from_public_key(&public_key);
        let key_derive_dur = t0.elapsed();

        // Open SQLCipher encrypted database
        let t1 = std::time::Instant::now();
        let db_path = data_dir.join("screenshots.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?;
        let open_dur = t1.elapsed();

        // Set SQLCipher key (hex format)
        let t2 = std::time::Instant::now();
        let key_hex = hex::encode(&db_key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", key_hex))
            .map_err(|e| format!("Failed to set database key: {}", e))?;

        // Verify that the key is correct
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|e| format!("Database key verification failed: {}", e))?;
        let pragma_dur = t2.elapsed();

        // Initialize table schema
        let t3 = std::time::Instant::now();
        self.init_tables(&conn)?;
        Self::set_auto_vacuum_incremental(&conn)?;
        let tables_dur = t3.elapsed();

        *self.db.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn);

        // Initialize approximate OCR row count using MAX(id) — O(log N) via primary key index.
        // AUTOINCREMENT ids only increase, so MAX(id) >= actual row count; acceptable for IDF.
        {
            let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(conn) = guard.as_ref() {
                let approx_count: i64 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(id), 0) FROM ocr_results",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                self.ocr_row_count
                    .store(approx_count as u64, Ordering::Relaxed);
            }
        }

        *initialized = true;

        tracing::info!(
            "[DIAG:INIT] SQLCipher initialized in {:?} (key_derive={:?}, db_open={:?}, pragma={:?}, init_tables={:?})",
            init_start.elapsed(),
            key_derive_dur,
            open_dur,
            pragma_dur,
            tables_dur
        );

        Ok(())
    }

    /// Shut down storage: close database connection.
    pub fn shutdown(&self) -> Result<(), String> {
        self.lazy_indexer_shutdown.store(true, Ordering::SeqCst);
        let mut db_guard = self.db.lock().map_err(|e| format!("lock error: {}", e))?;
        if db_guard.is_some() {
            *db_guard = None;
        }
        let mut init = self
            .initialized
            .lock()
            .map_err(|e| format!("lock error: {}", e))?;
        *init = false;
        Ok(())
    }

    /// Initialize database tables.
    fn init_tables(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            -- Screenshot records
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
                -- Field-level encryption columns
                window_title_enc BLOB,
                process_name_enc BLOB,
                metadata_enc BLOB,
                content_key_encrypted BLOB,
                -- Soft delete marker (1 = pending physical cleanup)
                is_deleted INTEGER NOT NULL DEFAULT 0
            );

            -- OCR results
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
                is_deleted INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            );

            -- Deferred physical cleanup queues
            CREATE TABLE IF NOT EXISTS delete_queue_screenshots (
                id INTEGER PRIMARY KEY
            );

            CREATE TABLE IF NOT EXISTS delete_queue_ocr (
                id INTEGER PRIMARY KEY
            );

            -- Blind bigram bitmap index table (stores postings as RoaringBitmap)
            CREATE TABLE IF NOT EXISTS blind_bitmap_index (
                token_hash TEXT PRIMARY KEY,
                postings_blob BLOB NOT NULL
            );

            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_image_hash ON screenshots(image_hash);
            CREATE INDEX IF NOT EXISTS idx_text_hash ON ocr_results(text_hash);
            CREATE INDEX IF NOT EXISTS idx_screenshot_id ON ocr_results(screenshot_id);
            CREATE INDEX IF NOT EXISTS idx_created_at ON screenshots(created_at);
            CREATE INDEX IF NOT EXISTS idx_process_name ON screenshots(process_name);

            -- Content-addressed dedup table for favicons
            CREATE TABLE IF NOT EXISTS page_icons (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content_hash TEXT UNIQUE NOT NULL,
                icon_enc BLOB NOT NULL,
                icon_key_encrypted BLOB NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            -- Content-addressed dedup table for link sets
            CREATE TABLE IF NOT EXISTS link_sets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content_hash TEXT UNIQUE NOT NULL,
                links_enc BLOB NOT NULL,
                links_key_encrypted BLOB NOT NULL,
                link_count INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            -- Enable foreign key constraints
            PRAGMA foreign_keys = ON;

            -- Generic key-value store for app-level metadata / migration markers
            CREATE TABLE IF NOT EXISTS app_metadata (
                key TEXT PRIMARY KEY,
                value TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );
            "#,
        )
        .map_err(|e| format!("Failed to initialize tables: {}", e))?;

        // If this is a fresh install (ocr_results is empty), mark HMAC v2 migration as done.
        // This prevents the lazy indexer from blocking on a fresh install.
        let ocr_empty: bool = conn
            .query_row("SELECT 1 FROM ocr_results LIMIT 1", [], |_| Ok(false))
            .unwrap_or(true);
        if ocr_empty {
            conn.execute(
                "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, ?2)",
                ["hmac_v2_migration_done", "true"],
            )
            .ok();
        }

        self.ensure_schema(conn)?;

        Ok(())
    }

    const AUTO_VACUUM_SENTINEL_PREFIX: &'static str = "auto_vacuum_incremental_done_v";

    fn startup_vacuum_sentinel_key() -> String {
        format!(
            "{}{}",
            Self::AUTO_VACUUM_SENTINEL_PREFIX,
            env!("CARGO_PKG_VERSION")
        )
    }

    fn set_auto_vacuum_incremental(conn: &Connection) -> Result<(), String> {
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
            .map_err(|e| format!("Failed to set PRAGMA auto_vacuum=INCREMENTAL: {}", e))
    }

    fn is_startup_vacuum_pending(conn: &Connection) -> bool {
        let sentinel_key = Self::startup_vacuum_sentinel_key();
        let done: bool = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                params![sentinel_key],
                |_| Ok(true),
            )
            .unwrap_or(false);
        !done
    }

    pub fn check_startup_vacuum_needed(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("startup_vacuum_check")?;
        let conn = guard.as_ref().unwrap();
        Self::set_auto_vacuum_incremental(conn)?;
        Ok(Self::is_startup_vacuum_pending(conn))
    }

    /// Run the versioned one-time full VACUUM if needed.
    pub fn run_startup_vacuum_if_needed(&self) -> Result<bool, String> {
        if self
            .startup_vacuum_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ALREADY_RUNNING".to_string());
        }

        let result = (|| {
            let guard = self.get_connection_named("startup_vacuum_run")?;
            let conn = guard.as_ref().unwrap();
            Self::set_auto_vacuum_incremental(conn)?;

            if !Self::is_startup_vacuum_pending(conn) {
                return Ok(false);
            }

            let version = env!("CARGO_PKG_VERSION");
            let sentinel_key = Self::startup_vacuum_sentinel_key();

            tracing::info!(
                "[DB] First startup for version {}, running full VACUUM for incremental auto_vacuum",
                version
            );
            conn.execute_batch("VACUUM;").map_err(|e| {
                format!(
                    "Failed to run full VACUUM for incremental auto_vacuum: {}",
                    e
                )
            })?;

            conn.execute(
                "INSERT OR IGNORE INTO app_metadata (key, value) VALUES (?1, ?2)",
                params![sentinel_key, version],
            )
            .map_err(|e| format!("Failed to write auto_vacuum sentinel: {}", e))?;

            Ok(true)
        })();

        self.startup_vacuum_in_progress
            .store(false, Ordering::SeqCst);

        result
    }

    /// Run full VACUUM manually from UI.
    ///
    /// Also writes current-version sentinel so next startup does not re-run
    /// the one-time startup VACUUM.
    pub fn run_manual_vacuum(&self) -> Result<(), String> {
        if self
            .startup_vacuum_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ALREADY_RUNNING".to_string());
        }

        let result = (|| {
            let guard = self.get_connection_named("manual_vacuum_run")?;
            let conn = guard.as_ref().unwrap();
            Self::set_auto_vacuum_incremental(conn)?;

            tracing::info!("[DB] Manual VACUUM started from settings");
            conn.execute_batch("VACUUM;")
                .map_err(|e| format!("Failed to run manual VACUUM: {}", e))?;

            let version = env!("CARGO_PKG_VERSION");
            let sentinel_key = Self::startup_vacuum_sentinel_key();
            conn.execute(
                "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, ?2)",
                params![sentinel_key, version],
            )
            .map_err(|e| format!("Failed to update auto_vacuum sentinel after manual VACUUM: {}", e))?;

            Ok(())
        })();

        self.startup_vacuum_in_progress
            .store(false, Ordering::SeqCst);

        result
    }

    /// Ensure backward compatibility by adding missing columns to existing tables.
    fn ensure_schema(&self, conn: &Connection) -> Result<(), String> {
        Self::add_column_if_missing(conn, "screenshots", "window_title_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "process_name_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "metadata_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "content_key_encrypted", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "is_deleted", "INTEGER NOT NULL DEFAULT 0")?;
        // Add status and committed_at for two-phase screenshot lifecycle
        Self::add_column_if_missing(conn, "screenshots", "status", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "committed_at", "TIMESTAMP")?;

        Self::add_column_if_missing(conn, "ocr_results", "text_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "ocr_results", "text_key_encrypted", "BLOB")?;
        Self::add_column_if_missing(conn, "ocr_results", "is_deleted", "INTEGER NOT NULL DEFAULT 0")?;

        // Browser extension metadata columns
        Self::add_column_if_missing(conn, "screenshots", "source", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "page_url_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "page_icon_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "visible_links_enc", "BLOB")?;

        // Content-addressed dedup references
        Self::add_column_if_missing(conn, "screenshots", "page_icon_id", "INTEGER")?;
        Self::add_column_if_missing(conn, "screenshots", "link_set_id", "INTEGER")?;

        // Classification columns
        Self::add_column_if_missing(conn, "screenshots", "category", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "category_confidence", "REAL")?;

        // Task clustering tables
        Self::create_table_if_missing(
            conn,
            "tasks",
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                label TEXT,
                auto_label TEXT,
                dominant_process TEXT,
                dominant_category TEXT,
                start_time REAL,
                end_time REAL,
                snapshot_count INTEGER DEFAULT 0,
                layer TEXT DEFAULT 'hot',
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )?;
        Self::create_table_if_missing(
            conn,
            "task_assignments",
            r#"
            CREATE TABLE IF NOT EXISTS task_assignments (
                screenshot_id INTEGER NOT NULL,
                task_id INTEGER NOT NULL,
                confidence REAL,
                PRIMARY KEY (screenshot_id, task_id),
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE,
                FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
            )
            "#,
        )?;

        // Index for reverse lookup: task_id → screenshot_ids
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_task_assignments_task_id ON task_assignments(task_id)",
        ).map_err(|e| format!("Failed to create task_assignments index: {}", e))?;

        // Staging table for bitmap index migration (same structure as blind_bitmap_index)
        Self::create_table_if_missing(
            conn,
            "blind_bitmap_index_staging",
            r#"
            CREATE TABLE IF NOT EXISTS blind_bitmap_index_staging (
                token_hash TEXT PRIMARY KEY,
                postings_blob BLOB NOT NULL
            )
            "#,
        )?;

        Self::create_table_if_missing(
            conn,
            "delete_queue_screenshots",
            r#"
            CREATE TABLE IF NOT EXISTS delete_queue_screenshots (
                id INTEGER PRIMARY KEY
            )
            "#,
        )?;

        Self::create_table_if_missing(
            conn,
            "delete_queue_ocr",
            r#"
            CREATE TABLE IF NOT EXISTS delete_queue_ocr (
                id INTEGER PRIMARY KEY
            )
            "#,
        )?;

        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_screenshots_deleted_created_at ON screenshots(is_deleted, created_at);
            CREATE INDEX IF NOT EXISTS idx_screenshots_process_deleted_created_at ON screenshots(process_name, is_deleted, created_at);
            CREATE INDEX IF NOT EXISTS idx_ocr_deleted_screenshot ON ocr_results(is_deleted, screenshot_id);
            "#,
        )
        .map_err(|e| format!("Failed to create soft-delete indexes: {}", e))?;

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

    fn create_table_if_missing(
        conn: &Connection,
        _table_name: &str,
        create_sql: &str,
    ) -> Result<(), String> {
        conn.execute_batch(create_sql)
            .map_err(|e| format!("Failed to create table {}: {}", _table_name, e))?;
        Ok(())
    }
}
