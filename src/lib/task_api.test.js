import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  getTasks,
  getTaskScreenshots,
  runClustering,
  setClusteringInterval,
} from './task_api';

describe('task_api', () => {
  it('calls getTasks with default payload', async () => {
    invoke.mockResolvedValue([]);

    await getTasks();

    expect(invoke).toHaveBeenCalledWith('storage_get_tasks', {
      layer: null,
      startTime: null,
      endTime: null,
      hideInactive: true,
      hideEntertainment: true,
      hideSocial: true,
    });
  });

  it('calls getTaskScreenshots with defaults', async () => {
    invoke.mockResolvedValue([]);

    await getTaskScreenshots(123);

    expect(invoke).toHaveBeenCalledWith('storage_get_task_screenshots', {
      taskId: 123,
      page: 0,
      pageSize: 50,
    });
  });

  it('throws when runClustering returns error', async () => {
    invoke.mockResolvedValue({ error: 'AUTH_REQUIRED' });

    await expect(runClustering()).rejects.toThrow('AUTH_REQUIRED');
  });

  it('sends clustering commands with provided params', async () => {
    invoke.mockResolvedValue({ status: 'success' });

    await runClustering({ startTime: 10, endTime: 20 });
    await setClusteringInterval('1w');

    expect(invoke).toHaveBeenNthCalledWith(1, 'execute_monitor_command', {
      payload: {
        command: 'run_clustering',
        start_time: 10,
        end_time: 20,
      },
    });

    expect(invoke).toHaveBeenNthCalledWith(2, 'execute_monitor_command', {
      payload: {
        command: 'set_clustering_interval',
        interval: '1w',
      },
    });
  });
});
