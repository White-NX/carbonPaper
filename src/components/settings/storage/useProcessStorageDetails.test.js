import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getSoftDeleteQueueStatus, getProcessMonthlyThumbnails, softDeleteProcessMonth, softDeleteScreenshots } from '../../../lib/monitor_api';
import { useProcessStorageDetails } from './useProcessStorageDetails';

const activity = vi.hoisted(() => ({ active: false, busy: false }));
vi.mock('../SettingsActivityContext', () => ({
  useSettingsActive: () => activity.active,
  useSettingsActivity: vi.fn(),
  useSettingsWindowActivity: () => ({ busy: activity.busy }),
}));
vi.mock('../../../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}) }));
vi.mock('../../../lib/monitor_api', () => ({
  fetchThumbnailBatch: vi.fn(async () => ({})),
  getProcessMonthlyThumbnails: vi.fn(),
  getProcessStorageStats: vi.fn(async () => []),
  getSoftDeleteQueueStatus: vi.fn(async () => ({})),
  softDeleteProcessMonth: vi.fn(async () => ({})),
  softDeleteScreenshots: vi.fn(async () => ({})),
}));

const page = (time_zone, process_name = 'editor.exe') => ({
  process_name, time_zone, page: 0, next_page: 1,
  items: [{ screenshot_id: 83558, month: '2026-10' }],
});
const t = key => key;

describe('process month calendar', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    activity.active = false;
    activity.busy = false;
  });
  afterEach(() => vi.useRealTimers());

  it('never overlaps a slow status poll and pauses during backup work', async () => {
    vi.useFakeTimers();
    activity.active = true;
    let finish;
    getSoftDeleteQueueStatus.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    const hook = renderHook(() => useProcessStorageDetails({ t }));
    await act(async () => vi.advanceTimersByTimeAsync(120_000));
    expect(getSoftDeleteQueueStatus).toHaveBeenCalledTimes(1);
    await act(async () => finish({ pending_screenshots: 12, pending_ocr: 34, running: false }));
    activity.busy = true;
    hook.rerender();
    await act(async () => vi.advanceTimersByTimeAsync(120_000));
    expect(getSoftDeleteQueueStatus).toHaveBeenCalledTimes(1);
    expect(hook.result.current.deleteQueueStatus.pending_screenshots).toBe(12);
    activity.busy = false;
    hook.rerender();
    await act(async () => {});
    expect(getSoftDeleteQueueStatus).toHaveBeenCalledTimes(2);
    hook.unmount();
  });

  it('pins pagination and a pending deletion to the calendar returned by the displayed page', async () => {
    getProcessMonthlyThumbnails.mockResolvedValue(page('Asia/Shanghai'));
    const { result } = renderHook(() => useProcessStorageDetails({ t }));
    act(() => result.current.openProcessDetail('editor.exe'));
    await waitFor(() => expect(result.current.processMonthData?.time_zone).toBe('Asia/Shanghai'));
    expect(getProcessMonthlyThumbnails).toHaveBeenLastCalledWith('editor.exe', 0, 60, null);
    await act(async () => result.current.loadProcessMonthPage('editor.exe', 1));
    expect(getProcessMonthlyThumbnails).toHaveBeenLastCalledWith('editor.exe', 1, 60, 'Asia/Shanghai');
    act(() => result.current.requestSoftDelete('editor.exe', '2026-10'));
    expect(result.current.pendingDeleteIntent.timeZone).toBe('Asia/Shanghai');

    // Simulate reopening the page after the OS time zone changes while an
    // earlier confirmation remains pending. Approval still refers to that page.
    getProcessMonthlyThumbnails.mockResolvedValue(page('America/New_York'));
    act(() => result.current.openProcessDetail('editor.exe'));
    await waitFor(() => expect(result.current.processMonthData?.time_zone).toBe('America/New_York'));
    await act(async () => result.current.handleConfirmSoftDelete());
    expect(softDeleteProcessMonth).toHaveBeenCalledWith('editor.exe', '2026-10', 'Asia/Shanghai');
  });

  it('keeps selection-based deletion tied to screenshot IDs', async () => {
    getProcessMonthlyThumbnails.mockResolvedValue(page('Asia/Shanghai'));
    const { result } = renderHook(() => useProcessStorageDetails({ t }));
    act(() => result.current.openProcessDetail('editor.exe'));
    await waitFor(() => expect(result.current.processMonthData).not.toBeNull());
    act(() => result.current.requestSoftDelete('editor.exe', '2026-10', [83558]));
    await act(async () => result.current.handleConfirmSoftDelete());
    expect(softDeleteScreenshots).toHaveBeenCalledWith([83558]);
    expect(softDeleteProcessMonth).not.toHaveBeenCalled();
  });

  it('ignores an old page response after switching processes', async () => {
    let finishOld;
    getProcessMonthlyThumbnails.mockImplementationOnce(() => new Promise(resolve => { finishOld = resolve; }));
    getProcessMonthlyThumbnails.mockResolvedValueOnce(page('America/New_York', 'browser.exe'));
    const { result } = renderHook(() => useProcessStorageDetails({ t }));
    act(() => result.current.openProcessDetail('editor.exe'));
    act(() => result.current.openProcessDetail('browser.exe'));
    await waitFor(() => expect(result.current.processMonthData?.process_name).toBe('browser.exe'));
    await act(async () => finishOld(page('Asia/Shanghai')));
    expect(result.current.processMonthData.time_zone).toBe('America/New_York');
    act(() => result.current.requestSoftDelete('browser.exe', '2026-10'));
    expect(result.current.pendingDeleteIntent.timeZone).toBe('America/New_York');
  });
});
