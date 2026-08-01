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
  toggleSmartClusterEnabled,
  updateSmartClusterAnchor,
  updateSmartClusterThreshold,
} from './task_api';

describe('API contract payloads', () => {
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

  it('sends monitor classification and maintenance payloads', async () => {
    invoke
      .mockResolvedValueOnce({ category: 'Development' })
      .mockResolvedValueOnce({ status: 'success', removed_count: 2 })
      .mockResolvedValueOnce({ status: 'success', deleted: true })
      .mockResolvedValueOnce({ status: 'success', deleted_count: 3 })
      .mockResolvedValueOnce({
        pending_count: 4,
        is_running: true,
        is_force_running: false,
        unverifiable_thresholds: 2,
      })
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
      // M2.5 step 6: clusters skipped because their stored threshold came from
      // the retired scorer and could not be re-derived.
      unverifiableThresholds: 2,
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

    expect(withAuth).toHaveBeenCalledTimes(7);
    expectWithAuth(1, { autoPrompt: true });
    expectWithAuth(2, { autoPrompt: true });
    expectWithAuth(3, { autoPrompt: true });
    expectWithAuth(4, { autoPrompt: true });
    expectWithAuth(5);
    expectWithAuth(6, { autoPrompt: true });
    expectWithAuth(7, { autoPrompt: true });
  });

  it('sends task and natural-language clustering payloads', async () => {
    invoke
      .mockResolvedValueOnce({
        results: [{ id: 1 }],
        reranked: true,
        rerank_variant: 'uint8',
        backend: 'python',
      })
      .mockResolvedValueOnce({ task_id: 7, screenshots: [] })
      .mockResolvedValueOnce(99)
      .mockResolvedValueOnce([101, 102]);

    // `backend` survives the wrapper: a reranked query is a Smart Cluster
    // calibration query, and the threshold derived from these scores is stored
    // with the scorer that produced them. Here the Rust path stood down and
    // Python answered even though Rust reranking is the configured default,
    // which is the case the field exists for.
    await expect(nlClusterQuery('invoice', 12, true)).resolves.toEqual({
      results: [{ id: 1 }],
      reranked: true,
      rerank_variant: 'uint8',
      backend: 'python',
    });
    await getRelatedScreenshots(42, 6);
    await mergeTasks([1, 2]);
    await saveClusteringResults([{ label: 'Work', screenshot_ids: [42] }]);

    // No `rerankVariant` key: M2.5 step 6 pinned the variant in Rust, and the
    // old `q4f16` default named a file that is never installed.
    expect(invoke).toHaveBeenNthCalledWith(1, 'monitor_nl_cluster_query', {
      query: 'invoice',
      nResults: 12,
      enableRerank: true,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'storage_get_related_screenshots', {
      screenshotId: 42,
      limit: 6,
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'storage_merge_tasks', {
      taskIds: [1, 2],
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'storage_save_clustering_results', {
      tasks: [{ label: 'Work', screenshot_ids: [42] }],
    });

    expect(withAuth).toHaveBeenCalledTimes(4);
    expectWithAuth(1);
    expectWithAuth(2);
    expectWithAuth(3, { autoPrompt: true });
    expectWithAuth(4, { autoPrompt: true });
  });

  it('sends smart cluster CRUD payloads', async () => {
    invoke.mockResolvedValue({});

    const createRequest = {
      anchor_text: 'Invoices',
      threshold: 0.72,
      examples: [{ screenshot_id: 42, is_positive: true, rerank_score: 0.91 }],
      // The backend the calibration query reported. Forwarded verbatim: the
      // Rust command stamps the threshold with the scorer that actually
      // produced these scores rather than with the configured one.
      scorer_backend: 'rust',
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

    expect(withAuth).toHaveBeenCalledTimes(5);
    expectWithAuth(1, { autoPrompt: true });
    expectWithAuth(2, { autoPrompt: true });
    expectWithAuth(3, { autoPrompt: true });
    expectWithAuth(4, { autoPrompt: true });
    expectWithAuth(5);
  });
});
