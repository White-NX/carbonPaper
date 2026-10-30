import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import useTimelinePreviewThumbnails from './useTimelinePreviewThumbnails';

vi.mock('../lib/monitor_api', () => ({ fetchThumbnailBatch: vi.fn() }));
let batches;
beforeEach(() => {
  batches = [];
  fetchThumbnailBatch.mockReset().mockImplementation((ids, options) => new Promise((resolve) => batches.push({ ids, ...options, resolve })));
});
afterEach(() => vi.restoreAllMocks());
const events = Array.from({ length: 20 }, (_, index) => ({ id: index + 1, timestamp: index * 1000 }));
const props = { events, viewStart: 2000, viewEnd: 8000, enabled: true };
const finish = (batch) => act(async () => batch.resolve(Object.fromEntries(batch.ids.map((id) => [id, `thumb-${id}`]))));

it('loads one small cached-only batch at a time and gives visible frames priority', async () => {
  const onLoad = vi.fn();
  const { result } = renderHook(() => useTimelinePreviewThumbnails({ ...props, onLoad }));
  expect(batches).toHaveLength(1);
  expect(batches[0].ids).toEqual([6, 5, 7, 4]);
  expect(batches[0].cachedOnly).toBe(true);
  await finish(batches[0]);
  expect(batches).toHaveLength(2);
  expect(result.current.read(6)).toBe('thumb-6');
  expect(onLoad).toHaveBeenCalledWith(expect.arrayContaining([expect.objectContaining({ event: events[5], imageSrc: 'thumb-6' })]));
  expect(result.current.read(6)).toBe('thumb-6');
});

it('yields to hover work while retaining the native slot until the obsolete batch settles', async () => {
  const { result } = renderHook(() => useTimelinePreviewThumbnails(props));
  act(() => result.current.setForegroundBusy(true));
  expect(batches[0].signal.aborted).toBe(true);
  act(() => result.current.setForegroundBusy(false));
  expect(batches).toHaveLength(1);
  await finish(batches[0]);
  expect(result.current.read(6)).toBeNull();
  expect(batches).toHaveLength(2);
  expect(batches[1].ids).toEqual(batches[0].ids);
});

it('prunes an old viewport and rejects pending results before prefetching the destination', async () => {
  const { result, rerender } = renderHook((options) => useTimelinePreviewThumbnails(options), { initialProps: props });
  await finish(batches[0]);
  expect(result.current.read(6)).toBe('thumb-6');
  rerender({ ...props, viewStart: 15000, viewEnd: 19000 });
  expect(result.current.read(6)).toBeNull();
  expect(batches[1].signal.aborted).toBe(true);
  expect(batches).toHaveLength(2);
  await finish(batches[1]);
  expect(batches).toHaveLength(3);
  expect(batches[2].ids).toEqual([18, 17, 19, 16]);
  expect(result.current.read(6)).toBeNull();
});

it('clears loaded and late image data on refresh, disable and unmount', async () => {
  const { result, rerender, unmount } = renderHook((options) => useTimelinePreviewThumbnails(options), { initialProps: { ...props, revision: 0 } });
  await finish(batches[0]);
  rerender({ ...props, revision: 1 });
  expect(result.current.read(6)).toBeNull();
  expect(batches[1].signal.aborted).toBe(true);
  await finish(batches[1]);
  expect(batches).toHaveLength(3);
  rerender({ ...props, revision: 1, enabled: false });
  expect(batches[2].signal.aborted).toBe(true);
  await finish(batches[2]);
  expect(result.current.read(6)).toBeNull();
  expect(batches).toHaveLength(3);
  rerender({ ...props, revision: 1 });
  expect(batches).toHaveLength(4);
  const read = result.current.read;
  unmount();
  await finish(batches[3]);
  expect(read(6)).toBeNull();
});

it('pauses during range loading or gestures and resumes from cached entries', async () => {
  const { result, rerender } = renderHook((options) => useTimelinePreviewThumbnails(options), { initialProps: { ...props, paused: true } });
  expect(batches).toHaveLength(0);
  rerender({ ...props, paused: false });
  await finish(batches[0]);
  rerender({ ...props, paused: true });
  expect(result.current.read(6)).toBe('thumb-6');
  await finish(batches[1]);
  expect(batches).toHaveLength(2);
  rerender({ ...props, paused: false });
  expect(batches).toHaveLength(3);
  expect(batches[2].ids).not.toContain(6);
});

it('prioritizes an exact hover target outside the prefetch plan and reloads a removed cached image', async () => {
  const { result } = renderHook(() => useTimelinePreviewThumbnails(props));
  await finish(batches[0]);
  act(() => result.current.prioritize({ id: 99, timestamp: 5500 }));
  expect(batches[1].signal.aborted).toBe(false);
  await finish(batches[1]);
  expect(batches[2].ids[0]).toBe(99);
  await finish(batches[2]);
  expect(result.current.read(99)).toBe('thumb-99');
  const previousBatches = batches.length;
  act(() => result.current.remove(99));
  act(() => result.current.prioritize({ id: 99, timestamp: 5500 }));
  expect(batches).toHaveLength(previousBatches + 1);
  expect(batches.at(-1).ids[0]).toBe(99);
  await finish(batches.at(-1));
  expect(result.current.read(99)).toBe('thumb-99');
});
