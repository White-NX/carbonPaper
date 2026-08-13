//! The timestamp format used when a stored time leaves the backend.
//!
//! Every `created_at` column in this database is filled by SQLite's
//! `DEFAULT CURRENT_TIMESTAMP`, which writes UTC wall-clock text with no zone
//! marker: `2026-08-11 06:07:40`. That shape is ambiguous the moment it leaves
//! Rust. JavaScript's `Date` reads a date-time without an offset as *local*
//! time, so handing the column through unchanged shifted every search result
//! by the machine's UTC offset — eight hours in UTC+8, which is what issue #166
//! reported.
//!
//! Patching the reader was not enough, because the two search paths disagreed
//! about what the string meant: the OCR path forwarded the raw UTC text while
//! the CLIP path converted to local time first, and the two are
//! indistinguishable once serialized. So the format is pinned here instead, and
//! everything that crosses into the frontend or the MCP server renders through
//! this module: RFC 3339 in UTC, with the `Z` that makes it self-describing.

use chrono::{DateTime, NaiveDateTime, Utc};

/// How SQLite writes `CURRENT_TIMESTAMP`.
const SQLITE_UTC: &str = "%Y-%m-%d %H:%M:%S";

/// What the frontend and the MCP server receive.
const WIRE: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Render Unix seconds as `2026-08-11T06:07:40Z`.
///
/// Prefer this over [`from_sqlite_utc`] where a query already selects
/// `strftime('%s', …)`: the seconds are unambiguous to begin with, so nothing
/// has to be re-parsed.
pub(crate) fn from_unix_seconds(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|value| value.format(WIRE).to_string())
        .unwrap_or_default()
}

/// Same, for a timestamp that may be missing. An absent time renders empty,
/// which is what the frontend's parsers already treat as "no time".
pub(crate) fn from_optional_seconds(seconds: Option<i64>) -> String {
    seconds.map(from_unix_seconds).unwrap_or_default()
}

/// Convert a raw `created_at` column to the wire format.
///
/// Text that does not parse is returned untouched rather than blanked, so a row
/// written by some future migration in a different shape still reaches the UI
/// with something to show.
pub(crate) fn from_sqlite_utc(text: &str) -> String {
    match sqlite_utc_to_seconds(text) {
        Some(seconds) => from_unix_seconds(seconds),
        None => text.to_string(),
    }
}

/// Unix seconds for a raw `created_at` column, or `None` when it does not parse.
///
/// Accepts the wire format too, so a value that has already been converted can
/// be fed back in without a special case at the call site.
pub(crate) fn sqlite_utc_to_seconds(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, SQLITE_UTC) {
        return Some(naive.and_utc().timestamp());
    }
    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|value| value.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_render_with_a_zone_marker() {
        // Without the trailing `Z` this is the bug from issue #166: JavaScript
        // would read it as local time.
        assert_eq!(from_unix_seconds(1_786_428_460), "2026-08-11T06:07:40Z");
    }

    #[test]
    fn missing_time_renders_empty() {
        assert_eq!(from_optional_seconds(None), "");
        assert_eq!(from_optional_seconds(Some(0)), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn sqlite_text_round_trips_through_the_wire_format() {
        let wire = from_sqlite_utc("2026-08-11 06:07:40");
        assert_eq!(wire, "2026-08-11T06:07:40Z");
        assert_eq!(sqlite_utc_to_seconds(&wire), Some(1_786_428_460));
    }

    #[test]
    fn unparseable_text_survives() {
        assert_eq!(from_sqlite_utc("not a time"), "not a time");
        assert_eq!(sqlite_utc_to_seconds("not a time"), None);
        assert_eq!(sqlite_utc_to_seconds(""), None);
    }

    #[test]
    fn an_offset_is_honoured_rather_than_dropped() {
        // The same instant as the naive UTC case above, written from UTC+8.
        assert_eq!(
            sqlite_utc_to_seconds("2026-08-11T14:07:40+08:00"),
            Some(1_786_428_460)
        );
    }
}
