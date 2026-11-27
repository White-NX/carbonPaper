const TIMELINE_IMAGE_CACHE_LIMIT = 800;
export const timelineImageCache = new Map();
const invalidationListeners = new Set();

export function setTimelineImageCache(key, dataUrl) {
  if (key === null || key === undefined || !dataUrl) return;
  if (!timelineImageCache.has(key) && timelineImageCache.size >= TIMELINE_IMAGE_CACHE_LIMIT) {
    const oldestKey = timelineImageCache.keys().next().value;
    timelineImageCache.delete(oldestKey);
  }
  timelineImageCache.set(key, dataUrl);
}

export function invalidateTimelineImage(screenshotId) {
  timelineImageCache.delete(screenshotId);
  // Fence in-flight batches synchronously before a saved-image refresh renders.
  for (const listener of invalidationListeners) listener();
}

export function subscribeTimelineImageInvalidation(listener) {
  invalidationListeners.add(listener);
  return () => invalidationListeners.delete(listener);
}
