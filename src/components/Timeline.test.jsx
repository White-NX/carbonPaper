import React, { StrictMode } from 'react';
import { act, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Timeline from './Timeline';
import { fetchThumbnailBatch, getTimeline, getTimelineDetails } from '../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('../lib/monitor_api', () => ({
  getTimeline: vi.fn(), getTimelineDetails: vi.fn(async () => []),
  getTimelineDensity: vi.fn(async () => []), fetchThumbnailBatch: vi.fn(async () => ({})),
}));
vi.mock('../hooks/useTimelineCamera', () => {
  const camera = { isFlying: () => false, setLive: () => {}, read: () => ({ zoom: 0.001 }) };
  return { default: function useTimelineCameraMock({ width, center, zoom, onFrame }) {
    React.useEffect(() => {
      onFrame({ center, span: width / zoom, scale: 1, translate: 0 });
    }, [width, center, zoom, onFrame]);
    return camera;
  } };
});
vi.mock('./timeline/OverviewBand', () => ({ default: () => null }));
vi.mock('./timeline/SessionBand', () => ({ default: () => null }));
vi.mock('./timeline/SearchMarkerLayer', () => ({ default: () => null }));
vi.mock('./timeline/DetailTrack', () => ({
  default: function MockDetailTrack({ events, tier, imageCache, onVisibleFramesChange }) {
    React.useEffect(() => {
      if (tier === 'frame') onVisibleFramesChange(events.map((event) => event.id));
    }, [events, tier, onVisibleFramesChange]);
    return <div data-testid="events">
      {events.map((event) => <React.Fragment key={event.id}>
        <span>{event.windowTitle}</span>
        {imageCache.has(event.id) && <img alt="" src={imageCache.get(event.id)} />}
      </React.Fragment>)}
    </div>;
  },
}));

let reads;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-01-01T00:00:00Z'));
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(800);
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  reads = [];
  getTimelineDetails.mockReset().mockResolvedValue([]);
  fetchThumbnailBatch.mockReset().mockResolvedValue({});
  getTimeline.mockImplementation((_start, _end, _limit, { signal }) => new Promise((resolve) => {
    reads.push({ signal, resolve });
  }));
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const record = (title) => [{ id: 1, timestamp: 1767225600, process_name: 'app', window_title: title }];
const advance = () => act(async () => { await vi.advanceTimersByTimeAsync(350); });

it('discards a reset read and loads the same viewport again after cancellation settles', async () => {
  const view = render(<StrictMode><Timeline /></StrictMode>);
  await advance();
  expect(reads).toHaveLength(1);
  view.rerender(<StrictMode><Timeline refreshKey={1} /></StrictMode>);
  await advance();
  expect(reads[0].signal.aborted).toBe(true);
  expect(reads).toHaveLength(1);
  await act(async () => { reads[0].resolve(record('stale')); });
  expect(reads).toHaveLength(2);
  expect(screen.getByTestId('events')).not.toHaveTextContent('stale');
  await act(async () => { reads[1].resolve(record('fresh')); });
  expect(screen.getByTestId('events')).toHaveTextContent('fresh');
  view.unmount();
});

it('cancels during SQL pause and resumes without believing the cancelled range is loaded', async () => {
  const view = render(<Timeline />);
  await advance();
  view.rerender(<Timeline sqlPaused />);
  expect(reads[0].signal.aborted).toBe(true);
  await act(async () => { reads[0].resolve(record('stale')); });
  await advance();
  expect(reads).toHaveLength(1);
  expect(screen.getByTestId('events')).not.toHaveTextContent('stale');
  view.rerender(<Timeline sqlPaused={false} />);
  await advance();
  expect(reads).toHaveLength(2);
  view.unmount();
  expect(reads[1].signal.aborted).toBe(true);
  await act(async () => { reads[1].resolve([]); });
});

it('paints progressive thumbnails before loading labels and reuses labels on periodic header refresh', async () => {
  const thumbnails = [];
  const details = [];
  fetchThumbnailBatch.mockImplementation((ids, { signal }) => new Promise((resolve) => {
    thumbnails.push({ ids, signal, resolve });
  }));
  getTimelineDetails.mockImplementation((ids, { signal }) => new Promise((resolve) => {
    details.push({ ids, signal, resolve });
  }));
  const headers = Array.from({ length: 13 }, (_, index) => ({
    id: 100 + index, timestamp: 1767225600 + index, window_title: null, process_name: null,
  }));
  const view = render(<Timeline />);
  await advance();
  expect(getTimeline).toHaveBeenCalledWith(expect.any(Number), expect.any(Number), 500, {
    signal: expect.any(AbortSignal), includeDetails: false,
  });
  await act(async () => { reads[0].resolve(headers); });
  expect(thumbnails[0].ids).toHaveLength(6);
  expect(details).toHaveLength(0);

  for (let index = 0; index < 3; index += 1) {
    const batch = thumbnails[index];
    await act(async () => {
      batch.resolve(Object.fromEntries(batch.ids.map((id) => [id, `data:image/jpeg;base64,${id}`])));
    });
    expect(screen.getByTestId('events').querySelectorAll('img')).toHaveLength(Math.min((index + 1) * 6, 13));
    if (index < 2) expect(details).toHaveLength(0);
  }
  expect(details[0].ids).toHaveLength(12);
  for (let index = 0; index < 2; index += 1) {
    const batch = details[index];
    await act(async () => {
      batch.resolve(batch.ids.map((id) => ({ id, process_name: 'app', window_title: `title-${id}` })));
    });
  }
  expect(screen.getByTestId('events')).toHaveTextContent('title-112');
  await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
  expect(reads).toHaveLength(2);
  await act(async () => { reads[1].resolve(headers); });
  expect(screen.getByTestId('events')).toHaveTextContent('title-112');
  expect(details).toHaveLength(2);
  expect(thumbnails).toHaveLength(3);
  view.unmount();
});

it('discards details from before an explicit reset and after unmount', async () => {
  const details = [];
  fetchThumbnailBatch.mockImplementation(async (ids) => Object.fromEntries(ids.map((id) => [id, 'data:image/jpeg;base64,image'])));
  getTimelineDetails.mockImplementation((ids, { signal }) => new Promise((resolve) => {
    details.push({ ids, signal, resolve });
  }));
  const headers = [{ id: 200, timestamp: 1767225600, window_title: null }];
  const view = render(<Timeline />);
  await advance();
  await act(async () => { reads[0].resolve(headers); });
  expect(details).toHaveLength(1);
  view.rerender(<Timeline refreshKey={1} />);
  await advance();
  expect(details[0].signal.aborted).toBe(true);
  await act(async () => { reads[1].resolve(headers); });
  expect(details).toHaveLength(1);
  await act(async () => { details[0].resolve([{ id: 200, window_title: 'stale detail' }]); });
  expect(screen.getByTestId('events')).not.toHaveTextContent('stale detail');
  expect(details).toHaveLength(2);
  view.unmount();
  expect(details[1].signal.aborted).toBe(true);
  await act(async () => { details[1].resolve([{ id: 200, window_title: 'unmounted detail' }]); });
});
