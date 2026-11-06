/** Generate activity key from app name and window title. */
export function getActivityKey(appName, windowTitle) {
  return `${appName || ''}::${windowTitle || ''}`;
}

/** Screenshots alone cannot establish continuity across a long observation gap. */
export const MAX_INFERRED_SESSION_GAP_MS = 5 * 60_000;

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
 * @param {{tailMs?: number, maxGapMs?: number}} options
 */
export function buildSessions(
  events,
  { tailMs = 0, maxGapMs = MAX_INFERRED_SESSION_GAP_MS } = {},
) {
  if (!events || events.length === 0) return [];

  const gapLimit = Number.isFinite(maxGapMs) ? Math.max(0, maxGapMs) : MAX_INFERRED_SESSION_GAP_MS;
  const tail = Number.isFinite(tailMs) ? Math.min(gapLimit, Math.max(0, tailMs)) : 0;

  const sessions = [];
  for (const event of events) {
    const key = getActivityKey(event.appName, event.windowTitle);
    const last = sessions[sessions.length - 1];
    const continuous = last && event.timestamp - last.end <= gapLimit;
    if (continuous && last.key === key) {
      last.end = event.timestamp;
      last.count += 1;
    } else {
      if (last) last.end = continuous ? event.timestamp : last.end + tail;
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

  sessions[sessions.length - 1].end += tail;

  return sessions;
}

/** Use observed intervals after tracking starts; retain bounded estimates for legacy history. */
export function buildTimelineSessions(events, usage) {
  // Until the tracking boundary is known, only screenshot points are factual.
  if (usage === undefined) return [];
  const cutover = usage?.tracking_started_ms;
  const legacyEvents = Number.isFinite(cutover)
    ? (events || []).filter((event) => event.timestamp < cutover) : events;
  const legacy = buildSessions(legacyEvents, { tailMs: estimateSampleInterval(legacyEvents) });
  if (!Number.isFinite(cutover)) return legacy;
  for (const session of legacy) session.end = Math.min(session.end, cutover);

  const normalizeApp = (name) => String(name || '').toLowerCase().replace(/\.exe$/, '');
  const frames = events || [];
  const icons = new Map();
  for (const frame of frames) {
    if (frame.processIcon) icons.set(normalizeApp(frame.appName), frame.processIcon);
  }
  const observed = [];
  for (const interval of usage.intervals || []) {
    if (!Number.isFinite(interval.start_ms) || !Number.isFinite(interval.end_ms)
      || interval.end_ms <= interval.start_ms) continue;
    const app = normalizeApp(interval.process_name);
    // Screenshot wire times have one-second precision; keep the opening frame
    // without letting its rounded timestamp extend an observed usage interval.
    const shots = frames.slice(lowerBound(frames, interval.start_ms - 1000), lowerBound(frames, interval.end_ms))
      .filter((event) => normalizeApp(event.appName) === app);
    const parts = shots.length ? buildSessions(
      shots.map((event) => ({ ...event, appName: interval.process_name,
        timestamp: Math.max(interval.start_ms, event.timestamp) })),
      { maxGapMs: Number.MAX_SAFE_INTEGER },
    ) : [{
      key: getActivityKey(interval.process_name, interval.window_title),
      appName: interval.process_name,
      windowTitle: interval.window_title,
      processIcon: icons.get(app),
      category: null, count: 0, firstEvent: null,
      start: interval.start_ms, end: interval.end_ms,
    }];
    parts[0].start = interval.start_ms;
    parts[parts.length - 1].end = interval.end_ms;
    for (const part of parts) {
      part.continuityId = interval.run_id;
      part.usageId = interval.id;
      part.nativeWindowTitle = interval.window_title;
      part.processPath = interval.process_path;
      const last = observed[observed.length - 1];
      const titleMatchesNative = (title) => {
        const page = String(title || '').trim().toLowerCase();
        const native = String(interval.window_title || '').trim().toLowerCase();
        return page && (native === page || native.startsWith(`${page} - `)
          || native.startsWith(`${page} — `));
      };
      // A change in input/media basis does not interrupt the activity. Browser
      // screenshot titles omit the native browser suffix; only reconcile a
      // screenshot-free part when the native title itself stayed unchanged.
      const sameTitle = last && (last.key === part.key
        || (last.nativeWindowTitle === part.nativeWindowTitle
          && ((!last.firstEvent && titleMatchesNative(part.windowTitle))
            || (!part.firstEvent && titleMatchesNative(last.windowTitle)))));
      if (last && last.end === part.start && last.continuityId === part.continuityId
        && normalizeApp(last.appName) === app
        && String(last.processPath || '').toLowerCase() === String(part.processPath || '').toLowerCase()
        && sameTitle) {
        last.end = part.end;
        last.count += part.count;
        if (!last.processIcon) last.processIcon = part.processIcon;
        if (!last.firstEvent && part.firstEvent) {
          last.firstEvent = part.firstEvent;
          last.windowTitle = part.windowTitle;
          last.key = part.key;
          last.category = part.category;
        }
      } else observed.push(part);
    }
  }
  return [...legacy, ...observed].sort((left, right) => left.start - right.start);
}

/**
 * Identify a display block by where it starts rather than by what it holds.
 *
 * A folded block always starts where its first child starts, so the same id
 * survives folding and unfolding. React then keeps the element across zoom
 * levels instead of tearing it down and building a new one, which is both what
 * stops icons from flashing and what makes an expand transition possible at all.
 */
function blockId(start) {
  return `blk-${start}`;
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
    id: blockId(activity.start),
    appName: activity.appName,
    windowTitle: activity.windowTitle,
    processIcon: activity.processIcon,
    category: activity.category,
    start: activity.start,
    end: activity.end,
    count: activity.count,
    firstEvent: activity.firstEvent,
    continuityId: activity.continuityId,
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
    continuityId: items[0].continuityId,
    category: items[0].category,
    segments: items,
    switches: items.length - 1,
    appCount: ranked.length,
  };

  if (ranked.length === 1 || ratioOf(leader) >= dominantRatio) {
    const block = {
      ...base,
      kind: 'app',
      id: blockId(start),
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
    id: blockId(start),
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
  if (left.end !== right.start || left.continuityId !== right.continuityId
    || !left.appName || left.appName !== right.appName) return false;
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
    const previous = last?.items[last.items.length - 1];
    if (isShort && last?.short && previous.end === activity.start
      && previous.continuityId === activity.continuityId) {
      last.items.push(activity);
    } else runs.push({ short: isShort, items: [activity] });
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

  // Two events sharing a timestamp can leave two blocks claiming the same start.
  // A duplicate React key would make one of them disappear, so break ties here
  // rather than weakening the identity every other block relies on.
  const usedIds = new Set();
  for (const block of merged) {
    const base = block.id;
    let candidate = base;
    let suffix = 1;
    while (usedIds.has(candidate)) {
      candidate = `${base}#${suffix}`;
      suffix += 1;
    }
    usedIds.add(candidate);
    block.id = candidate;
  }

  return merged;
}

/** Estimate a bounded tail from the median interval between adjacent screenshots. */
export function estimateSampleInterval(events) {
  if (!events || events.length < 2) return 0;
  const gaps = [];
  for (let i = 1; i < events.length; i += 1) {
    const gap = events[i].timestamp - events[i - 1].timestamp;
    if (gap > 0) gaps.push(gap);
  }
  if (gaps.length === 0) return 0;
  gaps.sort((a, b) => a - b);
  return Math.min(gaps[gaps.length >> 1], MAX_INFERRED_SESSION_GAP_MS);
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
