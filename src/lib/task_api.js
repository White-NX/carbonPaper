/**
 * Task clustering API — Tauri command wrappers for long-term task management.
 */
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

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
  return withAuth(() => invoke('storage_get_tasks', {
    layer: layer || null,
    startTime: startTime ?? null,
    endTime: endTime ?? null,
    hideInactive,
    hideEntertainment,
    hideSocial,
  }));
}

/**
 * Get screenshots assigned to a specific task.
 * @param {number} taskId
 * @param {number} [page=0]
 * @param {number} [pageSize=50]
 * @returns {Promise<Array>} TaskScreenshotStub[]
 */
export async function getTaskScreenshots(taskId, page = 0, pageSize = 50) {
  return withAuth(() => invoke('storage_get_task_screenshots', {
    taskId,
    page,
    pageSize,
  }));
}

/**
 * Rename a task.
 * @param {number} taskId
 * @param {string} label
 */
export async function updateTaskLabel(taskId, label) {
  return withAuth(() => invoke('storage_update_task_label', { taskId, label }), { autoPrompt: true });
}

/**
 * Delete a task (screenshots are preserved).
 * @param {number} taskId
 */
export async function deleteTask(taskId) {
  return withAuth(() => invoke('storage_delete_task', { taskId }), { autoPrompt: true });
}

/**
 * Remove a screenshot from a task assignment. The screenshot itself is preserved.
 * @param {number} taskId
 * @param {number} screenshotId
 * @returns {Promise<number>} Remaining assigned screenshots for the task.
 */
export async function removeTaskScreenshot(taskId, screenshotId) {
  return withAuth(() => invoke('storage_remove_task_screenshot', {
    taskId,
    screenshotId,
  }), { autoPrompt: true });
}

/**
 * Merge multiple tasks into one.
 * @param {number[]} taskIds - First ID becomes the target.
 * @returns {Promise<number>} The surviving task ID.
 */
export async function mergeTasks(taskIds) {
  return withAuth(() => invoke('storage_merge_tasks', { taskIds }), { autoPrompt: true });
}

/**
 * Save clustering results to the database.
 * @param {Array} tasks - SaveTaskRequest[]
 * @returns {Promise<number[]>} New task IDs.
 */
export async function saveClusteringResults(tasks) {
  return withAuth(() => invoke('storage_save_clustering_results', { tasks }), { autoPrompt: true });
}

/**
 * Get screenshots related to the given screenshot (same task cluster).
 * @param {number} screenshotId
 * @param {number} [limit=8]
 * @returns {Promise<{task_id: number, task_label: string|null, screenshots: Array}>}
 */
export async function getRelatedScreenshots(screenshotId, limit = 8) {
  return withAuth(() => invoke('storage_get_related_screenshots', {
    screenshotId,
    limit,
  }));
}

// ── Python-backed clustering commands (via monitor IPC) ────────────────

/**
 * Trigger a clustering run.
 * @param {Object} [options]
 * @param {number} [options.startTime] - optional range start (seconds)
 * @param {number} [options.endTime] - optional range end (seconds)
 * @param {'auto'|'full'|'batched'} [options.clusteringMode] - resource strategy
 * @param {boolean} [options.manual] - true when invoked from a user action
 * @returns {Promise<Object>} Clustering result summary.
 */
export async function runClustering({ startTime, endTime, clusteringMode, manual = false } = {}) {
  const result = await withAuth(() => invoke('monitor_run_clustering', {
    startTime: startTime ?? null,
    endTime: endTime ?? null,
    clusteringMode: clusteringMode || 'auto',
    manual,
  }), { autoPrompt: manual });
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
  return withAuth(() => invoke('monitor_get_clustering_status'));
}

/**
 * Read progress for the active or most recently persisted MiniLM migration
 * run. The migration itself is sentinel-triggered at startup and cannot be
 * started or cancelled from the frontend; an interrupted run resumes on the
 * next launch/unlock.
 */
export async function getMinilmRebuildStatus() {
  return invoke('get_minilm_rebuild_status');
}

/** List in-memory + persisted diagnostics and failed/discarded ledger jobs. */
export async function listMinilmRebuildErrors(offset = 0, limit = 100) {
  return withAuth(() => invoke('list_minilm_rebuild_errors', { offset, limit }));
}

/**
 * The same, for the Chinese-CLIP image-vector migration (M2.5 step 7).
 *
 * A separate command rather than one parameterised by index kind, because the
 * two runs have separate state and separate sentinels and only their
 * orchestration is shared. The response shape is field-compatible with the
 * MiniLM one, which is what lets a single overlay render either.
 */
export async function getClipRebuildStatus() {
  return invoke('get_clip_rebuild_status');
}

/** List diagnostics for the CLIP migration run. */
export async function listClipRebuildErrors(offset = 0, limit = 100) {
  return withAuth(() => invoke('list_clip_rebuild_errors', { offset, limit }));
}

/**
 * What a CLIP backfill would cover and cost.
 *
 * Read-only and unauthenticated, so the dialog can poll it while waiting for
 * the step-7 copy to settle. The counts it returns are deliberately separate:
 * `skipped_deleted` is the ordinary consequence of having deleted screenshots
 * and needs no action, while `never_indexed` is what a backfill would encode
 * and what `estimated_seconds` is an estimate for. The full-history census is
 * deferred until system idle unless `allowExpensive` is an explicit refresh.
 */
export async function getClipBackfillOffer(allowExpensive = false) {
  return invoke('get_clip_backfill_offer', { allowExpensive });
}

/**
 * Record the answer. `approved` widens the automatic repair scan from recent
 * screenshots to the whole history; `declined` leaves it narrow. Returns the
 * refreshed offer.
 */
export async function setClipBackfillDecision(decision) {
  return withAuth(() => invoke('set_clip_backfill_decision', { decision }), {
    autoPrompt: true,
  });
}

/** Whether the app is in global maintenance mode (blocking overlay shown). */
export async function getMaintenanceStatus() {
  return invoke('get_maintenance_status');
}

/**
 * Set the automatic clustering interval.
 * @param {'1d'|'1w'|'1m'|'6m'} interval
 */
export async function setClusteringInterval(interval) {
  return withAuth(() => invoke('monitor_set_clustering_interval', { interval }), { autoPrompt: true });
}

/**
 * Get task clusters from the Python clustering manager (live data, not DB).
 * @returns {Promise<Object>} { hot_clusters, cold_clusters }
 */
export async function getTaskClusters() {
  return withAuth(() => invoke('monitor_get_task_clusters'));
}

/**
 * Natural-language retrieval against the hot-layer MiniLM index.
 * Returns snapshots most similar to the query, ordered by descending similarity.
 *
 * The ONNX variant argument is gone as of M2.5 step 6: only `model_uint8.onnx`
 * is ever installed and the Rust reranker pins it, so the old `q4f16` default
 * named a file that is never on disk. The backend still reports which variant
 * produced the scores, in `rerank_variant`.
 *
 * `backend` is retained in the response envelope for persisted calibration
 * provenance. Current production responses always report `rust`; historical
 * Python scorer values are accepted only when reading old Smart Cluster data.
 *
 * `cancelled` is set when the user stopped a reranked query through
 * `nlRerankStopNow`. It is a success rather than an error on purpose: nothing
 * failed, and rendering it as a failure would tell the user their own click
 * broke something. `results` is empty in that case — partial rankings are not
 * returned, because a cross-encoder ordering over a prefix of the candidates is
 * not the ordering over all of them and the threshold derived from it would be
 * wrong.
 *
 * @param {string} query
 * @param {number} [nResults=30] - clamped to 30 by the backend when reranking
 * @param {boolean} [enableRerank=false] - if true, over-fetches and re-scores with bge-reranker-v2-m3
 * @returns {Promise<{results: Array, reranked: boolean, rerank_variant: string|null, backend: 'rust', cancelled: boolean}>}
 */
export async function nlClusterQuery(query, nResults = 30, enableRerank = false) {
  const result = await withAuth(() => invoke('monitor_nl_cluster_query', {
    query,
    nResults,
    enableRerank,
  }));
  if (result && result.error) {
    const err = new Error(result.error);
    if (result.error.startsWith('RERANKER_UNAVAILABLE')) err.code = 'RERANKER_UNAVAILABLE';
    throw err;
  }
  return {
    results: result?.results || [],
    reranked: !!result?.reranked,
    rerank_variant: result?.rerank_variant || null,
    backend: result?.backend || 'rust',
    cancelled: !!result?.cancelled,
  };
}

/**
 * Ask the running reranked query to stop.
 *
 * Resolves to whether there was one to stop. The query checks between chunks of
 * eight documents, so this returns long before the query's own promise does;
 * that promise then settles with `cancelled: true`.
 *
 * Not wrapped in `withAuth`: stopping work touches no user data, and prompting
 * for Windows Hello in order to *cancel* something would be exactly backwards.
 *
 * @returns {Promise<boolean>}
 */
export async function nlRerankStopNow() {
  return invoke('nl_rerank_stop_now');
}

/**
 * Check whether the reranker model is on disk and loaded.
 * @returns {Promise<{available: boolean, loaded: boolean, loaded_variant: string|null, provider: string|null, available_variants: string[], model_path: string}>}
 */
export async function getRerankerStatus() {
  const result = await withAuth(() => invoke('monitor_nl_cluster_reranker_status'));
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
  return withAuth(() => invoke('smart_cluster_list'));
}

/**
 * Get a single smart cluster by id.
 */
export async function getSmartCluster(id) {
  return withAuth(() => invoke('smart_cluster_get', { id }));
}

/**
 * Get the calibration examples (positive + negative) for a smart cluster.
 */
export async function getSmartClusterExamples(id) {
  return withAuth(() => invoke('smart_cluster_get_examples', { id }));
}

/**
 * Create a new smart cluster from calibration.
 * @param {Object} req
 * @param {string} req.anchor_text - what the scorer matches snapshots against
 * @param {string} [req.display_name] - the label shown in the UI; defaults to
 *   the anchor text when absent
 * @param {number} req.threshold
 * @param {string} [req.dominant_color]
 * @param {Array} req.examples - [{ screenshot_id, is_positive, rerank_score }]
 * @param {string} [req.scorer_backend] - current calibration queries use
 *   `rust`; the backend also accepts historical `python` provenance so old
 *   thresholds can be re-derived without starting a Python scorer.
 * @returns {Promise<{id: number, enqueued: number}>}
 */
export async function createSmartCluster(req) {
  return withAuth(() => invoke('smart_cluster_create', { req }), { autoPrompt: true });
}

export async function deleteSmartCluster(id) {
  return withAuth(() => invoke('smart_cluster_delete', { id }), { autoPrompt: true });
}

/**
 * Rename a smart cluster.
 *
 * Only the label changes. The anchor text stays as the user wrote it at
 * creation, because the cluster's threshold was calibrated against that exact
 * wording — renaming must not quietly change what the cluster collects.
 */
export async function renameSmartCluster(id, name) {
  return withAuth(() => invoke('smart_cluster_rename', { id, name }), { autoPrompt: true });
}

export async function updateSmartClusterThreshold(id, threshold) {
  return withAuth(() => invoke('smart_cluster_update_threshold', { id, threshold }), { autoPrompt: true });
}

export async function toggleSmartClusterEnabled(id, enabled) {
  return withAuth(() => invoke('smart_cluster_toggle_enabled', { id, enabled }), { autoPrompt: true });
}

export async function getSmartClusterAssignments(clusterId, page = 0, pageSize = 50) {
  return withAuth(() => invoke('smart_cluster_assignments', { clusterId, page, pageSize }));
}

export async function getSmartClusterOcrCorpus(clusterId, page = 0, pageSize = 50) {
  return withAuth(() => invoke('smart_cluster_ocr_corpus', { clusterId, page, pageSize }));
}

export async function getSmartClusterSummary(clusterId) {
  return withAuth(() => invoke('smart_cluster_get_summary', { clusterId }));
}

export async function upsertSmartClusterSummary(summary) {
  return withAuth(() => invoke('smart_cluster_upsert_summary', { summary }), { autoPrompt: true });
}

export async function deleteSmartClusterSummary(clusterId) {
  return withAuth(() => invoke('smart_cluster_delete_summary', { clusterId }), { autoPrompt: true });
}

export async function rescanSmartCluster(clusterId) {
  return withAuth(() => invoke('smart_cluster_rescan', { clusterId }), { autoPrompt: true });
}

export async function clearSmartClusterAssignments(clusterId) {
  return withAuth(() => invoke('smart_cluster_clear_assignments', { clusterId }), { autoPrompt: true });
}

export async function getSmartClusterStatus() {
  return withAuth(() => invoke('smart_cluster_status'));
}

/** Trigger the Rust Smart Cluster scorer to drain the pending queue once. */
export async function smartClusterDrainNow() {
  return withAuth(() => invoke('monitor_smart_cluster_drain_now'), { autoPrompt: true });
}

/** Stop the currently running Rust Smart Cluster drain pass. */
export async function smartClusterStopDrain() {
  return withAuth(() => invoke('monitor_smart_cluster_stop_drain'), { autoPrompt: true });
}
