import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useSettingsHost } from './useSettingsHost';

const events = vi.hoisted(() => new Map());
vi.mock('./useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => events.set(name, handler) }));
beforeEach(() => { events.clear(); invoke.mockReset().mockResolvedValue(null); });

describe('settings monitor request boundary', () => {
  it('ignores events without a native pending request', async () => {
    const onMonitorAction = vi.fn();
    renderHook(() => useSettingsHost({ onMonitorAction }));
    await act(() => events.get('settings-monitor-request')({ payload: { id: 99, action: 'stop' } }));
    expect(onMonitorAction).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith('complete_settings_monitor_action', expect.anything());
  });
  it('executes the claimed action instead of an action supplied in the event', async () => {
    invoke.mockImplementation(async (command) => command === 'take_settings_monitor_action' ? 'pause' : null);
    const onMonitorAction = vi.fn(async () => {});
    renderHook(() => useSettingsHost({ onMonitorAction }));
    await act(() => events.get('settings-monitor-request')({ payload: { id: 1, action: 'stop' } }));
    expect(onMonitorAction).toHaveBeenCalledWith('pause');
    expect(invoke).toHaveBeenCalledWith('complete_settings_monitor_action', { id: 1, error: null });
  });
});
