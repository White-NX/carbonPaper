import { useCallback, useEffect, useMemo, useRef } from 'react';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { createTimelineBatchQueue } from '../lib/timeline_batch_queue';
import { timelineIndexedPreviewEvents } from '../lib/timeline_index';
import {
  createTimelinePreviewCache,
  timelinePreviewEvents,
  TIMELINE_PREVIEW_THUMBNAIL_BYTES,
  TIMELINE_PREVIEW_THUMBNAIL_ITEMS,
} from '../lib/timeline_preview_cache';

const NO_EVENTS = [];

/** Prefetch exact screen targets and yield to foreground hover work. */
export default function useTimelinePreviewThumbnails({
  events = NO_EVENTS, index, viewStart, viewEnd, width, trackLeft,
  enabled, paused = false, revision, onLoad,
}) {
  const cache = useMemo(() => createTimelinePreviewCache({
    maxItems: TIMELINE_PREVIEW_THUMBNAIL_ITEMS, maxBytes: TIMELINE_PREVIEW_THUMBNAIL_BYTES,
  }), []);
  const planned = useMemo(() => index === undefined
    ? timelinePreviewEvents(events, viewStart, viewEnd)
    : timelineIndexedPreviewEvents(index, viewStart, viewEnd, {
      width, trackLeft, pixelRatio: globalThis.devicePixelRatio || 1, limit: TIMELINE_PREVIEW_THUMBNAIL_ITEMS,
    }), [events, index, viewStart, viewEnd, width, trackLeft]);
  const plannedRef = useRef(planned);
  const priorityRef = useRef(null);
  const enabledRef = useRef(enabled);
  const pausedRef = useRef(paused);
  const foregroundBusyRef = useRef(false);
  const onLoadRef = useRef(onLoad);
  plannedRef.current = planned;
  enabledRef.current = enabled;
  pausedRef.current = paused;
  onLoadRef.current = onLoad;

  const queue = useMemo(() => createTimelineBatchQueue(
    (ids, { signal }) => fetchThumbnailBatch(ids, { signal, cachedOnly: true }),
    {
      batchSize: 4,
      isLoaded: cache.has,
      onResult: (batch, ids) => {
        if (!enabledRef.current) return [];
        const current = new Map(plannedRef.current.map((event) => [event.id, event]));
        if (priorityRef.current) current.set(priorityRef.current.id, priorityRef.current);
        const loaded = [];
        for (const id of ids) {
          if (current.has(id) && cache.set(id, batch?.[id])) loaded.push({ event: current.get(id), imageSrc: batch[id] });
        }
        const accepted = loaded.filter(({ event }) => cache.has(event.id));
        if (accepted.length) onLoadRef.current?.(accepted);
        return accepted.map(({ event }) => event.id);
      },
      // Missing/corrupt disk thumbnails are skipped; originals remain available.
      onError: () => {},
    },
  ), [cache]);

  useEffect(() => {
    cache.clear();
    priorityRef.current = null;
    foregroundBusyRef.current = false;
    queue.setPaused(true);
    queue.reset();
    return () => {
      queue.setPaused(true);
      queue.reset();
      cache.clear();
    };
  }, [enabled, revision, cache, queue]);

  useEffect(() => {
    const ids = planned.map((event) => event.id);
    if (priorityRef.current && !ids.includes(priorityRef.current.id)) ids.unshift(priorityRef.current.id);
    cache.retain(ids);
    queue.setIds(ids, { paused: !enabled || paused || foregroundBusyRef.current });
  }, [planned, enabled, paused, revision, cache, queue]);

  const setForegroundBusy = useCallback((busy) => {
    foregroundBusyRef.current = busy;
    queue.setPaused(!enabledRef.current || pausedRef.current || busy);
  }, [queue]);

  const prioritize = useCallback((event) => {
    const sameId = priorityRef.current?.id === event?.id;
    priorityRef.current = event;
    if (sameId && (!event || cache.has(event.id))) return;
    if (event && !cache.has(event.id)) queue.invalidate(event.id);
    const ids = plannedRef.current.map((entry) => entry.id).filter((id) => id !== event?.id);
    if (event) ids.unshift(event.id);
    cache.retain(ids);
    queue.setIds(ids, { paused: !enabledRef.current || pausedRef.current || foregroundBusyRef.current });
  }, [cache, queue]);

  const remove = useCallback((id) => {
    cache.delete(id);
    queue.invalidate(id);
  }, [cache, queue]);

  return useMemo(() => ({ read: cache.get, remove, prioritize, setForegroundBusy }), [cache, remove, prioritize, setForegroundBusy]);
}
