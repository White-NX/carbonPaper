export const SNAPSHOT_PREVIEW_TAB_LIMIT = 8;
export const SNAPSHOT_PREVIEW_WINDOW_LABEL = 'snapshot-preview';
export const SNAPSHOT_PREVIEW_WINDOW_STATE_KEY = 'carbonpaper:snapshot-preview-window-state';

export function getSnapshotPreviewKey(item) {
  const id = item?.screenshot_id ?? item?.id ?? item?.metadata?.screenshot_id;
  if (typeof id === 'number' && id > 0) return `id:${id}`;
  const path = item?.image_path || item?.path || item?.metadata?.image_path;
  return path ? `path:${path}` : null;
}

export function normalizeSnapshotPreviewItem(item, options = {}) {
  const {
    thumbnailSrc = null,
    sourceLabel = null,
    sourceDetail = null,
    sourceType = null,
  } = options;

  const screenshotId = item?.screenshot_id ?? item?.id ?? item?.metadata?.screenshot_id;
  const imagePath = item?.image_path || item?.path || item?.metadata?.image_path;
  const createdAt = item?.created_at
    || item?.screenshot_created_at
    || item?.metadata?.created_at
    || item?.metadata?.screenshot_created_at
    || null;

  return {
    ...item,
    screenshot_id: screenshotId,
    id: screenshotId || item?.id,
    image_path: imagePath,
    path: imagePath,
    created_at: createdAt,
    process_name: item?.process_name || item?.appName || item?.metadata?.process_name || null,
    window_title: item?.window_title || item?.windowTitle || item?.metadata?.window_title || null,
    category: item?.category || item?.metadata?.category || null,
    thumbnailSrc: thumbnailSrc || item?.thumbnailSrc || null,
    sourceLabel: sourceLabel || item?.sourceLabel || item?.source?.label || null,
    sourceDetail: sourceDetail || item?.sourceDetail || item?.source?.detail || null,
    sourceType: sourceType || item?.sourceType || item?.source?.type || null,
  };
}
