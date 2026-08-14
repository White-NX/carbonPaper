import { describe, expect, it } from 'vitest';
import {
  DRIFT_LIMITS,
  FLY_MAX_MS,
  FLY_MIN_MS,
  approach,
  cameraTransform,
  createFlyPath,
  needsRedraw,
  smoothstep,
} from './timeline_camera';

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const WIDTH = 1000;

/** Screen position of a moment under a viewport, the way the bands lay it out. */
const project = (viewport, time) => WIDTH / 2 + (time - viewport.center) * viewport.zoom;

describe('cameraTransform', () => {
  it('is the identity when nothing has moved', () => {
    const viewport = { center: 1_000_000, zoom: 0.01 };
    const { scale, translate } = cameraTransform(viewport, viewport, WIDTH);

    expect(scale).toBe(1);
    expect(translate).toBe(0);
  });

  it('turns a pan into a pure translation', () => {
    const committed = { center: 1_000_000, zoom: 0.01 };
    const live = { center: 1_000_000 + 5000, zoom: 0.01 };
    const { scale, translate } = cameraTransform(committed, live, WIDTH);

    expect(scale).toBe(1);
    // Moving the viewport forward in time carries the content to the left
    expect(translate).toBeCloseTo(-50);
  });

  it('maps every moment to where the live viewport wants it', () => {
    const committed = { center: 1_700_000_000_000, zoom: 0.004 };
    const live = { center: 1_700_000_120_000, zoom: 0.0057 };
    const { scale, translate } = cameraTransform(committed, live, WIDTH);

    for (const offset of [-90_000, -1000, 0, 1000, 250_000]) {
      const time = committed.center + offset;
      expect(scale * project(committed, time) + translate).toBeCloseTo(project(live, time), 6);
    }
  });
});

describe('needsRedraw', () => {
  const committed = { center: 1_000_000, zoom: 0.01 };

  it('leaves a small drift alone', () => {
    const live = { center: committed.center + 10_000, zoom: 0.0105 };
    expect(needsRedraw(committed, live, WIDTH, DRIFT_LIMITS.gesture)).toBe(false);
  });

  it('asks for a redraw once the content has slid too far', () => {
    // 40_000 ms at 0.01 px/ms is 400 px, past the 300 px gesture limit
    const live = { center: committed.center + 40_000, zoom: 0.01 };
    expect(needsRedraw(committed, live, WIDTH, DRIFT_LIMITS.gesture)).toBe(true);
  });

  it('asks for a redraw once the content has stretched too far', () => {
    const live = { center: committed.center, zoom: 0.01 * 1.6 };
    expect(needsRedraw(committed, live, WIDTH, DRIFT_LIMITS.gesture)).toBe(true);
    expect(needsRedraw(committed, live, WIDTH, DRIFT_LIMITS.flight)).toBe(false);
  });

  it('treats a broken viewport as needing a redraw', () => {
    expect(needsRedraw(committed, { center: 0, zoom: 0 }, WIDTH, DRIFT_LIMITS.gesture)).toBe(true);
    expect(needsRedraw(committed, { center: NaN, zoom: 0.01 }, WIDTH, DRIFT_LIMITS.gesture)).toBe(true);
  });
});

describe('approach', () => {
  it('closes most of the gap over one time constant', () => {
    // 1 − 1/e of the way there
    expect(approach(0, 100, 50, 50)).toBeCloseTo(63.21, 1);
  });

  it('does not depend on how the elapsed time is cut up', () => {
    const oneStep = approach(0, 100, 32, 55);
    const twoSteps = approach(approach(0, 100, 16, 55), 100, 16, 55);

    expect(twoSteps).toBeCloseTo(oneStep, 9);
  });

  it('snaps to the target when asked for no smoothing at all', () => {
    expect(approach(0, 100, 16, 0)).toBe(100);
  });
});

describe('smoothstep', () => {
  it('pins both ends and stays clamped outside them', () => {
    expect(smoothstep(0)).toBe(0);
    expect(smoothstep(1)).toBe(1);
    expect(smoothstep(-3)).toBe(0);
    expect(smoothstep(4)).toBe(1);
    expect(smoothstep(0.5)).toBeCloseTo(0.5);
  });
});

describe('createFlyPath', () => {
  const now = 1_700_000_000_000;

  it('has nothing to do when the viewports match', () => {
    expect(createFlyPath({ center: now, span: HOUR }, { center: now, span: HOUR })).toBeNull();
  });

  it('refuses a viewport it cannot fly to', () => {
    expect(createFlyPath({ center: now, span: HOUR }, { center: now, span: 0 })).toBeNull();
    expect(createFlyPath({ center: NaN, span: HOUR }, { center: now, span: DAY })).toBeNull();
  });

  it('starts where it was asked to and finishes where it was sent', () => {
    const path = createFlyPath(
      { center: now, span: 30 * DAY },
      { center: now - 12 * DAY, span: 2 * MINUTE },
    );

    expect(path.at(0)).toEqual({ center: now, span: 30 * DAY });
    expect(path.at(1)).toEqual({ center: now - 12 * DAY, span: 2 * MINUTE });
  });

  it('arrives by the formula, not only by the shortcut at the end', () => {
    const from = { center: now, span: 30 * DAY };
    const to = { center: now - 12 * DAY, span: 2 * MINUTE };
    const path = createFlyPath(from, to);

    const almost = path.at(1 - 1e-7);
    expect(Math.abs(almost.center - to.center) / from.span).toBeLessThan(1e-5);
    expect(almost.span / to.span).toBeCloseTo(1, 3);
  });

  it('interpolates a pure zoom geometrically', () => {
    const path = createFlyPath({ center: now, span: HOUR }, { center: now, span: 100 * HOUR });
    const middle = path.at(0.5);

    expect(middle.center).toBe(now);
    // smoothstep leaves the midpoint at one half, so this is the geometric mean
    expect(middle.span / HOUR).toBeCloseTo(10, 6);
  });

  it('pulls back before travelling a long way', () => {
    const from = { center: now, span: 2 * MINUTE };
    const to = { center: now - 30 * DAY, span: 2 * MINUTE };
    const path = createFlyPath(from, to);

    let widest = 0;
    for (let step = 0; step <= 40; step += 1) {
      widest = Math.max(widest, path.at(step / 40).span);
    }

    // Wide enough to hold the whole journey, and no wider than it needs to be
    expect(widest).toBeGreaterThan(10 * DAY);
    expect(widest).toBeLessThan(60 * DAY);
  });

  it('moves the centre steadily forward on a pure pan', () => {
    const path = createFlyPath(
      { center: now, span: HOUR },
      { center: now + 8 * HOUR, span: HOUR },
    );

    let previous = -Infinity;
    for (let step = 0; step <= 20; step += 1) {
      const { center } = path.at(step / 20);
      expect(center).toBeGreaterThanOrEqual(previous);
      previous = center;
    }
  });

  it('keeps the duration inside its bounds', () => {
    const short = createFlyPath({ center: now, span: HOUR }, { center: now, span: 1.2 * HOUR });
    const long = createFlyPath({ center: now, span: 90 * DAY }, { center: now, span: MINUTE });

    expect(short.durationMs).toBe(FLY_MIN_MS);
    expect(long.durationMs).toBe(FLY_MAX_MS);
  });

  it('stays finite across a jump of many orders of magnitude', () => {
    const path = createFlyPath(
      { center: now, span: 20 * 365 * DAY },
      { center: now - 3 * 365 * DAY, span: 1000 },
    );

    for (let step = 0; step <= 20; step += 1) {
      const point = path.at(step / 20);
      expect(Number.isFinite(point.center)).toBe(true);
      expect(Number.isFinite(point.span)).toBe(true);
      expect(point.span).toBeGreaterThan(0);
    }
  });
});
