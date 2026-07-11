import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { downloadAndInstallUpdate } from './update_api';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

describe('downloadAndInstallUpdate', () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockReset();
  });

  it('reports explicit phases after download reaches 100 percent', async () => {
    const unlistenProgress = vi.fn();
    const unlistenPhase = vi.fn();
    const progressEvents = [];
    const listeners = {};

    listen.mockImplementation(async (name, handler) => {
      listeners[name] = handler;
      return name === 'updater-download-progress' ? unlistenProgress : unlistenPhase;
    });
    invoke.mockImplementation(async (command) => {
      if (command === 'updater_install') {
        listeners['updater-phase']?.({ payload: { phase: 'extracting' } });
        listeners['updater-phase']?.({ payload: { phase: 'applying' } });
      }
    });

    await downloadAndInstallUpdate((progress) => {
      progressEvents.push(progress);
    });

    expect(listen).toHaveBeenCalledWith('updater-download-progress', expect.any(Function));
    expect(listen).toHaveBeenCalledWith('updater-phase', expect.any(Function));
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['updater_install']);
    expect(progressEvents).toEqual([
      { phase: 'downloading', downloaded: 0, contentLength: 0 },
      { phase: 'extracting', downloaded: 1, contentLength: 1 },
      { phase: 'applying', downloaded: 1, contentLength: 1 },
    ]);
    expect(unlistenProgress).toHaveBeenCalledOnce();
    expect(unlistenPhase).toHaveBeenCalledOnce();
  });
});
