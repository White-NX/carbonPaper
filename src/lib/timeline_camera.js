/**
 * Camera model for the timeline.
 *
 * The bands draw their content once for a committed viewport and are then moved
 * by a single CSS transform, so panning and zooming cost one composited layer
 * instead of a full re-render of every block, thumbnail and tick. This module
 * holds the pure parts of that arrangement: the transform that maps the
 * committed viewport onto the live one, the two motion models the camera runs
 * on, and the rule for deciding that the drift has grown far enough that the
 * content has to be drawn again.
 */

/**
 * Curvature of the flight path, from Van Wijk and Nuij, "Smooth and efficient
 * zooming and panning" (2003). The square root of two is the value their user
 * study settled on, and the one d3 adopted.
 */
const RHO = Math.SQRT2;
const RHO2 = 2;
const RHO4 = 4;

/** Distance, relative to the starting span, below which a flight is pure zoom. */
const PURE_ZOOM_RATIO = 1e-9;

/** Flight duration per unit of path length, and the bounds it is held between. */
export const FLY_MS_PER_UNIT = 130;
export const FLY_MIN_MS = 200;
export const FLY_MAX_MS = 620;

/**
 * How far the live viewport may drift from the drawn one before the content is
 * drawn again.
 *
 * The three bounds are held to different standards, each for its own reason.
 *
 * A translation is rigid, so the content stays perfectly readable however far
 * it has slid and only the edge of the drawn area sets the limit.
 *
 * A scale used to be the tight one, because it stretched the labels and the
 * icons along with the blocks. It no longer does: the bands divide the camera's
 * scale back out of anything that stands for no span of time (see `.tl-steady`
 * in `index.css`). What is left is a question of room, and room is asymmetric.
 * Zooming in gives every block more of it than the labels inside were laid out
 * for, which shows only as slightly wider gaps. Zooming out takes room away,
 * and a label drawn for a block wider than the one now on screen has to be cut
 * off at its edge, so that direction is held roughly twice as close.
 *
 * A flight is over in half a second and its content is mostly a blur, so it is
 * allowed to run much further on one drawing — though not as far outward as it
 * once was, because the pull back at the start of the path is the slow, legible
 * part of the movement.
 */
export const DRIFT_LIMITS = {
  gesture: { translateRatio: 0.3, zoomIn: 1.15, zoomOut: 1.08 },
  flight: { translateRatio: 0.6, zoomIn: 2, zoomOut: 1.4 },
};

/**
 * How far past each screen edge content is drawn, as a fraction of the width.
 *
 * It has to exceed what the gesture drift limits allow, otherwise a pan would
 * pull the edge of the drawn content into view and leave a blank strip behind
 * it.
 */
export const RENDER_OVERSCAN_RATIO = 0.35;

/** Clamp to the unit interval. */
function clamp01(value) {
  if (!Number.isFinite(value)) return 0;
  return Math.min(1, Math.max(0, value));
}

/** Smoothstep, used to soften the two ends of a flight. */
export function smoothstep(t) {
  const x = clamp01(t);
  return x * x * (3 - 2 * x);
}

/**
 * Affine transform carrying the committed viewport onto the live one.
 *
 * Screen position is linear in time, `x(t) = width / 2 + (t - center) * zoom`,
 * so the two viewports differ by exactly one scale and one offset no matter how
 * far apart they are. Applied as `translateX(translate) scaleX(scale)` with the
 * transform origin at the left edge.
 *
 * @param {{center: number, zoom: number}} committed Viewport the content was drawn for
 * @param {{center: number, zoom: number}} live Viewport the user is looking at
 * @param {number} width Track width in pixels
 */
export function cameraTransform(committed, live, width) {
  const scale = live.zoom / committed.zoom;
  const translate = (width / 2) * (1 - scale) + live.zoom * (committed.center - live.center);
  return { scale, translate };
}

/**
 * Whether the drift has outgrown `limits` and the content must be drawn again.
 */
export function needsRedraw(committed, live, width, limits) {
  const { scale, translate } = cameraTransform(committed, live, width);
  if (!Number.isFinite(scale) || !Number.isFinite(translate) || scale <= 0) return true;

  return Math.abs(translate) > width * limits.translateRatio
    || scale > limits.zoomIn
    || scale < 1 / limits.zoomOut;
}

/**
 * Move `current` a frame's worth of the way toward `target`.
 *
 * An exponential approach rather than a fixed-length tween, because the wheel
 * is continuous input: a tween would restart on every notch and stutter, while
 * this simply keeps chasing a target that has moved further ahead. Being
 * expressed in terms of elapsed time rather than frames, it also behaves the
 * same on a 60 and a 144 hertz display.
 *
 * @param {number} tauMs Time constant; the gap shrinks to 37% of itself over it
 */
export function approach(current, target, dtMs, tauMs) {
  if (!(tauMs > 0)) return target;
  if (!Number.isFinite(current)) return target;
  return current + (target - current) * (1 - Math.exp(-dtMs / tauMs));
}

/**
 * Plan a flight from one viewport to another along Van Wijk's path.
 *
 * Interpolating position and zoom separately looks wrong over any real distance:
 * halfway through a jump across a month the view is still magnified enough that
 * the content streaks past unreadably. Van Wijk's path pulls back first, travels
 * while zoomed out, and settles in again, which keeps the apparent speed bounded
 * and is the reason the move reads as a camera rather than a jump cut.
 *
 * The formulation below is the paper's, rearranged so every quantity is measured
 * in units of the starting span. In milliseconds the squared spans of the raw
 * formulation reach 10^19, where the differences it takes lose most of their
 * significant digits.
 *
 * @param {{center: number, span: number}} from Viewport to leave
 * @param {{center: number, span: number}} to Viewport to arrive at
 * @returns {{durationMs: number, at: (progress: number) => {center: number, span: number}}|null}
 *   Null when there is nothing worth animating.
 */
export function createFlyPath(from, to) {
  const startCenter = from?.center;
  const startSpan = from?.span;
  const endCenter = to?.center;
  const endSpan = to?.span;

  const finite = [startCenter, startSpan, endCenter, endSpan].every(Number.isFinite);
  if (!finite || startSpan <= 0 || endSpan <= 0) return null;

  const shift = endCenter - startCenter;
  const distance = Math.abs(shift) / startSpan;
  const growth = endSpan / startSpan;

  let pathLength;
  let sample;

  if (distance < PURE_ZOOM_RATIO) {
    const logGrowth = Math.log(growth);
    pathLength = Math.abs(logGrowth) / RHO;
    sample = (t) => ({
      center: startCenter + shift * t,
      span: startSpan * Math.exp(logGrowth * t),
    });
  } else {
    const b0 = (growth * growth - 1 + RHO4 * distance * distance) / (2 * RHO2 * distance);
    const b1 = (growth * growth - 1 - RHO4 * distance * distance) / (2 * growth * RHO2 * distance);
    // log(sqrt(b² + 1) − b) is −asinh(b). Written the second way it survives a
    // large b, where the subtraction inside the logarithm cancels away to zero.
    const r0 = -Math.asinh(b0);
    const r1 = -Math.asinh(b1);
    pathLength = (r1 - r0) / RHO;

    const coshR0 = Math.cosh(r0);
    const sinhR0 = Math.sinh(r0);
    sample = (t) => {
      const s = t * pathLength;
      const travelled = (coshR0 * Math.tanh(RHO * s + r0) - sinhR0) / (RHO2 * distance);
      return {
        center: startCenter + travelled * shift,
        span: (startSpan * coshR0) / Math.cosh(RHO * s + r0),
      };
    };
  }

  if (!Number.isFinite(pathLength) || pathLength <= 0) return null;

  const durationMs = Math.min(
    FLY_MAX_MS,
    Math.max(FLY_MIN_MS, pathLength * FLY_MS_PER_UNIT),
  );

  return {
    durationMs,
    at: (progress) => (progress >= 1
      ? { center: endCenter, span: endSpan }
      : sample(smoothstep(progress))),
  };
}
