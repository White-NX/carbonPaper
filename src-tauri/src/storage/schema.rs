//! Database initialization, table creation, and schema migration.

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
};
use rusqlite::{params, Connection};
use std::sync::atomic::Ordering;

use super::{connection, StorageState};

impl StorageState {
    const MCP_PRIVACY_ACKNOWLEDGED_KEY: &'static str = "mcp_privacy_acknowledged";

    /// Initialize storage (create directories and database).
    pub fn initialize(&self) -> Result<(), String> {
        let _maintenance = self.database_maintenance("initialize");
        self.initialize_under_maintenance()
    }

    /// Initialize while the caller already owns the database maintenance gate.
    pub(crate) fn initialize_under_maintenance(&self) -> Result<(), String> {
        let init_start = std::time::Instant::now();
        let mut initialized = self.initialized.lock().unwrap_or_else(|e| e.into_inner());
        if *initialized {
            return Ok(());
        }
        // Belt-and-braces against a stale resident matrix from a previous
        // connection; `shutdown` already clears it on the normal path.
        self.reset_semantic_vector_cache();
        // `shutdown` parks the lazy indexer; without this the thread would stay
        // parked for the rest of the process after any backup or data-directory
        // switch, and its backlog would never be worked off again.
        self.lazy_indexer_shutdown.store(false, Ordering::SeqCst);

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

        // Set the SQLCipher key, verify it, and apply shared connection-local
        // policy. The helper intentionally leaves journal mode unchanged;
        // V1 remains on the existing DELETE default.
        let t2 = std::time::Instant::now();
        let connection_status = connection::configure_sqlcipher_connection(&conn, &db_key)
            .map_err(|e| format!("Failed to configure database connection: {}", e))?;
        let pragma_dur = t2.elapsed();

        // Initialize table schema
        let t3 = std::time::Instant::now();
        self.init_tables(&conn)?;
        self.initialize_database_mode_metadata(&conn, &connection_status.journal_mode)?;
        self.cleanup_derived_index_sidecars_at_startup(&conn, &data_dir)?;
        Self::set_auto_vacuum_incremental(&conn)?;
        let tables_dur = t3.elapsed();

        {
            let mut db_guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
            // Under the same lock the write path checks, so a task holding the
            // previous generation can never slip a statement into this file.
            self.bump_db_generation();
            *db_guard = Some(conn);
        }

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
            "[DIAG:INIT] SQLCipher initialized in {:?} (key_derive={:?}, db_open={:?}, pragma={:?}, journal_mode={}, synchronous={}, init_tables={:?})",
            init_start.elapsed(),
            key_derive_dur,
            open_dur,
            pragma_dur,
            connection_status.journal_mode,
            connection_status.synchronous,
            tables_dur
        );

        Ok(())
    }

    /// Shut down storage: close database connection.
    pub fn shutdown(&self) -> Result<(), String> {
        let _maintenance = self.database_maintenance("shutdown");
        self.shutdown_under_maintenance()
    }

    /// Close the primary connection while the maintenance gate is already held.
    pub(crate) fn shutdown_under_maintenance(&self) -> Result<(), String> {
        self.lazy_indexer_shutdown.store(true, Ordering::SeqCst);
        // The resident semantic matrix belongs to the connection being closed.
        // Its freshness epoch is per-database, so carrying it into whatever
        // connection comes next can silently score against the old file.
        self.reset_semantic_vector_cache();
        let mut db_guard = self.db.lock().map_err(|e| format!("lock error: {}", e))?;
        // Every id collected against this connection stops being meaningful the
        // moment it closes, whether the next `initialize` reopens the same file
        // or a restored backup.
        self.bump_db_generation();
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
        // Must run before the CREATE batch below, or `IF NOT EXISTS` would
        // create the new empty tables first and leave the legacy rows stranded
        // under the old names.
        Self::upgrade_migration_tables_to_derived(conn)?;

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

            -- Document references use their own row key so delayed native
            -- discovery never has to unwrap or rewrite screenshot metadata.
            CREATE TABLE IF NOT EXISTS screenshot_document_refs (
                screenshot_id INTEGER PRIMARY KEY,
                ref_enc BLOB NOT NULL,
                content_key_encrypted BLOB NOT NULL,
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

            -- Persistent ANN generations deliberately have independent
            -- lifecycle semantics from `derived_index_generations`: a new
            -- capture advances the visible epoch but does not invalidate the
            -- immutable ANN base. `derived_ann_changes` is the latest changed
            -- epoch per subject, allowing queries to overlay a bounded exact
            -- tail and tombstones on any older base generation.
            CREATE TABLE IF NOT EXISTS derived_ann_generations (
                index_kind TEXT PRIMARY KEY,
                generation INTEGER NOT NULL CHECK (generation > 0),
                covered_epoch INTEGER NOT NULL CHECK (covered_epoch >= 0),
                flat_file_name TEXT NOT NULL,
                flat_checksum_sha256 TEXT NOT NULL,
                ann_file_name TEXT NOT NULL,
                ann_checksum_sha256 TEXT NOT NULL,
                row_count INTEGER NOT NULL CHECK (row_count >= 0),
                dimensions INTEGER NOT NULL CHECK (dimensions > 0),
                model_id TEXT NOT NULL,
                model_revision TEXT NOT NULL,
                embedding_version INTEGER NOT NULL CHECK (embedding_version > 0),
                sidecar_format_version INTEGER NOT NULL CHECK (sidecar_format_version > 0),
                ann_format_version INTEGER NOT NULL CHECK (ann_format_version > 0),
                algorithm TEXT NOT NULL,
                implementation_version TEXT NOT NULL,
                metric TEXT NOT NULL,
                quantization TEXT NOT NULL,
                connectivity INTEGER NOT NULL CHECK (connectivity > 0),
                expansion_add INTEGER NOT NULL CHECK (expansion_add > 0),
                expansion_search INTEGER NOT NULL CHECK (expansion_search > 0),
                status TEXT NOT NULL DEFAULT 'ready',
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS derived_ann_changes (
                index_kind TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                change_epoch INTEGER NOT NULL CHECK (change_epoch >= 0),
                PRIMARY KEY (index_kind, subject_key)
            );

            CREATE INDEX IF NOT EXISTS idx_derived_ann_changes_epoch
                ON derived_ann_changes(index_kind, change_epoch, subject_key);

            -- ANN construction is expensive and can fail before a generation
            -- row exists. Keep its retry/circuit state independently so a
            -- restart cannot turn a permanent packaging or resource failure
            -- back into one full bootstrap per idle-worker tick.
            CREATE TABLE IF NOT EXISTS derived_ann_build_state (
                index_kind TEXT PRIMARY KEY,
                consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
                last_failure_at TEXT NOT NULL,
                next_retry_at TEXT NOT NULL,
                last_error_code TEXT NOT NULL,
                last_error TEXT NOT NULL,
                circuit_open INTEGER NOT NULL DEFAULT 0,
                -- 0 = none, 1 = pending Toast delivery/ack, 2 = delivered.
                notification_sent INTEGER NOT NULL DEFAULT 0 CHECK (notification_sent IN (0, 1, 2)),
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            -- Durable orchestration state for a foreground derived-index
            -- migration. The vector cache and sidecar remain rebuildable; these
            -- rows only make a long-running upgrade resumable and diagnosable.
            --
            -- One row set per `index_kind`, because MiniLM and CLIP migrate from
            -- different Chroma collections on different schedules and each has
            -- to be able to resume without reading the other's cursor.
            CREATE TABLE IF NOT EXISTS derived_migration_runs (
                run_id TEXT PRIMARY KEY,
                index_kind TEXT NOT NULL DEFAULT 'semantic_text',
                mode TEXT NOT NULL,
                vector_space_revision TEXT NOT NULL,
                status TEXT NOT NULL,
                phase TEXT NOT NULL,
                export_id TEXT,
                export_cursor INTEGER NOT NULL DEFAULT 0 CHECK (export_cursor >= 0),
                chroma_total INTEGER NOT NULL DEFAULT 0 CHECK (chroma_total >= 0),
                chroma_processed INTEGER NOT NULL DEFAULT 0 CHECK (chroma_processed >= 0),
                migrated INTEGER NOT NULL DEFAULT 0 CHECK (migrated >= 0),
                legacy_unverified INTEGER NOT NULL DEFAULT 0 CHECK (legacy_unverified >= 0),
                already_current INTEGER NOT NULL DEFAULT 0 CHECK (already_current >= 0),
                failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
                discarded INTEGER NOT NULL DEFAULT 0 CHECK (discarded >= 0),
                unmappable INTEGER NOT NULL DEFAULT 0 CHECK (unmappable >= 0),
                removed_extra INTEGER NOT NULL DEFAULT 0 CHECK (removed_extra >= 0),
                publish_current INTEGER NOT NULL DEFAULT 0 CHECK (publish_current >= 0),
                publish_total INTEGER NOT NULL DEFAULT 0 CHECK (publish_total >= 0),
                required_free_bytes INTEGER NOT NULL DEFAULT 0 CHECK (required_free_bytes >= 0),
                available_free_bytes INTEGER NOT NULL DEFAULT 0 CHECK (available_free_bytes >= 0),
                monitor_was_running INTEGER NOT NULL DEFAULT 0,
                monitor_was_paused INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                heartbeat_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                finished_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_derived_migration_runs_updated
                ON derived_migration_runs(index_kind, updated_at DESC);

            -- `run_id` is globally unique, so these two need no `index_kind` of
            -- their own; they reach it through the run they belong to.
            CREATE TABLE IF NOT EXISTS derived_migration_subjects (
                run_id TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                outcome TEXT NOT NULL,
                source_fingerprint TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (run_id, subject_key),
                FOREIGN KEY (run_id) REFERENCES derived_migration_runs(run_id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS derived_migration_run_errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                subject_key TEXT,
                phase TEXT NOT NULL,
                code TEXT NOT NULL,
                error TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (run_id) REFERENCES derived_migration_runs(run_id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_derived_migration_errors_run
                ON derived_migration_run_errors(run_id, id);

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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    NEW.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = NEW.index_kind)
                WHERE NEW.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    NEW.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = NEW.index_kind)
                WHERE NEW.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    OLD.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = OLD.index_kind)
                WHERE OLD.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    NEW.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = NEW.index_kind)
                WHERE NEW.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    NEW.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = NEW.index_kind)
                WHERE NEW.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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
                INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch)
                SELECT
                    'clip_image',
                    OLD.subject_key,
                    (SELECT data_epoch FROM derived_index_state WHERE index_kind = OLD.index_kind)
                WHERE OLD.index_kind = 'clip_image'
                ON CONFLICT(index_kind, subject_key) DO UPDATE SET
                    change_epoch = excluded.change_epoch;
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

            -- Generic key-value store for app-level metadata / migration markers
            CREATE TABLE IF NOT EXISTS app_metadata (
                key TEXT PRIMARY KEY,
                value TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            -- Persist the effective SQLite journal mode and the last
            -- controlled-transition outcome.  The row is deliberately kept
            -- in the authoritative database so a restart can diagnose an
            -- interrupted WAL-to-DELETE request without relying on sidecars.
            CREATE TABLE IF NOT EXISTS database_mode_metadata (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                actual_journal_mode TEXT NOT NULL CHECK (
                    actual_journal_mode IN ('delete', 'truncate', 'persist', 'memory', 'wal', 'off')
                ),
                requested_journal_mode TEXT NOT NULL CHECK (
                    requested_journal_mode IN ('delete', 'truncate', 'persist', 'memory', 'wal', 'off')
                ),
                transition_state TEXT NOT NULL CHECK (
                    transition_state IN ('stable', 'transitioning', 'failed')
                ),
                transition_id TEXT,
                previous_journal_mode TEXT CHECK (
                    previous_journal_mode IS NULL OR
                    previous_journal_mode IN ('delete', 'truncate', 'persist', 'memory', 'wal', 'off')
                ),
                last_error TEXT,
                started_at TEXT,
                completed_at TEXT,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            -- One durable row per automatic background task. The scheduler
            -- owns ordering and retry state; the task-specific queues remain
            -- in their existing business tables.
            CREATE TABLE IF NOT EXISTS background_scheduler_tasks (
                task_kind TEXT PRIMARY KEY,
                ready_since_ms INTEGER NOT NULL,
                next_attempt_at_ms INTEGER NOT NULL DEFAULT 0,
                failure_count INTEGER NOT NULL DEFAULT 0,
                last_served_seq INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                last_completed_at_ms INTEGER,
                status TEXT NOT NULL DEFAULT 'queued',
                manual_pending INTEGER NOT NULL DEFAULT 0,
                manual_in_flight INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_background_scheduler_ready
                ON background_scheduler_tasks(status, next_attempt_at_ms,
                                               ready_since_ms, last_served_seq);
            "#,
        )
        .map_err(|e| format!("Failed to initialize tables: {}", e))?;

        // Early development triggers briefly recorded non-CLIP subjects in
        // the ANN change table. They are not part of this accelerator's
        // corpus and would otherwise inflate tail diagnostics forever on an
        // upgraded developer database.
        conn.execute(
            "DELETE FROM derived_ann_changes WHERE index_kind <> 'clip_image'",
            [],
        )
        .map_err(|e| format!("Failed to clean legacy ANN changes: {e}"))?;

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
            let _maintenance = self.database_maintenance("startup_vacuum_run");
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
            let _maintenance = self.database_maintenance("manual_vacuum_run");
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
        // A manual request is consumed when its slice starts, but must be
        // restored when that slice is deferred or fails. Keep that distinction
        // durable so cancellation and process restart cannot resurrect or lose
        // a request accidentally.
        Self::add_column_if_missing(
            conn,
            "background_scheduler_tasks",
            "manual_in_flight",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        // `derived_migration_runs.index_kind` is deliberately not here: the
        // CREATE batch indexes over it, so it has to be in place before that
        // batch runs. See `upgrade_migration_tables_to_derived`.

        // M2.5 step-4 cutover: drop the retired shadow-comparison tables. They
        // only ever held query hashes and parity/latency numbers — never query
        // text or screenshot content — so dropping them loses no user data. A
        // beta install that ran the harness would otherwise carry both tables
        // and their indexes forever; the roadmap's retirement rule is that no
        // shadow surface survives its own cutover.
        conn.execute_batch(
            r#"
            DROP TABLE IF EXISTS semantic_shadow_samples;
            DROP TABLE IF EXISTS semantic_doc_encoder_runs;
            "#,
        )
        .map_err(|error| format!("Failed to drop retired shadow tables: {error}"))?;

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

        Self::create_table_if_missing(
            conn,
            "screenshot_document_refs",
            r#"
            CREATE TABLE IF NOT EXISTS screenshot_document_refs (
                screenshot_id INTEGER PRIMARY KEY,
                ref_enc BLOB NOT NULL,
                content_key_encrypted BLOB NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
            )
            "#,
        )?;

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
                threshold_model_id TEXT,
                threshold_model_revision TEXT,
                threshold_variant TEXT,
                threshold_provider TEXT,
                threshold_calibrated_at TIMESTAMP,
                threshold_rederive_failed_scorer TEXT,
                anchor_vector BLOB,
                anchor_vector_dimensions INTEGER,
                anchor_vector_source_hash TEXT,
                anchor_vector_model_id TEXT,
                anchor_vector_model_revision TEXT,
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

        // M2.5 step 6 — threshold provenance, added here rather than with the
        // other column migrations above because those run before this table
        // exists. A Smart Cluster threshold is derived from calibration rerank
        // scores and then compared against by the background worker, so the two
        // sides have to agree on which scorer produced the number. Every
        // threshold written before this existed came from the Python reranker;
        // these columns are NULL for those rows, which is exactly how the
        // re-derivation pass recognizes them. Four columns rather than one,
        // because the model, its revision, the quantization variant, and the
        // execution provider each move the logits independently.
        Self::add_column_if_missing(conn, "smart_clusters", "threshold_model_id", "TEXT")?;
        Self::add_column_if_missing(conn, "smart_clusters", "threshold_model_revision", "TEXT")?;
        Self::add_column_if_missing(conn, "smart_clusters", "threshold_variant", "TEXT")?;
        Self::add_column_if_missing(conn, "smart_clusters", "threshold_provider", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "smart_clusters",
            "threshold_calibrated_at",
            "TIMESTAMP",
        )?;

        // The verdict "this cluster's saved examples cannot produce a threshold
        // under scorer X", recorded so the worker stops paying for the answer.
        // Re-deriving costs a 570 MB cross-encoder load, and a cluster whose
        // positive examples were deleted fails that derivation every time it is
        // attempted — without this column, once a minute forever. It holds the
        // scorer the attempt was made under rather than a bare flag, so a
        // different build retries once instead of inheriting a verdict that was
        // never about it; it is cleared whenever the examples are re-saved or a
        // threshold is written, which are the two things that can change the
        // answer.
        Self::add_column_if_missing(
            conn,
            "smart_clusters",
            "threshold_rederive_failed_scorer",
            "TEXT",
        )?;

        // The cluster's anchor, already encoded by the bi-encoder.
        //
        // Not a speed-up of the encode itself, which is two milliseconds for a
        // sentence. What it removes is a *model swap*: the scoring pass needs
        // MiniLM to encode the anchors and the cross-encoder to score, the
        // engine holds exactly one model resident, and the anchors were
        // re-encoded at the head of every batch — so every batch paid to load
        // MiniLM (0.50 s) and then load the 570 MB cross-encoder back over it
        // (1.2 s warm). On a queue deep enough to need a hundred batches that
        // is most of a forced drain's wall-clock budget spent swapping.
        //
        // Invalidation is by content rather than by call site: the row records
        // the hash of the anchor text the vector was made from and the model
        // that made it, and a mismatch on either re-encodes. Nothing has to
        // remember to clear this when a cluster is edited, which is the failure
        // mode that made re-encoding look like the safer option in the first
        // place.
        Self::add_column_if_missing(conn, "smart_clusters", "anchor_vector", "BLOB")?;
        Self::add_column_if_missing(
            conn,
            "smart_clusters",
            "anchor_vector_dimensions",
            "INTEGER",
        )?;
        Self::add_column_if_missing(conn, "smart_clusters", "anchor_vector_source_hash", "TEXT")?;
        Self::add_column_if_missing(conn, "smart_clusters", "anchor_vector_model_id", "TEXT")?;
        Self::add_column_if_missing(
            conn,
            "smart_clusters",
            "anchor_vector_model_revision",
            "TEXT",
        )?;

        // The label the user sees, kept apart from the anchor text the scorer
        // matches against.
        //
        // These were one column until the cluster list started showing names:
        // an anchor is a sentence describing what to collect, which is the
        // right input for the scorer and the wrong thing to read down a 320px
        // list. Worse, renaming used to rewrite the anchor, so giving a cluster
        // a shorter name silently changed what it collected and invalidated the
        // threshold calibrated against the old wording.
        //
        // NULL means the row predates the split, and readers fall back to
        // `anchor_text` — the same string those clusters were already showing.
        Self::add_column_if_missing(conn, "smart_clusters", "display_name", "TEXT")?;

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
            CREATE INDEX IF NOT EXISTS idx_screenshots_deleted_category ON screenshots(is_deleted, category);
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

    /// Carry the M2.4 migration tables over to the kind-agnostic names.
    ///
    /// The three tables were introduced for MiniLM alone and named for it. M2.5
    /// step 7 migrates a second index kind (CLIP images) through the same
    /// orchestration, so the discriminator moved into the rows and the names
    /// stopped being true. Renaming rather than creating a parallel set keeps
    /// one code path for a page loop whose transactional cursor rules are the
    /// part worth not writing twice.
    ///
    /// `ALTER TABLE ... RENAME TO` also rewrites the foreign keys that point at
    /// the runs table, so the child tables keep referencing it after the move.
    /// Rows already present belong to a settled MiniLM run and are left alone;
    /// the `index_kind` default files them under `semantic_text`.
    ///
    /// The whole step has to finish before the CREATE batch runs. `IF NOT
    /// EXISTS` compares table names and nothing else, so a renamed table keeps
    /// its old column set through the batch, and the batch immediately indexes
    /// over `index_kind` — leaving the column to `ensure_schema` further down
    /// would abort initialization before it ever got there.
    fn upgrade_migration_tables_to_derived(conn: &Connection) -> Result<(), String> {
        for (legacy, current) in [
            ("minilm_migration_runs", "derived_migration_runs"),
            ("minilm_migration_subjects", "derived_migration_subjects"),
            (
                "minilm_migration_run_errors",
                "derived_migration_run_errors",
            ),
        ] {
            if !Self::table_exists(conn, legacy)? || Self::table_exists(conn, current)? {
                continue;
            }
            conn.execute_batch(&format!("ALTER TABLE {legacy} RENAME TO {current};"))
                .map_err(|e| format!("Failed to rename {legacy} to {current}: {e}"))?;
        }
        // Guarded on the table because a fresh install has none of this: the
        // CREATE batch declares `index_kind` itself, and an unguarded ALTER
        // would fail against a table that does not exist yet.
        if Self::table_exists(conn, "derived_migration_runs")? {
            Self::add_column_if_missing(
                conn,
                "derived_migration_runs",
                "index_kind",
                "TEXT NOT NULL DEFAULT 'semantic_text'",
            )?;
        }
        // The old indexes survive the rename attached to the renamed tables.
        // Dropping them lets the CREATE batch install the ones whose definition
        // now leads with `index_kind`.
        conn.execute_batch(
            r#"
            DROP INDEX IF EXISTS idx_minilm_migration_runs_updated;
            DROP INDEX IF EXISTS idx_minilm_migration_errors_run;
            "#,
        )
        .map_err(|e| format!("Failed to drop legacy migration indexes: {e}"))?;
        Ok(())
    }

    fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Failed to check for table {table}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use std::sync::Arc;

    /// The migration tables as M2.4 shipped them, before the rename moved them
    /// off MiniLM. An upgrading install hands `init_tables` exactly this shape,
    /// so the fixture reproduces it in full rather than a convenient subset:
    /// the column it does *not* have, `index_kind`, is the whole point.
    const LEGACY_MIGRATION_TABLES: &str = r#"
        CREATE TABLE minilm_migration_runs (
            run_id TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            vector_space_revision TEXT NOT NULL,
            status TEXT NOT NULL,
            phase TEXT NOT NULL,
            export_id TEXT,
            export_cursor INTEGER NOT NULL DEFAULT 0 CHECK (export_cursor >= 0),
            chroma_total INTEGER NOT NULL DEFAULT 0 CHECK (chroma_total >= 0),
            chroma_processed INTEGER NOT NULL DEFAULT 0 CHECK (chroma_processed >= 0),
            migrated INTEGER NOT NULL DEFAULT 0 CHECK (migrated >= 0),
            legacy_unverified INTEGER NOT NULL DEFAULT 0 CHECK (legacy_unverified >= 0),
            already_current INTEGER NOT NULL DEFAULT 0 CHECK (already_current >= 0),
            failed INTEGER NOT NULL DEFAULT 0 CHECK (failed >= 0),
            discarded INTEGER NOT NULL DEFAULT 0 CHECK (discarded >= 0),
            unmappable INTEGER NOT NULL DEFAULT 0 CHECK (unmappable >= 0),
            removed_extra INTEGER NOT NULL DEFAULT 0 CHECK (removed_extra >= 0),
            publish_current INTEGER NOT NULL DEFAULT 0 CHECK (publish_current >= 0),
            publish_total INTEGER NOT NULL DEFAULT 0 CHECK (publish_total >= 0),
            required_free_bytes INTEGER NOT NULL DEFAULT 0 CHECK (required_free_bytes >= 0),
            available_free_bytes INTEGER NOT NULL DEFAULT 0 CHECK (available_free_bytes >= 0),
            monitor_was_running INTEGER NOT NULL DEFAULT 0,
            monitor_was_paused INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            heartbeat_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            finished_at TEXT
        );

        CREATE TABLE minilm_migration_subjects (
            run_id TEXT NOT NULL,
            subject_key TEXT NOT NULL,
            outcome TEXT NOT NULL,
            source_fingerprint TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (run_id, subject_key),
            FOREIGN KEY (run_id) REFERENCES minilm_migration_runs(run_id) ON DELETE CASCADE
        );

        CREATE TABLE minilm_migration_run_errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            subject_key TEXT,
            phase TEXT NOT NULL,
            code TEXT NOT NULL,
            error TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (run_id) REFERENCES minilm_migration_runs(run_id) ON DELETE CASCADE
        );
    "#;

    /// Kept apart from the tables because the half-upgraded fixture must not
    /// have them: a stale index sitting under the new name would let
    /// `CREATE INDEX IF NOT EXISTS` skip out and hide the very failure the
    /// test is looking for.
    const LEGACY_MIGRATION_INDEXES: &str = r#"
        CREATE INDEX idx_minilm_migration_runs_updated
            ON minilm_migration_runs(updated_at DESC);
        CREATE INDEX idx_minilm_migration_errors_run
            ON minilm_migration_run_errors(run_id, id);
    "#;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        (temp, storage)
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("read table info");
        let mut columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .filter_map(Result::ok);
        columns.any(|name| name == column)
    }

    fn object_exists(conn: &Connection, kind: &str, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
            params![kind, name],
            |row| row.get::<_, bool>(0),
        )
        .expect("read sqlite_master")
    }

    fn insert_legacy_run(conn: &Connection, table: &str, run_id: &str) {
        conn.execute_batch(&format!(
            "INSERT INTO {table}
                 (run_id, mode, vector_space_revision, status, phase)
             VALUES
                 ('{run_id}', 'copy_chroma_hot_layer', 'revision-1', 'completed', 'finished');"
        ))
        .expect("insert legacy run row");
    }

    /// The upgrade path a released install actually takes: MiniLM-named tables
    /// carrying settled rows, opened by a build whose schema indexes over
    /// `index_kind`.
    #[test]
    fn init_tables_carries_legacy_minilm_migration_tables_over() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(LEGACY_MIGRATION_TABLES)
            .expect("install legacy migration tables");
        conn.execute_batch(LEGACY_MIGRATION_INDEXES)
            .expect("install legacy migration indexes");
        insert_legacy_run(&conn, "minilm_migration_runs", "run-legacy");
        conn.execute_batch(
            "INSERT INTO minilm_migration_subjects
                 (run_id, subject_key, outcome, source_fingerprint)
             VALUES ('run-legacy', 'screenshot:1', 'migrated', 'fingerprint-1');",
        )
        .expect("insert legacy subject row");

        storage
            .init_tables(&conn)
            .expect("initialize schema over a legacy database");

        assert!(column_exists(&conn, "derived_migration_runs", "index_kind"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_derived_migration_runs_updated"
        ));
        assert!(!object_exists(&conn, "table", "minilm_migration_runs"));
        assert!(!object_exists(
            &conn,
            "index",
            "idx_minilm_migration_runs_updated"
        ));

        // A settled MiniLM run keeps its rows and lands under the default kind.
        let kind: String = conn
            .query_row(
                "SELECT index_kind FROM derived_migration_runs WHERE run_id = 'run-legacy'",
                [],
                |row| row.get(0),
            )
            .expect("legacy run survives the upgrade");
        assert_eq!(kind, "semantic_text");
        let subjects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_migration_subjects WHERE run_id = 'run-legacy'",
                [],
                |row| row.get(0),
            )
            .expect("count carried-over subjects");
        assert_eq!(subjects, 1);

        // Every later launch runs the same path against the upgraded database.
        storage
            .init_tables(&conn)
            .expect("re-initialize an already upgraded database");
    }

    /// The state a database is left in when the upgrade fails partway: the
    /// rename commits on its own, then the CREATE batch aborts. Restarting has
    /// to finish the job instead of tripping over the same missing column.
    #[test]
    fn init_tables_repairs_a_half_upgraded_migration_runs_table() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(
            &LEGACY_MIGRATION_TABLES.replace("minilm_migration_", "derived_migration_"),
        )
        .expect("install half-upgraded migration tables");
        insert_legacy_run(&conn, "derived_migration_runs", "run-half");

        storage
            .init_tables(&conn)
            .expect("initialize schema over a half-upgraded database");

        assert!(column_exists(&conn, "derived_migration_runs", "index_kind"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_derived_migration_runs_updated"
        ));
        let kind: String = conn
            .query_row(
                "SELECT index_kind FROM derived_migration_runs WHERE run_id = 'run-half'",
                [],
                |row| row.get(0),
            )
            .expect("half-upgraded run survives the repair");
        assert_eq!(kind, "semantic_text");
    }

    /// A fresh install has no migration tables at all, so the upgrade step has
    /// to leave an absent table alone rather than trying to alter it.
    #[test]
    fn init_tables_defines_index_kind_on_a_fresh_database() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().expect("in-memory database");

        storage
            .init_tables(&conn)
            .expect("initialize schema on a fresh database");

        assert!(column_exists(&conn, "derived_migration_runs", "index_kind"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_derived_migration_runs_updated"
        ));
        assert!(object_exists(&conn, "table", "screenshot_document_refs"));
    }

    #[test]
    fn init_tables_installs_ann_manifest_changes_and_epoch_triggers() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();

        assert!(object_exists(&conn, "table", "derived_ann_generations"));
        assert!(object_exists(&conn, "table", "derived_ann_changes"));
        assert!(object_exists(&conn, "table", "derived_ann_build_state"));
        assert!(object_exists(
            &conn,
            "trigger",
            "derived_embeddings_epoch_after_insert"
        ));
        let trigger_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'derived_index_jobs_epoch_after_update'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(trigger_sql.contains("derived_ann_changes"));
        assert!(trigger_sql.contains("WHERE NEW.index_kind = 'clip_image'"));
    }

    #[test]
    fn ann_changes_track_clip_only() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        for (id, kind, subject) in [(1_i64, "semantic_text", "1"), (2, "clip_image", "hash-2")] {
            conn.execute(
                "INSERT INTO screenshots (id, image_path, image_hash) VALUES (?1, ?2, ?3)",
                params![id, format!("{id}.enc"), format!("hash-{id}")],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO derived_index_jobs (
                    index_kind, subject_key, status, model_id, model_revision,
                    embedding_version, source_fingerprint
                ) VALUES (?1, ?2, 'completed', 'model', 'revision', 1, 'source')
                "#,
                params![kind, subject],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO derived_embeddings (
                    index_kind, subject_key, model_id, model_revision,
                    embedding_version, source_fingerprint, dimensions, vector_f32
                ) VALUES (?1, ?2, 'model', 'revision', 1, 'source', 1, X'0000803F')
                "#,
                params![kind, subject],
            )
            .unwrap();
        }

        let semantic_changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_ann_changes WHERE index_kind = 'semantic_text'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let clip_changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_ann_changes WHERE index_kind = 'clip_image'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(semantic_changes, 0);
        assert_eq!(clip_changes, 1);
    }

    #[test]
    fn init_tables_removes_legacy_non_clip_ann_changes_idempotently() {
        let (_temp, storage) = test_storage();
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch) VALUES ('semantic_text', '1', 7)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO derived_ann_changes (index_kind, subject_key, change_epoch) VALUES ('clip_image', 'hash-a', 8)",
            [],
        )
        .unwrap();

        storage.init_tables(&conn).unwrap();
        storage.init_tables(&conn).unwrap();

        let semantic_changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_ann_changes WHERE index_kind = 'semantic_text'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let clip_changes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM derived_ann_changes WHERE index_kind = 'clip_image'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(semantic_changes, 0);
        assert_eq!(clip_changes, 1);
    }
}
