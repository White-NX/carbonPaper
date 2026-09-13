//! Archive-side generation fences and durable broker completion receipts.
use super::StorageState;
use crate::processing_stage::TaskReceipt;
use rusqlite::{params, Connection, OptionalExtension};

const DATASET_KEY: &str = "app_bound_dataset_id";

#[cfg(test)]
mod tests;

impl StorageState {
    pub(super) fn init_processing_stage_schema(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS screenshot_processing_revisions (
                screenshot_id INTEGER PRIMARY KEY REFERENCES screenshots(id) ON DELETE CASCADE,
                revision INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS app_bound_receipts (
                task_id TEXT NOT NULL,consumer INTEGER NOT NULL,dataset_id TEXT NOT NULL,
                receipt_json TEXT NOT NULL,acknowledged INTEGER NOT NULL DEFAULT 0,
                recorded_at INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(task_id,consumer)
            );
            CREATE TABLE IF NOT EXISTS app_bound_revocations (screenshot_id INTEGER PRIMARY KEY);
            CREATE TRIGGER IF NOT EXISTS app_bound_screenshot_deleted AFTER UPDATE OF is_deleted ON screenshots
            WHEN NEW.is_deleted=1 AND OLD.is_deleted=0 BEGIN
                INSERT OR IGNORE INTO app_bound_revocations(screenshot_id) VALUES(NEW.id);
            END;
            CREATE TRIGGER IF NOT EXISTS app_bound_screenshot_removed AFTER DELETE ON screenshots BEGIN
                INSERT OR IGNORE INTO app_bound_revocations(screenshot_id) VALUES(OLD.id);
            END;
            CREATE TRIGGER IF NOT EXISTS app_bound_ocr_insert AFTER INSERT ON ocr_results BEGIN
                INSERT INTO screenshot_processing_revisions(screenshot_id,revision) VALUES(NEW.screenshot_id,1)
                ON CONFLICT(screenshot_id) DO UPDATE SET revision=revision+1;
            END;
            DROP TRIGGER IF EXISTS app_bound_ocr_update;
            CREATE TRIGGER app_bound_ocr_update AFTER UPDATE OF text_enc,text_key_encrypted,is_deleted,screenshot_id ON ocr_results BEGIN
                INSERT INTO screenshot_processing_revisions(screenshot_id,revision) VALUES(NEW.screenshot_id,1)
                ON CONFLICT(screenshot_id) DO UPDATE SET revision=revision+1;
                UPDATE screenshot_processing_revisions SET revision=revision+1 WHERE screenshot_id=OLD.screenshot_id AND OLD.screenshot_id!=NEW.screenshot_id;
            END;
            CREATE TRIGGER IF NOT EXISTS app_bound_ocr_delete AFTER DELETE ON ocr_results BEGIN
                INSERT INTO screenshot_processing_revisions(screenshot_id,revision)
                SELECT OLD.screenshot_id,1 FROM screenshots WHERE id=OLD.screenshot_id
                ON CONFLICT(screenshot_id) DO UPDATE SET revision=revision+1;
            END;
            DROP TRIGGER IF EXISTS app_bound_metadata_update;
            CREATE TRIGGER app_bound_metadata_update AFTER UPDATE OF window_title_enc,content_key_encrypted,process_name,image_hash,timestamp,status ON screenshots BEGIN
                INSERT INTO screenshot_processing_revisions(screenshot_id,revision) VALUES(NEW.id,1)
                ON CONFLICT(screenshot_id) DO UPDATE SET revision=revision+1;
            END;") .map_err(|e|e.to_string())?;
        let has_recorded_at:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('app_bound_receipts') WHERE name='recorded_at')",[],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !has_recorded_at {
            conn.execute(
                "ALTER TABLE app_bound_receipts ADD COLUMN recorded_at INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO app_metadata(key,value) VALUES(?1,?2)",
            params![DATASET_KEY, carbonpaper_app_bound::protocol::random_id()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) fn processing_dataset_id(&self) -> Result<String, String> {
        let guard = self.get_connection_named("processing_dataset_id")?;
        guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT value FROM app_metadata WHERE key=?1",
                [DATASET_KEY],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn reset_processing_dataset(&self) -> Result<(), String> {
        self.processing_stage.retire_dataset()?;
        let guard = self.get_connection_named("reset_processing_dataset")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE app_metadata SET value=?2 WHERE key=?1",
            params![DATASET_KEY, carbonpaper_app_bound::protocol::random_id()],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM app_bound_receipts", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM app_bound_revocations", [])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        self.bump_db_generation();
        drop(guard);
        let directory = self
            .data_dir
            .lock()
            .map_err(|_| "data directory lock poisoned")?
            .clone();
        self.processing_stage.initialize(
            &directory,
            self.processing_dataset_id()?,
            self.db_generation(),
        )
    }

    pub(crate) fn finish_staged_deletions(&self) -> Result<(), String> {
        if !self.processing_stage.initialized() {
            return Ok(());
        }
        loop {
            let ids = {
                let guard = self.get_connection_named("staged_revocations")?;
                let conn = guard.as_ref().ok_or("Database not initialized")?;
                let mut stmt=conn.prepare("SELECT screenshot_id FROM app_bound_revocations ORDER BY screenshot_id LIMIT 128").map_err(|e|e.to_string())?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, i64>(0))
                    .map_err(|e| e.to_string())?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?
            };
            if ids.is_empty() {
                return Ok(());
            }
            self.processing_stage
                .revoke_screenshots(&ids)
                .map_err(|_| "APP_BOUND_REVOCATION_PENDING")?;
            let guard = self.get_connection_named("staged_revocations_ack")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            for id in ids {
                tx.execute(
                    "DELETE FROM app_bound_revocations WHERE screenshot_id=?1",
                    [id],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
    }

    pub(crate) fn staged_source_revision(&self, id: i64) -> Result<i64, String> {
        let guard = self.get_connection_named("staged_source_revision")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.query_row("SELECT COALESCE(r.revision,0) FROM screenshots s LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
            WHERE s.id=?1 AND s.is_deleted=0",[id],|r|r.get(0)).map_err(|e|e.to_string())
    }

    pub(crate) fn staged_screenshot_active(&self, id: i64) -> Result<bool, String> {
        let guard = self.get_connection_named("staged_screenshot_active")?;
        guard.as_ref().ok_or("Database not initialized")?.query_row("SELECT EXISTS(SELECT 1 FROM screenshots WHERE id=?1 AND is_deleted=0 AND status='committed')",[id],|r|r.get(0)).map_err(|e|e.to_string())
    }

    pub(crate) fn staged_source_is_current(&self, receipt: &TaskReceipt) -> Result<bool, String> {
        let guard = self.get_connection_named("staged_source_is_current")?;
        self.staged_source_current_on_conn(
            guard.as_ref().ok_or("Database not initialized")?,
            receipt,
        )
    }

    pub(super) fn staged_source_current_on_conn(
        &self,
        conn: &Connection,
        receipt: &TaskReceipt,
    ) -> Result<bool, String> {
        if self.db_generation() != receipt.db_generation
            || !carbonpaper_app_bound::protocol::valid_id(&receipt.task_id)
            || !carbonpaper_app_bound::protocol::valid_id(&receipt.lease_id)
        {
            return Ok(false);
        }
        conn.query_row("SELECT EXISTS(SELECT 1 FROM screenshots s LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
            WHERE s.id=?1 AND s.is_deleted=0 AND s.status='committed' AND COALESCE(r.revision,0)=?2
            AND (SELECT value FROM app_metadata WHERE key=?3)=?4)",params![receipt.screenshot_id,receipt.source_revision,DATASET_KEY,receipt.dataset_id],|r|r.get(0)).map_err(|e|e.to_string())
    }

    pub(super) fn record_staged_receipt_on_conn(
        conn: &Connection,
        receipt: &TaskReceipt,
    ) -> Result<(), String> {
        let json = serde_json::to_string(receipt).map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO app_bound_receipts(task_id,consumer,dataset_id,receipt_json,recorded_at) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(task_id,consumer) DO NOTHING",
            params![receipt.task_id,receipt.consumer.bit(),receipt.dataset_id,json,carbonpaper_app_bound::protocol::now_secs()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub(crate) fn record_staged_receipt(&self, receipt: &TaskReceipt) -> Result<(), String> {
        self.processing_stage.check_receipt(receipt)?;
        let guard = self.get_connection_named("record_staged_receipt")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if !self.staged_source_current_on_conn(conn, receipt)? {
            return Err("staged source changed".into());
        }
        Self::record_staged_receipt_on_conn(conn, receipt)
    }

    pub(crate) fn pending_staged_receipts(&self) -> Result<Vec<TaskReceipt>, String> {
        let guard = self.get_connection_named("pending_staged_receipts")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "DELETE FROM app_bound_receipts WHERE acknowledged=1 AND recorded_at<?1",
            [carbonpaper_app_bound::protocol::now_secs()
                - 60 * carbonpaper_app_bound::protocol::DAY_SECS],
        )
        .map_err(|e| e.to_string())?;
        let mut stmt=conn.prepare("SELECT task_id,consumer,dataset_id,CASE WHEN length(receipt_json)<=4096 THEN receipt_json ELSE '' END FROM app_bound_receipts WHERE acknowledged=0 ORDER BY recorded_at,task_id LIMIT 64").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        let mut receipts = Vec::new();
        for (id, consumer, dataset, json) in rows {
            if let Ok(receipt) = serde_json::from_str::<TaskReceipt>(&json) {
                if receipt.task_id == id
                    && i64::from(receipt.consumer.bit()) == consumer
                    && receipt.dataset_id == dataset
                    && carbonpaper_app_bound::protocol::valid_id(&id)
                    && carbonpaper_app_bound::protocol::valid_id(&dataset)
                    && carbonpaper_app_bound::protocol::valid_id(&receipt.lease_id)
                    && receipt.screenshot_id > 0
                {
                    receipts.push(receipt);
                    continue;
                }
            }
            // Corrupt user-writable metadata cannot block unrelated results or
            // authorize a fresh write. Unknown broker tasks still fail closed.
            conn.execute(
                "UPDATE app_bound_receipts SET acknowledged=1 WHERE task_id=?1 AND consumer=?2",
                params![id, consumer],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(receipts)
    }

    pub(crate) fn pending_staged_receipt_count(&self) -> Result<u64, String> {
        let guard = self.get_connection_named("pending_staged_receipt_count")?;
        let count: i64 = guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT COUNT(*) FROM app_bound_receipts WHERE acknowledged=0",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(count.max(0) as u64)
    }

    pub(crate) fn pending_staged_classification_receipt_count(&self) -> Result<u64, String> {
        let guard = self.get_connection_named("pending_staged_classification_receipt_count")?;
        let count: i64 = guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT COUNT(*) FROM app_bound_receipts WHERE acknowledged=0 AND consumer=?1",
                [carbonpaper_app_bound::protocol::Consumer::Classification.bit()],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(count.max(0) as u64)
    }

    pub(crate) fn clear_staged_receipt(&self, receipt: &TaskReceipt) -> Result<(), String> {
        let guard = self.get_connection_named("clear_staged_receipt")?;
        guard.as_ref().ok_or("Database not initialized")?.execute("UPDATE app_bound_receipts SET acknowledged=1 WHERE task_id=?1 AND consumer=?2 AND dataset_id=?3",
            params![receipt.task_id,receipt.consumer.bit(),receipt.dataset_id]).map(|_|()).map_err(|e|e.to_string())
    }

    pub(crate) fn commit_staged_category(
        &self,
        receipt: &TaskReceipt,
        category: Option<&str>,
        confidence: Option<f64>,
    ) -> Result<bool, String> {
        if receipt.consumer != carbonpaper_app_bound::protocol::Consumer::Classification {
            return Err("invalid staged consumer".into());
        }
        // Classification returns a weighted score, which can exceed 1 after
        // anchor and process-prior bonuses, rather than a probability.
        if category.is_some_and(|s| s.len() > 256)
            || confidence.is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err("invalid classification result".into());
        }
        // Transport retries of an already committed callback must succeed even
        // after its key has been revoked. Never apply its supplied result twice.
        {
            let guard = self.get_connection_named("staged_category_receipt")?;
            let prior:Option<String>=guard.as_ref().ok_or("Database not initialized")?.query_row(
                "SELECT receipt_json FROM app_bound_receipts WHERE task_id=?1 AND consumer=?2 AND dataset_id=?3",
                params![receipt.task_id,receipt.consumer.bit(),receipt.dataset_id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
            if prior.as_deref() == Some(&serde_json::to_string(receipt).map_err(|e| e.to_string())?)
            {
                return Ok(false);
            }
        }
        self.processing_stage.check_receipt(receipt)?;
        let mut guard = self.get_connection_named("commit_staged_category")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        if !self.staged_source_current_on_conn(conn, receipt)? {
            return Err("staged source changed".into());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let already_committed: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM app_bound_receipts WHERE task_id=?1 AND consumer=?2)",
                params![receipt.task_id, receipt.consumer.bit()],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if already_committed {
            return Ok(false);
        }
        if let Some(category) = category {
            tx.execute("UPDATE screenshots SET category=?2,category_confidence=?3 WHERE id=?1 AND is_deleted=0",params![receipt.screenshot_id,category,confidence]).map_err(|e|e.to_string())?;
        }
        tx.execute("UPDATE screenshot_ocr_status SET postprocess_status='completed',postprocess_error=NULL WHERE screenshot_id=?1",[receipt.screenshot_id]).map_err(|e|e.to_string())?;
        Self::record_staged_receipt_on_conn(&tx, receipt)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }
}
