import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('./auth_api', () => ({
  withAuth: async (fn) => fn(),
  requestAuth: vi.fn(),
  checkAuthSession: vi.fn(),
  initAuthListeners: vi.fn(),
  lockSession: vi.fn(),
}));

import {
  classifyDebug,
  deleteRecordsByTimeRange,
  deleteScreenshot,
  getIndexHealth,
  getSmartClusterWorkerStatus,
  removeLocalAnchorsByProcess,
  retryVectorIndexing,
} from './monitor_api';
import {
  createSmartCluster,
  getRelatedScreenshots,
  getSmartClusterAssignments,
  mergeTasks,
  nlClusterQuery,
  saveClusteringResults,
  smartClusterCalibratePreview,
  toggleSmartClusterEnabled,
  updateSmartClusterAnchor,
  updateSmartClusterThreshold,
} from './task_api';

describe('API contract payloads', () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it('sends monitor classification and maintenance payloads', async () => {
    invoke
      .mockResolvedValueOnce({ category: 'Development' })
      .mockResolvedValueOnce({ status: 'success', removed_count: 2 })
      .mockResolvedValueOnce({ status: 'success', deleted: true })
      .mockResolvedValueOnce({ status: 'success', deleted_count: 3 })
      .mockResolvedValueOnce({ pending_count: 4, is_running: true, is_force_running: false })
      .mockResolvedValueOnce({ status: 'success', screenshots_count: 10 })
      .mockResolvedValueOnce({ status: 'success', enqueued: 2 });

    await classifyDebug({ title: 'Editor', ocrText: 'text', processName: 'code.exe' });
    await removeLocalAnchorsByProcess('Development', 'code.exe');
    await deleteScreenshot(42);
    await deleteRecordsByTimeRange(5, 1_000_000);
    await expect(getSmartClusterWorkerStatus()).resolves.toEqual({
      pending_count: 4,
      running: true,
      forceRunning: false,
    });
    await getIndexHealth({ refreshVector: true });
    await retryVectorIndexing(12);

    expect(invoke).toHaveBeenNthCalledWith(1, 'monitor_classify_debug', {
      title: 'Editor',
      ocrText: 'text',
      processName: 'code.exe',
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'monitor_remove_local_anchors_by_process', {
      category: 'Development',
      processName: 'code.exe',
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'storage_delete_screenshot', {
      screenshotId: 42,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'storage_delete_by_time_range', {
      startTime: 700000,
      endTime: 1000000,
    });
    expect(invoke).toHaveBeenNthCalledWith(5, 'monitor_smart_cluster_worker_status');
    expect(invoke).toHaveBeenNthCalledWith(6, 'storage_get_index_health', {
      refreshVector: true,
    });
    expect(invoke).toHaveBeenNthCalledWith(7, 'storage_retry_vector_indexing', {
      limit: 12,
    });
  });

  it('sends task and natural-language clustering payloads', async () => {
    invoke
      .mockResolvedValueOnce({ results: [{ id: 1 }], reranked: true, rerank_variant: 'q4f16' })
      .mockResolvedValueOnce({ results: [{ id: 2 }] })
      .mockResolvedValueOnce({ task_id: 7, screenshots: [] })
      .mockResolvedValueOnce(99)
      .mockResolvedValueOnce([101, 102]);

    await expect(nlClusterQuery('invoice', 12, true, 'q4f16')).resolves.toEqual({
      results: [{ id: 1 }],
      reranked: true,
      rerank_variant: 'q4f16',
    });
    await expect(smartClusterCalibratePreview('invoice', 8)).resolves.toEqual([{ id: 2 }]);
    await getRelatedScreenshots(42, 6);
    await mergeTasks([1, 2]);
    await saveClusteringResults([{ label: 'Work', screenshot_ids: [42] }]);

    expect(invoke).toHaveBeenNthCalledWith(1, 'monitor_nl_cluster_query', {
      query: 'invoice',
      nResults: 12,
      enableRerank: true,
      rerankVariant: 'q4f16',
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'monitor_smart_cluster_calibrate_preview', {
      query: 'invoice',
      nResults: 8,
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'storage_get_related_screenshots', {
      screenshotId: 42,
      limit: 6,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'storage_merge_tasks', {
      taskIds: [1, 2],
    });
    expect(invoke).toHaveBeenNthCalledWith(5, 'storage_save_clustering_results', {
      tasks: [{ label: 'Work', screenshot_ids: [42] }],
    });
  });

  it('sends smart cluster CRUD payloads', async () => {
    invoke.mockResolvedValue({});

    const createRequest = {
      anchor_text: 'Invoices',
      threshold: 0.72,
      examples: [{ screenshot_id: 42, is_positive: true, rerank_score: 0.91 }],
    };

    await createSmartCluster(createRequest);
    await updateSmartClusterAnchor(7, 'Receipts');
    await updateSmartClusterThreshold(7, 0.8);
    await toggleSmartClusterEnabled(7, false);
    await getSmartClusterAssignments(7, 2, 20);

    expect(invoke).toHaveBeenNthCalledWith(1, 'smart_cluster_create', {
      req: createRequest,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'smart_cluster_update_anchor', {
      id: 7,
      anchor: 'Receipts',
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'smart_cluster_update_threshold', {
      id: 7,
      threshold: 0.8,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'smart_cluster_toggle_enabled', {
      id: 7,
      enabled: false,
    });
    expect(invoke).toHaveBeenNthCalledWith(5, 'smart_cluster_assignments', {
      clusterId: 7,
      page: 2,
      pageSize: 20,
    });
  });
});
