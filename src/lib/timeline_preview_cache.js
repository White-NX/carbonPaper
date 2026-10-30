export const TIMELINE_PREVIEW_THUMBNAIL_ITEMS = 2048;
export const TIMELINE_PREVIEW_THUMBNAIL_BYTES = 32 * 1024 * 1024;
const OVERSCAN_RATIO = 0.25;

/** Cache compressed data URLs only; the preview decodes the displayed image. */
export function createTimelinePreviewCache({ maxItems, maxBytes }) {
  const entries = new Map();
  let bytes = 0;
  const remove = (id) => {
    const entry = entries.get(id);
    if (!entry) return;
    bytes -= entry.length * 2;
    entries.delete(id);
  };
  return {
    has: (id) => entries.has(id),
    peek: (id) => entries.get(id) || null,
    get(id) {
      const entry = entries.get(id);
      if (!entry) return null;
      entries.delete(id);
      entries.set(id, entry);
      return entry;
    },
    set(id, src) {
      if (!src || typeof src !== 'string' || src.length * 2 > maxBytes || maxItems < 1) return false;
      remove(id);
      entries.set(id, src);
      bytes += src.length * 2;
      while (entries.size > maxItems || bytes > maxBytes) remove(entries.keys().next().value);
      return entries.has(id);
    },
    delete: remove,
    retain(ids) {
      const wanted = new Set(ids);
      for (const id of entries.keys()) if (!wanted.has(id)) remove(id);
    },
    clear() { entries.clear(); bytes = 0; },
    get size() { return entries.size; },
    get bytes() { return bytes; },
  };
}

function pickEvenly(events, limit) {
  if (events.length <= limit) return events;
  if (limit < 1) return [];
  if (limit === 1) return [events[Math.floor(events.length / 2)]];
  return Array.from({ length: limit }, (_, index) => events[Math.round(index * (events.length - 1) / (limit - 1))]);
}

/** Cover the visible samples first, then a quarter-screen margin on each side. */
export function timelinePreviewEvents(events, start, end, limit = TIMELINE_PREVIEW_THUMBNAIL_ITEMS) {
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start || limit < 1) return [];
  const margin = (end - start) * OVERSCAN_RATIO;
  const candidates = events.filter((event) => Number.isSafeInteger(event.id) && event.id > 0
    && Number.isFinite(event.timestamp) && event.timestamp >= start - margin && event.timestamp <= end + margin);
  const visible = candidates.filter((event) => event.timestamp >= start && event.timestamp <= end);
  const edges = candidates.filter((event) => event.timestamp < start || event.timestamp > end);
  const picked = pickEvenly(visible, limit);
  const center = (start + end) / 2;
  return [...picked, ...pickEvenly(edges, limit - picked.length)].sort((a, b) => {
    const priority = (event) => event.timestamp >= start && event.timestamp <= end ? 0 : 1;
    return priority(a) - priority(b) || Math.abs(a.timestamp - center) - Math.abs(b.timestamp - center);
  });
}
