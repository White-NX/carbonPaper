import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { getTimelineIndex } from '../lib/monitor_api';
import { createTimelineIndex } from '../lib/timeline_index';
import { createTimelineRequestQueue } from '../lib/timeline_requests';

const REFRESH_MS = 5000;

/** One unsampled viewport index; reads retain cancellation/authentication guards. */
export default function useTimelinePreviewIndex({ enabled, paused, revision, viewStart, viewEnd }) {
  const [index, setIndex] = useState(null);
  const indexRef = useRef(null);
  const requestedRef = useRef(null);
  const viewRef = useRef(null);
  viewRef.current = { enabled, paused, start: viewStart, end: viewEnd };

  const queue = useMemo(() => createTimelineRequestQueue(
    ({ start, end }, signal) => getTimelineIndex(start, end, { signal }),
    {
      onResult: (entries, { start, end }) => {
        if (!viewRef.current.enabled) return;
        const loaded = createTimelineIndex(entries, start, end);
        indexRef.current = loaded;
        setIndex(loaded);
      },
      onError: (error) => {
        requestedRef.current = null;
        console.error('[Timeline] Preview index load error:', error);
      },
    },
  ), []);

  const ensure = useCallback((refresh = false) => {
    const { enabled: live, paused: busy, start, end } = viewRef.current;
    if (!live || busy || !Number.isFinite(start) || !Number.isFinite(end) || start >= end) return;
    const span = end - start;
    const requested = requestedRef.current;
    if (!refresh && requested && requested.start <= start - span * 0.1 && requested.end >= end + span * 0.1) return;
    const range = { start: start - span * 0.25, end: end + span * 0.25 };
    requestedRef.current = range;
    queue.request(range, { refresh });
  }, [queue]);

  useEffect(() => {
    queue.cancel();
    requestedRef.current = null;
    indexRef.current = null;
    setIndex(null);
    return () => {
      queue.cancel();
      requestedRef.current = null;
      indexRef.current = null;
    };
  }, [enabled, revision, queue]);

  useEffect(() => { ensure(); }, [enabled, paused, revision, viewStart, viewEnd, ensure]);
  useEffect(() => {
    if (!enabled) return undefined;
    const timer = setInterval(() => ensure(true), REFRESH_MS);
    return () => clearInterval(timer);
  }, [enabled, ensure]);

  const read = useCallback((time) => indexRef.current?.nearest(time), []);
  return { index, read };
}
