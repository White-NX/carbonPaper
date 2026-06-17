export const SNAPSHOT_PREVIEW_TAB_LIMIT = 8;
export const SNAPSHOT_PREVIEW_WINDOW_LABEL = 'snapshot-preview';
export const SNAPSHOT_PREVIEW_WINDOW_STATE_KEY = 'carbonpaper:snapshot-preview-window-state';

const pickFirst = (...values) => values.find((value) => value !== undefined && value !== null) ?? null;

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

export function sanitizeSnapshotPreviewItem(item) {
  if (!item) return null;
  const screenshotId = pickFirst(item.screenshot_id, item.id, item.metadata?.screenshot_id);
  const imagePath = pickFirst(item.image_path, item.path, item.metadata?.image_path);
  const createdAt = pickFirst(
    item.created_at,
    item.screenshot_created_at,
    item.metadata?.created_at,
    item.metadata?.screenshot_created_at,
  );

  const metadata = {};
  const metadataFields = [
    ['screenshot_id', screenshotId],
    ['image_path', imagePath],
    ['created_at', createdAt],
    ['process_name', pickFirst(item.process_name, item.appName, item.metadata?.process_name)],
    ['window_title', pickFirst(item.window_title, item.windowTitle, item.metadata?.window_title)],
    ['category', pickFirst(item.category, item.metadata?.category)],
  ];
  metadataFields.forEach(([key, value]) => {
    if (value !== null && value !== undefined && value !== '') metadata[key] = value;
  });

  return {
    screenshot_id: screenshotId,
    id: screenshotId || item.id || null,
    image_path: imagePath,
    path: imagePath,
    created_at: createdAt,
    process_name: pickFirst(item.process_name, item.appName, item.metadata?.process_name),
    window_title: pickFirst(item.window_title, item.windowTitle, item.metadata?.window_title),
    category: pickFirst(item.category, item.metadata?.category),
    sourceLabel: pickFirst(item.sourceLabel, item.source?.label),
    sourceDetail: pickFirst(item.sourceDetail, item.source?.detail),
    sourceType: pickFirst(item.sourceType, item.source?.type),
    similarity: item.similarity ?? null,
    rerank_score: item.rerank_score ?? null,
    assigned_at: item.assigned_at ?? null,
    metadata,
  };
}

export function sanitizeSnapshotPreviewState(state = {}) {
  const tabs = Array.isArray(state.tabs)
    ? state.tabs.map(sanitizeSnapshotPreviewItem).filter((tab) => getSnapshotPreviewKey(tab))
    : [];
  const activeKey = state.activeKey && tabs.some((tab) => getSnapshotPreviewKey(tab) === state.activeKey)
    ? state.activeKey
    : (tabs[0] ? getSnapshotPreviewKey(tabs[0]) : null);

  return {
    tabs,
    activeKey,
    updatedAt: state.updatedAt || Date.now(),
  };
}
