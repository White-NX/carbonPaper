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
  fetchSearchImage,
  fetchThumbnail,
  fetchTimelineImage,
  REQUEST_DEADLINES,
  searchScreenshots,
  listProcesses,
  getScreenshotDetails,
  resumeOfficeDocument,
  updateMonitorFilters,
  getTimeline,
  getTimelineDetails,
  fetchThumbnailBatch,
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

  it('cancels the exact timeline request and waits for backend acknowledgement', async () => {
    let finish;
    invoke.mockImplementation((command) => command === 'storage_get_timeline'
      ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve());
    const controller = new AbortController();
    const request = getTimeline(100, 200, 500, { signal: controller.signal });
    const settled = vi.fn();
    const handled = request.catch(settled);
    const params = invoke.mock.calls[0][1];
    expect(params).toMatchObject({ startTime: 100, endTime: 200, maxRecords: 500, requestId: expect.any(String) });
    controller.abort();
    await Promise.resolve();
    expect(invoke).toHaveBeenCalledWith('storage_cancel_timeline', { requestId: params.requestId });
    expect(settled).not.toHaveBeenCalled();
    finish([{ id: 1 }]);
    await handled;
    expect(settled.mock.calls[0][0].name).toBe('AbortError');
  });

  it('does not send a timeline read that was cancelled before admission', async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(getTimeline(100, 200, 500, { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
    expect(invoke).not.toHaveBeenCalled();
  });

  it('requests lightweight timeline headers only when the caller opts in', async () => {
    invoke.mockResolvedValue([]);
    await getTimeline(100, 200);
    expect(invoke.mock.calls[0][1]).not.toHaveProperty('includeDetails');
    await getTimeline(100, 200, 500, { includeDetails: false });
    expect(invoke).toHaveBeenLastCalledWith('storage_get_timeline', {
      startTime: 100, endTime: 200, maxRecords: 500, includeDetails: false,
      requestId: expect.any(String),
    });
    expectWithAuth(2);
  });

  it('cancels deferred timeline details and discards results after backend acknowledgement', async () => {
    let finish;
    invoke.mockImplementation((command) => command === 'storage_get_timeline_details'
      ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve());
    const controller = new AbortController();
    const request = getTimelineDetails([12, 34], { signal: controller.signal });
    const assertion = expect(request).rejects.toMatchObject({ name: 'AbortError' });
    const params = invoke.mock.calls[0][1];
    expect(params).toEqual({ ids: [12, 34], requestId: expect.any(String) });
    expectWithAuth(1);
    controller.abort();
    expect(invoke).toHaveBeenCalledWith('storage_cancel_timeline', { requestId: params.requestId });
    finish([{ id: 12 }]);
    await assertion;
  });

  it('does not admit deferred details or thumbnails with an already cancelled signal', async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(getTimelineDetails([1], { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
    await expect(fetchThumbnailBatch([1], { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
    expect(invoke).not.toHaveBeenCalled();
  });

  it('waits for thumbnail IPC to settle before rejecting a cancelled batch', async () => {
    let finish;
    invoke.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const controller = new AbortController();
    const settled = vi.fn();
    const request = fetchThumbnailBatch([1, 2], { signal: controller.signal }).catch(settled);
    controller.abort();
    await Promise.resolve();
    expect(settled).not.toHaveBeenCalled();
    finish({ results: { 1: { status: 'success', data: 'stale' } } });
    await request;
    expect(settled.mock.calls[0][0].name).toBe('AbortError');
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('maps successful thumbnail entries while retaining partial batch results', async () => {
    invoke.mockResolvedValue({ results: {
      1: { status: 'success', data: 'first' },
      2: { status: 'error', error: 'unavailable' },
      3: { status: 'success', mime_type: 'image/png', data: 'third' },
    } });
    await expect(fetchThumbnailBatch([1, 2, 3])).resolves.toEqual({
      1: 'data:image/jpeg;base64,first', 3: 'data:image/png;base64,third',
    });
    expectWithAuth(1);
  });

  it('calls Rust visual search with the stable command payload', async () => {
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

  it('lets visual grids omit OCR without changing the default text contract', async () => {
    invoke.mockResolvedValue({ results: [{ id: 1, ocr_text: '' }] });
    await searchScreenshots('game', 'nl', { includeOcr: false });
    expect(invoke).toHaveBeenCalledWith('monitor_search_nl', expect.objectContaining({
      query: 'game', includeOcr: false,
    }));
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

  it('resumes an Office document through the authenticated command', async () => {
    invoke.mockResolvedValue({ status: 'opened' });

    await expect(resumeOfficeDocument(42)).resolves.toEqual({ status: 'opened' });
    expect(invoke).toHaveBeenCalledWith('office_resume_document', { screenshotId: 42 });
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

  it('loads picture-search originals through the authenticated image command', async () => {
    invoke.mockResolvedValue({ status: 'success', data: 'original', mime_type: 'image/jpeg' });
    await expect(fetchSearchImage(42)).resolves.toBe('data:image/jpeg;base64,original');
    expect(invoke).toHaveBeenCalledWith('storage_get_image', { id: 42, path: null });
    expectWithAuth(1);
    invoke.mockResolvedValue({ status: 'success', data: 'legacy' });
    await expect(fetchSearchImage(null, 'memory://hash')).resolves.toBe('data:image/png;base64,legacy');
    expect(invoke).toHaveBeenLastCalledWith('storage_get_image', { id: null, path: 'memory://hash' });
  });

  it('does not load cancelled originals before admission or after waiting for authentication', async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(fetchSearchImage(1, null, { signal: controller.signal })).rejects.toMatchObject({ name: 'AbortError' });
    expect(withAuth).not.toHaveBeenCalled();
    const waiting = new AbortController();
    withAuth.mockImplementationOnce(async (fn) => { waiting.abort(); return fn(); });
    await expect(fetchSearchImage(2, null, { signal: waiting.signal })).rejects.toMatchObject({ name: 'AbortError' });
    expect(invoke).not.toHaveBeenCalled();
  });

  it('limits search originals to two active reads, skips cancelled queued cards and keeps previews available', async () => {
    const finishes = new Map();
    invoke.mockImplementation((_command, { id }) => new Promise((resolve) => finishes.set(id, resolve)));
    const firstController = new AbortController();
    const thirdController = new AbortController();
    const first = fetchSearchImage(1, null, { signal: firstController.signal });
    const second = fetchSearchImage(2);
    const third = fetchSearchImage(3, null, { signal: thirdController.signal });
    const fourth = fetchSearchImage(4);
    const firstAssertion = expect(first).rejects.toMatchObject({ name: 'AbortError' });
    const thirdAssertion = expect(third).rejects.toMatchObject({ name: 'AbortError' });
    expect([...finishes.keys()]).toEqual([1, 2]);
    firstController.abort();
    thirdController.abort();
    await Promise.resolve();
    expect([...finishes.keys()]).toEqual([1, 2]);
    const preview = fetchImage(5);
    expect(finishes.has(5)).toBe(true);
    finishes.get(1)({ status: 'success', data: 'discarded' });
    await firstAssertion;
    await thirdAssertion;
    await sleep(0);
    expect([...finishes.keys()]).toEqual([1, 2, 5, 4]);
    for (const id of [2, 4, 5]) finishes.get(id)({ status: 'success', data: `image-${id}` });
    await expect(Promise.all([second, fourth, preview])).resolves.toEqual([
      'data:image/png;base64,image-2', 'data:image/png;base64,image-4', 'data:image/png;base64,image-5',
    ]);
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
