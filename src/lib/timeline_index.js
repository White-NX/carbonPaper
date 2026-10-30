/** Complete, ordered [ID, Unix milliseconds] pairs for one loaded range. */
export function createTimelineIndex(entries, start, end) {
  if (!Number.isFinite(start) || !Number.isFinite(end) || start >= end) throw new Error('Invalid timeline index range');
  // Retain 16 bytes per screenshot rather than one JS object per history row.
  const size = entries.length;
  const data = new Float64Array(size * 2);
  for (let i = 0; i < size; i += 1) {
    const [id, time] = entries[i];
    if (!Number.isSafeInteger(id) || id <= 0 || !Number.isSafeInteger(time)
      || (i > 0 && (time < data[i * 2 - 1] || (time === data[i * 2 - 1] && id <= data[i * 2 - 2])))) {
      throw new Error('Invalid timeline index entry');
    }
    data[i * 2] = id;
    data[i * 2 + 1] = time;
  }
  const bound = (time, inclusive) => {
    let low = 0;
    let high = size;
    while (low < high) {
      const middle = Math.floor((low + high) / 2);
      const value = data[middle * 2 + 1];
      if (value < time || (inclusive && value === time)) low = middle + 1;
      else high = middle;
    }
    return low;
  };
  const at = (i) => ({ id: data[i * 2], timestamp: data[i * 2 + 1] });
  return {
    start, end, size, byteLength: data.byteLength,
    nearest(time) {
      // Undefined means coverage is missing; null means confirmed empty history.
      if (!Number.isFinite(time) || time < start || time > end) return undefined;
      if (!data.length) return null;
      const next = bound(time, true);
      if (next === 0) return at(0);
      if (next === size) return at(next - 1);
      const before = at(next - 1);
      const after = at(next);
      // Match storage: earlier time wins a tie; the before candidate uses the
      // last ID at its timestamp, while the after candidate uses the first ID.
      return time - before.timestamp <= after.timestamp - time ? before : after;
    },
    between(from, to, limit) {
      const first = bound(from, false);
      const last = bound(to, true);
      if (last - first > limit) return null;
      return Array.from({ length: last - first }, (_, i) => at(first + i));
    },
  };
}

/** Prefetch the exact IDs a pointer can select at screen/device pixel positions. */
export function timelineIndexedPreviewEvents(index, start, end, {
  width = 256, pixelRatio = 1, trackLeft = 0, limit = 2048,
} = {}) {
  if (!index || !Number.isFinite(start) || !Number.isFinite(end) || start >= end || limit < 1
    || start < index.start || end > index.end) return [];
  const center = (start + end) / 2;
  const span = end - start;
  const selected = new Map();
  const add = (event, priority) => {
    if (event && !selected.has(event.id) && selected.size < limit) selected.set(event.id, { event, priority });
  };
  const ratio = Number.isFinite(pixelRatio) && pixelRatio > 0 ? pixelRatio : 1;
  const screenWidth = Number.isFinite(width) && width > 0 ? width : 256;
  const firstPhysicalPixel = Math.ceil(trackLeft * ratio);
  const firstPixel = firstPhysicalPixel / ratio - trackLeft;
  const pixels = Math.floor((screenWidth - firstPixel) * ratio) + 1;
  const count = Math.min(limit, pixels + 2);
  const visible = index.between(start, end, count);
  if (visible) visible.forEach((event) => add(event, 0));
  // Cover boundaries even when the nearest screenshot is outside the viewport.
  add(index.nearest(start), 0);
  add(index.nearest(end), 0);
  if (!visible) {
    const steps = Math.max(1, count - 2);
    const times = [];
    for (let i = 0; i < steps; i += 1) {
      const pixel = steps >= pixels ? i : Math.round(i * (pixels - 1) / Math.max(1, steps - 1));
      const x = Math.max(0, Math.min(screenWidth, (firstPhysicalPixel + pixel) / ratio - trackLeft));
      const time = center + (x - screenWidth / 2) / (screenWidth / span);
      add(index.nearest(time), 0);
      times.push(time);
    }
    for (const time of times) {
      // Arithmetic order can move a floating-point pointer time across an exact
      // midpoint. Warm both IDs there, while selection still uses one exact ID.
      add(index.nearest(Math.max(start, time - 1)), 0);
      add(index.nearest(Math.min(end, time + 1)), 0);
    }
  }
  // A quarter-screen margin on each side makes small pans reuse warm entries.
  const marginSteps = Math.min(limit - selected.size, Math.ceil(pixels / 2));
  for (let i = 0; i < marginSteps; i += 1) {
    const side = i % 2 ? 1 : -1;
    const offset = span * 0.25 * (Math.floor(i / 2) + 1) / Math.max(1, Math.ceil(marginSteps / 2));
    add(index.nearest((side < 0 ? start : end) + side * offset), 1);
  }
  return [...selected.values()].sort((a, b) => a.priority - b.priority
    || Math.abs(a.event.timestamp - center) - Math.abs(b.event.timestamp - center))
    .map(({ event }) => event);
}
