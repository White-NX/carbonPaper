import { act, renderHook } from '@testing-library/react';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { updateMonitorFilters } from '../../../lib/monitor_api';
import { useCaptureFilterSettings } from './useCaptureFilterSettings';
import { useSavedCaptureFilters } from './useSavedCaptureFilters';

vi.mock('../../../lib/monitor_api', () => ({ updateMonitorFilters: vi.fn(), deleteRecordsByTimeRange: vi.fn() }));
vi.mock('../../../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}) }));
const saved = { processes: ['private.exe'], titles: [], ignoreProtected: true };
const t = (key) => key;

beforeEach(() => {
  localStorage.clear();
  localStorage.setItem('monitorFilters', JSON.stringify(saved));
  updateMonitorFilters.mockReset().mockResolvedValue({});
});
afterEach(() => vi.restoreAllMocks());

describe('capture rule drafts', () => {
  it('does not persist or apply edits before saving, including on a monitor restart', async () => {
    const editor = renderHook(({ monitorStatus }) => useCaptureFilterSettings({ monitorStatus, t }), { initialProps: { monitorStatus: 'stopped' } });
    act(() => editor.result.current.setProcessInput('secret.exe'));
    act(() => editor.result.current.addProcessTags());
    editor.rerender({ monitorStatus: 'running' });
    expect(JSON.parse(localStorage.getItem('monitorFilters'))).toEqual(saved);
    expect(updateMonitorFilters).not.toHaveBeenCalled();
    renderHook(() => useSavedCaptureFilters({ monitorStatus: 'running' }));
    expect(updateMonitorFilters).toHaveBeenCalledWith({ processes: saved.processes, titles: [], ignore_protected: true });
    expect(editor.result.current.filtersDirty).toBe(true);
  });

  it('commits all visible edits and applies the saved rules once', async () => {
    const { result } = renderHook(() => useCaptureFilterSettings({ monitorStatus: 'running', t }));
    act(() => { result.current.setProcessInput('secret.exe'); result.current.setTitleInput('Private'); });
    await act(() => result.current.handleSaveFilters());
    expect(JSON.parse(localStorage.getItem('monitorFilters'))).toEqual({ ...saved, processes: ['private.exe', 'secret.exe'], titles: ['private'] });
    expect(updateMonitorFilters).toHaveBeenCalledTimes(1);
    expect(result.current.filtersDirty).toBe(false);
    expect(result.current.pendingApply).toBe(false);
  });

  it('defers application while recording is stopped and keeps failed application retryable', async () => {
    const { result, rerender } = renderHook(({ monitorStatus }) => useCaptureFilterSettings({ monitorStatus, t }), { initialProps: { monitorStatus: 'stopped' } });
    act(() => result.current.setTitleInput('personal'));
    await act(() => result.current.handleSaveFilters());
    expect(updateMonitorFilters).not.toHaveBeenCalled();
    expect(result.current.saveFiltersMessage).toBe('settings.save_filters.saved_local_not_running');
    rerender({ monitorStatus: 'running' });
    updateMonitorFilters.mockRejectedValueOnce(new Error('unavailable'));
    await act(() => result.current.handleSaveFilters());
    expect(result.current.pendingApply).toBe(true);
    await act(() => result.current.handleSaveFilters());
    expect(result.current.pendingApply).toBe(false);
  });

  it('keeps the draft and previous rules when persistence fails', async () => {
    const { result } = renderHook(() => useCaptureFilterSettings({ monitorStatus: 'running', t }));
    act(() => result.current.setTitleInput('personal'));
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    await act(() => result.current.handleSaveFilters());
    expect(result.current.filtersDirty).toBe(true);
    expect(JSON.parse(localStorage.getItem('monitorFilters'))).toEqual(saved);
    expect(updateMonitorFilters).not.toHaveBeenCalled();
  });
});
