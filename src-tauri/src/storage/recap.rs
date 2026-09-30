//! Encrypted recap payloads, source invalidation and durable request reservations.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

use super::StorageState;
use crate::ai::recap::{RecapSettings, RecapUsage};

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
             CREATE INDEX IF NOT EXISTS idx_recap_usage_run ON recap_usage(run_id,role);"
        ).map_err(|e| e.to_string())?;
        // Invalidate the whole source day: later batches may have used earlier
        // task indices. Corrections and usage survive invalidation.
        for (table, prefix, time_expr) in [
            ("screenshots", "recap_s", "strftime('%s', {row}.created_at)*1000"),
            ("ocr_results", "recap_o", "(SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id={row}.screenshot_id)"),
            ("screenshot_document_refs", "recap_d", "(SELECT strftime('%s',created_at)*1000 FROM screenshots WHERE id={row}.screenshot_id)"),
        ] {
            for (event, rows) in [("INSERT", vec!["NEW"]), ("UPDATE", vec!["OLD", "NEW"]), ("DELETE", vec!["OLD"])] {
                let conditions = rows.iter().map(|row| {
                    let time = time_expr.replace("{row}", row);
                    format!("({time} >= start_ms AND {time} < covered_until_ms)")
                }).collect::<Vec<_>>().join(" OR ");
                let purge = if event=="DELETE" || (table=="screenshots" && event=="UPDATE") {
                    let guard = if event=="UPDATE" {"AND OLD.is_deleted=0 AND NEW.is_deleted<>0"}else{""};
                    format!("DELETE FROM recap_payloads WHERE kind IN ('batch','index','screen') AND day IN (SELECT day FROM recap_days WHERE {conditions}) {guard};")
                }else{String::new()};
                conn.execute_batch(&format!(
                    "CREATE TRIGGER IF NOT EXISTS {prefix}_{event} AFTER {event} ON {table} BEGIN
                     UPDATE recap_days SET revision=revision+1 WHERE {conditions}; {purge} END;"
                )).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub(crate) fn recap_day_revision(
        &self,
        day: &str,
        start: i64,
        end: i64,
        privacy: &str,
    ) -> Result<(i64, i64, i64), String> {
        let guard = self.get_connection_named("recap_day_revision")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let covered =
            crate::ai::recap::batch_bounds(start, end, chrono::Utc::now().timestamp_millis())
                .last()
                .map(|(_, b)| *b)
                .unwrap_or(start);
        conn.execute("INSERT INTO recap_days(day,start_ms,end_ms,privacy_hash,covered_until_ms) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(day) DO UPDATE SET revision=revision+1,privacy_hash=excluded.privacy_hash
            WHERE privacy_hash<>excluded.privacy_hash", params![day,start,end,privacy,covered]).map_err(|e| e.to_string())?;
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
    use std::sync::Arc;

    fn storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE screenshots(id INTEGER PRIMARY KEY,created_at TEXT,status TEXT,is_deleted INTEGER DEFAULT 0);
            CREATE TABLE ocr_results(id INTEGER PRIMARY KEY,screenshot_id INTEGER);
            CREATE TABLE screenshot_document_refs(screenshot_id INTEGER PRIMARY KEY);").unwrap();
        StorageState::init_recap_tables(&conn).unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        (temp, storage)
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
        s.recap_day_revision("2026-01-01", start, end, "privacy-one")
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
    fn privacy_changes_invalidate_and_keyset_paging_has_no_500_row_cutoff() {
        let (_temp, s) = storage();
        let (start, end) = crate::ai::recap::day_bounds("2026-01-01").unwrap();
        let (revision, _, _) = s
            .recap_day_revision("2026-01-01", start, end, "one")
            .unwrap();
        assert_eq!(
            s.recap_day_revision("2026-01-01", start, end, "one")
                .unwrap()
                .0,
            revision
        );
        assert!(
            s.recap_day_revision("2026-01-01", start, end, "two")
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
