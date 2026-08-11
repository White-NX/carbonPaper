/** Generate activity key from app name and window title. */
export function getActivityKey(appName, windowTitle) {
  return `${appName || ''}::${windowTitle || ''}`;
}

/** Unique deduplication key for an event. */
export function getEventKey(event) {
  if (!event) return null;
  if (event.id !== null && event.id !== undefined) return event.id;
  if (event.imagePath) return event.imagePath;
  return `${event.timestamp ?? 0}-${event.appName || ''}-${event.windowTitle || ''}`;
}

/** Index of first event with timestamp >= target. */
function lowerBound(events, target) {
  let low = 0;
  let high = events.length;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (events[mid].timestamp < target) low = mid + 1;
    else high = mid;
  }
  return low;
}

/** Index of first event with timestamp > target. */
function upperBound(events, target) {
  let low = 0;
  let high = events.length;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (events[mid].timestamp <= target) low = mid + 1;
    else high = mid;
  }
  return low;
}

/**
 * Merge consecutive events for the same activity into raw session segments.
 *
 * @param {Array} events Events sorted by timestamp ascending
 * @param {{tailMs?: number}} options
 */
export function buildSessions(events, { tailMs = 0 } = {}) {
  if (!events || events.length === 0) return [];

  const sessions = [];
  for (const event of events) {
    const key = getActivityKey(event.appName, event.windowTitle);
    const last = sessions[sessions.length - 1];
    if (last && last.key === key) {
      last.end = event.timestamp;
      last.count += 1;
    } else {
      sessions.push({
        key,
        appName: event.appName,
        windowTitle: event.windowTitle,
        processIcon: event.processIcon,
        category: event.category,
        start: event.timestamp,
        end: event.timestamp,
        count: 1,
        firstEvent: event,
      });
    }
  }

  for (let i = 0; i < sessions.length - 1; i += 1) {
    sessions[i].end = sessions[i + 1].start;
  }
  const tail = sessions[sessions.length - 1];
  tail.end = tail.end + Math.max(0, tailMs);

  return sessions;
}

/** Default ratio for an app to dominate a run of short activities. */
const DEFAULT_DOMINANT_RATIO = 0.65;

/** Max app slices in a mixed block. */
const MIXED_APP_SLICES = 6;

/** Recalculate block metrics. */
function refreshAppBlockStats(block) {
  const titles = new Set();
  const apps = new Set();
  let interruptions = 0;

  for (const segment of block.segments) {
    apps.add(segment.appName || '');
    if (segment.appName === block.appName) titles.add(segment.windowTitle || '');
    else interruptions += 1;
  }

  block.windowCount = titles.size;
  block.appCount = apps.size;
  block.interruptions = interruptions;
  block.switches = Math.max(0, block.segments.length - 1);
}

/** Convert a wide activity to an activity block. */
function toActivityBlock(activity) {
  return {
    kind: 'activity',
    id: `activity-${activity.key}-${activity.start}`,
    appName: activity.appName,
    windowTitle: activity.windowTitle,
    processIcon: activity.processIcon,
    category: activity.category,
    start: activity.start,
    end: activity.end,
    count: activity.count,
    firstEvent: activity.firstEvent,
    segments: [activity],
    windowCount: 1,
    switches: 0,
    interruptions: 0,
    apps: null,
    appCount: 1,
  };
}

/** Fold a run of short activities into an app or mixed block. */
function foldRun(items, dominantRatio) {
  const totals = new Map();
  let totalMs = 0;
  let count = 0;

  for (const item of items) {
    const ms = Math.max(0, item.end - item.start);
    totalMs += ms;
    count += item.count;
    const name = item.appName || '';
    let entry = totals.get(name);
    if (!entry) {
      entry = { appName: item.appName, ms: 0, processIcon: null };
      totals.set(name, entry);
    }
    entry.ms += ms;
    if (!entry.processIcon) entry.processIcon = item.processIcon;
  }

  const ranked = Array.from(totals.values()).sort(
    (a, b) => b.ms - a.ms || String(a.appName || '').localeCompare(String(b.appName || '')),
  );
  const leader = ranked[0];
  // Avoid division by zero when total duration is zero
  const ratioOf = (entry) => (totalMs > 0 ? entry.ms / totalMs : 1 / ranked.length);

  const start = items[0].start;
  const base = {
    start,
    end: items[items.length - 1].end,
    count,
    firstEvent: items[0].firstEvent,
    category: items[0].category,
    segments: items,
    switches: items.length - 1,
    appCount: ranked.length,
  };

  if (ranked.length === 1 || ratioOf(leader) >= dominantRatio) {
    const block = {
      ...base,
      kind: 'app',
      id: `app-${leader.appName || ''}-${start}`,
      appName: leader.appName,
      windowTitle: null,
      processIcon: leader.processIcon,
      apps: null,
    };
    refreshAppBlockStats(block);
    return block;
  }

  return {
    ...base,
    kind: 'mixed',
    id: `mixed-${start}`,
    appName: null,
    windowTitle: null,
    processIcon: null,
    windowCount: 0,
    interruptions: 0,
    apps: ranked.slice(0, MIXED_APP_SLICES).map((entry) => ({
      appName: entry.appName,
      processIcon: entry.processIcon,
      ratio: ratioOf(entry),
    })),
  };
}

/** Check if adjacent blocks of the same app should merge. */
function canAbsorb(left, right) {
  if (!left.appName || left.appName !== right.appName) return false;
  return left.kind === 'app' || right.kind === 'app';
}

/**
 * Collapse session activities into display blocks suitable for current zoom level.
 *
 * @param {Array} activities Output from buildSessions
 * @param {{minBlockMs?: number, dominantRatio?: number}} options
 */
export function collapseSessions(
  activities,
  { minBlockMs = 0, dominantRatio = DEFAULT_DOMINANT_RATIO } = {},
) {
  if (!activities || activities.length === 0) return [];

  // Group consecutive short activities
  const runs = [];
  for (const activity of activities) {
    const isShort = activity.end - activity.start < minBlockMs;
    const last = runs[runs.length - 1];
    if (isShort && last?.short) last.items.push(activity);
    else runs.push({ short: isShort, items: [activity] });
  }

  const blocks = runs.map((run) => (
    run.items.length > 1 ? foldRun(run.items, dominantRatio) : toActivityBlock(run.items[0])
  ));

  // Merge adjacent blocks of the same app
  const merged = [];
  for (const block of blocks) {
    const last = merged[merged.length - 1];
    if (last && canAbsorb(last, block)) {
      last.kind = 'app';
      last.end = block.end;
      last.count += block.count;
      last.segments = last.segments.concat(block.segments);
      last.windowTitle = null;
      if (!last.processIcon) last.processIcon = block.processIcon;
      refreshAppBlockStats(last);
    } else {
      merged.push(block);
    }
  }

  return merged;
}

/** Estimate median interval between adjacent screenshots. */
export function estimateSampleInterval(events) {
  if (!events || events.length < 2) return 0;
  const gaps = [];
  for (let i = 1; i < events.length; i += 1) {
    const gap = events[i].timestamp - events[i - 1].timestamp;
    if (gap > 0) gaps.push(gap);
  }
  if (gaps.length === 0) return 0;
  gaps.sort((a, b) => a - b);
  return gaps[gaps.length >> 1];
}

/** Prune events outside specified start/end window. */
export function pruneEvents(events, keepStart, keepEnd) {
  if (!events || events.length === 0) return events;
  if (events[0].timestamp >= keepStart && events[events.length - 1].timestamp <= keepEnd) {
    return events;
  }
  return events.slice(lowerBound(events, keepStart), upperBound(events, keepEnd));
}

/** Merge two sorted event arrays and remove duplicates. */
export function mergeSortedEvents(existing, incoming) {
  const left = existing || [];
  const right = incoming || [];
  if (left.length === 0) return right.slice();
  if (right.length === 0) return left.slice();

  const merged = [];
  const seen = new Set();
  let i = 0;
  let j = 0;

  const push = (event) => {
    const key = getEventKey(event);
    if (seen.has(key)) return;
    seen.add(key);
    merged.push(event);
  };

  while (i < left.length && j < right.length) {
    if (left[i].timestamp <= right[j].timestamp) push(left[i++]);
    else push(right[j++]);
  }
  while (i < left.length) push(left[i++]);
  while (j < right.length) push(right[j++]);

  return merged;
}

/**
 * Aggregate app distribution ratio per time bucket.
 *
 * @param {Array} events Sampled events
 * @param {Array} buckets Density buckets ({timestamp, count, bucketMs})
 * @param {number} maxSlices Max slices per bucket
 */
export function aggregateAppDistribution(events, buckets, maxSlices = 4) {
  if (!buckets || buckets.length === 0) return [];

  const bucketMs = buckets[0].bucketMs;
  if (!bucketMs || bucketMs <= 0) return [];

  // Group sampled events into buckets and count app occurrences
  const tally = new Map();
  for (const event of events || []) {
    const bucketStart = Math.floor(event.timestamp / bucketMs) * bucketMs;
    let apps = tally.get(bucketStart);
    if (!apps) {
      apps = new Map();
      tally.set(bucketStart, apps);
    }
    const name = event.appName || '';
    apps.set(name, (apps.get(name) || 0) + 1);
  }

  return buckets.map((bucket) => {
    const bucketStart = Math.floor(bucket.timestamp / bucketMs) * bucketMs;
    const apps = tally.get(bucketStart);
    if (!apps || apps.size === 0) {
      return { ...bucket, slices: [] };
    }

    const sampleTotal = Array.from(apps.values()).reduce((sum, n) => sum + n, 0);
    const ranked = Array.from(apps.entries())
      .map(([appName, samples]) => ({ appName, samples }))
      .sort((a, b) => b.samples - a.samples || a.appName.localeCompare(b.appName));

    const top = ranked.slice(0, maxSlices);
    const restSamples = ranked.slice(maxSlices).reduce((sum, item) => sum + item.samples, 0);

    const slices = top.map((item) => ({
      appName: item.appName,
      ratio: item.samples / sampleTotal,
      count: Math.round((item.samples / sampleTotal) * bucket.count),
    }));

    if (restSamples > 0) {
      slices.push({
        appName: null,
        ratio: restSamples / sampleTotal,
        count: Math.round((restSamples / sampleTotal) * bucket.count),
      });
    }

    return { ...bucket, slices };
  });
}
