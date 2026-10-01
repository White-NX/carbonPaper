//! Encrypted recap payloads, source invalidation and durable request reservations.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;

use super::StorageState;
use crate::ai::recap::{PrivacyFingerprint, RecapSettings, RecapUsage};

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
             CREATE TABLE IF NOT EXISTS recap_usage (
                id INTEGER PRIMARY KEY, day TEXT NOT NULL, role TEXT NOT NULL, run_id TEXT NOT NULL,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, settled INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX IF NOT EXISTS idx_recap_usage_day ON recap_usage(day,role);
             CREATE INDEX IF NOT EXISTS idx_recap_usage_run ON recap_usage(run_id,role);
             CREATE TABLE IF NOT EXISTS recap_summary_jobs (
                day TEXT PRIMARY KEY, version INTEGER NOT NULL,
                completed_version INTEGER NOT NULL DEFAULT 0);
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
                    format!("DELETE FROM recap_payloads WHERE kind IN ('batch','index','screen','summary') AND day IN (SELECT day FROM recap_days WHERE {conditions}) {guard};")
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
        Ok(())
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
        let covered =
            crate::ai::recap::batch_bounds(start, end, chrono::Utc::now().timestamp_millis())
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
        conn.query_row(
            "SELECT revision,start_ms,end_ms FROM recap_days WHERE day=?",
            [day],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())
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
        if !self.is_session_valid() {
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
        if !self.is_session_valid() {
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
        let mut bytes = serde_json::to_vec(value).map_err(|_| "RECAP_INVALID_CACHE")?;
        let encrypted = self.encrypt_payload_with_row_key(&bytes);
        Self::zeroize_bytes(&mut bytes);
        let (data, row_key) = encrypted?;
        let guard = self.get_connection_named("recap_write")?;
        if generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
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

    pub(crate) fn recap_list_days(&self) -> Result<Vec<String>, String> {
        let guard = self.get_connection_named("recap_list_days")?;
        let mut stmt = guard
            .as_ref()
            .ok_or("RECAP_STORAGE_UNAVAILABLE")?
            .prepare("SELECT day FROM recap_days ORDER BY day DESC LIMIT 730")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
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
        let guard = self.get_connection_named("recap_usage")?;
        guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?.query_row(
            "SELECT coalesce(sum(CASE WHEN role='generation' THEN input_tokens ELSE 0 END),0),
             coalesce(sum(CASE WHEN role='generation' THEN output_tokens ELSE 0 END),0),
             coalesce(sum(CASE WHEN role='screening' THEN input_tokens ELSE 0 END),0),count(*) FROM recap_usage WHERE day=?", [day],
            |r| Ok(RecapUsage {input_tokens:r.get::<_,i64>(0)?.max(0) as u64,output_tokens:r.get::<_,i64>(1)?.max(0) as u64,screening_input_tokens:r.get::<_,i64>(2)?.max(0) as u64,requests:r.get::<_,i64>(3)?.max(0) as u64})
        ).map_err(|e| e.to_string())
    }
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
        conn.execute_batch("CREATE TABLE screenshots(id INTEGER PRIMARY KEY,created_at TEXT,status TEXT,is_deleted INTEGER DEFAULT 0,window_title TEXT,metadata TEXT);
            CREATE TABLE ocr_results(id INTEGER PRIMARY KEY,screenshot_id INTEGER,text TEXT);
            CREATE TABLE screenshot_document_refs(screenshot_id INTEGER PRIMARY KEY,ref_enc BLOB,updated_at TEXT);").unwrap();
        StorageState::init_recap_tables(&conn).unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        (temp, storage)
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
        let config = SensitiveFilterConfig::default();
        let legacy = crate::ai::recap::digest(&serde_json::to_string(&config).unwrap());
        let saved = serde_json::to_value(config).unwrap();
        let reloaded = PrivacyFingerprint::new(serde_json::from_value(saved).unwrap()).unwrap();
        let (start, end) = crate::ai::recap::day_bounds("2026-01-01").unwrap();
        {
            let guard = s.db.lock().unwrap();
            let conn = guard.as_ref().unwrap();
            for (day, revision) in [("current", 5), ("stale", 6)] {
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
        for (day, revision, visible) in [("current", 5, 3), ("stale", 6, 0)] {
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
            s.recap_day_revision("current", start, end, &privacy(false))
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
