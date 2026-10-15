import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import * as api from '../lib/recap_api';
import useRecapSearch from './useRecapSearch';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('../lib/recap_api', () => ({ searchRecaps: vi.fn(), getRecapSearchStatus: vi.fn(), prepareRecapSearch: vi.fn(), cancelRecapSearchIndex: vi.fn() }));
const page = (text, revision = 'v1') => ({ revision, items: [{ text }], next_offset: null, total: 1, offset: 0, limit: 20 });
let events;
beforeEach(() => {
  vi.clearAllMocks();
  events = new Map();
  listen.mockImplementation(async (event, listener) => { events.set(event, listener); return () => events.delete(event); });
  api.getRecapSearchStatus.mockResolvedValue({ revision: 'v1', running: false, activities: 3, dates: 2, indexed_chunks: 0, total_chunks: 3 });
  api.searchRecaps.mockResolvedValue(page('fresh'));
});

describe('recap search lifecycle', () => {
  it('discards superseded queries and clears protected data on lock', async () => {
    let resolveFirst;
    api.searchRecaps.mockImplementationOnce(() => new Promise((resolve) => { resolveFirst = resolve; }));
    const { result, rerender } = renderHook(({ auth }) => useRecapSearch(true, auth), { initialProps: { auth: true } });
    await waitFor(() => expect(result.current.status).not.toBeNull());
    act(() => result.current.update({ query: 'old query' }));
    let first;
    act(() => { first = result.current.search(); });
    act(() => result.current.update({ query: 'new query' }));
    await act(async () => result.current.search());
    expect(result.current.result.items[0].text).toBe('fresh');
    rerender({ auth: false });
    await act(async () => { resolveFirst(page('stale private text')); await first; });
    expect(result.current.result).toBeNull();
    expect(result.current.filters.query).toBe('');
    expect(result.current.status).toBeNull();
  });

  it('never combines pages from different source revisions', async () => {
    api.searchRecaps.mockResolvedValueOnce({ ...page('first'), next_offset: 20, total: 21 })
      .mockResolvedValueOnce({ ...page('changed', 'v2'), offset: 20 });
    const { result } = renderHook(() => useRecapSearch(true, true));
    await waitFor(() => expect(result.current.status).not.toBeNull());
    act(() => result.current.update({ query: 'cache' }));
    await act(async () => result.current.search());
    await act(async () => result.current.loadMore());
    await waitFor(() => expect(result.current.result).toBeNull());
    expect(result.current.changed).toBe(true);
  });

  it('invalidates existing hits when the saved corpus changes', async () => {
    const { result } = renderHook(() => useRecapSearch(true, true));
    await waitFor(() => expect(result.current.status).not.toBeNull());
    act(() => result.current.update({ query: 'cache' }));
    await act(async () => result.current.search());
    api.getRecapSearchStatus.mockResolvedValue({ revision: 'v2', total_chunks: 0, indexed_chunks: 0 });
    await act(async () => events.get('recap-search-progress')());
    expect(result.current.result).toBeNull();
    expect(result.current.changed).toBe(true);
  });

  it('lets preparation stop without treating cancellation as a failure', async () => {
    let reject;
    api.prepareRecapSearch.mockImplementationOnce(() => new Promise((_, fail) => { reject = fail; }));
    api.cancelRecapSearchIndex.mockImplementationOnce(async () => { reject(new Error('AI_CANCELLED')); return true; });
    const { result } = renderHook(() => useRecapSearch(true, true));
    await waitFor(() => expect(result.current.status).not.toBeNull());
    let preparation;
    act(() => { preparation = result.current.prepare(); });
    expect(result.current.preparing).toBe(true);
    await act(async () => { await result.current.stop(); await preparation; });
    expect(api.cancelRecapSearchIndex).toHaveBeenCalledOnce();
    expect(result.current.preparing).toBe(false);
    expect(result.current.indexError).toBe('');
  });
});
