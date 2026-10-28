import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import CompactTimeline from './CompactTimeline';
import { getSearchMarkerFitRange } from '../../lib/timeline_search';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, args) => args ? `${key} ${JSON.stringify(args)}` : key,
    i18n: { language: 'en' },
  }),
}));

const base = Date.parse('2026-10-27T08:00:00Z');
const marker = (id, time) => ({ id, time, result: { screenshot_id: id } });

beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(400);
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it('keeps loaded hits visible after the camera moves outside their overview range', () => {
  const markers = [marker('a', base), marker('b', base + 3600000)];
  const onSelectMarker = vi.fn();
  render(<CompactTimeline markers={markers} centerTime={base + 86400000} onSelectMarker={onSelectMarker} />);
  expect(screen.getAllByRole('button')).toHaveLength(2);
  const track = screen.getByRole('slider', { name: 'timeline.searchResultsTrack' });
  expect(track).toHaveAttribute('data-in-range', 'false');
  expect(track).toHaveValue('1000');
  fireEvent.click(screen.getAllByRole('button')[0]);
  expect(onSelectMarker).toHaveBeenCalledWith(markers[0]);
});

it('scrubs the padded result range and exposes its time to assistive technology', () => {
  const markers = [marker('a', base), marker('b', base + 3600000)];
  const onScrub = vi.fn();
  const view = render(<CompactTimeline markers={markers} centerTime={base} onScrub={onScrub} />);
  const track = screen.getByRole('slider');
  expect(track).toHaveAttribute('aria-valuetext');
  fireEvent.change(track, { target: { value: '750' } });
  const range = getSearchMarkerFitRange(markers);
  const time = range.from + (range.to - range.from) * 0.75;
  expect(onScrub).toHaveBeenCalledWith(time);
  view.rerender(<CompactTimeline markers={markers} centerTime={time} onScrub={onScrub} />);
  expect(track).toHaveValue('750');
});

it('selects the hovered result in a dense cluster without overlapping adjacent hit areas', () => {
  const markers = [
    marker('a', base), marker('b', base + 5000),
    marker('c', base + 55000), marker('d', base + 600000),
  ];
  const onSelectMarker = vi.fn();
  const view = render(<CompactTimeline markers={markers} centerTime={base + 300000} onSelectMarker={onSelectMarker} />);
  view.rerender(<CompactTimeline markers={markers} hoveredIds={['b']} centerTime={base + 300000} onSelectMarker={onSelectMarker} />);
  const buttons = screen.getAllByRole('button');
  expect(buttons).toHaveLength(3);
  expect(buttons[0]).toHaveAttribute('data-active', 'true');
  fireEvent.click(buttons[0]);
  expect(onSelectMarker).toHaveBeenCalledWith(markers[1]);
  for (let index = 1; index < buttons.length; index += 1) {
    const previousRight = parseFloat(buttons[index - 1].style.left) + parseFloat(buttons[index - 1].style.width);
    expect(parseFloat(buttons[index].style.left)).toBeGreaterThanOrEqual(previousRight);
  }
});
