import React, { StrictMode } from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Timeline from './Timeline';
import WorkspaceTimelines from './WorkspaceTimelines';
import { fetchImage, fetchThumbnailBatch, getTimeline, getTimelineDetails, getTimelineDensity, getTimelineIndex, getTimelineNearest } from '../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('../lib/monitor_api', () => ({
  getTimeline: vi.fn(), getTimelineDetails: vi.fn(async () => []),
  getTimelineDensity: vi.fn(async () => []), fetchThumbnailBatch: vi.fn(async () => ({})),
  getTimelineNearest: vi.fn(), fetchImage: vi.fn(async () => 'data:image/png;base64,original'),
  getTimelineIndex: vi.fn(),
}));
vi.mock('../hooks/useTimelineCamera', async () => {
  const { useEffect, useMemo, useRef } = await import('react');
  return { default: function useCamera({ width, center, zoom, onCommit, onFrame }) {
    const view = useRef({ center, zoom });
    const trackWidth = useRef(width);
    trackWidth.current = width;
    const commit = useRef(onCommit);
    commit.current = onCommit;
    useEffect(() => {
      onFrame({ center, zoom, span: width / zoom, scale: 1, translate: 0 });
    }, [width, center, zoom, onFrame]);
    return useMemo(() => {
      const move = (nextCenter, nextZoom) => {
        view.current = { center: nextCenter, zoom: nextZoom };
        commit.current(view.current);
      };
      return {
        isFlying: () => false, setLive: () => {}, read: () => view.current, jumpTo: move, flyTo: move,
        panBy: (delta) => move(view.current.center - delta / view.current.zoom, view.current.zoom),
        zoomAt: (x, factor) => {
          const zoom = view.current.zoom * factor;
          const anchor = view.current.center + (x - trackWidth.current / 2) / view.current.zoom;
          move(anchor - (x - trackWidth.current / 2) / zoom, zoom);
        },
      };
    }, []);
  } };
});
vi.mock('./timeline/DensityBackdrop', () => ({
  default: ({ timeToX, width }) => {
    const zoom = (timeToX(Date.now() + 60000) - timeToX(Date.now())) / 60000;
    const start = Date.now() - timeToX(Date.now()) / zoom;
    return <div data-testid="viewport" data-start={start} data-end={start + width / zoom} />;
  },
}));
vi.mock('./timeline/SessionBand', () => ({
  default: ({ blocks }) => <div data-testid="events">{blocks.flatMap((block) => block.segments.map((segment) =>
    <span key={`${block.id}-${segment.start}`} data-timeline-item="session">{segment.windowTitle}</span>))}</div>,
}));
vi.mock('./timeline/SearchMarkerLayer', () => ({
  default: ({ markers = [], onSelectMarker }) => <div data-testid="markers">{markers.map((marker) => (
    <button key={marker.id} onClick={() => onSelectMarker?.(marker)}>{marker.id}</button>
  ))}</div>,
}));

let reads;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date('2026-01-01T00:00:00Z'));
  localStorage.clear();
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({ left: 0, top: 0, right: 800, bottom: 72, width: 800, height: 72 });
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  vi.stubGlobal('PointerEvent', class extends MouseEvent {
    constructor(type, options = {}) {
      super(type, options);
      this.pointerId = options.pointerId ?? 1;
      this.pointerType = options.pointerType || 'mouse';
    }
  });
  reads = [];
  getTimelineDetails.mockReset().mockResolvedValue([]);
  getTimelineIndex.mockReset().mockResolvedValue([]);
  fetchThumbnailBatch.mockReset().mockResolvedValue({});
  fetchImage.mockReset().mockResolvedValue('data:image/png;base64,original');
  getTimelineNearest.mockReset().mockResolvedValue({ id: 42, timestamp: 1767225660, process_name: 'app', window_title: 'actual nearest' });
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

it('loads labels and cached-only viewport thumbnails and reuses both on periodic header refresh', async () => {
  const details = [];
  getTimelineDetails.mockImplementation((ids, { signal }) => new Promise((resolve) => {
    details.push({ ids, signal, resolve });
  }));
  const headers = Array.from({ length: 13 }, (_, index) => ({
    id: 100 + index, timestamp: 1767225600 + index, window_title: null, process_name: null,
  }));
  getTimelineIndex.mockResolvedValue(headers.map((record) => [record.id, record.timestamp * 1000]));
  fetchThumbnailBatch.mockImplementation(async (ids) => Object.fromEntries(ids.map((id) => [id, `thumbnail-${id}`])));
  const view = render(<Timeline />);
  await advance();
  expect(getTimeline).toHaveBeenCalledWith(expect.any(Number), expect.any(Number), 500, {
    signal: expect.any(AbortSignal), includeDetails: false,
  });
  await act(async () => { reads[0].resolve(headers); });
  expect(fetchThumbnailBatch).toHaveBeenCalledTimes(4);
  for (const [ids, options] of fetchThumbnailBatch.mock.calls) {
    expect(ids.length).toBeLessThanOrEqual(4);
    expect(options.cachedOnly).toBe(true);
  }
  expect(fetchImage).not.toHaveBeenCalled();
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
  expect(fetchThumbnailBatch).toHaveBeenCalledTimes(4);
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

it('pauses hidden timeline reads and shortcuts, then resumes the existing viewport', async () => {
  const view = render(<Timeline />);
  fireEvent.keyDown(window, { key: '2' });
  await advance();
  const overview = screen.getByTestId('viewport');
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
  expect(screen.getByTestId('viewport').dataset.start).toBe(before.start);
  expect(screen.getByTestId('viewport').dataset.end).toBe(before.end);
  await advance();
  expect(reads).toHaveLength(2);
});

it('defers a source screenshot jump until the home timeline is visible', async () => {
  const jumpTimestamp = { time: Date.parse('2025-12-25T12:00:00Z') };
  const view = render(<Timeline active={false} jumpTimestamp={jumpTimestamp} />);
  expect(getTimeline).not.toHaveBeenCalled();
  view.rerender(<Timeline active jumpTimestamp={jumpTimestamp} />);
  const overview = screen.getByTestId('viewport');
  expect((Number(overview.dataset.start) + Number(overview.dataset.end)) / 2).toBe(jumpTimestamp.time);
  await advance();
  view.unmount();
  await act(async () => { reads[0].resolve([]); });
});

it.each([
  ['hidden', { active: false }, 300],
  ['compact', { compact: true }, 301],
])('cancels staged session details while the timeline is %s', async (_mode, pauseProps, id) => {
  const details = [];
  getTimelineDetails.mockImplementation((ids, { signal }) => new Promise((resolve) => {
    details.push({ ids, signal, resolve });
  }));
  const headers = [{ id, timestamp: 1767225600, window_title: null, process_name: null }];
  const view = render(<Timeline />);
  await advance();
  await act(async () => { reads[0].resolve(headers); });
  expect(details).toHaveLength(1);

  view.rerender(<Timeline {...pauseProps} />);
  expect(details[0].signal.aborted).toBe(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
  expect(reads).toHaveLength(1);
  expect(details).toHaveLength(1);
  view.rerender(<Timeline />);
  await advance();
  await act(async () => { reads[1].resolve(headers); });
  expect(details).toHaveLength(1);
  await act(async () => { details[0].resolve([{ id, window_title: 'stale detail' }]); });
  expect(screen.getByTestId('events')).not.toHaveTextContent('stale detail');
  expect(details).toHaveLength(2);
  await act(async () => { details[1].resolve([{ id, window_title: 'fresh detail' }]); });
  expect(screen.getByTestId('events')).toHaveTextContent('fresh detail');
  view.unmount();
});

it('keeps compact search markers selectable without reading screenshot history', () => {
  const result = { screenshot_id: 42 };
  const onSelect = vi.fn();
  render(<Timeline compact searchState={{ fitKey: 'query', markers: [{ id: 'result', time: Date.now(), result }] }} onSelectSearchResult={onSelect} />);
  expect(screen.getByRole('slider', { name: 'timeline.searchResultsTrack' })).toBeInTheDocument();
  expect(screen.queryByTestId('viewport')).not.toBeInTheDocument();
  expect(screen.queryByTestId('events')).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'timeline.today' })).not.toBeInTheDocument();
  expect(getTimeline).not.toHaveBeenCalled();
  expect(getTimelineDensity).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'timeline.searchResultAt' }));
  expect(onSelect).toHaveBeenCalledWith(result);
});

it('keeps the compact overview and camera position across expansion without hidden shortcuts', () => {
  const markerTime = Date.parse('2025-12-01T12:00:00Z');
  const searchState = { fitKey: 'query', markers: [
    { id: 'a', time: markerTime, result: { screenshot_id: 41 } },
    { id: 'b', time: markerTime + 3600000, result: { screenshot_id: 42 } },
  ] };
  const view = render(<Timeline compact searchState={searchState} />);
  const track = screen.getByRole('slider');
  fireEvent.change(track, { target: { value: '250' } });
  const selectedTime = track.getAttribute('aria-valuetext');
  fireEvent.keyDown(window, { key: '1' });
  expect(track).toHaveAttribute('aria-valuetext', selectedTime);

  view.rerender(<Timeline searchState={searchState} />);
  const overview = screen.getByTestId('viewport');
  const expandedCenter = (Number(overview.dataset.start) + Number(overview.dataset.end)) / 2;
  const range = { from: markerTime - 3600000 * 0.12, to: markerTime + 3600000 * 1.12 };
  expect(expandedCenter).toBeCloseTo(range.from + (range.to - range.from) * 0.25, 0);
  const viewport = { ...overview.dataset };
  view.rerender(<Timeline compact searchState={searchState} />);
  expect(screen.getByRole('slider')).toHaveAttribute('aria-valuetext', selectedTime);
  view.rerender(<Timeline searchState={searchState} />);
  expect(screen.getByTestId('viewport').dataset).toMatchObject(viewport);
});

it('shows an expand entry without a track or screenshot reads when compact search has no hits', () => {
  const onToggleCompact = vi.fn();
  render(<Timeline compact onToggleCompact={onToggleCompact} />);
  expect(screen.queryByRole('slider')).not.toBeInTheDocument();
  expect(screen.getByText('timeline.title')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'timeline.expand' }));
  expect(onToggleCompact).toHaveBeenCalledOnce();
  expect(getTimeline).not.toHaveBeenCalled();
  expect(getTimelineDensity).not.toHaveBeenCalled();
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
  const before = { ...within(home).getByTestId('viewport').dataset };
  view.rerender(<WorkspaceTimelines activeTab="advanced-search" searchState={searchState} />);
  const search = screen.getByRole('region', { name: 'timeline.label' });
  expect(search).not.toBe(home);
  expect(within(search).getByTestId('viewport').dataset.start).not.toBe(before.start);
  fireEvent.click(screen.getByRole('button', { name: 'timeline.collapse' }));
  expect(localStorage.getItem('searchTimelineCollapsed')).toBe('true');
  view.rerender(<WorkspaceTimelines activeTab="recap" searchState={searchState} />);
  expect(screen.queryByRole('region', { name: 'timeline.label' })).not.toBeInTheDocument();
  view.rerender(<WorkspaceTimelines activeTab="smart-cluster" searchState={searchState} />);
  expect(screen.queryByRole('region', { name: 'timeline.label' })).not.toBeInTheDocument();
  view.rerender(<WorkspaceTimelines activeTab="preview" searchState={searchState} />);
  expect(screen.getByRole('region', { name: 'timeline.label' })).toBe(home);
  expect(within(home).getByTestId('viewport').dataset.start).toBe(before.start);
  expect(within(home).getByTestId('viewport').dataset.end).toBe(before.end);
  expect(home).toHaveStyle({ height: '72px' });
  view.rerender(<WorkspaceTimelines activeTab="advanced-search" searchState={searchState} />);
  expect(screen.getByRole('button', { name: 'timeline.expand' })).toHaveAttribute('aria-expanded', 'false');
  expect(within(screen.getByRole('region', { name: 'timeline.label' })).getByRole('slider')).toBeInTheDocument();
});

it('selects the actual nearest snapshot on a bar click and moves the playhead to its time', async () => {
  const onSelect = vi.fn();
  render(<Timeline onSelectEvent={onSelect} />);
  await advance();
  await act(async () => reads[0].resolve(record('sampled event')));
  const track = screen.getByTestId('timeline-track');
  const viewport = { ...screen.getByTestId('viewport').dataset };
  fireEvent.pointerDown(track, { clientX: 510, clientY: 20, button: 0 });
  fireEvent.pointerUp(track, { clientX: 510, clientY: 20, button: 0 });
  await act(async () => fireEvent.click(screen.getByText('sampled event'), { clientX: 510, clientY: 20, button: 0 }));
  expect(getTimelineNearest).toHaveBeenCalledWith(1767225710000, { signal: expect.any(AbortSignal) });
  expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 42, timestamp: 1767225660000 }));
  expect(screen.getByTestId('timeline-playhead')).toHaveStyle({ left: '460px' });
  expect(screen.getByTestId('viewport').dataset).toMatchObject(viewport);
});

it('pans on a drag and suppresses the resulting click without changing the selected snapshot', async () => {
  const onSelect = vi.fn();
  render(<Timeline onSelectEvent={onSelect} highlightedEventId={1} />);
  await advance();
  await act(async () => reads[0].resolve(record('selected')));
  const track = screen.getByTestId('timeline-track');
  fireEvent.pointerDown(track, { clientX: 400, clientY: 20, button: 0 });
  fireEvent.pointerMove(track, { clientX: 450, clientY: 20 });
  fireEvent.pointerUp(track, { clientX: 450, clientY: 20, button: 0 });
  fireEvent.click(track, { clientX: 450, clientY: 20, button: 0 });
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(onSelect).not.toHaveBeenCalled();
  expect(screen.getByTestId('timeline-playhead')).toHaveStyle({ left: '450px' });
});

it('treats small pointer jitter as a click and keeps the viewport still', async () => {
  render(<Timeline />);
  const track = screen.getByTestId('timeline-track');
  const before = { ...screen.getByTestId('viewport').dataset };
  fireEvent.pointerDown(track, { clientX: 400, clientY: 20, button: 0 });
  fireEvent.pointerMove(track, { clientX: 402, clientY: 20 });
  expect(screen.getByTestId('viewport').dataset).toMatchObject(before);
  fireEvent.pointerUp(track, { clientX: 402, clientY: 20, button: 0 });
  await act(async () => fireEvent.click(track, { clientX: 402, clientY: 20, button: 0 }));
  expect(getTimelineNearest).toHaveBeenCalledOnce();
});

it('ends a captured drag and disables shortcuts while paused, then resumes ordinary hover', () => {
  const view = render(<Timeline />);
  const track = screen.getByTestId('timeline-track');
  fireEvent.pointerDown(track, { clientX: 400, clientY: 20, button: 0 });
  fireEvent.pointerMove(track, { clientX: 450, clientY: 20 });
  expect(track.className).toContain('cursor-grabbing');
  const before = { ...screen.getByTestId('viewport').dataset };
  view.rerender(<Timeline sqlPaused />);
  expect(track.className).toContain('cursor-grab');
  fireEvent.keyDown(window, { key: '2' });
  expect(screen.getByTestId('viewport').dataset).toMatchObject(before);
  view.rerender(<Timeline />);
  fireEvent.pointerMove(track, { clientX: 500, clientY: 20 });
  expect(screen.getByTestId('viewport').dataset).toMatchObject(before);
});

it('pans with touchpad scrolling and consumes pinch events without browser page zoom', () => {
  render(<Timeline />);
  const track = screen.getByTestId('timeline-track');
  const before = screen.getByTestId('viewport');
  const start = Number(before.dataset.start);
  const span = Number(before.dataset.end) - start;
  const scroll = new WheelEvent('wheel', { deltaX: 30, clientX: 400, bubbles: true, cancelable: true });
  act(() => track.dispatchEvent(scroll));
  expect(scroll.defaultPrevented).toBe(true);
  expect(Number(before.dataset.start)).toBe(start + 30000);
  expect(Number(before.dataset.end) - Number(before.dataset.start)).toBe(span);
  const pinch = new WheelEvent('wheel', { deltaY: -4, ctrlKey: true, clientX: 400, bubbles: true, cancelable: true });
  act(() => track.dispatchEvent(pinch));
  expect(pinch.defaultPrevented).toBe(true);
  expect(Number(before.dataset.end) - Number(before.dataset.start)).toBeCloseTo(span / Math.exp(0.04), 2);
});

it('does not let an obsolete click selection complete after the timeline becomes hidden', async () => {
  let finish;
  let signal;
  getTimelineNearest.mockImplementation((_time, options) => {
    signal = options.signal;
    return new Promise((resolve) => { finish = resolve; });
  });
  const onSelect = vi.fn();
  const view = render(<Timeline onSelectEvent={onSelect} />);
  fireEvent.click(screen.getByTestId('timeline-track'), { clientX: 510, clientY: 20, button: 0 });
  view.rerender(<Timeline active={false} onSelectEvent={onSelect} />);
  expect(signal.aborted).toBe(true);
  await act(async () => finish({ id: 42, timestamp: 1767225600 }));
  expect(onSelect).not.toHaveBeenCalled();
});

it('renders a viewport thumbnail before the exact original and reuses the image element when promoting it', async () => {
  const thumbnail = 'data:image/jpeg;base64,thumbnail';
  getTimelineIndex.mockResolvedValue([[1, 1767225600000], [42, 1767225602000]]);
  getTimelineDetails.mockImplementation(async (ids) => ids.includes(42)
    ? [{ id: 42, timestamp: 1767225602, process_name: 'app', window_title: 'actual nearest' }] : []);
  fetchThumbnailBatch.mockResolvedValue({ 42: thumbnail });
  let finishImage;
  fetchImage.mockImplementationOnce(() => new Promise((resolve) => { finishImage = resolve; }));
  render(<Timeline />);
  await advance();
  await act(async () => reads[0].resolve(record('sampled event')));
  fireEvent.pointerMove(screen.getByTestId('timeline-track'), { clientX: 402, clientY: 20 });
  const image = screen.getByRole('img', { name: 'timeline.snapshotPreview' });
  expect(image).toHaveAttribute('src', thumbnail);
  expect(image).toHaveAttribute('data-preview-source', 'thumbnail');
  expect(fetchImage).not.toHaveBeenCalled();
  await act(async () => vi.advanceTimersByTimeAsync(100));
  expect(screen.getByRole('tooltip')).not.toHaveTextContent('sampled event');
  expect(fetchImage).toHaveBeenCalledWith(42);
  expect(getTimelineNearest).not.toHaveBeenCalled();
  await act(async () => finishImage('data:image/png;base64,original'));
  expect(screen.getByRole('img', { name: 'timeline.snapshotPreview' })).toBe(image);
  expect(image).toHaveAttribute('data-preview-source', 'original');
  expect(screen.getByRole('tooltip')).toHaveTextContent('actual nearest');
});

it('opens the exact cached hover snapshot on a quick click without another nearest lookup', async () => {
  const onSelect = vi.fn();
  getTimelineIndex.mockResolvedValue([[1, 1767225600000], [42, 1767225602000]]);
  fetchThumbnailBatch.mockResolvedValue({ 42: 'data:image/jpeg;base64,thumbnail' });
  render(<Timeline onSelectEvent={onSelect} />);
  await advance();
  await act(async () => reads[0].resolve(record('sampled event')));
  const track = screen.getByTestId('timeline-track');
  fireEvent.pointerMove(track, { clientX: 402, clientY: 20 });
  expect(screen.getByRole('img', { name: 'timeline.snapshotPreview' })).toHaveAttribute('src', 'data:image/jpeg;base64,thumbnail');
  fireEvent.pointerDown(track, { clientX: 402, clientY: 20, button: 0 });
  fireEvent.pointerUp(track, { clientX: 402, clientY: 20, button: 0 });
  fireEvent.click(track, { clientX: 402, clientY: 20, button: 0 });
  expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 42, timestamp: 1767225602000 }));
  expect(getTimelineNearest).not.toHaveBeenCalled();
  expect(fetchImage).not.toHaveBeenCalled();
  expect(screen.getByTestId('timeline-playhead')).toHaveStyle({ left: '402px' });
});

it('replaces the previous image element immediately when a sweep chooses another snapshot ID', async () => {
  getTimelineIndex.mockResolvedValue([[1, 1767225600000], [42, 1767225602000]]);
  fetchThumbnailBatch.mockResolvedValue({ 1: 'data:image/jpeg;base64,first', 42: 'data:image/jpeg;base64,second' });
  render(<Timeline />);
  await advance();
  await act(async () => reads[0].resolve(record('sampled event')));
  const track = screen.getByTestId('timeline-track');
  fireEvent.pointerMove(track, { clientX: 400, clientY: 20 });
  const first = screen.getByRole('img', { name: 'timeline.snapshotPreview' });
  expect(first).toHaveAttribute('data-preview-id', '1');
  fireEvent.pointerMove(track, { clientX: 402, clientY: 20 });
  const second = screen.getByRole('img', { name: 'timeline.snapshotPreview' });
  expect(first).not.toBeInTheDocument();
  expect(second).toHaveAttribute('data-preview-id', '42');
  expect(second).toHaveAttribute('src', 'data:image/jpeg;base64,second');
});

it('keeps a long stationary preview as the click target after the index has refreshed', async () => {
  const onSelect = vi.fn();
  getTimelineIndex.mockResolvedValue([[1, 1767225600000]]);
  fetchThumbnailBatch.mockResolvedValue({ 1: 'data:image/jpeg;base64,first', 42: 'data:image/jpeg;base64,second' });
  render(<Timeline onSelectEvent={onSelect} />);
  await advance();
  await act(async () => reads[0].resolve(record('sampled event')));
  const track = screen.getByTestId('timeline-track');
  fireEvent.pointerMove(track, { clientX: 402, clientY: 20 });
  getTimelineIndex.mockResolvedValue([[1, 1767225600000], [42, 1767225602000]]);
  await act(async () => vi.advanceTimersByTimeAsync(5000));
  expect(screen.getByRole('img', { name: 'timeline.snapshotPreview' })).toHaveAttribute('data-preview-id', '1');
  fireEvent.pointerDown(track, { clientX: 402, clientY: 20, button: 0 });
  fireEvent.pointerUp(track, { clientX: 402, clientY: 20, button: 0 });
  fireEvent.click(track, { clientX: 402, clientY: 20, button: 0 });
  expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ id: 1, timestamp: 1767225600000 }));
  expect(getTimelineNearest).not.toHaveBeenCalled();
});
