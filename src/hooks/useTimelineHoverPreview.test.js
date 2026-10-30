import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchImage, fetchThumbnailBatch, getTimelineDetails, getTimelineIndex, getTimelineNearest } from '../lib/monitor_api';
import useTimelineHoverPreview from './useTimelineHoverPreview';

vi.mock('../lib/monitor_api', () => ({ fetchImage: vi.fn(), fetchThumbnailBatch: vi.fn(), getTimelineDetails: vi.fn(), getTimelineIndex: vi.fn(), getTimelineNearest: vi.fn() }));
let lookups;
let images;
beforeEach(() => {
  vi.useFakeTimers();
  lookups = [];
  images = [];
  getTimelineNearest.mockReset().mockImplementation((time, { signal }) => new Promise((resolve) => lookups.push({ time, signal, resolve })));
  fetchImage.mockReset().mockImplementation((id) => new Promise((resolve) => images.push({ id, resolve })));
  fetchThumbnailBatch.mockReset().mockResolvedValue({});
  getTimelineIndex.mockReset().mockResolvedValue([]);
  getTimelineDetails.mockReset().mockResolvedValue([]);
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
const target = (time, id = 1, zoom = 0.001) => ({ time, zoom, event: { id, timestamp: time }, clientX: 300, clientY: 40, trackTop: 0, trackBottom: 72 });
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
  expect(result.current.read(5000).id).toBe(1);
  act(() => result.current.hide());
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

const sampled = (id, timestamp) => ({ id, timestamp, appName: `app-${id}`, windowTitle: `sample-${id}` });
const thumbnailProps = (events, exact = events) => {
  getTimelineIndex.mockResolvedValue(exact.map((event) => [event.id, event.timestamp]));
  getTimelineDetails.mockImplementation(async (ids) => exact.filter((event) => ids.includes(event.id)).map((event) => record(event.id, event.timestamp)));
  return { enabled: true, events, viewStart: 4000, viewEnd: 8000, width: 800 };
};

it('selects an unsampled exact ID immediately and never substitutes a sampled thumbnail during decoding', async () => {
  const event = sampled(1, 5000);
  const exact = sampled(2, 6000);
  fetchThumbnailBatch.mockResolvedValue({ 1: 'thumbnail-1' });
  let finishDecode;
  vi.stubGlobal('Image', class { decode() { return new Promise((resolve) => { finishDecode = resolve; }); } });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps([event], [event, exact])));
  await act(async () => {});
  act(() => result.current.show({ ...target(5600), event }));
  expect(result.current.state).toMatchObject({ event: { id: 2, timestamp: 6000 }, imageSrc: null });
  expect(result.current.read(5600).id).toBe(2);
  await advance();
  expect(lookups).toHaveLength(0);
  expect(images[0].id).toBe(2);
  await act(async () => images[0].resolve('original-2'));
  expect(result.current.state).toMatchObject({ event: { id: 2 }, imageSrc: null });
  await act(async () => finishDecode());
  expect(result.current.state).toMatchObject({
    event: { id: 2, timestamp: 6000, windowTitle: 'snapshot-2' }, imageSrc: 'original-2', source: 'original',
  });
});

it('shows the exact prefetched thumbnail immediately and upgrades only that ID to an original', async () => {
  const events = [sampled(1, 5000), sampled(2, 6000)];
  fetchThumbnailBatch.mockResolvedValue({ 1: 'thumbnail-1', 2: 'thumbnail-2' });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps(events)));
  await act(async () => {});
  act(() => result.current.show({ ...target(5600), event: events[0] }));
  expect(result.current.state).toMatchObject({ event: events[1], imageSrc: 'thumbnail-2', source: 'thumbnail' });
  await advance();
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(images[0].id).toBe(2);
  await act(async () => images[0].resolve('original-2'));
  expect(result.current.state).toMatchObject({ event: { id: 2 }, imageSrc: 'original-2' });
});

it('keeps fast sweeps free of original requests and never fills a missing exact thumbnail with another ID', async () => {
  const events = [sampled(1, 5000), sampled(2, 6000), sampled(3, 7000)];
  fetchThumbnailBatch.mockResolvedValue({ 1: 'thumbnail-1', 3: 'thumbnail-3' });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps(events)));
  await act(async () => {});
  act(() => result.current.show({ ...target(6000, 2), event: events[1] }));
  expect(result.current.state).toMatchObject({ event: { id: 2 }, imageSrc: null });
  await advance(20);
  act(() => result.current.show({ ...target(6010, 2), event: events[1] }));
  expect(result.current.state).toMatchObject({ event: { id: 2 }, imageSrc: null });
  await advance(20);
  act(() => result.current.hide());
  await advance(200);
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(fetchImage).not.toHaveBeenCalled();
});

it('continues exact thumbnail prefetch during a sweep and yields when original work starts', async () => {
  let finishBatch;
  let signal;
  fetchThumbnailBatch.mockImplementation((_ids, options) => {
    signal = options.signal;
    return new Promise((resolve) => { finishBatch = resolve; });
  });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps([sampled(1, 5000)])));
  await act(async () => {});
  act(() => result.current.show(target(5000)));
  await advance(50);
  expect(signal.aborted).toBe(false);
  act(() => result.current.show(target(5100)));
  await advance(99);
  expect(signal.aborted).toBe(false);
  await advance(1);
  expect(signal.aborted).toBe(true);
  await act(async () => finishBatch({ 1: 'stale-thumbnail' }));
  expect(result.current.state.imageSrc).toBeNull();
});

it('clears exact thumbnails after pause or refresh and discards an original still decoding', async () => {
  const event = sampled(1, 5000);
  let finishDecode;
  vi.stubGlobal('Image', class { decode() { return new Promise((resolve) => { finishDecode = resolve; }); } });
  fetchThumbnailBatch.mockResolvedValue({ 1: 'thumbnail-1' });
  const props = thumbnailProps([event]);
  const { result, rerender } = renderHook((options) => useTimelineHoverPreview(options), { initialProps: props });
  await act(async () => {});
  act(() => result.current.show(target(5000)));
  await advance();
  await act(async () => images[0].resolve('original-1'));
  rerender({ ...props, enabled: false });
  await act(async () => finishDecode());
  expect(result.current.state.imageSrc).toBeNull();
  fetchThumbnailBatch.mockResolvedValue({});
  rerender({ ...props, revision: 1 });
  await act(async () => {});
  act(() => result.current.show(target(5000)));
  expect(result.current.state.imageSrc).toBeNull();
});

it('recovers a corrupt exact thumbnail by loading the same original', async () => {
  const event = sampled(1, 5000);
  fetchThumbnailBatch.mockResolvedValue({ 1: 'broken-thumbnail' });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps([event])));
  await act(async () => {});
  act(() => result.current.show(target(5000)));
  act(() => result.current.reportImageError(1, 'thumbnail'));
  expect(result.current.state).toMatchObject({ imageSrc: null, error: false });
  await advance();
  await act(async () => images[0].resolve('original-1'));
  expect(result.current.state).toMatchObject({ event: { id: 1 }, imageSrc: 'original-1', source: 'original', error: false });
});

it('keeps the first image ID stable when the exact thumbnail arrives 40ms after the pointer stops', async () => {
  const hour = 3600000;
  const events = [sampled(4, 12 * hour - 1200000), sampled(2, 12 * hour - 600000), sampled(1, 12 * hour), sampled(3, 12 * hour + 600000), sampled(5, 13 * hour)];
  getTimelineIndex.mockResolvedValue(events.map((event) => [event.id, event.timestamp]));
  let finishBatch;
  fetchThumbnailBatch.mockImplementationOnce(async (ids) => Object.fromEntries(ids.map((id) => [id, `thumbnail-${id}`])))
    .mockImplementationOnce(() => new Promise((resolve) => { finishBatch = resolve; }));
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true, events, viewStart: 0, viewEnd: 24 * hour, width: 800 }));
  await act(async () => {});
  act(() => result.current.show({ ...target(13 * hour, 5, 800 / (24 * hour)), event: events.at(-1) }));
  expect(result.current.state).toMatchObject({ event: { id: 5 }, imageSrc: null });
  await advance(40);
  await act(async () => finishBatch({ 5: 'thumbnail-5' }));
  expect(result.current.state).toMatchObject({ event: { id: 5 }, imageSrc: 'thumbnail-5' });
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(fetchImage).not.toHaveBeenCalled();
});

it('preserves immediate exact images through a day-scale sweep and never changes the stopped snapshot ID', async () => {
  const day = 86400000;
  const full = Array.from({ length: 8640 }, (_, i) => sampled(i + 1, i * 10000));
  const samples = full.filter((_, i) => i % 60 === 0);
  getTimelineIndex.mockResolvedValue(full.map((event) => [event.id, event.timestamp]));
  getTimelineDetails.mockImplementation(async (ids) => full.filter((event) => ids.includes(event.id)).map((event) => record(event.id, event.timestamp)));
  fetchThumbnailBatch.mockImplementation(async (ids) => Object.fromEntries(ids.map((id) => [id, `thumbnail-${id}`])));
  const { result } = renderHook(() => useTimelineHoverPreview({ enabled: true, events: samples, viewStart: 0, viewEnd: day, width: 800 }));
  await advance(0);
  let stoppedId;
  for (const x of [200, 350, 337]) {
    const time = day / 2 + (x - 400) / (800 / day);
    const id = Math.ceil(time / 10000 - 0.5) + 1;
    act(() => result.current.show(target(time, 1, 800 / day)));
    expect(result.current.state).toMatchObject({ event: { id }, imageSrc: `thumbnail-${id}`, source: 'thumbnail' });
    expect(fetchImage).not.toHaveBeenCalled();
    await advance(20);
    stoppedId = id;
  }
  expect((stoppedId - 1) % 60).not.toBe(0);
  await advance(100);
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(images).toHaveLength(1);
  expect(images[0].id).toBe(stoppedId);
  expect(result.current.state.event.id).toBe(stoppedId);
  await act(async () => images[0].resolve(`original-${stoppedId}`));
  expect(result.current.state).toMatchObject({ event: { id: stoppedId }, imageSrc: `original-${stoppedId}`, source: 'original' });
});

it('pins an already visible ID through index refresh and selects the updated index on the next movement', async () => {
  const events = [sampled(1, 5000)];
  fetchThumbnailBatch.mockResolvedValue({ 1: 'thumbnail-1', 2: 'thumbnail-2' });
  const { result } = renderHook(() => useTimelineHoverPreview(thumbnailProps(events)));
  await act(async () => {});
  act(() => result.current.show(target(6000)));
  await advance();
  await act(async () => images[0].resolve('original-1'));
  getTimelineIndex.mockResolvedValue([[1, 5000], [2, 6000]]);
  await advance(5000);
  expect(result.current.state).toMatchObject({ event: { id: 1 }, imageSrc: 'original-1' });
  expect(result.current.read(6000).id).toBe(1);
  act(() => result.current.hide());
  expect(result.current.read(6000).id).toBe(1);
  act(() => result.current.show(target(6001, 2)));
  expect(result.current.state).toMatchObject({ event: { id: 2 }, imageSrc: 'thumbnail-2' });
});
