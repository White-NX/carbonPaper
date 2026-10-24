import React from 'react';
import { fireEvent, render } from '@testing-library/react';
import { expect, it, vi } from 'vitest';
import DetailTrack from './DetailTrack';

it('prioritizes the highlighted and on-screen frames without changing their render order', () => {
  const events = [-50, 20, 100, 180, 270, 350].map((timestamp, index) => ({ id: index + 1, timestamp }));
  const onVisibleFramesChange = vi.fn();
  const onSelectEvent = vi.fn();
  const { container } = render(<DetailTrack
    tier="frame" events={events} timeToX={(timestamp) => timestamp}
    width={300} height={58} overscanPx={100} highlightedEventId={6}
    imageCache={new Map()} onVisibleFramesChange={onVisibleFramesChange} onSelectEvent={onSelectEvent}
  />);
  expect(onVisibleFramesChange).toHaveBeenLastCalledWith([6, 4, 3, 5, 2, 1]);
  const frames = container.querySelectorAll('[data-timeline-item="frame"]');
  expect(frames).toHaveLength(6);
  fireEvent.click(frames[2]);
  expect(onSelectEvent).toHaveBeenCalledWith(events[2]);
});
