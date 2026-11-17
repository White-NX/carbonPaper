import { act, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useSelectedSnapshot } from './useSelectedSnapshot';

vi.mock('../lib/auth_api', () => ({
  withAuth: vi.fn(async (fn) => fn()), requestAuth: vi.fn(), checkAuthSession: vi.fn(),
  initAuthListeners: vi.fn(), lockSession: vi.fn(),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

const requests = [];
afterEach(async () => {
  // Settle every real queue entry even if an assertion failed.
  await act(async () => { for (const request of requests) request.resolve(null); });
  requests.length = 0;
});

it('fetches completed OCR through the real request queue without reusing an earlier pending response', async () => {
  const events = new Map();
  listen.mockImplementation(async (name, callback) => {
    events.set(name, callback);
    return vi.fn();
  });
  invoke.mockImplementation((command) => {
    if (command === 'storage_get_screenshot_details') {
      return new Promise((resolve) => requests.push({ resolve }));
    }
    return Promise.resolve({ status: 'success', data: 'image' });
  });
  const { result } = renderHook(() => useSelectedSnapshot());
  await act(async () => result.current.setSelectedEvent({ id: 8 }));
  await act(async () => requests[0].resolve({
    record: { id: 8, image_hash: 'original', status: 'committed' }, ocr_results: [],
  }));

  await act(async () => result.current.refreshEditedSnapshot({
    screenshot_id: 8, image_hash: 'edited', ocr_status: 'pending',
  }, 'edited image'));
  const status = { screenshot_id: 8, image_hash: 'edited', status: 'completed' };
  await act(async () => events.get('screenshot-ocr-updated')({ payload: status }));
  expect(result.current.selectedDetails.edit_ocr.status).toBe('completed');
  expect(requests).toHaveLength(3);

  // The saved image hash matches both responses, so only a fresh read and the
  // caller version can keep the older pending result from regressing the UI.
  await act(async () => requests[1].resolve({
    record: { id: 8, image_hash: 'edited', status: 'committed' },
    edit_ocr: { ...status, status: 'pending' }, ocr_results: [],
  }));
  expect(result.current.selectedDetails.edit_ocr.status).toBe('completed');
  await act(async () => requests[2].resolve({
    record: { id: 8, image_hash: 'edited', status: 'committed' },
    edit_ocr: status, ocr_results: [{ text: 'recognized text' }],
  }));
  expect(result.current.selectedDetails.edit_ocr.status).toBe('completed');
  expect(result.current.selectedDetails.ocr_results).toEqual([{ text: 'recognized text' }]);
  expect(result.current.selectedImageSrc).toBe('edited image');
});
