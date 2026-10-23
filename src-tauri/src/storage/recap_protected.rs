//! Restricted result commit for service-authorized recap generation.
use super::*;
use crate::processing_stage::recap::SourceVersion;
use serde::Serialize;

impl StorageState {
    pub(crate) fn recap_check_run_versions_on_connection(
        &self,
        conn: &rusqlite::Connection,
        date: &str,
        config_revision: i64,
        correction_version: i64,
    ) -> Result<(), String> {
        let current: (i64, i64) = conn
            .query_row(
                "SELECT CAST((SELECT value FROM app_metadata WHERE key='recap_config_revision') AS INTEGER),
                    COALESCE((SELECT version FROM recap_summary_jobs WHERE day=?1),0)",
                [date],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        if current != (config_revision, correction_version) {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(())
    }

    pub(crate) fn recap_config_revision(&self) -> Result<i64, String> {
        let guard = self.get_connection_named("recap_config_revision")?;
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key='recap_config_revision'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn recap_correction_version(&self, date: &str) -> Result<i64, String> {
        let guard = self.get_connection_named("recap_correction_version")?;
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .query_row(
                "SELECT COALESCE((SELECT version FROM recap_summary_jobs WHERE day=?1),0)",
                [date],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }
    pub(crate) fn recap_sources_current(&self, sources: &[SourceVersion]) -> Result<bool, String> {
        let guard = self.get_connection_named("recap_sources_current")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        for source in sources {
            let current:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM screenshots s LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
                WHERE s.id=?1 AND s.is_deleted=0 AND s.status='committed' AND COALESCE(r.revision,0)=?2)",
                rusqlite::params![source.id,source.revision],|r|r.get(0)).map_err(|e|e.to_string())?;
            if !current {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn recap_commit_protected<T: Serialize>(
        &self,
        authorization: &crate::ai::recap::protected::ProtectedRun,
        kind: &str,
        key: &str,
        day: &str,
        revision: i64,
        generation: u64,
        value: &T,
    ) -> Result<(), String> {
        authorization.check(self)?;
        if day != authorization.date || !matches!(kind, "batch" | "index" | "screen" | "summary") {
            return Err("AUTH_REQUIRED".into());
        }
        self.recap_commit_payload_checked(kind, key, day, revision, generation, value, |conn| {
            authorization.check_sources_on_connection(self, conn)
        })
    }

    pub(crate) fn recap_record_input_receipt(
        &self,
        receipt: &crate::processing_stage::TaskReceipt,
    ) -> Result<(), String> {
        if receipt.consumer != carbonpaper_app_bound::protocol::Consumer::Recap {
            return Err("invalid staged consumer".into());
        }
        self.processing_stage
            .check_receipt(receipt)
            .map_err(|e| e.to_string())?;
        let guard = self.get_connection_named("recap_input_receipt")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        if !self.staged_source_current_on_conn(conn, receipt)? {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        if Self::record_staged_receipt_on_conn(&tx, receipt)? {
            tx.execute("UPDATE recap_days SET revision=revision+1 WHERE
                (SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id=?1)>=start_ms AND
                (SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id=?1)<covered_until_ms",
                [receipt.screenshot_id]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}
