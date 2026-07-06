import { beforeEach, describe, expect, it, vi } from 'vitest';
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
  getTasks,
  getTaskScreenshots,
  getSmartClusterOcrCorpus,
  getSmartClusterSummary,
  upsertSmartClusterSummary,
  deleteSmartClusterSummary,
  removeTaskScreenshot,
  runClustering,
  setClusteringInterval,
} from './task_api';

describe('task_api', () => {
  beforeEach(() => {
    invoke.mockReset();
    withAuth.mockClear();
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
    expectWithAuth(1);
  });

  it('calls getTaskScreenshots with defaults', async () => {
    invoke.mockResolvedValue([]);

    await getTaskScreenshots(123);

    expect(invoke).toHaveBeenCalledWith('storage_get_task_screenshots', {
      taskId: 123,
      page: 0,
      pageSize: 50,
    });
    expectWithAuth(1);
  });

  it('calls removeTaskScreenshot with expected payload', async () => {
    invoke.mockResolvedValue(11);

    await removeTaskScreenshot(123, 456);

    expect(invoke).toHaveBeenCalledWith('storage_remove_task_screenshot', {
      taskId: 123,
      screenshotId: 456,
    });
    expectWithAuth(1, { autoPrompt: true });
  });

  it('throws when runClustering returns error', async () => {
    invoke.mockResolvedValue({ error: 'AUTH_REQUIRED' });

    await expect(runClustering()).rejects.toThrow('AUTH_REQUIRED');
    expectWithAuth(1, { autoPrompt: false });
  });

  it('sends clustering commands with provided params', async () => {
    invoke.mockResolvedValue({ status: 'success' });

    await runClustering({ startTime: 10, endTime: 20 });
    await setClusteringInterval('1w');

    expect(invoke).toHaveBeenNthCalledWith(1, 'monitor_run_clustering', {
      startTime: 10,
      endTime: 20,
      clusteringMode: 'auto',
      manual: false,
    });

    expect(invoke).toHaveBeenNthCalledWith(2, 'monitor_set_clustering_interval', { interval: '1w' });
    expectWithAuth(1, { autoPrompt: false });
    expectWithAuth(2, { autoPrompt: true });
  });

  it('calls smart cluster summary commands with expected payloads', async () => {
    invoke.mockResolvedValue({});

    const summary = {
      smart_cluster_id: 7,
      title: 'MCP work',
      summary: 'Summarized smart cluster',
      ocr_summary: 'OCR mentions MCP tools',
    };

    await getSmartClusterOcrCorpus(7, 1, 25);
    await getSmartClusterSummary(7);
    await upsertSmartClusterSummary(summary);
    await deleteSmartClusterSummary(7);

    expect(invoke).toHaveBeenNthCalledWith(1, 'smart_cluster_ocr_corpus', {
      clusterId: 7,
      page: 1,
      pageSize: 25,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'smart_cluster_get_summary', { clusterId: 7 });
    expect(invoke).toHaveBeenNthCalledWith(3, 'smart_cluster_upsert_summary', { summary });
    expect(invoke).toHaveBeenNthCalledWith(4, 'smart_cluster_delete_summary', { clusterId: 7 });
    expectWithAuth(1);
    expectWithAuth(2);
    expectWithAuth(3, { autoPrompt: true });
    expectWithAuth(4, { autoPrompt: true });
  });
});
