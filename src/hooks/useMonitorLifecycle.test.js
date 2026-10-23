import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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
afterEach(() => vi.useRealTimers());
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

describe('capture loop that ends on its own', () => {
  it('is restarted, but not indefinitely', async () => {
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await waitFor(() => expect(result.current.backendStatus).toBe('online'));
    const starts = () => invoke.mock.calls.filter(([command]) => command === 'start_monitor').length;
    const loopEnds = () => {
      stopped = true;
      events.get('monitor-stopped')({ payload: { intentional: false } });
    };

    act(loopEnds);
    await waitFor(() => expect(starts()).toBe(1));
    act(loopEnds);
    await waitFor(() => expect(starts()).toBe(2));

    act(loopEnds);
    await act(async () => {});
    expect(starts()).toBe(2);
    expect(result.current.backendStatus).toBe('offline');
  });
});

describe('first-run wizard', () => {
  it('does not start recording until the wizard releases capture', async () => {
    stopped = true;
    const starts = () => invoke.mock.calls.filter(([command]) => command === 'start_monitor').length;
    const { result, rerender } = renderHook((props) => useMonitorLifecycle({ ...options, ...props }), {
      initialProps: { captureHeld: true },
    });
    await waitFor(() => expect(result.current.backendStatus).toBe('offline'));
    await act(async () => {});
    expect(starts()).toBe(0);

    rerender({ captureHeld: false });
    await waitFor(() => expect(starts()).toBe(1));
  });
});

describe('power-saving admission', () => {
  it('waits for the startup power probe and stays stopped on battery', async () => {
    stopped = true;
    const starts = () => invoke.mock.calls.filter(([command]) => command === 'start_monitor').length;
    const { result, rerender } = renderHook((props) => useMonitorLifecycle({ ...options, ...props }), {
      initialProps: { powerStatusReady: false, powerSavingSuppressed: false },
    });
    await waitFor(() => expect(result.current.backendStatus).toBe('offline'));
    expect(starts()).toBe(0);
    rerender({ powerStatusReady: true, powerSavingSuppressed: true });
    await act(async () => {});
    expect(starts()).toBe(0);
    rerender({ powerStatusReady: true, powerSavingSuppressed: false });
    await waitFor(() => expect(starts()).toBe(1));
  });

  it('treats a native power rejection as temporary and resumes when AC returns', async () => {
    const reportBackendError = vi.fn();
    const { result, rerender } = renderHook((props) => useMonitorLifecycle({ ...options, reportBackendError, ...props }), {
      initialProps: { powerSavingSuppressed: false },
    });
    await waitFor(() => expect(result.current.backendStatus).toBe('online'));
    const originalInvoke = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, ...args) => {
      if (command === 'start_monitor') throw 'POWER_SAVING_ACTIVE';
      return originalInvoke(command, ...args);
    });
    await act(() => result.current.handleStartBackend());
    expect(reportBackendError).not.toHaveBeenCalled();
    expect(result.current.backendError).toBe('');
    expect(result.current.monitorPowerSavingSuppressed).toBe(true);
    expect(invoke.mock.calls.filter(([command]) => command === 'start_monitor')).toHaveLength(1);
    rerender({ powerSavingSuppressed: true });
    invoke.mockImplementation(originalInvoke);
    rerender({ powerSavingSuppressed: false });
    await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === 'start_monitor')).toHaveLength(2));
    expect(result.current.monitorPowerSavingSuppressed).toBe(false);
  });

  it('clears native admission blocking through polling when power events are missing', async () => {
    vi.useFakeTimers();
    let nativePowerSaving = false;
    const originalInvoke = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, ...args) => {
      if (command === 'get_monitor_status') return JSON.stringify({ stopped, power_saving_active: nativePowerSaving });
      if (command === 'start_monitor' && nativePowerSaving) throw 'POWER_SAVING_ACTIVE';
      return originalInvoke(command, ...args);
    });
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    expect(result.current.backendStatus).toBe('online');

    stopped = true;
    nativePowerSaving = true;
    await act(() => result.current.handleStartBackend());
    expect(result.current.monitorPowerSavingSuppressed).toBe(true);
    const starts = () => invoke.mock.calls.filter(([command]) => command === 'start_monitor').length;
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(starts()).toBe(1);

    nativePowerSaving = false;
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(result.current.monitorPowerSavingSuppressed).toBe(false);
    expect(starts()).toBe(2);
  });

  it('recovers a stale battery event and retains automatic restart after backend recovery', async () => {
    vi.useFakeTimers();
    stopped = true;
    let nativePowerSaving = true;
    const originalInvoke = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, ...args) => {
      if (command === 'get_monitor_status') return JSON.stringify({ stopped, power_saving_active: nativePowerSaving });
      return originalInvoke(command, ...args);
    });
    const { result } = renderHook(() => useMonitorLifecycle({ ...options, powerSavingSuppressed: true }));
    await act(async () => {});
    const starts = () => invoke.mock.calls.filter(([command]) => command === 'start_monitor').length;
    expect(starts()).toBe(0);

    // The backend independently resumed; the WebView missed the AC event.
    nativePowerSaving = false;
    stopped = false;
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(result.current.backendStatus).toBe('online');
    expect(result.current.monitorPowerSavingSuppressed).toBe(false);
    act(() => {
      stopped = true;
      events.get('monitor-stopped')({ payload: { intentional: false } });
    });
    await act(async () => {});
    expect(starts()).toBe(1);
  });

  it('does not clear a newer native power rejection with an older status response', async () => {
    vi.useFakeTimers();
    const originalInvoke = invoke.getMockImplementation();
    let resolveStatus;
    let holdStatus = false;
    invoke.mockImplementation(async (command, ...args) => {
      if (command === 'get_monitor_status' && holdStatus) return new Promise((resolve) => { resolveStatus = resolve; });
      if (command === 'start_monitor') throw 'POWER_SAVING_ACTIVE';
      return originalInvoke(command, ...args);
    });
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    holdStatus = true;
    await act(() => vi.advanceTimersByTimeAsync(3000));
    await act(() => result.current.handleStartBackend());
    await act(async () => resolveStatus(JSON.stringify({ stopped: true, power_saving_active: false })));
    expect(result.current.monitorPowerSavingSuppressed).toBe(true);
    expect(invoke.mock.calls.filter(([command]) => command === 'start_monitor')).toHaveLength(1);
  });
});
