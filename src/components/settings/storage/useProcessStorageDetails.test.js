import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getProcessMonthlyThumbnails, softDeleteProcessMonth, softDeleteScreenshots } from '../../../lib/monitor_api';
import { useProcessStorageDetails } from './useProcessStorageDetails';

vi.mock('../SettingsActivityContext', () => ({ useSettingsActive: () => false, useSettingsActivity: vi.fn() }));
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
  beforeEach(() => vi.clearAllMocks());

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
