import { describe, expect, it } from 'vitest';
import {
  captureDateOf,
  groupSearchResults,
  parseCreatedAt,
  sampleRecentCaptures,
} from './search_grouping';

/** 构造一条 OCR 结果，时间以秒为单位。 */
const hit = (id, processName, windowTitle, seconds) => ({
  screenshot_id: id,
  process_name: processName,
  window_title: windowTitle,
  screenshot_created_at: seconds,
  text: `text-${id}`,
});

const BASE = 1_754_900_000;

describe('parseCreatedAt', () => {
  it('treats plain numbers as seconds', () => {
    expect(parseCreatedAt(BASE).getTime()).toBe(BASE * 1000);
  });

  it('reads the RFC 3339 strings the backend sends', () => {
    expect(parseCreatedAt('2026-08-11T02:57:11Z')).toEqual(new Date('2026-08-11T02:57:11Z'));
  });

  it('reads a zone-less timestamp as UTC, not local time', () => {
    // Issue #166：数据库存的是 UTC，按本地时间解析会整体偏掉一个时区。
    expect(parseCreatedAt('2026-08-11 02:57:11')).toEqual(new Date('2026-08-11T02:57:11Z'));
  });

  it('honours an offset that is already present', () => {
    expect(parseCreatedAt('2026-08-11 10:57:11+08:00')).toEqual(new Date('2026-08-11T02:57:11Z'));
  });

  it('returns null for empty input', () => {
    expect(parseCreatedAt(null)).toBeNull();
    expect(parseCreatedAt('')).toBeNull();
  });
});

describe('captureDateOf', () => {
  it('prefers the numeric timestamp', () => {
    const item = { timestamp: BASE, screenshot_created_at: '2020-01-01T00:00:00Z' };
    expect(captureDateOf(item).getTime()).toBe(BASE * 1000);
  });

  it('ranks screenshot_created_at above created_at', () => {
    // 搜索结果的 created_at 是 OCR 行的写入时间，晚于拍摄。
    const item = {
      screenshot_created_at: '2026-08-11T02:57:11Z',
      created_at: '2026-08-11T03:20:00Z',
    };
    expect(captureDateOf(item)).toEqual(new Date('2026-08-11T02:57:11Z'));
  });

  it('does not read a vector hit with an unknown time as 1970', () => {
    const item = { metadata: { timestamp: 0, created_at: '2026-08-11T02:57:11Z' } };
    expect(captureDateOf(item)).toEqual(new Date('2026-08-11T02:57:11Z'));
  });

  it('returns null when nothing carries a time', () => {
    expect(captureDateOf({})).toBeNull();
  });
});

describe('groupSearchResults', () => {
  it('merges adjacent hits from the same window', () => {
    const groups = groupSearchResults([
      hit(1, 'msedge.exe', 'bilibili', BASE),
      hit(2, 'msedge.exe', 'bilibili', BASE - 13),
      hit(3, 'msedge.exe', 'bilibili', BASE - 28),
    ], 'ocr');

    expect(groups).toHaveLength(1);
    expect(groups[0].primary.screenshot_id).toBe(1);
    expect(groups[0].duplicates.map((item) => item.screenshot_id)).toEqual([2, 3]);
  });

  it('keeps hits apart when they are outside the time window', () => {
    const groups = groupSearchResults([
      hit(1, 'msedge.exe', 'bilibili', BASE),
      hit(2, 'msedge.exe', 'bilibili', BASE - 3600),
    ], 'ocr');

    expect(groups).toHaveLength(2);
  });

  it('does not merge different windows of the same process', () => {
    const groups = groupSearchResults([
      hit(1, 'Code.exe', 'a.ts', BASE),
      hit(2, 'Code.exe', 'b.ts', BASE - 5),
    ], 'ocr');

    expect(groups).toHaveLength(2);
  });

  it('never merges entries without process or window information', () => {
    const groups = groupSearchResults([
      hit(1, '', '', BASE),
      hit(2, '', '', BASE - 5),
    ], 'ocr');

    expect(groups).toHaveLength(2);
  });

  it('does not merge when timestamps are missing', () => {
    const groups = groupSearchResults([
      { screenshot_id: 1, process_name: 'a.exe', window_title: 'w' },
      { screenshot_id: 2, process_name: 'a.exe', window_title: 'w' },
    ], 'ocr');

    expect(groups).toHaveLength(2);
  });

  it('reads process and title from metadata in NL mode', () => {
    const groups = groupSearchResults([
      { screenshot_id: 1, metadata: { process_name: 'a.exe', window_title: 'w', created_at: BASE } },
      { screenshot_id: 2, metadata: { process_name: 'a.exe', window_title: 'w', created_at: BASE - 10 } },
    ], 'nl');

    expect(groups).toHaveLength(1);
    expect(groups[0].duplicates).toHaveLength(1);
  });

  it('preserves the incoming order', () => {
    const groups = groupSearchResults([
      hit(1, 'a.exe', 'w1', BASE),
      hit(2, 'b.exe', 'w2', BASE - 10),
      hit(3, 'a.exe', 'w1', BASE - 20),
    ], 'ocr');

    expect(groups.map((group) => group.primary.screenshot_id)).toEqual([1, 2, 3]);
  });

  it('returns an empty array for no results', () => {
    expect(groupSearchResults([], 'ocr')).toEqual([]);
  });
});

describe('sampleRecentCaptures', () => {
  /** 构造一张截图，时间以秒为单位。 */
  const shot = (id, processName, seconds) => ({
    screenshot_id: id,
    process_name: processName,
    created_at: seconds,
  });

  const MINUTE = 60;

  it('keeps only one capture per source within the cooldown', () => {
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'terminal.exe', BASE - 30),
      shot(3, 'terminal.exe', BASE - 90),
      shot(4, 'msedge.exe', BASE - 120),
    ], 2);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 4]);
  });

  it('lets a source return once the cooldown has passed', () => {
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'terminal.exe', BASE - 31 * MINUTE),
    ], 12);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2]);
  });

  it('collapses sources that alternate, which run-based folding would keep', () => {
    // 用户在编辑器和浏览器之间来回切换时，两个来源会互相打断对方的连续段。
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'msedge.exe', BASE - 20),
      shot(3, 'terminal.exe', BASE - 40),
      shot(4, 'msedge.exe', BASE - 60),
      shot(5, 'terminal.exe', BASE - 80),
    ], 2);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2]);
  });

  it('backfills skipped captures rather than leaving the grid short', () => {
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'terminal.exe', BASE - 30),
      shot(3, 'terminal.exe', BASE - 60),
    ], 3);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2, 3]);
  });

  it('backfills only as far as the requested count', () => {
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'terminal.exe', BASE - 30),
      shot(3, 'terminal.exe', BASE - 60),
      shot(4, 'terminal.exe', BASE - 90),
    ], 2);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2]);
  });

  it('keeps the newest-first order when backfilling', () => {
    const picked = sampleRecentCaptures([
      shot(1, 'terminal.exe', BASE),
      shot(2, 'terminal.exe', BASE - 30),
      shot(3, 'msedge.exe', BASE - 60),
    ], 3);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2, 3]);
  });

  it('never exceeds the requested count', () => {
    const records = Array.from({ length: 40 }, (_, index) =>
      shot(index + 1, `app-${index}.exe`, BASE - index * 10));

    expect(sampleRecentCaptures(records, 12)).toHaveLength(12);
  });

  it('does not fold captures with an unknown source together', () => {
    const picked = sampleRecentCaptures([
      shot(1, '', BASE),
      shot(2, '', BASE - 10),
    ], 12);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1, 2]);
  });

  it('reads the source from metadata when the record is nested', () => {
    const picked = sampleRecentCaptures([
      { screenshot_id: 1, metadata: { process_name: 'a.exe', created_at: BASE } },
      { screenshot_id: 2, metadata: { process_name: 'a.exe', created_at: BASE - 10 } },
    ], 1);

    expect(picked.map((item) => item.screenshot_id)).toEqual([1]);
  });

  it('handles empty input and a zero count', () => {
    expect(sampleRecentCaptures([], 12)).toEqual([]);
    expect(sampleRecentCaptures([shot(1, 'a.exe', BASE)], 0)).toEqual([]);
    expect(sampleRecentCaptures(null, 12)).toEqual([]);
  });
});
