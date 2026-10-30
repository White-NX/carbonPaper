import { describe, expect, it } from 'vitest';
import { nearestTimelineEvent, timelineEventFromRecord, timelinePreviewPosition, timelineWheelAction } from './timeline_interaction';

describe('timeline snapshot coordinates', () => {
  it('preserves epoch zero and converts storage seconds explicitly', () => {
    expect(timelineEventFromRecord({ id: 1, timestamp: 0 }).timestamp).toBe(0);
    expect(timelineEventFromRecord({ id: 2, timestamp: 1767225600 }).timestamp).toBe(1767225600000);
    expect(timelineEventFromRecord(null)).toBeNull();
  });

  it('finds the nearest sorted display event and prefers the earlier event on a tie', () => {
    const events = [100, 200, 500].map((timestamp) => ({ timestamp }));
    expect(nearestTimelineEvent(events, 349)).toBe(events[1]);
    expect(nearestTimelineEvent(events, 350)).toBe(events[1]);
    expect(nearestTimelineEvent(events, 351)).toBe(events[2]);
    expect(nearestTimelineEvent(events, 0)).toBe(events[0]);
    expect(nearestTimelineEvent(events, 1000)).toBe(events[2]);
    expect(nearestTimelineEvent([], 200)).toBeNull();
  });
});

describe('timeline wheel gestures', () => {
  const wheel = (props) => ({ deltaX: 0, deltaY: 0, deltaMode: 0, ...props });
  it('pans with horizontal and vertical precise touchpad input', () => {
    expect(timelineWheelAction(wheel({ deltaX: 24 }), 800, null, 0)).toEqual({ kind: 'pan', deltaX: -24 });
    expect(timelineWheelAction(wheel({ deltaY: -5.5 }), 800, null, 0)).toEqual({ kind: 'pan', deltaX: 5.5 });
  });
  it('keeps a fast swipe and its momentum in pan mode', () => {
    const previous = { kind: 'pan', at: 100 };
    expect(timelineWheelAction(wheel({ deltaY: 120 }), 800, previous, 150).kind).toBe('pan');
    expect(timelineWheelAction(wheel({ deltaY: 120 }), 800, previous, 400).kind).toBe('zoom');
  });
  it('uses the magnitude of a pinch without discrete zoom steps', () => {
    const small = timelineWheelAction(wheel({ deltaY: -2, ctrlKey: true }), 800, null, 0);
    const large = timelineWheelAction(wheel({ deltaY: -8, ctrlKey: true }), 800, null, 0);
    expect(small.immediate).toBe(true);
    expect(small.factor).toBeCloseTo(Math.exp(0.02));
    expect(large.factor).toBeGreaterThan(small.factor);
    expect(small.factor).toBeLessThan(1.2);
  });
  it('normalizes line-based mouse wheels and supports Shift horizontal panning', () => {
    expect(timelineWheelAction(wheel({ deltaY: 3, deltaMode: 1 }), 800, null, 0).factor).toBeCloseTo(1 / 1.2);
    expect(timelineWheelAction(wheel({ deltaY: 120, shiftKey: true }), 800, null, 0)).toEqual({ kind: 'pan', deltaX: -120 });
    expect(timelineWheelAction(wheel({}), 800, null, 0)).toBeNull();
  });
});

it('keeps the full-image card inside narrow viewports and beside the pointer', () => {
  for (const [viewportWidth, viewportHeight, clientX] of [[1366, 768, 1320], [320, 240, 160]]) {
    const position = timelinePreviewPosition({ clientX, clientY: 110, trackTop: 100, trackBottom: 172 }, viewportWidth, viewportHeight);
    expect(position.left).toBeGreaterThanOrEqual(12);
    expect(position.left + position.width).toBeLessThanOrEqual(viewportWidth - 12);
    expect(position.top).toBeGreaterThanOrEqual(12);
    expect(position.top + position.imageHeight + 66).toBeLessThanOrEqual(viewportHeight - 12);
  }
});
