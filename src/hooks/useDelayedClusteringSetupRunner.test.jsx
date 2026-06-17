import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { runClustering, saveClusteringResults } from '../lib/task_api';
import { useDelayedClusteringSetupRunner } from './useDelayedClusteringSetupRunner';

vi.mock('../lib/task_api', () => ({
  runClustering: vi.fn(),
  saveClusteringResults: vi.fn(),
}));

describe('useDelayedClusteringSetupRunner', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    Object.defineProperty(window, 'confirm', {
      configurable: true,
      writable: true,
      value: vi.fn(() => true),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('prompts for resource mode and reports degraded saved results', async () => {
    const pushNotification = vi.fn();
    const onClose = vi.fn();
    runClustering
      .mockResolvedValueOnce({
        status: 'needs_user_choice',
        reason: 'large_range',
        n_total: 4,
        estimate: {
          count: 4,
          memory: { estimated_peak_bytes: 2 * 1024 ** 3 },
        },
      })
      .mockResolvedValueOnce({
        status: 'success',
        degraded: true,
        sample_size: 2,
        assigned_count: 1,
        clusters: [{
          centroid: [0, 1],
          screenshot_ids: ['11', '12'],
          start_time: 1,
          end_time: 2,
          dominant_process: 'Code.exe',
          dominant_category: 'Development',
        }],
      });
    saveClusteringResults.mockResolvedValue([1]);

    const { result } = renderHook(() => useDelayedClusteringSetupRunner({
      delayMs: 10,
      onClose,
      pushNotification,
    }));

    act(() => {
      result.current(true);
    });
    await act(async () => {
      await vi.runAllTimersAsync();
    });

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(saveClusteringResults).toHaveBeenCalledTimes(1);
    expect(runClustering).toHaveBeenNthCalledWith(1, { manual: true });
    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining('降级分批模式'));
    expect(runClustering).toHaveBeenNthCalledWith(2, {
      manual: true,
      clusteringMode: 'batched',
    });
    expect(saveClusteringResults).toHaveBeenCalledWith([expect.objectContaining({
      layer: 'hot',
      dominant_process: 'Code.exe',
      screenshot_ids: ['11', '12'],
    })]);
    expect(pushNotification).toHaveBeenCalledWith(expect.objectContaining({
      type: 'success',
      title: '任务聚类完成',
      message: expect.stringContaining('降级分批模式'),
    }));
  });
});
