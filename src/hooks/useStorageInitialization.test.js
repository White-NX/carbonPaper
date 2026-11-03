import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useStorageInitialization } from './useStorageInitialization';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

const running = { running: true, ready: false, phase: 'vectors', processed: 512, total: 1000, revision: 2, error: null };
const ready = { ...running, running: false, ready: true, phase: 'finalizing', revision: 4 };

describe('storage startup recovery', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    listen.mockResolvedValue(vi.fn());
  });
  afterEach(() => vi.useRealTimers());

  it('recovers an existing job on mount and ignores a stale poll after completion', async () => {
    let finishPoll;
    invoke.mockResolvedValueOnce(running).mockImplementationOnce(() => new Promise((resolve) => { finishPoll = resolve; }));
    const hook = renderHook(() => useStorageInitialization());
    expect(hook.result.current.ready).toBe(false);
    expect(hook.result.current.showOverlay).toBe(false);
    await act(async () => {});
    expect(hook.result.current.status.processed).toBe(512);
    expect(hook.result.current.showOverlay).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    const onProgress = listen.mock.calls.find(([name]) => name === 'storage-initialization-progress')[1];
    await act(async () => { onProgress({ payload: ready }); });
    await act(async () => { finishPoll(running); });
    expect(hook.result.current.ready).toBe(true);
    expect(hook.result.current.showOverlay).toBe(false);
    expect(hook.result.current.status.revision).toBe(4);
    const calls = invoke.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(invoke).toHaveBeenCalledTimes(calls);
  });

  it('shows a failed first status read without admitting storage access', async () => {
    invoke.mockRejectedValueOnce(new Error('status unavailable'));
    const hook = renderHook(() => useStorageInitialization());
    await act(async () => {});
    expect(hook.result.current.ready).toBe(false);
    expect(hook.result.current.error).toBe('status unavailable');
    expect(hook.result.current.showOverlay).toBe(true);
  });

  it('retains failure until an explicit retry and follows the restarted job', async () => {
    const failed = { ...running, running: false, phase: 'compacting', error: 'disk full', revision: 3 };
    const restarted = { ...running, phase: 'credentials', processed: 0, total: 0, revision: 4 };
    invoke.mockImplementation(async (command) => command === 'storage_retry_initialization' ? restarted : failed);
    const hook = renderHook(() => useStorageInitialization());
    await act(async () => {});
    expect(hook.result.current.error).toBe('disk full');
    await act(async () => { await vi.advanceTimersByTimeAsync(1500); });
    expect(invoke.mock.calls.filter(([name]) => name === 'storage_retry_initialization')).toHaveLength(0);
    await act(async () => { await hook.result.current.retry(); });
    expect(hook.result.current.error).toBeNull();
    expect(hook.result.current.status.phase).toBe('credentials');
    const onProgress = listen.mock.calls.find(([name]) => name === 'storage-initialization-progress')[1];
    await act(async () => { onProgress({ payload: { ...ready, revision: 5 } }); });
    expect(hook.result.current.ready).toBe(true);
  });
});
