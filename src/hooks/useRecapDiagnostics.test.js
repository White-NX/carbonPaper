import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { getRecapDay } from '../lib/recap_api';
import useRecapDiagnostics from './useRecapDiagnostics';

let progressState;
vi.mock('./useRecapProgress', () => ({ default: () => progressState }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/recap_api', () => ({ getRecapDay: vi.fn() }));
beforeEach(() => {
  progressState = { progress: null, error: '' };
  getRecapDay.mockReset().mockResolvedValue({ batches: [] });
  listen.mockClear();
});

describe('recap diagnostic snapshots', () => {
  it('refreshes saved outcomes on cache notifications and run completion, not text deltas', async () => {
    const { result, rerender } = renderHook(() => useRecapDiagnostics('2026-10-04', true));
    await waitFor(() => expect(result.current.day).toEqual({ batches: [] }));
    expect(getRecapDay).toHaveBeenCalledWith('2026-10-04', { includeRecords: false, includeAttempts: true });
    getRecapDay.mockResolvedValue({ batches: [{ status: 'partial' }] });
    await act(async () => listen.mock.calls[0][1]());
    expect(result.current.day.batches[0].status).toBe('partial');
    progressState = { progress: { run_id: 'run', version: 1, completed_batches: 0 }, error: '' };
    await act(async () => rerender());
    getRecapDay.mockClear();
    progressState = { progress: { ...progressState.progress, version: 2, attempts: [{ reasoning: 'delta' }] }, error: '' };
    await act(async () => rerender());
    expect(getRecapDay).not.toHaveBeenCalled();
    getRecapDay.mockResolvedValue({ batches: [{ status: 'ready', summary: {} }] });
    progressState = { progress: { ...progressState.progress, finished_at_ms: 1000 }, error: '' };
    await act(async () => rerender());
    expect(result.current.day.batches[0].status).toBe('ready');
    expect(getRecapDay).toHaveBeenCalledOnce();
  });

  it('coalesces overlapping cache notifications and cleans up its subscription', async () => {
    let resolve;
    const unlisten = vi.fn();
    listen.mockResolvedValueOnce(unlisten);
    getRecapDay.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    const { result, unmount } = renderHook(() => useRecapDiagnostics('2026-10-04', true));
    await waitFor(() => expect(getRecapDay).toHaveBeenCalledOnce());
    const notify = listen.mock.calls[0][1];
    await act(async () => { notify(); notify(); });
    expect(getRecapDay).toHaveBeenCalledOnce();
    getRecapDay.mockResolvedValue({ batches: [{ status: 'ready' }] });
    await act(async () => resolve({ batches: [{ status: 'partial' }] }));
    await waitFor(() => expect(result.current.day.batches[0].status).toBe('ready'));
    expect(getRecapDay).toHaveBeenCalledTimes(2);
    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalledOnce());
  });

  it('discards private snapshots and late responses when hidden or changing date', async () => {
    let resolve;
    getRecapDay.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    const { result, rerender } = renderHook(({ date, active }) => useRecapDiagnostics(date, active), { initialProps: { date: '2026-10-04', active: true } });
    await waitFor(() => expect(getRecapDay).toHaveBeenCalledOnce());
    rerender({ date: '2026-10-04', active: false });
    await act(async () => resolve({ batches: [{ attempts: [{ text: 'private' }] }] }));
    expect(result.current.day).toBeNull();
    getRecapDay.mockResolvedValue({ date: '2026-10-03', batches: [] });
    rerender({ date: '2026-10-03', active: true });
    await waitFor(() => expect(result.current.day.date).toBe('2026-10-03'));
  });

  it('clears saved private content when the progress read loses authorization', async () => {
    getRecapDay.mockResolvedValue({ batches: [{ attempts: [{ text: 'private' }] }] });
    const { result, rerender } = renderHook(() => useRecapDiagnostics('2026-10-04', true));
    await waitFor(() => expect(result.current.day).not.toBeNull());
    progressState = { progress: null, error: 'AUTH_REQUIRED' };
    rerender();
    expect(result.current.day).toBeNull();
    expect(result.current.error).toBe('AUTH_REQUIRED');
  });
});
