import React, { StrictMode } from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Timeline from './Timeline';
import WorkspaceTimelines from './WorkspaceTimelines';
import { getTimeline, getTimelineDensity } from '../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('../lib/monitor_api', () => ({
  getTimeline: vi.fn(), getTimelineDensity: vi.fn(async () => []), fetchThumbnailBatch: vi.fn(async () => ({})),
}));
vi.mock('../hooks/useTimelineCamera', async () => {
  const { useMemo, useRef } = await import('react');
  return { default: function useCamera({ center, zoom, onCommit }) {
    const view = useRef({ center, zoom });
    const commit = useRef(onCommit);
    commit.current = onCommit;
    return useMemo(() => {
      const move = (nextCenter, nextZoom) => {
        view.current = { center: nextCenter, zoom: nextZoom };
        commit.current(view.current);
      };
      return { isFlying: () => false, setLive: () => {}, read: () => view.current, jumpTo: move, flyTo: move };
    }, []);
  } };
});
vi.mock('./timeline/OverviewBand', () => ({
  default: ({ viewStart, viewEnd }) => <div data-testid="overview" data-start={viewStart} data-end={viewEnd} />,
}));
vi.mock('./timeline/SessionBand', () => ({ default: () => null }));
vi.mock('./timeline/SearchMarkerLayer', () => ({
  default: ({ markers = [], onSelectMarker }) => <div data-testid="markers">{markers.map((marker) => (
    <button key={marker.id} onClick={() => onSelectMarker?.(marker)}>{marker.id}</button>
  ))}</div>,
}));
vi.mock('./timeline/DetailTrack', () => ({
  default: ({ events }) => <div data-testid="events">{events.map((event) => event.windowTitle).join(',')}</div>,
}));

let reads;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-01-01T00:00:00Z'));
  localStorage.clear();
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

it('pauses hidden timeline reads and shortcuts, then resumes the existing viewport', async () => {
  const view = render(<Timeline />);
  fireEvent.keyDown(window, { key: '2' });
  await advance();
  const overview = screen.getByTestId('overview');
  const before = { start: overview.dataset.start, end: overview.dataset.end };
  const densityReads = getTimelineDensity.mock.calls.length;
  view.rerender(<Timeline active={false} />);
  expect(screen.queryByRole('region', { name: 'timeline.label' })).not.toBeInTheDocument();
  expect(reads[0].signal.aborted).toBe(true);
  await act(async () => { reads[0].resolve(record('stale')); });
  fireEvent.keyDown(window, { key: '1' });
  await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
  expect(reads).toHaveLength(1);
  expect(getTimelineDensity).toHaveBeenCalledTimes(densityReads);
  view.rerender(<Timeline active />);
  expect(screen.getByTestId('overview').dataset.start).toBe(before.start);
  expect(screen.getByTestId('overview').dataset.end).toBe(before.end);
  await advance();
  expect(reads).toHaveLength(2);
});

it('defers a source screenshot jump until the home timeline is visible', () => {
  const jumpTimestamp = { time: Date.parse('2025-12-25T12:00:00Z') };
  const view = render(<Timeline active={false} jumpTimestamp={jumpTimestamp} />);
  expect(getTimeline).not.toHaveBeenCalled();
  view.rerender(<Timeline active jumpTimestamp={jumpTimestamp} />);
  const overview = screen.getByTestId('overview');
  expect((Number(overview.dataset.start) + Number(overview.dataset.end)) / 2).toBe(jumpTimestamp.time);
});

it('keeps compact search markers selectable without reading screenshot history', () => {
  const result = { screenshot_id: 42 };
  const onSelect = vi.fn();
  render(<Timeline compact searchState={{ fitKey: 'query', markers: [{ id: 'result', time: Date.now(), result }] }} onSelectSearchResult={onSelect} />);
  expect(screen.getByRole('region', { name: 'timeline.label' })).toHaveStyle({ height: '64px' });
  expect(getTimeline).not.toHaveBeenCalled();
  expect(getTimelineDensity).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'result' }));
  expect(onSelect).toHaveBeenCalledWith(result);
});

it('exposes the interactive marker layer to keyboard and assistive-technology users', async () => {
  const { default: SearchMarkerLayer } = await vi.importActual('./timeline/SearchMarkerLayer');
  const marker = { id: 'source', time: Date.now(), result: { screenshot_id: 42 } };
  const onSelect = vi.fn();
  render(<SearchMarkerLayer markers={[marker]} width={100} height={20} timeToX={() => 50} onSelectMarker={onSelect} />);
  fireEvent.click(screen.getByRole('button'));
  expect(onSelect).toHaveBeenCalledWith(marker);
});

it('preserves the home viewport and search collapse preference across page changes', () => {
  const searchState = { fitKey: 'query', markers: [{ id: 'hit', time: Date.parse('2025-12-01T12:00:00Z'), result: { screenshot_id: 42 } }] };
  const view = render(<WorkspaceTimelines activeTab="preview" searchState={searchState} />);
  fireEvent.keyDown(window, { key: '2' });
  const home = screen.getByRole('region', { name: 'timeline.label' });
  const before = { ...within(home).getByTestId('overview').dataset };
  view.rerender(<WorkspaceTimelines activeTab="advanced-search" searchState={searchState} />);
  const search = screen.getByRole('region', { name: 'timeline.label' });
  expect(search).not.toBe(home);
  expect(within(search).getByTestId('overview').dataset.start).not.toBe(before.start);
  fireEvent.click(screen.getByRole('button', { name: 'timeline.collapse' }));
  expect(localStorage.getItem('searchTimelineCollapsed')).toBe('true');
  view.rerender(<WorkspaceTimelines activeTab="recap" searchState={searchState} />);
  expect(screen.queryByRole('region', { name: 'timeline.label' })).not.toBeInTheDocument();
  view.rerender(<WorkspaceTimelines activeTab="smart-cluster" searchState={searchState} />);
  expect(screen.queryByRole('region', { name: 'timeline.label' })).not.toBeInTheDocument();
  view.rerender(<WorkspaceTimelines activeTab="preview" searchState={searchState} />);
  expect(screen.getByRole('region', { name: 'timeline.label' })).toBe(home);
  expect(within(home).getByTestId('overview').dataset.start).toBe(before.start);
  expect(within(home).getByTestId('overview').dataset.end).toBe(before.end);
  expect(home).toHaveStyle({ height: '126px' });
  view.rerender(<WorkspaceTimelines activeTab="advanced-search" searchState={searchState} />);
  expect(screen.getByRole('button', { name: 'timeline.expand' })).toHaveAttribute('aria-expanded', 'false');
  expect(screen.getByRole('region', { name: 'timeline.label' })).toHaveStyle({ height: '64px' });
});
