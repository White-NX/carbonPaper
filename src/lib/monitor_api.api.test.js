import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('./auth_api', () => ({
  withAuth: vi.fn(async (fn) => fn()),
  requestAuth: vi.fn(),
  checkAuthSession: vi.fn(),
  initAuthListeners: vi.fn(),
  lockSession: vi.fn(),
}));

import { withAuth } from './auth_api';
import {
  fetchImage,
  fetchThumbnail,
  fetchTimelineImage,
  REQUEST_DEADLINES,
  searchScreenshots,
  listProcesses,
  getScreenshotDetails,
  updateMonitorFilters,
} from './monitor_api';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

describe('monitor_api command wrappers', () => {
  let consoleErrorSpy;

  beforeEach(() => {
    invoke.mockReset();
    withAuth.mockClear();
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    consoleErrorSpy.mockRestore();
  });

  const expectWithAuth = (callNumber, options) => {
    const call = withAuth.mock.calls[callNumber - 1];
    expect(call?.[0]).toEqual(expect.any(Function));
    if (options === undefined) {
      expect(call).toHaveLength(1);
    } else {
      expect(call?.[1]).toEqual(options);
    }
  };

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
    expect(invoke).toHaveBeenCalledWith('monitor_search_nl', {
      query: 'invoice',
      limit: 5,
      offset: 2,
      processNames: ['chrome.exe'],
      startTime: 10,
      endTime: 20,
      fuzzy: false,
    });
    expectWithAuth(1);
  });

  it('throws for nl search when backend returns error', async () => {
    invoke.mockResolvedValue({ error: 'bad_request' });

    await expect(searchScreenshots('x', 'nl')).rejects.toThrow('bad_request');
    expectWithAuth(1);
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
    expectWithAuth(1);
  });

  it('returns empty array when listProcesses invoke fails', async () => {
    invoke.mockRejectedValue(new Error('pipe error'));

    await expect(listProcesses()).resolves.toEqual([]);
    expectWithAuth(1);
  });

  it('returns normalized error object when getScreenshotDetails throws', async () => {
    invoke.mockRejectedValue(new Error('boom'));

    await expect(getScreenshotDetails(1)).resolves.toEqual({ error: 'Error: boom' });
    expectWithAuth(1);
  });

  it('maps unknown command to unsupported code in updateMonitorFilters', async () => {
    invoke.mockResolvedValue({ error: 'unknown command' });

    await expect(updateMonitorFilters({})).rejects.toMatchObject({ code: 'unsupported' });
    expectWithAuth(1, { autoPrompt: true });
  });

  it('dedupes concurrent full image requests by id', async () => {
    invoke.mockImplementation(async () => {
      await sleep(5);
      return { status: 'success', data: 'abc', mime_type: 'image/png' };
    });

    const [first, second] = await Promise.all([fetchImage(42), fetchImage(42)]);

    expect(first).toBe('data:image/png;base64,abc');
    expect(second).toBe(first);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('storage_get_image', { id: 42, path: null });
    expectWithAuth(1);
  });

  it('rejects full image requests that exceed the queue deadline', async () => {
    vi.useFakeTimers();
    invoke.mockImplementation(() => new Promise(() => {}));

    const request = fetchImage(9001);
    const assertion = expect(request).rejects.toMatchObject({ code: 'deadline_exceeded' });

    await vi.advanceTimersByTimeAsync(REQUEST_DEADLINES.imageMs + 1);

    await assertion;
    expect(invoke).toHaveBeenCalledWith('storage_get_image', { id: 9001, path: null });
    expectWithAuth(1);
  });

  it('rejects thumbnail requests that exceed the queue deadline', async () => {
    vi.useFakeTimers();
    invoke.mockImplementation(() => new Promise(() => {}));

    const request = fetchThumbnail(9003);
    const assertion = expect(request).rejects.toMatchObject({ code: 'deadline_exceeded' });

    await vi.advanceTimersByTimeAsync(REQUEST_DEADLINES.thumbnailMs + 1);

    await assertion;
    expect(invoke).toHaveBeenCalledWith('storage_get_thumbnail', { id: 9003, path: null });
    expectWithAuth(1);
  });

  it('rejects timeline image requests that exceed the queue deadline', async () => {
    vi.useFakeTimers();
    invoke.mockImplementation(() => new Promise(() => {}));

    const request = fetchTimelineImage(9004);
    const assertion = expect(request).rejects.toMatchObject({ code: 'deadline_exceeded' });

    await vi.advanceTimersByTimeAsync(REQUEST_DEADLINES.timelineImageMs + 1);

    await assertion;
    expect(invoke).toHaveBeenCalledWith('storage_get_thumbnail', { id: 9004, path: null });
    expectWithAuth(1);
  });

  it('returns an error object when screenshot details exceed the queue deadline', async () => {
    vi.useFakeTimers();
    invoke.mockImplementation(() => new Promise(() => {}));

    const request = getScreenshotDetails(9002);
    const assertion = expect(request).resolves.toEqual({
      error: `Error: deadline exceeded after ${REQUEST_DEADLINES.detailMs}ms`,
    });

    await vi.advanceTimersByTimeAsync(REQUEST_DEADLINES.detailMs + 1);

    await assertion;
    expect(invoke).toHaveBeenCalledWith('storage_get_screenshot_details', { id: 9002, path: null });
    expectWithAuth(1);
  });

  it('dedupes concurrent thumbnail requests by path', async () => {
    invoke.mockImplementation(async () => {
      await sleep(5);
      return { status: 'success', data: 'thumb', mime_type: 'image/jpeg' };
    });

    const [first, second] = await Promise.all([
      fetchThumbnail(null, 'D:/shots/a.jpg'),
      fetchThumbnail(null, 'D:/shots/a.jpg'),
    ]);

    expect(first).toBe('data:image/jpeg;base64,thumb');
    expect(second).toBe(first);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('storage_get_thumbnail', { id: null, path: 'D:/shots/a.jpg' });
    expectWithAuth(1);
  });
});
