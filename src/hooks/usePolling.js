import { useEffect, useRef } from 'react';

/**
 * Run `tick` now and then again every `intervalMs` until it returns `false`,
 * the component unmounts, or `enabled` turns off.
 *
 * Uses a setTimeout chain rather than setInterval so a slow backend call is
 * never overlapped by the next one. Changing `intervalMs` or `enabled`
 * restarts the chain; `tick` itself is read through a ref so callers do not
 * have to memoise it.
 */
export function usePolling(tick, { intervalMs, enabled = true }) {
  const tickRef = useRef(tick);
  tickRef.current = tick;

  useEffect(() => {
    if (!enabled) return undefined;
    let cancelled = false;
    let timer;
    const run = async () => {
      let keepGoing = true;
      try {
        keepGoing = (await tickRef.current()) !== false;
      } catch {
        // A failed read is retried on the next tick; callers that want to stop
        // on failure catch it themselves and return false.
      }
      if (cancelled || !keepGoing) return;
      timer = setTimeout(run, intervalMs);
    };
    run();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [enabled, intervalMs]);
}
