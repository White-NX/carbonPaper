import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  fetchThumbnailBatch,
  getIndexHealth,
  getProcessStorageStats,
  getSoftDeleteQueueStatus,
} from '../../lib/monitor_api';
import { useStorageManagementController } from './useStorageManagementController';

vi.mock('../../lib/auth_api', () => ({
  withAuth: vi.fn((fn) => fn()),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
}));

vi.mock('../../lib/monitor_api', () => ({
  fetchThumbnailBatch: vi.fn(async () => ({})),
  getIndexHealth: vi.fn(async () => ({ status: 'success', monitor_available: false })),
  getProcessMonthlyThumbnails: vi.fn(),
  getProcessStorageStats: vi.fn(async () => []),
  getSoftDeleteQueueStatus: vi.fn(async () => ({ pending_screenshots: 0, pending_ocr: 0, running: false })),
  retryVectorIndexing: vi.fn(),
  softDeleteProcessMonth: vi.fn(),
  softDeleteScreenshots: vi.fn(),
}));

const t = (_key, fallback) => fallback || _key;

describe('useStorageManagementController', () => {
  beforeEach(() => {
    localStorage.clear();
    fetchThumbnailBatch.mockClear();
    getIndexHealth.mockClear();
    getProcessStorageStats.mockClear();
    getSoftDeleteQueueStatus.mockClear();
  });

  it('loads index health diagnostics even when the monitor is stopped', async () => {
    getIndexHealth.mockResolvedValueOnce({
      status: 'success',
      monitor_available: false,
      screenshots_count: 12,
      ocr_rows_count: 34,
    });

    const { result } = renderHook(() => useStorageManagementController({
      storage: { root_path: 'C:/CarbonPaper' },
      onRefresh: vi.fn(),
      t,
      monitorStatus: 'stopped',
    }));

    await waitFor(() => expect(getIndexHealth).toHaveBeenCalledWith({ refreshVector: false }));
    expect(result.current.indexHealth).toMatchObject({
      monitor_available: false,
      screenshots_count: 12,
      ocr_rows_count: 34,
    });
  });

  it('does not request vector refresh from the overview refresh when the monitor is stopped', async () => {
    const onRefresh = vi.fn();
    const { result } = renderHook(() => useStorageManagementController({
      storage: { root_path: 'C:/CarbonPaper' },
      onRefresh,
      t,
      monitorStatus: 'stopped',
    }));

    await waitFor(() => expect(getIndexHealth).toHaveBeenCalledWith({ refreshVector: false }));
    getIndexHealth.mockClear();

    act(() => {
      result.current.handleRefresh();
    });

    await waitFor(() => expect(getIndexHealth).toHaveBeenCalledWith({ refreshVector: false }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });
});
