//! Civil dates are interpreted with named time-zone rules, then represented by
//! half-open UTC bounds. Saved recap calendars never follow later OS changes.
use chrono::{DateTime, Duration, Months, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

pub(crate) fn time_zone(name: Option<&str>) -> Result<Tz, String> {
    let name = match name {
        Some(name) => name.to_owned(),
        None => iana_time_zone::get_timezone()
            .map_err(|e| format!("CALENDAR_TIME_ZONE_UNAVAILABLE: {e}"))?,
    };
    name.parse()
        .map_err(|_| "CALENDAR_INVALID_TIME_ZONE".into())
}

pub(crate) fn parse_date(date: &str) -> Result<NaiveDate, String> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "RECAP_INVALID_DATE")?;
    if parsed.to_string() != date {
        return Err("RECAP_INVALID_DATE".into());
    }
    Ok(parsed)
}

fn wall_time(zone: Tz, day: NaiveDate, hour: u32) -> Result<DateTime<Tz>, String> {
    let wall = day.and_hms_opt(hour, 0, 0).ok_or("RECAP_INVALID_DATE")?;
    // Some zones advance their clocks at midnight. Start at the first valid
    // local instant; choose the earlier instant when a clock repeats.
    for minute in 0..=180 {
        let candidate = wall
            .checked_add_signed(Duration::minutes(minute))
            .ok_or("RECAP_INVALID_DATE")?;
        if let Some(instant) = zone.from_local_datetime(&candidate).earliest() {
            return Ok(instant);
        }
    }
    Err("RECAP_INVALID_DATE".into())
}

pub(crate) fn month_bounds(month: &str, zone: Tz) -> Result<(i64, i64), String> {
    let day = parse_date(&format!("{month}-01")).map_err(|_| "CALENDAR_INVALID_MONTH")?;
    let next = day
        .checked_add_months(Months::new(1))
        .ok_or("CALENDAR_INVALID_MONTH")?;
    Ok((
        wall_time(zone, day, 0)?.timestamp_millis(),
        wall_time(zone, next, 0)?.timestamp_millis(),
    ))
}

/// `recorded_bounds` is the honest provenance for legacy days: their UTC
/// bounds survive, but the original named time zone was never recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDay {
    pub date: String,
    pub time_zone: Option<String>,
    pub rule: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub start_utc_offset_seconds: i32,
    pub end_utc_offset_seconds: i32,
    pub period_boundaries_ms: Vec<i64>,
}

impl CalendarDay {
    pub fn local(date: &str) -> Result<Self, String> {
        Self::in_zone(date, time_zone(None)?)
    }

    pub(crate) fn in_zone(date: &str, zone: Tz) -> Result<Self, String> {
        let day = parse_date(date)?;
        let next = day.succ_opt().ok_or("RECAP_INVALID_DATE")?;
        let start = wall_time(zone, day, 0)?;
        let end = wall_time(zone, next, 0)?;
        let mut boundaries = vec![start.timestamp_millis()];
        for hour in [4, 8, 12, 16, 20] {
            boundaries.push(wall_time(zone, day, hour)?.timestamp_millis());
        }
        boundaries.push(end.timestamp_millis());
        boundaries.sort_unstable();
        boundaries.dedup();
        Ok(Self {
            date: date.into(),
            time_zone: Some(zone.name().into()),
            rule: "system_local_at_creation".into(),
            start_ms: start.timestamp_millis(),
            end_ms: end.timestamp_millis(),
            start_utc_offset_seconds: start.offset().fix().local_minus_utc(),
            end_utc_offset_seconds: end.offset().fix().local_minus_utc(),
            period_boundaries_ms: boundaries,
        })
    }

    pub(crate) fn recorded(
        date: &str,
        start_ms: i64,
        end_ms: i64,
        saved_starts: &[i64],
    ) -> Result<Self, String> {
        let day = parse_date(date)?;
        let next = day.succ_opt().ok_or("RECAP_INVALID_DATE")?;
        let midnight = day
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let next_midnight = next
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let start_offset = midnight.checked_sub(start_ms).ok_or("RECAP_INVALID_DATE")? / 1000;
        let end_offset = next_midnight
            .checked_sub(end_ms)
            .ok_or("RECAP_INVALID_DATE")?
            / 1000;
        if start_ms >= end_ms || start_offset.abs() >= 86_400 || end_offset.abs() >= 86_400 {
            return Err("RECAP_INVALID_DATE".into());
        }
        let mut boundaries = vec![start_ms, end_ms];
        boundaries.extend(
            saved_starts
                .iter()
                .copied()
                .filter(|t| *t > start_ms && *t < end_ms),
        );
        let last_saved_start = boundaries
            .iter()
            .copied()
            .filter(|t| *t < end_ms)
            .max()
            .unwrap_or(start_ms);
        // Saved period keys take precedence over reconstruction. For a legacy
        // clock transition, unfinished periods after the final saved start use
        // the closing offset. Never insert a guessed boundary between saved keys.
        for hour in [4, 8, 12, 16, 20] {
            let boundary = midnight + hour * 3_600_000 - end_offset * 1000;
            if boundary > last_saved_start && boundary < end_ms {
                boundaries.push(boundary);
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        Ok(Self {
            date: date.into(),
            time_zone: None,
            rule: "recorded_bounds".into(),
            start_ms,
            end_ms,
            start_utc_offset_seconds: start_offset as i32,
            end_utc_offset_seconds: end_offset as i32,
            period_boundaries_ms: boundaries,
        })
    }

    pub fn closed_periods(&self, now_ms: i64) -> Vec<(i64, i64)> {
        self.period_boundaries_ms
            .windows(2)
            .filter(|w| w[1] <= now_ms)
            .map(|w| (w[0], w[1]))
            .collect()
    }
}

pub(crate) fn sqlite_utc(ms: i64) -> Result<String, String> {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        .ok_or_else(|| "CALENDAR_INVALID_TIMESTAMP".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_midnight_and_month_boundaries_cover_the_same_capture() {
        let zone = chrono_tz::Asia::Shanghai;
        let capture = DateTime::parse_from_rfc3339("2026-10-01T00:30:00+08:00")
            .unwrap()
            .timestamp_millis();
        let day = CalendarDay::in_zone("2026-10-01", zone).unwrap();
        let september = month_bounds("2026-09", zone).unwrap();
        let october = month_bounds("2026-10", zone).unwrap();
        assert!(day.start_ms <= capture && capture < day.end_ms);
        assert!(capture >= september.1);
        assert!(october.0 <= capture && capture < october.1);
        assert_eq!(sqlite_utc(day.start_ms).unwrap(), "2026-09-30 16:00:00");
        for invalid in ["2026-13", "2026-00", "2026-1", "2026-10-01"] {
            assert!(month_bounds(invalid, zone).is_err());
        }
    }

    #[test]
    fn saved_days_keep_dst_periods_when_the_current_zone_changes() {
        for (date, hours, first_hours) in [("2026-03-08", 23, 3), ("2026-11-01", 25, 5)] {
            let day = CalendarDay::in_zone(date, chrono_tz::America::New_York).unwrap();
            assert_eq!(day.end_ms - day.start_ms, hours * 3_600_000);
            let periods = day.closed_periods(day.end_ms);
            assert_eq!(periods.len(), 6);
            assert_eq!(periods[0].1 - periods[0].0, first_hours * 3_600_000);
            assert!(periods.windows(2).all(|p| p[0].1 == p[1].0));
            let stored = serde_json::to_string(&day).unwrap();
            let other = CalendarDay::in_zone(date, chrono_tz::Asia::Tokyo).unwrap();
            assert_ne!(day.start_ms, other.start_ms);
            let restored: CalendarDay = serde_json::from_str(&stored).unwrap();
            assert_eq!(restored.closed_periods(day.end_ms), periods);
            assert_eq!(restored.closed_periods(periods[0].1 - 1).len(), 0);
        }
    }

    #[test]
    fn legacy_bounds_recover_the_recorded_offset_without_claiming_a_zone() {
        let original = CalendarDay::in_zone("2026-10-01", chrono_tz::Asia::Shanghai).unwrap();
        let legacy = CalendarDay::recorded(
            &original.date,
            original.start_ms,
            original.end_ms,
            &original.period_boundaries_ms,
        )
        .unwrap();
        assert_eq!(legacy.time_zone, None);
        assert_eq!(legacy.start_utc_offset_seconds, 8 * 3600);
        assert_eq!(
            legacy.closed_periods(legacy.end_ms),
            original.closed_periods(original.end_ms)
        );
    }
}
