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
