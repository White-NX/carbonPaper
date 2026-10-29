import { act, renderHook } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { useSnapshotPreviewData } from './useSnapshotPreviewData';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('../lib/monitor_api', () => ({ fetchImage: vi.fn(), getScreenshotDetails: vi.fn() }));
let images, details;
beforeEach(() => {
  images = []; details = [];
  fetchImage.mockReset().mockImplementation((id, path) => new Promise((resolve) => images.push({ id, path, resolve })));
  getScreenshotDetails.mockReset().mockImplementation((id, path) => new Promise((resolve, reject) => details.push({ id, path, resolve, reject })));
});

it('paints the image before slow details and retains it after a detail failure', async () => {
  const { result } = renderHook(() => useSnapshotPreviewData({ screenshot_id: 42 }));
  await act(async () => images[0].resolve('original'));
  expect(result.current.imageSrc).toBe('original'); expect(result.current.loading).toBe(true);
  await act(async () => details[0].reject(new Error('details failed')));
  expect(result.current.imageSrc).toBe('original'); expect(result.current.error).toBe('details failed');
  expect(result.current.loading).toBe(false);
});

it('ignores late results after switching, closing or locking the viewer', async () => {
  const { result, rerender } = renderHook(({ item, enabled }) => useSnapshotPreviewData(item, enabled), {
    initialProps: { item: { screenshot_id: 1 }, enabled: true },
  });
  rerender({ item: { screenshot_id: 2 }, enabled: true });
  await act(async () => { images[0].resolve('old'); details[0].resolve({ record: { id: 1 } }); });
  expect(result.current.imageSrc).toBeNull();
  await act(async () => images[1].resolve('current'));
  expect(result.current.imageSrc).toBe('current');
  rerender({ item: { screenshot_id: 2 }, enabled: false });
  await act(async () => details[1].resolve({ record: { id: 2 } }));
  expect(result.current.imageSrc).toBeNull(); expect(result.current.details).toBeNull();
  rerender({ item: { screenshot_id: 2 }, enabled: true });
  expect(images).toHaveLength(3);
  rerender({ item: null, enabled: true });
  await act(async () => { images[2].resolve('late after close'); details[2].resolve({ record: { id: 2 } }); });
  expect(result.current.imageSrc).toBeNull(); expect(result.current.details).toBeNull();
});

it('retries the same screenshot and handles legacy path-only items', async () => {
  const { result } = renderHook(() => useSnapshotPreviewData({ image_path: 'memory://legacy' }));
  expect(images[0]).toMatchObject({ id: null, path: 'memory://legacy' });
  await act(async () => { images[0].resolve(null); details[0].resolve({ error: 'missing' }); });
  expect(result.current.error).toBe('missing');
  act(() => result.current.retry());
  await act(async () => { images[1].resolve('recovered'); details[1].resolve({ record: {} }); });
  expect(result.current.error).toBeNull(); expect(result.current.imageSrc).toBe('recovered');
});
