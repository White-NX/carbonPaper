import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getUsageAppDetails, getUsageDay, setUsageAppCategory } from '../lib/usage_api';
import useUsageStatistics from './useUsageStatistics';

vi.mock('../lib/usage_api', async (importOriginal) => ({
  ...await importOriginal(), getUsageDay: vi.fn(), getUsageAppDetails: vi.fn(), setUsageAppCategory: vi.fn(),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};
const app = { id: 'endfield.exe', name: 'Endfield', total_ms: 1000 };
const day = (date) => ({ date, generation: 7, apps: [app], total_ms: 1000 });

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(new Date(2026, 10, 24, 12));
  getUsageDay.mockReset();
  getUsageAppDetails.mockReset();
  setUsageAppCategory.mockReset();
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

  it('skips intermediate dates even when reads resolve immediately', async () => {
    getUsageDay.mockImplementation(async (date) => day(date));
    const transition = vi.fn((_from, _to, commit) => commit());
    const { result } = renderHook(() => useUsageStatistics(true, true, transition));
    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-24'));
    act(() => result.current.stepDate(-1));
    act(() => result.current.stepDate(-1));
    act(() => result.current.stepDate(-1));
    expect(result.current).toMatchObject({ date: '2026-11-24', requestedDate: '2026-11-21', loading: true });
    expect(getUsageDay).toHaveBeenCalledTimes(1);

    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-21'));
    expect(getUsageDay.mock.calls).toEqual([['2026-11-24'], ['2026-11-21']]);
    expect(transition).toHaveBeenCalledExactlyOnceWith('2026-11-24', '2026-11-21', expect.any(Function));
  });

  it('keeps the displayed day and skips a pending result after a newer selection', async () => {
    const skipped = deferred();
    const latest = deferred();
    getUsageDay.mockResolvedValueOnce(day('2026-11-24')).mockReturnValueOnce(skipped.promise).mockReturnValueOnce(latest.promise);
    const transition = vi.fn((_from, _to, commit) => commit());
    const { result } = renderHook(() => useUsageStatistics(true, true, transition));
    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-24'));
    act(() => result.current.stepDate(-1));
    await waitFor(() => expect(getUsageDay).toHaveBeenLastCalledWith('2026-11-23'));
    act(() => {
      result.current.stepDate(-1);
      result.current.stepDate(-1);
    });
    expect(result.current).toMatchObject({ date: '2026-11-24', requestedDate: '2026-11-21', loading: true });
    expect(getUsageDay.mock.calls).toEqual([['2026-11-24'], ['2026-11-23']]);
    await act(async () => skipped.resolve(day('2026-11-23')));
    await waitFor(() => expect(getUsageDay).toHaveBeenLastCalledWith('2026-11-21'));
    expect(result.current.day.date).toBe('2026-11-24');
    expect(transition).not.toHaveBeenCalled();

    await act(async () => latest.resolve(day('2026-11-21')));
    expect(transition).toHaveBeenCalledExactlyOnceWith('2026-11-24', '2026-11-21', expect.any(Function));
    expect(result.current).toMatchObject({ date: '2026-11-21', loading: false });
  });

  it('ignores a date commit if the selection changes during capture', async () => {
    getUsageDay.mockImplementation(async (date) => day(date));
    const capture = deferred();
    const transition = vi.fn(async (_from, _to, commit) => { await capture.promise; return commit(); });
    const { result } = renderHook(() => useUsageStatistics(true, true, transition));
    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-24'));
    act(() => result.current.stepDate(-1));
    await waitFor(() => expect(transition).toHaveBeenCalledTimes(1));

    act(() => result.current.stepDate(-1));
    await act(async () => capture.resolve());
    expect(await transition.mock.results[0].value).toBe(false);
    await waitFor(() => expect(result.current.date).toBe('2026-11-22'));
  });

  it('keeps the displayed app and its details after an error and retries the failed date', async () => {
    const failed = deferred();
    getUsageDay.mockResolvedValueOnce(day('2026-11-24')).mockReturnValueOnce(failed.promise);
    getUsageAppDetails.mockResolvedValue({ date: '2026-11-24', app_id: app.id, titles: [], websites: [] });
    const { result } = renderHook(() => useUsageStatistics(true, true));
    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-24'));
    act(() => result.current.selectApp(app.id));
    await waitFor(() => expect(result.current.details?.app_id).toBe(app.id));

    act(() => result.current.stepDate(-1));
    await waitFor(() => expect(getUsageDay).toHaveBeenLastCalledWith('2026-11-23'));
    await act(async () => failed.reject(new Error('USAGE_QUERY_FAILED')));
    expect(result.current).toMatchObject({ date: '2026-11-24', requestedDate: '2026-11-23', navigationDate: '2026-11-24',
      loading: false, error: 'Error: USAGE_QUERY_FAILED', selectedApp: app, details: { date: '2026-11-24' } });
    act(() => window.dispatchEvent(new Event('focus')));
    expect(getUsageDay).toHaveBeenCalledTimes(2);

    setUsageAppCategory.mockResolvedValue(undefined);
    getUsageDay.mockResolvedValueOnce({ ...day('2026-11-24'), apps: [{ ...app, category: '游戏' }] });
    await act(async () => result.current.saveCategory(app.id, '游戏', 7));
    expect(getUsageDay).toHaveBeenLastCalledWith('2026-11-24');
    expect(result.current).toMatchObject({ requestedDate: '2026-11-23', error: 'Error: USAGE_QUERY_FAILED', selectedApp: { category: '游戏' } });

    getUsageDay.mockResolvedValueOnce(day('2026-11-23'));
    await act(async () => result.current.refresh());
    expect(getUsageDay).toHaveBeenLastCalledWith('2026-11-23');
    expect(result.current).toMatchObject({ date: '2026-11-23', error: '', selectedApp: null, details: null });
  });

  it('keeps same-day polling responsive when a category change lands during a read', async () => {
    const polled = deferred();
    getUsageDay.mockResolvedValueOnce(day('2026-11-24')).mockReturnValueOnce(polled.promise)
      .mockResolvedValueOnce({ ...day('2026-11-24'), apps: [{ ...app, category: '游戏' }] });
    setUsageAppCategory.mockResolvedValue(undefined);
    const { result } = renderHook(() => useUsageStatistics(true, true));
    await waitFor(() => expect(result.current.day?.date).toBe('2026-11-24'));
    act(() => window.dispatchEvent(new Event('focus')));
    expect(result.current.loading).toBe(true);
    let saved;
    await act(async () => { saved = result.current.saveCategory(app.id, '游戏', 7); });
    await act(async () => { polled.resolve(day('2026-11-24')); await saved; });
    expect(getUsageDay).toHaveBeenCalledTimes(3);
    expect(result.current.day.apps[0].category).toBe('游戏');
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

  it('refreshes category totals after saving and rejects changes from a stale archive', async () => {
    getUsageDay.mockResolvedValue(day('2026-11-24'));
    setUsageAppCategory.mockResolvedValue(undefined);
    const { result } = renderHook(() => useUsageStatistics(true, true));
    await waitFor(() => expect(result.current.day).not.toBeNull());
    await expect(result.current.saveCategory(app.id, '游戏', 6)).rejects.toThrow('USAGE_SOURCE_CHANGED');
    expect(setUsageAppCategory).not.toHaveBeenCalled();
    getUsageDay.mockResolvedValue({ ...day('2026-11-24'), apps: [{ ...app, category: '游戏', category_is_manual: true }] });
    await act(async () => result.current.saveCategory(app.id, '游戏', 7));
    expect(setUsageAppCategory).toHaveBeenCalledWith(app.id, '游戏', 7);
    expect(result.current.day.apps[0].category).toBe('游戏');
  });
});
