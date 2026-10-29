import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';
import { useSelectedSnapshot } from './useSelectedSnapshot';

vi.mock('../lib/monitor_api', () => ({ fetchImage: vi.fn(), getScreenshotDetails: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

let images;
let details;
beforeEach(() => {
  images = [];
  details = [];
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

it('drops loaded and in-flight data on lock and reloads the same selection after unlock', async () => {
  const { result, rerender } = renderHook(({ enabled }) => useSelectedSnapshot({ enabled }), { initialProps: { enabled: true } });
  act(() => result.current.setSelectedEvent({ id: 8 }));
  await act(async () => images[0].resolve('sensitive image'));
  rerender({ enabled: false });
  expect(result.current.selectedImageSrc).toBeNull();
  await act(async () => details[0].resolve({ record: { id: 8 } }));
  expect(result.current.selectedDetails).toBeNull();
  expect(result.current.selectedEvent.id).toBe(8);
  rerender({ enabled: true });
  expect(images).toHaveLength(2);
  await act(async () => { images[1].resolve('reloaded'); details[1].resolve({ record: { id: 8 } }); });
  expect(result.current.selectedImageSrc).toBe('reloaded');
});
