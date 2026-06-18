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
    const unlisten = vi.fn();
    const progressEvents = [];

    listen.mockResolvedValue(unlisten);
    invoke.mockResolvedValue(undefined);

    await downloadAndInstallUpdate((progress) => {
      progressEvents.push(progress);
    });

    expect(listen).toHaveBeenCalledWith('updater-download-progress', expect.any(Function));
    expect(invoke.mock.calls.map(([command]) => command)).toEqual([
      'updater_download',
      'updater_extract',
      'updater_apply',
    ]);
    expect(progressEvents).toEqual([
      { phase: 'downloading', downloaded: 0, contentLength: 0 },
      { phase: 'extracting', downloaded: 1, contentLength: 1 },
      { phase: 'applying', downloaded: 1, contentLength: 1 },
    ]);
    expect(unlisten).toHaveBeenCalledOnce();
  });
});
