//! Compact usage intervals. Private catalog payloads are encrypted before the
//! short write transaction; day reads never decrypt screenshot/OCR content.
use super::StorageState;
use crate::{
    calendar::{self, CalendarDay},
    usage::{aggregate, types::*},
};
use chrono::{Datelike, Days, TimeZone};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

mod calendars;
pub(crate) mod categories;
mod legacy;

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    // Keep the legacy ciphertext and its row keys together. Renaming and
    // creating the replacement must either both commit or both roll back.
    legacy::preserve_schema(&tx)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS usage_apps (
        id INTEGER PRIMARY KEY, token TEXT NOT NULL UNIQUE, info_enc BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS usage_contexts (
        id INTEGER PRIMARY KEY, token TEXT NOT NULL UNIQUE, app_id INTEGER NOT NULL REFERENCES usage_apps(id),
        category TEXT NOT NULL, info_enc BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS usage_days (
        id INTEGER PRIMARY KEY, date TEXT NOT NULL UNIQUE, calendar TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS usage_intervals (
        day_id INTEGER NOT NULL REFERENCES usage_days(id), run_id INTEGER NOT NULL, sequence INTEGER NOT NULL,
        start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 3),
        context_id INTEGER REFERENCES usage_contexts(id), PRIMARY KEY(day_id,run_id,sequence),
        CHECK(end_ms>start_ms), CHECK(state<>0 OR context_id IS NOT NULL)) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS idx_usage_day_time ON usage_intervals(day_id,start_ms);")
        .map_err(|e| e.to_string())?;
    categories::initialize(&tx)?;
    tx.commit().map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
struct ContextInfo {
    title: String,
    url: Option<String>,
}

#[derive(Clone)]
struct AppEntry {
    identity: Arc<UsageAppIdentity>,
    token: String,
    encrypted: Vec<u8>,
    id: Option<i64>,
}
#[derive(Clone)]
struct ContextEntry {
    identity: Arc<UsageIdentity>,
    token: String,
    app: Arc<AppEntry>,
    encrypted: Vec<u8>,
    id: Option<i64>,
}

/// Owned by the single native recorder thread. IDs only survive one database
/// generation; bounded FIFO catalogs also bound retained private metadata.
pub(crate) struct UsageWriter {
    generation: u64,
    zone: Tz,
    apps: VecDeque<Arc<AppEntry>>,
    contexts: VecDeque<ContextEntry>,
    written_ends: HashMap<(i64, i64), i64>,
}

impl UsageWriter {
    pub fn new(generation: u64, zone: Tz) -> Self {
        Self {
            generation,
            zone,
            apps: VecDeque::new(),
            contexts: VecDeque::new(),
            written_ends: HashMap::new(),
        }
    }

    pub fn change_time_zone(
        &mut self,
        storage: &StorageState,
        zone: Tz,
        at_ms: i64,
    ) -> Result<(), String> {
        if self.zone == zone {
            return Ok(());
        }
        let guard = storage
            .try_get_connection_named("usage.time_zone")?
            .ok_or("USAGE_WRITE_BUSY")?;
        if self.generation != storage.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        if crate::maintenance::is_active() || storage.is_database_maintenance_pending() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        calendars::change_zone(&tx, self.zone, zone, at_ms)?;
        tx.commit().map_err(|e| e.to_string())?;
        self.zone = zone;
        Ok(())
    }

    fn prepare(
        &mut self,
        storage: &StorageState,
        identity: &Arc<UsageIdentity>,
    ) -> Result<ContextEntry, String> {
        if let Some(entry) = self.contexts.iter().find(|e| e.identity == *identity) {
            return Ok(entry.clone());
        }
        let app = match self.apps.iter().find(|e| e.identity == identity.app) {
            Some(entry) => entry.clone(),
            None => {
                let lookup =
                    serde_json::to_vec(&(&identity.app.process_name, &identity.app.process_path))
                        .map_err(|e| e.to_string())?;
                let token = storage.protected.usage_token("app", &lookup)?;
                let bytes = serde_json::to_vec(identity.app.as_ref()).map_err(|e| e.to_string())?;
                let encrypted =
                    storage
                        .protected
                        .seal("metadata", &format!("usage_app:{token}"), &bytes)?;
                let entry = Arc::new(AppEntry {
                    identity: identity.app.clone(),
                    token,
                    encrypted,
                    id: None,
                });
                self.apps.push_back(entry.clone());
                if self.apps.len() > 64 {
                    self.apps.pop_front();
                }
                entry
            }
        };
        let info = ContextInfo {
            title: identity.title.clone(),
            url: identity.url.clone(),
        };
        let bytes = serde_json::to_vec(&info).map_err(|e| e.to_string())?;
        let lookup = serde_json::to_vec(&(&app.token, &identity.category, &info.title, &info.url))
            .map_err(|e| e.to_string())?;
        let token = storage.protected.usage_token("context", &lookup)?;
        let encrypted =
            storage
                .protected
                .seal("metadata", &format!("usage_context:{token}"), &bytes)?;
        let entry = ContextEntry {
            identity: identity.clone(),
            token,
            app,
            encrypted,
            id: None,
        };
        self.contexts.push_back(entry.clone());
        if self.contexts.len() > 512 {
            self.contexts.pop_front();
        }
        Ok(entry)
    }

    pub fn write(&mut self, storage: &StorageState, spans: &[UsageSpan]) -> Result<(), String> {
        if self.generation != storage.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        if crate::maintenance::is_active() || storage.is_database_maintenance_pending() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let spans: Vec<_> = spans
            .iter()
            .filter(|s| {
                s.end_ms > s.start_ms
                    && s.end_ms
                        > self
                            .written_ends
                            .get(&(s.run_id, s.sequence))
                            .copied()
                            .unwrap_or(s.start_ms)
            })
            .collect();
        if spans.is_empty() {
            return Ok(());
        }
        let mut prepared = HashMap::new();
        let mut span_contexts = HashMap::new();
        for span in &spans {
            if let Some(identity) = span
                .identity
                .as_ref()
                .filter(|_| span.state == UsageState::Active)
            {
                let entry = self.prepare(storage, identity)?;
                span_contexts.insert((span.run_id, span.sequence), entry.token.clone());
                prepared.insert(entry.token.clone(), entry);
            }
        }
        // Do not queue behind large archive operations on the sampling thread.
        let guard = storage
            .try_get_connection_named("usage.checkpoint")?
            .ok_or("USAGE_WRITE_BUSY")?;
        if self.generation != storage.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        if crate::maintenance::is_active() || storage.is_database_maintenance_pending() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut app_ids = HashMap::new();
        let mut context_ids = HashMap::new();
        for entry in prepared.values() {
            if let Some(id) = entry.id {
                context_ids.insert(entry.token.clone(), id);
                continue;
            }
            let app = &entry.app;
            let app_id = match app.id.or_else(|| app_ids.get(&app.token).copied()) {
                Some(id) => id,
                None => {
                    // Refresh a re-observed icon once per cached app identity.
                    // A failed extraction must not erase the stored icon.
                    tx.execute(
                        "INSERT INTO usage_apps(token,info_enc) VALUES(?1,?2)
                        ON CONFLICT(token) DO UPDATE SET info_enc=excluded.info_enc WHERE ?3",
                        params![app.token, app.encrypted, app.identity.icon.is_some()],
                    )
                    .map_err(|e| e.to_string())?;
                    tx.query_row(
                        "SELECT id FROM usage_apps WHERE token=?1",
                        [&app.token],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(|e| e.to_string())?
                }
            };
            app_ids.insert(app.token.clone(), app_id);
            tx.execute("INSERT OR IGNORE INTO usage_contexts(token,app_id,category,info_enc) VALUES(?1,?2,?3,?4)",params![entry.token,app_id,entry.identity.category,entry.encrypted]).map_err(|e|e.to_string())?;
            let id = tx
                .query_row(
                    "SELECT id FROM usage_contexts WHERE token=?1",
                    [&entry.token],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())?;
            context_ids.insert(entry.token.clone(), id);
        }
        let mut days: HashMap<String, (i64, CalendarDay)> = HashMap::new();
        for span in &spans {
            let context_id = span_contexts
                .get(&(span.run_id, span.sequence))
                .and_then(|t| context_ids.get(t))
                .copied();
            let state = if span.state == UsageState::Active && context_id.is_none() {
                UsageState::Unknown
            } else {
                span.state
            };
            let mut start = span.start_ms.max(
                self.written_ends
                    .get(&(span.run_id, span.sequence))
                    .copied()
                    .unwrap_or(span.start_ms),
            );
            // A span is checkpointed at most every 30s, but its start can be on
            // yesterday. The day key keeps repeated midnight splits idempotent.
            while start < span.end_ms {
                let date = self
                    .zone
                    .timestamp_millis_opt(start)
                    .single()
                    .ok_or("CALENDAR_INVALID_TIMESTAMP")?
                    .date_naive()
                    .to_string();
                if !days.contains_key(&date) {
                    days.insert(
                        date.clone(),
                        calendars::for_write(&tx, &date, self.zone, start)?,
                    );
                }
                let (day_id, cal) = &days[&date];
                let local_end = CalendarDay::in_zone(&date, self.zone)?.end_ms;
                let end = span.end_ms.min(local_end);
                let clipped_start = start.max(cal.start_ms);
                let clipped_end = end.min(cal.end_ms);
                if clipped_end > clipped_start {
                    tx.execute("INSERT INTO usage_intervals(day_id,run_id,sequence,start_ms,end_ms,state,context_id)
                        VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(day_id,run_id,sequence) DO UPDATE SET
                        start_ms=MIN(start_ms,excluded.start_ms), end_ms=MAX(end_ms,excluded.end_ms)",
                        params![day_id,span.run_id,span.sequence,clipped_start,clipped_end,state.code(),context_id]).map_err(|e|e.to_string())?;
                }
                start = end;
            }
        }
        let first = spans.iter().map(|s| s.start_ms).min().unwrap();
        tx.execute("INSERT INTO app_metadata(key,value) VALUES('usage_recorded_since_ms',?1)
            ON CONFLICT(key) DO UPDATE SET value=CAST(MIN(CAST(value AS INTEGER),CAST(excluded.value AS INTEGER)) AS TEXT)",[first.to_string()]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        self.written_ends = spans
            .iter()
            .map(|s| ((s.run_id, s.sequence), s.end_ms))
            .collect();
        for entry in &mut self.apps {
            if let Some(id) = app_ids.get(&entry.token) {
                if entry.id != Some(*id) {
                    Arc::make_mut(entry).id = Some(*id);
                }
            }
        }
        for entry in &mut self.contexts {
            if let Some(id) = context_ids.get(&entry.token) {
                entry.id = Some(*id);
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct RowSpan {
    day: String,
    run: i64,
    sequence: i64,
    start: i64,
    end: i64,
    state: UsageState,
    context: Option<i64>,
    app: Option<i64>,
    category: Option<String>,
}

fn calendar_on_conn(conn: &Connection, date: &str, zone: Tz) -> Result<CalendarDay, String> {
    match calendars::read(conn, date)? {
        Some((_, saved)) => Ok(saved),
        None => CalendarDay::in_zone(date, zone),
    }
}
fn rows_on_conn(conn: &Connection, first: &str, last: &str) -> Result<Vec<RowSpan>, String> {
    let mut stmt=conn.prepare("SELECT d.date,i.run_id,i.sequence,i.start_ms,i.end_ms,i.state,i.context_id,c.app_id,c.category
        FROM usage_days d JOIN usage_intervals i ON i.day_id=d.id LEFT JOIN usage_contexts c ON c.id=i.context_id
        WHERE d.date>=?1 AND d.date<=?2 ORDER BY d.date,i.start_ms").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(params![first, last], |r| {
            Ok(RowSpan {
                day: r.get(0)?,
                run: r.get(1)?,
                sequence: r.get(2)?,
                start: r.get(3)?,
                end: r.get(4)?,
                state: UsageState::from_code(r.get(5)?),
                context: r.get(6)?,
                app: r.get(7)?,
                category: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string());
    rows
}
fn span(row: &RowSpan, app: Option<Arc<UsageAppIdentity>>) -> UsageSpan {
    UsageSpan {
        run_id: row.run,
        sequence: row.sequence,
        start_ms: row.start,
        end_ms: row.end,
        state: row.state,
        identity: app.map(|app| {
            Arc::new(UsageIdentity {
                app,
                title: String::new(),
                url: None,
                category: row.category.clone().unwrap_or_else(|| "未分类".into()),
            })
        }),
    }
}

impl StorageState {
    fn read_usage_apps(
        &self,
        rows: &[RowSpan],
    ) -> Result<HashMap<i64, Arc<UsageAppIdentity>>, String> {
        let ids: HashSet<_> = rows.iter().filter_map(|r| r.app).collect();
        let payloads = self.with_read_connection_named("usage.apps", |conn| {
            let mut stmt = conn
                .prepare("SELECT token,info_enc FROM usage_apps WHERE id=?1")
                .map_err(|e| e.to_string())?;
            ids.iter()
                .map(|id| {
                    let (token, bytes) = stmt
                        .query_row([id], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                        })
                        .map_err(|e| e.to_string())?;
                    Ok((*id, token, bytes))
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        payloads
            .into_iter()
            .map(|(id, token, bytes)| {
                let bytes = self
                    .protected
                    .open_usage(&format!("usage_app:{token}"), &bytes)?;
                Ok((
                    id,
                    Arc::new(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?),
                ))
            })
            .collect()
    }

    pub(crate) fn usage_recorded_dates(
        &self,
        first_date: &str,
        last_date: &str,
        live: LiveUsage,
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        let first = calendar::parse_date(first_date)?;
        let last = calendar::parse_date(last_date)?;
        let offset = (last - first).num_days();
        if !(0..42).contains(&offset) {
            return Err("USAGE_INVALID_DATE".into());
        }
        let zone = calendar::time_zone(live.time_zone.as_deref())?;
        let live_spans = if live.generation == self.db_generation() {
            live.spans.as_slice()
        } else {
            &[]
        };

        self.with_read_connection_named("usage.recorded_dates", |conn| {
            // Use the day/time index and stop at the first observation. No app
            // catalog, context payloads, or per-day aggregates are needed.
            let mut saved = conn
                .prepare(
                    "SELECT EXISTS(SELECT 1 FROM usage_intervals
                 WHERE day_id=?1 AND start_ms<?2 AND end_ms>?3 AND state<>?4)",
                )
                .map_err(|e| e.to_string())?;
            let mut legacy = legacy::observation_query(conn)?;
            let mut dates = Vec::new();
            for date in first.iter_days().take(offset as usize + 1) {
                let date = date.to_string();
                let (id, mut day) = match calendars::read(conn, &date)? {
                    Some((id, day)) => (Some(id), day),
                    None if live_spans.is_empty() && legacy.is_none() => continue,
                    None => (None, CalendarDay::in_zone(&date, zone)?),
                };
                calendars::include_live(&mut day, live_spans, zone)?;
                let end = now_ms.min(day.end_ms);
                if end <= day.start_ms {
                    continue;
                }
                if let Some(id) = id {
                    let observed: bool = saved
                        .query_row(
                            params![id, end, day.start_ms, UsageState::Unknown.code()],
                            |row| row.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    if observed {
                        dates.push(date);
                        continue;
                    }
                }
                if !live_spans.is_empty()
                    && calendars::live_on_day(live_spans, &date, zone)?
                        .iter()
                        .any(|span| {
                            span.state != UsageState::Unknown
                                && span.start_ms.max(day.start_ms) < span.end_ms.min(end)
                        })
                {
                    dates.push(date);
                    continue;
                }
                if let Some(query) = &mut legacy {
                    let observed: bool = query
                        .query_row([day.start_ms, end], |row| row.get(0))
                        .map_err(|e| e.to_string())?;
                    if observed {
                        dates.push(date);
                    }
                }
            }
            Ok(dates)
        })
    }

    pub(crate) fn usage_day(
        &self,
        date: &str,
        live: LiveUsage,
        now_ms: i64,
    ) -> Result<UsageDay, String> {
        let generation = self.db_generation();
        let selected = calendar::parse_date(date)?;
        let selected_offset = selected.weekday().num_days_from_monday();
        let monday = selected
            .checked_sub_days(Days::new(u64::from(selected_offset)))
            .ok_or("USAGE_INVALID_DATE")?;
        let sunday = monday
            .checked_add_days(Days::new(6))
            .ok_or("USAGE_INVALID_DATE")?;
        let zone = calendar::time_zone(live.time_zone.as_deref())?;
        let live_spans = if live.generation == generation {
            live.spans
        } else {
            vec![]
        };
        let (rows, legacy_rows, mut calendars, since) =
            self.with_read_connection_named("usage.day", |conn| {
                let rows = rows_on_conn(conn, &monday.to_string(), &sunday.to_string())?;
                let mut calendars = Vec::with_capacity(7);
                for offset in 0..=6 {
                    let date = monday
                        .checked_add_days(Days::new(offset))
                        .ok_or("USAGE_INVALID_DATE")?
                        .to_string();
                    let mut calendar = calendar_on_conn(conn, &date, zone)?;
                    calendars::include_live(&mut calendar, &live_spans, zone)?;
                    calendars.push(calendar);
                }
                let since: Option<String> = conn
                    .query_row(
                        "SELECT value FROM app_metadata WHERE key='usage_recorded_since_ms'",
                        [],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                let legacy_rows = legacy::rows(
                    conn,
                    calendars.iter().map(|cal| cal.start_ms).min().unwrap(),
                    calendars.iter().map(|cal| cal.end_ms).max().unwrap(),
                )?;
                let since = since
                    .and_then(|s| s.parse::<i64>().ok())
                    .into_iter()
                    .chain(legacy::recorded_since(conn)?)
                    .min();
                Ok((rows, legacy_rows, calendars, since))
            })?;
        let selected_rows: Vec<_> = rows.iter().filter(|r| r.day == date).cloned().collect();
        let apps = self.read_usage_apps(&selected_rows)?;
        let selected_cal = &calendars[selected_offset as usize];
        let selected_legacy: Vec<_> = legacy_rows
            .iter()
            .filter(|row| {
                row.state == UsageState::Active
                    && row.end_ms > selected_cal.start_ms
                    && row.start_ms < now_ms.min(selected_cal.end_ms)
            })
            .collect();
        let legacy_identities = legacy::identities(self, &selected_legacy, generation)?;
        let mut week = Vec::with_capacity(calendars.len());
        for cal in &calendars {
            let mut observations: Vec<_> = rows
                .iter()
                .filter(|r| r.day == cal.date)
                .map(|r| span(r, None))
                .collect();
            observations.extend(legacy_rows.iter().map(|row| row.span(None)));
            observations.extend(calendars::live_on_day(&live_spans, &cal.date, zone)?);
            let observations = aggregate::normalized(
                observations,
                cal.start_ms,
                now_ms.min(cal.end_ms).max(cal.start_ms),
            );
            let total_ms = observations
                .iter()
                .filter(|s| s.state == UsageState::Active)
                .map(|s| s.end_ms - s.start_ms)
                .sum();
            let observed_ms = observations
                .iter()
                .filter(|s| s.state != UsageState::Unknown)
                .map(|s| s.end_ms - s.start_ms)
                .sum();
            week.push(UsageWeekDay {
                date: cal.date.clone(),
                total_ms,
                observed_ms,
            });
        }
        let mut observations: Vec<_> = selected_rows
            .iter()
            .map(|r| span(r, r.app.and_then(|id| apps.get(&id).cloned())))
            .collect();
        observations.extend(
            legacy_rows
                .iter()
                .map(|row| row.span(legacy_identities.get(&row.id).cloned())),
        );
        let live_since = live_spans
            .iter()
            .filter(|s| s.end_ms > s.start_ms)
            .map(|s| s.start_ms)
            .min();
        observations.extend(calendars::live_on_day(&live_spans, date, zone)?);
        let since = since.into_iter().chain(live_since).min();
        if self.db_generation() != generation {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        let manual = self.apply_usage_categories(generation, &mut observations)?;
        let selected_cal = calendars.swap_remove(selected_offset as usize);
        let mut day = aggregate::summarize(selected_cal, observations, now_ms, since, week);
        day.generation = generation;
        for app in &mut day.apps {
            app.category_is_manual = manual.contains_key(&app.id);
        }
        Ok(day)
    }

    pub(crate) fn usage_app_details(
        &self,
        date: &str,
        id: &str,
        live: LiveUsage,
        now_ms: i64,
    ) -> Result<UsageAppDetails, String> {
        calendar::parse_date(date)?;
        if id.is_empty() || id.len() > 260 {
            return Err("USAGE_INVALID_APP".into());
        }
        let generation = self.db_generation();
        let zone = calendar::time_zone(live.time_zone.as_deref())?;
        let (cal, rows, legacy_rows) =
            self.with_read_connection_named("usage.details", |conn| {
                let mut cal = calendar_on_conn(conn, date, zone)?;
                if live.generation == generation {
                    calendars::include_live(&mut cal, &live.spans, zone)?;
                }
                let rows = rows_on_conn(conn, date, date)?;
                let legacy_rows = legacy::rows(conn, cal.start_ms, cal.end_ms)?;
                Ok((cal, rows, legacy_rows))
            })?;
        let selected_legacy: Vec<_> = legacy_rows
            .iter()
            .filter(|row| {
                row.state == UsageState::Active
                    && row.process_name.to_lowercase() == id
                    && row.start_ms < now_ms.min(cal.end_ms)
            })
            .collect();
        let legacy_identities = legacy::identities(self, &selected_legacy, generation)?;
        let apps = self.read_usage_apps(&rows)?;
        let context_ids: HashSet<_> = rows
            .iter()
            .filter(|r| {
                r.app
                    .and_then(|id| apps.get(&id))
                    .is_some_and(|app| aggregate::app_id(app) == id)
            })
            .filter_map(|r| r.context)
            .collect();
        let payloads = self.with_read_connection_named("usage.contexts", |conn| {
            let mut stmt = conn
                .prepare("SELECT token,info_enc FROM usage_contexts WHERE id=?1")
                .map_err(|e| e.to_string())?;
            context_ids
                .iter()
                .map(|id| {
                    let (token, bytes) = stmt
                        .query_row([id], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                        })
                        .map_err(|e| e.to_string())?;
                    Ok((*id, token, bytes))
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        let mut contexts: HashMap<i64, ContextInfo> = HashMap::new();
        for (id, token, bytes) in payloads {
            let bytes = self
                .protected
                .open_usage(&format!("usage_context:{token}"), &bytes)?;
            contexts.insert(
                id,
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            );
        }
        let mut observations = vec![];
        for row in rows {
            let mut observed = span(&row, row.app.and_then(|id| apps.get(&id).cloned()));
            if let (Some(identity), Some(context)) = (
                observed.identity.as_mut(),
                row.context.and_then(|id| contexts.get(&id)),
            ) {
                let identity = Arc::make_mut(identity);
                identity.title = context.title.clone();
                identity.url = context.url.clone();
            }
            observations.push(observed);
        }
        observations.extend(
            legacy_rows
                .iter()
                .map(|row| row.span(legacy_identities.get(&row.id).cloned())),
        );
        if live.generation == generation {
            observations.extend(calendars::live_on_day(&live.spans, date, zone)?);
        }
        let observations = aggregate::normalized(
            observations,
            cal.start_ms,
            now_ms.min(cal.end_ms).max(cal.start_ms),
        );
        let mut titles = HashMap::new();
        let mut websites = HashMap::new();
        for span in observations.into_iter().filter(|s| {
            s.state == UsageState::Active
                && s.identity
                    .as_ref()
                    .is_some_and(|i| aggregate::app_id(&i.app) == id)
        }) {
            if let Some(identity) = span.identity {
                add_detail(&mut titles, &identity.title, span.start_ms, span.end_ms);
                if let Some(host) = identity
                    .url
                    .as_ref()
                    .and_then(|url| url::Url::parse(url).ok())
                    .and_then(|u| u.host_str().map(str::to_owned))
                {
                    add_detail(&mut websites, &host, span.start_ms, span.end_ms);
                }
            }
        }
        if generation != self.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        Ok(UsageAppDetails {
            date: date.into(),
            app_id: id.into(),
            titles: sorted_details(titles),
            websites: sorted_details(websites),
        })
    }
}
fn add_detail(details: &mut HashMap<String, UsageDetail>, label: &str, start: i64, end: i64) {
    if label.is_empty() {
        return;
    }
    let detail = details.entry(label.into()).or_insert_with(|| UsageDetail {
        label: label.into(),
        total_ms: 0,
        first_ms: start,
        last_ms: end,
    });
    detail.total_ms += end - start;
    detail.first_ms = detail.first_ms.min(start);
    detail.last_ms = detail.last_ms.max(end);
}
fn sorted_details(details: HashMap<String, UsageDetail>) -> Vec<UsageDetail> {
    let mut list: Vec<_> = details.into_values().collect();
    list.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.label.cmp(&b.label))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshed_icons_replace_cached_payloads_without_changing_usage_or_erasing_icons_on_failure()
    {
        let storage = super::super::test_storage_with_schema(std::path::PathBuf::from(
            "usage-icon-refresh-fixture",
        ));
        let generation = storage.db_generation();
        let calendar = CalendarDay::in_zone("2026-11-24", chrono_tz::UTC).unwrap();
        let identity = |icon: Option<&str>| {
            Arc::new(UsageIdentity {
                app: Arc::new(UsageAppIdentity {
                    process_name: "msedge.exe".into(),
                    name: "Microsoft Edge".into(),
                    process_path: "D:/Apps/msedge.exe".into(),
                    icon: icon.map(str::to_owned),
                }),
                title: "Existing title".into(),
                url: None,
                category: "网页浏览".into(),
            })
        };
        let span = |run_id, start_ms, end_ms, icon| UsageSpan {
            run_id,
            sequence: 0,
            start_ms,
            end_ms,
            state: UsageState::Active,
            identity: Some(identity(icon)),
        };
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            ..Default::default()
        };
        let original = span(
            1,
            calendar.start_ms,
            calendar.start_ms + 60_000,
            Some("old-icon"),
        );
        UsageWriter::new(generation, chrono_tz::UTC)
            .write(&storage, &[original])
            .unwrap();
        let mut refreshed = span(
            2,
            calendar.start_ms + 60_000,
            calendar.start_ms + 120_000,
            Some("corrected-icon"),
        );
        let mut writer = UsageWriter::new(generation, chrono_tz::UTC);
        writer.write(&storage, &[refreshed.clone()]).unwrap();
        let payload = || {
            storage
                .with_read_connection_named("usage.icon-test", |conn| {
                    conn.query_row("SELECT info_enc FROM usage_apps", [], |row| {
                        row.get::<_, Vec<u8>>(0)
                    })
                    .map_err(|error| error.to_string())
                })
                .unwrap()
        };
        let corrected_payload = payload();
        assert!(carbonpaper_app_bound::payload::is_sealed(
            &corrected_payload
        ));
        refreshed.end_ms += 60_000;
        writer.write(&storage, &[refreshed]).unwrap();
        assert_eq!(
            payload(),
            corrected_payload,
            "checkpoints reuse the cached app payload"
        );

        let failed = span(
            3,
            calendar.start_ms + 180_000,
            calendar.start_ms + 240_000,
            None,
        );
        UsageWriter::new(generation, chrono_tz::UTC)
            .write(&storage, &[failed])
            .unwrap();
        assert_eq!(
            payload(),
            corrected_payload,
            "failed extraction preserves the existing icon"
        );
        let day = storage
            .usage_day("2026-11-24", live.clone(), calendar.end_ms)
            .unwrap();
        assert_eq!(day.total_ms, 240_000);
        assert_eq!(day.apps.len(), 1);
        assert_eq!(day.apps[0].icon.as_deref(), Some("corrected-icon"));
        assert_eq!(day.apps[0].category, "网页浏览");
        let details = storage
            .usage_app_details("2026-11-24", "msedge.exe", live, calendar.end_ms)
            .unwrap();
        assert_eq!(details.titles[0].label, "Existing title");
        assert_eq!(details.titles[0].total_ms, 240_000);
        storage
            .with_read_connection_named("usage.icon-catalog-test", |conn| {
                for table in ["usage_apps", "usage_contexts"] {
                    let count = conn
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .map_err(|error| error.to_string())?;
                    assert_eq!(
                        count, 1,
                        "catalog IDs and references survive the icon refresh"
                    );
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn midnight_checkpoints_are_idempotent_and_catalogs_are_protected() {
        let storage =
            super::super::test_storage_with_schema(std::path::PathBuf::from("usage-fixture"));
        let generation = storage.db_generation();
        let day = CalendarDay::in_zone("2026-11-24", chrono_tz::UTC).unwrap();
        let identity = Arc::new(UsageIdentity {
            app: Arc::new(UsageAppIdentity {
                process_name: "endfield.exe".into(),
                name: "Endfield".into(),
                process_path: "D:/Games/endfield.exe".into(),
                icon: None,
            }),
            title: "Secret title".into(),
            url: Some("https://private.example/page".into()),
            category: "游戏".into(),
        });
        let mut writer = UsageWriter::new(generation, chrono_tz::UTC);
        let mut observed = UsageSpan {
            run_id: 1,
            sequence: 0,
            start_ms: day.end_ms - 10_000,
            end_ms: day.end_ms + 10_000,
            state: UsageState::Active,
            identity: Some(identity),
        };
        writer.write(&storage, &[observed.clone()]).unwrap();
        observed.end_ms += 10_000;
        writer.write(&storage, &[observed.clone()]).unwrap();
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            spans: vec![observed.clone()],
        };
        let first = storage
            .usage_day("2026-11-24", live.clone(), observed.end_ms)
            .unwrap();
        let second = storage
            .usage_day("2026-11-25", live.clone(), observed.end_ms)
            .unwrap();
        assert_eq!(first.total_ms, 10_000);
        assert_eq!(second.total_ms, 20_000);
        assert_eq!(second.apps[0].visits, 1);
        let details = storage
            .usage_app_details("2026-11-25", "endfield.exe", live, observed.end_ms)
            .unwrap();
        assert_eq!(details.titles[0].label, "Secret title");
        assert_eq!(details.websites[0].total_ms, 20_000);
        let guard = storage.get_connection_named("usage.test").unwrap();
        let conn = guard.as_ref().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM usage_intervals", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        for table in ["usage_apps", "usage_contexts"] {
            let bytes = conn
                .query_row(&format!("SELECT info_enc FROM {table}"), [], |r| {
                    r.get::<_, Vec<u8>>(0)
                })
                .unwrap();
            assert!(carbonpaper_app_bound::payload::is_sealed(&bytes));
            assert!(!String::from_utf8_lossy(&bytes).contains("Secret title"));
        }
        storage.bump_db_generation();
        assert_eq!(
            writer.write(&storage, &[observed]).unwrap_err(),
            "USAGE_SOURCE_CHANGED"
        );
    }

    #[test]
    fn weekly_totals_follow_the_selected_monday_through_sunday() {
        let storage = super::super::test_storage_with_schema("usage-week".into());
        let generation = storage.db_generation();
        let zone = chrono_tz::UTC;
        let identity = Arc::new(UsageIdentity {
            app: Arc::new(UsageAppIdentity {
                process_name: "code.exe".into(),
                name: "Visual Studio Code".into(),
                process_path: "D:/code.exe".into(),
                icon: None,
            }),
            title: String::new(),
            url: None,
            category: "编程开发".into(),
        });
        let spans: Vec<_> = [
            ("2026-12-27", 5_000),
            ("2026-12-30", 10_000),
            ("2027-01-02", 20_000),
            ("2027-01-04", 40_000),
        ]
        .into_iter()
        .enumerate()
        .map(|(sequence, (date, duration))| {
            let start = CalendarDay::in_zone(date, zone).unwrap().start_ms;
            UsageSpan {
                run_id: 1,
                sequence: sequence as i64,
                start_ms: start,
                end_ms: start + duration,
                state: UsageState::Active,
                identity: Some(identity.clone()),
            }
        })
        .collect();
        UsageWriter::new(generation, zone)
            .write(&storage, &spans)
            .unwrap();
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            spans: vec![],
        };
        let now = spans.last().unwrap().end_ms;
        let wednesday = storage.usage_day("2026-12-30", live.clone(), now).unwrap();
        assert_eq!(wednesday.calendar.date, "2026-12-30");
        assert_eq!(wednesday.total_ms, 10_000);
        assert_eq!(
            wednesday
                .week
                .iter()
                .map(|day| (day.date.as_str(), day.total_ms))
                .collect::<Vec<_>>(),
            [
                ("2026-12-28", 0),
                ("2026-12-29", 0),
                ("2026-12-30", 10_000),
                ("2026-12-31", 0),
                ("2027-01-01", 0),
                ("2027-01-02", 20_000),
                ("2027-01-03", 0),
            ]
        );
        let monday = storage.usage_day("2027-01-04", live, now).unwrap();
        assert_eq!(monday.week.first().unwrap().date, "2027-01-04");
        assert_eq!(monday.week.first().unwrap().total_ms, 40_000);
        assert_eq!(monday.week.last().unwrap().date, "2027-01-10");
    }

    #[test]
    fn details_and_overview_apply_the_same_overlap_and_archive_generation_rules() {
        let storage = super::super::test_storage_with_schema(std::path::PathBuf::from(
            "usage-overlap-fixture",
        ));
        let generation = storage.db_generation();
        let calendar = CalendarDay::in_zone("2026-11-24", chrono_tz::UTC).unwrap();
        let start = calendar.start_ms;
        let identity = |process: &str, title: &str| {
            Arc::new(UsageIdentity {
                app: Arc::new(UsageAppIdentity {
                    process_name: process.into(),
                    name: process.into(),
                    process_path: process.into(),
                    icon: None,
                }),
                title: title.into(),
                url: None,
                category: "未分类".into(),
            })
        };
        let spans = vec![
            UsageSpan {
                run_id: 1,
                sequence: 0,
                start_ms: start,
                end_ms: start + 60_000,
                state: UsageState::Active,
                identity: Some(identity("a.exe", "a")),
            },
            UsageSpan {
                run_id: 2,
                sequence: 0,
                start_ms: start + 30_000,
                end_ms: start + 90_000,
                state: UsageState::Active,
                identity: Some(identity("b.exe", "b")),
            },
        ];
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            spans: spans.clone(),
        };
        let day = storage
            .usage_day("2026-11-24", live.clone(), start + 120_000)
            .unwrap();
        let detail = storage
            .usage_app_details("2026-11-24", "b.exe", live.clone(), start + 120_000)
            .unwrap();
        assert_eq!(day.total_ms, 90_000);
        assert_eq!(
            day.apps
                .iter()
                .find(|app| app.id == "b.exe")
                .unwrap()
                .total_ms,
            30_000
        );
        assert_eq!(detail.titles[0].total_ms, 30_000);
        let stale = LiveUsage {
            generation: generation + 1,
            ..live
        };
        assert_eq!(
            storage
                .usage_day("2026-11-24", stale, start + 120_000)
                .unwrap()
                .total_ms,
            0
        );
    }
}
