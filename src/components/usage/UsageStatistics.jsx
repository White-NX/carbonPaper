import React, { useCallback, useLayoutEffect, useRef } from 'react';
import { flushSync } from 'react-dom';
import useUsageStatistics from '../../hooks/useUsageStatistics';
import UsageStatisticsView from './UsageStatisticsView';

export default function UsageStatistics({ active, isAuthenticated, onViewSnapshots }) {
  const surface = useRef(null);
  const animation = useRef(null);
  const transitionDate = useCallback(async (from, to, commit) => {
    const element = surface.current;
    if (!element?.startViewTransition || window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) {
      commit();
      return;
    }

    animation.current?.skipTransition();
    let transition;
    try {
      // Capture only this view. The date arrows stay live during playback.
      transition = element.startViewTransition({
        types: [to < from ? 'usage-earlier' : 'usage-later'],
        update: () => { if (!flushSync(commit)) transition.skipTransition(); },
      });
    } catch {
      commit();
      return;
    }
    animation.current = transition;
    transition.ready.catch(() => {}); // A superseding selection can skip the capture.
    const clear = () => { if (animation.current === transition) animation.current = null; };
    transition.finished.then(clear, clear);
    await transition.updateCallbackDone;
  }, []);
  useLayoutEffect(() => () => animation.current?.skipTransition(), [active, isAuthenticated]);

  const statistics = useUsageStatistics(active, isAuthenticated, transitionDate);
  if (!active || !isAuthenticated) return null;
  return <UsageStatisticsView {...statistics} surfaceRef={surface} onViewSnapshots={onViewSnapshots} />;
}
