import { expect, it } from 'vitest';
import { createTimelineIndex, timelineIndexedPreviewEvents } from './timeline_index';

it('matches storage ties and duplicate timestamp IDs, including subsecond pointer times', () => {
  const index = createTimelineIndex([[1, 1000], [2, 1000], [3, 2000], [4, 2000], [5, 5000]], 0, 6000);
  for (const [time, id] of [[0, 1], [999.9, 1], [1000, 2], [1500, 2], [1500.1, 3], [1999, 3], [2000, 4], [3500, 4], [3500.1, 5], [6000, 5]]) {
    expect(index.nearest(time).id).toBe(id);
  }
  expect(index.byteLength).toBe(5 * 16);
});

it('distinguishes missing coverage from empty history and uses outside neighbours in gaps', () => {
  const empty = createTimelineIndex([], 0, 1000);
  expect(empty.nearest(500)).toBeNull();
  expect(empty.nearest(-1)).toBeUndefined();
  expect(empty.nearest(NaN)).toBeUndefined();
  const gap = createTimelineIndex([[1, -100000], [2, 101000]], 0, 1000);
  expect(gap.nearest(500).id).toBe(1);
  expect(gap.nearest(501).id).toBe(2);
  expect(timelineIndexedPreviewEvents(gap, 0, 1000).map((event) => event.id)).toEqual([1, 2]);
});

it('prefetches exact device-pixel targets omitted by coarse timeline samples', () => {
  const day = 86400000;
  const entries = Array.from({ length: 8640 }, (_, i) => [i + 1, i * 10000]);
  const index = createTimelineIndex(entries, -day / 4, day * 1.25);
  const planned = timelineIndexedPreviewEvents(index, 0, day, { width: 800, pixelRatio: 1.5, trackLeft: 10.25 });
  const ids = new Set(planned.map((event) => event.id));
  for (let physicalX = Math.ceil(10.25 * 1.5); physicalX <= (800 + 10.25) * 1.5; physicalX += 1) {
    const offset = (physicalX / 1.5 - 10.25) / 800 * day;
    const expectedId = Math.min(8640, Math.ceil(offset / 10000 - 0.5) + 1);
    expect(ids.has(expectedId), `missing exact pointer target ${expectedId}`).toBe(true);
  }
  expect(planned.length).toBeLessThanOrEqual(2048);
  expect(ids.size).toBe(planned.length);
  expect(planned.some((event) => (event.id - 1) % 60 !== 0)).toBe(true);
});

it('bounds wide/high-DPI prefetch and retains all records in a sparse viewport', () => {
  const index = createTimelineIndex(Array.from({ length: 10000 }, (_, i) => [i + 1, i * 1000]), -1000, 10000000);
  const wide = timelineIndexedPreviewEvents(index, 0, 9999000, { width: 8000, pixelRatio: 3, limit: 128 });
  expect(wide.length).toBeLessThanOrEqual(128);
  expect(wide.map((event) => event.id)).toEqual(expect.arrayContaining([1, 10000]));
  const sparse = timelineIndexedPreviewEvents(index, 400000, 404000, { width: 800, limit: 32 });
  for (const id of [401, 402, 403, 404, 405]) expect(sparse.map((event) => event.id)).toContain(id);
  expect(timelineIndexedPreviewEvents(index, -2000, 0)).toEqual([]);
});

it('rejects invalid or unsorted index data rather than treating it as complete coverage', () => {
  for (const entries of [[[0, 1000]], [[1, NaN]], [[2, 1000], [1, 1000]], [[1, 2000], [2, 1000]]]) {
    expect(() => createTimelineIndex(entries, 0, 3000)).toThrow();
  }
  expect(() => createTimelineIndex([], NaN, 1000)).toThrow();
});
