import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchImage, getTimelineNearest } from '../lib/monitor_api';
import useTimelineHoverPreview from './useTimelineHoverPreview';

vi.mock('../lib/monitor_api', () => ({ fetchImage: vi.fn(), getTimelineNearest: vi.fn() }));
let lookups;
let images;
beforeEach(() => {
  vi.useFakeTimers();
  lookups = [];
  images = [];
  getTimelineNearest.mockReset().mockImplementation((time, { signal }) => new Promise((resolve) => lookups.push({ time, signal, resolve })));
  fetchImage.mockReset().mockImplementation((id) => new Promise((resolve) => images.push({ id, resolve })));
});
afterEach(() => vi.useRealTimers());
const target = (time, id = 1) => ({ time, event: { id, timestamp: time }, clientX: 300, clientY: 40, trackTop: 0, trackBottom: 72 });
const record = (id, timestamp) => ({ id, timestamp: timestamp / 1000, process_name: 'app', window_title: `snapshot-${id}` });
const advance = (ms = 100) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

it('loads only the actual nearest original after a hover settles, and retains the image through exit', async () => {
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true }));
  act(() => result.current.show(target(5000)));
  await advance(99);
  expect(lookups).toHaveLength(0);
  expect(images).toHaveLength(0);
  await advance(1);
  await act(async () => lookups[0].resolve(record(2, 6000)));
  expect(images[0].id).toBe(2);
  expect(result.current.read(5000).id).toBe(2);
  await act(async () => images[0].resolve('original-2'));
  expect(result.current.state.imageSrc).toBe('original-2');
  act(() => result.current.hide());
  expect(result.current.state.visible).toBe(false);
  expect(result.current.state.imageSrc).toBe('original-2');
  await advance(180);
  expect(result.current.state.imageSrc).toBeNull();
});

it('does not read any image when the pointer passes over the timeline briefly', async () => {
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true }));
  act(() => result.current.show(target(5000)));
  await advance(50);
  act(() => result.current.hide());
  await advance(500);
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(fetchImage).not.toHaveBeenCalled();
});

it('keeps an empty history quiet and exposes corrupt image failures without retaining the image', async () => {
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true }));
  act(() => result.current.show(target(5000)));
  await advance();
  await act(async () => lookups[0].resolve(null));
  expect(result.current.state.visible).toBe(false);
  act(() => result.current.show(target(5000)));
  await advance();
  expect(images).toHaveLength(0);
  expect(lookups).toHaveLength(1);
  act(() => result.current.show(target(6000)));
  await advance();
  await act(async () => lookups[1].resolve(record(1, 6000)));
  await act(async () => images[0].resolve('corrupt image'));
  act(() => result.current.reportImageError(1));
  expect(result.current.state.imageSrc).toBeNull();
  expect(result.current.state.error).toBe(true);
  act(() => result.current.hide());
  act(() => result.current.show(target(6000)));
  expect(result.current.state.imageSrc).toBeNull();
});

it('discards an obsolete original and waits for it to settle before loading the latest target', async () => {
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true }));
  act(() => result.current.show(target(5000)));
  await advance();
  await act(async () => lookups[0].resolve(record(1, 5000)));
  act(() => result.current.show(target(8000, 2)));
  await advance();
  await act(async () => lookups[1].resolve(record(2, 8000)));
  expect(images).toHaveLength(1);
  await act(async () => images[0].resolve('stale original'));
  expect(result.current.state.imageSrc).toBeNull();
  expect(images).toHaveLength(2);
  await act(async () => images[1].resolve('current original'));
  expect(result.current.state.imageSrc).toBe('current original');
});

it('reuses a cached original while active and clears it when the window is paused', async () => {
  const { result, rerender } = renderHook(({ enabled }) => useTimelineHoverPreview({ enabled }), { initialProps: { enabled: true } });
  act(() => result.current.show(target(5000)));
  await advance();
  await act(async () => lookups[0].resolve(record(1, 5000)));
  await act(async () => images[0].resolve('cached original'));
  act(() => result.current.hide());
  await advance(180);
  act(() => result.current.show(target(5000)));
  expect(result.current.state.imageSrc).toBe('cached original');
  await advance();
  expect(images).toHaveLength(1);
  rerender({ enabled: false });
  expect(result.current.state.imageSrc).toBeNull();
  rerender({ enabled: true });
  act(() => result.current.show(target(5000)));
  expect(result.current.state.imageSrc).toBeNull();
  await advance();
  expect(lookups).toHaveLength(2);
});

it('refreshes an old hover resolution so a new capture can become the nearest snapshot', async () => {
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true }));
  act(() => result.current.show(target(5000)));
  await advance();
  await act(async () => lookups[0].resolve(record(1, 4000)));
  await act(async () => images[0].resolve('old original'));
  await advance(600);
  expect(result.current.read(5000)).toBeNull();
  act(() => result.current.show(target(5000)));
  await advance();
  expect(lookups).toHaveLength(2);
  await act(async () => lookups[1].resolve(record(2, 5000)));
  expect(result.current.read(5000).id).toBe(2);
});

it('rejects late lookups and originals after pause, refresh or unmount', async () => {
  const { result, rerender, unmount } = renderHook(({ enabled, revision }) => useTimelineHoverPreview({ enabled, revision }),
    { initialProps: { enabled: true, revision: 0 } });
  act(() => result.current.show(target(5000)));
  await advance();
  rerender({ enabled: false, revision: 0 });
  expect(lookups[0].signal.aborted).toBe(true);
  await act(async () => lookups[0].resolve(record(1, 5000)));
  expect(images).toHaveLength(0);
  rerender({ enabled: true, revision: 0 });
  act(() => result.current.show(target(6000)));
  await advance();
  await act(async () => lookups[1].resolve(record(2, 6000)));
  rerender({ enabled: true, revision: 1 });
  await act(async () => images[0].resolve('stale after reset'));
  expect(result.current.state.imageSrc).toBeNull();
  unmount();
});
