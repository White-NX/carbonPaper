import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import {
  getSmartClusterWorkerStatus,
  getSoftDeleteQueueStatus,
  searchScreenshots,
} from '../lib/monitor_api';
import { useDepsSyncOverlay } from './useDepsSyncOverlay';
import { useRequiredModelDownload } from './useRequiredModelDownload';
import { useSearchBoxController } from './useSearchBoxController';
import { useTauriEventListener } from './useTauriEventListener';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock('../lib/monitor_api', () => ({
  fetchThumbnailBatch: vi.fn(async () => ({})),
  getSmartClusterWorkerStatus: vi.fn(async () => ({ pending_count: 0, running: false })),
  getSoftDeleteQueueStatus: vi.fn(async () => ({ pending_screenshots: 0, pending_ocr: 0, running: false })),
  searchScreenshots: vi.fn(async () => []),
}));

vi.mock('../lib/task_api', () => ({
  smartClusterStopDrain: vi.fn(async () => ({})),
}));

const t = (key, values) => (values ? `${key}:${JSON.stringify(values)}` : key);

describe('startup and search recovery hooks', () => {
  beforeEach(() => {
    invoke.mockImplementation(async (command) => {
      if (command === 'get_advanced_config') return { smart_cluster_enabled: false, use_onnx: false };
      if (command === 'storage_check_hmac_migration_status') return { needs_migration: false, is_running: false };
      return null;
    });
    listen.mockResolvedValue(vi.fn());
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('clears SearchBox loading when the query is emptied while a request is pending', async () => {
    vi.useFakeTimers();
    searchScreenshots.mockReturnValue(new Promise(() => {}));

    const { result, unmount } = renderHook(() => useSearchBoxController({
      onSelectResult: vi.fn(),
      onSubmit: vi.fn(),
      backendOnline: true,
      monitorPaused: false,
      handlePauseMonitor: vi.fn(),
      handleResumeMonitor: vi.fn(),
      t,
    }));

    act(() => {
      result.current.setQuery('pending');
    });
    await act(async () => {
      vi.advanceTimersByTime(500);
      await Promise.resolve();
    });

    expect(result.current.loading).toBe(true);

    act(() => {
      result.current.setQuery('');
    });
    await act(async () => {
      vi.advanceTimersByTime(500);
      await Promise.resolve();
    });

    expect(result.current.loading).toBe(false);
    expect(result.current.results).toEqual([]);
    unmount();
  });

  it('does not auto-retry dependency sync after a failure until retry is requested', async () => {
    const onDepsSync = vi.fn(async () => {
      throw new Error('sync failed');
    });

    const { result } = renderHook(() => useDepsSyncOverlay({
      depsNeedUpdate: true,
      pythonVersion: '3.12.10',
      renderVenvInstallStep: null,
      depsSyncing: false,
      onDepsSync,
    }));

    await waitFor(() => expect(result.current.depsSyncError).toBe('sync failed'));
    expect(onDepsSync).toHaveBeenCalledTimes(1);

    await act(async () => {
      await Promise.resolve();
    });
    expect(onDepsSync).toHaveBeenCalledTimes(1);

    act(() => {
      result.current.retryDepsSync();
    });

    await waitFor(() => expect(onDepsSync).toHaveBeenCalledTimes(2));
  });

  it('retries required model download only when retry is requested after a failure', async () => {
    invoke.mockImplementation(async (command) => {
      if (command === 'get_advanced_config') return { use_onnx: false };
      if (command === 'download_model') throw new Error('download failed');
      return null;
    });

    const { result } = renderHook(() => useRequiredModelDownload({
      modelsNeedDownload: true,
      missingModels: {
        'chinese-clip': { complete: false },
      },
      renderVenvInstallStep: null,
      depsNeedUpdate: false,
      onModelsDownloadComplete: vi.fn(),
      t,
    }));

    await waitFor(() => expect(result.current.modelDownloadError).toBe('download failed'));
    expect(invoke.mock.calls.filter(([command]) => command === 'download_model')).toHaveLength(1);

    await act(async () => {
      await Promise.resolve();
    });
    expect(invoke.mock.calls.filter(([command]) => command === 'download_model')).toHaveLength(1);

    act(() => {
      result.current.retryModelDownload();
    });

    await waitFor(() => {
      expect(invoke.mock.calls.filter(([command]) => command === 'download_model')).toHaveLength(2);
    });
  });

  it('unsubscribes a Tauri listener that resolves after unmount', async () => {
    let resolveListen;
    const unlisten = vi.fn();
    listen.mockImplementationOnce(() => new Promise((resolve) => {
      resolveListen = resolve;
    }));

    const { unmount } = renderHook(() => useTauriEventListener('late-event', vi.fn()));
    unmount();

    await act(async () => {
      resolveListen(unlisten);
      await Promise.resolve();
    });

    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
