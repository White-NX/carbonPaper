import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Focus, Play } from 'lucide-react';
import { getTimeline, getTimelineDensity, fetchThumbnailBatch } from '../lib/monitor_api';
import {
  aggregateAppDistribution,
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
import { accentWithAlpha } from '../lib/timeline_palette';
import OverviewBand from './timeline/OverviewBand';
import SessionBand from './timeline/SessionBand';
import DetailTrack from './timeline/DetailTrack';
import SearchMarkerLayer from './timeline/SearchMarkerLayer';
import { getSearchMarkerFitRange } from '../lib/timeline_search';

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
const OVERVIEW_PREFETCH_FACTOR = 2;

/** Refetch once the viewport comes this close to the edge of loaded data. */
const REFETCH_MARGIN_RATIO = 0.35;
const OVERVIEW_REFETCH_MARGIN_RATIO = 0.1;

/** Shortest gap between two fetches of the same kind while the user is dragging. */
const EVENT_FETCH_INTERVAL_MS = 300;
const DENSITY_FETCH_INTERVAL_MS = 350;
const OVERVIEW_FETCH_INTERVAL_MS = 500;
const THUMBNAIL_BATCH_INTERVAL_MS = 250;

/** Density bars aimed for across one screen width, and across the overview band. */
const DENSITY_BUCKETS_PER_SCREEN = 120;
const OVERVIEW_BUCKET_COUNT = 160;

/** How often loaded data is read again so newly captured screenshots appear. */
const FOLLOW_REFRESH_MS = 1000;
const IDLE_REFRESH_MS = 5000;

/** Overview band span configuration factor. */
const OVERVIEW_SPAN_FACTOR = 10;
const OVERVIEW_MIN_SPAN = 2 * HOUR;
const OVERVIEW_MAX_SPAN = 90 * DAY;
/** Recenter threshold ratio for overview band. */
const OVERVIEW_RECENTER_RATIO = 0.3;

/** Drag slop threshold in pixels. */
const DRAG_SLOP_PX = 4;

/** Zoom bounds in pixels per millisecond: a year across, down to 20 px a second. */
const MIN_ZOOM = 100 / (365 * DAY);
const MAX_ZOOM = 20 / 1000;

/** Multiplier applied to the zoom by one wheel notch. */
const WHEEL_ZOOM_STEP = 1.2;

/** Local timezone offset relative to UTC in milliseconds. */
function localBucketOffsetMs() {
  return -new Date().getTimezoneOffset() * MINUTE;
}

const TIMELINE_IMAGE_CACHE_LIMIT = 800;
const timelineImageCache = new Map();
/** Ids the backend holds no thumbnail for, so a batch is not asked for them forever. */
const timelineImageMissing = new Set();

function setTimelineImageCache(key, dataUrl) {
  if (key === null || key === undefined || !dataUrl) return;
  if (!timelineImageCache.has(key) && timelineImageCache.size >= TIMELINE_IMAGE_CACHE_LIMIT) {
    const oldestKey = timelineImageCache.keys().next().value;
    timelineImageCache.delete(oldestKey);
  }
  timelineImageCache.set(key, dataUrl);
}

/** Record a missing thumbnail, evicting the oldest so the set stays bounded. */
function markTimelineImageMissing(key) {
  if (!timelineImageMissing.has(key) && timelineImageMissing.size >= TIMELINE_IMAGE_CACHE_LIMIT) {
    timelineImageMissing.delete(timelineImageMissing.values().next().value);
  }
  timelineImageMissing.add(key);
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
  onClearHighlight,
  jumpTimestamp,
  highlightedEventId,
  refreshKey,
  sqlPaused,
  searchState,
  onSelectSearchResult,
}) => {
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

  const lastMouseXRef = useRef(0);
  const isDraggingRef = useRef(false);
  const dragMovedRef = useRef(0);
  const fetchEpochRef = useRef(0);
  const densityEpochRef = useRef(0);
  const overviewEpochRef = useRef(0);
  const wheelIdleTimerRef = useRef(null);
  const pendingImageIdsRef = useRef([]);
  const searchFitKeyRef = useRef(null);
  const searchFitSuspendedRef = useRef(false);
  const jumpTokenRef = useRef(null);

  /** Ranges already asked for, so a pan only refetches near their edges. */
  const requestedEventsRef = useRef(null);
  const requestedDensityRef = useRef(null);
  const requestedOverviewRef = useRef(null);
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
  const overviewViewRef = useRef({ anchor: 0, span: 0 });
  const zoomLevelRef = useRef(null);
  const thumbnailInFlightRef = useRef(false);
  const loadThumbnailsRef = useRef(null);

  /** Elements the camera moves, and the axis it repositions label by label. */
  const sessionStageRef = useRef(null);
  const detailStageRef = useRef(null);
  const markerStageRef = useRef(null);
  const axisRef = useRef(null);
  const stages = useMemo(
    () => [sessionStageRef, detailStageRef, markerStageRef],
    [],
  );

  const visibleSpan = width > 0 ? width / zoom : HOUR;
  const viewStart = centerTime - visibleSpan / 2;
  const viewEnd = centerTime + visibleSpan / 2;
  const overscanPx = width * RENDER_OVERSCAN_RATIO;

  const tier = visibleSpan <= FRAME_TIER_MAX_SPAN
    ? 'frame'
    : visibleSpan <= SESSION_TIER_MAX_SPAN
      ? 'session'
      : 'day';

  // Snapped so the band, and the fetch behind it, hold still inside a zoom level
  const overviewSpan = Math.max(
    snapSpanUpMs(visibleSpan),
    Math.min(
      OVERVIEW_MAX_SPAN,
      snapSpanUpMs(Math.max(OVERVIEW_MIN_SPAN, visibleSpan * OVERVIEW_SPAN_FACTOR)),
    ),
  );
  const overviewStart = overviewAnchor - overviewSpan / 2;
  const overviewEnd = overviewAnchor + overviewSpan / 2;

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

  // Recenter the overview band once the viewport has drifted too far across it.
  // Suspended mid-flight, where the centre is passing through values nobody is
  // meant to read.
  useEffect(() => {
    if (overviewInteracting || camera.isFlying()) return;
    if (Math.abs(centerTime - overviewAnchor) > overviewSpan * OVERVIEW_RECENTER_RATIO) {
      setOverviewAnchor(centerTime);
    }
  }, [centerTime, overviewAnchor, overviewSpan, overviewInteracting, camera]);

  // ── Data Fetching ──────────────────────────────────────────

  const fetchEventsRange = useCallback(async (start, end, sampleMs) => {
    const epoch = ++fetchEpochRef.current;

    try {
      const records = await getTimeline(start, end, TIMELINE_SAMPLE_LIMIT);
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

      // Two fetches taken at different sampling widths describe the same hours at
      // different fidelity. Merging them would leave the session band showing a
      // dense island inside a sparse surround, so that one zoom level looks
      // different depending on the route the user took to reach it. Only samples
      // of matching width are merged; a change of width replaces outright.
      const sameFidelity = loadedSampleMsRef.current === sampleMs;
      loadedSampleMsRef.current = sampleMs;

      setEvents((prev) => (sameFidelity
        ? pruneEvents(mergeSortedEvents(prev, mapped), start, end)
        : mapped));
    } catch (error) {
      console.error('[Timeline] Fetch error:', error);
      // Forget the request so the range guard tries again rather than believing
      // this range is covered. A newer request has already replaced it.
      if (fetchEpochRef.current === epoch) requestedEventsRef.current = null;
    }
  }, []);

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

  const fetchOverviewRange = useCallback(async (start, end, bucketMs) => {
    const epoch = ++overviewEpochRef.current;

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
      if (overviewEpochRef.current === epoch) requestedOverviewRef.current = null;
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
  const requestOverview = useMemo(
    () => createThrottle(fetchOverviewRange, OVERVIEW_FETCH_INTERVAL_MS),
    [fetchOverviewRange],
  );

  /**
   * Fetch screenshots only once the viewport is running out of loaded data.
   *
   * Panning used to reset a debounce timer on every frame, so nothing arrived
   * until the pointer was released. Deciding on coverage instead lets a drag keep
   * loading while still costing a few requests rather than one per frame, because
   * each fetch reaches well past both edges of the screen.
   */
  const ensureEvents = useCallback((center, span, force = false) => {
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
    requestEvents(start, end, sampleMs);
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

  const ensureOverview = useCallback((anchor, span, force = false) => {
    if (!span) return;

    const requestSpan = span * OVERVIEW_PREFETCH_FACTOR;
    const bucketMs = snapSpanUpMs(span / OVERVIEW_BUCKET_COUNT);
    const requested = requestedOverviewRef.current;

    const covered = requested?.bucketMs === bucketMs
      && isRangeCovered(
        requested,
        anchor - span / 2,
        anchor + span / 2,
        span * OVERVIEW_REFETCH_MARGIN_RATIO,
      );
    if (covered && !force) return;

    const start = anchor - requestSpan / 2;
    const end = anchor + requestSpan / 2;
    requestedOverviewRef.current = { start, end, bucketMs };
    requestOverview(start, end, bucketMs);
  }, [requestOverview]);

  useEffect(() => {
    overviewViewRef.current = { anchor: overviewAnchor, span: overviewSpan };
  }, [overviewAnchor, overviewSpan]);

  // A flight asks for its destination once when it sets off, so nothing is
  // requested for the viewports it passes over on the way. Each of those is on
  // screen for a couple of frames and would never arrive in time to be seen.
  useEffect(() => {
    if (sqlPaused || !width || camera.isFlying()) return;
    ensureEvents(centerTime, visibleSpan);
    ensureDensity(centerTime, visibleSpan);
  }, [centerTime, visibleSpan, width, sqlPaused, camera, ensureEvents, ensureDensity]);

  useEffect(() => {
    if (sqlPaused || !width || camera.isFlying()) return;
    ensureOverview(overviewAnchor, overviewSpan);
  }, [overviewAnchor, overviewSpan, width, sqlPaused, camera, ensureOverview]);

  useEffect(() => {
    if (refreshKey === undefined || !width) return;
    setEvents([]);
    timelineImageMissing.clear();
    requestedEventsRef.current = null;
    requestedDensityRef.current = null;
    requestedOverviewRef.current = null;
    loadedSampleMsRef.current = null;
    setImageEpoch((prev) => prev + 1);
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
      const overview = overviewViewRef.current;
      ensureEvents(view.center, view.span, true);
      ensureDensity(view.center, view.span, true);
      ensureOverview(overview.anchor, overview.span, true);
    }, period);

    return () => clearInterval(interval);
  }, [sqlPaused, width, isFollowingNow, ensureEvents, ensureDensity, ensureOverview]);

  useEffect(() => () => {
    requestEvents.cancel();
    requestDensity.cancel();
    requestOverview.cancel();
  }, [requestEvents, requestDensity, requestOverview]);

  // ── Thumbnail Loading ──────────────────────────────────────

  const handleVisibleFramesChange = useCallback((ids) => {
    pendingImageIdsRef.current = ids;
  }, []);

  /**
   * Load thumbnails for whatever is on screen right now.
   *
   * Only one batch is in flight at a time, and a successful batch immediately
   * asks for another round, so a drag keeps filling in frames instead of waiting
   * for the pointer to be released.
   */
  const loadVisibleThumbnails = useCallback(() => {
    if (thumbnailInFlightRef.current) return;

    const ids = pendingImageIdsRef.current;
    if (!ids || ids.length === 0) return;

    const wanted = ids.filter(
      (id) => !timelineImageCache.has(id) && !timelineImageMissing.has(id),
    );
    if (wanted.length === 0) return;

    thumbnailInFlightRef.current = true;
    fetchThumbnailBatch(wanted)
      .then((batch) => {
        let added = 0;
        for (const id of wanted) {
          const dataUrl = batch?.[id];
          if (dataUrl) {
            setTimelineImageCache(id, dataUrl);
            added += 1;
          } else {
            // Remember the gap, otherwise these ids are asked for on every round
            markTimelineImageMissing(id);
          }
        }
        if (added > 0) setImageEpoch((prev) => prev + 1);

        thumbnailInFlightRef.current = false;
        // A round may have been skipped while this batch was in flight
        loadThumbnailsRef.current?.();
      })
      .catch((error) => {
        thumbnailInFlightRef.current = false;
        console.error('[Timeline] Batch thumbnail load error:', error);
      });
  }, []);

  const requestThumbnails = useMemo(
    () => createThrottle(loadVisibleThumbnails, THUMBNAIL_BATCH_INTERVAL_MS),
    [loadVisibleThumbnails],
  );

  useEffect(() => {
    loadThumbnailsRef.current = requestThumbnails;
    return () => {
      loadThumbnailsRef.current = null;
      requestThumbnails.cancel();
    };
  }, [requestThumbnails]);

  useEffect(() => {
    if (sqlPaused || tier !== 'frame') return;
    requestThumbnails();
  }, [centerTime, zoom, width, events, sqlPaused, tier, requestThumbnails]);

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
    setImageEpoch((prev) => prev + 1);
  }, []);

  const suspendSearchFit = useCallback(() => {
    if (searchState?.markers?.length) searchFitSuspendedRef.current = true;
  }, [searchState]);

  /**
   * Send the camera to a viewport, and start loading the destination while it is
   * still on its way.
   *
   * Every deliberate move goes through here: the quick zoom buttons, the search
   * fit, a double click on a session, a click in the overview band. Continuous
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

  const seekTo = useCallback((time) => {
    suspendSearchFit();
    setIsFollowingNow(false);
    flyTo(time, camera.read().zoom);
  }, [camera, flyTo, suspendSearchFit]);

  /**
   * Continuous drag of the overview viewport.
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
    resetImagePipeline();
  }, [width, flyTo, resetImagePipeline, suspendSearchFit]);

  const fitSearchMarkers = useCallback(() => {
    const range = getSearchMarkerFitRange(searchState?.markers);
    if (!range || !width) return;
    setIsFollowingNow(false);
    flyTo((range.from + range.to) / 2, width / (range.to - range.from));
    resetImagePipeline();
  }, [searchState?.markers, width, flyTo, resetImagePipeline]);

  useEffect(() => {
    if (!searchState?.fitKey || !searchState.markers?.length) {
      searchFitKeyRef.current = null;
      searchFitSuspendedRef.current = false;
      return;
    }
    if (!width) return;

    const isNewSearch = searchFitKeyRef.current !== searchState.fitKey;
    if (isNewSearch) {
      searchFitKeyRef.current = searchState.fitKey;
      searchFitSuspendedRef.current = false;
    }

    if (isNewSearch && !searchFitSuspendedRef.current) fitSearchMarkers();
  }, [searchState?.fitKey, searchState?.markers, width, fitSearchMarkers]);

  const zoomToSpan = useCallback((spanMs) => {
    if (!width) return;
    suspendSearchFit();
    setIsFollowingNow(false);
    flyTo(Date.now(), width / spanMs);
    resetImagePipeline();
  }, [width, flyTo, resetImagePipeline, suspendSearchFit]);

  const jumpToNow = useCallback(() => {
    suspendSearchFit();
    setIsFollowingNow(false);
    // Following starts once the camera has landed, so the flight itself is not
    // fighting a centre that keeps moving forward underneath it.
    flyTo(Date.now(), MAX_ZOOM, { onDone: () => setIsFollowingNow(true) });
    resetImagePipeline();
  }, [flyTo, resetImagePipeline, suspendSearchFit]);

  // Guarded by the token rather than by the dependency list, because `flyTo`
  // changes identity whenever the track is resized and must not replay a jump.
  useEffect(() => {
    if (!jumpTimestamp?.time || jumpTokenRef.current === jumpTimestamp) return;
    jumpTokenRef.current = jumpTimestamp;
    setIsFollowingNow(false);
    flyTo(jumpTimestamp.time, Math.max(camera.read().zoom, 0.005));
    resetImagePipeline();
  }, [jumpTimestamp, camera, flyTo, resetImagePipeline]);

  useEffect(() => {
    camera.setLive(isFollowingNow);
  }, [camera, isFollowingNow]);

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
    suspendSearchFit();
    setIsFollowingNow(false);
    setIsDragging(true);
    isDraggingRef.current = true;
    dragMovedRef.current = 0;
    lastMouseXRef.current = event.clientX;
  }, [suspendSearchFit]);

  const handleMouseMove = useCallback((event) => {
    if (!isDraggingRef.current) return;
    const deltaX = event.clientX - lastMouseXRef.current;
    lastMouseXRef.current = event.clientX;
    dragMovedRef.current += Math.abs(deltaX);
    // Straight to the camera, with no smoothing: content that trails the cursor
    // during a drag reads as lag rather than as polish.
    camera.panBy(deltaX);
  }, [camera]);

  const endDrag = useCallback(() => {
    if (!isDraggingRef.current) return;
    setIsDragging(false);
    isDraggingRef.current = false;
    setImageEpoch((prev) => prev + 1);
  }, []);

  const handleWheel = useCallback((event) => {
    try { event.preventDefault(); } catch { /* passive listener */ }
    setIsFollowingNow(false);
    suspendSearchFit();

    if (wheelIdleTimerRef.current) clearTimeout(wheelIdleTimerRef.current);
    wheelIdleTimerRef.current = setTimeout(() => setImageEpoch((prev) => prev + 1), 160);

    const rect = containerRef.current?.getBoundingClientRect();
    if (!rect) return;

    camera.zoomAt(
      event.clientX - rect.left,
      event.deltaY < 0 ? WHEEL_ZOOM_STEP : 1 / WHEEL_ZOOM_STEP,
    );
  }, [camera, suspendSearchFit]);

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
        onScrub={scrubTo}
        onSeek={seekTo}
        onSeekRange={seekRange}
        onInteractingChange={setOverviewInteracting}
        searchMarkers={searchState?.markers || []}
        hoveredSearchMarkerIds={searchState?.hoveredIds || []}
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
          overscanPx={overscanPx}
          foldLevel={fold.level}
          stageRef={sessionStageRef}
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
          overscanPx={overscanPx}
          stageRef={detailStageRef}
          highlightedEventId={highlightedEventId}
          imageCache={timelineImageCache}
          imageEpoch={imageEpoch}
          onSelectEvent={handleSelectFrame}
          onSeek={seekTo}
          onVisibleFramesChange={handleVisibleFramesChange}
        />

        <div className="pointer-events-none absolute inset-x-0" style={{ top: SESSION_HEIGHT, height: DETAIL_HEIGHT }}>
          <SearchMarkerLayer
            markers={searchState?.markers || []}
            hoveredIds={searchState?.hoveredIds || []}
            timeToX={timeToX}
            width={width}
            height={DETAIL_HEIGHT}
            overscanPx={overscanPx}
            stageRef={markerStageRef}
            onSelectMarker={(marker) => {
              if (marker?.result) onSelectSearchResult?.(marker.result);
            }}
          />
        </div>

        <div className="relative" style={{ height: AXIS_HEIGHT }}>
          <div ref={axisRef} className="absolute inset-0">
            {ticks.map((tick) => (
              <div
                key={tick.time}
                data-x={tick.x}
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
        </div>

        <div
          className="pointer-events-none absolute inset-y-0 left-1/2 z-30 w-px"
          style={{ backgroundColor: accentWithAlpha(0.75) }}
        />
      </div>

      <div className="absolute bottom-1.5 right-2 z-40 flex items-center gap-1">
        {searchState?.markers?.length > 0 && (
          <button
            type="button"
            className="rounded border border-ide-border bg-ide-panel p-1 text-ide-warning shadow-sm transition-colors hover:bg-ide-active"
            onClick={(event) => {
              event.stopPropagation();
              searchFitSuspendedRef.current = false;
              fitSearchMarkers();
            }}
            title={t('timeline.fitSearchResults')}
            aria-label={t('timeline.fitSearchResults')}
          >
            <Focus size={14} />
          </button>
        )}
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
