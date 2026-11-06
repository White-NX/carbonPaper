import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useMonitorLifecycle } from './useMonitorLifecycle';

const events = vi.hoisted(() => new Map());
vi.mock('./useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => events.set(name, handler) }));
let nativeStatus;
const options = {
  powerSavingSuppressed: false,
  formatErrorDetails: String,
  reportBackendError: vi.fn(),
  resetBackendErrorDedupe: vi.fn(),
  t: (key) => key,
};
beforeEach(() => {
  events.clear();
  nativeStatus = { stopped: true, paused: false, starting: false, power_saving_active: false, error: null, revision: 0 };
  options.reportBackendError.mockClear();
  options.resetBackendErrorDedupe.mockClear();
  invoke.mockReset().mockImplementation(async (command, { action } = {}) => {
    if (command === 'get_monitor_status') return JSON.stringify(nativeStatus);
    if (command === 'settings_monitor_action') {
      nativeStatus = {
        ...nativeStatus, revision: nativeStatus.revision + 1, error: null,
        stopped: action === 'stop', paused: action === 'pause',
      };
      return null;
    }
    throw new Error('Unexpected command: ' + command);
  });
});
afterEach(() => vi.useRealTimers());

describe('application-owned capture lifecycle', () => {
  it('observes a stopped runtime through mounting, power changes and WebView recreation without starting it', async () => {
    vi.useFakeTimers();
    nativeStatus.auto_start_suppressed = true;
    const hook = renderHook((props) => useMonitorLifecycle({ ...options, ...props }), {
      initialProps: { powerSavingSuppressed: false },
    });
    await act(async () => {});
    expect(hook.result.current.backendStatus).toBe('offline');
    hook.rerender({ powerSavingSuppressed: true });
    hook.rerender({ powerSavingSuppressed: false });
    await act(() => vi.advanceTimersByTimeAsync(30000));
    hook.unmount();
    const reopened = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    expect(reopened.result.current.backendStatus).toBe('offline');
    expect(invoke.mock.calls.every(([command]) => command === 'get_monitor_status')).toBe(true);
  });

  it('displays native startup, recovery and a retained retry limit', async () => {
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await waitFor(() => expect(result.current.backendStatus).toBe('offline'));
    act(() => events.get('monitor-status-changed')({ payload: { ...nativeStatus, starting: true, revision: 1 } }));
    expect(result.current.backendStatus).toBe('waiting');
    act(() => events.get('monitor-status-changed')({ payload: { ...nativeStatus, stopped: false, revision: 2 } }));
    expect(result.current.backendStatus).toBe('online');
    act(() => events.get('monitor-status-changed')({
      payload: { ...nativeStatus, error: 'CAPTURE_RESTART_LIMIT', auto_start_suppressed: true, revision: 3 },
    }));
    expect(result.current.backendStatus).toBe('offline');
    expect(result.current.backendError).toBe('settings.general.monitor.errors.restartLimit');
    expect(invoke.mock.calls.every(([command]) => command === 'get_monitor_status')).toBe(true);
  });

  it('forwards explicit start, pause and resume through the native action boundary', async () => {
    const { result } = renderHook(() => useMonitorLifecycle(options));
    await waitFor(() => expect(result.current.backendStatus).toBe('offline'));
    await act(() => result.current.handleStartBackend());
    expect(result.current.backendStatus).toBe('online');
    await act(() => result.current.handlePauseMonitor());
    expect(result.current.monitorPaused).toBe(true);
    await act(() => result.current.handleResumeMonitor());
    expect(result.current.monitorPaused).toBe(false);
    expect(invoke.mock.calls.filter(([command]) => command === 'settings_monitor_action').map(([, args]) => args.action))
      .toEqual(['start', 'pause', 'resume']);
  });

  it('ignores an older status response after a newer native event', async () => {
    vi.useFakeTimers();
    const hook = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    let finishRead;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }));
    await act(() => vi.advanceTimersByTimeAsync(3000));
    act(() => events.get('monitor-status-changed')({ payload: { ...nativeStatus, stopped: false, revision: 2 } }));
    await act(async () => finishRead(JSON.stringify(nativeStatus)));
    expect(hook.result.current.backendStatus).toBe('online');
  });

  it('waits for native power recovery without a failure notification or a frontend restart', async () => {
    vi.useFakeTimers();
    const hook = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    invoke.mockRejectedValueOnce('POWER_SAVING_ACTIVE');
    await act(() => hook.result.current.handleStartBackend());
    expect(hook.result.current.monitorPowerSavingSuppressed).toBe(true);
    expect(options.reportBackendError).not.toHaveBeenCalled();
    nativeStatus = { ...nativeStatus, stopped: false, power_saving_active: false, revision: 1 };
    await act(() => vi.advanceTimersByTimeAsync(3000));
    expect(hook.result.current.monitorPowerSavingSuppressed).toBe(false);
    expect(hook.result.current.backendStatus).toBe('online');
    expect(invoke.mock.calls.filter(([command]) => command === 'settings_monitor_action')).toHaveLength(1);
  });

  it('does not clear a newer power rejection with an older status response', async () => {
    vi.useFakeTimers();
    const hook = renderHook(() => useMonitorLifecycle(options));
    await act(async () => {});
    let finishRead;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }));
    await act(() => vi.advanceTimersByTimeAsync(3000));
    invoke.mockRejectedValueOnce('POWER_SAVING_ACTIVE');
    await act(() => hook.result.current.handleStartBackend());
    await act(async () => finishRead(JSON.stringify(nativeStatus)));
    expect(hook.result.current.monitorPowerSavingSuppressed).toBe(true);
    expect(invoke.mock.calls.filter(([command]) => command === 'settings_monitor_action')).toHaveLength(1);
  });
});
