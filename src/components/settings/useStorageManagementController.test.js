import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

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
    invoke.mockReset();
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

  it('does not push cached storage policy to the backend on mount', async () => {
    localStorage.setItem('snapshotRetentionPeriod', '1month');
    localStorage.setItem('snapshotStorageLimit', '10');

    renderHook(() => useStorageManagementController({
      storage: { root_path: 'C:/CarbonPaper' },
      onRefresh: vi.fn(),
      t,
      monitorStatus: 'stopped',
    }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_get_policy'));
    expect(invoke).not.toHaveBeenCalledWith('storage_set_policy', expect.anything());
  });

  it('lets the backend policy override stale cached values, treating missing as disabled', async () => {
    localStorage.setItem('snapshotRetentionPeriod', '1month');
    localStorage.setItem('snapshotStorageLimit', '10');
    invoke.mockImplementation(async (cmd) => {
      if (cmd === 'storage_get_policy') return { storage_limit: '50' };
      return undefined;
    });

    const { result } = renderHook(() => useStorageManagementController({
      storage: { root_path: 'C:/CarbonPaper' },
      onRefresh: vi.fn(),
      t,
      monitorStatus: 'stopped',
    }));

    await waitFor(() => expect(result.current.storageLimit).toBe('50'));
    expect(result.current.retentionPeriod).toBe('permanent');
    expect(localStorage.getItem('snapshotStorageLimit')).toBe('50');
    expect(localStorage.getItem('snapshotRetentionPeriod')).toBe('permanent');
    expect(invoke).not.toHaveBeenCalledWith('storage_set_policy', expect.anything());
  });

  it('persists the policy only on an explicit user change', async () => {
    const { result } = renderHook(() => useStorageManagementController({
      storage: { root_path: 'C:/CarbonPaper' },
      onRefresh: vi.fn(),
      t,
      monitorStatus: 'stopped',
    }));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith('storage_get_policy'));

    await act(async () => {
      await result.current.setRetentionPeriod('6months');
    });

    expect(result.current.retentionPeriod).toBe('6months');
    expect(localStorage.getItem('snapshotRetentionPeriod')).toBe('6months');
    expect(invoke).toHaveBeenCalledWith('storage_set_policy', {
      policy: { storage_limit: 'unlimited', retention_period: '6months' },
    });
  });
});
