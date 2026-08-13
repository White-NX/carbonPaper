import { describe, expect, it } from 'vitest';
import {
  SEARCH_FIT_MIN_SPAN_MS,
  buildSearchTimelineMarkers,
  clusterSearchTimelineMarkers,
  getSearchMarkerFitRange,
  searchResultMarkerId,
} from './timeline_search';

describe('timeline search markers', () => {
  it('builds chronological markers and deduplicates screenshots', () => {
    const markers = buildSearchTimelineMarkers([
      { screenshot_id: 7, timestamp: 1_800 },
      { screenshot_id: 7, timestamp: 1_801 },
      { screenshot_id: 4, screenshot_created_at: '1970-01-01T00:20:00Z' },
      { screenshot_id: 9, timestamp: 0 },
    ]);

    expect(markers).toMatchObject([
      { id: 'screenshot:4', screenshotId: 4, time: 1_200_000 },
      { id: 'screenshot:7', screenshotId: 7, time: 1_800_000 },
    ]);
  });

  it('uses a stable path identity when a screenshot id is unavailable', () => {
    expect(searchResultMarkerId({ image_path: 'memory://abc' })).toBe('path:memory://abc');
  });

  it('keeps single and nearby hits in a readable minimum range', () => {
    const range = getSearchMarkerFitRange([{ id: 'a', time: 10_000 }]);
    expect(range.to - range.from).toBe(SEARCH_FIT_MIN_SPAN_MS);
    expect((range.from + range.to) / 2).toBe(10_000);
  });

  it('pads the full span when hits are far apart', () => {
    const range = getSearchMarkerFitRange([
      { id: 'a', time: 0 },
      { id: 'b', time: 10_000_000 },
    ]);
    expect(range.from).toBeLessThan(0);
    expect(range.to).toBeGreaterThan(10_000_000);
  });

  it('clusters dense markers and promotes a cluster containing a hovered hit', () => {
    const clusters = clusterSearchTimelineMarkers(
      [
        { id: 'a', time: 10 },
        { id: 'b', time: 14 },
        { id: 'c', time: 40 },
      ],
      (time) => time,
      100,
      ['b'],
      7,
    );

    expect(clusters).toHaveLength(2);
    expect(clusters[0]).toMatchObject({ count: 2, active: true, from: 10, to: 14 });
    expect(clusters[0].representative.id).toBe('b');
    expect(clusters[1]).toMatchObject({ count: 1, active: false, from: 40, to: 40 });
  });
});
