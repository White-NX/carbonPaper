//! Database initialization, table creation, and schema migration.

use crate::credential_manager::{
    derive_db_key_from_public_key, get_cached_public_key, load_public_key_from_file,
};
use rusqlite::Connection;
use std::sync::atomic::Ordering;

use super::StorageState;

impl StorageState {
    /// Initialize storage (create directories and database).
    pub fn initialize(&self) -> Result<(), String> {
        let init_start = std::time::Instant::now();
        let mut initialized = self.initialized.lock().unwrap();
        if *initialized {
            return Ok(());
        }

        // Create directories
        let data_dir = self.data_dir.lock().unwrap().clone();
        let screenshot_dir = self.screenshot_dir.lock().unwrap().clone();

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
        let tables_dur = t3.elapsed();

        *self.db.lock().unwrap() = Some(conn);

        // Initialize approximate OCR row count using MAX(id) — O(log N) via primary key index.
        // AUTOINCREMENT ids only increase, so MAX(id) >= actual row count; acceptable for IDF.
        {
            let guard = self.db.lock().unwrap();
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
                content_key_encrypted BLOB
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
                FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
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

            -- Enable foreign key constraints
            PRAGMA foreign_keys = ON;
            "#,
        )
        .map_err(|e| format!("Failed to initialize tables: {}", e))?;

        self.ensure_schema(conn)?;

        Ok(())
    }

    /// Ensure backward compatibility by adding missing columns to existing tables.
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

        // Browser extension metadata columns
        Self::add_column_if_missing(conn, "screenshots", "source", "TEXT")?;
        Self::add_column_if_missing(conn, "screenshots", "page_url_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "page_icon_enc", "BLOB")?;
        Self::add_column_if_missing(conn, "screenshots", "visible_links_enc", "BLOB")?;

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
}
