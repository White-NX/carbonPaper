import { describe, expect, it } from 'vitest';
import {
  aggregateAppDistribution,
  buildSessions,
  collapseSessions,
  estimateSampleInterval,
  getActivityKey,
  mergeSortedEvents,
  pruneEvents,
} from './timeline_sessions';

const event = (id, timestamp, appName, windowTitle = '') => ({
  id,
  timestamp,
  appName,
  windowTitle,
});

describe('getActivityKey', () => {
  it('treats different windows of one app as different activities', () => {
    expect(getActivityKey('msedge', 'GitHub')).not.toBe(getActivityKey('msedge', 'comma.ai'));
  });

  it('tolerates missing fields', () => {
    expect(getActivityKey(undefined, undefined)).toBe('::');
  });
});

describe('buildSessions', () => {
  it('merges consecutive events of the same activity', () => {
    const sessions = buildSessions([
      event(1, 1000, 'msedge', 'GitHub'),
      event(2, 2000, 'msedge', 'GitHub'),
      event(3, 3000, 'Code', 'Timeline.jsx'),
    ]);

    expect(sessions).toHaveLength(2);
    expect(sessions[0].count).toBe(2);
    expect(sessions[1].appName).toBe('Code');
  });

  it('extends each session up to the start of the next one', () => {
    const sessions = buildSessions([
      event(1, 1000, 'msedge'),
      event(2, 2000, 'msedge'),
      event(3, 9000, 'Code'),
    ]);

    // Last event at 2000 extends to next session start at 9000
    expect(sessions[0].end).toBe(9000);
  });

  it('gives the trailing session a tail so it is not zero-width', () => {
    const sessions = buildSessions([event(1, 5000, 'Code')], { tailMs: 2000 });

    expect(sessions[0].start).toBe(5000);
    expect(sessions[0].end).toBe(7000);
  });

  it('returns nothing for an empty input', () => {
    expect(buildSessions([])).toEqual([]);
    expect(buildSessions(null)).toEqual([]);
  });

  it('splits a session when the same app changes window', () => {
    const sessions = buildSessions([
      event(1, 1000, 'msedge', 'GitHub'),
      event(2, 2000, 'msedge', 'comma.ai'),
    ]);

    expect(sessions).toHaveLength(2);
  });
});

describe('collapseSessions', () => {
  /** Helper to create contiguous activity segments. */
  const activities = (specs) => {
    let cursor = 0;
    return specs.map(([appName, windowTitle, duration]) => {
      const activity = {
        key: getActivityKey(appName, windowTitle),
        appName,
        windowTitle,
        processIcon: null,
        category: null,
        start: cursor,
        end: cursor + duration,
        count: 1,
        firstEvent: { id: cursor, timestamp: cursor, appName, windowTitle },
      };
      cursor += duration;
      return activity;
    });
  };

  it('leaves wide activities alone', () => {
    const blocks = collapseSessions(
      activities([['Code', 'a.js', 5000], ['msedge', 'GitHub', 5000]]),
      { minBlockMs: 1000 },
    );

    expect(blocks.map((block) => block.kind)).toEqual(['activity', 'activity']);
    expect(blocks[0].windowTitle).toBe('a.js');
  });

  it('folds a run of title flicker into one app block', () => {
    const blocks = collapseSessions(
      activities([
        ['wt', 'claude ~/a', 200],
        ['wt', 'claude ~/b', 300],
        ['wt', 'claude ~/c', 250],
        ['wt', 'claude ~/d', 150],
      ]),
      { minBlockMs: 1000 },
    );

    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe('app');
    expect(blocks[0].appName).toBe('wt');
    expect(blocks[0].windowCount).toBe(4);
    expect(blocks[0].switches).toBe(3);
    expect(blocks[0].interruptions).toBe(0);
    // Block boundaries remain unchanged
    expect(blocks[0].start).toBe(0);
    expect(blocks[0].end).toBe(900);
  });

  it('keeps a dominant app and counts the rest as interruptions', () => {
    const blocks = collapseSessions(
      activities([
        ['wt', 'claude ~/a', 400],
        ['msedge', 'docs', 100],
        ['wt', 'claude ~/b', 400],
      ]),
      { minBlockMs: 1000 },
    );

    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe('app');
    expect(blocks[0].appName).toBe('wt');
    expect(blocks[0].interruptions).toBe(1);
    // Interrupted segments are retained
    expect(blocks[0].segments).toHaveLength(3);
  });

  it('calls a run mixed when nobody dominates', () => {
    const blocks = collapseSessions(
      activities([
        ['wt', 'a', 400],
        ['msedge', 'b', 350],
        ['Code', 'c', 300],
      ]),
      { minBlockMs: 1000 },
    );

    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe('mixed');
    expect(blocks[0].appName).toBeNull();
    expect(blocks[0].appCount).toBe(3);
    expect(blocks[0].switches).toBe(2);
    // Sorted by duration descending
    expect(blocks[0].apps.map((app) => app.appName)).toEqual(['wt', 'msedge', 'Code']);
    expect(blocks[0].apps[0].ratio).toBeCloseTo(400 / 1050);
  });

  it('lists at most six apps but reports the true total', () => {
    const blocks = collapseSessions(
      activities([
        ['a', '', 400], ['b', '', 350], ['c', '', 300], ['d', '', 250],
        ['e', '', 200], ['f', '', 150], ['g', '', 100], ['h', '', 50],
      ]),
      { minBlockMs: 1000 },
    );

    expect(blocks[0].kind).toBe('mixed');
    // Store up to max app slices
    expect(blocks[0].apps).toHaveLength(6);
    expect(blocks[0].appCount).toBe(8);
  });

  it('does not let a patch of flicker split one app into three blocks', () => {
    const blocks = collapseSessions(
      activities([
        ['wt', 'claude ~/a', 5000],
        ['wt', 'claude ~/b', 200],
        ['wt', 'claude ~/c', 200],
        ['wt', 'claude ~/d', 5000],
      ]),
      { minBlockMs: 1000 },
    );

    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe('app');
    expect(blocks[0].end).toBe(10400);
    expect(blocks[0].windowCount).toBe(4);
  });

  it('keeps two wide activities of one app apart', () => {
    const blocks = collapseSessions(
      activities([['wt', 'claude ~/a', 5000], ['wt', 'claude ~/b', 5000]]),
      { minBlockMs: 1000 },
    );

    // Wide activities remain separate
    expect(blocks).toHaveLength(2);
    expect(blocks.every((block) => block.kind === 'activity')).toBe(true);
  });

  it('leaves an isolated short activity visible', () => {
    const blocks = collapseSessions(
      activities([['Code', 'a.js', 5000], ['msedge', 'docs', 100], ['Code', 'b.js', 5000]]),
      { minBlockMs: 1000 },
    );

    expect(blocks.map((block) => block.appName)).toEqual(['Code', 'msedge', 'Code']);
  });

  it('passes everything through when no threshold is given', () => {
    const input = activities([['wt', 'a', 10], ['wt', 'b', 10]]);
    const blocks = collapseSessions(input);

    expect(blocks).toHaveLength(2);
    expect(blocks.every((block) => block.kind === 'activity')).toBe(true);
  });

  it('survives zero-length activities without dividing by zero', () => {
    const blocks = collapseSessions(
      activities([['a', '', 0], ['b', '', 0], ['c', '', 0]]),
      { minBlockMs: 1000 },
    );

    expect(blocks).toHaveLength(1);
    expect(blocks[0].kind).toBe('mixed');
    expect(blocks[0].apps.every((app) => Number.isFinite(app.ratio))).toBe(true);
  });

  it('gives every block a distinct id', () => {
    const blocks = collapseSessions(
      activities([
        ['Code', 'a.js', 5000],
        ['wt', 'x', 100], ['msedge', 'y', 100], ['Code', 'z', 100],
        ['Code', 'b.js', 5000],
      ]),
      { minBlockMs: 1000 },
    );

    const ids = blocks.map((block) => block.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('returns nothing for an empty input', () => {
    expect(collapseSessions([])).toEqual([]);
    expect(collapseSessions(null)).toEqual([]);
  });

  it('turns a half-day of real-looking churn into a handful of blocks', () => {
    const MINUTE = 60_000;
    const specs = [];

    // 2 hours terminal with frequent title updates
    for (let i = 0; i < 80; i += 1) specs.push(['wt', `claude step ${i}`, 1.5 * MINUTE]);
    // 20 minutes docs reading
    specs.push(['msedge', 'docs', 20 * MINUTE]);
    // 40 minutes switching between terminal and browser
    for (let i = 0; i < 20; i += 1) {
      specs.push([i % 2 === 0 ? 'wt' : 'msedge', `tab ${i}`, 2 * MINUTE]);
    }
    // 1 hour game
    specs.push(['game', 'Elden Ring', 60 * MINUTE]);

    // 7.5 minutes min block threshold
    const blocks = collapseSessions(activities(specs), { minBlockMs: 7.5 * MINUTE });

    expect(specs).toHaveLength(102);
    expect(blocks.map((block) => [block.kind, block.appName])).toEqual([
      ['app', 'wt'],
      ['activity', 'msedge'],
      ['mixed', null],
      ['activity', 'game'],
    ]);
    // Switches and window counts preserved
    expect(blocks[0].switches).toBe(79);
    expect(blocks[0].windowCount).toBe(80);
    expect(blocks[2].switches).toBe(19);
  });
});

describe('estimateSampleInterval', () => {
  it('takes the median so idle gaps do not skew it', () => {
    const events = [
      event(1, 0, 'a'),
      event(2, 1000, 'a'),
      event(3, 2000, 'a'),
      event(4, 3_600_000, 'a'), // Idle gap
    ];

    expect(estimateSampleInterval(events)).toBe(1000);
  });

  it('returns zero when there is nothing to measure', () => {
    expect(estimateSampleInterval([])).toBe(0);
    expect(estimateSampleInterval([event(1, 0, 'a')])).toBe(0);
  });
});

describe('mergeSortedEvents', () => {
  it('merges two sorted runs keeping order', () => {
    const merged = mergeSortedEvents(
      [event(1, 1000, 'a'), event(3, 3000, 'a')],
      [event(2, 2000, 'a'), event(4, 4000, 'a')],
    );

    expect(merged.map((item) => item.timestamp)).toEqual([1000, 2000, 3000, 4000]);
  });

  it('drops duplicates by id', () => {
    const merged = mergeSortedEvents(
      [event(1, 1000, 'a'), event(2, 2000, 'a')],
      [event(2, 2000, 'a'), event(3, 3000, 'a')],
    );

    expect(merged.map((item) => item.id)).toEqual([1, 2, 3]);
  });

  it('handles either side being empty', () => {
    const one = [event(1, 1000, 'a')];
    expect(mergeSortedEvents([], one)).toHaveLength(1);
    expect(mergeSortedEvents(one, [])).toHaveLength(1);
  });
});

describe('pruneEvents', () => {
  const events = [
    event(1, 1000, 'a'),
    event(2, 2000, 'a'),
    event(3, 3000, 'a'),
    event(4, 4000, 'a'),
  ];

  it('keeps only what falls inside the window', () => {
    expect(pruneEvents(events, 2000, 3000).map((item) => item.id)).toEqual([2, 3]);
  });

  it('returns the same array when nothing needs dropping', () => {
    expect(pruneEvents(events, 0, 9000)).toBe(events);
  });

  it('can prune everything away', () => {
    expect(pruneEvents(events, 8000, 9000)).toEqual([]);
  });
});

describe('aggregateAppDistribution', () => {
  const buckets = [
    { timestamp: 0, count: 100, bucketMs: 1000 },
    { timestamp: 1000, count: 40, bucketMs: 1000 },
  ];

  it('splits a bucket by the app mix of its sampled events', () => {
    const result = aggregateAppDistribution(
      [
        event(1, 100, 'msedge'),
        event(2, 200, 'msedge'),
        event(3, 300, 'msedge'),
        event(4, 400, 'Code'),
        event(5, 1100, 'Code'),
      ],
      buckets,
    );

    const [first, second] = result;
    expect(first.slices).toHaveLength(2);
    expect(first.slices[0].appName).toBe('msedge');
    expect(first.slices[0].ratio).toBeCloseTo(0.75);
    // Slices calculated from sampling ratio
    expect(first.slices[0].count).toBe(75);
    expect(second.slices[0].appName).toBe('Code');
  });

  it('folds everything past maxSlices into one unnamed slice', () => {
    const result = aggregateAppDistribution(
      [
        event(1, 10, 'a'), event(2, 20, 'a'),
        event(3, 30, 'b'),
        event(4, 40, 'c'),
        event(5, 50, 'd'),
        event(6, 60, 'e'),
      ],
      [{ timestamp: 0, count: 60, bucketMs: 1000 }],
      2,
    );

    const slices = result[0].slices;
    expect(slices).toHaveLength(3);
    expect(slices[slices.length - 1].appName).toBeNull();
    expect(slices.reduce((sum, slice) => sum + slice.ratio, 0)).toBeCloseTo(1);
  });

  it('leaves buckets with no samples empty rather than guessing', () => {
    const result = aggregateAppDistribution([], buckets);
    expect(result[0].slices).toEqual([]);
  });

  it('returns nothing without buckets', () => {
    expect(aggregateAppDistribution([event(1, 0, 'a')], [])).toEqual([]);
  });
});
