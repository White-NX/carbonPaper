import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';
import { useSelectedSnapshot } from './useSelectedSnapshot';
import { listen } from '@tauri-apps/api/event';

vi.mock('../lib/monitor_api', () => ({ fetchImage: vi.fn(), getScreenshotDetails: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

let images;
let details;
let events;
beforeEach(() => {
  images = [];
  details = [];
  events = new Map();
  listen.mockImplementation(async (name, callback) => {
    events.set(name, callback);
    return vi.fn();
  });
  fetchImage.mockReset().mockImplementation((id) => new Promise((resolve, reject) => {
    images.push({ id, resolve, reject });
  }));
  getScreenshotDetails.mockReset().mockImplementation((id) => new Promise((resolve) => {
    details.push({ id, resolve });
  }));
  vi.spyOn(console, 'error').mockImplementation(() => {});
});
afterEach(() => vi.restoreAllMocks());

it('shows the original image while OCR and metadata are still pending', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 1 }));
  await act(async () => images[0].resolve('data:image/png;base64,image'));
  expect(result.current.selectedImageSrc).toBe('data:image/png;base64,image');
  expect(result.current.selectedDetails).toBeNull();
  expect(result.current.isLoadingDetails).toBe(true);
  const response = { record: { id: 1 }, ocr_results: [] };
  await act(async () => details[0].resolve(response));
  expect(result.current.selectedDetails).toEqual(response);
  expect(result.current.isLoadingDetails).toBe(false);
});

it('retains the image when metadata fails independently', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 2 }));
  await act(async () => details[0].resolve({ error: 'metadata unavailable' }));
  await act(async () => images[0].resolve('data:image/png;base64,image'));
  expect(result.current.selectedImageSrc).toBe('data:image/png;base64,image');
  expect(result.current.lastError).toBe('metadata unavailable');
  expect(result.current.isLoadingDetails).toBe(false);
});

it('clears old details and ignores stale image and metadata results after selection changes', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 3 }));
  await act(async () => details[0].resolve({ record: { id: 3 } }));
  expect(result.current.selectedDetails.record.id).toBe(3);
  act(() => result.current.setSelectedEvent({ id: 4 }));
  expect(result.current.selectedDetails).toBeNull();
  await act(async () => images[0].resolve('old image'));
  expect(result.current.selectedImageSrc).toBeNull();
  act(() => result.current.setSelectedEvent({ id: 5 }));
  await act(async () => {
    images[1].resolve('another old image');
    details[1].resolve({ record: { id: 4 } });
  });
  expect(result.current.selectedDetails).toBeNull();
  expect(result.current.selectedImageSrc).toBeNull();
  await act(async () => {
    images[2].resolve('current image');
    details[2].resolve({ record: { id: 5 } });
  });
  expect(result.current.selectedImageSrc).toBe('current image');
  expect(result.current.selectedDetails.record.id).toBe(5);
});

it('stops the loading state when selection is cleared and discards late results', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 6 }));
  expect(result.current.isLoadingDetails).toBe(true);
  act(() => result.current.clearSelection());
  expect(result.current.isLoadingDetails).toBe(false);
  await act(async () => {
    images[0].resolve('old image');
    details[0].resolve({ record: { id: 6 } });
  });
  expect(result.current.selectedImageSrc).toBeNull();
  expect(result.current.selectedDetails).toBeNull();
});

it('still corrects the timeline position from the stored UTC capture time', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 7, _fromNlSearch: true, timestamp: 0 }));
  await act(async () => images[0].resolve('current image'));
  await act(async () => details[0].resolve({ record: { created_at: '2026-01-01T00:00:00Z' } }));
  expect(result.current.timelineJump.time).toBe(Date.parse('2026-01-01T00:00:00Z'));
});

it('publishes the edited image and clears old OCR before asynchronous detail refresh', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 8 }));
  await act(async () => {
    images[0].resolve('old image');
    details[0].resolve({ record: { id: 8, image_hash: 'old', status: 'committed' }, ocr_results: [{ text: 'old private text' }] });
  });
  act(() => result.current.refreshEditedSnapshot({ screenshot_id: 8, image_hash: 'new', image_path: 'new.enc', ocr_status: 'pending' }, 'new image'));
  expect(result.current.selectedImageSrc).toBe('new image');
  expect(result.current.selectedDetails.ocr_results).toEqual([]);
  expect(result.current.selectedDetails.edit_ocr.status).toBe('pending');
  act(() => events.get('screenshot-ocr-updated')({ payload: { screenshot_id: 8, image_hash: 'old', status: 'completed' } }));
  expect(details).toHaveLength(2);
  act(() => events.get('screenshot-ocr-updated')({ payload: { screenshot_id: 8, image_hash: 'new', status: 'completed' } }));
  await act(async () => details[2].resolve({ record: { id: 8, image_hash: 'new' }, ocr_results: [{ text: 'new text' }] }));
  await act(async () => details[1].resolve({ record: { id: 8, image_hash: 'old' }, ocr_results: [{ text: 'old private text' }] }));
  expect(result.current.selectedDetails.ocr_results).toEqual([{ text: 'new text' }]);
  expect(result.current.selectedImageSrc).toBe('new image');
});

it('ignores a saved-image refresh after leaving and returning to the same snapshot', async () => {
  const { result } = renderHook(() => useSelectedSnapshot());
  act(() => result.current.setSelectedEvent({ id: 9 }));
  await act(async () => {
    images[0].resolve('old image');
    details[0].resolve({ record: { id: 9, image_hash: 'old' }, ocr_results: [] });
  });
  act(() => result.current.refreshEditedSnapshot({ screenshot_id: 9, image_hash: 'new', image_path: 'new.enc' }, 'new image'));
  act(() => result.current.setSelectedEvent({ id: 10 }));
  act(() => result.current.setSelectedEvent({ id: 9 }));
  await act(async () => details[1].resolve({ record: { id: 9, image_hash: 'new' }, ocr_results: [{ text: 'late refresh' }] }));
  expect(result.current.selectedDetails).toBeNull();
  await act(async () => {
    images[2].resolve('current image');
    details[3].resolve({ record: { id: 9, image_hash: 'new' }, ocr_results: [{ text: 'current text' }] });
  });
  expect(result.current.selectedDetails.ocr_results).toEqual([{ text: 'current text' }]);
});
