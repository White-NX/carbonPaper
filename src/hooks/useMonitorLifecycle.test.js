import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useMonitorLifecycle } from './useMonitorLifecycle';

const events = vi.hoisted(() => new Map());
vi.mock('./useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => events.set(name, handler) }));
let stopped;
beforeEach(() => {
  stopped = false;
  localStorage.clear();
  events.clear();
  invoke.mockReset().mockImplementation(async (command) => {
    if (command === 'get_monitor_autostart') return true;
    if (command === 'get_monitor_status') return JSON.stringify({ stopped });
    if (command === 'stop_monitor') { stopped = true; events.get('monitor-stopped')?.({ payload: {} }); }
    if (command === 'start_monitor') stopped = false;
    return null;
  });
});
const options = { modelsCheckDone: true, modelsNeedDownload: false,
  powerSavingSuppressed: false, formatErrorDetails: String, reportBackendError: vi.fn(), resetBackendErrorDedupe: vi.fn(), t: (key) => key };

describe('monitor actions from settings', () => {
  it('does not auto-start after a settings stop, including the native stopped event', async () => {
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await waitFor(() => expect(result.current.backendStatus).toBe('online'));
    await act(() => result.current.handleSettingsMonitorAction('stop'));
    expect(result.current.backendStatus).toBe('offline');
    expect(invoke.mock.calls.filter(([command]) => command === 'start_monitor')).toHaveLength(0);
    await act(() => result.current.handleSettingsMonitorAction('start'));
    expect(result.current.backendStatus).toBe('online');
    expect(invoke.mock.calls.filter(([command]) => command === 'start_monitor')).toHaveLength(1);
  });
  it('rejects arbitrary bridge actions without invoking a runtime command', async () => {
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await waitFor(() => expect(result.current.backendStatus).toBe('online'));
    await expect(result.current.handleSettingsMonitorAction('exit_app')).rejects.toThrow('INVALID_MONITOR_ACTION');
    expect(invoke).not.toHaveBeenCalledWith('exit_app');
  });
});
