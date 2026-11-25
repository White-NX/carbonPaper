import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getUsageAppDetails, getUsageDay } from '../lib/usage_api';
import useUsageStatistics from './useUsageStatistics';

vi.mock('../lib/usage_api', () => ({ getUsageDay: vi.fn(), getUsageAppDetails: vi.fn() }));
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};
const app = { id: 'endfield.exe', name: 'Endfield', total_ms: 1000 };
const day = (date) => ({ date, apps: [app], total_ms: 1000 });

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(new Date(2026, 10, 24, 12));
  getUsageDay.mockReset();
  getUsageAppDetails.mockReset();
});
afterEach(() => vi.useRealTimers());

describe('usage statistics requests', () => {
  it('ignores a late response after switching dates and only reads details after selecting an app', async () => {
    const old = deferred(); const current = deferred();
    getUsageDay.mockImplementation((date) => date === '2026-11-24' ? old.promise : current.promise);
    getUsageAppDetails.mockResolvedValue({ date: '2026-11-23', app_id: app.id, titles: [], websites: [] });
    const { result } = renderHook(() => useUsageStatistics(true, true));
    expect(getUsageAppDetails).not.toHaveBeenCalled();
    act(() => result.current.selectDate('2026-11-23'));
    await act(async () => old.resolve(day('2026-11-24')));
    expect(result.current.day).toBeNull();
    await act(async () => current.resolve(day('2026-11-23')));
    expect(result.current.day.date).toBe('2026-11-23');
    act(() => result.current.selectApp(app.id));
    await waitFor(() => expect(result.current.details?.app_id).toBe(app.id));
    expect(getUsageAppDetails).toHaveBeenCalledTimes(1);
    expect(getUsageAppDetails).toHaveBeenCalledWith('2026-11-23', app.id);
  });

  it('clears private results on lock and rejects details that arrive after the selection is cleared', async () => {
    getUsageDay.mockResolvedValue(day('2026-11-24'));
    const details = deferred();
    getUsageAppDetails.mockReturnValue(details.promise);
    const { result, rerender } = renderHook(({ authenticated }) => useUsageStatistics(true, authenticated), { initialProps: { authenticated: true } });
    await waitFor(() => expect(result.current.day).not.toBeNull());
    act(() => result.current.selectApp(app.id));
    act(() => result.current.clearApp());
    await act(async () => details.resolve({ date: '2026-11-24', app_id: app.id, titles: [{ label: 'private title' }] }));
    expect(result.current.details).toBeNull();
    rerender({ authenticated: false });
    expect(result.current.day).toBeNull();
    expect(result.current.selectedApp).toBeNull();
    await act(async () => result.current.refresh());
    expect(getUsageDay).toHaveBeenCalledTimes(1);
  });
});
