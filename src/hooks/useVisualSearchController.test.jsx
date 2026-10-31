import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useVisualSearchController } from './useVisualSearchController';
import { fetchThumbnailBatch, searchScreenshots } from '../lib/monitor_api';

vi.mock('../lib/monitor_api', () => ({
  searchScreenshots: vi.fn(), fetchThumbnailBatch: vi.fn(),
}));

const item = (id) => ({ id: `clip-${id}`, image_path: `memory://${id}`,
  metadata: { screenshot_id: id, process_name: 'code.exe', created_at: '2026-10-31T00:00:00Z' } });
const props = () => ({ active: true, query: 'moon', debouncedQuery: 'moon', processNames: [],
  categories: [], startTime: null, endTime: null, refreshKey: 0 });

describe('visual search controller', () => {
  beforeEach(() => {
    searchScreenshots.mockReset().mockResolvedValue([]);
    fetchThumbnailBatch.mockReset().mockImplementation(async (ids) => Object.fromEntries(ids.map((id) => [id, `thumbnail-${id}`])));
  });

  it('fetches a small collapsed preview and only three thumbnails', async () => {
    searchScreenshots.mockResolvedValue(Array.from({ length: 6 }, (_, index) => item(index + 1)));
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    expect(result.current.expanded).toBe(false);
    expect(result.current.results[0]).toEqual(expect.objectContaining({ id: 1, screenshot_id: 1, process_name: 'code.exe' }));
    expect(searchScreenshots).toHaveBeenCalledWith('moon', 'nl', expect.objectContaining({ limit: 6, offset: 0, includeOcr: false }));
    await waitFor(() => expect(fetchThumbnailBatch).toHaveBeenCalledWith([1, 2, 3], { signal: expect.any(AbortSignal) }));
  });

  it('expands with a separate page while retaining shared filters', async () => {
    searchScreenshots.mockImplementation(async (_query, _mode, options) => options.offset === 0
      ? Array.from({ length: 6 }, (_, index) => item(index + 1)) : [item(7), item(8)]);
    const options = { ...props(), categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200 };
    const { result } = renderHook(useVisualSearchController, { initialProps: options });
    await waitFor(() => expect(result.current.results).toHaveLength(6));
    act(() => result.current.setExpanded(true));
    await waitFor(() => expect(result.current.results).toHaveLength(8));
    expect(searchScreenshots).toHaveBeenLastCalledWith('moon', 'nl', expect.objectContaining({
      offset: 6, limit: 24, categories: ['development'], processNames: ['code.exe'], startTime: 100, endTime: 200,
    }));
    expect(result.current.hasMore).toBe(false);
    act(() => result.current.setExpanded(false));
    act(() => result.current.setExpanded(true));
    expect(searchScreenshots).toHaveBeenCalledTimes(2);
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

  it('retains the first page after a paging failure and retries only the page', async () => {
    searchScreenshots.mockResolvedValueOnce(Array.from({ length: 6 }, (_, index) => item(index + 1)))
      .mockRejectedValueOnce(new Error('temporary failure')).mockResolvedValueOnce([item(7)]);
    const { result } = renderHook(useVisualSearchController, { initialProps: props() });
    await waitFor(() => expect(result.current.results).toHaveLength(6));
    act(() => result.current.setExpanded(true));
    await waitFor(() => expect(result.current.pageError).toBe(true));
    expect(result.current.results).toHaveLength(6);
    act(() => result.current.retry());
    await waitFor(() => expect(result.current.results).toHaveLength(7));
    expect(searchScreenshots.mock.calls.map(([, , options]) => options.offset)).toEqual([0, 6, 6]);
  });

  it('starts one fresh request on explicit refresh and skips blank or inactive searches', async () => {
    const initial = props();
    const { result, rerender } = renderHook(useVisualSearchController, { initialProps: initial });
    await waitFor(() => expect(result.current.phase).toBe('ready'));
    rerender({ ...initial, refreshKey: 1 });
    await waitFor(() => expect(searchScreenshots).toHaveBeenCalledTimes(2));
    rerender({ ...initial, active: false });
    rerender({ ...initial, query: '', debouncedQuery: '' });
    expect(searchScreenshots).toHaveBeenCalledTimes(2);
  });
});
