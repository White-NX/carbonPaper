import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { getSnapshotRecapContext } from '../lib/recap_api';
import useSnapshotRecap from './useSnapshotRecap';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/recap_api', () => ({ getSnapshotRecapContext: vi.fn() }));

beforeEach(() => {
  vi.useFakeTimers();
  getSnapshotRecapContext.mockReset().mockImplementation(async (id) => ({ status: 'ready', items: [{ activity_id: String(id) }] }));
  listen.mockReset().mockResolvedValue(() => {});
});
afterEach(() => vi.useRealTimers());
const advance = (ms = 120) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

describe('snapshot recap lifecycle', () => {
  it('debounces timeline scrubbing and skips locked or inactive reads', async () => {
    const { result, rerender } = renderHook(({ id, active, auth }) => useSnapshotRecap(id, active, auth), {
      initialProps: { id: 1, active: true, auth: true },
    });
    rerender({ id: 2, active: true, auth: true });
    rerender({ id: 3, active: true, auth: true });
    await advance();
    expect(getSnapshotRecapContext).toHaveBeenCalledOnce();
    expect(getSnapshotRecapContext).toHaveBeenLastCalledWith(3);
    rerender({ id: 3, active: false, auth: true });
    expect(result.current.context).toBeNull();
    await advance(16000);
    rerender({ id: 3, active: true, auth: false });
    await advance();
    expect(getSnapshotRecapContext).toHaveBeenCalledOnce();
  });

  it('discards an earlier response even after returning to the same snapshot', async () => {
    let resolve;
    getSnapshotRecapContext.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const { result, rerender } = renderHook(({ id }) => useSnapshotRecap(id, true, true), { initialProps: { id: 1 } });
    await advance();
    rerender({ id: 2 });
    expect(result.current.context).toBeNull();
    await advance();
    rerender({ id: 1 });
    await advance();
    await act(async () => resolve({ status: 'ready', items: [{ activity_id: 'stale private activity' }] }));
    expect(result.current.context.items[0].activity_id).toBe('1');
  });

  it('clears private content on lock and removes change subscriptions', async () => {
    const unlisten = vi.fn(); listen.mockResolvedValue(unlisten);
    const { result, rerender } = renderHook(({ auth }) => useSnapshotRecap(1, true, auth), { initialProps: { auth: true } });
    await advance();
    expect(result.current.context).not.toBeNull();
    let resolve;
    getSnapshotRecapContext.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    act(() => { result.current.refresh(); });
    rerender({ auth: false });
    expect(result.current.context).toBeNull();
    await act(async () => resolve({ status: 'ready', items: [{ activity_id: 'private after lock' }] }));
    expect(result.current.context).toBeNull();
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it('refreshes corrections and bounds retries when a source keeps changing', async () => {
    const { result } = renderHook(() => useSnapshotRecap(1, true, true));
    await advance();
    getSnapshotRecapContext.mockResolvedValueOnce({ status: 'unmatched', items: [] });
    const changed = listen.mock.calls.find(([event]) => event === 'recap-changed')[1];
    await act(async () => { await changed(); });
    expect(result.current.context.status).toBe('unmatched');
    getSnapshotRecapContext.mockRejectedValueOnce(new Error('RECAP_SOURCE_CHANGED'))
      .mockRejectedValueOnce(new Error('RECAP_SOURCE_CHANGED'));
    await act(async () => { await result.current.refresh(); });
    await advance(0);
    expect(result.current.context).toBeNull();
    expect(result.current.error).toContain('RECAP_SOURCE_CHANGED');
    expect(getSnapshotRecapContext).toHaveBeenCalledTimes(4);
  });
});
