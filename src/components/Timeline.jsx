import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronUp, Focus, Play } from 'lucide-react';
import { Button } from './ui/Button';
import { getTimeline, getTimelineDetails, getTimelineDensity, getTimelineNearest } from '../lib/monitor_api';
import {
  buildSessions,
  collapseSessions,
  estimateSampleInterval,
  mergeSortedEvents,
  pruneEvents,
} from '../lib/timeline_sessions';
import {
  SPAN_LADDER_MS,
  createThrottle,
  isRangeCovered,
  resolveZoomLevel,
  snapSpanUpMs,
} from '../lib/timeline_viewport';
import { RENDER_OVERSCAN_RATIO } from '../lib/timeline_camera';
import useTimelineCamera from '../hooks/useTimelineCamera';
import SessionBand from './timeline/SessionBand';
import DensityBackdrop from './timeline/DensityBackdrop';
import TimelineHoverPreview from './timeline/TimelineHoverPreview';
import SearchMarkerLayer from './timeline/SearchMarkerLayer';
import CompactTimeline from './timeline/CompactTimeline';
import { getSearchMarkerFitRange } from '../lib/timeline_search';
import { createTimelineRequestQueue } from '../lib/timeline_requests';
import { createTimelineBatchQueue } from '../lib/timeline_batch_queue';
import { nearestTimelineEvent, timelineEventFromRecord, timelineWheelAction } from '../lib/timeline_interaction';

const MINUTE = 60000;
const HOUR = 3600000;
const DAY = 86400000;

/** Track heights. */
const SESSION_HEIGHT = 44;
const AXIS_HEIGHT = 28;
const TIMELINE_HEIGHT = SESSION_HEIGHT + AXIS_HEIGHT;

/** Use day-sized density buckets when viewing more than eight hours. */
const SESSION_TIER_MAX_SPAN = 8 * HOUR;

/** Minimum pixel width target before collapsing session blocks. */
const SESSION_MIN_BLOCK_PX = 32;

/**
 * Records one timeline fetch may return. Mirrors the default applied by
 * `storage_get_timeline` in `src-tauri/src/commands/storage.rs`, which samples
 * evenly across the requested range once the range holds more rows than this.
 * Stated here as well because the sampling width it implies is what decides
 * whether two fetches may be merged.
 */
const TIMELINE_SAMPLE_LIMIT = 500;

/** How much wider than the viewport each kind of fetch reaches. */
const EVENT_PREFETCH_FACTOR = 2.5;
const DENSITY_PREFETCH_FACTOR = 2.5;

/** Refetch once the viewport comes this close to the edge of loaded data. */
const REFETCH_MARGIN_RATIO = 0.35;

/** Shortest gap between two fetches of the same kind while the user is dragging. */
const EVENT_FETCH_INTERVAL_MS = 300;
const DENSITY_FETCH_INTERVAL_MS = 350;

/** Density buckets aimed for across one screen width. */
const DENSITY_BUCKETS_PER_SCREEN = 120;

/** How often loaded data is read again so newly captured screenshots appear. */
const FOLLOW_REFRESH_MS = 1000;
const IDLE_REFRESH_MS = 5000;

/** Drag slop threshold in pixels. */
const DRAG_SLOP_PX = 4;

/** Zoom bounds in pixels per millisecond: a year across, down to 20 px a second. */
const MIN_ZOOM = 100 / (365 * DAY);
const MAX_ZOOM = 20 / 1000;

/** Local timezone offset relative to UTC in milliseconds. */
function localBucketOffsetMs() {
  return -new Date().getTimezoneOffset() * MINUTE;
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

const Timeline = ({
  onSelectEvent,
  jumpTimestamp,
  highlightedEventId,
  refreshKey,
  sqlPaused: windowSqlPaused = false,
  searchState,
  onSelectSearchResult,
  active = true,
  compact = false,
  onToggleCompact,
}) => {
  const { t } = useTranslation();
  const containerRef = useRef(null);
  const trackRef = useRef(null);
  const hoverPreviewRef = useRef(null);
  const hoverCursorRef = useRef(null);
  const playheadRef = useRef(null);
  const playheadAxisRef = useRef(null);
  const selectionTimeRef = useRef(null);
  // Compact search draws the already-loaded result markers without reading screenshot history.
  const sqlPaused = windowSqlPaused || !active || compact;

  const [width, setWidth] = useState(0);
  const [events, setEvents] = useState([]);
  const [rangePending, setRangePending] = useState(false);
  const [densityBuckets, setDensityBuckets] = useState([]);
  const [selectedSnapshot, setSelectedSnapshot] = useState(null);
  const [selectionError, setSelectionError] = useState(false);

  const [centerTime, setCenterTime] = useState(Date.now());
  const [zoom, setZoom] = useState(0.001);
  const [isFollowingNow, setIsFollowingNow] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const gestureRef = useRef(null);
  const dragMovedRef = useRef(0);
  const densityEpochRef = useRef(0);
  const lastWheelRef = useRef(null);
  const previewBusyRef = useRef(false);
  const selectionPausedRef = useRef(sqlPaused);
  const selectCallbackRef = useRef(onSelectEvent);
  selectionPausedRef.current = sqlPaused;
  selectCallbackRef.current = onSelectEvent;
  const searchFitKeyRef = useRef(null);
  const searchFitSuspendedRef = useRef(false);
  const jumpTokenRef = useRef(null);

  /** Ranges already asked for, so a pan only refetches near their edges. */
  const requestedEventsRef = useRef(null);
  const requestedDensityRef = useRef(null);
  /** Sampling width behind the events currently held, used to decide merge or replace. */
  const loadedSampleMsRef = useRef(null);
  /**
   * Live viewport, written on every camera frame.
   *
   * Timers read the view from here rather than through their own dependencies,
   * because depending on it directly rebuilt them on every frame of a movement,
   * which is why the periodic refresh never fired during a drag.
   */
  const liveViewRef = useRef({ center: 0, span: 0 });
  const zoomLevelRef = useRef(null);
  const syncLoadersRef = useRef(null);
  const detailsCacheRef = useRef(new Map());
  const eventsRef = useRef(events);
  eventsRef.current = events;
  const pipelinePausedRef = useRef(true);
  pipelinePausedRef.current = Boolean(sqlPaused) || rangePending;

  /** Elements the camera moves, and the axis it repositions label by label. */
  const sessionStageRef = useRef(null);
  const densityStageRef = useRef(null);
  const markerStageRef = useRef(null);
  const axisRef = useRef(null);
  const stages = useMemo(
    () => [sessionStageRef, densityStageRef, markerStageRef],
    [],
  );

  const visibleSpan = width > 0 ? width / zoom : HOUR;
  const viewStart = centerTime - visibleSpan / 2;
  const viewEnd = centerTime + visibleSpan / 2;
  const overscanPx = width * RENDER_OVERSCAN_RATIO;

  const selectedEvent = searchState && selectedSnapshot ? selectedSnapshot : highlightedEventId != null
    ? events.find((event) => event.id === highlightedEventId)
      || (selectedSnapshot?.id === highlightedEventId ? selectedSnapshot : null)
    : highlightedEventId === undefined ? selectedSnapshot : null;
  const selectedTime = selectedEvent?.timestamp ?? (highlightedEventId != null ? jumpTimestamp?.time : null) ?? null;
  selectionTimeRef.current = selectedTime;

  const detailsQueue = useMemo(() => createTimelineBatchQueue(getTimelineDetails, {
    batchSize: 12,
    isLoaded: (id) => detailsCacheRef.current.has(id),
    onResult: (records, ids) => {
      const current = new Set(eventsRef.current.map((event) => event.id));
      const requested = new Set(ids);
      const accepted = [];
      for (const record of records || []) {
        if (!current.has(record.id) || !requested.has(record.id)) continue;
        detailsCacheRef.current.set(record.id, {
          appName: record.process_name,
          windowTitle: record.window_title,
          processIcon: record.process_icon || null,
          processPath: record.process_path || null,
        });
        accepted.push(record.id);
      }
      if (accepted.length) setEvents((prev) => prev.map((event) => ({
        ...event, ...detailsCacheRef.current.get(event.id),
      })));
      return accepted;
    },
    onError: (error) => console.error('[Timeline] Detail load error:', error),
  }), []);

  const syncLoaders = useCallback(() => {
    const paused = pipelinePausedRef.current || previewBusyRef.current;
    const currentIds = eventsRef.current.map((event) => event.id);
    detailsQueue.setIds(currentIds, { paused });
  }, [detailsQueue]);

  const handlePreviewBusyChange = useCallback((busy) => {
    if (previewBusyRef.current === busy) return;
    previewBusyRef.current = busy;
    syncLoadersRef.current?.();
  }, []);

  useEffect(() => {
    syncLoadersRef.current = syncLoaders;
    syncLoaders();
    return () => {
      syncLoadersRef.current = null;
      detailsQueue.setPaused(true);
      detailsQueue.reset();
    };
  }, [syncLoaders, detailsQueue]);

  useEffect(() => { syncLoaders(); }, [events, sqlPaused, rangePending, syncLoaders]);

  const timeToX = useCallback(
    (time) => width / 2 + (time - centerTime) * zoom,
    [width, centerTime, zoom],
  );

  const tickStepMs = useMemo(() => getTickStep(zoom), [zoom]);

  useEffect(() => {
    if (!active || !containerRef.current) return undefined;
    const measuredWidth = containerRef.current.clientWidth;
    if (measuredWidth > 0) setWidth(measuredWidth);
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        if (entry.contentRect.width > 0) setWidth(entry.contentRect.width);
      }
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [active, compact]);

  // ── Camera ─────────────────────────────────────────────────

  /**
   * Adopt the viewport the camera has drifted to as the one the bands are drawn
   * for.
   *
   * Everything downstream still reads `centerTime` and `zoom`; the only change
   * is that they now move a few times a second instead of once per frame.
   */
  const handleCameraCommit = useCallback((view) => {
    setCenterTime(view.center);
    setZoom(view.zoom);
  }, []);

  /**
   * Called after every transform write, both during movement and after a redraw.
   *
   * It keeps the live viewport within reach of the timers, and moves the axis
   * label by label. The axis is deliberately left outside the transform: it
   * carries text, and a scaled stage would stretch it.
   */
  const handleCameraFrame = useCallback((frame) => {
    liveViewRef.current = { center: frame.center, span: frame.span };

    const trackWidth = containerRef.current?.clientWidth || 0;
    const frameZoom = frame.zoom ?? trackWidth / frame.span;
    const selectedX = selectionTimeRef.current == null ? trackWidth / 2
      : trackWidth / 2 + (selectionTimeRef.current - frame.center) * frameZoom;
    for (const marker of [playheadRef.current, playheadAxisRef.current]) {
      if (!marker) continue;
      marker.style.left = `${selectedX}px`;
      marker.style.visibility = selectedX < 0 || selectedX > trackWidth ? 'hidden' : '';
    }

    const node = axisRef.current;
    if (!node) return;

    for (const child of node.children) {
      const base = Number(child.dataset.x);
      if (!Number.isFinite(base)) continue;
      const shift = (frame.scale - 1) * base + frame.translate;
      child.style.transform = `translate3d(${shift}px, 0, 0) translateX(-50%)`;
    }
  }, []);

  const camera = useTimelineCamera({
    width,
    center: centerTime,
    zoom,
    minZoom: MIN_ZOOM,
    maxZoom: MAX_ZOOM,
    stages,
    onCommit: handleCameraCommit,
    onFrame: handleCameraFrame,
  });

  useEffect(() => {
    if (active && !compact && !sqlPaused) return;
    const view = camera.read();
    camera.jumpTo(view.center, view.zoom);
    const pointerId = gestureRef.current?.id;
    try {
      if (pointerId != null && trackRef.current?.hasPointerCapture(pointerId)) trackRef.current.releasePointerCapture(pointerId);
    } catch { /* The browser may already have cancelled capture. */ }
    gestureRef.current = null;
    lastWheelRef.current = null;
    if (hoverCursorRef.current) hoverCursorRef.current.style.opacity = '0';
    setIsDragging(false);
  }, [active, compact, sqlPaused, camera]);

  // ── Data Fetching ──────────────────────────────────────────

  const eventRequests = useMemo(() => createTimelineRequestQueue(
    ({ start, end }, signal) => getTimeline(start, end, TIMELINE_SAMPLE_LIMIT, { signal, includeDetails: false }),
    { onBusyChange: (pending) => {
      if (syncLoadersRef.current) setRangePending(pending);
    }, onResult: (records, { start, end, sampleMs }) => {
      const mapped = (records || [])
        .filter((record) => record.timestamp != null)
        .map((record) => ({ ...timelineEventFromRecord(record), ...detailsCacheRef.current.get(record.id) }))
        .filter((event) => !Number.isNaN(event.timestamp));

      // Two fetches taken at different sampling widths describe the same hours at
      // different fidelity. Merging them would leave the session band showing a
      // dense island inside a sparse surround, so that one zoom level looks
      // different depending on the route the user took to reach it. Only samples
      // of matching width are merged; a change of width replaces outright.
      const sameFidelity = loadedSampleMsRef.current === sampleMs;
      loadedSampleMsRef.current = sampleMs;

      setEvents((prev) => {
        const next = sameFidelity ? pruneEvents(mergeSortedEvents(prev, mapped), start, end) : mapped;
        const live = new Set(next.map((event) => event.id));
        for (const id of detailsCacheRef.current.keys()) {
          if (!live.has(id)) detailsCacheRef.current.delete(id);
        }
        return next;
      });
    }, onError: (error, range) => {
      console.error('[Timeline] Fetch error:', error);
      // Forget the request so the range guard tries again rather than believing
      // this range is covered. A newer request has already replaced it.
      const requested = requestedEventsRef.current;
      if (requested?.start === range.start && requested?.end === range.end
          && requested?.sampleMs === range.sampleMs) requestedEventsRef.current = null;
    } },
  ), []);

  const fetchEventsRange = useCallback((start, end, sampleMs, refresh) => {
    eventRequests.request({ start, end, sampleMs }, { refresh });
  }, [eventRequests]);

  const fetchDensityRange = useCallback(async (start, end, bucketMs) => {
    const epoch = ++densityEpochRef.current;

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
      if (densityEpochRef.current === epoch) requestedDensityRef.current = null;
    }
  }, []);

  const requestEvents = useMemo(
    () => createThrottle(fetchEventsRange, EVENT_FETCH_INTERVAL_MS),
    [fetchEventsRange],
  );
  const requestDensity = useMemo(
    () => createThrottle(fetchDensityRange, DENSITY_FETCH_INTERVAL_MS),
    [fetchDensityRange],
  );

  /**
   * Fetch screenshots only once the viewport is running out of loaded data.
   *
   * Panning used to reset a debounce timer on every frame, so nothing arrived
   * until the pointer was released. Deciding on coverage instead lets a drag keep
   * loading while still costing a few requests rather than one per frame, because
   * each fetch reaches well past both edges of the screen.
   */
  const ensureEvents = useCallback((center, span, force = false, refresh = false) => {
    if (!span) return;

    const requestSpan = span * EVENT_PREFETCH_FACTOR;
    const sampleMs = snapSpanUpMs(requestSpan / TIMELINE_SAMPLE_LIMIT);
    const requested = requestedEventsRef.current;

    const covered = requested?.sampleMs === sampleMs
      && isRangeCovered(
        requested,
        center - span / 2,
        center + span / 2,
        span * REFETCH_MARGIN_RATIO,
      );
    if (covered && !force) return;

    const start = center - requestSpan / 2;
    const end = center + requestSpan / 2;
    requestedEventsRef.current = { start, end, sampleMs };
    requestEvents(start, end, sampleMs, refresh);
  }, [requestEvents]);

  const ensureDensity = useCallback((center, span, force = false) => {
    if (!span) return;

    const requestSpan = span * DENSITY_PREFETCH_FACTOR;
    const rawBucket = span / DENSITY_BUCKETS_PER_SCREEN;
    // Ensure bucket width is at least one day for day tier
    const bucketMs = span > SESSION_TIER_MAX_SPAN
      ? Math.max(DAY, snapSpanUpMs(rawBucket))
      : snapSpanUpMs(rawBucket);
    const requested = requestedDensityRef.current;

    const covered = requested?.bucketMs === bucketMs
      && isRangeCovered(
        requested,
        center - span / 2,
        center + span / 2,
        span * REFETCH_MARGIN_RATIO,
      );
    if (covered && !force) return;

    const start = center - requestSpan / 2;
    const end = center + requestSpan / 2;
    requestedDensityRef.current = { start, end, bucketMs };
    requestDensity(start, end, bucketMs);
  }, [requestDensity]);

  // A flight asks for its destination once when it sets off, so nothing is
  // requested for the viewports it passes over on the way. Each of those is on
  // screen for a couple of frames and would never arrive in time to be seen.
  useEffect(() => {
    if (sqlPaused || !width || camera.isFlying()) return;
    ensureEvents(centerTime, visibleSpan);
    ensureDensity(centerTime, visibleSpan);
  }, [centerTime, visibleSpan, width, sqlPaused, camera, ensureEvents, ensureDensity]);

  useEffect(() => {
    if (refreshKey === undefined || !width) return;
    setEvents([]);
    detailsCacheRef.current.clear();
    detailsQueue.reset();
    requestedEventsRef.current = null;
    requestedDensityRef.current = null;
    loadedSampleMsRef.current = null;
    requestEvents.cancel();
    eventRequests.cancel();
    if (sqlPaused) return;
    ensureEvents(centerTime, visibleSpan, true);
    ensureDensity(centerTime, visibleSpan, true);
    // Refresh only when refreshKey changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshKey]);

  // Read the loaded range again on a timer so newly captured screenshots appear.
  // The interval deliberately reads the viewport from a ref: depending on it
  // directly rebuilt the timer on every pan frame, which is why it never fired
  // during a drag.
  useEffect(() => {
    if (sqlPaused || !width) return undefined;

    const period = isFollowingNow ? FOLLOW_REFRESH_MS : IDLE_REFRESH_MS;
    const interval = setInterval(() => {
      const view = liveViewRef.current;
      ensureEvents(view.center, view.span, true, true);
      ensureDensity(view.center, view.span, true);
    }, period);

    return () => clearInterval(interval);
  }, [sqlPaused, width, isFollowingNow, ensureEvents, ensureDensity]);

  useEffect(() => () => {
    requestEvents.cancel();
    eventRequests.cancel();
    // Effect replay must request this range again after cancelling its read.
    requestedEventsRef.current = null;
    requestDensity.cancel();
  }, [requestEvents, requestDensity, eventRequests]);

  useEffect(() => {
    if (sqlPaused) {
      requestEvents.cancel();
      eventRequests.cancel();
      requestedEventsRef.current = null;
      requestDensity.cancel();
      requestedDensityRef.current = null;
      densityEpochRef.current += 1;
    }
  }, [sqlPaused, requestEvents, requestDensity, eventRequests]);

  // ── Derived Data ───────────────────────────────────────────

  const sampleInterval = useMemo(() => estimateSampleInterval(events), [events]);
  const activities = useMemo(
    () => buildSessions(events, { tailMs: sampleInterval }),
    [events, sampleInterval],
  );
  /**
   * Fold threshold for the session band, snapped so it holds still while the user
   * scrolls inside one zoom level.
   *
   * Derived straight from `zoom` it drifts with every pixel of scroll, so blocks
   * flip between folded and expanded one at a time at moments nobody can predict,
   * and every block is rebuilt on every frame of a zoom. Snapped, the band changes
   * only at a handful of boundaries, and `collapseSessions` runs only there.
   */
  const fold = useMemo(() => {
    if (!width || visibleSpan <= 0) return { level: -1, minBlockMs: 0 };
    const level = resolveZoomLevel(visibleSpan, zoomLevelRef.current);
    zoomLevelRef.current = level;
    return { level, minBlockMs: (SPAN_LADDER_MS[level] * SESSION_MIN_BLOCK_PX) / width };
  }, [visibleSpan, width]);

  const sessionBlocks = useMemo(
    () => collapseSessions(activities, { minBlockMs: fold.minBlockMs }),
    [activities, fold.minBlockMs],
  );

  /** Find session block matching the highlighted screenshot event. */
  const activeSessionId = useMemo(() => {
    if (selectedTime == null) return null;
    const block = sessionBlocks.find(
      (item) => selectedTime >= item.start && selectedTime < item.end,
    );
    return block ? block.id : null;
  }, [selectedTime, sessionBlocks]);

  // ── Navigation ───────────────────────────────────────────────

  const hideHoverPreview = useCallback(() => {
    hoverPreviewRef.current?.hide();
    if (hoverCursorRef.current) hoverCursorRef.current.style.opacity = '0';
  }, []);

  const suspendSearchFit = useCallback(() => {
    if (searchState?.markers?.length) searchFitSuspendedRef.current = true;
  }, [searchState]);

  /**
   * Send the camera to a viewport, and start loading the destination while it is
   * still on its way.
   *
   * Every deliberate move goes through here: the quick zoom buttons, the search
   * fit and a double click on a session. Continuous
   * input does not, because a path only makes sense between two places the user
   * has actually chosen.
   */
  const flyTo = useCallback((nextCenter, nextZoom, options) => {
    const target = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, nextZoom));
    camera.flyTo(nextCenter, target, options);

    if (!sqlPaused && width) {
      const span = width / target;
      ensureEvents(nextCenter, span, true);
      ensureDensity(nextCenter, span, true);
    }
  }, [camera, ensureDensity, ensureEvents, sqlPaused, width]);

  /**
   * Continuous drag of the compact search viewport.
   *
   * Unlike the main track this one redraws on every move rather than riding the
   * transform, because the rectangle the user has hold of is drawn from the
   * committed viewport. Letting it drift would leave the very handle being
   * dragged trailing behind the cursor.
   */
  const scrubTo = useCallback((time) => {
    suspendSearchFit();
    setIsFollowingNow(false);
    camera.jumpTo(time, camera.read().zoom);
  }, [camera, suspendSearchFit]);

  const seekRange = useCallback((from, to) => {
    if (!width || to <= from) return;
    suspendSearchFit();
    setIsFollowingNow(false);
    flyTo((from + to) / 2, width / (to - from));
    hideHoverPreview();
  }, [width, flyTo, hideHoverPreview, suspendSearchFit]);

  const fitSearchMarkers = useCallback(() => {
    const range = getSearchMarkerFitRange(searchState?.markers);
    if (!range || !width) return;
    setIsFollowingNow(false);
    flyTo((range.from + range.to) / 2, width / (range.to - range.from));
    hideHoverPreview();
  }, [searchState?.markers, width, flyTo, hideHoverPreview]);

  useEffect(() => {
    if (!active || !width || containerRef.current?.clientWidth !== width) return;
    if (!searchState?.fitKey || !searchState.markers?.length) {
      searchFitKeyRef.current = null;
      searchFitSuspendedRef.current = false;
      return;
    }
    const isNewSearch = searchFitKeyRef.current !== searchState.fitKey;
    if (isNewSearch) {
      searchFitKeyRef.current = searchState.fitKey;
      searchFitSuspendedRef.current = false;
    }

    if (isNewSearch && !searchFitSuspendedRef.current) fitSearchMarkers();
  }, [active, searchState?.fitKey, searchState?.markers, width, fitSearchMarkers]);

  const zoomToSpan = useCallback((spanMs) => {
    if (!width) return;
    suspendSearchFit();
    setIsFollowingNow(false);
    flyTo(Date.now(), width / spanMs);
    hideHoverPreview();
  }, [width, flyTo, hideHoverPreview, suspendSearchFit]);

  const jumpToNow = useCallback(() => {
    suspendSearchFit();
    setIsFollowingNow(false);
    // Following starts once the camera has landed, so the flight itself is not
    // fighting a centre that keeps moving forward underneath it.
    flyTo(Date.now(), MAX_ZOOM, { onDone: () => setIsFollowingNow(true) });
    hideHoverPreview();
  }, [flyTo, hideHoverPreview, suspendSearchFit]);

  // Guarded by the token rather than by the dependency list, because `flyTo`
  // changes identity whenever the track is resized and must not replay a jump.
  useEffect(() => {
    if (!active || !width || !jumpTimestamp?.time || jumpTokenRef.current === jumpTimestamp) return;
    jumpTokenRef.current = jumpTimestamp;
    setIsFollowingNow(false);
    flyTo(jumpTimestamp.time, Math.max(camera.read().zoom, 0.005));
    hideHoverPreview();
  }, [active, width, jumpTimestamp, camera, flyTo, hideHoverPreview]);

  useEffect(() => {
    camera.setLive(active && !compact && !sqlPaused && isFollowingNow);
  }, [active, compact, sqlPaused, camera, isFollowingNow]);

  useEffect(() => {
    if (!active || compact || sqlPaused) return undefined;
    const handleKeyDown = (event) => {
      if (event.defaultPrevented || event.ctrlKey || event.altKey || event.metaKey) return;
      const target = event.target;
      if (target?.closest?.('input, textarea, select, [contenteditable="true"], [role="dialog"], [role="alertdialog"], [inert]')) return;

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
  }, [active, compact, sqlPaused, zoomToSpan, jumpToNow]);

  // ── Pointer Interaction ─────────────────────────────────────

  const selectSnapshot = useCallback((event) => {
    if (!event || selectionPausedRef.current) return;
    setSelectedSnapshot(event);
    setSelectionError(false);
    selectCallbackRef.current?.(event);
  }, []);

  const selectionRequests = useMemo(() => createTimelineRequestQueue(
    ({ start }, signal) => getTimelineNearest(start, { signal }),
    {
      onResult: (record) => selectSnapshot(timelineEventFromRecord(record)),
      onError: () => { if (!selectionPausedRef.current) setSelectionError(true); },
    },
  ), [selectSnapshot]);

  useEffect(() => {
    if (sqlPaused) selectionRequests.cancel();
    return () => selectionRequests.cancel();
  }, [sqlPaused, selectionRequests]);

  useEffect(() => {
    selectionRequests.cancel();
    setSelectedSnapshot(null);
    setSelectionError(false);
  }, [refreshKey, selectionRequests]);

  const pointerTime = useCallback((clientX) => {
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || !width) return null;
    const view = camera.read();
    return view.center + (Math.max(0, Math.min(width, clientX - rect.left)) - width / 2) / view.zoom;
  }, [camera, width]);

  const handlePointerDown = useCallback((event) => {
    if (event.button !== 0 || sqlPaused
      || event.target?.closest?.('[data-timeline-tool]')) return;
    selectionRequests.cancel();
    dragMovedRef.current = 0;
    gestureRef.current = { id: event.pointerId, startX: event.clientX, startY: event.clientY, lastX: event.clientX, moved: false };
    // Preserve a marker's ordinary click target; capture it once it becomes a drag.
    if (!event.target?.closest?.('[data-timeline-item="search-marker"]')) {
      try { event.currentTarget.setPointerCapture(event.pointerId); } catch { /* No capture in detached test surfaces. */ }
    }
    hideHoverPreview();
  }, [sqlPaused, selectionRequests, hideHoverPreview]);

  const handlePointerMove = useCallback((event) => {
    const gesture = gestureRef.current;
    if (gesture) {
      if (gesture.id !== event.pointerId) return;
      dragMovedRef.current = Math.max(dragMovedRef.current,
        Math.hypot(event.clientX - gesture.startX, event.clientY - gesture.startY));
      if (!gesture.moved && dragMovedRef.current <= DRAG_SLOP_PX) return;
      if (!gesture.moved) {
        gesture.moved = true;
        try { trackRef.current?.setPointerCapture(event.pointerId); } catch { /* Detached test surface. */ }
        suspendSearchFit();
        setIsFollowingNow(false);
        setIsDragging(true);
      }
      camera.panBy(event.clientX - gesture.lastX);
      gesture.lastX = event.clientX;
      return;
    }
    if (sqlPaused || event.pointerType === 'touch'
      || event.target?.closest?.('[data-timeline-tool]')) {
      hideHoverPreview();
      return;
    }
    const rect = containerRef.current?.getBoundingClientRect();
    const time = pointerTime(event.clientX);
    if (!rect || time == null) return;
    if (hoverCursorRef.current) {
      hoverCursorRef.current.style.left = `${event.clientX - rect.left}px`;
      hoverCursorRef.current.style.opacity = '1';
    }
    hoverPreviewRef.current?.show({
      time, event: nearestTimelineEvent(eventsRef.current, time),
      clientX: event.clientX, clientY: event.clientY, trackTop: rect.top, trackBottom: rect.bottom,
    });
  }, [camera, pointerTime, hideHoverPreview, sqlPaused, suspendSearchFit]);

  const endDrag = useCallback((event) => {
    if (event && gestureRef.current?.id !== event.pointerId) return;
    gestureRef.current = null;
    setIsDragging(false);
    try {
      if (event?.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    } catch { /* Capture may already have been released by the browser. */ }
  }, []);

  useEffect(() => {
    const blur = () => { dragMovedRef.current = Infinity; endDrag(); hideHoverPreview(); };
    window.addEventListener('blur', blur);
    return () => window.removeEventListener('blur', blur);
  }, [endDrag, hideHoverPreview]);

  const handleWheel = useCallback((event) => {
    const now = performance.now();
    const action = timelineWheelAction(event, width, lastWheelRef.current, now);
    if (!action) return;
    // Native non-passive listener also suppresses Chromium's Ctrl+wheel page zoom.
    event.preventDefault();
    lastWheelRef.current = { kind: action.kind, at: now };
    suspendSearchFit();
    setIsFollowingNow(false);
    hideHoverPreview();
    selectionRequests.cancel();
    if (action.kind === 'pan') camera.panBy(action.deltaX);
    else {
      const rect = containerRef.current?.getBoundingClientRect();
      if (rect) camera.zoomAt(Math.max(0, Math.min(width, event.clientX - rect.left)), action.factor, { immediate: action.immediate });
    }
  }, [camera, width, suspendSearchFit, hideHoverPreview, selectionRequests]);

  useEffect(() => {
    const track = trackRef.current;
    if (!active || compact || !track) return undefined;
    track.addEventListener('wheel', handleWheel, { passive: false });
    return () => track.removeEventListener('wheel', handleWheel);
  }, [active, compact, handleWheel]);

  const handleTimelineClick = useCallback((event) => {
    if (sqlPaused || event.button !== 0 || event.detail > 1 || dragMovedRef.current > DRAG_SLOP_PX
      || event.target?.closest?.('[data-timeline-tool], [data-timeline-item="search-marker"]')) return;
    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect || event.clientX < rect.left || event.clientX > rect.right
      || event.clientY < rect.top || event.clientY > rect.bottom) return;
    const time = pointerTime(event.clientX);
    if (time == null) return;
    suspendSearchFit();
    setIsFollowingNow(false);
    setSelectionError(false);
    const resolved = hoverPreviewRef.current?.read(time);
    if (resolved) selectSnapshot(resolved);
    else selectionRequests.request({ start: time, end: time });
    hideHoverPreview();
  }, [pointerTime, hideHoverPreview, selectSnapshot, selectionRequests, sqlPaused, suspendSearchFit]);

  const handleZoomToSession = useCallback((session) => {
    if (dragMovedRef.current > DRAG_SLOP_PX) return;
    const padding = Math.max(MINUTE, (session.end - session.start) * 0.15);
    seekRange(session.start - padding, session.end + padding);
  }, [seekRange]);

  const handleDoubleClick = useCallback((event) => {
    if (event.target?.closest?.('[data-timeline-tool], [data-timeline-item="search-marker"]')) return;
    const time = pointerTime(event.clientX);
    const session = sessionBlocks.find((block) => time >= block.start && time < block.end);
    if (session) handleZoomToSession(session);
  }, [handleZoomToSession, pointerTime, sessionBlocks]);

  // ── Axis Ticks ──────────────────────────────────────────────

  // Generated past both edges by the same margin the bands use, because the
  // camera slides the axis around between redraws and a short tick list would
  // leave a bare strip trailing behind the movement.
  const ticks = useMemo(() => {
    if (!width) return [];

    const margin = overscanPx + 60;
    const from = viewStart - margin / zoom;
    const to = viewEnd + margin / zoom;

    const result = [];
    let current = Math.floor(from / tickStepMs) * tickStepMs;
    let guard = 0;

    while (current < to && guard < 200) {
      const x = timeToX(current);
      if (x > -margin && x < width + margin) {
        result.push({ time: current, x, label: formatTick(new Date(current), tickStepMs) });
      }
      current += tickStepMs;
      guard += 1;
    }
    return result;
  }, [width, overscanPx, zoom, viewStart, viewEnd, tickStepMs, timeToX]);

  const quickZooms = [
    { key: 'today', span: DAY, label: t('timeline.today'), title: t('timeline.zoomToday'), shortcut: '1' },
    { key: 'week', span: 7 * DAY, label: t('timeline.week'), title: t('timeline.zoomWeek'), shortcut: '2' },
    { key: 'month', span: 30 * DAY, label: t('timeline.month'), title: t('timeline.zoomMonth'), shortcut: '3' },
  ];

  if (compact) {
    return (
      <div
        ref={containerRef}
        role="region"
        aria-label={t('timeline.label')}
        hidden={!active}
        className={`tl-container relative w-full shrink-0 select-none border-b border-ide-border bg-ide-panel ${active ? '' : 'hidden'}`}
        data-keep-selection="true"
      >
        <CompactTimeline
          markers={searchState?.markers}
          hoveredIds={searchState?.hoveredIds}
          centerTime={centerTime}
          onScrub={scrubTo}
          onSelectMarker={(marker) => {
            scrubTo(marker.time);
            if (marker.result) onSelectSearchResult?.(marker.result);
          }}
          onExpand={onToggleCompact}
        />
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      role="region"
      aria-label={t('timeline.label')}
      hidden={!active}
      className={`tl-container relative w-full shrink-0 select-none overflow-hidden border-b border-ide-border ${active ? '' : 'hidden'}`}
      style={{ height: TIMELINE_HEIGHT, backgroundColor: 'var(--ide-timeline-bg)' }}
      data-keep-selection="true"
    >
      <div
        ref={trackRef}
        data-testid="timeline-track"
        className={`relative ${isDragging ? 'cursor-grabbing' : 'cursor-grab'}`}
        style={{ height: TIMELINE_HEIGHT, touchAction: 'none' }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={endDrag}
        onPointerCancel={(event) => { dragMovedRef.current = Infinity; endDrag(event); hideHoverPreview(); }}
        onLostPointerCapture={endDrag}
        onPointerLeave={hideHoverPreview}
        onClick={handleTimelineClick}
        onDoubleClick={handleDoubleClick}
      >
        <div className="relative overflow-hidden" style={{ height: SESSION_HEIGHT }}>
          <DensityBackdrop buckets={densityBuckets} timeToX={timeToX} width={width}
            height={SESSION_HEIGHT} overscanPx={overscanPx} stageRef={densityStageRef} />
          <SessionBand
            blocks={sessionBlocks}
            timeToX={timeToX}
            width={width}
            height={SESSION_HEIGHT}
            overscanPx={overscanPx}
            foldLevel={fold.level}
            stageRef={sessionStageRef}
            activeId={activeSessionId}
            embedded
            showTooltips={false}
            onZoomTo={handleZoomToSession}
          />
          <SearchMarkerLayer
            markers={searchState?.markers || []}
            hoveredIds={searchState?.hoveredIds || []}
            timeToX={timeToX}
            width={width}
            height={SESSION_HEIGHT}
            overscanPx={overscanPx}
            stageRef={markerStageRef}
            onSelectMarker={(marker) => {
              if (!marker?.result) return;
              setSelectedSnapshot({ id: marker.result.screenshot_id, timestamp: marker.time });
              onSelectSearchResult?.(marker.result);
            }}
          />
          <div ref={playheadRef} data-testid="timeline-playhead" className="tl-playhead" style={{ left: timeToX(selectedTime ?? centerTime) }}>
            <span className="tl-playhead-head" />
          </div>
          <div ref={hoverCursorRef} className="tl-hover-cursor" />
          {selectionError && <span role="alert" className="pointer-events-none absolute right-2 top-2 z-40 rounded border border-ide-border bg-ide-panel px-2 py-1 text-xs text-ide-muted">{t('timeline.selectionUnavailable')}</span>}
        </div>

        <div className="flex items-stretch border-t border-ide-border" style={{ height: AXIS_HEIGHT }}>
          <div className="relative min-w-0 flex-1 overflow-hidden" data-testid="timeline-axis">
            <div ref={axisRef} className="absolute inset-y-0 left-0" style={{ width }}>
              {ticks.map((tick) => (
                <div
                  key={tick.time}
                  data-x={tick.x}
                  className="pointer-events-none absolute top-0 flex -translate-x-1/2 flex-col items-center"
                  style={{ left: tick.x }}
                >
                  <span className="h-1 w-px bg-ide-border" />
                  <span className="mt-1 whitespace-nowrap font-mono text-[10px] leading-none text-ide-muted">
                    {tick.label}
                  </span>
                </div>
              ))}
            </div>
            <div ref={playheadAxisRef} className="tl-axis-playhead" style={{ left: timeToX(selectedTime ?? centerTime) }}>
              <time className="tl-playhead-time">{new Date(selectedTime ?? centerTime).toLocaleTimeString(undefined, { hour12: false })}</time>
            </div>
          </div>
          <div className="tl-toolbar" data-timeline-tool>
            {onToggleCompact && (
              <Button
                variant="secondary"
                size="xs"
                icon={ChevronUp}
                className="h-[22px] rounded py-0 text-[11px]"
                aria-expanded
                aria-label={t('timeline.collapse')}
                data-tauri-drag-region="false"
                onClick={(event) => {
                  event.stopPropagation();
                  onToggleCompact();
                }}
              >
                <span className="tl-collapse-label">{t('timeline.collapse')}</span>
              </Button>
            )}
            {searchState?.markers?.length > 0 && (
              <Button
                variant="ghost"
                size="xs"
                className="h-[22px] rounded p-1 text-ide-warning"
                onClick={(event) => {
                  event.stopPropagation();
                  searchFitSuspendedRef.current = false;
                  fitSearchMarkers();
                }}
                title={t('timeline.fitSearchResults')}
                aria-label={t('timeline.fitSearchResults')}
              >
                <Focus size={14} />
              </Button>
            )}
            {quickZooms.map((item) => (
              <Button
                key={item.key}
                variant="ghost"
                size="xs"
                className="h-[22px] rounded px-2 py-0 text-[11px] text-ide-text"
                onClick={(event) => {
                  event.stopPropagation();
                  zoomToSpan(item.span);
                }}
                title={`${item.title} (${item.shortcut})`}
              >
                {item.label}
              </Button>
            ))}
            <Button
              variant="ghost"
              size="xs"
              aria-label={t('timeline.jumpToNow')}
              aria-pressed={isFollowingNow}
              className={`h-[22px] rounded p-1 ${
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
            </Button>
          </div>
        </div>
      </div>
      <TimelineHoverPreview ref={hoverPreviewRef} enabled={!sqlPaused} revision={refreshKey} onBusyChange={handlePreviewBusyChange} />
    </div>
  );
};

export default Timeline;
