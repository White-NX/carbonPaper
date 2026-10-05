//! Bounded timeline reads: only the labels, icons and timestamps rendered by the UI.
use super::{wire_time, StorageState};
use crate::credential_manager::{
    decrypt_row_key_with_cng_measured, decrypt_with_master_key, CredentialError,
    RowKeyDecryptTimings,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub(crate) const CANCELLED: &str = "TIMELINE_CANCELLED";

#[derive(Debug, Serialize)]
pub struct TimelineRecord {
    pub id: i64,
    pub image_path: String,
    pub timestamp: Option<i64>,
    pub created_at: String,
    pub window_title: Option<String>,
    pub process_name: Option<String>,
    pub process_icon: Option<String>,
    pub process_path: Option<String>,
    pub category: Option<String>,
}

struct RawTimelineRow {
    id: i64,
    image_path: String,
    created_at: String,
    timestamp: Option<i64>,
    window_title: Option<String>,
    process_name: Option<String>,
    metadata: Option<String>,
    title_enc: Option<Vec<u8>>,
    process_enc: Option<Vec<u8>>,
    metadata_enc: Option<Vec<u8>>,
    key_enc: Option<Vec<u8>>,
    icon_enc: Option<Vec<u8>>,
    icon_id: Option<i64>,
    shared_icon_enc: Option<Vec<u8>>,
    shared_icon_key: Option<Vec<u8>>,
    category: Option<String>,
}

#[derive(Default)]
struct Timings {
    db_wait: Duration,
    count: Duration,
    sql: Duration,
    hydration: Duration,
    cng: RowKeyDecryptTimings,
    rows: usize,
    processed: usize,
    icon_cache_hits: usize,
}

impl StorageState {
    pub fn get_timeline_records(
        &self,
        start_ts: f64,
        end_ts: f64,
        limit: i64,
        cancelled: &AtomicBool,
    ) -> Result<Vec<TimelineRecord>, String> {
        let started = Instant::now();
        let mut timings = Timings::default();
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(CANCELLED.to_owned())
            } else if !self.credential_state.is_session_valid()
                || self.credential_state.silent_read_auth_required()
            {
                Err("AUTH_REQUIRED".to_owned())
            } else {
                Ok(())
            }
        };
        let result = (|| {
            check()?;
            let raw = {
                let waiting = Instant::now();
                let guard = self.get_connection_named("get_timeline_records")?;
                timings.db_wait = waiting.elapsed();
                check()?;
                read_rows(
                    guard.as_ref().unwrap(),
                    start_ts,
                    end_ts,
                    limit,
                    &check,
                    &mut timings,
                )?
            };
            timings.rows = raw.len();
            // No DB guard, native handle or decrypted key is retained by the
            // request cache. Every row and the final response recheck UI auth.
            let hydrating = Instant::now();
            let result = hydrate(
                raw,
                &check,
                &mut |ciphertext, cng| {
                    let key = match decrypt_row_key_with_cng_measured(
                        &self.credential_state,
                        ciphertext,
                        cng,
                        &|| {
                            cancelled.load(Ordering::Acquire)
                                || !self.credential_state.is_session_valid()
                        },
                    ) {
                        Ok(key) => key.map(Zeroizing::new),
                        Err(CredentialError::AuthRequired) => return Err("AUTH_REQUIRED".into()),
                        // Match the full screenshot reader's legacy fallback
                        // for an unreadable row, while never swallowing auth.
                        Err(_) => None,
                    };
                    check()?;
                    Ok(key)
                },
                &mut timings,
            );
            timings.hydration = hydrating.elapsed();
            result
        })();
        let total = started.elapsed();
        let assembly = timings
            .hydration
            .saturating_sub(timings.cng.lock_wait + timings.cng.decrypt);
        // Aggregate values only: no row ids, titles, links, ciphertext or keys.
        let status = match &result {
            Ok(_) => "complete",
            Err(error) if error == CANCELLED => "cancelled",
            Err(_) => "error",
        };
        macro_rules! report {
            ($level:ident) => {
                tracing::$level!(status, rows = timings.rows, processed = timings.processed,
                    cng_calls = timings.cng.calls, icon_cache_hits = timings.icon_cache_hits,
                    db_wait = ?timings.db_wait, count = ?timings.count, sql = ?timings.sql,
                    cng_wait = ?timings.cng.lock_wait, cng_decrypt = ?timings.cng.decrypt,
                    ?assembly, ?total, "[DIAG:DB] get_timeline_records timing");
            }
        }
        if total >= Duration::from_secs(1) {
            report!(warn);
        } else {
            report!(debug);
        }
        result
    }
}

fn read_rows(
    conn: &Connection,
    start_ts: f64,
    end_ts: f64,
    limit: i64,
    check: &impl Fn() -> Result<(), String>,
    timings: &mut Timings,
) -> Result<Vec<RawTimelineRow>, String> {
    if !start_ts.is_finite() || !end_ts.is_finite() || start_ts > end_ts {
        return Err("Invalid timeline range".into());
    }
    let format = |ts: f64| {
        DateTime::<Utc>::from_timestamp(ts as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .ok_or_else(|| "Invalid timeline timestamp".to_owned())
    };
    let start = format(start_ts)?;
    let end = format(end_ts)?;
    let limit = limit.clamp(1, 500);
    let counting = Instant::now();
    // Only distinguish <= limit from > limit. Stop the covering-index scan
    // after limit + 1 rather than counting an entire year of history.
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM screenshots
         WHERE is_deleted = 0 AND created_at BETWEEN ?1 AND ?2 LIMIT ?3)",
            params![start, end, limit + 1],
            |row| row.get(0),
        )
        .map_err(|e| format!("Timeline count failed: {e}"))?;
    timings.count = counting.elapsed();
    check()?;
    let selection = if total > limit {
        let bucket = StorageState::snap_bucket_seconds((end_ts - start_ts).max(1.0) / limit as f64);
        format!(
            "JOIN (SELECT MIN(id) AS picked_id FROM screenshots
            WHERE is_deleted = 0 AND created_at BETWEEN ?1 AND ?2
            GROUP BY CAST(strftime('%s', created_at) AS INTEGER) / {bucket}) picks
            ON picks.picked_id = s.id"
        )
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT s.id, s.image_path, s.created_at,
        CAST(strftime('%s', s.created_at) AS INTEGER), s.window_title, s.process_name,
        s.metadata, s.window_title_enc, s.process_name_enc, s.metadata_enc,
        s.content_key_encrypted, s.page_icon_enc, s.page_icon_id,
        pi.icon_enc, pi.icon_key_encrypted, s.category
        FROM screenshots s {selection}
        LEFT JOIN page_icons pi ON pi.id = s.page_icon_id
        WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2
        ORDER BY s.created_at ASC, s.id ASC LIMIT ?3"
    );
    let querying = Instant::now();
    let result = (|| {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![start, end, limit], |row| {
                Ok(RawTimelineRow {
                    id: row.get(0)?,
                    image_path: row.get(1)?,
                    created_at: row.get(2)?,
                    timestamp: row.get(3)?,
                    window_title: row.get(4)?,
                    process_name: row.get(5)?,
                    metadata: row.get(6)?,
                    title_enc: row.get(7)?,
                    process_enc: row.get(8)?,
                    metadata_enc: row.get(9)?,
                    key_enc: row.get(10)?,
                    icon_enc: row.get(11)?,
                    icon_id: row.get(12)?,
                    shared_icon_enc: row.get(13)?,
                    shared_icon_key: row.get(14)?,
                    category: row.get(15)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut result = Vec::new();
        for row in rows {
            check()?;
            result.push(row.map_err(|e| e.to_string())?);
        }
        Ok(result)
    })();
    timings.sql = querying.elapsed();
    result
}

fn decrypt_string(
    data: Option<&Vec<u8>>,
    key: Option<&[u8]>,
    plain: Option<String>,
) -> Option<String> {
    match (data, key) {
        (Some(data), Some(key)) => decrypt_with_master_key(key, data)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok()),
        _ => plain,
    }
}

fn hydrate(
    rows: Vec<RawTimelineRow>,
    check: &impl Fn() -> Result<(), String>,
    unwrap: &mut impl FnMut(
        &[u8],
        &mut RowKeyDecryptTimings,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, String>,
    timings: &mut Timings,
) -> Result<Vec<TimelineRecord>, String> {
    let mut icons: HashMap<i64, Option<Zeroizing<String>>> = HashMap::new();
    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        check()?;
        let key = row
            .key_enc
            .as_ref()
            .map(|key| unwrap(key, &mut timings.cng))
            .transpose()?
            .flatten();
        let key = key.as_ref().map(|key| key.as_slice());
        let title = decrypt_string(row.title_enc.as_ref(), key, row.window_title);
        let process = decrypt_string(row.process_enc.as_ref(), key, row.process_name);
        let metadata =
            decrypt_string(row.metadata_enc.as_ref(), key, row.metadata).map(Zeroizing::new);
        let metadata = metadata
            .as_ref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        let mut icon = metadata
            .as_ref()
            .and_then(|meta| meta.get("process_icon"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let process_path = metadata
            .as_ref()
            .and_then(|meta| meta.get("process_path"))
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        if icon.is_none() {
            icon = decrypt_string(row.icon_enc.as_ref(), key, None);
        }
        if icon.is_none() {
            if let Some(id) = row.icon_id {
                check()?;
                if let Some(cached) = icons.get(&id) {
                    timings.icon_cache_hits += 1;
                    icon = cached.as_ref().map(|value| value.to_string());
                } else if let (Some(data), Some(key)) =
                    (row.shared_icon_enc.as_ref(), row.shared_icon_key.as_ref())
                {
                    let key = unwrap(key, &mut timings.cng)?;
                    check()?;
                    icon = decrypt_string(Some(data), key.as_ref().map(|key| key.as_slice()), None);
                    icons.insert(id, icon.clone().map(Zeroizing::new));
                }
            }
        }
        records.push(TimelineRecord {
            id: row.id,
            image_path: row.image_path,
            created_at: wire_time::from_sqlite_utc(&row.created_at),
            timestamp: row.timestamp,
            window_title: title,
            process_name: process,
            process_icon: icon,
            process_path,
            category: row.category,
        });
        timings.processed += 1;
    }
    check()?;
    Ok(records)
}

#[cfg(test)]
mod tests;
