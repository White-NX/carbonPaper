import React, { StrictMode } from 'react';
import { act, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Timeline from './Timeline';
import { getTimeline } from '../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('../lib/monitor_api', () => ({
  getTimeline: vi.fn(), getTimelineDensity: vi.fn(async () => []), fetchThumbnailBatch: vi.fn(async () => ({})),
}));
vi.mock('../hooks/useTimelineCamera', () => {
  const camera = { isFlying: () => false, setLive: () => {}, read: () => ({ zoom: 0.001 }) };
  return { default: () => camera };
});
vi.mock('./timeline/OverviewBand', () => ({ default: () => null }));
vi.mock('./timeline/SessionBand', () => ({ default: () => null }));
vi.mock('./timeline/SearchMarkerLayer', () => ({ default: () => null }));
vi.mock('./timeline/DetailTrack', () => ({
  default: ({ events }) => <div data-testid="events">{events.map((event) => event.windowTitle).join(',')}</div>,
}));

let reads;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-01-01T00:00:00Z'));
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(800);
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  reads = [];
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
