import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { getMonitorFilters, updateMonitorFilters } from '../../../lib/monitor_api';
import { setPreference } from '../../../lib/preference_store';
import { useCaptureFilterSettings } from './useCaptureFilterSettings';
import { useSavedCaptureFilters } from './useSavedCaptureFilters';

vi.mock('../../../lib/monitor_api', () => ({ getMonitorFilters: vi.fn(), updateMonitorFilters: vi.fn(), deleteRecordsByTimeRange: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('../../../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}) }));
const saved = { processes: ['private.exe'], titles: [], ignoreProtected: true };
const t = (key) => key;

beforeEach(() => {
  localStorage.clear();
  localStorage.setItem('monitorFilters', JSON.stringify(saved));
  updateMonitorFilters.mockReset().mockResolvedValue({});
  getMonitorFilters.mockReset().mockResolvedValue(null);
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
    await waitFor(() => expect(updateMonitorFilters).toHaveBeenCalledWith({ processes: saved.processes, titles: [], ignore_protected: true }, { promptMode: 'defer' }));
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

  it('saves native rules while recording is stopped and keeps failed saves retryable', async () => {
    const { result, rerender } = renderHook(({ monitorStatus }) => useCaptureFilterSettings({ monitorStatus, t }), { initialProps: { monitorStatus: 'stopped' } });
    act(() => result.current.setTitleInput('personal'));
    await act(() => result.current.handleSaveFilters());
    expect(updateMonitorFilters).toHaveBeenCalledTimes(1);
    expect(result.current.saveFiltersMessage).toBe('settings.save_filters.saved_local_not_running');
    rerender({ monitorStatus: 'running' });
    updateMonitorFilters.mockRejectedValueOnce(new Error('unavailable'));
    await act(() => result.current.handleSaveFilters());
    expect(result.current.pendingApply).toBe(true);
    expect(result.current.filtersDirty).toBe(true);
    expect(result.current.filterSettings.titles).toEqual(['personal']);
    await act(() => result.current.handleSaveFilters());
    expect(result.current.pendingApply).toBe(false);
  });

  it('keeps committed native rules and an editable draft when the backend save fails', async () => {
    const { result } = renderHook(() => useCaptureFilterSettings({ monitorStatus: 'running', t }));
    act(() => result.current.setTitleInput('personal'));
    updateMonitorFilters.mockRejectedValueOnce(new Error('disk unavailable'));
    await act(() => result.current.handleSaveFilters());
    expect(JSON.parse(localStorage.getItem('monitorFilters'))).toEqual(saved);
    expect(result.current.filterSettings.titles).toEqual(['personal']);
    expect(result.current.filtersDirty).toBe(true);
    expect(result.current.pendingApply).toBe(true);
  });

  it('keeps a tray exclusion when unrelated settings are saved from an existing draft', async () => {
    const { result } = renderHook(() => useCaptureFilterSettings({ monitorStatus: 'running', t }));
    await act(async () => {});
    act(() => result.current.setTitleInput('personal'));
    act(() => result.current.addTitleTags());
    act(() => setPreference('monitorFilters', JSON.stringify({ ...saved, processes: [...saved.processes, 'chrome'] })));
    expect(result.current.filterSettings.titles).toEqual(['personal']);
    expect(result.current.filterSettings.processes).toEqual(['private.exe', 'chrome']);
    await act(() => result.current.handleSaveFilters());
    expect(updateMonitorFilters).toHaveBeenCalledWith({ processes: ['private.exe', 'chrome'], titles: ['personal'], ignore_protected: true });
  });

  it('reads native rules instead of overwriting them with a stale WebView cache', async () => {
    getMonitorFilters.mockResolvedValue({ processes: ['chrome'], titles: ['private'], ignore_protected: false });
    renderHook(() => useSavedCaptureFilters({ monitorStatus: 'running' }));
    await waitFor(() => expect(JSON.parse(localStorage.getItem('monitorFilters')).processes).toEqual(['chrome']));
    expect(updateMonitorFilters).not.toHaveBeenCalled();
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
