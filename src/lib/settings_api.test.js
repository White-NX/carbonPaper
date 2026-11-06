import { afterEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { emitTo } from '@tauri-apps/api/event';
import { openSettingsWindow, runMonitorAction } from './settings_api';

vi.mock('@tauri-apps/api/event', () => ({ emitTo: vi.fn(async () => {}) }));
afterEach(() => window.history.replaceState({}, '', '/'));
describe('settings navigation', () => {
  it('opens the native settings window from the main UI', async () => {
    await openSettingsWindow('organize', 'daily-recap');
    expect(invoke).toHaveBeenCalledWith('open_settings_window', { tab: 'organize', section: 'daily-recap' });
    expect(emitTo).not.toHaveBeenCalled();
  });
  it('navigates locally inside settings without invoking the main-only command', async () => {
    window.history.replaceState({}, '', '/?window=settings');
    await openSettingsWindow('ai');
    expect(emitTo).toHaveBeenCalledWith('settings', 'settings-navigate', { tab: 'ai', section: null });
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe('native monitor actions', () => {
  it.each(['/', '/?window=settings'])('restarts atomically from %s without using a main-window bridge', async (url) => {
    window.history.replaceState({}, '', url);
    await runMonitorAction('restart');
    expect(invoke.mock.calls).toEqual([['settings_monitor_action', { action: 'restart' }]]);
    expect(emitTo).not.toHaveBeenCalled();
  });

  it('rejects arbitrary commands before reaching the native boundary', async () => {
    await expect(runMonitorAction('exit_app')).rejects.toThrow('INVALID_MONITOR_ACTION');
    expect(invoke).not.toHaveBeenCalled();
  });
});
