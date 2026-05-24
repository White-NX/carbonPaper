/**
 * Task clustering API — Tauri command wrappers for long-term task management.
 */
import { invoke } from '@tauri-apps/api/core';

// ── DB-backed task queries (Rust) ──────────────────────────────────────

/**
 * Get tasks from the database.
 * @param {Object} [options]
 * @param {string} [options.layer] - 'hot' | 'cold' | undefined (all)
 * @param {number} [options.startTime] - start timestamp (seconds)
 * @param {number} [options.endTime] - end timestamp (seconds)
 * @param {boolean} [options.hideInactive] - hide tasks inactive >30 days (default true)
 * @param {boolean} [options.hideEntertainment] - hide entertainment-dominated tasks (default true)
 * @param {boolean} [options.hideSocial] - hide social-dominated tasks (default true)
 * @returns {Promise<Array>} TaskRecord[]
 */
export async function getTasks({ layer, startTime, endTime, hideInactive = true, hideEntertainment = true, hideSocial = true } = {}) {
  return invoke('storage_get_tasks', {
    layer: layer || null,
    startTime: startTime ?? null,
    endTime: endTime ?? null,
    hideInactive,
    hideEntertainment,
    hideSocial,
  });
}

/**
 * Get screenshots assigned to a specific task.
 * @param {number} taskId
 * @param {number} [page=0]
 * @param {number} [pageSize=50]
 * @returns {Promise<Array>} TaskScreenshotStub[]
 */
export async function getTaskScreenshots(taskId, page = 0, pageSize = 50) {
  return invoke('storage_get_task_screenshots', {
    taskId,
    page,
    pageSize,
  });
}

/**
 * Rename a task.
 * @param {number} taskId
 * @param {string} label
 */
export async function updateTaskLabel(taskId, label) {
  return invoke('storage_update_task_label', { taskId, label });
}

/**
 * Delete a task (screenshots are preserved).
 * @param {number} taskId
 */
export async function deleteTask(taskId) {
  return invoke('storage_delete_task', { taskId });
}

/**
 * Merge multiple tasks into one.
 * @param {number[]} taskIds - First ID becomes the target.
 * @returns {Promise<number>} The surviving task ID.
 */
export async function mergeTasks(taskIds) {
  return invoke('storage_merge_tasks', { taskIds });
}

/**
 * Save clustering results to the database.
 * @param {Array} tasks - SaveTaskRequest[]
 * @returns {Promise<number[]>} New task IDs.
 */
export async function saveClusteringResults(tasks) {
  return invoke('storage_save_clustering_results', { tasks });
}

/**
 * Get screenshots related to the given screenshot (same task cluster).
 * @param {number} screenshotId
 * @param {number} [limit=8]
 * @returns {Promise<{task_id: number, task_label: string|null, screenshots: Array}>}
 */
export async function getRelatedScreenshots(screenshotId, limit = 8) {
  return invoke('storage_get_related_screenshots', {
    screenshotId,
    limit,
  });
}

// ── Python-backed clustering commands (via monitor IPC) ────────────────

/**
 * Trigger a clustering run.
 * @param {Object} [options]
 * @param {number} [options.startTime] - optional range start (seconds)
 * @param {number} [options.endTime] - optional range end (seconds)
 * @returns {Promise<Object>} Clustering result summary.
 */
export async function runClustering({ startTime, endTime } = {}) {
  const result = await invoke('execute_monitor_command', {
    payload: {
      command: 'run_clustering',
      start_time: startTime ?? null,
      end_time: endTime ?? null,
    },
  });
  if (result && result.error) {
    throw new Error(result.error);
  }
  return result;
}

/**
 * Get the current clustering scheduler status.
 * @returns {Promise<Object>} { config, last_result }
 */
export async function getClusteringStatus() {
  return invoke('execute_monitor_command', {
    payload: { command: 'get_clustering_status' },
  });
}

/**
 * Set the automatic clustering interval.
 * @param {'1d'|'1w'|'1m'|'6m'} interval
 */
export async function setClusteringInterval(interval) {
  return invoke('execute_monitor_command', {
    payload: { command: 'set_clustering_interval', interval },
  });
}

/**
 * Get task clusters from the Python clustering manager (live data, not DB).
 * @returns {Promise<Object>} { hot_clusters, cold_clusters }
 */
export async function getTaskClusters() {
  return invoke('execute_monitor_command', {
    payload: { command: 'get_tasks' },
  });
}

/**
 * Natural-language retrieval against the hot-layer MiniLM index (demo).
 * Returns snapshots most similar to the query, ordered by descending similarity.
 * @param {string} query
 * @param {number} [nResults=30]
 * @param {boolean} [enableRerank=false] - if true, over-fetches and re-scores with bge-reranker-v2-m3
 * @param {string} [rerankVariant='fp16'] - ONNX variant: 'fp16' | 'q4f16' | 'int8' | 'fp32'
 * @returns {Promise<{results: Array, reranked: boolean, rerank_variant: string|null}>}
 */
export async function nlClusterQuery(query, nResults = 30, enableRerank = false, rerankVariant = 'q4f16') {
  const result = await invoke('execute_monitor_command', {
    payload: {
      command: 'nl_cluster_query',
      query,
      n_results: nResults,
      enable_rerank: enableRerank,
      rerank_variant: rerankVariant,
    },
  });
  if (result && result.error) {
    const err = new Error(result.error);
    if (result.error.startsWith('RERANKER_UNAVAILABLE')) err.code = 'RERANKER_UNAVAILABLE';
    throw err;
  }
  return {
    results: result?.results || [],
    reranked: !!result?.reranked,
    rerank_variant: result?.rerank_variant || null,
  };
}

/**
 * Check whether the reranker model is on disk and loaded.
 * @returns {Promise<{available: boolean, loaded: boolean, loaded_variant: string|null, provider: string|null, available_variants: string[], model_path: string}>}
 */
export async function getRerankerStatus() {
  const result = await invoke('execute_monitor_command', {
    payload: { command: 'nl_cluster_reranker_status' },
  });
  if (result && result.error) throw new Error(result.error);
  return {
    available: !!result?.available,
    loaded: !!result?.loaded,
    loaded_variant: result?.loaded_variant || null,
    provider: result?.provider || null,
    available_variants: result?.available_variants || [],
    model_path: result?.model_path || '',
  };
}

// ── Smart Cluster API ──────────────────────────────────────────────────

/**
 * List all smart clusters with their assignment counts.
 */
export async function listSmartClusters() {
  return invoke('smart_cluster_list');
}

/**
 * Get a single smart cluster by id.
 */
export async function getSmartCluster(id) {
  return invoke('smart_cluster_get', { id });
}

/**
 * Get the calibration examples (positive + negative) for a smart cluster.
 */
export async function getSmartClusterExamples(id) {
  return invoke('smart_cluster_get_examples', { id });
}

/**
 * Create a new smart cluster from calibration.
 * @param {Object} req
 * @param {string} req.anchor_text
 * @param {number} req.threshold
 * @param {string} [req.dominant_color]
 * @param {Array} req.examples - [{ screenshot_id, is_positive, rerank_score }]
 * @returns {Promise<{id: number, enqueued: number}>}
 */
export async function createSmartCluster(req) {
  return invoke('smart_cluster_create', { req });
}

export async function deleteSmartCluster(id) {
  return invoke('smart_cluster_delete', { id });
}

export async function updateSmartClusterAnchor(id, anchor) {
  return invoke('smart_cluster_update_anchor', { id, anchor });
}

export async function updateSmartClusterThreshold(id, threshold) {
  return invoke('smart_cluster_update_threshold', { id, threshold });
}

export async function toggleSmartClusterEnabled(id, enabled) {
  return invoke('smart_cluster_toggle_enabled', { id, enabled });
}

export async function getSmartClusterAssignments(clusterId, page = 0, pageSize = 50) {
  return invoke('smart_cluster_assignments', { clusterId, page, pageSize });
}

export async function rescanSmartCluster(clusterId) {
  return invoke('smart_cluster_rescan', { clusterId });
}

export async function clearSmartClusterAssignments(clusterId) {
  return invoke('smart_cluster_clear_assignments', { clusterId });
}

export async function getSmartClusterStatus() {
  return invoke('smart_cluster_status');
}

/**
 * Trigger the Python worker to drain the pending queue immediately,
 * bypassing the idle gate for one pass.
 */
export async function smartClusterDrainNow() {
  return invoke('execute_monitor_command', {
    payload: { command: 'smart_cluster_drain_now' },
  });
}

/**
 * Run a calibration preview query — same as nlClusterQuery with rerank=true
 * but routed through a dedicated command so future tuning (over-fetch, etc)
 * doesn't affect the explore demo.
 */
export async function smartClusterCalibratePreview(query, nResults = 30) {
  const result = await invoke('execute_monitor_command', {
    payload: {
      command: 'smart_cluster_calibrate_preview',
      query,
      n_results: nResults,
    },
  });
  if (result && result.error) {
    const err = new Error(result.error);
    if (result.error.startsWith('RERANKER_UNAVAILABLE')) err.code = 'RERANKER_UNAVAILABLE';
    throw err;
  }
  return result?.results || [];
}
