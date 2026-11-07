import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useSettingsHost } from './useSettingsHost';

const events = vi.hoisted(() => new Map());
vi.mock('./useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => events.set(name, handler) }));
beforeEach(() => { events.clear(); invoke.mockReset().mockResolvedValue(null); });

describe('settings window notifications', () => {
  it('opens settings and follows the native close notification', async () => {
    const onClosed = vi.fn();
    const { result } = renderHook(() => useSettingsHost({ onClosed }));
    await act(() => result.current.openSettings('capture'));
    expect(result.current.showSettings).toBe(true);
    expect(invoke).toHaveBeenCalledWith('open_settings_window', { tab: 'capture', section: null });
    act(() => events.get('settings-window-changed')({ payload: false }));
    expect(result.current.showSettings).toBe(false);
    expect(onClosed).toHaveBeenCalledOnce();
  });

  it('refreshes records and model state after changes from the settings window', () => {
    const onRecordsChanged = vi.fn();
    const onClosed = vi.fn();
    renderHook(() => useSettingsHost({ onRecordsChanged, onClosed }));
    act(() => events.get('settings-preferences-changed')({ payload: ['storage', 'records', 'models'] }));
    expect(onRecordsChanged).toHaveBeenCalledOnce();
    expect(onClosed).toHaveBeenCalledOnce();
    expect(invoke).not.toHaveBeenCalled();
  });
});
