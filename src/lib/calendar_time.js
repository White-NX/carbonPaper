/** Format a saved recap in the calendar that originally defined its day. */
export function calendarTimeFormatter(locale, calendar) {
  const options = { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };
  let shiftMs = 0;
  if (calendar?.time_zone) {
    options.timeZone = calendar.time_zone;
  } else if (calendar?.rule === 'recorded_bounds') {
    options.timeZone = 'UTC';
    const offset = calendar.start_utc_offset_seconds;
    if (Number.isFinite(offset) && offset === calendar.end_utc_offset_seconds) {
      shiftMs = offset * 1000;
    } else {
      // A legacy day can record the two endpoint offsets without recording
      // when its clock changed. Show explicit UTC instead of inventing a rule.
      options.timeZoneName = 'short';
    }
  }
  const formatter = new Intl.DateTimeFormat(locale, options);
  return (ms) => Number.isFinite(ms) ? formatter.format(new Date(ms + shiftMs)) : '';
}
