/**
 * Approximate download size of each model, in megabytes.
 *
 * Measured from the files `model_management.rs::model_download_spec` fetches,
 * rounded to something a person can plan around. Shown so a user knows what a
 * download will cost before agreeing to it; update when a spec's file list or
 * revision changes.
 */
export const MODEL_DOWNLOAD_MB = {
  'chinese-clip': 170,
  'bge-small-zh': 23,
  'minilm-l12': 130,
  'bge-reranker-v2-m3': 560,
};

export const REQUIRED_MODEL_IDS = ['chinese-clip', 'bge-small-zh', 'minilm-l12'];

/** Total size of the given model ids, rounded to the nearest 10 MB. */
export function downloadSizeMb(modelIds) {
  const total = modelIds.reduce((sum, id) => sum + (MODEL_DOWNLOAD_MB[id] ?? 0), 0);
  return Math.max(10, Math.round(total / 10) * 10);
}

/** Required models that `check_model_files` reports as incomplete. */
export function missingRequiredModels(modelStatus) {
  if (!modelStatus) return [];
  return REQUIRED_MODEL_IDS.filter((id) => modelStatus[id] && !modelStatus[id].complete);
}
