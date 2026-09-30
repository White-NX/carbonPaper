import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { getRecapProgress } from '../lib/recap_api';
import useRecapProgress from './useRecapProgress';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/recap_api', () => ({ getRecapProgress: vi.fn() }));
beforeEach(() => {
  getRecapProgress.mockReset().mockResolvedValue(null);
  listen.mockClear();
});

describe('recap progress subscription', () => {
  it('loads the current background run and refreshes after a content-free notification', async () => {
    getRecapProgress.mockResolvedValue({ run_id: 'run', stage: 'thinking', version: 1 });
    const { result } = renderHook(() => useRecapProgress('2026-09-30', true, true));
    await waitFor(() => expect(result.current.progress?.stage).toBe('thinking'));
    expect(getRecapProgress).toHaveBeenCalledWith('2026-09-30');
    getRecapProgress.mockResolvedValue({ run_id: 'run', stage: 'writing', version: 2 });
    await act(async () => { await listen.mock.calls[0][1](); });
    expect(result.current.progress.stage).toBe('writing');
  });
  it('clears content on lock and ignores a late reply from the previous page', async () => {
    let resolve;
    getRecapProgress.mockImplementationOnce(() => new Promise((r) => { resolve = r; }));
    const { result, rerender } = renderHook(({ date, unlocked }) => useRecapProgress(date, true, unlocked), {
      initialProps: { date: '2026-09-30', unlocked: true },
    });
    await waitFor(() => expect(getRecapProgress).toHaveBeenCalledOnce());
    rerender({ date: '2026-09-30', unlocked: false });
    await act(async () => { resolve({ stage: 'thinking', attempts: [{ reasoning: 'private' }] }); });
    expect(result.current.progress).toBeNull();
    getRecapProgress.mockResolvedValue({ date: '2026-10-01', stage: 'ready' });
    rerender({ date: '2026-10-01', unlocked: true });
    await waitFor(() => expect(result.current.progress?.date).toBe('2026-10-01'));
  });
  it('coalesces notifications while a snapshot is being fetched', async () => {
    let resolve;
    getRecapProgress.mockImplementationOnce(() => new Promise((r) => { resolve = r; }));
    const { result } = renderHook(() => useRecapProgress('2026-09-30', true, true));
    await waitFor(() => expect(getRecapProgress).toHaveBeenCalledOnce());
    const notify = listen.mock.calls[0][1];
    await act(async () => { await notify(); await notify(); });
    expect(getRecapProgress).toHaveBeenCalledOnce();
    getRecapProgress.mockResolvedValue({ stage: 'ready', version: 20 });
    await act(async () => { resolve({ stage: 'thinking', version: 10 }); });
    await waitFor(() => expect(result.current.progress?.version).toBe(20));
    expect(getRecapProgress).toHaveBeenCalledTimes(2);
  });
});
