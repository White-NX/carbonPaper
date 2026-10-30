const HOVER_PREVIEW_RADIUS_PX = 16;

/** Timeline positions and records use milliseconds; storage records use seconds. */
export function timelineEventFromRecord(record) {
  if (!record) return null;
  const timestamp = record.timestamp != null ? record.timestamp * 1000 : Date.parse(record.created_at);
  if (!Number.isFinite(timestamp)) return null;
  return {
    id: record.id,
    timestamp,
    imagePath: record.image_path,
    appName: record.process_name,
    windowTitle: record.window_title,
    processIcon: record.process_icon || null,
    processPath: record.process_path || null,
    category: record.category || null,
  };
}

/** A provisional hover target from sorted display data. Exact selection uses storage. */
export function nearestTimelineEvent(events, time) {
  if (!events?.length || !Number.isFinite(time)) return null;
  let low = 0;
  let high = events.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (events[middle].timestamp < time) low = middle + 1;
    else high = middle;
  }
  const before = events[low - 1];
  const after = events[low];
  if (!before) return after;
  if (!after) return before;
  return time - before.timestamp <= after.timestamp - time ? before : after;
}

/** A fixed CSS-pixel radius keeps hover previews predictable at every zoom. */
export function isTimelinePreviewNearby(time, event, zoom) {
  return Number.isFinite(time) && Number.isFinite(event?.timestamp)
    && Number.isFinite(zoom) && zoom > 0
    && Math.abs(event.timestamp - time) * zoom <= HOVER_PREVIEW_RADIUS_PX;
}

/**
 * Chromium exposes pinch as Ctrl+wheel, and precise two-finger scrolling in
 * pixel units. Keep a precise gesture in pan mode through its momentum tail;
 * a fast swipe can grow to the same delta as a discrete mouse-wheel notch.
 */
export function timelineWheelAction(event, width, previous, now) {
  const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? width : 1;
  const dx = (event.deltaX || 0) * unit;
  const dy = (event.deltaY || 0) * unit;
  if (!dx && !dy) return null;
  if (event.ctrlKey || event.metaKey) {
    return { kind: 'zoom', factor: Math.exp(-Math.max(-160, Math.min(160, dy)) * 0.01), immediate: true };
  }
  const precise = event.deltaMode === 0 && (dx !== 0 || Math.abs(dy) < 50 || !Number.isInteger(dy));
  const continuingPan = previous?.kind === 'pan' && now - previous.at < 240;
  if (event.shiftKey || precise || continuingPan) {
    const delta = event.shiftKey ? dx || dy : Math.abs(dx) >= Math.abs(dy) ? dx : dy;
    return { kind: 'pan', deltaX: -delta };
  }
  const wheelPixels = event.deltaMode === 1 ? dy * 2.5 : dy;
  return { kind: 'zoom', factor: Math.exp(-Math.max(-600, Math.min(600, wheelPixels)) * Math.log(1.2) / 120), immediate: false };
}

/** Keep the original-image card beside the pointer and inside the viewport. */
export function timelinePreviewPosition({ clientX, clientY, trackTop, trackBottom }, viewportWidth, viewportHeight) {
  const margin = 12;
  const width = Math.min(440, Math.max(0, viewportWidth - margin * 2));
  const imageHeight = Math.min(248, Math.max(80, viewportHeight - 100));
  const height = imageHeight + 66;
  const right = clientX + 20;
  const left = right + width <= viewportWidth - margin ? right : clientX - width - 20;
  const below = Math.max(trackBottom + 10, clientY + 16);
  const top = below + height <= viewportHeight - margin ? below : trackTop - height - 10;
  return {
    left: Math.max(margin, Math.min(left, viewportWidth - width - margin)),
    top: Math.max(margin, Math.min(top, viewportHeight - height - margin)),
    width,
    imageHeight,
  };
}
