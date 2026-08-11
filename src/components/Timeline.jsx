import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Play } from 'lucide-react';
import { getTimeline, getTimelineDensity, fetchThumbnailBatch, clearTimelineImageQueue } from '../lib/monitor_api';
import {
  aggregateAppDistribution,
  buildSessions,
  collapseSessions,
  estimateSampleInterval,
  mergeSortedEvents,
  pruneEvents,
} from '../lib/timeline_sessions';
import { accentWithAlpha } from '../lib/timeline_palette';
import OverviewBand from './timeline/OverviewBand';
import SessionBand from './timeline/SessionBand';
import DetailTrack from './timeline/DetailTrack';

const MINUTE = 60000;
const HOUR = 3600000;
const DAY = 86400000;

/** Track heights. */
const OVERVIEW_HEIGHT = 20;
const SESSION_HEIGHT = 30;
const DETAIL_HEIGHT = 58;
const AXIS_HEIGHT = 18;
const TIMELINE_HEIGHT = OVERVIEW_HEIGHT + SESSION_HEIGHT + DETAIL_HEIGHT + AXIS_HEIGHT;

/** Tier threshold boundaries for detail track. */
const FRAME_TIER_MAX_SPAN = 30 * MINUTE;
const SESSION_TIER_MAX_SPAN = 8 * HOUR;

/** Minimum pixel width target before collapsing session blocks. */
const SESSION_MIN_BLOCK_PX = 32;

/** Overview band span configuration factor. */
const OVERVIEW_SPAN_FACTOR = 10;
const OVERVIEW_MIN_SPAN = 2 * HOUR;
const OVERVIEW_MAX_SPAN = 90 * DAY;
/** Recenter threshold ratio for overview band. */
const OVERVIEW_RECENTER_RATIO = 0.3;

/** Drag slop threshold in pixels. */
const DRAG_SLOP_PX = 4;

/** Local timezone offset relative to UTC in milliseconds. */
function localBucketOffsetMs() {
  return -new Date().getTimezoneOffset() * MINUTE;
}

const TIMELINE_IMAGE_CACHE_LIMIT = 800;
const timelineImageCache = new Map();

function setTimelineImageCache(key, dataUrl) {
  if (key === null || key === undefined || !dataUrl) return;
  if (!timelineImageCache.has(key) && timelineImageCache.size >= TIMELINE_IMAGE_CACHE_LIMIT) {
    const oldestKey = timelineImageCache.keys().next().value;
    timelineImageCache.delete(oldestKey);
  }
  timelineImageCache.set(key, dataUrl);
}

function simpleDebounce(func, wait) {
  let timeout;
  return function debounced(...args) {
    clearTimeout(timeout);
    timeout = setTimeout(() => func.apply(this, args), wait);
  };
}

function simpleThrottle(func, limit) {
  let lastRun = 0;
  return function throttled(...args) {
    const now = Date.now();
    if (now - lastRun >= limit) {
      func.apply(this, args);
      lastRun = now;
    }
  };
}

/** Snap raw bucket width to standardized step. */
function snapBucketMs(raw) {
  const steps = [
    1000, 5000, 15000, 30000,
    MINUTE, 5 * MINUTE, 15 * MINUTE, 30 * MINUTE,
    HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR,
    DAY, 7 * DAY, 30 * DAY,
  ];
  return steps.find((step) => step >= raw) || steps[steps.length - 1];
}

function getTickStep(zoom) {
  const minSpacing = 120;
  const target = minSpacing / zoom;
  const steps = [
    1000, 2000, 5000, 10000, 15000, 30000,
    MINUTE, 2 * MINUTE, 5 * MINUTE, 15 * MINUTE, 30 * MINUTE,
    HOUR, 2 * HOUR, 6 * HOUR, 12 * HOUR,
    DAY, 2 * DAY, 7 * DAY, 30 * DAY, 90 * DAY, 180 * DAY,
    365 * DAY, 2 * 365 * DAY, 5 * 365 * DAY, 10 * 365 * DAY,
  ];
  return steps.find((step) => step >= target) || steps[steps.length - 1];
}

function formatTick(date, stepMs) {
  if (stepMs >= 365 * DAY) return `${date.getFullYear()}`;
  if (stepMs >= 28 * DAY) return date.toLocaleString('default', { month: 'short', year: 'numeric' });
  if (stepMs >= DAY) return date.toLocaleString('default', { month: 'short', day: 'numeric' });

  const hours = date.getHours();
  const minutes = String(date.getMinutes()).padStart(2, '0');
  const seconds = String(date.getSeconds()).padStart(2, '0');

  if (stepMs >= HOUR) return `${date.getMonth() + 1}/${date.getDate()} ${hours}:00`;
  if (stepMs >= MINUTE) return `${hours}:${minutes}`;
  return `${hours}:${minutes}:${seconds}`;
}

const Timeline = ({ onSelectEvent, onClearHighlight, jumpTimestamp, highlightedEventId, refreshKey, sqlPaused }) => {
  const { t } = useTranslation();
  const containerRef = useRef(null);

  const [width, setWidth] = useState(0);
  const [events, setEvents] = useState([]);
  const [densityBuckets, setDensityBuckets] = useState([]);
  const [overviewBuckets, setOverviewBuckets] = useState([]);
  const [imageEpoch, setImageEpoch] = useState(0);

  const [centerTime, setCenterTime] = useState(Date.now());
  const [zoom, setZoom] = useState(0.001);
  const [isFollowingNow, setIsFollowingNow] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const [overviewAnchor, setOverviewAnchor] = useState(() => Date.now());
  const [overviewInteracting, setOverviewInteracting] = useState(false);

  const MIN_ZOOM = 100 / (365 * DAY);
  const MAX_ZOOM = 20 / 1000;

  const lastMouseXRef = useRef(0);
  const isDraggingRef = useRef(false);
  const dragMovedRef = useRef(0);
  const fetchEpochRef = useRef(0);
  const densityEpochRef = useRef(0);
  const overviewEpochRef = useRef(0);
  const wheelIdleTimerRef = useRef(null);
  const pendingImageIdsRef = useRef([]);
  const batchTimerRef = useRef(null);

  const visibleSpan = width > 0 ? width / zoom : HOUR;
  const viewStart = centerTime - visibleSpan / 2;
  const viewEnd = centerTime + visibleSpan / 2;

  const tier = visibleSpan <= FRAME_TIER_MAX_SPAN
    ? 'frame'
    : visibleSpan <= SESSION_TIER_MAX_SPAN
      ? 'session'
      : 'day';

  const overviewSpan = Math.min(
    OVERVIEW_MAX_SPAN,
    Math.max(OVERVIEW_MIN_SPAN, visibleSpan * OVERVIEW_SPAN_FACTOR),
  );
  const overviewStart = overviewAnchor - overviewSpan / 2;
  const overviewEnd = overviewAnchor + overviewSpan / 2;

  // Recenter overview band anchor when viewport drifts too far
  useEffect(() => {
    if (overviewInteracting) return;
    if (Math.abs(centerTime - overviewAnchor) > overviewSpan * OVERVIEW_RECENTER_RATIO) {
      setOverviewAnchor(centerTime);
    }
  }, [centerTime, overviewAnchor, overviewSpan, overviewInteracting]);

  const timeToX = useCallback(
    (time) => width / 2 + (time - centerTime) * zoom,
    [width, centerTime, zoom],
  );

  const tickStepMs = useMemo(() => getTickStep(zoom), [zoom]);

  useEffect(() => {
    if (!containerRef.current) return undefined;
    setWidth(containerRef.current.clientWidth);
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) setWidth(entry.contentRect.width);
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, []);

  // ── Data Fetching ──────────────────────────────────────────

  const fetchEventsRaw = useCallback(async (center, currentZoom, containerWidth) => {
    if (!containerWidth) return;

    const epoch = ++fetchEpochRef.current;
    const span = containerWidth / currentZoom;
    const start = center - span;
    const end = center + span;

    try {
      const records = await getTimeline(start, end);
      if (fetchEpochRef.current !== epoch) return;

      const mapped = (records || [])
        .filter((record) => record.timestamp != null)
        .map((record) => {
          let meta = null;
          if (record?.metadata) {
            try {
              meta = typeof record.metadata === 'string' ? JSON.parse(record.metadata) : record.metadata;
            } catch {
              meta = null;
            }
          }
          return {
            id: record.id,
            timestamp: record.timestamp
              ? record.timestamp * 1000
              : (record.created_at ? new Date(record.created_at).getTime() : 0),
            imagePath: record.image_path,
            appName: record.process_name,
            windowTitle: record.window_title,
            processIcon: record.process_icon || meta?.process_icon || record.page_icon || null,
            processPath: record.process_path || meta?.process_path || null,
            category: record.category || null,
          };
        })
        .filter((event) => !Number.isNaN(event.timestamp));

      setEvents((prev) => pruneEvents(mergeSortedEvents(prev, mapped), center - span * 1.5, center + span * 1.5));
    } catch (error) {
      console.error('[Timeline] Fetch error:', error);
    }
  }, []);

  const fetchDensityRaw = useCallback(async (center, currentZoom, containerWidth) => {
    if (!containerWidth) return;

    const epoch = ++densityEpochRef.current;
    const span = containerWidth / currentZoom;
    const start = center - span;
    const end = center + span;

    // Ensure bucket width is at least one day for day tier
    const raw = (span * 2) / 220;
    const bucketMs = span > SESSION_TIER_MAX_SPAN
      ? Math.max(DAY, snapBucketMs(raw))
      : snapBucketMs(raw);

    try {
      const buckets = await getTimelineDensity(start, end, bucketMs, localBucketOffsetMs());
      if (densityEpochRef.current !== epoch) return;
      setDensityBuckets((buckets || []).map((bucket) => ({
        timestamp: bucket.timestamp * 1000,
        count: bucket.count,
        bucketMs,
      })));
    } catch (error) {
      console.error('[Timeline] Density fetch error:', error);
    }
  }, []);

  const fetchOverviewRaw = useCallback(async (start, end) => {
    const epoch = ++overviewEpochRef.current;
    const bucketMs = snapBucketMs((end - start) / 160);

    try {
      const buckets = await getTimelineDensity(start, end, bucketMs, localBucketOffsetMs());
      if (overviewEpochRef.current !== epoch) return;
      setOverviewBuckets((buckets || []).map((bucket) => ({
        timestamp: bucket.timestamp * 1000,
        count: bucket.count,
        bucketMs,
      })));
    } catch (error) {
      console.error('[Timeline] Overview fetch error:', error);
    }
  }, []);

  const fetchEventsDebounced = useMemo(() => simpleDebounce(fetchEventsRaw, 500), [fetchEventsRaw]);
  const fetchEventsThrottled = useMemo(() => simpleThrottle(fetchEventsRaw, 1000), [fetchEventsRaw]);
  const fetchDensityDebounced = useMemo(() => simpleDebounce(fetchDensityRaw, 600), [fetchDensityRaw]);
  const fetchOverviewDebounced = useMemo(() => simpleDebounce(fetchOverviewRaw, 900), [fetchOverviewRaw]);

  useEffect(() => {
    if (sqlPaused || !width) return;
    if (isFollowingNow) fetchEventsThrottled(centerTime, zoom, width);
    else fetchEventsDebounced(centerTime, zoom, width);
    fetchDensityDebounced(centerTime, zoom, width);
    fetchOverviewDebounced(overviewStart, overviewEnd);
  }, [
    centerTime, zoom, width, isFollowingNow, sqlPaused,
    overviewStart, overviewEnd,
    fetchEventsThrottled, fetchEventsDebounced, fetchDensityDebounced, fetchOverviewDebounced,
  ]);

  useEffect(() => {
    if (refreshKey === undefined) return;
    setEvents([]);
    clearTimelineImageQueue();
    setImageEpoch((prev) => prev + 1);
    fetchEventsRaw(centerTime, zoom, width);
    // Refresh only when refreshKey changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshKey]);

  useEffect(() => {
    if (sqlPaused) return undefined;
    const interval = setInterval(() => {
      if (!isFollowingNow && !isDragging) fetchEventsDebounced(centerTime, zoom, width);
    }, 5000);
    return () => clearInterval(interval);
  }, [isFollowingNow, isDragging, centerTime, zoom, width, fetchEventsDebounced, sqlPaused]);

  // ── Thumbnail Loading ──────────────────────────────────────

  const handleVisibleFramesChange = useCallback((ids) => {
    pendingImageIdsRef.current = ids;
  }, []);

  useEffect(() => {
    if (sqlPaused || tier !== 'frame') return undefined;
    if (batchTimerRef.current) clearTimeout(batchTimerRef.current);

    batchTimerRef.current = setTimeout(() => {
      const ids = pendingImageIdsRef.current;
      if (!ids || ids.length === 0) return;
      const uncached = ids.filter((id) => !timelineImageCache.has(id));
      if (uncached.length === 0) return;

      fetchThumbnailBatch(uncached)
        .then((batch) => {
          if (!batch) return;
          let added = 0;
          for (const [id, dataUrl] of Object.entries(batch)) {
            if (dataUrl) {
              setTimelineImageCache(Number(id), dataUrl);
              added += 1;
            }
          }
          if (added > 0) setImageEpoch((prev) => prev + 1);
        })
        .catch((error) => console.error('[Timeline] Batch thumbnail load error:', error));
    }, 300);

    return () => {
      if (batchTimerRef.current) clearTimeout(batchTimerRef.current);
    };
  }, [centerTime, zoom, width, events, sqlPaused, tier]);

  // ── Derived Data ───────────────────────────────────────────

  const sampleInterval = useMemo(() => estimateSampleInterval(events), [events]);
  const activities = useMemo(
    () => buildSessions(events, { tailMs: sampleInterval }),
    [events, sampleInterval],
  );
  const sessionBlocks = useMemo(
    () => collapseSessions(activities, {
      minBlockMs: zoom > 0 ? SESSION_MIN_BLOCK_PX / zoom : 0,
    }),
    [activities, zoom],
  );
  const distribution = useMemo(
    () => (tier === 'day' ? aggregateAppDistribution(events, densityBuckets, 4) : []),
    [tier, events, densityBuckets],
  );

  /** Find session block matching the highlighted screenshot event. */
  const activeSessionId = useMemo(() => {
    if (highlightedEventId === null || highlightedEventId === undefined) return null;
    const event = events.find((item) => item.id === highlightedEventId);
    if (!event) return null;
    const block = sessionBlocks.find(
      (item) => event.timestamp >= item.start && event.timestamp < item.end,
    );
    return block ? block.id : null;
  }, [events, sessionBlocks, highlightedEventId]);

  // ── Navigation ───────────────────────────────────────────────

  const resetImagePipeline = useCallback(() => {
    clearTimelineImageQueue();
    setImageEpoch((prev) => prev + 1);
  }, []);

  const seekTo = useCallback((time) => {
    setIsFollowingNow(false);
    setCenterTime(time);
  }, []);

  const seekRange = useCallback((from, to) => {
    if (!width || to <= from) return;
    setIsFollowingNow(false);
    setCenterTime((from + to) / 2);
    setZoom(Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, width / (to - from))));
    resetImagePipeline();
  }, [width, MIN_ZOOM, MAX_ZOOM, resetImagePipeline]);

  const zoomToSpan = useCallback((spanMs) => {
    if (!width) return;
    setIsFollowingNow(false);
    setCenterTime(Date.now());
    setZoom(Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, width / spanMs)));
    resetImagePipeline();
  }, [width, MIN_ZOOM, MAX_ZOOM, resetImagePipeline]);

  const jumpToNow = useCallback(() => {
    setCenterTime(Date.now());
    setZoom(MAX_ZOOM);
    setIsFollowingNow(true);
    resetImagePipeline();
  }, [MAX_ZOOM, resetImagePipeline]);

  useEffect(() => {
    if (!jumpTimestamp?.time) return;
    setIsFollowingNow(false);
    setCenterTime(jumpTimestamp.time);
    setZoom((prev) => Math.max(prev, 0.005));
    resetImagePipeline();
  }, [jumpTimestamp, resetImagePipeline]);

  useEffect(() => {
    if (!isFollowingNow) return undefined;
    let frameId;
    const tick = () => {
      setCenterTime(Date.now());
      frameId = requestAnimationFrame(tick);
    };
    tick();
    return () => {
      if (frameId) cancelAnimationFrame(frameId);
    };
  }, [isFollowingNow]);

  useEffect(() => {
    const handleKeyDown = (event) => {
      if (event.ctrlKey || event.altKey || event.metaKey) return;
      const target = event.target;
      if (target?.closest?.('input, textarea, select, [contenteditable="true"]')) return;

      switch (event.key) {
        case '1': zoomToSpan(DAY); break;
        case '2': zoomToSpan(7 * DAY); break;
        case '3': zoomToSpan(30 * DAY); break;
        case '0': jumpToNow(); break;
        default: return;
      }
      event.preventDefault();
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [zoomToSpan, jumpToNow]);

  // ── Pointer Interaction ─────────────────────────────────────

  const handleMouseDown = useCallback((event) => {
    setIsFollowingNow(false);
    setIsDragging(true);
    isDraggingRef.current = true;
    dragMovedRef.current = 0;
    lastMouseXRef.current = event.clientX;
    clearTimelineImageQueue();
  }, []);

  const handleMouseMove = useCallback((event) => {
    if (!isDraggingRef.current) return;
    const deltaX = event.clientX - lastMouseXRef.current;
    lastMouseXRef.current = event.clientX;
    dragMovedRef.current += Math.abs(deltaX);
    setCenterTime((prev) => prev - deltaX / zoom);
  }, [zoom]);

  const endDrag = useCallback(() => {
    if (!isDraggingRef.current) return;
    setIsDragging(false);
    isDraggingRef.current = false;
    setImageEpoch((prev) => prev + 1);
  }, []);

  const handleWheel = useCallback((event) => {
    try { event.preventDefault(); } catch { /* passive listener */ }
    setIsFollowingNow(false);
    clearTimelineImageQueue();

    if (wheelIdleTimerRef.current) clearTimeout(wheelIdleTimerRef.current);
    wheelIdleTimerRef.current = setTimeout(() => setImageEpoch((prev) => prev + 1), 160);

    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const cursorX = event.clientX - rect.left;
    const timeAtCursor = centerTime + (cursorX - width / 2) / zoom;

    const nextZoom = Math.max(
      MIN_ZOOM,
      Math.min(MAX_ZOOM, event.deltaY < 0 ? zoom * 1.2 : zoom / 1.2),
    );

    setZoom(nextZoom);
    setCenterTime(timeAtCursor - (cursorX - width / 2) / nextZoom);
  }, [centerTime, width, zoom, MIN_ZOOM, MAX_ZOOM]);

  const handleBackgroundClick = useCallback((event) => {
    if (dragMovedRef.current > DRAG_SLOP_PX) return;
    // Ignore background click if clicking an interactive item
    if (event.target?.closest?.('[data-timeline-item]')) return;
    setIsFollowingNow(false);
    onClearHighlight?.();
  }, [onClearHighlight]);

  /** Filter out clicks caused by drag mouseup. */
  const guardedSelect = useCallback((handler) => (payload) => {
    if (dragMovedRef.current > DRAG_SLOP_PX) return;
    handler(payload);
  }, []);

  const handleSelectSession = useMemo(
    () => guardedSelect((session) => {
      if (session.firstEvent) onSelectEvent?.(session.firstEvent);
    }),
    [guardedSelect, onSelectEvent],
  );

  const handleZoomToSession = useCallback((session) => {
    const padding = Math.max(MINUTE, (session.end - session.start) * 0.15);
    seekRange(session.start - padding, session.end + padding);
  }, [seekRange]);

  const handleSelectFrame = useMemo(
    () => guardedSelect((event) => onSelectEvent?.(event)),
    [guardedSelect, onSelectEvent],
  );

  // ── Axis Ticks ──────────────────────────────────────────────

  const ticks = useMemo(() => {
    if (!width) return [];
    const result = [];
    let current = Math.floor(viewStart / tickStepMs) * tickStepMs;
    let guard = 0;

    while (current < viewEnd && guard < 100) {
      const x = timeToX(current);
      if (x > -60 && x < width + 60) {
        result.push({ time: current, x, label: formatTick(new Date(current), tickStepMs) });
      }
      current += tickStepMs;
      guard += 1;
    }
    return result;
  }, [width, viewStart, viewEnd, tickStepMs, timeToX]);

  const quickZooms = [
    { key: 'today', span: DAY, label: t('timeline.today'), title: t('timeline.zoomToday'), shortcut: '1' },
    { key: 'week', span: 7 * DAY, label: t('timeline.week'), title: t('timeline.zoomWeek'), shortcut: '2' },
    { key: 'month', span: 30 * DAY, label: t('timeline.month'), title: t('timeline.zoomMonth'), shortcut: '3' },
  ];

  return (
    <div
      ref={containerRef}
      className="relative w-full shrink-0 select-none overflow-hidden border-b border-ide-border"
      style={{ height: TIMELINE_HEIGHT, backgroundColor: 'var(--ide-timeline-bg)' }}
      data-keep-selection="true"
    >
      <OverviewBand
        width={width}
        height={OVERVIEW_HEIGHT}
        buckets={overviewBuckets}
        rangeStart={overviewStart}
        rangeEnd={overviewEnd}
        viewStart={viewStart}
        viewEnd={viewEnd}
        onSeek={seekTo}
        onSeekRange={seekRange}
        onInteractingChange={setOverviewInteracting}
      />

      <div
        className={`relative ${isDragging ? 'cursor-grabbing' : 'cursor-grab'}`}
        style={{ height: SESSION_HEIGHT + DETAIL_HEIGHT + AXIS_HEIGHT }}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={endDrag}
        onMouseLeave={endDrag}
        onWheel={handleWheel}
        onClick={handleBackgroundClick}
      >
        <SessionBand
          blocks={sessionBlocks}
          timeToX={timeToX}
          width={width}
          height={SESSION_HEIGHT}
          activeId={activeSessionId}
          onSelect={handleSelectSession}
          onZoomTo={handleZoomToSession}
        />

        <DetailTrack
          tier={tier}
          events={events}
          buckets={densityBuckets}
          distribution={distribution}
          timeToX={timeToX}
          width={width}
          height={DETAIL_HEIGHT}
          highlightedEventId={highlightedEventId}
          imageCache={timelineImageCache}
          imageEpoch={imageEpoch}
          onSelectEvent={handleSelectFrame}
          onSeek={seekTo}
          onVisibleFramesChange={handleVisibleFramesChange}
        />

        <div className="relative" style={{ height: AXIS_HEIGHT }}>
          {ticks.map((tick) => (
            <div
              key={tick.time}
              className="pointer-events-none absolute top-0 flex -translate-x-1/2 flex-col items-center"
              style={{ left: tick.x }}
            >
              <span className="h-1 w-px bg-ide-border" />
              <span className="mt-0.5 whitespace-nowrap font-mono text-[10px] leading-none text-ide-muted">
                {tick.label}
              </span>
            </div>
          ))}
        </div>

        <div
          className="pointer-events-none absolute inset-y-0 left-1/2 z-30 w-px"
          style={{ backgroundColor: accentWithAlpha(0.75) }}
        />
      </div>

      <div className="absolute bottom-1.5 right-2 z-40 flex items-center gap-1">
        {quickZooms.map((item) => (
          <button
            key={item.key}
            type="button"
            className="rounded border border-ide-border bg-ide-panel px-2 py-0.5 text-[11px] text-ide-text shadow-sm transition-colors hover:bg-ide-active"
            onClick={(event) => {
              event.stopPropagation();
              zoomToSpan(item.span);
            }}
            title={`${item.title} (${item.shortcut})`}
          >
            {item.label}
          </button>
        ))}
        <button
          type="button"
          className={`rounded-full border p-1 shadow transition-colors ${
            isFollowingNow
              ? 'border-ide-accent bg-ide-accent text-white'
              : 'border-ide-border bg-ide-panel text-ide-text hover:bg-ide-active'
          }`}
          onClick={(event) => {
            event.stopPropagation();
            jumpToNow();
          }}
          title={`${t('timeline.jumpToNow')} (0)`}
        >
          <Play size={14} fill={isFollowingNow ? 'currentColor' : 'none'} className={isFollowingNow ? '' : 'ml-0.5'} />
        </button>
      </div>
    </div>
  );
};

export default Timeline;
