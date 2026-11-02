import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useVisualSearchController } from './useVisualSearchController';
import { fetchThumbnailBatch, searchScreenshots } from '../lib/monitor_api';

vi.mock('../lib/monitor_api', () => ({ searchScreenshots: vi.fn(), fetchThumbnailBatch: vi.fn() }));

const item = (id) => ({ id: `clip-${id}`, image_path: `memory://${id}`,
  metadata: { screenshot_id: id, process_name: 'code.exe', created_at: '2026-10-31T00:00:00Z' } });
const page = (offset = 0) => Array.from({ length: 10 }, (_, index) => item(offset + index + 1));
const galleryPage = (offset = 0) => Array.from({ length: 50 }, (_, index) => item(offset + index + 1));
const props = () => ({ active: true, query: 'moon', debouncedQuery: 'moon', processNames: [],
  categories: [], startTime: null, endTime: null, refreshKey: 0 });

describe('visual search controller', () => {
  beforeEach(() => {
    searchScreenshots.mockReset().mockResolvedValue([]);
    fetchThumbnailBatch.mockReset().mockImplementation(async (ids) => Object.fromEntries(ids.map((id) => [id, `thumbnail-${id}`])));
  });

  it('starts with ten results and preloads their thumbnails', async () => {
    searchScreenshots.mockResolvedValue(page());
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    expect(result.current.summaryResults).toHaveLength(10);
    expect(result.current.results[0]).toEqual(expect.objectContaining({ id: 1, screenshot_id: 1, process_name: 'code.exe' }));
    expect(searchScreenshots).toHaveBeenCalledWith('moon', 'nl', expect.objectContaining({ limit: 10, offset: 0, includeOcr: false }));
    await waitFor(() => expect(fetchThumbnailBatch).toHaveBeenCalledWith(
      Array.from({ length: 10 }, (_, index) => index + 1), { signal: expect.any(AbortSignal) }));
  });

  it('expands the summary by ten only once while retaining shared filters', async () => {
    searchScreenshots.mockImplementation(async (_query, _mode, options) => page(options.offset));
    const options = { ...props(), categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200 };
    const { result } = renderHook(useVisualSearchController, { initialProps: options });
    await waitFor(() => expect(result.current.summaryResults).toHaveLength(10));
    expect(result.current.summaryExpanded).toBe(false);
    act(() => result.current.showMore());
    await waitFor(() => expect(result.current.summaryResults).toHaveLength(20));
    expect(result.current.summaryExpanded).toBe(true);
    expect(searchScreenshots).toHaveBeenLastCalledWith('moon', 'nl', expect.objectContaining({
      offset: 10, limit: 10, categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200,
    }));
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(20);
    expect(searchScreenshots.mock.calls.map(([, , options]) => options.offset)).toEqual([0, 10]);
  });

  it('keeps gallery paging separate from the summary and reuses its cached pages', async () => {
    searchScreenshots.mockImplementation(async (_query, _mode, options) => page(options.offset));
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    await act(async () => result.current.loadMore());
    await act(async () => result.current.loadMore());
    expect(result.current.results).toHaveLength(30);
    expect(result.current.summaryResults).toHaveLength(10);
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(20);
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(20);
    expect(searchScreenshots).toHaveBeenCalledTimes(3);
  });

  it('loads fifty matches on gallery entry and fifty per page while keeping the summary at ten', async () => {
    searchScreenshots.mockImplementation(async (_query, _mode, options) => options.limit === 50 ? galleryPage(options.offset) : page(options.offset));
    const options = { ...props(), categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200 };
    const { result } = renderHook(useVisualSearchController, { initialProps: options });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    expect(result.current.galleryReady).toBe(false);
    await act(async () => result.current.loadGalleryPage());
    expect(result.current.results).toHaveLength(50);
    expect(result.current.summaryResults).toHaveLength(10);
    expect(result.current.galleryReady).toBe(true);
    await act(async () => result.current.loadGalleryPage());
    expect(result.current.results).toHaveLength(100);
    expect(result.current.summaryResults).toHaveLength(10);
    expect(searchScreenshots.mock.calls.map(([, , request]) => [request.offset, request.limit])).toEqual([[0, 10], [0, 50], [50, 50]]);
    expect(searchScreenshots).toHaveBeenLastCalledWith('moon', 'nl', expect.objectContaining({
      categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200, includeOcr: false,
    }));
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(20);
    expect(searchScreenshots).toHaveBeenCalledTimes(3);
  });

  it('retains the summary after a gallery entry failure and retries the full fifty-result page', async () => {
    searchScreenshots.mockResolvedValueOnce(page()).mockRejectedValueOnce(new Error('gallery unavailable'))
      .mockResolvedValueOnce(galleryPage()).mockResolvedValueOnce([item(50), item(51)]);
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    await act(async () => result.current.loadGalleryPage());
    expect(result.current.pageError).toBe(true);
    expect(result.current.summaryResults).toHaveLength(10);
    await act(async () => result.current.loadGalleryPage());
    expect(result.current.results).toHaveLength(50);
    expect(result.current.pageError).toBe(false);
    await act(async () => result.current.loadGalleryPage());
    expect(result.current.results).toHaveLength(51);
    expect(result.current.offset).toBe(52);
    expect(result.current.hasMore).toBe(false);
    expect(searchScreenshots.mock.calls.map(([, , request]) => [request.offset, request.limit])).toEqual([[0, 10], [0, 50], [0, 50], [50, 50]]);
  });

  it('reuses a complete short summary without requesting another gallery page', async () => {
    searchScreenshots.mockResolvedValue([item(1)]);
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    expect(result.current.galleryReady).toBe(true);
    await act(async () => result.current.loadGalleryPage());
    expect(searchScreenshots).toHaveBeenCalledTimes(1);
  });

  it('deduplicates matches while advancing by the raw page offset', async () => {
    searchScreenshots.mockResolvedValueOnce(page()).mockResolvedValueOnce([item(10), item(11)]);
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    await act(async () => result.current.loadMore());
    expect(result.current.results).toHaveLength(11);
    expect(result.current.offset).toBe(12);
    expect(result.current.hasMore).toBe(false);
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(11);
  });

  it('reuses cached pictures and retries a page error without expanding beyond twenty', async () => {
    searchScreenshots.mockResolvedValueOnce(page()).mockResolvedValueOnce(page(10))
      .mockRejectedValueOnce(new Error('gallery page failed')).mockResolvedValueOnce(page(20));
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    await act(async () => result.current.loadMore());
    await act(async () => result.current.loadMore());
    expect(result.current.pageError).toBe(true);
    expect(result.current.summaryPageError).toBe(false);
    act(() => result.current.showMore());
    expect(result.current.summaryResults).toHaveLength(20);
    expect(searchScreenshots).toHaveBeenCalledTimes(3);
    expect(result.current.summaryPageError).toBe(true);
    act(() => result.current.showMore());
    await waitFor(() => expect(result.current.results).toHaveLength(30));
    expect(result.current.summaryResults).toHaveLength(20);
    expect(result.current.summaryPageError).toBe(false);
    expect(searchScreenshots.mock.calls.map(([, , options]) => options.offset)).toEqual([0, 10, 20, 20]);
  });

  it('reuses text-result thumbnails and clears them at a session boundary', async () => {
    searchScreenshots.mockResolvedValue([item(1)]);
    const initial = { ...props(), sharedThumbnails: { 1: 'shared thumbnail' } };
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    expect(result.current.thumbnails[1]).toBe('shared thumbnail');
    expect(fetchThumbnailBatch).not.toHaveBeenCalled();
    rerender({ ...initial, active: false });
    expect(result.current.thumbnails).toEqual({});
  });

  it('drops intermediate queued queries and never applies an obsolete result', async () => {
    let finishOld;
    searchScreenshots.mockImplementationOnce(() => new Promise((resolve) => { finishOld = resolve; }));
    const initial = props();
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(finishOld).toEqual(expect.any(Function)));
    rerender({ ...initial, query: 'middle', debouncedQuery: 'middle' });
    rerender({ ...initial, query: 'newest', debouncedQuery: 'newest' });
    searchScreenshots.mockResolvedValue([item(99)]);
    expect(searchScreenshots).toHaveBeenCalledTimes(1);
    await act(async () => finishOld([item(1)]));
    await waitFor(() => expect(result.current.results[0]?.screenshot_id).toBe(99));
    expect(searchScreenshots.mock.calls.map(([query]) => query)).toEqual(['moon', 'newest']);
    expect(fetchThumbnailBatch.mock.calls.flatMap(([ids]) => ids)).not.toContain(1);
  });

  it.each(['clear', 'lock'])('clears private results and ignores a late thumbnail batch on %s', async (action) => {
    let finishThumbnail;
    searchScreenshots.mockResolvedValue([item(1)]);
    fetchThumbnailBatch.mockImplementationOnce(() => new Promise((resolve) => { finishThumbnail = resolve; }));
    const initial = props();
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(finishThumbnail).toEqual(expect.any(Function)));
    rerender(action === 'clear' ? { ...initial, query: '', debouncedQuery: 'moon' } : { ...initial, active: false });
    expect(result.current.results).toEqual([]);
    expect(result.current.thumbnails).toEqual({});
    await act(async () => finishThumbnail({ 1: 'private thumbnail' }));
    expect(result.current.thumbnails).toEqual({});
  });

  it('retains the first page after a paging failure and retries only that page', async () => {
    searchScreenshots.mockResolvedValueOnce(page()).mockRejectedValueOnce(new Error('temporary failure')).mockResolvedValueOnce([item(11)]);
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    act(() => result.current.showMore());
    await waitFor(() => expect(result.current.pageError).toBe(true));
    expect(result.current.summaryResults).toHaveLength(10);
    expect(result.current.summaryExpanded).toBe(true);
    expect(result.current.pageErrorMessage).toBe('temporary failure');
    act(() => result.current.retry());
    await waitFor(() => expect(result.current.summaryResults).toHaveLength(11));
    expect(result.current.pageError).toBe(false);
    expect(result.current.pageErrorMessage).toBeNull();
    expect(searchScreenshots.mock.calls.map(([, , options]) => options.offset)).toEqual([0, 10, 10]);
  });

  it('resets the summary to ten on a new query and discards a pending gallery page', async () => {
    let finishPage;
    searchScreenshots.mockResolvedValueOnce(page()).mockResolvedValueOnce(page(10))
      .mockImplementationOnce(() => new Promise(resolve => { finishPage = resolve; }))
      .mockResolvedValueOnce(page(100));
    const initial = props();
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(result.current.results).toHaveLength(10));
    act(() => result.current.showMore());
    await waitFor(() => expect(result.current.summaryResults).toHaveLength(20));
    expect(result.current.summaryExpanded).toBe(true);
    act(() => { void result.current.loadGalleryPage(); });
    await waitFor(() => expect(finishPage).toEqual(expect.any(Function)));
    rerender({ ...initial, query: 'sun', debouncedQuery: 'sun' });
    expect(result.current.results).toEqual([]);
    await act(async () => finishPage(page(10)));
    await waitFor(() => expect(result.current.results[0]?.screenshot_id).toBe(101));
    expect(result.current.summaryResults).toHaveLength(10);
    expect(result.current.summaryExpanded).toBe(false);
    expect(result.current.results.map(item => item.screenshot_id)).not.toContain(11);
  });

  it('keeps the gallery identity stable on retry and starts fresh requests on refresh', async () => {
    searchScreenshots.mockRejectedValueOnce(new Error('temporarily unavailable')).mockResolvedValue([]);
    const initial = props();
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(result.current.phase).toBe('error'));
    const searchKey = result.current.searchKey;
    act(() => result.current.retry());
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    expect(result.current.searchKey).toBe(searchKey);
    rerender({ ...initial, refreshKey: 1 });
    await waitFor(() => expect(searchScreenshots).toHaveBeenCalledTimes(3));
    rerender({ ...initial, active: false });
    rerender({ ...initial, query: '', debouncedQuery: '' });
    expect(searchScreenshots).toHaveBeenCalledTimes(3);
  });
});
