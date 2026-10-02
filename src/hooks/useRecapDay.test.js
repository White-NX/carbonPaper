import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { getRecapDay, getRecapSettings, listRecapDays } from '../lib/recap_api';
import useRecapDay from './useRecapDay';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/recap_api', () => ({ getRecapDay: vi.fn(), getRecapSettings: vi.fn(), listRecapDays: vi.fn() }));
beforeEach(() => {
  getRecapDay.mockReset().mockImplementation(async (date) => ({ date }));
  getRecapSettings.mockResolvedValue({ enabled: true });
  listRecapDays.mockResolvedValue([]);
});

describe('recap reading lifecycle', () => {
  it('clears a cached view when its source revision is invalidated', async () => {
    const { result } = renderHook(() => useRecapDay('2026-10-01', true, true));
    await waitFor(() => expect(result.current.day).not.toBeNull());
    getRecapDay.mockRejectedValueOnce(new Error('RECAP_SOURCE_CHANGED'));
    await act(async () => result.current.refresh());
    expect(result.current.day).toBeNull();
    expect(result.current.error).toContain('RECAP_SOURCE_CHANGED');
  });
  it('coalesces refreshes and requests a light projection', async () => {
    let resolve;
    getRecapDay.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const { result } = renderHook(() => useRecapDay('2026-10-01', true, true));
    await waitFor(() => expect(getRecapDay).toHaveBeenCalledOnce());
    act(() => { result.current.refresh(); result.current.refresh(); });
    expect(getRecapDay).toHaveBeenCalledOnce();
    await act(async () => resolve({ date: '2026-10-01' }));
    await waitFor(() => expect(getRecapDay).toHaveBeenCalledTimes(2));
    expect(getRecapDay).toHaveBeenLastCalledWith('2026-10-01', { includeRecords: false, includeAttempts: false });
  });
  it('keeps cached content while inactive, cancels listeners and clears it on lock', async () => {
    const unlisten = vi.fn(); listen.mockResolvedValue(unlisten);
    const { result, rerender } = renderHook(({ active, auth }) => useRecapDay('2026-10-01', active, auth), { initialProps: { active: true, auth: true } });
    await waitFor(() => expect(result.current.day).not.toBeNull());
    rerender({ active: false, auth: true });
    await waitFor(() => expect(unlisten).toHaveBeenCalled());
    expect(result.current.day.date).toBe('2026-10-01');
    rerender({ active: false, auth: false });
    expect(result.current.day).toBeNull();
    expect(result.current.settings).toBeNull();
  });
  it('ignores old responses after switching dates, even when switching back', async () => {
    let resolve;
    getRecapDay.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const { result, rerender } = renderHook(({ date }) => useRecapDay(date, true, true), { initialProps: { date: '2026-10-01' } });
    await waitFor(() => expect(getRecapDay).toHaveBeenCalledOnce());
    rerender({ date: '2026-10-02' });
    await waitFor(() => expect(result.current.day?.date).toBe('2026-10-02'));
    rerender({ date: '2026-10-01' });
    await waitFor(() => expect(result.current.day?.date).toBe('2026-10-01'));
    await act(async () => resolve({ date: 'stale secret' }));
    expect(result.current.day.date).toBe('2026-10-01');
  });
});
