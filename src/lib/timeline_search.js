import { captureDateOf } from './search_grouping';

export const SEARCH_FIT_MIN_SPAN_MS = 10 * 60 * 1000;
const SEARCH_FIT_PADDING_RATIO = 0.12;

/** Stable identity shared by search rows and timeline markers. */
export function searchResultMarkerId(item, fallback = '') {
  const screenshotId = item?.screenshot_id ?? item?.metadata?.screenshot_id;
  if (screenshotId !== null && screenshotId !== undefined && screenshotId !== '') {
    return `screenshot:${screenshotId}`;
  }

  const path = item?.image_path || item?.metadata?.image_path || item?.path;
  return path ? `path:${path}` : `result:${fallback}`;
}

/** Build a compact, chronological marker set from the currently loaded hits. */
export function buildSearchTimelineMarkers(results) {
  const byId = new Map();

  (results || []).forEach((item, index) => {
    const capturedAt = captureDateOf(item);
    if (!capturedAt || Number.isNaN(capturedAt.getTime())) return;

    const id = searchResultMarkerId(item, index);
    if (byId.has(id)) return;
    byId.set(id, {
      id,
      screenshotId: item?.screenshot_id ?? item?.metadata?.screenshot_id ?? null,
      time: capturedAt.getTime(),
      result: item,
    });
  });

  return [...byId.values()].sort((a, b) => a.time - b.time);
}

/** Return a padded range that keeps close or single hits readable. */
export function getSearchMarkerFitRange(markers) {
  if (!markers?.length) return null;

  const first = markers[0].time;
  const last = markers[markers.length - 1].time;
  if (!Number.isFinite(first) || !Number.isFinite(last)) return null;

  const rawSpan = Math.max(0, last - first);
  const span = Math.max(
    SEARCH_FIT_MIN_SPAN_MS,
    rawSpan * (1 + SEARCH_FIT_PADDING_RATIO * 2),
  );
  const center = (first + last) / 2;
  return { from: center - span / 2, to: center + span / 2 };
}

/**
 * Collapse nearby on-screen hits into a bounded number of visual markers.
 * Clusters are intentionally based on a fixed starting x so a dense chain does
 * not merge into one marker spanning the whole timeline.
 */
export function clusterSearchTimelineMarkers(
  markers,
  timeToX,
  width,
  hoveredIds = [],
  clusterWidthPx = 7,
) {
  if (!markers?.length || !width) return [];

  const hovered = hoveredIds instanceof Set ? hoveredIds : new Set(hoveredIds || []);
  const visible = markers
    .map((marker) => ({ marker, x: timeToX(marker.time) }))
    .filter(({ x }) => Number.isFinite(x) && x >= -clusterWidthPx && x <= width + clusterWidthPx)
    .sort((a, b) => a.x - b.x);

  const clusters = [];
  for (const entry of visible) {
    const current = clusters[clusters.length - 1];
    if (!current || entry.x - current.startX > clusterWidthPx) {
      clusters.push({
        key: entry.marker.id,
        startX: entry.x,
        xTotal: entry.x,
        x: entry.x,
        count: 1,
        active: hovered.has(entry.marker.id),
        from: entry.marker.time,
        to: entry.marker.time,
        markerIds: [entry.marker.id],
        representative: entry.marker,
      });
      continue;
    }

    current.xTotal += entry.x;
    current.count += 1;
    current.x = current.xTotal / current.count;
    current.active = current.active || hovered.has(entry.marker.id);
    current.to = entry.marker.time;
    current.markerIds.push(entry.marker.id);
    if (hovered.has(entry.marker.id)) current.representative = entry.marker;
  }

  return clusters;
}
