import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { getTimelineIndex } from '../lib/monitor_api';
import useTimelinePreviewIndex from './useTimelinePreviewIndex';

vi.mock('../lib/monitor_api', () => ({ getTimelineIndex: vi.fn() }));
let reads;
const props = { enabled: true, paused: false, revision: 0, viewStart: 1000, viewEnd: 3000 };
beforeEach(() => {
  vi.useFakeTimers();
  reads = [];
  getTimelineIndex.mockReset().mockImplementation((start, end, { signal }) => new Promise((resolve) => reads.push({ start, end, signal, resolve })));
});
afterEach(() => vi.useRealTimers());
const finish = (read, entries = [[1, 0], [2, 2000], [3, 4000]]) => act(async () => read.resolve(entries));

it('loads complete millisecond coverage with margins and reuses it for small viewport moves', async () => {
  const { result, rerender } = renderHook((options) => useTimelinePreviewIndex(options), { initialProps: props });
  expect(reads[0]).toMatchObject({ start: 500, end: 3500 });
  expect(result.current.read(2000)).toBeUndefined();
  await finish(reads[0]);
  expect(result.current.read(2000).id).toBe(2);
  expect(result.current.read(3600)).toBeUndefined();
  rerender({ ...props, viewStart: 1100, viewEnd: 3100 });
  expect(reads).toHaveLength(1);
  rerender({ ...props, viewStart: 3900, viewEnd: 5900 });
  expect(reads[1]).toMatchObject({ start: 3400, end: 6400 });
});

it('retains only the latest requested range and waits for obsolete SQL to settle', async () => {
  const { result, rerender } = renderHook((options) => useTimelinePreviewIndex(options), { initialProps: props });
  rerender({ ...props, viewStart: 10000, viewEnd: 12000 });
  rerender({ ...props, viewStart: 20000, viewEnd: 22000 });
  expect(reads).toHaveLength(1);
  expect(reads[0].signal.aborted).toBe(true);
  await finish(reads[0]);
  expect(result.current.read(2000)).toBeUndefined();
  expect(reads[1]).toMatchObject({ start: 19500, end: 22500 });
  await finish(reads[1], [[4, 19000], [5, 21000], [6, 23000]]);
  expect(result.current.read(21000).id).toBe(5);
});

it('refreshes new captures without starving an active read and defers reads during gestures', async () => {
  const { result, rerender } = renderHook((options) => useTimelinePreviewIndex(options), { initialProps: { ...props, paused: true } });
  expect(reads).toHaveLength(0);
  rerender(props);
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(reads[0].signal.aborted).toBe(false);
  await finish(reads[0]);
  expect(reads).toHaveLength(1);
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(reads).toHaveLength(2);
  await finish(reads[1], [[1, 0], [4, 2000], [3, 4000]]);
  expect(result.current.read(2000).id).toBe(4);
  rerender({ ...props, paused: true });
  await act(async () => vi.advanceTimersByTimeAsync(10000));
  expect(reads).toHaveLength(2);
});

it('clears index data and rejects late reads on refresh, lock and unmount', async () => {
  const { result, rerender, unmount } = renderHook((options) => useTimelinePreviewIndex(options), { initialProps: props });
  await finish(reads[0]);
  rerender({ ...props, revision: 1 });
  expect(result.current.read(2000)).toBeUndefined();
  rerender({ ...props, revision: 1, enabled: false });
  expect(reads[1].signal.aborted).toBe(true);
  await finish(reads[1]);
  expect(result.current.read(2000)).toBeUndefined();
  rerender({ ...props, revision: 1 });
  const read = result.current.read;
  unmount();
  expect(reads[2].signal.aborted).toBe(true);
  await finish(reads[2]);
  expect(read(2000)).toBeUndefined();
});
