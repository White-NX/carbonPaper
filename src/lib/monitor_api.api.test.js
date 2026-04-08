import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('./auth_api', () => ({
  withAuth: async (fn) => fn(),
  requestAuth: vi.fn(),
  checkAuthSession: vi.fn(),
  initAuthListeners: vi.fn(),
  lockSession: vi.fn(),
}));

import {
  searchScreenshots,
  listProcesses,
  getScreenshotDetails,
  updateMonitorFilters,
} from './monitor_api';

describe('monitor_api command wrappers', () => {
  let consoleErrorSpy;

  beforeEach(() => {
    invoke.mockReset();
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
  });

  it('calls nl search with monitor command payload', async () => {
    invoke.mockResolvedValue({ results: [{ id: 1 }] });

    const results = await searchScreenshots('invoice', 'nl', {
      limit: 5,
      offset: 2,
      processNames: ['chrome.exe'],
      startTime: 10,
      endTime: 20,
      fuzzy: false,
    });

    expect(results).toEqual([{ id: 1 }]);
    expect(invoke).toHaveBeenCalledWith('execute_monitor_command', {
      payload: {
        command: 'search_nl',
        query: 'invoice',
        limit: 5,
        offset: 2,
        process_names: ['chrome.exe'],
        start_time: 10,
        end_time: 20,
        fuzzy: false,
      },
    });
  });

  it('throws for nl search when backend returns error', async () => {
    invoke.mockResolvedValue({ error: 'bad_request' });

    await expect(searchScreenshots('x', 'nl')).rejects.toThrow('bad_request');
  });

  it('calls ocr search with normalized filters', async () => {
    invoke.mockResolvedValue([{ screenshot_id: 1 }]);

    const results = await searchScreenshots('hello', 'ocr', {
      processNames: [],
      categories: [],
    });

    expect(results).toEqual([{ screenshot_id: 1 }]);
    expect(invoke).toHaveBeenCalledWith('storage_search', {
      query: 'hello',
      limit: 20,
      offset: 0,
      fuzzy: true,
      processNames: null,
      categories: null,
      startTime: null,
      endTime: null,
    });
  });

  it('returns empty array when listProcesses invoke fails', async () => {
    invoke.mockRejectedValue(new Error('pipe error'));

    await expect(listProcesses()).resolves.toEqual([]);
  });

  it('returns normalized error object when getScreenshotDetails throws', async () => {
    invoke.mockRejectedValue(new Error('boom'));

    await expect(getScreenshotDetails(1)).resolves.toEqual({ error: 'Error: boom' });
  });

  it('maps unknown command to unsupported code in updateMonitorFilters', async () => {
    invoke.mockResolvedValue({ error: 'unknown command' });

    await expect(updateMonitorFilters({})).rejects.toMatchObject({ code: 'unsupported' });
  });
});
