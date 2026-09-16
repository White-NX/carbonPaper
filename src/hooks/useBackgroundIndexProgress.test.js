import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { getBackgroundIndexProgress } from '../lib/monitor_api';
import { useBackgroundIndexProgress } from './useBackgroundIndexProgress';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('../lib/monitor_api', () => ({ getBackgroundIndexProgress: vi.fn() }));

const idle = { phase: 'idle', running: false, processed: 0, indexed: 0, total: 0, revision: 0 };
const clip = { phase: 'running', running: true, processed: 3, indexed: 3, total: 10, run_id: 1, revision: 2 };

describe('useBackgroundIndexProgress', () => {
  let events;
  beforeEach(() => {
    vi.useFakeTimers();
    events = {};
    listen.mockImplementation(async (name, callback) => { events[name] = callback; return vi.fn(); });
    getBackgroundIndexProgress.mockResolvedValue({ semantic: idle, clip });
  });
  afterEach(() => { vi.useRealTimers(); });

  it('retains progress across failed polls and scheduler waits, then clears activity on completion', async () => {
    const hook = renderHook(() => useBackgroundIndexProgress());
    await act(async () => {});
    getBackgroundIndexProgress.mockResolvedValueOnce(null);
    await act(async () => { await hook.result.current.refresh(); });
    expect(hook.result.current.progress.clip.processed).toBe(3);
    expect(hook.result.current.progress.clip.phase).toBe('running');
    getBackgroundIndexProgress.mockResolvedValueOnce({
      clip: { ...clip, running: false, phase: 'waiting_for_unlock', revision: 3 },
    });
    await act(async () => { await hook.result.current.refresh(); });
    expect(hook.result.current.progress.clip.phase).toBe('waiting_for_unlock');
    expect(hook.result.current.progress.clip.processed).toBe(3);
    getBackgroundIndexProgress.mockResolvedValueOnce({ clip: { ...clip, running: false, phase: 'idle', revision: 4 } });
    await act(async () => { await hook.result.current.refresh(); });
    expect(hook.result.current.progress.clip.phase).toBe('idle');
  });

  it('accepts events before the first poll and ignores older replies and late events', async () => {
    let reply;
    getBackgroundIndexProgress.mockReturnValueOnce(new Promise((resolve) => { reply = resolve; }));
    const hook = renderHook(() => useBackgroundIndexProgress());
    await act(async () => {});
    act(() => events['clip-index-progress']({ payload: { ...clip, processed: 5, revision: 4 } }));
    expect(hook.result.current.progress.clip.processed).toBe(5);
    await act(async () => { reply({ semantic: idle, clip }); });
    expect(hook.result.current.progress.clip.processed).toBe(5);
    getBackgroundIndexProgress.mockResolvedValueOnce({ clip: { ...clip, phase: 'idle', running: false, revision: 6 } });
    await act(async () => { await hook.result.current.refresh(); });
    act(() => events['clip-index-progress']({ payload: { ...clip, revision: 5 } }));
    expect(hook.result.current.progress.clip.phase).toBe('idle');
  });

  it('does not overlap slow polls and reads again after a command requests fresh state', async () => {
    let reply;
    getBackgroundIndexProgress.mockReturnValueOnce(new Promise((resolve) => { reply = resolve; }));
    const hook = renderHook(() => useBackgroundIndexProgress());
    await act(async () => { await vi.advanceTimersByTimeAsync(12000); });
    expect(getBackgroundIndexProgress).toHaveBeenCalledTimes(1);
    let fresh;
    act(() => { fresh = hook.result.current.refresh({ fresh: true }); });
    await act(async () => { reply({ semantic: idle, clip }); await fresh; });
    expect(getBackgroundIndexProgress).toHaveBeenCalledTimes(2);
  });
});
