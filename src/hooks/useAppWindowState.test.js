import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

import { useAppWindowActions, usePowerSavingState } from './useAppWindowState';

const events = vi.hoisted(() => new Map());
vi.mock('./useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => events.set(name, handler) }));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(),
}));

describe('useAppWindowActions', () => {
  const hide = vi.fn();

  beforeEach(() => {
    getCurrentWindow.mockReturnValue({
      hide,
      minimize: vi.fn(),
      toggleMaximize: vi.fn(),
    });
    invoke.mockResolvedValue(undefined);
  });

  it('uses the backend tray command so hiding starts the lightweight timer', async () => {
    const { result } = renderHook(() => useAppWindowActions());

    await act(async () => {
      await result.current.hideToTray();
    });

    expect(invoke).toHaveBeenCalledWith('hide_to_tray');
    expect(hide).not.toHaveBeenCalled();
  });
});

describe('initial power state', () => {
  beforeEach(() => {
    localStorage.clear();
    events.clear();
    getCurrentWindow.mockReturnValue({ listen: vi.fn().mockResolvedValue(vi.fn()) });
    invoke.mockReset();
  });

  it('does not publish a ready state until the power query finishes', async () => {
    let resolvePower;
    invoke.mockReturnValue(new Promise((resolve) => { resolvePower = resolve; }));
    const { result } = renderHook(() => usePowerSavingState());
    expect(result.current.powerStatusReady).toBe(false);
    await act(async () => resolvePower({ enabled: true, active: true, ac_connected: false }));
    expect(result.current.powerStatusReady).toBe(true);
    expect(result.current.powerSavingSuppressed).toBe(true);
  });

  it('keeps an unplug event that arrives before an older startup response', async () => {
    let resolvePower;
    invoke.mockReturnValue(new Promise((resolve) => { resolvePower = resolve; }));
    const { result } = renderHook(() => usePowerSavingState());
    act(() => events.get('power-saving-changed')({ payload: { enabled: true, active: true } }));
    await act(async () => resolvePower({ enabled: true, active: false, ac_connected: true }));
    await waitFor(() => expect(result.current.powerStatusReady).toBe(true));
    expect(result.current.powerSavingSuppressed).toBe(true);
  });
});
