import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { fetchImage, getTimelineDetails, getTimelineNearest } from '../lib/monitor_api';
import { createTimelineBatchQueue } from '../lib/timeline_batch_queue';
import { createTimelineRequestQueue } from '../lib/timeline_requests';
import { isTimelinePreviewNearby, timelineEventFromRecord } from '../lib/timeline_interaction';
import { createTimelinePreviewCache } from '../lib/timeline_preview_cache';
import useTimelinePreviewIndex from './useTimelinePreviewIndex';
import useTimelinePreviewThumbnails from './useTimelinePreviewThumbnails';

const HOVER_DELAY_MS = 100;
const EXIT_MS = 180;
const CACHE_BYTES = 16 * 1024 * 1024;
const CACHE_ITEMS = 6;
const NO_EVENTS = [];
const empty = { event: null, imageSrc: null, source: null, visible: false, error: false };

async function decodeOriginal(imageSrc, signal) {
  if (imageSrc && typeof Image !== 'undefined' && typeof Image.prototype.decode === 'function') {
    const image = new Image();
    image.decoding = 'async';
    image.src = imageSrc;
    await image.decode();
  }
  if (signal.aborted) throw new DOMException('Preview cancelled', 'AbortError');
  return imageSrc;
}

/** Resolve the exact ID synchronously; thumbnails and originals share that ID. */
export default function useTimelineHoverPreview({
  enabled, revision, onBusyChange, events = NO_EVENTS, viewStart, viewEnd, width, trackLeft, prefetchPaused,
}) {
  const [state, setState] = useState(empty);
  const targetRef = useRef(null);
  const resolvedRef = useRef(null);
  const cache = useMemo(() => createTimelinePreviewCache({ maxItems: CACHE_ITEMS, maxBytes: CACHE_BYTES }), []);
  const metadata = useMemo(() => new Map(), []);
  const eventsRef = useRef(null);
  eventsRef.current = useMemo(() => new Map(events.map((event) => [event.id, event])), [events]);
  const hoverTimerRef = useRef(null);
  const exitTimerRef = useRef(null);
  const enabledRef = useRef(enabled);
  const nearestBusyRef = useRef(false);
  const foregroundRef = useRef(false);
  const imageQueueRef = useRef(null);
  const detailsQueueRef = useRef(null);
  const busyCallbackRef = useRef(onBusyChange);
  enabledRef.current = enabled;
  busyCallbackRef.current = onBusyChange;

  const index = useTimelinePreviewIndex({ enabled: enabled && width !== 0, revision, viewStart, viewEnd, paused: prefetchPaused });
  const indexRef = useRef(index);
  indexRef.current = index;
  const decorate = useCallback((event) => event ? {
    ...eventsRef.current.get(event.id), ...event, ...metadata.get(event.id), id: event.id, timestamp: event.timestamp,
  } : event, [metadata]);
  const remember = useCallback((record) => {
    const event = timelineEventFromRecord(record);
    if (!event) return null;
    metadata.delete(event.id);
    metadata.set(event.id, event);
    while (metadata.size > 128) metadata.delete(metadata.keys().next().value);
    return event;
  }, [metadata]);

  const handleThumbnailsLoaded = useCallback((loaded) => {
    const target = targetRef.current;
    const resolved = resolvedRef.current;
    if (!enabledRef.current || !target || resolved?.time !== target.time || !resolved.event) return;
    const matching = loaded.find((entry) => entry.event.id === resolved.event.id);
    if (!matching) return;
    setState((previous) => previous.event?.id === resolved.event.id && previous.source !== 'original'
      ? { ...previous, imageSrc: matching.imageSrc, source: 'thumbnail', error: false } : previous);
  }, []);
  const thumbnails = useTimelinePreviewThumbnails({
    index: index.index, events, viewStart, viewEnd, width, trackLeft,
    enabled, paused: prefetchPaused, revision, onLoad: handleThumbnailsLoaded,
  });

  const notifyBusy = useCallback(() => {
    const id = resolvedRef.current?.event?.id;
    const busy = Boolean(targetRef.current) && (nearestBusyRef.current
      || (foregroundRef.current && id != null
        && (imageQueueRef.current?.isBusy([id]) || detailsQueueRef.current?.isBusy([id]))));
    thumbnails.setForegroundBusy(busy);
    busyCallbackRef.current?.(busy);
  }, [thumbnails]);

  const imageQueue = useMemo(() => createTimelineBatchQueue(async ([id], { signal }) => {
    if (signal.aborted) throw new DOMException('Preview cancelled', 'AbortError');
    const imageSrc = await fetchImage(id);
    if (signal.aborted) throw new DOMException('Preview cancelled', 'AbortError');
    return decodeOriginal(imageSrc, signal);
  }, {
    batchSize: 1,
    isLoaded: cache.has,
    onResult: (imageSrc, [id]) => {
      if (!enabledRef.current || !targetRef.current) return [];
      cache.set(id, imageSrc);
      const resolved = resolvedRef.current;
      if (resolved?.time === targetRef.current.time && resolved.event?.id === id) {
        const thumbnailSrc = !imageSrc ? thumbnails.read(id) : null;
        setState((previous) => previous.event?.id === id ? {
          ...previous, imageSrc: imageSrc || thumbnailSrc,
          source: imageSrc ? 'original' : thumbnailSrc ? 'thumbnail' : null, error: !imageSrc,
        } : previous);
      }
      return imageSrc ? [id] : [];
    },
    onError: () => {
      const resolved = resolvedRef.current;
      if (enabledRef.current && targetRef.current?.time === resolved?.time && resolved?.event) {
        const imageSrc = thumbnails.read(resolved.event.id);
        setState((previous) => ({ ...previous, imageSrc, source: imageSrc ? 'thumbnail' : null, error: true }));
      }
    },
    onSettled: notifyBusy,
  }), [cache, thumbnails, notifyBusy]);
  imageQueueRef.current = imageQueue;

  const detailsQueue = useMemo(() => createTimelineBatchQueue(getTimelineDetails, {
    batchSize: 1,
    isLoaded: (id) => metadata.has(id) || eventsRef.current.get(id)?.appName != null,
    onResult: (records) => {
      if (!enabledRef.current) return [];
      const accepted = (records || []).map(remember).filter(Boolean);
      const resolved = resolvedRef.current;
      if (targetRef.current?.time === resolved?.time && accepted.some((event) => event.id === resolved?.event?.id)) {
        const event = decorate(resolved.event);
        resolvedRef.current = { ...resolved, event };
        setState((previous) => previous.event?.id === event.id ? { ...previous, event } : previous);
      }
      return accepted.map((event) => event.id);
    },
    onError: () => {},
    onSettled: notifyBusy,
  }), [metadata, remember, decorate, notifyBusy]);
  detailsQueueRef.current = detailsQueue;

  const startForeground = useCallback(() => {
    const resolved = resolvedRef.current;
    const target = targetRef.current;
    if (!target || target.time !== resolved?.time
      || !isTimelinePreviewNearby(target.time, resolved?.event, target.zoom)) return;
    const ids = [resolved.event.id];
    imageQueue.setIds(ids, { paused: false });
    detailsQueue.setIds(ids, { paused: false });
    notifyBusy();
  }, [imageQueue, detailsQueue, notifyBusy]);

  const resolve = useCallback((time, selected) => {
    const target = targetRef.current;
    if (!enabledRef.current || target?.time !== time || selected === undefined
      || (resolvedRef.current?.time === time
        && isTimelinePreviewNearby(time, resolvedRef.current.event, target.zoom))) return;
    const event = decorate(selected);
    resolvedRef.current = { time, event, resolvedAt: performance.now() };
    if (!isTimelinePreviewNearby(time, event, target.zoom)) {
      setState(empty);
      thumbnails.prioritize(null);
      return;
    }
    const originalSrc = cache.get(event.id);
    const imageSrc = originalSrc || thumbnails.read(event.id);
    setState({
      event, imageSrc, source: originalSrc ? 'original' : imageSrc ? 'thumbnail' : null, visible: true, error: false,
    });
    thumbnails.prioritize(event);
    if (foregroundRef.current) startForeground();
  }, [decorate, cache, thumbnails, startForeground]);

  const nearestQueue = useMemo(() => createTimelineRequestQueue(
    ({ start }, signal) => getTimelineNearest(start, { signal }),
    {
      onResult: (record, { start }) => {
        if (!enabledRef.current || targetRef.current?.time !== start) return;
        resolve(start, record ? remember(record) : null);
      },
      onError: (_error, { start }) => {
        if (enabledRef.current && targetRef.current?.time === start && resolvedRef.current?.time !== start) {
          setState((previous) => ({ ...previous, error: true }));
        }
      },
      onBusyChange: (busy) => { nearestBusyRef.current = busy; notifyBusy(); },
    },
  ), [resolve, remember, notifyBusy]);

  const show = useCallback((target) => {
    if (!enabledRef.current) return;
    clearTimeout(exitTimerRef.current);
    clearTimeout(hoverTimerRef.current);
    const sameHover = targetRef.current?.time === target.time && targetRef.current?.zoom === target.zoom;
    const previous = resolvedRef.current;
    targetRef.current = target;
    if (!sameHover) {
      nearestQueue.cancel();
      imageQueue.setIds([], { paused: true });
      detailsQueue.setIds([], { paused: true });
      foregroundRef.current = false;
      resolvedRef.current = null;
      const indexed = indexRef.current.read(target.time);
      const selected = indexed !== undefined ? indexed
        : previous?.time === target.time && performance.now() - previous.resolvedAt < 500 ? previous.event : undefined;
      if (selected !== undefined) resolve(target.time, selected);
      else {
        // Wait for the exact timestamp before deciding whether the pointer is
        // close enough; an unresolved gap must not flash a loading card.
        setState(empty);
        thumbnails.prioritize(null);
      }
    }
    notifyBusy();
    hoverTimerRef.current = setTimeout(() => {
      foregroundRef.current = true;
      if (resolvedRef.current?.time === target.time) startForeground();
      else nearestQueue.request({ start: target.time, end: target.time });
      notifyBusy();
    }, HOVER_DELAY_MS);
  }, [resolve, imageQueue, detailsQueue, nearestQueue, thumbnails, notifyBusy, startForeground]);

  // A late index or new capture can reveal a nearby snapshot in a hidden gap.
  // Refreshes cannot replace an ID already previewed for a stationary pointer.
  useEffect(() => {
    const target = targetRef.current;
    if (target && !isTimelinePreviewNearby(target.time, resolvedRef.current?.event, target.zoom)) {
      resolve(target.time, index.read(target.time));
    }
  }, [index.index, index.read, resolve]);

  const hide = useCallback(() => {
    clearTimeout(hoverTimerRef.current);
    clearTimeout(exitTimerRef.current);
    targetRef.current = null;
    foregroundRef.current = false;
    nearestQueue.cancel();
    imageQueue.setIds([], { paused: true });
    detailsQueue.setIds([], { paused: true });
    thumbnails.prioritize(null);
    notifyBusy();
    setState((previous) => previous.visible ? { ...previous, visible: false } : previous);
    exitTimerRef.current = setTimeout(() => setState(empty), EXIT_MS);
  }, [imageQueue, detailsQueue, nearestQueue, thumbnails, notifyBusy]);

  const clear = useCallback(() => {
    clearTimeout(hoverTimerRef.current);
    clearTimeout(exitTimerRef.current);
    targetRef.current = null;
    resolvedRef.current = null;
    foregroundRef.current = false;
    nearestQueue.cancel();
    imageQueue.setPaused(true);
    imageQueue.reset();
    detailsQueue.setPaused(true);
    detailsQueue.reset();
    cache.clear();
    metadata.clear();
    notifyBusy();
    setState(empty);
  }, [cache, metadata, imageQueue, detailsQueue, nearestQueue, notifyBusy]);

  useEffect(() => {
    clear();
    return () => {
      clearTimeout(hoverTimerRef.current);
      clearTimeout(exitTimerRef.current);
      nearestQueue.cancel();
      imageQueue.setPaused(true);
      imageQueue.reset();
      detailsQueue.setPaused(true);
      detailsQueue.reset();
      cache.clear();
      metadata.clear();
      targetRef.current = null;
      busyCallbackRef.current?.(false);
    };
  }, [enabled, revision, clear, cache, metadata, imageQueue, detailsQueue, nearestQueue]);

  const read = useCallback((time) => {
    const resolved = resolvedRef.current;
    // Pointer-down hides the card before click selects it. Retain that chosen
    // identity until a new hover or revision, even after a long stationary hover.
    if (resolved?.time === time) return resolved.event;
    return decorate(indexRef.current.read(time)) || null;
  }, [decorate]);
  const reportImageError = useCallback((id, source = 'original') => {
    if (source === 'thumbnail') thumbnails.remove(id);
    else cache.delete(id);
    setState((previous) => {
      if (previous.event?.id !== id || previous.source !== source) return previous;
      const imageSrc = source === 'original' ? thumbnails.read(id) : null;
      return { ...previous, imageSrc, source: imageSrc ? 'thumbnail' : null, error: source === 'original' };
    });
  }, [cache, thumbnails]);
  return { state, show, hide, read, reportImageError };
}
