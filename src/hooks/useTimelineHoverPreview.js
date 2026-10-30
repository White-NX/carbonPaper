import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { fetchImage, getTimelineNearest } from '../lib/monitor_api';
import { createTimelineBatchQueue } from '../lib/timeline_batch_queue';
import { createTimelineRequestQueue } from '../lib/timeline_requests';
import { timelineEventFromRecord } from '../lib/timeline_interaction';

const HOVER_DELAY_MS = 100;
const EXIT_MS = 180;
const CACHE_BYTES = 16 * 1024 * 1024;
const CACHE_ITEMS = 6;
const empty = { event: null, imageSrc: null, visible: false, error: false };

/** One latest nearest lookup and one latest original, with no retained data on pause. */
export default function useTimelineHoverPreview({ enabled, revision, onBusyChange }) {
  const [state, setState] = useState(empty);
  const targetRef = useRef(null);
  const resolvedRef = useRef(null);
  const cacheRef = useRef(new Map());
  const hoverTimerRef = useRef(null);
  const exitTimerRef = useRef(null);
  const enabledRef = useRef(enabled);
  const nearestBusyRef = useRef(false);
  const imageQueueRef = useRef(null);
  const busyCallbackRef = useRef(onBusyChange);
  enabledRef.current = enabled;
  busyCallbackRef.current = onBusyChange;

  const notifyBusy = useCallback(() => {
    const id = resolvedRef.current?.event?.id;
    busyCallbackRef.current?.(Boolean(targetRef.current)
      && (nearestBusyRef.current || (id != null && imageQueueRef.current?.isBusy([id]))));
  }, []);

  const imageQueue = useMemo(() => createTimelineBatchQueue(async ([id], { signal }) => {
    if (signal.aborted) throw new DOMException('Preview cancelled', 'AbortError');
    const imageSrc = await fetchImage(id);
    if (signal.aborted) throw new DOMException('Preview cancelled', 'AbortError');
    return imageSrc;
  }, {
    batchSize: 1,
    isLoaded: (id) => cacheRef.current.has(id),
    onResult: (imageSrc, [id]) => {
      if (!enabledRef.current || !targetRef.current) return [];
      if (imageSrc && imageSrc.length * 2 <= CACHE_BYTES) {
        cacheRef.current.delete(id);
        cacheRef.current.set(id, imageSrc);
        let bytes = [...cacheRef.current.values()].reduce((sum, src) => sum + src.length * 2, 0);
        while (cacheRef.current.size > CACHE_ITEMS || bytes > CACHE_BYTES) {
          const oldest = cacheRef.current.keys().next().value;
          bytes -= cacheRef.current.get(oldest).length * 2;
          cacheRef.current.delete(oldest);
        }
      }
      setState((previous) => previous.event?.id === id
        ? { ...previous, imageSrc, error: !imageSrc } : previous);
      return imageSrc ? [id] : [];
    },
    onError: () => {
      if (enabledRef.current && targetRef.current) setState((previous) => ({ ...previous, error: true }));
    },
    onSettled: notifyBusy,
  }), [notifyBusy]);
  imageQueueRef.current = imageQueue;

  const nearestQueue = useMemo(() => createTimelineRequestQueue(
    ({ start }, signal) => getTimelineNearest(start, { signal }),
    {
      onResult: (record, { start }) => {
        if (!enabledRef.current || targetRef.current?.time !== start) return;
        const event = timelineEventFromRecord(record);
        resolvedRef.current = { time: start, event, resolvedAt: performance.now() };
        const imageSrc = event ? cacheRef.current.get(event.id) || null : null;
        setState(event ? { event, imageSrc, visible: true, error: false } : empty);
        imageQueue.setIds(event ? [event.id] : [], { paused: !event });
        notifyBusy();
      },
      onError: (_error, { start }) => {
        if (enabledRef.current && targetRef.current?.time === start) {
          setState((previous) => ({ ...previous, error: true }));
        }
      },
      onBusyChange: (busy) => { nearestBusyRef.current = busy; notifyBusy(); },
    },
  ), [imageQueue, notifyBusy]);

  const show = useCallback((target) => {
    if (!enabledRef.current) return;
    clearTimeout(exitTimerRef.current);
    clearTimeout(hoverTimerRef.current);
    targetRef.current = target;
    // Cached originals may appear immediately. New image I/O waits for the
    // actual nearest snapshot instead of loading every sampled frame crossed.
    const event = resolvedRef.current?.time === target.time ? resolvedRef.current.event : target.event;
    if (resolvedRef.current?.time === target.time && !event
      && performance.now() - resolvedRef.current.resolvedAt < 500) {
      setState(empty);
      return;
    }
    const cached = event ? cacheRef.current.get(event.id) || null : null;
    setState((previous) => previous.visible && previous.event?.id === event?.id
      ? previous : { event, imageSrc: cached, visible: true, error: false });
    hoverTimerRef.current = setTimeout(() => {
      if (resolvedRef.current?.time === target.time
        && performance.now() - resolvedRef.current.resolvedAt < 500) {
        imageQueue.setIds(event ? [event.id] : [], { paused: !event });
        notifyBusy();
        return;
      }
      nearestQueue.request({ start: target.time, end: target.time });
    }, HOVER_DELAY_MS);
  }, [imageQueue, nearestQueue, notifyBusy]);

  const hide = useCallback(() => {
    clearTimeout(hoverTimerRef.current);
    clearTimeout(exitTimerRef.current);
    targetRef.current = null;
    nearestQueue.cancel();
    imageQueue.setIds([], { paused: true });
    busyCallbackRef.current?.(false);
    setState((previous) => previous.visible ? { ...previous, visible: false } : previous);
    exitTimerRef.current = setTimeout(() => setState(empty), EXIT_MS);
  }, [imageQueue, nearestQueue]);

  const clear = useCallback(() => {
    clearTimeout(hoverTimerRef.current);
    clearTimeout(exitTimerRef.current);
    targetRef.current = null;
    resolvedRef.current = null;
    nearestQueue.cancel();
    imageQueue.setPaused(true);
    imageQueue.reset();
    cacheRef.current.clear();
    busyCallbackRef.current?.(false);
    setState(empty);
  }, [imageQueue, nearestQueue]);

  useEffect(() => {
    clear();
    return () => {
      clearTimeout(hoverTimerRef.current);
      clearTimeout(exitTimerRef.current);
      nearestQueue.cancel();
      imageQueue.setPaused(true);
      imageQueue.reset();
      cacheRef.current.clear();
      targetRef.current = null;
      busyCallbackRef.current?.(false);
    };
  }, [enabled, revision, clear, imageQueue, nearestQueue]);

  const read = useCallback((time) => resolvedRef.current?.time === time
    && performance.now() - resolvedRef.current.resolvedAt < 500 ? resolvedRef.current.event : null, []);
  const reportImageError = useCallback((id) => {
    cacheRef.current.delete(id);
    setState((previous) => previous.event?.id === id ? { ...previous, imageSrc: null, error: true } : previous);
  }, []);
  return { state, show, hide, read, reportImageError };
}
