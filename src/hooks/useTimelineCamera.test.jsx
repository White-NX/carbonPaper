import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import useTimelineCamera from './useTimelineCamera';

const WIDTH = 1000;
const CENTER = 1_700_000_000_000;
const ZOOM = 0.01;
const DAY = 86_400_000;

/** Frames and the clock are driven by hand so the motion is deterministic. */
let clock = 0;
let pendingFrames = [];

function advance(totalMs, steps = 16) {
  for (let step = 0; step < steps; step += 1) {
    clock += totalMs / steps;
    const due = pendingFrames;
    pendingFrames = [];
    for (const callback of due) callback(clock);
  }
}

/**
 * Mount the camera over one stage element, recording what it asks to commit.
 * The caller normally feeds a commit back as new props, which `adopt` does.
 */
function mountCamera({ width = WIDTH } = {}) {
  const commits = [];
  const stage = { current: document.createElement('div') };
  const stages = [stage];

  const hook = renderHook(
    ({ view }) => useTimelineCamera({
      width,
      center: view.center,
      zoom: view.zoom,
      minZoom: 1e-9,
      maxZoom: 1,
      stages,
      onCommit: (next) => commits.push(next),
    }),
    { initialProps: { view: { center: CENTER, zoom: ZOOM } } },
  );

  return {
    camera: () => hook.result.current,
    commits,
    stage,
    adopt: () => act(() => {
      hook.rerender({ view: commits[commits.length - 1] });
    }),
  };
}

beforeEach(() => {
  clock = 0;
  pendingFrames = [];
  vi.spyOn(performance, 'now').mockImplementation(() => clock);
  vi.stubGlobal('requestAnimationFrame', (callback) => pendingFrames.push(callback));
  vi.stubGlobal('cancelAnimationFrame', () => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('useTimelineCamera', () => {
  it('starts with the content exactly where it was drawn', () => {
    const { stage } = mountCamera();
    expect(stage.current.style.transform).toBe('translate3d(0px, 0, 0) scaleX(1)');
  });

  it('scales about the left edge, which is where the arithmetic measures from', () => {
    const { camera, stage } = mountCamera();

    act(() => camera().zoomAt(WIDTH / 2, 1.2));
    act(() => advance(400));

    expect(stage.current.style.transformOrigin).toBe('0 0');
    // Zooming about the middle leaves the middle where it is: 500 = 1.2 * 500 + t
    const translate = Number(/translate3d\((-?[\d.]+)px/.exec(stage.current.style.transform)[1]);
    const scale = Number(/scaleX\(([\d.]+)\)/.exec(stage.current.style.transform)[1]);
    expect(scale * (WIDTH / 2) + translate).toBeCloseTo(WIDTH / 2, 6);
  });

  it('publishes the scale so the content can take it back out of its labels', () => {
    const { camera, stage } = mountCamera();

    expect(stage.current.style.getPropertyValue('--tl-cam-scale')).toBe('1');

    act(() => camera().zoomAt(WIDTH / 2, 1.2));
    act(() => advance(400));

    const published = Number(stage.current.style.getPropertyValue('--tl-cam-scale'));
    const scale = Number(/scaleX\(([\d.]+)\)/.exec(stage.current.style.transform)[1]);
    expect(published).toBeCloseTo(scale, 12);
  });

  it('moves a drag with the transform instead of asking for a redraw', () => {
    const { camera, commits, stage } = mountCamera();

    act(() => camera().panBy(120));

    // Dragging to the right carries the content with the pointer
    expect(stage.current.style.transform).toBe('translate3d(120px, 0, 0) scaleX(1)');
    expect(commits).toHaveLength(0);
  });

  it('asks for a redraw once the drag has pulled the content too far', () => {
    const { camera, commits } = mountCamera();

    act(() => camera().panBy(200));
    expect(commits).toHaveLength(0);

    act(() => camera().panBy(200));
    expect(commits).toHaveLength(1);
    // 400 px at 0.01 px per ms is 40 s of timeline, travelled backwards
    expect(commits[0].center).toBeCloseTo(CENTER - 40_000, 6);
  });

  it('puts the content back where it belongs after a redraw is taken up', () => {
    const { camera, stage, adopt } = mountCamera();

    act(() => camera().panBy(400));
    adopt();

    expect(stage.current.style.transform).toBe('translate3d(0px, 0, 0) scaleX(1)');
  });

  it('holds the moment under the cursor still for the whole zoom', () => {
    const { camera, stage } = mountCamera();
    const cursorX = 250;
    const timeAtCursor = CENTER + (cursorX - WIDTH / 2) / ZOOM;

    act(() => camera().zoomAt(cursorX, 1.2));

    for (let step = 0; step < 6; step += 1) {
      act(() => advance(30, 2));
      const { center, zoom } = camera().read();
      expect(center + (cursorX - WIDTH / 2) / zoom).toBeCloseTo(timeAtCursor, 6);
    }

    expect(stage.current.style.transform).toMatch(/scaleX\(1\.[01]/);
  });

  it('settles on the zoom it was asked for', () => {
    const { camera } = mountCamera();

    act(() => camera().zoomAt(WIDTH / 2, 1.2));
    act(() => advance(600));

    expect(camera().read().zoom).toBeCloseTo(ZOOM * 1.2, 12);
  });

  it('flies to a distant viewport rather than arriving at once', () => {
    const { camera, commits } = mountCamera();
    const destination = CENTER - 30 * DAY;

    act(() => camera().flyTo(destination, ZOOM));

    expect(camera().isFlying()).toBe(true);
    expect(commits).toHaveLength(0);

    act(() => advance(1000));

    expect(camera().isFlying()).toBe(false);
    expect(camera().read().center).toBe(destination);
    expect(commits[commits.length - 1]).toEqual({ center: destination, zoom: ZOOM });
  });

  it('pulls back on the way, so the journey is never a smear', () => {
    const { camera } = mountCamera();
    const widths = [];

    act(() => camera().flyTo(CENTER - 30 * DAY, ZOOM));
    for (let step = 0; step < 20; step += 1) {
      act(() => advance(25, 1));
      widths.push(camera().read().span);
    }

    expect(Math.max(...widths)).toBeGreaterThan(10 * DAY);
  });

  it('reports the arrival once, and only once it has landed', () => {
    const { camera } = mountCamera();
    const onDone = vi.fn();

    act(() => camera().flyTo(CENTER - 30 * DAY, ZOOM, { onDone }));
    act(() => advance(100, 4));
    expect(onDone).not.toHaveBeenCalled();

    act(() => advance(1000));
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it('abandons a flight the moment the user takes over', () => {
    const { camera } = mountCamera();

    act(() => camera().flyTo(CENTER - 30 * DAY, ZOOM));
    act(() => advance(100, 4));
    const midFlight = camera().read().center;

    act(() => camera().panBy(10));
    act(() => advance(400));

    expect(camera().isFlying()).toBe(false);
    // Stopped where it was interrupted rather than continuing to the destination
    expect(Math.abs(camera().read().center - midFlight)).toBeLessThan(30 * DAY);
  });

  it('keeps the centre on the present while following', () => {
    const { camera } = mountCamera();
    vi.spyOn(Date, 'now').mockReturnValue(CENTER + 5000);

    act(() => camera().setLive(true));
    act(() => advance(50, 2));

    expect(camera().read().center).toBe(CENTER + 5000);
  });

  it('arrives without animating when the system asks for less motion', () => {
    vi.stubGlobal('matchMedia', () => ({
      matches: true,
      addEventListener: () => {},
      removeEventListener: () => {},
    }));

    const { camera, commits } = mountCamera();
    const destination = CENTER - 30 * DAY;

    act(() => camera().flyTo(destination, ZOOM));

    expect(camera().isFlying()).toBe(false);
    expect(commits).toEqual([{ center: destination, zoom: ZOOM }]);
  });

  it('keeps a jump inside the zoom bounds it was given', () => {
    const { camera, commits } = mountCamera();

    act(() => camera().jumpTo(CENTER, 100));

    expect(commits[0].zoom).toBe(1);
  });
});
