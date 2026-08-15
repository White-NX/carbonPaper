/**
 * Zoom quantization and loaded-range bookkeeping for the timeline.
 *
 * Two ideas live here. The first is that every zoom-dependent quantity snaps
 * onto a shared ladder, so it holds still while the user scrolls inside one
 * level instead of drifting with each pixel. The second is that fetches are
 * driven by whether the viewport is still covered by what is already in memory,
 * not by how long the pointer has been still, so panning keeps loading.
 */

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const YEAR = 365 * DAY;

/**
 * Standardized durations that zoom-dependent quantities snap onto.
 *
 * The first twenty-one steps mirror `snap_bucket_seconds` in
 * `src-tauri/src/storage/screenshot.rs`, so a request span maps onto a stable
 * server-side sampling bucket rather than a slightly different one on every
 * fetch. The remaining steps extend past the thirty-day cap the backend never
 * needs, because the timeline zooms out to several years.
 */
export const SPAN_LADDER_MS = [
  SECOND, 2 * SECOND, 5 * SECOND, 10 * SECOND, 15 * SECOND, 30 * SECOND,
  MINUTE, 2 * MINUTE, 5 * MINUTE, 10 * MINUTE, 15 * MINUTE, 30 * MINUTE,
  HOUR, 2 * HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR,
  DAY, 2 * DAY, 7 * DAY, 30 * DAY,
  60 * DAY, 90 * DAY, 180 * DAY, YEAR,
  2 * YEAR, 5 * YEAR, 10 * YEAR, 20 * YEAR,
];

const LAST_LEVEL = SPAN_LADDER_MS.length - 1;

/** Round a duration up to the next ladder step. */
export function snapSpanUpMs(raw) {
  if (!Number.isFinite(raw) || raw <= 0) return SPAN_LADDER_MS[0];
  return SPAN_LADDER_MS.find((step) => step >= raw) ?? SPAN_LADDER_MS[LAST_LEVEL];
}

/** Ladder index closest to `span`, measured on a logarithmic scale. */
function nearestLevel(span) {
  let best = 0;
  let bestDistance = Infinity;
  for (let index = 0; index <= LAST_LEVEL; index += 1) {
    const distance = Math.abs(Math.log(SPAN_LADDER_MS[index] / span));
    if (distance < bestDistance) {
      bestDistance = distance;
      best = index;
    }
  }
  return best;
}

/**
 * How far past a level boundary a span must travel before the level is allowed
 * to change. Without it a span resting on a boundary flips back and forth on
 * every pixel of scroll, rebuilding every session block each time.
 */
const LEVEL_HYSTERESIS = 1.12;

/**
 * Resolve the zoom level for `span`, staying on `previousLevel` until the span
 * clears the boundary by {@link LEVEL_HYSTERESIS}.
 *
 * Feeding the result back in as `previousLevel` returns the same level, so the
 * caller may hold it in a ref and recompute during render without drifting.
 *
 * @param {number} span Visible time span in milliseconds
 * @param {number|null} previousLevel Level resolved on the previous render
 */
export function resolveZoomLevel(span, previousLevel = null) {
  if (!Number.isFinite(span) || span <= 0) return 0;

  const natural = nearestLevel(span);
  if (previousLevel === null || previousLevel === undefined) return natural;

  const current = Math.min(Math.max(0, Math.trunc(previousLevel)), LAST_LEVEL);
  if (natural === current) return current;

  // Boundaries sit at the geometric mean of two neighbouring steps, because the
  // ladder is geometric rather than linear.
  if (natural > current) {
    const boundary = Math.sqrt(SPAN_LADDER_MS[current] * SPAN_LADDER_MS[current + 1]);
    if (span <= boundary * LEVEL_HYSTERESIS) return current;
  } else {
    const boundary = Math.sqrt(SPAN_LADDER_MS[current] * SPAN_LADDER_MS[current - 1]);
    if (span >= boundary / LEVEL_HYSTERESIS) return current;
  }

  return natural;
}

/**
 * Whether `[wantStart, wantEnd]` still sits inside an already loaded range with
 * `margin` to spare on both sides.
 *
 * The margin is what turns a drag into a handful of fetches instead of one per
 * frame: the viewport may wander freely until it approaches the edge of loaded
 * data, and only then is a wider range requested.
 *
 * @param {{start: number, end: number}|null} loaded Range currently held or requested
 */
export function isRangeCovered(loaded, wantStart, wantEnd, margin = 0) {
  if (!loaded) return false;
  return wantStart >= loaded.start + margin && wantEnd <= loaded.end - margin;
}

/**
 * Throttle `func` to one call per `waitMs`, keeping a trailing call so the final
 * arguments of a gesture are never dropped.
 *
 * A debounce is the wrong tool for pan and zoom: the pointer resets its timer on
 * every frame, so nothing runs until the user stops. A leading-edge-only
 * throttle has the opposite flaw and loses the position the user settles on.
 */
export function createThrottle(func, waitMs) {
  let lastRun = -Infinity;
  let timer = null;
  let pendingArgs = null;

  const run = (args) => {
    lastRun = Date.now();
    pendingArgs = null;
    func(...args);
  };

  const throttled = (...args) => {
    const elapsed = Date.now() - lastRun;
    if (timer === null && elapsed >= waitMs) {
      run(args);
      return;
    }

    pendingArgs = args;
    if (timer === null) {
      timer = setTimeout(() => {
        timer = null;
        if (pendingArgs) run(pendingArgs);
      }, Math.max(0, waitMs - elapsed));
    }
  };

  throttled.cancel = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    pendingArgs = null;
  };

  return throttled;
}
