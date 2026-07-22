//! Database initialization, table creation, and schema migration.

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
};
use rusqlite::{params, Connection};
use std::sync::atomic::Ordering;

use super::StorageState;

impl StorageState {
    const MCP_PRIVACY_ACKNOWLEDGED_KEY: &'static str = "mcp_privacy_acknowledged";

    /// Initialize storage (create directories and database).
    pub fn initialize(&self) -> Result<(), String> {
        let init_start = std::time::Instant::now();
        let mut initialized = self.initialized.lock().unwrap_or_else(|e| e.into_inner());
        if *initialized {
            return Ok(());
        }

        // Create directories
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

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
        self.cleanup_derived_index_sidecars_at_startup(&conn, &data_dir)?;
        Self::set_auto_vacuum_incremental(&conn)?;
        let tables_dur = t3.elapsed();

        *self.db.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn);

        // Initialize approximate OCR row count using MAX(id) — O(log N) via primary key index.
        // AUTOINCREMENT ids only increase, so MAX(id) >= actual row count; acceptable for IDF.
        {
            let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(conn) = guard.as_ref() {
                let approx_count: i64 = conn
                    .query_row("SELECT COALESCE(MAX(id), 0) FROM ocr_results", [], |row| {
                        row.get(0)
                    })
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
    pub(super) fn init_tables(&self, conn: &Connection) -> Result<(), String> {
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

            -- OCR lifecycle is kept separate from screenshot metadata so
            -- failed inference can remain retryable without changing the
            -- durable screenshot record.
            CREATE TABLE IF NOT EXISTS screenshot_ocr_status (
                screenshot_id INTEGER PRIMARY KEY,
                status TEXT NOT NULL DEFAULT 'pending',
                engine TEXT,
                model_id TEXT,
                execution_provider TEXT,
                error TEXT,
                elapsed_ms REAL,
                postprocess_status TEXT NOT NULL DEFAULT 'none',
                postprocess_error TEXT,
                postprocess_attempts INTEGER NOT NULL DEFAULT 0,
                postprocess_next_retry_at TIMESTAMP,
                attempted_at TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            );

            -- Rust-owned derived semantic vectors. SQLite is the durable cache;
            -- any ANN sidecar remains rebuildable from these rows.
            CREATE TABLE IF NOT EXISTS derived_embeddings (
                index_kind TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                dimensions INTEGER NOT NULL CHECK (dimensions > 0),
                vector_f32 BLOB NOT NULL,
                model_id TEXT NOT NULL,
                model_revision TEXT NOT NULL,
                embedding_version INTEGER NOT NULL CHECK (embedding_version > 0),
                source_fingerprint TEXT NOT NULL,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (index_kind, subject_key)
            );

            -- Per-subject migration/rebuild ledger. A vector is query-visible
            -- only while this row is completed and its version fields match.
            CREATE TABLE IF NOT EXISTS derived_index_jobs (
                index_kind TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                status TEXT NOT NULL,
                error_code TEXT,
                error TEXT,
                attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
                next_retry_at TIMESTAMP,
                lease_token TEXT,
                model_id TEXT NOT NULL,
                model_revision TEXT NOT NULL,
                embedding_version INTEGER NOT NULL CHECK (embedding_version > 0),
                source_fingerprint TEXT NOT NULL,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (index_kind, subject_key)
            );

            CREATE INDEX IF NOT EXISTS idx_derived_embeddings_model
                ON derived_embeddings(index_kind, model_id, model_revision, embedding_version);
            CREATE INDEX IF NOT EXISTS idx_derived_index_jobs_status
                ON derived_index_jobs(index_kind, status, next_retry_at, updated_at);

            -- Monotonic query-visible content epoch for each derived index.
            -- Only mutations that can change the completed embedding join advance
            -- this value, allowing sidecar publication to ignore ledger-only churn.
            CREATE TABLE IF NOT EXISTS derived_index_state (
                index_kind TEXT PRIMARY KEY,
                data_epoch INTEGER NOT NULL DEFAULT 0 CHECK (data_epoch >= 0),
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS derived_index_generations (
                index_kind TEXT PRIMARY KEY,
                generation INTEGER NOT NULL CHECK (generation > 0),
                data_epoch INTEGER NOT NULL DEFAULT 0 CHECK (data_epoch >= 0),
                file_name TEXT NOT NULL,
                checksum_sha256 TEXT NOT NULL,
                row_count INTEGER NOT NULL CHECK (row_count >= 0),
                dimensions INTEGER,
                model_id TEXT,
                model_revision TEXT,
                embedding_version INTEGER CHECK (embedding_version > 0),
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            DROP TRIGGER IF EXISTS derived_embeddings_epoch_after_insert;
            CREATE TRIGGER derived_embeddings_epoch_after_insert
            AFTER INSERT ON derived_embeddings
            WHEN EXISTS (
                SELECT 1 FROM derived_index_jobs j
                 WHERE j.index_kind = NEW.index_kind
                   AND j.subject_key = NEW.subject_key
                   AND j.status = 'completed'
                   AND j.model_id = NEW.model_id
                   AND j.model_revision = NEW.model_revision
                   AND j.embedding_version = NEW.embedding_version
                   AND j.source_fingerprint = NEW.source_fingerprint
            )
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (NEW.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = NEW.index_kind;
            END;

            DROP TRIGGER IF EXISTS derived_embeddings_epoch_after_update;
            CREATE TRIGGER derived_embeddings_epoch_after_update
            AFTER UPDATE ON derived_embeddings
            WHEN EXISTS (
                SELECT 1 FROM derived_index_jobs j
                 WHERE j.index_kind = OLD.index_kind
                   AND j.subject_key = OLD.subject_key
                   AND j.status = 'completed'
                   AND j.model_id = OLD.model_id
                   AND j.model_revision = OLD.model_revision
                   AND j.embedding_version = OLD.embedding_version
                   AND j.source_fingerprint = OLD.source_fingerprint
            ) OR EXISTS (
                SELECT 1 FROM derived_index_jobs j
                 WHERE j.index_kind = NEW.index_kind
                   AND j.subject_key = NEW.subject_key
                   AND j.status = 'completed'
                   AND j.model_id = NEW.model_id
                   AND j.model_revision = NEW.model_revision
                   AND j.embedding_version = NEW.embedding_version
                   AND j.source_fingerprint = NEW.source_fingerprint
            )
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (NEW.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = NEW.index_kind;
            END;

            DROP TRIGGER IF EXISTS derived_embeddings_epoch_after_delete;
            CREATE TRIGGER derived_embeddings_epoch_after_delete
            AFTER DELETE ON derived_embeddings
            WHEN EXISTS (
                SELECT 1 FROM derived_index_jobs j
                 WHERE j.index_kind = OLD.index_kind
                   AND j.subject_key = OLD.subject_key
                   AND j.status = 'completed'
                   AND j.model_id = OLD.model_id
                   AND j.model_revision = OLD.model_revision
                   AND j.embedding_version = OLD.embedding_version
                   AND j.source_fingerprint = OLD.source_fingerprint
            )
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (OLD.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = OLD.index_kind;
            END;

            DROP TRIGGER IF EXISTS derived_index_jobs_epoch_after_insert;
            CREATE TRIGGER derived_index_jobs_epoch_after_insert
            AFTER INSERT ON derived_index_jobs
            WHEN NEW.status = 'completed' AND EXISTS (
                SELECT 1 FROM derived_embeddings e
                 WHERE e.index_kind = NEW.index_kind
                   AND e.subject_key = NEW.subject_key
                   AND e.model_id = NEW.model_id
                   AND e.model_revision = NEW.model_revision
                   AND e.embedding_version = NEW.embedding_version
                   AND e.source_fingerprint = NEW.source_fingerprint
            )
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (NEW.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = NEW.index_kind;
            END;

            DROP TRIGGER IF EXISTS derived_index_jobs_epoch_after_update;
            CREATE TRIGGER derived_index_jobs_epoch_after_update
            AFTER UPDATE ON derived_index_jobs
            WHEN (OLD.status = 'completed' AND EXISTS (
                SELECT 1 FROM derived_embeddings e
                 WHERE e.index_kind = OLD.index_kind
                   AND e.subject_key = OLD.subject_key
                   AND e.model_id = OLD.model_id
                   AND e.model_revision = OLD.model_revision
                   AND e.embedding_version = OLD.embedding_version
                   AND e.source_fingerprint = OLD.source_fingerprint
            )) OR (NEW.status = 'completed' AND EXISTS (
                SELECT 1 FROM derived_embeddings e
                 WHERE e.index_kind = NEW.index_kind
                   AND e.subject_key = NEW.subject_key
                   AND e.model_id = NEW.model_id
                   AND e.model_revision = NEW.model_revision
                   AND e.embedding_version = NEW.embedding_version
                   AND e.source_fingerprint = NEW.source_fingerprint
            ))
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (NEW.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = NEW.index_kind;
            END;

            DROP TRIGGER IF EXISTS derived_index_jobs_epoch_after_delete;
            CREATE TRIGGER derived_index_jobs_epoch_after_delete
            AFTER DELETE ON derived_index_jobs
            WHEN OLD.status = 'completed' AND EXISTS (
                SELECT 1 FROM derived_embeddings e
                 WHERE e.index_kind = OLD.index_kind
                   AND e.subject_key = OLD.subject_key
                   AND e.model_id = OLD.model_id
                   AND e.model_revision = OLD.model_revision
                   AND e.embedding_version = OLD.embedding_version
                   AND e.source_fingerprint = OLD.source_fingerprint
            )
            BEGIN
                INSERT INTO derived_index_state (index_kind, data_epoch, updated_at)
                VALUES (OLD.index_kind, 1, CURRENT_TIMESTAMP)
                ON CONFLICT(index_kind) DO UPDATE SET
                    data_epoch = data_epoch + 1,
                    updated_at = CURRENT_TIMESTAMP;
                DELETE FROM derived_index_generations WHERE index_kind = OLD.index_kind;
            END;

            -- Derived rows follow screenshot lifecycle changes transactionally.
            -- Text vectors use the screenshot id; image vectors use image_hash.
            DROP TRIGGER IF EXISTS cleanup_derived_index_on_screenshot_soft_delete;
            CREATE TRIGGER cleanup_derived_index_on_screenshot_soft_delete
            AFTER UPDATE OF is_deleted ON screenshots
            WHEN OLD.is_deleted = 0 AND NEW.is_deleted != 0
            BEGIN
                DELETE FROM derived_embeddings
                 WHERE index_kind = 'semantic_text'
                   AND subject_key = CAST(NEW.id AS TEXT);
                DELETE FROM derived_index_jobs
                 WHERE index_kind = 'semantic_text'
                   AND subject_key = CAST(NEW.id AS TEXT);
                DELETE FROM derived_embeddings
                 WHERE index_kind = 'clip_image'
                   AND subject_key = NEW.image_hash
                   AND NOT EXISTS (
                       SELECT 1 FROM screenshots
                        WHERE image_hash = NEW.image_hash AND is_deleted = 0
                   );
                DELETE FROM derived_index_jobs
                 WHERE index_kind = 'clip_image'
                   AND subject_key = NEW.image_hash
                   AND NOT EXISTS (
                       SELECT 1 FROM screenshots
                        WHERE image_hash = NEW.image_hash AND is_deleted = 0
                   );
            END;

            DROP TRIGGER IF EXISTS cleanup_derived_index_on_screenshot_delete;
            CREATE TRIGGER cleanup_derived_index_on_screenshot_delete
            AFTER DELETE ON screenshots
            BEGIN
                DELETE FROM derived_embeddings
                 WHERE index_kind = 'semantic_text'
                   AND subject_key = CAST(OLD.id AS TEXT);
                DELETE FROM derived_index_jobs
                 WHERE index_kind = 'semantic_text'
                   AND subject_key = CAST(OLD.id AS TEXT);
                DELETE FROM derived_embeddings
                 WHERE index_kind = 'clip_image'
                   AND subject_key = OLD.image_hash
                   AND NOT EXISTS (
                       SELECT 1 FROM screenshots
                        WHERE image_hash = OLD.image_hash AND is_deleted = 0
                   );
                DELETE FROM derived_index_jobs
                 WHERE index_kind = 'clip_image'
                   AND subject_key = OLD.image_hash
                   AND NOT EXISTS (
                       SELECT 1 FROM screenshots
                        WHERE image_hash = OLD.image_hash AND is_deleted = 0
                   );
            END;

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
        self.recover_interrupted_derived_index_jobs_at_startup(conn)?;

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

    pub fn is_mcp_privacy_acknowledged(&self) -> Result<bool, String> {
        let guard = self.get_connection_named("mcp_privacy_ack_check")?;
        let conn = guard.as_ref().unwrap();
        let acknowledged: bool = conn
            .query_row(
                "SELECT 1 FROM app_metadata WHERE key = ?1",
                params![Self::MCP_PRIVACY_ACKNOWLEDGED_KEY],
                |_| Ok(true),
            )
            .unwrap_or(false);
        Ok(acknowledged)
    }

    pub fn mark_mcp_privacy_acknowledged(&self) -> Result<(), String> {
        let guard = self.get_connection_named("mcp_privacy_ack_mark")?;
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO app_metadata (key, value) VALUES (?1, '1')",
            params![Self::MCP_PRIVACY_ACKNOWLEDGED_KEY],
        )
        .map_err(|e| format!("Failed to mark MCP privacy acknowledgement: {}", e))?;
        Ok(())
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
            .map_err(|e| {
                format!(
                    "Failed to update auto_vacuum sentinel after manual VACUUM: {}",
                    e
                )
            })?;

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
        Self::add_column_if_missing(
            conn,
            "screenshots",
            "is_deleted",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        // Add status and committed_at for two-phase screenshot lifecycle
        Self::add_column_if_missing(conn, "screenshots", "status", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "committed_at", "TIMESTAMP")?;

        Self::add_column_if_missing(conn, "ocr_results", "text_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "ocr_results", "text_key_encrypted", "BLOB")?;
        Self::add_column_if_missing(
            conn,
            "ocr_results",
            "is_deleted",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

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

        Self::add_column_if_missing(conn, "derived_index_generations", "model_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "derived_index_generations",
            "data_epoch",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::add_column_if_missing(conn, "derived_index_generations", "model_revision", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "derived_index_generations",
            "embedding_version",
            "INTEGER",
        )?;
        Self::add_column_if_missing(conn, "derived_index_jobs", "lease_token", "TEXT")?;

        Self::create_table_if_missing(
            conn,
            "screenshot_ocr_status",
            r#"
            CREATE TABLE IF NOT EXISTS screenshot_ocr_status (
                screenshot_id INTEGER PRIMARY KEY,
                status TEXT NOT NULL DEFAULT 'pending',
                engine TEXT,
                model_id TEXT,
                execution_provider TEXT,
                error TEXT,
                elapsed_ms REAL,
                postprocess_status TEXT NOT NULL DEFAULT 'none',
                postprocess_error TEXT,
                postprocess_attempts INTEGER NOT NULL DEFAULT 0,
                postprocess_next_retry_at TIMESTAMP,
                attempted_at TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            )
            "#,
        )?;
        Self::add_column_if_missing(
            conn,
            "screenshot_ocr_status",
            "postprocess_status",
            "TEXT NOT NULL DEFAULT 'none'",
        )?;
        Self::add_column_if_missing(conn, "screenshot_ocr_status", "postprocess_error", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "screenshot_ocr_status",
            "postprocess_attempts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::add_column_if_missing(
            conn,
            "screenshot_ocr_status",
            "postprocess_next_retry_at",
            "TIMESTAMP",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_screenshot_ocr_status_status ON screenshot_ocr_status(status);\
             CREATE INDEX IF NOT EXISTS idx_screenshot_ocr_postprocess_retry ON screenshot_ocr_status(postprocess_status, postprocess_next_retry_at, updated_at);",
        )
        .map_err(|e| format!("Failed to create OCR lifecycle indexes: {}", e))?;

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
        )
        .map_err(|e| format!("Failed to create task_assignments index: {}", e))?;

        // Smart cluster tables (NL-anchored user-defined clusters)
        Self::create_table_if_missing(
            conn,
            "smart_clusters",
            r#"
            CREATE TABLE IF NOT EXISTS smart_clusters (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                anchor_text TEXT NOT NULL,
                threshold REAL NOT NULL DEFAULT 0.0,
                enabled INTEGER NOT NULL DEFAULT 1,
                dominant_color TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )?;
        Self::create_table_if_missing(
            conn,
            "smart_cluster_examples",
            r#"
            CREATE TABLE IF NOT EXISTS smart_cluster_examples (
                smart_cluster_id INTEGER NOT NULL,
                screenshot_id INTEGER NOT NULL,
                is_positive INTEGER NOT NULL,
                rerank_score REAL,
                PRIMARY KEY (smart_cluster_id, screenshot_id),
                FOREIGN KEY (smart_cluster_id) REFERENCES smart_clusters(id) ON DELETE CASCADE,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            )
            "#,
        )?;
        Self::create_table_if_missing(
            conn,
            "smart_cluster_assignments",
            r#"
            CREATE TABLE IF NOT EXISTS smart_cluster_assignments (
                smart_cluster_id INTEGER NOT NULL,
                screenshot_id INTEGER NOT NULL,
                rerank_score REAL,
                assigned_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (smart_cluster_id, screenshot_id),
                FOREIGN KEY (smart_cluster_id) REFERENCES smart_clusters(id) ON DELETE CASCADE,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            )
            "#,
        )?;
        Self::create_table_if_missing(
            conn,
            "smart_cluster_summaries",
            r#"
            CREATE TABLE IF NOT EXISTS smart_cluster_summaries (
                smart_cluster_id INTEGER PRIMARY KEY,
                title TEXT,
                summary TEXT,
                ocr_summary TEXT,
                key_points_json TEXT,
                evidence_json TEXT,
                source_snapshot_count INTEGER,
                source_hash TEXT,
                model_provider TEXT,
                model_name TEXT,
                prompt_version TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (smart_cluster_id) REFERENCES smart_clusters(id) ON DELETE CASCADE
            )
            "#,
        )?;
        Self::create_table_if_missing(
            conn,
            "smart_cluster_pending",
            r#"
            CREATE TABLE IF NOT EXISTS smart_cluster_pending (
                screenshot_id INTEGER PRIMARY KEY,
                queued_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            )
            "#,
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_smart_cluster_assignments_cluster ON smart_cluster_assignments(smart_cluster_id);
             CREATE INDEX IF NOT EXISTS idx_smart_cluster_assignments_screenshot ON smart_cluster_assignments(screenshot_id);
             CREATE INDEX IF NOT EXISTS idx_smart_cluster_summaries_updated_at ON smart_cluster_summaries(updated_at);
             CREATE INDEX IF NOT EXISTS idx_smart_cluster_pending_queued_at ON smart_cluster_pending(queued_at);",
        ).map_err(|e| format!("Failed to create smart_cluster indices: {}", e))?;

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
