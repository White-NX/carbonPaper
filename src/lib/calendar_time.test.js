import { describe, expect, it } from 'vitest';
import { calendarTimeFormatter } from './calendar_time';
import fixture from '../tests/fixtures/screenshot_contract_v3.json';

describe('saved recap calendars', () => {
  it('renders the shared capture in its recorded calendar, independently of the current zone', () => {
    const time = calendarTimeFormatter('en-US', { time_zone: fixture.time_zone });
    expect(time(fixture.captured_at_ms)).toBe('00:30');
    expect(calendarTimeFormatter('en-US', { time_zone: 'UTC' })(fixture.captured_at_ms)).toBe('16:30');
    expect(time(null)).toBe('');
  });

  it('uses named daylight-saving rules for instants on either side of a clock change', () => {
    const time = calendarTimeFormatter('en-US', { time_zone: 'America/New_York' });
    expect(time(Date.parse('2026-03-08T06:59:00Z'))).toBe('01:59');
    expect(time(Date.parse('2026-03-08T07:00:00Z'))).toBe('03:00');
    expect(time(Date.parse('2026-11-01T05:59:00Z'))).toBe('01:59');
    expect(time(Date.parse('2026-11-01T06:00:00Z'))).toBe('01:00');
  });

  it('keeps the recorded offset of legacy days without inventing a named zone', () => {
    const time = calendarTimeFormatter('en-US', {
      rule: 'recorded_bounds', time_zone: null,
      start_utc_offset_seconds: 28800, end_utc_offset_seconds: 28800,
    });
    expect(time(fixture.captured_at_ms)).toBe('00:30');
  });

  it('labels UTC when a legacy clock transition rule is unavailable', () => {
    const time = calendarTimeFormatter('en-US', {
      rule: 'recorded_bounds', time_zone: null,
      start_utc_offset_seconds: -18000, end_utc_offset_seconds: -14400,
    });
    expect(time(Date.parse('2026-03-08T07:00:00Z'))).toBe('07:00 UTC');
  });
});
