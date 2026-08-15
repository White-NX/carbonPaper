import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  SPAN_LADDER_MS,
  createThrottle,
  isRangeCovered,
  resolveZoomLevel,
  snapSpanUpMs,
} from './timeline_viewport';

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Ladder index of the one-hour step, used as a readable anchor in the tests. */
const HOUR_LEVEL = SPAN_LADDER_MS.indexOf(HOUR);

describe('snapSpanUpMs', () => {
  it('leaves a value that is already a ladder step alone', () => {
    expect(snapSpanUpMs(HOUR)).toBe(HOUR);
  });

  it('rounds up to the next step', () => {
    expect(snapSpanUpMs(HOUR + 1)).toBe(2 * HOUR);
    expect(snapSpanUpMs(45 * MINUTE)).toBe(HOUR);
  });

  it('clamps at both ends instead of returning nonsense', () => {
    expect(snapSpanUpMs(0)).toBe(SPAN_LADDER_MS[0]);
    expect(snapSpanUpMs(-1)).toBe(SPAN_LADDER_MS[0]);
    expect(snapSpanUpMs(Number.NaN)).toBe(SPAN_LADDER_MS[0]);
    expect(snapSpanUpMs(1e15)).toBe(SPAN_LADDER_MS[SPAN_LADDER_MS.length - 1]);
  });
});

describe('resolveZoomLevel', () => {
  it('picks the closest step when there is no previous level', () => {
    expect(resolveZoomLevel(HOUR)).toBe(HOUR_LEVEL);
    expect(resolveZoomLevel(DAY)).toBe(SPAN_LADDER_MS.indexOf(DAY));
  });

  it('is idempotent so it can be recomputed every render', () => {
    const once = resolveZoomLevel(5_400_000);
    expect(resolveZoomLevel(5_400_000, once)).toBe(once);
  });

  it('holds the current level while the span sits near a boundary', () => {
    // The 1h/2h boundary is at ~1.41h; 1.5h is past it but inside the margin.
    expect(resolveZoomLevel(1.5 * HOUR, HOUR_LEVEL)).toBe(HOUR_LEVEL);
    expect(resolveZoomLevel(1.33 * HOUR, HOUR_LEVEL + 1)).toBe(HOUR_LEVEL + 1);
  });

  it('gives way once the span clears the boundary by the full margin', () => {
    expect(resolveZoomLevel(1.7 * HOUR, HOUR_LEVEL)).toBe(HOUR_LEVEL + 1);
    expect(resolveZoomLevel(1.1 * HOUR, HOUR_LEVEL + 1)).toBe(HOUR_LEVEL);
  });

  it('jumps straight to the right level after a quick-zoom button', () => {
    expect(resolveZoomLevel(DAY, HOUR_LEVEL)).toBe(SPAN_LADDER_MS.indexOf(DAY));
  });

  it('tolerates a previous level outside the ladder', () => {
    expect(resolveZoomLevel(HOUR, 999)).toBe(HOUR_LEVEL);
    expect(resolveZoomLevel(HOUR, -5)).toBe(HOUR_LEVEL);
  });

  it('falls back to the finest level for a span it cannot use', () => {
    expect(resolveZoomLevel(0)).toBe(0);
    expect(resolveZoomLevel(Number.NaN)).toBe(0);
  });
});

describe('isRangeCovered', () => {
  const loaded = { start: 0, end: 1000 };

  it('reports nothing loaded as uncovered', () => {
    expect(isRangeCovered(null, 100, 200, 0)).toBe(false);
  });

  it('accepts a viewport well inside the loaded range', () => {
    expect(isRangeCovered(loaded, 400, 600, 100)).toBe(true);
  });

  it('rejects a viewport that has drifted into the margin', () => {
    expect(isRangeCovered(loaded, 50, 600, 100)).toBe(false);
    expect(isRangeCovered(loaded, 400, 950, 100)).toBe(false);
  });
});

describe('createThrottle', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('runs the first call straight away', () => {
    const spy = vi.fn();
    createThrottle(spy, 100)('a');

    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith('a');
  });

  it('collapses a burst into one leading and one trailing call', () => {
    const spy = vi.fn();
    const throttled = createThrottle(spy, 100);

    throttled(1);
    throttled(2);
    throttled(3);
    expect(spy).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(100);
    // The position the caller settled on is not lost
    expect(spy).toHaveBeenCalledTimes(2);
    expect(spy).toHaveBeenLastCalledWith(3);
  });

  it('runs again once the window has passed', () => {
    const spy = vi.fn();
    const throttled = createThrottle(spy, 100);

    throttled(1);
    vi.advanceTimersByTime(200);
    throttled(2);

    expect(spy).toHaveBeenCalledTimes(2);
    expect(spy).toHaveBeenLastCalledWith(2);
  });

  it('drops the trailing call when cancelled', () => {
    const spy = vi.fn();
    const throttled = createThrottle(spy, 100);

    throttled(1);
    throttled(2);
    throttled.cancel();
    vi.advanceTimersByTime(500);

    expect(spy).toHaveBeenCalledTimes(1);
  });
});
