import { expect, it } from 'vitest';
import { createTimelinePreviewCache, timelinePreviewEvents, TIMELINE_PREVIEW_THUMBNAIL_ITEMS } from './timeline_preview_cache';

it('evicts the least recently used image and releases entries outside the viewport', () => {
  const cache = createTimelinePreviewCache({ maxItems: 2, maxBytes: 100 });
  cache.set(1, 'one');
  cache.set(2, 'two');
  expect(cache.get(1)).toBe('one');
  cache.set(3, 'three');
  expect(cache.has(2)).toBe(false);
  expect(cache.size).toBe(2);
  expect(cache.bytes).toBe(16);
  cache.retain([3]);
  expect(cache.get(1)).toBeNull();
  expect(cache.bytes).toBe(10);
  cache.clear();
  expect(cache.size).toBe(0);
  expect(cache.bytes).toBe(0);
});

it('bounds string storage independently of item count and rejects oversized images', () => {
  const cache = createTimelinePreviewCache({ maxItems: 100, maxBytes: 16 });
  cache.set(1, 'aa');
  cache.set(2, 'bbbb');
  cache.set(3, 'ccc');
  expect(cache.has(1)).toBe(false);
  expect(cache.bytes).toBe(14);
  expect(cache.set(4, 'x'.repeat(9))).toBe(false);
  cache.set(3, 'c');
  expect(cache.bytes).toBe(10);
  for (let id = 4; id < 1000; id += 1) {
    cache.set(id, 'xxx');
    expect(cache.bytes).toBeLessThanOrEqual(16);
  }
});

const events = Array.from({ length: 1001 }, (_, index) => ({ id: index + 1, timestamp: index * 1000 }));

it('prefetches visible records before the quarter-screen margins and excludes unrelated history', () => {
  const planned = timelinePreviewEvents(events, 400000, 404000);
  expect(planned.map((event) => event.timestamp)).toEqual([402000, 401000, 403000, 400000, 404000, 399000, 405000]);
});

it('keeps dense and macro viewports bounded while covering both ends of the visible samples', () => {
  for (const [start, end] of [[0, 1000000], [100000, 900000], [0, 365 * 86400000]]) {
    const visible = events.filter((event) => event.timestamp >= start && event.timestamp <= end);
    const planned = timelinePreviewEvents(events, start, end);
    expect(planned.length).toBeLessThanOrEqual(TIMELINE_PREVIEW_THUMBNAIL_ITEMS);
    expect(planned.length).toBeGreaterThanOrEqual(Math.min(TIMELINE_PREVIEW_THUMBNAIL_ITEMS, visible.length));
    expect(planned).toContain(visible[0]);
    expect(planned).toContain(visible.at(-1));
    expect(new Set(planned.map((event) => event.id)).size).toBe(planned.length);
  }
});

it('ignores invalid ranges and records', () => {
  expect(timelinePreviewEvents(events, NaN, 1000)).toEqual([]);
  expect(timelinePreviewEvents(events, 1000, 1000)).toEqual([]);
  expect(timelinePreviewEvents(events, 1000, 0)).toEqual([]);
  expect(timelinePreviewEvents(events, 0, 1000, 0)).toEqual([]);
  expect(timelinePreviewEvents([{ id: -1, timestamp: 0 }, { id: 1, timestamp: NaN }, { id: '2', timestamp: 0 }], 0, 1000)).toEqual([]);
});
