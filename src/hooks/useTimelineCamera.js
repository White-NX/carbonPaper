import { useCallback, useEffect, useLayoutEffect, useMemo, useRef } from 'react';
import {
  DRIFT_LIMITS,
  approach,
  cameraTransform,
  createFlyPath,
  needsRedraw,
} from '../lib/timeline_camera';

/**
 * Time constant of the wheel zoom. Short enough that the view feels attached to
 * the wheel, long enough that a single notch reads as a movement.
 */
const GLIDE_TAU_MS = 55;

/** Remaining zoom error below which the glide is called finished. */
const GLIDE_EPSILON = 1e-3;

/** Longest frame the motion will integrate, so a backgrounded tab cannot jump. */
const MAX_FRAME_MS = 64;

/** Quiet period after the last movement before the content is drawn exactly. */
const SETTLE_MS = 80;

/**
 * Shortest gap between two redraws asked for by drift.
 *
 * Without it a fast gesture would cross the scale limit several times a frame
 * and put the whole render cost straight back, which is what the camera exists
 * to avoid. A gesture never reaches the edge of the drawn area fast enough for
 * this to hold up a redraw that was needed to keep the screen covered.
 */
const MIN_REDRAW_GAP_MS = 55;

/**
 * How far below the caller's own zoom floor a flight may pull back.
 *
 * The path is supposed to leave the normal range on its way, so this only cuts
 * off values that would be absurd rather than merely wide.
 */
const FLIGHT_ZOOM_HEADROOM = 64;

/** Centre implied by holding `anchorTime` still under `anchorX` at the current zoom. */
function anchoredCenter(cam, trackWidth) {
  if (cam.anchorTime === null) return cam.targetCenter;
  return cam.anchorTime - (cam.anchorX - trackWidth / 2) / cam.zoom;
}

/** Abandon whatever motion is running and take the current viewport as the goal. */
function abandonMotion(cam) {
  cam.mode = 'idle';
  cam.flight = null;
  cam.anchorTime = null;
  cam.targetCenter = cam.center;
  cam.targetZoom = cam.zoom;
}

/**
 * Drive the timeline viewport as a camera moving over content that was drawn
 * once.
 *
 * The bands are laid out for a committed viewport, held by the caller in React
 * state. This hook keeps a second, live viewport outside React and expresses the
 * difference between the two as one CSS transform written straight onto the
 * elements in `stages`. A pan or a zoom therefore costs two style writes per
 * band rather than a reconciliation of every block, thumbnail and marker on
 * screen. Only when the difference grows large enough to be seen, or when the
 * movement ends, is the content asked to redraw itself for the new viewport.
 *
 * The second of those writes is the scale, published as `--tl-cam-scale` for the
 * content to divide back out of the parts of itself that stand for no span of
 * time. A stretched icon or label is the one thing about this arrangement the
 * eye picks up immediately, and the correction costs the band a repaint while it
 * moves — still far less than drawing it again.
 *
 * @param {object} options
 * @param {number} options.width Track width in pixels
 * @param {number} options.center Committed centre, in milliseconds
 * @param {number} options.zoom Committed zoom, in pixels per millisecond
 * @param {Array<{current: HTMLElement|null}>} options.stages Elements carrying the transform
 * @param {(view: {center: number, zoom: number}) => void} options.onCommit Adopt a viewport as the drawn one
 * @param {(frame: object) => void} [options.onFrame] Called after every transform write
 */
export default function useTimelineCamera({
  width,
  center,
  zoom,
  minZoom,
  maxZoom,
  stages,
  onCommit,
  onFrame,
}) {
  const camRef = useRef(null);
  if (camRef.current === null) {
    camRef.current = {
      center,
      zoom,
      targetCenter: center,
      targetZoom: zoom,
      anchorTime: null,
      anchorX: 0,
      mode: 'idle',
      flight: null,
      live: false,
      lastFrameAt: 0,
    };
  }

  const committedRef = useRef({ center, zoom });
  const widthRef = useRef(width);
  const boundsRef = useRef({ minZoom, maxZoom });
  const stagesRef = useRef(stages);
  const onCommitRef = useRef(onCommit);
  const onFrameRef = useRef(onFrame);
  const reducedMotionRef = useRef(false);
  const frameRef = useRef(0);
  const settleRef = useRef(null);
  const loopRef = useRef(null);
  const lastRedrawRef = useRef(-Infinity);

  const clampTarget = useCallback((value) => {
    const { minZoom: low, maxZoom: high } = boundsRef.current;
    if (!Number.isFinite(value) || value <= 0) return low;
    return Math.min(high, Math.max(low, value));
  }, []);

  const clampFlightZoom = useCallback((value) => {
    const { minZoom: low, maxZoom: high } = boundsRef.current;
    if (!Number.isFinite(value) || value <= 0) return low;
    return Math.min(high, Math.max(low / FLIGHT_ZOOM_HEADROOM, value));
  }, []);

  const applyTransform = useCallback(() => {
    const cam = camRef.current;
    const trackWidth = widthRef.current;
    const { scale, translate } = cameraTransform(committedRef.current, cam, trackWidth);
    const safeScale = Number.isFinite(scale) && scale > 0 ? scale : 1;
    const safeTranslate = Number.isFinite(translate) ? translate : 0;
    const css = `translate3d(${safeTranslate}px, 0, 0) scaleX(${safeScale})`;

    for (const stage of stagesRef.current || []) {
      const node = stage?.current;
      if (!node) continue;
      // The transform is derived as `scale * x + translate` with x measured from
      // the left edge of the track, so the origin has to sit there. Left at the
      // default centre, every zoom would land half a screen off. Set here rather
      // than in the markup because the arithmetic depends on it.
      node.style.transformOrigin = '0 0';
      node.style.transform = css;
      // Published for the content to undo the horizontal scale on the parts of
      // itself that stand for nothing temporal — an icon, a label, a thumbnail.
      // One property per stage rather than a write per element: the browser
      // hands the value down to every descendant that asks for it, which is
      // what makes the correction affordable at this rate.
      node.style.setProperty('--tl-cam-scale', String(safeScale));
    }

    onFrameRef.current?.({
      center: cam.center,
      zoom: cam.zoom,
      span: trackWidth > 0 ? trackWidth / cam.zoom : 0,
      scale: safeScale,
      translate: safeTranslate,
    });
  }, []);

  /**
   * Ask for the live viewport to become the one the content is drawn for.
   *
   * The redraw is allowed to arrive a frame or two later. Nothing is torn in the
   * meantime because `committedRef` is only advanced from the layout effect, at
   * which point the new content is already in the document: until then the
   * transform is still being measured against the layout actually on screen, so
   * every frame in between shows the content in exactly the right place.
   */
  const commit = useCallback(() => {
    const cam = camRef.current;
    const committed = committedRef.current;
    if (committed.center === cam.center && committed.zoom === cam.zoom) return;

    lastRedrawRef.current = performance.now();
    onCommitRef.current?.({ center: cam.center, zoom: cam.zoom });
  }, []);

  /** Redraw only if the drift has outgrown `limits` and a redraw is due. */
  const commitIfDrifted = useCallback((limits) => {
    if (performance.now() - lastRedrawRef.current < MIN_REDRAW_GAP_MS) return;
    if (!needsRedraw(committedRef.current, camRef.current, widthRef.current, limits)) return;
    commit();
  }, [commit]);

  const scheduleSettle = useCallback(() => {
    if (settleRef.current) clearTimeout(settleRef.current);
    settleRef.current = setTimeout(() => {
      settleRef.current = null;
      commit();
    }, SETTLE_MS);
  }, [commit]);

  const ensureLoop = useCallback(() => {
    if (frameRef.current) return;
    camRef.current.lastFrameAt = 0;
    frameRef.current = requestAnimationFrame(() => loopRef.current?.());
  }, []);

  const runFrame = useCallback(() => {
    frameRef.current = 0;

    const cam = camRef.current;
    const trackWidth = widthRef.current;
    const now = performance.now();
    const elapsed = cam.lastFrameAt ? Math.min(MAX_FRAME_MS, now - cam.lastFrameAt) : 16;
    cam.lastFrameAt = now;

    let moving = false;
    let landed = null;

    if (cam.mode === 'flight' && cam.flight) {
      const { flight } = cam;
      const progress = flight.durationMs > 0
        ? (now - flight.startedAt) / flight.durationMs
        : 1;

      if (progress >= 1) {
        cam.center = cam.targetCenter;
        cam.zoom = cam.targetZoom;
        landed = flight.onDone || null;
        cam.flight = null;
        cam.mode = 'idle';
      } else {
        const point = flight.path.at(progress);
        cam.center = point.center;
        cam.zoom = clampFlightZoom(point.span > 0 ? trackWidth / point.span : cam.zoom);
        moving = true;
      }
    } else if (cam.mode === 'glide') {
      const next = Math.exp(
        approach(Math.log(cam.zoom), Math.log(cam.targetZoom), elapsed, GLIDE_TAU_MS),
      );

      if (Math.abs(Math.log(next / cam.targetZoom)) < GLIDE_EPSILON) {
        cam.zoom = cam.targetZoom;
        cam.mode = 'idle';
        cam.center = anchoredCenter(cam, trackWidth);
        cam.targetCenter = cam.center;
      } else {
        cam.zoom = next;
        cam.center = anchoredCenter(cam, trackWidth);
        moving = true;
      }
    }

    if (cam.live) {
      cam.center = Date.now();
      cam.targetCenter = cam.center;
      moving = true;
    }

    if (moving) {
      commitIfDrifted(cam.mode === 'flight' ? DRIFT_LIMITS.flight : DRIFT_LIMITS.gesture);
      applyTransform();
      if (!frameRef.current) {
        frameRef.current = requestAnimationFrame(() => loopRef.current?.());
      }
    } else {
      // The movement is over, so the content is drawn exactly at once rather
      // than left stretched until a timer notices.
      cam.lastFrameAt = 0;
      commit();
      applyTransform();
    }

    landed?.();
  }, [applyTransform, clampFlightZoom, commit, commitIfDrifted]);

  /** Follow the pointer exactly; a drag that trails the cursor feels like mud. */
  const panBy = useCallback((deltaX) => {
    const cam = camRef.current;
    cam.live = false;
    abandonMotion(cam);

    cam.center -= deltaX / cam.zoom;
    cam.targetCenter = cam.center;

    commitIfDrifted(DRIFT_LIMITS.gesture);
    applyTransform();
    scheduleSettle();
  }, [applyTransform, commitIfDrifted, scheduleSettle]);

  const jumpTo = useCallback((nextCenter, nextZoom) => {
    const cam = camRef.current;
    cam.live = false;
    abandonMotion(cam);

    cam.center = Number.isFinite(nextCenter) ? nextCenter : cam.center;
    cam.zoom = clampTarget(nextZoom);
    cam.targetCenter = cam.center;
    cam.targetZoom = cam.zoom;

    commit();
    applyTransform();
  }, [applyTransform, clampTarget, commit]);

  /**
   * Zoom about a point on screen, so whatever the cursor is over stays there for
   * the whole movement rather than only at its two ends.
   */
  const zoomAt = useCallback((cursorX, factor) => {
    const cam = camRef.current;
    const trackWidth = widthRef.current;
    if (!trackWidth) return;

    cam.live = false;
    if (cam.mode !== 'glide') abandonMotion(cam);

    cam.anchorX = cursorX;
    cam.anchorTime = cam.center + (cursorX - trackWidth / 2) / cam.zoom;
    cam.targetZoom = clampTarget(cam.targetZoom * factor);
    cam.targetCenter = cam.anchorTime - (cursorX - trackWidth / 2) / cam.targetZoom;

    if (reducedMotionRef.current) {
      cam.mode = 'idle';
      cam.zoom = cam.targetZoom;
      cam.center = cam.targetCenter;
      commit();
      applyTransform();
      return;
    }

    cam.mode = 'glide';
    ensureLoop();
  }, [applyTransform, clampTarget, commit, ensureLoop]);

  /** Travel to a distant viewport along a path that keeps the content legible. */
  const flyTo = useCallback((nextCenter, nextZoom, options = {}) => {
    const cam = camRef.current;
    const trackWidth = widthRef.current;
    const targetZoom = clampTarget(nextZoom);

    const path = (reducedMotionRef.current || !trackWidth)
      ? null
      : createFlyPath(
        { center: cam.center, span: trackWidth / cam.zoom },
        { center: nextCenter, span: trackWidth / targetZoom },
      );

    if (!path) {
      jumpTo(nextCenter, targetZoom);
      options.onDone?.();
      return;
    }

    cam.live = false;
    cam.anchorTime = null;
    cam.targetCenter = nextCenter;
    cam.targetZoom = targetZoom;
    cam.mode = 'flight';
    cam.flight = {
      path,
      startedAt: performance.now(),
      durationMs: path.durationMs,
      onDone: options.onDone,
    };

    ensureLoop();
  }, [clampTarget, ensureLoop, jumpTo]);

  /** Pin the centre to the present, redrawing only as the content slides away. */
  const setLive = useCallback((on) => {
    const cam = camRef.current;
    const next = Boolean(on);
    if (cam.live === next) return;

    cam.live = next;
    if (next) {
      cam.anchorTime = null;
      ensureLoop();
    }
  }, [ensureLoop]);

  const read = useCallback(() => {
    const cam = camRef.current;
    const trackWidth = widthRef.current;
    return {
      center: cam.center,
      zoom: cam.zoom,
      span: trackWidth > 0 ? trackWidth / cam.zoom : 0,
    };
  }, []);

  const isFlying = useCallback(() => camRef.current.mode === 'flight', []);

  // Every render: take the newest inputs, then rewrite the transform before the
  // browser paints. A commit lands here, and this is what resets the transform
  // to match the freshly drawn content.
  useLayoutEffect(() => {
    committedRef.current = { center, zoom };
    widthRef.current = width;
    boundsRef.current = { minZoom, maxZoom };
    stagesRef.current = stages;
    onCommitRef.current = onCommit;
    onFrameRef.current = onFrame;
    loopRef.current = runFrame;
    applyTransform();
  });

  useEffect(() => {
    const query = window.matchMedia?.('(prefers-reduced-motion: reduce)');
    if (!query) return undefined;

    reducedMotionRef.current = query.matches;
    const handleChange = (event) => {
      reducedMotionRef.current = event.matches;
    };

    query.addEventListener?.('change', handleChange);
    return () => query.removeEventListener?.('change', handleChange);
  }, []);

  useEffect(() => () => {
    if (frameRef.current) cancelAnimationFrame(frameRef.current);
    if (settleRef.current) clearTimeout(settleRef.current);
    frameRef.current = 0;
    settleRef.current = null;
  }, []);

  return useMemo(
    () => ({ panBy, zoomAt, flyTo, jumpTo, setLive, read, isFlying }),
    [panBy, zoomAt, flyTo, jumpTo, setLive, read, isFlying],
  );
}
