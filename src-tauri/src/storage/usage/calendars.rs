//! Preserve recorded date assignments while changing the calendar for new work.
use crate::{calendar::CalendarDay, usage::types::UsageSpan};
use chrono::{Offset, TimeZone};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};

pub(super) fn read(conn: &Connection, date: &str) -> Result<Option<(i64, CalendarDay)>, String> {
    let row = conn
        .query_row(
            "SELECT id,calendar FROM usage_days WHERE date=?1",
            [date],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    row.map(|(id, json)| Ok((id, serde_json::from_str(&json).map_err(|e| e.to_string())?)))
        .transpose()
}

fn save(conn: &Connection, id: i64, day: &CalendarDay) -> Result<(), String> {
    conn.execute(
        "UPDATE usage_days SET calendar=?2 WHERE id=?1",
        params![id, serde_json::to_string(day).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn insert(conn: &Connection, day: &CalendarDay) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO usage_days(date,calendar) VALUES(?1,?2)",
        params![
            day.date,
            serde_json::to_string(day).map_err(|e| e.to_string())?
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

fn merge(day: &mut CalendarDay, current: &CalendarDay, at_ms: i64) {
    // A date can be revisited when travelling west. Keep every recorded
    // interval on its original date and extend its bounds for the new visit.
    if day.time_zone != current.time_zone {
        day.time_zone = None;
        day.rule = "system_time_zone_changed".into();
    }
    day.end_ms = current.end_ms;
    day.end_utc_offset_seconds = current.end_utc_offset_seconds;
    day.period_boundaries_ms
        .retain(|boundary| *boundary < at_ms && *boundary < day.end_ms);
    day.period_boundaries_ms.extend(
        current
            .period_boundaries_ms
            .iter()
            .copied()
            .filter(|boundary| *boundary >= at_ms && *boundary > day.start_ms),
    );
    day.period_boundaries_ms.extend([day.start_ms, day.end_ms]);
    day.period_boundaries_ms.sort_unstable();
    day.period_boundaries_ms.dedup();
}

pub(super) fn for_write(
    conn: &Connection,
    date: &str,
    zone: Tz,
    start_ms: i64,
) -> Result<(i64, CalendarDay), String> {
    let current = CalendarDay::in_zone(date, zone)?;
    match read(conn, date)? {
        Some((id, mut day)) => {
            if current.end_ms > day.end_ms {
                // Reopening a saved date must not clip the new tail at the
                // earlier zone's midnight, including after an app restart.
                merge(&mut day, &current, start_ms);
                save(conn, id, &day)?;
            }
            Ok((id, day))
        }
        None => Ok((insert(conn, &current)?, current)),
    }
}

pub(super) fn change_zone(
    conn: &Connection,
    old_zone: Tz,
    zone: Tz,
    at_ms: i64,
) -> Result<(), String> {
    let old_date = old_zone
        .timestamp_millis_opt(at_ms.saturating_sub(1))
        .single()
        .ok_or("CALENDAR_INVALID_TIMESTAMP")?
        .date_naive()
        .to_string();
    let current = zone
        .timestamp_millis_opt(at_ms)
        .single()
        .ok_or("CALENDAR_INVALID_TIMESTAMP")?;
    let new_date = current.date_naive().to_string();
    let Some((id, mut old_day)) = read(conn, &old_date)? else {
        return Ok(());
    };
    let current_day = CalendarDay::in_zone(&new_date, zone)?;
    if old_date == new_date {
        merge(&mut old_day, &current_day, at_ms);
        return save(conn, id, &old_day);
    }
    // The caller has checkpointed all observations through this instant.
    // Close the old date at the transition, so live time cannot belong to both.
    old_day.end_ms = at_ms;
    old_day.time_zone = None;
    old_day.rule = "system_time_zone_changed".into();
    old_day.end_utc_offset_seconds = old_zone
        .timestamp_millis_opt(at_ms)
        .single()
        .ok_or("CALENDAR_INVALID_TIMESTAMP")?
        .offset()
        .fix()
        .local_minus_utc();
    old_day
        .period_boundaries_ms
        .retain(|boundary| *boundary < at_ms);
    old_day.period_boundaries_ms.push(at_ms);
    save(conn, id, &old_day)?;
    match read(conn, &new_date)? {
        Some((id, mut day)) => {
            merge(&mut day, &current_day, at_ms);
            save(conn, id, &day)
        }
        None => {
            let mut day = current_day;
            day.start_ms = at_ms;
            day.start_utc_offset_seconds = current.offset().fix().local_minus_utc();
            day.period_boundaries_ms
                .retain(|boundary| *boundary > at_ms);
            day.period_boundaries_ms.insert(0, at_ms);
            insert(conn, &day).map(|_| ())
        }
    }
}

pub(super) fn live_on_day(
    spans: &[UsageSpan],
    date: &str,
    zone: Tz,
) -> Result<Vec<UsageSpan>, String> {
    let day = CalendarDay::in_zone(date, zone)?;
    Ok(spans
        .iter()
        .filter_map(|span| {
            let mut span = span.clone();
            span.start_ms = span.start_ms.max(day.start_ms);
            span.end_ms = span.end_ms.min(day.end_ms);
            (span.end_ms > span.start_ms).then_some(span)
        })
        .collect())
}

pub(super) fn include_live(
    day: &mut CalendarDay,
    spans: &[UsageSpan],
    zone: Tz,
) -> Result<(), String> {
    if !spans
        .iter()
        .any(|span| span.end_ms > span.start_ms && span.end_ms > day.end_ms)
    {
        return Ok(());
    }
    let current = CalendarDay::in_zone(&day.date, zone)?;
    if current.end_ms > day.end_ms {
        if let Some(start) = spans
            .iter()
            .filter(|span| {
                span.end_ms > span.start_ms
                    && span.end_ms > day.end_ms
                    && span.end_ms > current.start_ms
                    && span.start_ms < current.end_ms
            })
            .map(|span| span.start_ms.max(current.start_ms))
            .min()
        {
            // The reopened tail is visible before its next checkpoint. This is
            // a query-local calendar; persisted history changes only on writes.
            merge(day, &current, start);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        storage::{StorageState, UsageWriter},
        usage::types::*,
    };
    use std::sync::Arc;

    const HOUR: i64 = 3_600_000;
    fn span(sequence: i64, start_ms: i64, end_ms: i64) -> UsageSpan {
        UsageSpan {
            run_id: 1,
            sequence,
            start_ms,
            end_ms,
            state: UsageState::Active,
            identity: Some(Arc::new(UsageIdentity {
                app: Arc::new(UsageAppIdentity {
                    process_name: "code.exe".into(),
                    name: "Visual Studio Code".into(),
                    process_path: "D:/Apps/code.exe".into(),
                    icon: None,
                }),
                title: "Time-zone transition".into(),
                url: None,
                category: "编程开发".into(),
            })),
        }
    }
    fn live(storage: &StorageState, zone: Tz, spans: Vec<UsageSpan>) -> LiveUsage {
        LiveUsage {
            generation: storage.db_generation(),
            time_zone: Some(zone.name().into()),
            spans,
        }
    }
    fn stored_duration(storage: &StorageState) -> i64 {
        storage
            .with_read_connection_named("usage.time-zone-test", |conn| {
                conn.query_row(
                    "SELECT SUM(end_ms-start_ms) FROM usage_intervals",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .unwrap()
    }

    #[test]
    fn east_and_west_zone_changes_preserve_history_and_checkpoint_every_live_millisecond() {
        for (old_zone, zone) in [
            (chrono_tz::UTC, chrono_tz::Asia::Shanghai),
            (chrono_tz::Asia::Shanghai, chrono_tz::UTC),
        ] {
            let storage = crate::storage::test_storage_with_schema("usage-zone-change".into());
            let generation = storage.db_generation();
            let mut writer = UsageWriter::new(generation, old_zone);
            let past = CalendarDay::in_zone("2026-11-24", old_zone).unwrap();
            let utc_day = CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap();
            let at = utc_day.start_ms + 15 * HOUR;
            let before = span(1, at - HOUR, at);
            writer
                .write(
                    &storage,
                    &[
                        span(0, past.start_ms + HOUR, past.start_ms + 2 * HOUR),
                        before,
                    ],
                )
                .unwrap();
            writer.change_time_zone(&storage, zone, at).unwrap();
            let midnight = CalendarDay::in_zone("2026-11-25", zone).unwrap().end_ms;
            let mut after = span(2, at, at + HOUR / 2);
            writer.write(&storage, &[after.clone()]).unwrap();
            after.end_ms = midnight + HOUR;
            let pending = live(&storage, zone, vec![after.clone()]);
            let first = storage
                .usage_day("2026-11-25", pending.clone(), after.end_ms)
                .unwrap();
            let second = storage
                .usage_day("2026-11-26", pending.clone(), after.end_ms)
                .unwrap();
            assert_eq!(first.total_ms, HOUR + midnight - at);
            assert_eq!(first.calendar.rule, "system_time_zone_changed");
            assert!(first.calendar.time_zone.is_none());
            assert_eq!(second.total_ms, HOUR);
            assert_eq!(second.calendar.time_zone.as_deref(), Some(zone.name()));
            let details = storage
                .usage_app_details("2026-11-25", "code.exe", pending, after.end_ms)
                .unwrap();
            assert_eq!(details.titles[0].total_ms, first.total_ms);
            writer.write(&storage, &[after.clone()]).unwrap();
            writer.write(&storage, &[after.clone()]).unwrap();
            let saved = storage
                .usage_day("2026-11-25", live(&storage, zone, vec![]), after.end_ms)
                .unwrap();
            assert_eq!(saved.total_ms, first.total_ms);
            assert_eq!(stored_duration(&storage), 2 * HOUR + after.end_ms - at);
            assert_eq!(
                storage
                    .usage_day("2026-11-24", live(&storage, zone, vec![]), after.end_ms)
                    .unwrap()
                    .calendar,
                past,
                "closed dates keep their original calendar",
            );
        }
    }

    #[test]
    fn travelling_west_can_revisit_a_date_without_losing_or_double_counting_its_live_tail() {
        let storage = crate::storage::test_storage_with_schema("usage-zone-revisit".into());
        let mut writer = UsageWriter::new(storage.db_generation(), chrono_tz::Asia::Shanghai);
        let past = CalendarDay::in_zone("2026-11-24", chrono_tz::Asia::Shanghai).unwrap();
        let utc_day = CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap();
        let at = utc_day.start_ms + 6 * HOUR;
        writer
            .write(
                &storage,
                &[
                    span(0, past.end_ms - 4 * HOUR, past.end_ms - 3 * HOUR),
                    span(1, at - HOUR, at),
                ],
            )
            .unwrap();
        let zone = chrono_tz::America::Los_Angeles;
        writer.change_time_zone(&storage, zone, at).unwrap();
        let after = span(2, at, at + 4 * HOUR);
        let pending = live(&storage, zone, vec![after.clone()]);
        let revisited = storage
            .usage_day("2026-11-24", pending.clone(), after.end_ms)
            .unwrap();
        let next = storage
            .usage_day("2026-11-25", pending.clone(), after.end_ms)
            .unwrap();
        assert_eq!(revisited.total_ms, 3 * HOUR);
        assert_eq!(next.total_ms, 3 * HOUR);
        assert_eq!(
            next.week.iter().map(|day| day.total_ms).sum::<i64>(),
            6 * HOUR
        );
        for date in ["2026-11-24", "2026-11-25"] {
            let details = storage
                .usage_app_details(date, "code.exe", pending.clone(), after.end_ms)
                .unwrap();
            assert_eq!(details.titles[0].total_ms, 3 * HOUR);
        }
        writer.write(&storage, &[after.clone()]).unwrap();
        writer.write(&storage, &[after.clone()]).unwrap();
        assert_eq!(stored_duration(&storage), 6 * HOUR);
        for date in ["2026-11-24", "2026-11-25"] {
            let saved = storage
                .usage_day(date, live(&storage, zone, vec![]), after.end_ms)
                .unwrap();
            assert_eq!(saved.total_ms, 3 * HOUR);
        }
        storage.bump_db_generation();
        assert_eq!(
            writer
                .change_time_zone(&storage, chrono_tz::UTC, at)
                .unwrap_err(),
            "USAGE_SOURCE_CHANGED",
        );
    }
}
