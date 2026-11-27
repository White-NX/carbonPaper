/** Format a saved calendar time, optionally selecting an Intl date/time part. */
export function calendarTimeFormatter(locale, calendar, part) {
  const options = { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };
  let shiftMs = 0;
  if (calendar?.time_zone) {
    options.timeZone = calendar.time_zone;
  } else if (calendar?.rule === 'recorded_bounds' || calendar?.rule === 'system_time_zone_changed') {
    options.timeZone = 'UTC';
    const offset = calendar.start_utc_offset_seconds;
    if (calendar.rule === 'recorded_bounds' && Number.isFinite(offset) && offset === calendar.end_utc_offset_seconds) {
      shiftMs = offset * 1000;
    } else {
      // A day with multiple system zones has no single named clock rule.
      // The same applies to legacy days with unknown transitions.
      options.timeZoneName = 'short';
    }
  }
  const formatter = new Intl.DateTimeFormat(locale, options);
  return (ms) => {
    if (!Number.isFinite(ms)) return '';
    const date = new Date(ms + shiftMs);
    return part ? formatter.formatToParts(date).find((entry) => entry.type === part)?.value || '' : formatter.format(date);
  };
}
