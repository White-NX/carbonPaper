//! Encrypted activity documents backed by the shared HMAC bitmap index.
use super::blind_index::{BlindIndex, MutationStats};
use super::blind_search::{self, SearchTelemetry, Stopwatch, Tier, VERIFY_CAP};
use super::search_plan::QueryPlan;
use super::{BackgroundReadError, StorageState};
use roaring::RoaringBitmap;
use rusqlite::{params, Connection};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::{HashMap, HashSet};
use zeroize::Zeroizing;

pub(crate) struct RecapKeywordCandidate<T> {
    pub value: T,
    pub hits: u32,
    pub groups: u32,
}

impl StorageState {
    /// Visibility checks use current rows while callers retain their original
    /// result positions. New rows never shift an existing page's offset.
    pub(crate) fn search_visible_snapshots(
        &self,
        rows: &[(i64, i64)],
    ) -> Result<HashSet<i64>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        self.with_read_connection_named("search_visible_snapshots", |conn| {
            let mut visible = HashSet::new();
            for batch in rows.chunks(250) {
                let values = vec!["(?,?)"; batch.len()].join(",");
                let sql = format!(
                    "WITH requested(ocr_id,screenshot_id) AS MATERIALIZED (VALUES {values})
                     SELECT requested.ocr_id FROM requested
                     CROSS JOIN ocr_results r NOT INDEXED ON r.id=requested.ocr_id
                     CROSS JOIN screenshots s NOT INDEXED ON s.id=requested.screenshot_id
                     WHERE r.screenshot_id=s.id AND r.is_deleted=0 AND s.is_deleted=0"
                );
                let parameters: Vec<i64> = batch
                    .iter()
                    .flat_map(|(ocr, screenshot)| [*ocr, *screenshot])
                    .collect();
                let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
                visible.extend(
                    statement
                        .query_map(rusqlite::params_from_iter(parameters), |row| {
                            row.get::<_, i64>(0)
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(|e| e.to_string())?,
                );
            }
            Ok(visible)
        })
    }

    pub(crate) fn search_visible_recap_days(
        &self,
        days: &[String],
    ) -> Result<HashSet<String>, String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        self.with_read_connection_named("search_visible_recaps", |conn| {
            let mut visible = HashSet::new();
            for batch in days.chunks(500) {
                let values = vec!["(?)"; batch.len()].join(",");
                let sql = format!(
                    "WITH requested(day) AS MATERIALIZED (VALUES {values})
                     SELECT DISTINCT d.day FROM requested
                     JOIN recap_days d ON d.day=requested.day
                     WHERE d.day NOT IN (SELECT day FROM recap_deleted_days)
                     AND EXISTS(SELECT 1 FROM recap_payloads p WHERE p.day=d.day
                                AND p.kind='batch' AND p.revision=d.revision)"
                );
                let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
                visible.extend(
                    statement
                        .query_map(rusqlite::params_from_iter(batch), |row| {
                            row.get::<_, String>(0)
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(|e| e.to_string())?,
                );
            }
            Ok(visible)
        })
    }

    pub(super) fn init_recap_keyword_tables(conn: &Connection) -> Result<(), String> {
        super::blind_index::ensure_recap_schema(conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS recap_search_documents (
                id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id <= 4294967295),
                day TEXT NOT NULL, revision INTEGER NOT NULL, privacy_hash TEXT NOT NULL,
                start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                data BLOB NOT NULL, row_key BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_recap_search_documents_day
                ON recap_search_documents(day,revision,privacy_hash);
             CREATE INDEX IF NOT EXISTS idx_recap_search_documents_time
                ON recap_search_documents(start_ms,end_ms);
             CREATE TABLE IF NOT EXISTS recap_search_tokens (
                document_id INTEGER NOT NULL, token_hash TEXT NOT NULL,
                PRIMARY KEY(document_id,token_hash),
                FOREIGN KEY(document_id) REFERENCES recap_search_documents(id) ON DELETE CASCADE);
             CREATE TABLE IF NOT EXISTS recap_search_processes (
                process_hash TEXT NOT NULL, document_id INTEGER NOT NULL,
                PRIMARY KEY(process_hash,document_id),
                FOREIGN KEY(document_id) REFERENCES recap_search_documents(id) ON DELETE CASCADE);
             CREATE TABLE IF NOT EXISTS recap_search_dates (
                day TEXT PRIMARY KEY, revision INTEGER NOT NULL, privacy_hash TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS recap_search_removed_tokens (
                token_hash TEXT NOT NULL, document_id INTEGER NOT NULL,
                PRIMARY KEY(token_hash,document_id));
             CREATE TRIGGER IF NOT EXISTS recap_keyword_document_delete BEFORE DELETE ON recap_search_documents BEGIN
                INSERT OR IGNORE INTO recap_search_removed_tokens(token_hash,document_id)
                    SELECT token_hash,document_id FROM recap_search_tokens WHERE document_id=OLD.id; END;
             CREATE TRIGGER IF NOT EXISTS recap_keyword_day_delete AFTER DELETE ON recap_days BEGIN
                DELETE FROM recap_search_documents WHERE day=OLD.day;
                DELETE FROM recap_search_dates WHERE day=OLD.day; END;
             CREATE TRIGGER IF NOT EXISTS recap_keyword_day_update AFTER UPDATE ON recap_days
             WHEN OLD.revision IS NOT NEW.revision OR OLD.privacy_hash IS NOT NEW.privacy_hash BEGIN
                DELETE FROM recap_search_documents WHERE day=OLD.day;
                DELETE FROM recap_search_dates WHERE day=OLD.day; END;"
        ).map_err(|e| e.to_string())?;
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let row = if event == "DELETE" { "OLD" } else { "NEW" };
            conn.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS recap_keyword_payload_{event};
                 CREATE TRIGGER recap_keyword_payload_{event} AFTER {event} ON recap_payloads
                 WHEN {row}.kind IN ('batch','corrections') BEGIN
                    DELETE FROM recap_search_documents WHERE day={row}.day;
                    DELETE FROM recap_search_dates WHERE day={row}.day; END;"
            ))
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn recap_keyword_day_revision(&self, day: &str) -> Result<i64, String> {
        self.with_read_connection_named("recap_keyword_revision", |conn| {
            conn.query_row("SELECT revision FROM recap_days WHERE day=?1", [day], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())
        })
    }

    /// Only changed dates are materialized. This lookup contains no activity text.
    pub(crate) fn recap_keyword_pending_dates(&self, privacy: &str) -> Result<Vec<String>, String> {
        self.with_read_connection_named("recap_keyword_pending", |conn| {
            let mut stmt = conn.prepare(
                "SELECT d.day FROM recap_days d LEFT JOIN recap_search_dates k ON k.day=d.day
                 WHERE d.day NOT IN (SELECT day FROM recap_deleted_days)
                 AND d.privacy_hash=?1
                 AND EXISTS(SELECT 1 FROM recap_payloads p WHERE p.day=d.day AND p.kind='batch' AND p.revision=d.revision)
                 AND (k.day IS NULL OR k.revision<>d.revision OR k.privacy_hash<>?1)
                 ORDER BY d.day"
            ).map_err(|e| e.to_string())?;
            let rows = stmt.query_map([privacy], |r| r.get(0)).map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string());
            rows
        })
    }

    pub(crate) fn recap_keyword_prune(&self, privacy: &str) -> Result<(), String> {
        let guard = self.get_connection_named("recap_keyword_prune")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut stmt = tx.prepare(
            "SELECT k.day FROM recap_search_dates k WHERE k.privacy_hash<>?1 OR NOT EXISTS(
                SELECT 1 FROM recap_days d WHERE d.day=k.day AND d.revision=k.revision
                AND d.privacy_hash=?1
                AND d.day NOT IN (SELECT day FROM recap_deleted_days)
                AND EXISTS(SELECT 1 FROM recap_payloads p WHERE p.kind='batch' AND p.day=d.day AND p.revision=d.revision))"
        ).map_err(|e| e.to_string())?;
        let days = stmt
            .query_map([privacy], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);
        for day in days {
            remove_day(&tx, &day)?;
        }
        drain_removed_tokens(&tx)?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn recap_keyword_replace_day<T: Serialize>(
        &self,
        day: &str,
        revision: i64,
        privacy: &str,
        generation: u64,
        epoch: i64,
        documents: Vec<(T, String, Vec<String>, i64, i64)>,
    ) -> Result<(), String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let hmac = Zeroizing::new(self.credential_state.get_hmac_key()?);
        let mut encrypted = Vec::with_capacity(documents.len());
        for (value, mut text, processes, start, end) in documents {
            let hashes: HashSet<_> = Self::bigram_tokenize(&text)
                .into_iter()
                .map(|token| Self::compute_hmac_hash(&token, &hmac))
                .collect();
            use zeroize::Zeroize;
            text.zeroize();
            let process_hashes: HashSet<_> = processes
                .iter()
                .map(|process| process_hash(process, &hmac))
                .collect();
            let mut bytes = serde_json::to_vec(&value).map_err(|_| "RECAP_INVALID_CACHE")?;
            let result = self.encrypt_payload_with_row_key(&bytes);
            Self::zeroize_bytes(&mut bytes);
            let (data, row_key) = result?;
            encrypted.push((data, row_key, hashes, process_hashes, start, end));
        }
        let guard = self.get_connection_named("recap_keyword_replace")?;
        let conn = guard.as_ref().ok_or("RECAP_STORAGE_UNAVAILABLE")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let current: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM recap_days WHERE day=?1 AND revision=?2 AND privacy_hash=?4
                 AND day NOT IN (SELECT day FROM recap_deleted_days))
             AND (SELECT version FROM recap_search_state WHERE id=1)=?3",
                params![day, revision, epoch, privacy],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !current || generation != self.db_generation() {
            return Err("RECAP_SOURCE_CHANGED".into());
        }
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        remove_day(&tx, day)?;
        let mut postings: HashMap<String, RoaringBitmap> = HashMap::new();
        for (data, key, tokens, processes, start, end) in encrypted {
            tx.execute("INSERT INTO recap_search_documents(day,revision,privacy_hash,start_ms,end_ms,data,row_key)
                VALUES(?1,?2,?3,?4,?5,?6,?7)", params![day,revision,privacy,start,end,data,key]).map_err(|e| e.to_string())?;
            let id = u32::try_from(tx.last_insert_rowid()).map_err(|_| "RECAP_INVALID_CACHE")?;
            for hash in tokens {
                tx.execute(
                    "INSERT INTO recap_search_tokens(document_id,token_hash) VALUES(?1,?2)",
                    params![id, hash],
                )
                .map_err(|e| e.to_string())?;
                postings.entry(hash).or_default().insert(id);
            }
            for hash in processes {
                tx.execute(
                    "INSERT INTO recap_search_processes(process_hash,document_id) VALUES(?1,?2)",
                    params![hash, id],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        let index = BlindIndex::recaps(&tx);
        let mut stats = MutationStats::default();
        for (hash, ids) in postings {
            index.add(&hash, &ids, &mut stats)?;
        }
        tx.execute(
            "INSERT INTO recap_search_dates(day,revision,privacy_hash) VALUES(?1,?2,?3)",
            params![day, revision, privacy],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn recap_keyword_candidates<T: DeserializeOwned + Send>(
        &self,
        query: &str,
        privacy: &str,
        start_date: Option<&str>,
        end_date: Option<&str>,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
        processes: &[String],
    ) -> Result<(Vec<RecapKeywordCandidate<T>>, bool), String> {
        if !self.is_session_valid() {
            return Err("AUTH_REQUIRED".into());
        }
        let hmac = Zeroizing::new(self.credential_state.get_hmac_key()?);
        let conn = self.open_read_connection_named("recap_keyword_search")?;
        let plan = QueryPlan::build(query);
        let filtered = start_date.is_some()
            || end_date.is_some()
            || start_ms.is_some()
            || end_ms.is_some()
            || !processes.is_empty();
        let allowed = if filtered {
            let mut allowed = RoaringBitmap::new();
            let mut sql = String::from(
            "SELECT k.id FROM recap_search_documents k JOIN recap_days d ON d.day=k.day AND d.revision=k.revision
             JOIN recap_search_dates v ON v.day=k.day AND v.revision=k.revision AND v.privacy_hash=k.privacy_hash
             WHERE k.privacy_hash=?1 AND k.day NOT IN (SELECT day FROM recap_deleted_days)
             AND (?2 IS NULL OR k.day>=?2) AND (?3 IS NULL OR k.day<=?3)
             AND (?4 IS NULL OR k.end_ms>=?4) AND (?5 IS NULL OR k.start_ms<=?5)"
            );
            if !processes.is_empty() {
                let parameters = (6..6 + processes.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                sql.push_str(&format!(" AND k.id IN (SELECT document_id FROM recap_search_processes WHERE process_hash IN ({parameters}))"));
            }
            if !plan.has_index_terms() {
                sql.push_str(&format!(
                    " ORDER BY k.start_ms DESC,k.id DESC LIMIT {VERIFY_CAP}"
                ));
            }
            let mut parameters: Vec<rusqlite::types::Value> = vec![
                privacy.to_owned().into(),
                start_date
                    .map(|s| s.to_owned().into())
                    .unwrap_or(rusqlite::types::Value::Null),
                end_date
                    .map(|s| s.to_owned().into())
                    .unwrap_or(rusqlite::types::Value::Null),
                start_ms
                    .map(Into::into)
                    .unwrap_or(rusqlite::types::Value::Null),
                end_ms
                    .map(Into::into)
                    .unwrap_or(rusqlite::types::Value::Null),
            ];
            parameters.extend(processes.iter().map(|p| process_hash(p, &hmac).into()));
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            for id in stmt
                .query_map(rusqlite::params_from_iter(parameters), |r| {
                    r.get::<_, u32>(0)
                })
                .map_err(|e| e.to_string())?
            {
                allowed.insert(id.map_err(|e| e.to_string())?);
            }
            Some(allowed)
        } else {
            None
        };
        let mut telemetry = SearchTelemetry::new();
        let mut watch = Stopwatch::start();
        let mut candidates = if plan.has_index_terms() {
            blind_search::recall(
                BlindIndex::recaps(&conn),
                &plan,
                &hmac,
                true,
                allowed.as_ref(),
                VERIFY_CAP,
                &mut telemetry,
                &mut watch,
                None,
            )?
            .0
        } else {
            let ids = if let Some(allowed) = &allowed {
                blind_search::newest_ids(allowed, VERIFY_CAP)
            } else {
                let mut stmt = conn.prepare("SELECT id FROM recap_search_documents ORDER BY start_ms DESC,id DESC LIMIT ?1").map_err(|e| e.to_string())?;
                let ids = stmt
                    .query_map([VERIFY_CAP as i64], |r| r.get(0))
                    .map_err(|e| e.to_string())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| e.to_string())?;
                ids
            };
            ids.into_iter()
                .map(|row_id| blind_search::Candidate {
                    row_id,
                    tier: Tier::Scan,
                    hits: 0,
                    total: 0,
                })
                .collect()
        };
        candidates.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| b.hits.cmp(&a.hits))
                .then_with(|| b.row_id.cmp(&a.row_id))
        });
        blind_search::reserve_for_fuzzy(&mut candidates, VERIFY_CAP);
        let complete = candidates.len() < VERIFY_CAP;
        candidates.truncate(VERIFY_CAP);
        let mut encrypted = Vec::with_capacity(candidates.len());
        for batch in candidates.chunks(blind_search::SQL_CHUNK) {
            let parameters = (2..2 + batch.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT k.id,k.data,k.row_key FROM recap_search_documents k
                JOIN recap_search_dates v ON v.day=k.day AND v.revision=k.revision AND v.privacy_hash=k.privacy_hash
                WHERE k.privacy_hash=?1 AND k.id IN ({parameters})");
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let mut values: Vec<rusqlite::types::Value> = vec![privacy.to_owned().into()];
            values.extend(batch.iter().map(|candidate| candidate.row_id.into()));
            let candidates: HashMap<_, _> = batch
                .iter()
                .map(|candidate| (candidate.row_id, *candidate))
                .collect();
            for row in stmt
                .query_map(rusqlite::params_from_iter(values), |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?
            {
                let (id, data, key) = row.map_err(|e| e.to_string())?;
                encrypted.push((candidates[&id], data, key));
            }
        }
        drop(conn);
        let values = super::screenshot::unwrap_batch_parallel(
            &self.credential_state,
            encrypted,
            |session, (candidate, data, key)| {
                let mut bytes =
                    Self::decrypt_payload_with_unwrap(&data, &key, &|c| session.unwrap_row_key(c))?;
                let value = serde_json::from_slice(&bytes)
                    .map_err(|_| BackgroundReadError::Other("RECAP_INVALID_CACHE".into()));
                Self::zeroize_bytes(&mut bytes);
                value.map(|value| RecapKeywordCandidate {
                    value,
                    hits: candidate.hits,
                    groups: candidate.total,
                })
            },
        )
        .map_err(|e| match e {
            BackgroundReadError::AuthRequired => "AUTH_REQUIRED".into(),
            BackgroundReadError::Other(e) => e,
        })?;
        Ok((values, complete))
    }
}

fn process_hash(process: &str, hmac: &[u8]) -> String {
    StorageState::compute_hmac_hash(
        &format!("recap-process:{}", process.trim().to_lowercase()),
        hmac,
    )
}

pub(super) fn remove_day(conn: &Connection, day: &str) -> Result<(), String> {
    conn.execute("DELETE FROM recap_search_documents WHERE day=?1", [day])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM recap_search_dates WHERE day=?1", [day])
        .map_err(|e| e.to_string())?;
    drain_removed_tokens(conn)
}

fn drain_removed_tokens(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT token_hash,document_id FROM recap_search_removed_tokens")
        .map_err(|e| e.to_string())?;
    let mut removed: HashMap<String, RoaringBitmap> = HashMap::new();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))
        .map_err(|e| e.to_string())?
    {
        let (hash, id) = row.map_err(|e| e.to_string())?;
        removed.entry(hash).or_default().insert(id);
    }
    drop(stmt);
    let index = BlindIndex::recaps(conn);
    let mut stats = MutationStats::default();
    for (hash, ids) in removed {
        index.remove(&hash, &ids, &mut stats)?;
    }
    conn.execute("DELETE FROM recap_search_removed_tokens", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}
