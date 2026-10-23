//! Encrypted recap payloads, source invalidation and durable request reservations.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;

use super::{BackgroundReadError, StorageState};
use crate::ai::recap::{PrivacyFingerprint, RecapSettings, RecapUsage};
use crate::calendar::CalendarDay;

const PAYLOAD_KEYS_SQL: &str =
    "SELECT key FROM recap_payloads WHERE kind=?1 AND day=?2 AND revision=?3 ORDER BY key";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecapSnapshotScope {
    pub timestamp_ms: i64,
    pub dates: Vec<String>,
    pub only_deleted: bool,
}

impl StorageState {
    pub(super) fn init_recap_tables(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS recap_days (
                day TEXT PRIMARY KEY, start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                revision INTEGER NOT NULL DEFAULT 0, privacy_hash TEXT NOT NULL,
                covered_until_ms INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS recap_payloads (
                kind TEXT NOT NULL, key TEXT NOT NULL, day TEXT NOT NULL,
                revision INTEGER NOT NULL, data BLOB NOT NULL, row_key BLOB NOT NULL,
                PRIMARY KEY(kind,key));
             CREATE INDEX IF NOT EXISTS idx_recap_payload_day ON recap_payloads(day);
             CREATE INDEX IF NOT EXISTS idx_recap_payload_day_kind_revision_key
                ON recap_payloads(day,kind,revision,key);
             CREATE TABLE IF NOT EXISTS recap_deleted_days (day TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS recap_calendars (day TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TRIGGER IF NOT EXISTS recap_calendar_delete AFTER DELETE ON recap_days BEGIN
                DELETE FROM recap_calendars WHERE day=OLD.day; END;
             CREATE TABLE IF NOT EXISTS recap_usage (
                id INTEGER PRIMARY KEY, day TEXT NOT NULL, role TEXT NOT NULL, run_id TEXT NOT NULL,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, settled INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS idx_recap_usage_day ON recap_usage(day,role);
             CREATE INDEX IF NOT EXISTS idx_recap_usage_run ON recap_usage(run_id,role);
             CREATE TABLE IF NOT EXISTS recap_summary_jobs (
                day TEXT PRIMARY KEY, version INTEGER NOT NULL,
                completed_version INTEGER NOT NULL DEFAULT 0);
             INSERT OR IGNORE INTO app_metadata(key,value) VALUES('recap_config_revision','0');
             CREATE TRIGGER IF NOT EXISTS recap_config_insert AFTER INSERT ON recap_payloads
                WHEN NEW.kind='settings' BEGIN
                UPDATE app_metadata SET value=CAST(value AS INTEGER)+1 WHERE key='recap_config_revision'; END;
             CREATE TRIGGER IF NOT EXISTS recap_config_update AFTER UPDATE ON recap_payloads
                WHEN NEW.kind='settings' BEGIN
                UPDATE app_metadata SET value=CAST(value AS INTEGER)+1 WHERE key='recap_config_revision'; END;
             CREATE TRIGGER IF NOT EXISTS recap_correction_insert AFTER INSERT ON recap_payloads
                WHEN NEW.kind='corrections' BEGIN
                INSERT INTO recap_summary_jobs(day,version) VALUES(NEW.day,1)
                ON CONFLICT(day) DO UPDATE SET version=version+1; END;
             CREATE TRIGGER IF NOT EXISTS recap_correction_update AFTER UPDATE ON recap_payloads
                WHEN NEW.kind='corrections' BEGIN
                INSERT INTO recap_summary_jobs(day,version) VALUES(NEW.day,1)
                ON CONFLICT(day) DO UPDATE SET version=version+1; END;
             INSERT OR IGNORE INTO recap_summary_jobs(day,version)
                SELECT DISTINCT day,1 FROM recap_payloads WHERE kind='corrections';"
        ).map_err(|e| e.to_string())?;
        // Real evidence edits invalidate the day: later batches may depend on
        // earlier task identities. Derived metadata and no-op writes must not
        // evict expensive recaps. Recreate legacy triggers so upgrades apply.
        let mut triggers = String::from("SAVEPOINT recap_triggers;");
        for (table, prefix, time_expr) in [
            ("screenshots", "recap_s", "strftime('%s', {row}.created_at)*1000"),
            ("ocr_results", "recap_o", "(SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id={row}.screenshot_id)"),
            ("screenshot_document_refs", "recap_d", "(SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id={row}.screenshot_id)"),
        ] {
            let mut statement = conn.prepare(&format!("PRAGMA table_info({table})")).map_err(|e| e.to_string())?;
            let columns = statement.query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
            let relevant: &[&str] = match table {
                "screenshots" => &["id", "created_at", "window_title", "process_name", "window_title_enc",
                    "process_name_enc", "page_url_enc", "content_key_encrypted", "is_deleted"],
                "ocr_results" => &["id", "screenshot_id", "text", "text_enc", "text_key_encrypted", "is_deleted"],
                _ => &["screenshot_id", "ref_enc", "content_key_encrypted"],
            };
            let mut changes = relevant.iter().filter(|c| columns.iter().any(|name| name == **c))
                .map(|c| format!("OLD.{c} IS NOT NEW.{c}")).collect::<Vec<_>>();
            if table == "screenshots" && columns.iter().any(|c| c == "status") {
                changes.push("coalesce(OLD.status='aborted',0) IS NOT coalesce(NEW.status='aborted',0)".into());
            }
            for (event, rows) in [("INSERT", vec!["NEW"]), ("UPDATE", vec!["OLD", "NEW"]), ("DELETE", vec!["OLD"])] {
                let conditions = rows.iter().map(|row| {
                    let time = time_expr.replace("{row}", row);
                    format!("({time} >= start_ms AND {time} < covered_until_ms)")
                }).collect::<Vec<_>>().join(" OR ");
                let purge = if event=="DELETE" || (table=="screenshots" && event=="UPDATE") {
                    let guard = if event=="UPDATE" {"AND OLD.is_deleted=0 AND NEW.is_deleted<>0"}else{""};
                    format!("DELETE FROM recap_payloads WHERE kind IN ('batch','index','screen','summary','search') AND day IN (SELECT day FROM recap_days WHERE {conditions}) {guard};")
                }else{String::new()};
                let when = if event == "UPDATE" { format!("WHEN {}", changes.join(" OR ")) } else { String::new() };
                triggers.push_str(&format!(
                    "DROP TRIGGER IF EXISTS {prefix}_{event};
                     CREATE TRIGGER {prefix}_{event} AFTER {event} ON {table} {when} BEGIN
                     UPDATE recap_days SET revision=revision+1 WHERE {conditions}; {purge} END;"
                ));
            }
        }
        triggers.push_str("RELEASE recap_triggers;");
        if let Err(error) = conn.execute_batch(&triggers) {
            let _ = conn.execute_batch("ROLLBACK TO recap_triggers; RELEASE recap_triggers;");
            return Err(error.to_string());
        }
        Self::init_recap_search_epoch(conn)?;
        Self::init_recap_keyword_tables(conn)?;
        Ok(())
    }

    fn init_recap_search_epoch(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS recap_search_state (
                id INTEGER PRIMARY KEY CHECK(id=1), version INTEGER NOT NULL);
             INSERT OR IGNORE INTO recap_search_state VALUES(1,0);
             CREATE TRIGGER IF NOT EXISTS recap_search_day_update AFTER UPDATE ON recap_days
                WHEN OLD.revision IS NOT NEW.revision OR OLD.privacy_hash IS NOT NEW.privacy_hash BEGIN
                UPDATE recap_search_state SET version=version+1 WHERE id=1; END;"
        ).map_err(|e| e.to_string())?;
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let row = if event == "DELETE" { "OLD" } else { "NEW" };
            conn.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS recap_search_payload_{event};
                 CREATE TRIGGER recap_search_payload_{event} AFTER {event} ON recap_payloads
                 WHEN {row}.kind IN ('batch','corrections','settings') BEGIN
                 UPDATE recap_search_state SET version=version+1 WHERE id=1; END;"
            ))
            .map_err(|e| e.to_string())?;
        }
        for table in ["recap_days", "recap_deleted_days"] {
            for event in ["INSERT", "DELETE"] {
                conn.execute_batch(&format!(
                    "CREATE TRIGGER IF NOT EXISTS recap_search_{table}_{event} AFTER {event} ON {table} BEGIN
                     UPDATE recap_search_state SET version=version+1 WHERE id=1; END;"
                )).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub(crate) fn recap_search_epoch(&self) -> Result<i64, String> {
        let guard = self.get_connection_named("recap_search_epoch")?;
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .query_row(
                "SELECT version FROM recap_search_state WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    /// Search covers all saved dates, independently of the calendar's 730-date
    /// discovery limit and the screenshot MiniLM index's 30-day retention.
    pub(crate) fn recap_search_days(&self, privacy: &str) -> Result<Vec<(String, i64)>, String> {
        let guard = self.get_connection_named("recap_search_days")?;
        let mut statement = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?.prepare(
            "SELECT d.day,d.revision FROM recap_days d
             WHERE d.day NOT IN (SELECT day FROM recap_deleted_days)
             AND d.privacy_hash=?1
             AND EXISTS(SELECT 1 FROM recap_payloads p WHERE p.kind='batch' AND p.day=d.day AND p.revision=d.revision)
             ORDER BY d.day DESC"
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([privacy], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
    }

    /// Covering lookup avoids decrypting or sorting unrelated history when
    /// opening the search shards for one day.
    pub(crate) fn recap_payload_keys(
        &self,
        kind: &str,
        day: &str,
        revision: i64,
    ) -> Result<Vec<String>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let guard = self.get_connection_named("recap_payload_keys")?;
        let mut statement = guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .prepare(PAYLOAD_KEYS_SQL)
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![kind, day, revision], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
    }

    pub(crate) fn recap_day_revision(
        &self,
        day: &str,
        start: i64,
        end: i64,
        privacy: &PrivacyFingerprint,
    ) -> Result<(i64, i64, i64), String> {
        let guard = self.get_connection_named("recap_day_revision")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let conn = &*tx;
        let calendar = ensure_day_calendar(conn, day, start, end)?;
        let (start, end) = (calendar.start_ms, calendar.end_ms);
        let previous: Option<String> = conn
            .query_row(
                "SELECT privacy_hash FROM recap_days WHERE day=?",
                [day],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let adopt_legacy = match previous.as_deref() {
            Some(hash) if hash != privacy.as_str() => privacy.matches_legacy(hash)?,
            _ => false,
        };
        let covered = calendar
            .closed_periods(chrono::Utc::now().timestamp_millis())
            .last()
            .map(|(_, b)| *b)
            .unwrap_or(start);
        conn.execute("INSERT INTO recap_days(day,start_ms,end_ms,privacy_hash,covered_until_ms) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(day) DO UPDATE SET revision=revision+CASE WHEN ?6 AND privacy_hash=?7 THEN 0 ELSE 1 END,privacy_hash=excluded.privacy_hash
            WHERE privacy_hash<>excluded.privacy_hash", params![day,start,end,privacy.as_str(),covered,adopt_legacy,previous]).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE recap_days SET covered_until_ms=max(covered_until_ms,?) WHERE day=?",
            params![covered, day],
        )
        .map_err(|e| e.to_string())?;
        let result = conn
            .query_row(
                "SELECT revision,start_ms,end_ms FROM recap_days WHERE day=?",
                [day],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }

    pub(crate) fn recap_calendar(&self, day: &str) -> Result<CalendarDay, String> {
        let guard = self.get_connection_named("recap_calendar")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let (start, end) = conn
            .query_row(
                "SELECT start_ms,end_ms FROM recap_days WHERE day=?",
                [day],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        ensure_day_calendar(conn, day, start, end)
    }

    pub(crate) fn recap_revision_matches(
        &self,
        day: &str,
        revision: i64,
        generation: u64,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("recap_revision_matches")?;
        if generation != self.db_generation() {
            return Ok(false);
        }
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM recap_days WHERE day=? AND revision=?)",
                params![day, revision],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    /// Fence a composed read against the revision of the generating run, even
    /// when the read itself successfully observes a newer, empty day.
    pub(crate) fn recap_read_at_revision<T>(
        &self,
        day: &str,
        revision: i64,
        generation: u64,
        read: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let value = read()?;
        if !self.recap_revision_matches(day, revision, generation)? {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        Ok(value)
    }

    pub(crate) fn recap_pending_summaries(&self) -> Result<HashMap<String, i64>, String> {
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        let guard = self.get_connection_named("recap_pending_summaries")?;
        let mut stmt = guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .prepare("SELECT day,version FROM recap_summary_jobs WHERE version>completed_version")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// Retain the version counter after completion so neither a restart nor a
    /// late acknowledgement can erase a newer correction. The payload triggers
    /// enqueue atomically with its encrypted write, including undo operations.
    pub(crate) fn recap_finish_summary_update(
        &self,
        day: &str,
        version: i64,
        generation: u64,
    ) -> Result<(), String> {
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        let guard = self.get_connection_named("recap_finish_summary_update")?;
        if self.db_generation() != generation {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .execute(
                "UPDATE recap_summary_jobs SET completed_version=version WHERE day=? AND version=?",
                params![day, version],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) fn recap_read<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
        revision: Option<i64>,
    ) -> Result<Option<T>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        self.recap_read_background(kind, key, revision)
    }

    /// Internal generation path. Public commands retain their session gate.
    pub(crate) fn recap_read_background<T: DeserializeOwned>(
        &self,
        kind: &str,
        key: &str,
        revision: Option<i64>,
    ) -> Result<Option<T>, String> {
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        let generation = self.db_generation();
        let row: Option<(Vec<u8>, Vec<u8>)> = {
            let guard = self.get_connection_named("recap_read")?;
            guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?.query_row(
                "SELECT data,row_key FROM recap_payloads WHERE kind=?1 AND key=?2 AND (?3 IS NULL OR revision=?3)",
                params![kind,key,revision], |r| Ok((r.get(0)?,r.get(1)?))
            ).optional().map_err(|e| e.to_string())?
        };
        let Some((data, key)) = row else {
            return Ok(None);
        };
        let mut bytes = self
            .decrypt_payload_with_row_key_silent(&data, &key)
            .map_err(|_| "AUTH_REQUIRED")?;
        let result = serde_json::from_slice(&bytes).map_err(|_| "RECAP_INVALID_CACHE".to_string());
        Self::zeroize_bytes(&mut bytes);
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        result.map(Some)
    }

    pub(crate) fn recap_write<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        day: &str,
        revision: i64,
        generation: u64,
        value: &T,
    ) -> Result<(), String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        self.recap_write_background(kind, key, day, revision, generation, value)
    }

    pub(crate) fn recap_write_background<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        day: &str,
        revision: i64,
        generation: u64,
        value: &T,
    ) -> Result<(), String> {
        self.recap_write_background_checked(kind, key, day, revision, generation, value, |_| Ok(()))
    }

    pub(crate) fn recap_write_background_checked<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        day: &str,
        revision: i64,
        generation: u64,
        value: &T,
        check: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        if !self.is_silent_read_authorized() {
            return Err("AUTH_REQUIRED".into());
        }
        self.recap_commit_payload_checked(kind, key, day, revision, generation, value, check)
    }

    /// Encryption needs only the public key. The protected caller must validate
    /// its independent service-owned authorization before entering this writer.
    pub(super) fn recap_commit_payload_checked<T: Serialize>(
        &self,
        kind: &str,
        key: &str,
        day: &str,
        revision: i64,
        generation: u64,
        value: &T,
        check: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| "RECAP_INVALID_CACHE")?;
        let encrypted = self.encrypt_payload_with_row_key(&bytes);
        Self::zeroize_bytes(&mut bytes);
        let (data, row_key) = encrypted?;
        let guard = self.get_connection_named("recap_write")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        check(conn)?;
        if revision >= 0
            && !conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM recap_days WHERE day=? AND revision=?)",
                    params![day, revision],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(|e| e.to_string())?
        {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        conn.execute("INSERT INTO recap_payloads(kind,key,day,revision,data,row_key) VALUES (?1,?2,?3,?4,?5,?6)
            ON CONFLICT(kind,key) DO UPDATE SET day=excluded.day,revision=excluded.revision,data=excluded.data,row_key=excluded.row_key",
            params![kind,key,day,revision,data,row_key]).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Resolve a snapshot against saved calendars, including dates older than
    /// the calendar picker limit. A read must not revive a deleted recap.
    pub(crate) fn recap_snapshot_scope(
        &self,
        screenshot_id: i64,
    ) -> Result<Option<RecapSnapshotScope>, String> {
        let guard = self.get_connection_named("recap_snapshot_scope")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let timestamp_ms = conn
            .query_row(
                "SELECT CAST(strftime('%s',created_at) AS INTEGER)*1000
                 FROM screenshots WHERE id=?1 AND is_deleted=0",
                [screenshot_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(timestamp_ms) = timestamp_ms else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT d.day,EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=d.day)
                 FROM recap_days d WHERE d.start_ms<=?1 AND d.end_ms>?1 ORDER BY d.day",
            )
            .map_err(|e| e.to_string())?;
        let days = stmt
            .query_map([timestamp_ms], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let only_deleted = !days.is_empty() && days.iter().all(|(_, deleted)| *deleted);
        Ok(Some(RecapSnapshotScope {
            timestamp_ms,
            dates: days
                .into_iter()
                .filter_map(|(date, deleted)| (!deleted).then_some(date))
                .collect(),
            only_deleted,
        }))
    }

    pub(crate) fn recap_list_days(&self) -> Result<Vec<String>, String> {
        let guard = self.get_connection_named("recap_list_days")?;
        let mut stmt = guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .prepare("SELECT day FROM recap_days WHERE day NOT IN (SELECT day FROM recap_deleted_days) ORDER BY day DESC LIMIT 730")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
    }

    pub(crate) fn recap_delete_day(&self, day: &str, generation: u64) -> Result<(), String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let mut guard = self.get_connection_named("recap_delete_day")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let tx = guard
            .as_mut()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .transaction()
            .map_err(|e| e.to_string())?;
        Self::delete_recap_rows(&tx, day)?;
        tx.commit().map_err(|e| e.to_string())
    }

    fn delete_recap_rows(conn: &Connection, day: &str) -> Result<(), String> {
        super::recap_search::remove_day(conn, day)?;
        // Keep the revision monotonic and retain spent budgets. Reading the day
        // must not revive old results or enable additional requests for free.
        for sql in [
            "UPDATE recap_days SET revision=revision+1 WHERE day=?",
            "DELETE FROM recap_payloads WHERE day=?",
            "DELETE FROM recap_summary_jobs WHERE day=?",
            "INSERT OR IGNORE INTO recap_deleted_days(day) VALUES (?)",
        ] {
            conn.execute(sql, [day]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn recap_is_deleted(&self, day: &str) -> Result<bool, String> {
        let guard = self.get_connection_named("recap_is_deleted")?;
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM recap_deleted_days WHERE day=?)",
                [day],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn recap_allow_generation(&self, day: &str, generation: u64) -> Result<(), String> {
        let guard = self.get_connection_named("recap_allow_generation")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .execute("DELETE FROM recap_deleted_days WHERE day=?", [day])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Keyset pagination includes every record, including rows at identical timestamps.
    pub(crate) fn recap_source_page(
        &self,
        start: i64,
        end: i64,
        after: &(String, i64),
    ) -> Result<Vec<(i64, String)>, String> {
        let format = |ms| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .ok_or("RECAP_INVALID_DATE")
        };
        let guard = self.get_connection_named("recap_source_page")?;
        let mut stmt = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?.prepare(
            "SELECT id,created_at FROM screenshots WHERE is_deleted=0 AND (status IS NULL OR status<>'aborted')
             AND created_at>=?1 AND created_at<?2 AND (created_at>?3 OR (created_at=?3 AND id>?4))
             ORDER BY created_at,id LIMIT 128"
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![format(start)?, format(end)?, after.0, after.1],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
    }

    /// Read only icon metadata from already-filtered recap sources. Extension
    /// metadata stores a website favicon in process_icon, so exclude it here.
    pub(crate) fn recap_process_icon(&self, ids: &[i64]) -> Result<Option<String>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        for chunk in ids.chunks(128) {
            let rows = {
                let guard = self.get_connection_named("recap_process_icon")?;
                let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
                let placeholders = vec!["?"; chunk.len()].join(",");
                let mut statement = conn
                    .prepare(&format!(
                        "SELECT metadata,metadata_enc,content_key_encrypted FROM screenshots
                     WHERE id IN ({placeholders}) AND is_deleted=0
                     AND (status IS NULL OR status<>'aborted')
                     AND (source IS NULL OR source<>'extension')
                     AND (metadata IS NOT NULL OR metadata_enc IS NOT NULL)
                     ORDER BY id DESC"
                    ))
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map(rusqlite::params_from_iter(chunk), |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                rows
            };
            // Decrypt outside the database mutex; stop as soon as an icon is found.
            for (plain, encrypted, key) in rows {
                if !self.is_session_valid() {
                    return Err("AUTH_REQUIRED".into());
                }
                let mut bytes = match (encrypted, key) {
                    (Some(data), Some(key)) => {
                        match self.decrypt_payload_with_row_key_silent(&data, &key) {
                            Ok(bytes) => bytes,
                            Err(BackgroundReadError::AuthRequired) => {
                                return Err("AUTH_REQUIRED".into())
                            }
                            Err(_) => continue,
                        }
                    }
                    (None, _) => plain.unwrap_or_default().into_bytes(),
                    _ => continue,
                };
                #[derive(serde::Deserialize)]
                struct IconMetadata {
                    process_icon: Option<String>,
                }
                let metadata = serde_json::from_slice::<IconMetadata>(&bytes);
                Self::zeroize_bytes(&mut bytes);
                if let Some(icon) = metadata
                    .ok()
                    .and_then(|m| m.process_icon)
                    .filter(|i| !i.trim().is_empty())
                {
                    return Ok(Some(icon));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn recap_page_url(&self, id: i64) -> Result<String, String> {
        let row: Option<(Vec<u8>, Vec<u8>)> = {
            let guard = self.get_connection_named("recap_page_url")?;
            guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?.query_row(
                "SELECT page_url_enc,content_key_encrypted FROM screenshots WHERE id=? AND is_deleted=0 AND page_url_enc IS NOT NULL AND content_key_encrypted IS NOT NULL",
                [id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e| e.to_string())?
        };
        match row {
            Some((data, key)) => String::from_utf8(
                self.decrypt_payload_with_row_key_silent(&data, &key)
                    .map_err(|_| "AUTH_REQUIRED")?,
            )
            .map_err(|_| "RECAP_INVALID_SOURCE".into()),
            None => Ok(String::new()),
        }
    }

    pub(crate) fn recap_reserve(
        &self,
        day: &str,
        role: &str,
        run: &str,
        input: u64,
        output: u64,
        settings: &RecapSettings,
        generation: u64,
    ) -> Result<i64, String> {
        let mut guard = self.get_connection_named("recap_reserve")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let tx = guard
            .as_mut()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .transaction()
            .map_err(|e| e.to_string())?;
        let totals = |sql: &str, key: &str| -> Result<(u64, u64, u64), String> {
            tx.query_row(sql, params![key, role], |r| {
                Ok((
                    r.get::<_, i64>(0)?.max(0) as u64,
                    r.get::<_, i64>(1)?.max(0) as u64,
                    r.get::<_, i64>(2)?.max(0) as u64,
                ))
            })
            .map_err(|e| e.to_string())
        };
        let daily = totals("SELECT coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),count(*) FROM recap_usage WHERE day=? AND role=?",day)?;
        let batch = totals("SELECT coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),count(*) FROM recap_usage WHERE run_id=? AND role=?",run)?;
        if role == "screening" {
            if daily.0.saturating_add(input) > settings.screening_input_tokens {
                return Err("RECAP_BUDGET_EXHAUSTED".into());
            }
        } else if batch.2 >= 2
            || batch.0.saturating_add(input) > settings.batch_input_tokens
            || batch.1.saturating_add(output) > settings.batch_output_tokens
            || daily.0.saturating_add(input) > settings.daily_input_tokens
            || daily.1.saturating_add(output) > settings.daily_output_tokens
        {
            return Err("RECAP_BUDGET_EXHAUSTED".into());
        }
        let input = i64::try_from(input).map_err(|_| "RECAP_BUDGET_EXHAUSTED")?;
        let output = i64::try_from(output).map_err(|_| "RECAP_BUDGET_EXHAUSTED")?;
        tx.execute("INSERT INTO recap_usage(day,role,run_id,input_tokens,output_tokens) VALUES (?,?,?,?,?)",params![day,role,run,input,output]).map_err(|e| e.to_string())?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub(crate) fn recap_settle(
        &self,
        id: i64,
        input: u64,
        output: u64,
        generation: u64,
    ) -> Result<(), String> {
        let input = i64::try_from(input).map_err(|_| "RECAP_INVALID_RESPONSE")?;
        let output = i64::try_from(output).map_err(|_| "RECAP_INVALID_RESPONSE")?;
        let guard = self.get_connection_named("recap_settle")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .execute(
                "UPDATE recap_usage SET input_tokens=?1,output_tokens=?2,settled=1
             WHERE id=?3 AND settled=0 AND ?1>0 AND (role='screening' OR ?2>0)",
                params![input, output, id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) fn recap_usage(&self, day: &str) -> Result<RecapUsage, String> {
        self.with_read_connection_named("recap_usage", |conn| conn.query_row(
            "SELECT coalesce(sum(CASE WHEN role='generation' THEN input_tokens ELSE 0 END),0),
             coalesce(sum(CASE WHEN role='generation' THEN output_tokens ELSE 0 END),0),
             coalesce(sum(CASE WHEN role='screening' THEN input_tokens ELSE 0 END),0),count(*) FROM recap_usage WHERE day=?", [day],
            |r| Ok(RecapUsage {input_tokens:r.get::<_,i64>(0)?.max(0) as u64,output_tokens:r.get::<_,i64>(1)?.max(0) as u64,screening_input_tokens:r.get::<_,i64>(2)?.max(0) as u64,requests:r.get::<_,i64>(3)?.max(0) as u64})
        ).map_err(|e| e.to_string()))
    }
}

fn ensure_day_calendar(
    conn: &Connection,
    day: &str,
    start: i64,
    end: i64,
) -> Result<CalendarDay, String> {
    let previous: Option<(i64, i64)> = conn
        .query_row(
            "SELECT start_ms,end_ms FROM recap_days WHERE day=?",
            [day],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let stored: Option<String> = conn
        .query_row("SELECT data FROM recap_calendars WHERE day=?", [day], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(data) = stored {
        let calendar: CalendarDay =
            serde_json::from_str(&data).map_err(|_| "RECAP_INVALID_CACHE")?;
        if calendar.date != day
            || calendar.start_ms >= calendar.end_ms
            || previous.is_some_and(|bounds| bounds != (calendar.start_ms, calendar.end_ms))
            || calendar.period_boundaries_ms.first() != Some(&calendar.start_ms)
            || calendar.period_boundaries_ms.last() != Some(&calendar.end_ms)
            || calendar
                .period_boundaries_ms
                .windows(2)
                .any(|w| w[0] >= w[1])
        {
            return Err("RECAP_INVALID_CACHE".into());
        }
        return Ok(calendar);
    }
    let calendar = if let Some((start, end)) = previous {
        let mut statement = conn.prepare("SELECT p.key FROM recap_payloads p JOIN recap_days d ON d.day=p.day AND d.revision=p.revision WHERE p.day=? AND p.kind IN ('batch','index')").map_err(|e| e.to_string())?;
        let keys = statement
            .query_map([day], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let starts: Vec<i64> = keys
            .iter()
            .filter_map(|key| key.rsplit_once(':')?.1.parse().ok())
            .collect();
        CalendarDay::recorded(day, start, end, &starts)?
    } else {
        let current = CalendarDay::local(day)?;
        if (current.start_ms, current.end_ms) == (start, end) {
            current
        } else {
            CalendarDay::recorded(day, start, end, &[])?
        }
    };
    conn.execute(
        "INSERT INTO recap_calendars(day,data) VALUES(?,?)",
        params![
            day,
            serde_json::to_string(&calendar).map_err(|e| e.to_string())?
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(calendar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use crate::sensitive_filter::SensitiveFilterConfig;
    use std::sync::Arc;

    fn privacy(enabled: bool) -> PrivacyFingerprint {
        PrivacyFingerprint::new(SensitiveFilterConfig {
            enabled,
            ..Default::default()
        })
        .unwrap()
    }

    fn storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE screenshots(id INTEGER PRIMARY KEY,created_at TEXT,status TEXT,is_deleted INTEGER DEFAULT 0,window_title TEXT,metadata TEXT);
            CREATE TABLE ocr_results(id INTEGER PRIMARY KEY,screenshot_id INTEGER,text TEXT);
            CREATE TABLE screenshot_document_refs(screenshot_id INTEGER PRIMARY KEY,ref_enc BLOB,updated_at TEXT);").unwrap();
        StorageState::init_recap_tables(&conn).unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        (temp, storage)
    }

    #[test]
    fn snapshot_recap_scope_uses_recorded_bounds_and_excludes_deleted_sources() {
        let (_temp, s) = storage();
        let timestamp_ms = 1_767_225_600_000_i64;
        {
            let guard = s.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            conn.execute_batch(
                "INSERT INTO screenshots(id,created_at,status) VALUES(1,'2026-01-01 00:00:00','ready');
                 INSERT INTO screenshots(id,created_at,status,is_deleted) VALUES(2,'2026-01-01 00:00:00','ready',1);",
            ).unwrap();
            // The saved label need not be the snapshot's date in today's zone.
            conn.execute(
                "INSERT INTO recap_days(day,start_ms,end_ms,privacy_hash,covered_until_ms)
                 VALUES('2025-12-31',?1,?2,'privacy',?2)",
                params![timestamp_ms - 1000, timestamp_ms + 1000],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO recap_days(day,start_ms,end_ms,privacy_hash,covered_until_ms)
                 VALUES('2026-01-01',?1,?2,'privacy',?2)",
                params![timestamp_ms - 2000, timestamp_ms],
            )
            .unwrap();
        }
        let scope = s.recap_snapshot_scope(1).unwrap().unwrap();
        assert_eq!(scope.timestamp_ms, timestamp_ms);
        assert_eq!(scope.dates, vec!["2025-12-31"]);
        assert!(!scope.only_deleted);
        assert!(s.recap_snapshot_scope(2).unwrap().is_none());
        assert!(s.recap_snapshot_scope(999).unwrap().is_none());
        s.db.lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .execute(
                "INSERT INTO recap_deleted_days(day) VALUES('2025-12-31')",
                [],
            )
            .unwrap();
        let scope = s.recap_snapshot_scope(1).unwrap().unwrap();
        assert!(scope.dates.is_empty());
        assert!(scope.only_deleted);
    }

    #[test]
    fn a_saved_calendar_survives_zone_changes_and_regeneration_until_its_day_is_removed() {
        let (_temp, s) = storage();
        let date = "2026-01-01";
        let original = CalendarDay::local(date).unwrap();
        let (revision, start, end) = s
            .recap_day_revision(date, original.start_ms, original.end_ms, &privacy(true))
            .unwrap();
        assert_eq!(s.recap_calendar(date).unwrap(), original);
        let changed = CalendarDay::in_zone(
            date,
            if original.time_zone.as_deref() == Some("Asia/Tokyo") {
                chrono_tz::America::New_York
            } else {
                chrono_tz::Asia::Tokyo
            },
        )
        .unwrap();
        assert_ne!(changed.start_ms, start);
        assert_eq!(
            s.recap_day_revision(date, changed.start_ms, changed.end_ms, &privacy(true))
                .unwrap(),
            (revision, start, end)
        );
        assert_eq!(
            s.recap_calendar(date).unwrap().closed_periods(end),
            original.closed_periods(end)
        );
        let mut guard = s.db.lock().unwrap();
        let conn = guard.as_mut().unwrap();
        let tx = conn.transaction().unwrap();
        StorageState::delete_recap_rows(&tx, date).unwrap();
        tx.commit().unwrap();
        // Recap deletion retains the day's monotonic revision and UTC bounds.
        // Keep the matching calendar so regeneration uses that same day.
        let saved: String = conn
            .query_row(
                "SELECT data FROM recap_calendars WHERE day=?",
                [date],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<CalendarDay>(&saved).unwrap(),
            original
        );
        conn.execute("DELETE FROM recap_days WHERE day=?", [date])
            .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM recap_calendars WHERE day=?",
                [date],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn legacy_dst_days_keep_recorded_bounds_keys_and_source_revisions() {
        for date in ["2026-03-08", "2026-11-01"] {
            let (_temp, s) = storage();
            let original = CalendarDay::in_zone(date, chrono_tz::America::New_York).unwrap();
            {
                let guard = s.db.lock().unwrap();
                let conn = guard.as_ref().unwrap();
                conn.execute(
                    "INSERT INTO recap_days VALUES(?1,?2,?3,5,?4,?3)",
                    params![
                        date,
                        original.start_ms,
                        original.end_ms,
                        privacy(true).as_str()
                    ],
                )
                .unwrap();
                for (start, _) in original.closed_periods(original.end_ms) {
                    conn.execute(
                        "INSERT INTO recap_payloads VALUES('batch',?1,?2,5,x'00',x'00')",
                        params![format!("{date}:{start}"), date],
                    )
                    .unwrap();
                }
            }
            let changed = CalendarDay::in_zone(date, chrono_tz::Asia::Shanghai).unwrap();
            assert_eq!(
                s.recap_day_revision(date, changed.start_ms, changed.end_ms, &privacy(true))
                    .unwrap(),
                (5, original.start_ms, original.end_ms)
            );
            let calendar = s.recap_calendar(date).unwrap();
            assert_eq!(calendar.time_zone, None);
            assert_eq!(calendar.rule, "recorded_bounds");
            assert_eq!(calendar.period_boundaries_ms, original.period_boundaries_ms);
            assert_eq!(
                calendar.start_utc_offset_seconds,
                original.start_utc_offset_seconds
            );
            assert_eq!(
                calendar.end_utc_offset_seconds,
                original.end_utc_offset_seconds
            );
            assert_eq!(s.recap_calendar(date).unwrap(), calendar);
        }
    }

    #[test]
    fn search_shard_lookup_uses_a_covering_range_without_walking_other_days() {
        let (_temp, s) = storage();
        let guard = s.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
            INSERT INTO recap_payloads SELECT 'search',printf('shard-%05d',x),printf('day-%05d',x),0,x'00',x'00' FROM n;").unwrap();
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {PAYLOAD_KEYS_SQL}"))
            .unwrap()
            .query_map(params!["search", "day-05000", 0], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(plan.contains("SEARCH recap_payloads USING COVERING INDEX idx_recap_payload_day_kind_revision_key"),"{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        let mut statement = conn.prepare(PAYLOAD_KEYS_SQL).unwrap();
        let keys = statement
            .query_map(params!["search", "day-05000", 0], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(keys, vec!["shard-05000"]);
        assert!(statement.get_status(rusqlite::StatementStatus::VmStep) < 100);
    }
    #[test]
    fn deleting_a_day_purges_private_payloads_and_preserves_sources_and_budget() {
        let (_temp, s) = storage();
        let generation = s.db_generation();
        {
            let mut guard = s.db.lock().unwrap();
            let conn = guard.as_mut().unwrap();
            conn.execute_batch(
                "INSERT INTO screenshots(id,created_at) VALUES(1,'2026-01-01 01:00:00');
                INSERT INTO recap_days VALUES('2026-01-01',0,1,7,'privacy',1);
                INSERT INTO recap_days VALUES('2026-01-02',1,2,0,'privacy',2);
                INSERT INTO recap_usage VALUES(1,'2026-01-01','generation','run',100,10,1);
                INSERT INTO recap_payloads VALUES('settings','settings','',-1,x'00',x'00');
                INSERT INTO recap_payloads VALUES('batch','other','2026-01-02',0,x'00',x'00');",
            )
            .unwrap();
            for kind in [
                "batch",
                "summary",
                "index",
                "screen",
                "corrections",
                "search",
            ] {
                conn.execute(
                    "INSERT INTO recap_payloads VALUES(?1,?1,'2026-01-01',7,x'00',x'00')",
                    [kind],
                )
                .unwrap();
            }
            let tx = conn.transaction().unwrap();
            StorageState::delete_recap_rows(&tx, "2026-01-01").unwrap();
            tx.commit().unwrap();
            for (table, expected) in [
                ("recap_payloads", 2),
                ("recap_summary_jobs", 0),
                ("recap_usage", 1),
                ("screenshots", 1),
            ] {
                let count: i64 = conn
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, expected, "{table}");
            }
        }
        assert!(s.recap_is_deleted("2026-01-01").unwrap());
        assert!(!s
            .recap_revision_matches("2026-01-01", 7, generation)
            .unwrap());
        assert_eq!(s.recap_list_days().unwrap(), vec!["2026-01-02"]);
        s.recap_allow_generation("2026-01-01", generation).unwrap();
        assert!(!s.recap_is_deleted("2026-01-01").unwrap());
        assert_eq!(
            s.recap_delete_day("2026-01-01", generation).unwrap_err(),
            "AUTH_REQUIRED"
        );
    }

    #[test]
    fn composed_reads_reject_a_new_empty_revision_but_allow_corrections() {
        let (_temp, s) = storage();
        let day = "2026-01-01";
        let (start, end) = crate::ai::recap::day_bounds(day).unwrap();
        s.db.lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .execute_batch(
                "INSERT INTO screenshots(id,created_at) VALUES(1,'2026-01-01 01:00:00');
             INSERT INTO ocr_results VALUES(1,1,'original');",
            )
            .unwrap();
        let (revision, _, _) = s
            .recap_day_revision(day, start, end, &privacy(true))
            .unwrap();
        let generation = s.db_generation();
        // Corrections affect summary fingerprints, not the source fence.
        s.recap_read_at_revision(day, revision, generation, || {
            s.db.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .execute(
                    "INSERT INTO recap_payloads VALUES('corrections',?1,?1,-1,x'00',x'00')",
                    [day],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
        // A late OCR write lands after the caller's check. The composed read
        // succeeds against the new revision and finds no current payloads.
        let result = s.recap_read_at_revision(day, revision, generation, || {
            s.db.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .execute("UPDATE ocr_results SET text='late OCR' WHERE id=1", [])
                .unwrap();
            let (new_revision, _, _) = s.recap_day_revision(day, start, end, &privacy(true))?;
            assert!(new_revision > revision);
            let count: i64 =
                s.db.lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .query_row(
                        "SELECT count(*) FROM recap_payloads WHERE day=?1 AND revision=?2",
                        params![day, new_revision],
                        |r| r.get(0),
                    )
                    .unwrap();
            assert_eq!(count, 0);
            Ok(Vec::<String>::new())
        });
        assert_eq!(result.unwrap_err(), "RECAP_SOURCE_CHANGED");
        // This also fences the empty-loop path when invalidation predates read.
        assert_eq!(
            s.recap_read_at_revision(day, revision, generation, || Ok(()))
                .unwrap_err(),
            "RECAP_SOURCE_CHANGED"
        );
    }

    #[test]
    fn search_epoch_tracks_sources_and_corrections_but_not_embedding_writes() {
        let (_temp, s) = storage();
        let write = |sql: &str| {
            s.db.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .execute_batch(sql)
                .unwrap()
        };
        write(
            "INSERT INTO recap_days VALUES('2026-01-01',0,9999999999999,0,'privacy',9999999999999);
               INSERT INTO recap_payloads VALUES('batch','batch','2026-01-01',0,x'00',x'00');",
        );
        let original = s.recap_search_epoch().unwrap();
        assert_eq!(
            s.recap_search_days("privacy").unwrap(),
            vec![("2026-01-01".into(), 0)]
        );
        assert!(s.recap_search_days("changed-privacy").unwrap().is_empty());
        assert!(s
            .recap_keyword_pending_dates("changed-privacy")
            .unwrap()
            .is_empty());
        assert_eq!(s.recap_search_epoch().unwrap(), original);
        write(
            "INSERT INTO recap_payloads VALUES('search','2026-01-01','2026-01-01',0,x'00',x'00');",
        );
        assert_eq!(s.recap_search_epoch().unwrap(), original);
        write("INSERT INTO recap_payloads VALUES('corrections','2026-01-01','2026-01-01',-1,x'00',x'00');");
        let corrected = s.recap_search_epoch().unwrap();
        assert!(corrected > original);
        write("INSERT INTO screenshots(id,created_at) VALUES(1,'2026-01-01 01:00:00');");
        assert!(s.recap_search_epoch().unwrap() > corrected);
        assert!(
            s.recap_search_days("privacy").unwrap().is_empty(),
            "stale batches must leave the corpus"
        );
        write("UPDATE screenshots SET is_deleted=1 WHERE id=1;");
        let count: i64 =
            s.db.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM recap_payloads WHERE kind='search'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(count, 0, "deletion purges saved vectors as well as text");
    }

    #[test]
    fn correction_jobs_migrate_once_and_enqueue_atomically() {
        let (_temp, s) = storage();
        let guard = s.db.lock().unwrap();
        let conn = guard.as_ref().unwrap();
        conn.execute_batch(
            "DROP TRIGGER recap_correction_insert;
             DROP TRIGGER recap_correction_update;
             DROP TABLE recap_summary_jobs;
             INSERT INTO recap_payloads VALUES('corrections','old','old',-1,x'00',x'00');",
        )
        .unwrap();
        StorageState::init_recap_tables(conn).unwrap();
        let job = || {
            conn.query_row(
                "SELECT version,completed_version FROM recap_summary_jobs WHERE day='old'",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(job(), (1, 0));
        conn.execute(
            "UPDATE recap_summary_jobs SET completed_version=version",
            [],
        )
        .unwrap();
        StorageState::init_recap_tables(conn).unwrap();
        assert_eq!(job(), (1, 1));
        conn.execute_batch(
            "CREATE TRIGGER reject_job BEFORE UPDATE ON recap_summary_jobs BEGIN
                SELECT RAISE(ABORT,'injected queue failure'); END;",
        )
        .unwrap();
        assert!(conn
            .execute("UPDATE recap_payloads SET data=x'01' WHERE key='old'", [])
            .is_err());
        let data: Vec<u8> = conn
            .query_row("SELECT data FROM recap_payloads WHERE key='old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(data, vec![0]);
        assert_eq!(job(), (1, 1));
        conn.execute_batch(
            "DROP TRIGGER reject_job;
            UPDATE recap_payloads SET data=x'01' WHERE key='old';",
        )
        .unwrap();
        assert_eq!(job(), (2, 1));
    }

    #[test]
    fn pending_and_failed_requests_keep_their_reservations() {
        let (_temp, s) = storage();
        let settings = RecapSettings {
            daily_input_tokens: 10_000,
            daily_output_tokens: 2000,
            batch_input_tokens: 10_000,
            batch_output_tokens: 2000,
            ..Default::default()
        };
        let g = s.db_generation();
        let first = s
            .recap_reserve("day", "generation", "one", 7000, 1000, &settings, g)
            .unwrap();
        assert_eq!(s.recap_usage("day").unwrap().input_tokens, 7000);
        assert_eq!(
            s.recap_reserve("day", "generation", "two", 4000, 1000, &settings, g)
                .unwrap_err(),
            "RECAP_BUDGET_EXHAUSTED"
        );
        // Some compatible endpoints report absent usage fields as zero. Such
        // responses must not turn potentially billable work into a free run.
        s.recap_settle(first, 0, 0, g).unwrap();
        s.recap_settle(first, 1000, 0, g).unwrap();
        assert_eq!(s.recap_usage("day").unwrap().input_tokens, 7000);
        assert_eq!(s.recap_usage("day").unwrap().output_tokens, 1000);
        s.recap_settle(first, 2000, 300, g).unwrap();
        s.recap_settle(first, 1, 1, g).unwrap(); // Settlement is idempotent.
        assert_eq!(s.recap_usage("day").unwrap().input_tokens, 2000);
        s.recap_reserve("day", "generation", "one", 1000, 100, &settings, g)
            .unwrap();
        assert_eq!(
            s.recap_reserve("day", "generation", "one", 1, 1, &settings, g)
                .unwrap_err(),
            "RECAP_BUDGET_EXHAUSTED"
        );
        s.recap_reserve("day", "screening", "one", 3000, 0, &settings, g)
            .unwrap();
        assert_eq!(s.recap_usage("day").unwrap().screening_input_tokens, 3000);
        assert_eq!(s.recap_usage("day").unwrap().input_tokens, 3000);
    }
    #[test]
    fn future_captures_do_not_invalidate_closed_periods_but_source_edits_do() {
        let (_temp, s) = storage();
        let (start, end) = crate::ai::recap::day_bounds("2026-01-01").unwrap();
        s.recap_day_revision("2026-01-01", start, end, &privacy(true))
            .unwrap();
        let conn = s.db.lock().unwrap();
        let conn = conn.as_ref().unwrap();
        conn.execute(
            "UPDATE recap_days SET covered_until_ms=?",
            [start + 4 * 3_600_000],
        )
        .unwrap();
        let format = |ms| {
            chrono::DateTime::from_timestamp_millis(ms)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        };
        conn.execute(
            "INSERT INTO screenshots(id,created_at) VALUES (1,?)",
            [format(start + 5 * 3_600_000)],
        )
        .unwrap();
        let revision = || {
            conn.query_row("SELECT revision FROM recap_days", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(revision(), 0);
        conn.execute(
            "INSERT INTO screenshots(id,created_at) VALUES (2,?)",
            [format(start + 60_000)],
        )
        .unwrap();
        let before = revision();
        assert!(before > 0);
        conn.execute("INSERT INTO ocr_results(id,screenshot_id) VALUES (1,2)", [])
            .unwrap();
        assert!(revision() > before);
        let before = revision();
        conn.execute("UPDATE screenshots SET is_deleted=1 WHERE id=2", [])
            .unwrap();
        assert!(revision() > before);
        let before = revision();
        conn.execute("DELETE FROM screenshots WHERE id=2", [])
            .unwrap();
        assert!(revision() > before);
    }
    #[test]
    fn trigger_upgrade_keeps_recaps_on_noop_and_unrelated_updates_and_purges_summaries_on_delete() {
        let (_temp, s) = storage();
        let conn = s.db.lock().unwrap();
        let conn = conn.as_ref().unwrap();
        conn.execute_batch(
            "DROP TRIGGER recap_s_UPDATE;
            CREATE TRIGGER recap_s_UPDATE AFTER UPDATE ON screenshots BEGIN
            UPDATE recap_days SET revision=revision+1; END;",
        )
        .unwrap();
        StorageState::init_recap_tables(conn).unwrap();
        conn.execute_batch("INSERT INTO screenshots(id,created_at,window_title,status) VALUES(1,'2026-01-01 01:00:00','original','ready');
            INSERT INTO ocr_results VALUES(1,1,'text');
            INSERT INTO screenshot_document_refs VALUES(1,x'01','old');
            INSERT INTO recap_days VALUES('day',0,9999999999999,0,'privacy',9999999999999);
            INSERT INTO recap_payloads VALUES('summary','key','day',0,x'00',x'00');").unwrap();
        let revision = || {
            conn.query_row("SELECT revision FROM recap_days WHERE day='day'", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        conn.execute_batch(
            "UPDATE screenshots SET window_title=window_title;
            UPDATE screenshots SET metadata='background bookkeeping',status='processed';
            UPDATE ocr_results SET text=text;
            UPDATE screenshot_document_refs SET updated_at='new';",
        )
        .unwrap();
        assert_eq!(revision(), 0);
        conn.execute("UPDATE screenshots SET window_title='changed'", [])
            .unwrap();
        assert_eq!(revision(), 1);
        conn.execute("UPDATE ocr_results SET text='edited'", [])
            .unwrap();
        assert_eq!(revision(), 2);
        conn.execute("UPDATE screenshots SET status='aborted'", [])
            .unwrap();
        assert_eq!(revision(), 3);
        conn.execute("UPDATE screenshots SET is_deleted=1", [])
            .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM recap_payloads WHERE kind='summary'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn privacy_changes_invalidate_and_keyset_paging_has_no_500_row_cutoff() {
        let (_temp, s) = storage();
        let (start, end) = crate::ai::recap::day_bounds("2026-01-01").unwrap();
        let (revision, _, _) = s
            .recap_day_revision("2026-01-01", start, end, &privacy(true))
            .unwrap();
        assert_eq!(
            s.recap_day_revision("2026-01-01", start, end, &privacy(true))
                .unwrap()
                .0,
            revision
        );
        assert!(
            s.recap_day_revision("2026-01-01", start, end, &privacy(false))
                .unwrap()
                .0
                > revision
        );
        let time = chrono::DateTime::from_timestamp_millis(start + 60_000)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        {
            let guard = s.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            for id in 1..=601 {
                conn.execute(
                    "INSERT INTO screenshots(id,created_at) VALUES (?,?)",
                    params![id, time],
                )
                .unwrap();
            }
            conn.execute("UPDATE screenshots SET is_deleted=1 WHERE id=500", [])
                .unwrap();
        }
        let mut cursor = (String::new(), 0);
        let mut ids = Vec::new();
        loop {
            let page = s.recap_source_page(start, end, &cursor).unwrap();
            let Some((id, time)) = page.last() else { break };
            cursor = (time.clone(), *id);
            ids.extend(page.into_iter().map(|(id, _)| id));
        }
        assert_eq!(ids.len(), 600);
        assert_eq!(ids.last(), Some(&601));
        assert!(!ids.contains(&500));
    }
    #[test]
    fn legacy_privacy_upgrade_preserves_current_payloads_without_reviving_stale_ones() {
        let (_temp, s) = storage();
        let config = SensitiveFilterConfig {
            version: 2,
            ..SensitiveFilterConfig::default()
        };
        let legacy = crate::ai::recap::digest(&serde_json::to_string(&config).unwrap());
        let saved = serde_json::to_value(config).unwrap();
        let reloaded = PrivacyFingerprint::new(serde_json::from_value(saved).unwrap()).unwrap();
        let (start, end) = crate::ai::recap::day_bounds("2026-01-01").unwrap();
        {
            let guard = s.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            for (day, revision) in [("2026-01-01", 5), ("2026-01-02", 6)] {
                let (start, end) = crate::ai::recap::day_bounds(day).unwrap();
                conn.execute(
                    "INSERT INTO recap_days VALUES (?1,?2,?3,?4,?5,?3)",
                    params![day, start, end, revision, legacy],
                )
                .unwrap();
                for kind in ["batch", "summary", "index"] {
                    conn.execute(
                        "INSERT INTO recap_payloads VALUES (?1,?2,?2,5,x'00',x'00')",
                        params![kind, day],
                    )
                    .unwrap();
                }
            }
        }
        for (day, revision, visible) in [("2026-01-01", 5, 3), ("2026-01-02", 6, 0)] {
            let (start, end) = crate::ai::recap::day_bounds(day).unwrap();
            assert_eq!(
                s.recap_day_revision(day, start, end, &reloaded).unwrap().0,
                revision
            );
            let guard = s.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            let stored: String = conn
                .query_row(
                    "SELECT privacy_hash FROM recap_days WHERE day=?",
                    [day],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stored, reloaded.as_str());
            let count: i64 = conn.query_row("SELECT count(*) FROM recap_payloads p JOIN recap_days d ON p.day=d.day AND p.revision=d.revision WHERE d.day=?",
                [day], |r| r.get(0)).unwrap();
            assert_eq!(count, visible);
        }
        assert_eq!(
            s.recap_day_revision("2026-01-01", start, end, &privacy(false))
                .unwrap()
                .0,
            6
        );
    }

    #[test]
    fn old_database_generation_cannot_spend_or_settle_against_a_replacement() {
        let (_temp, s) = storage();
        let g = s.db_generation();
        s.bump_db_generation();
        assert_eq!(
            s.recap_reserve("day", "generation", "run", 1, 1, &Default::default(), g)
                .unwrap_err(),
            "RECAP_SOURCE_CHANGED"
        );
        assert_eq!(
            s.recap_settle(1, 1, 1, g).unwrap_err(),
            "RECAP_SOURCE_CHANGED"
        );
        assert!(s
            .recap_read::<RecapSettings>("settings", "settings", None)
            .is_err());
    }
}
