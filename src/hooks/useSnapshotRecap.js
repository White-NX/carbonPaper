import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getSnapshotRecapContext } from '../lib/recap_api';

export default function useSnapshotRecap(screenshotId, active, authenticated) {
  const [state, setState] = useState({ id: null, context: null, loading: false, error: '' });
  const refreshRef = useRef(async () => {});
  const refresh = useCallback(() => refreshRef.current(), []);
  const enabled = active && authenticated && Number.isSafeInteger(screenshotId) && screenshotId > 0;

  useEffect(() => {
    refreshRef.current = async () => {};
    if (!enabled) {
      setState({ id: null, context: null, loading: false, error: '' });
      return undefined;
    }
    let live = true;
    let inFlight = null;
    let pending = false;
    let sourceRetry = true;
    setState({ id: screenshotId, context: null, loading: true, error: '' });
    const load = () => {
      if (!live || document.hidden) return Promise.resolve();
      if (inFlight) { pending = true; return inFlight; }
      setState((previous) => ({ ...previous, loading: !previous.context, error: '' }));
      inFlight = (async () => {
        try {
          const context = await getSnapshotRecapContext(screenshotId);
          if (live) {
            sourceRetry = true;
            setState({ id: screenshotId, context, loading: false, error: '' });
          }
        } catch (error) {
          if (!live) return;
          const retry = sourceRetry && String(error).includes('RECAP_SOURCE_CHANGED');
          sourceRetry = false;
          pending ||= retry;
          setState({ id: screenshotId, context: null, loading: retry, error: retry ? '' : String(error) });
        } finally {
          inFlight = null;
          if (pending && live) { pending = false; queueMicrotask(load); }
        }
      })();
      return inFlight;
    };
    refreshRef.current = load;
    // Fast timeline scrubbing should not decrypt a recap for every passed frame.
    const initial = setTimeout(load, 120);
    const timer = setInterval(load, 15000);
    const subscription = listen('recap-changed', load);
    subscription.catch((error) => console.warn('Failed to listen for recap changes:', error));
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => {
      live = false;
      refreshRef.current = async () => {};
      clearTimeout(initial);
      clearInterval(timer);
      subscription.then((unlisten) => unlisten()).catch(() => {});
      window.removeEventListener('focus', load);
      document.removeEventListener('visibilitychange', load);
    };
  }, [screenshotId, enabled]);

  const current = enabled && state.id === screenshotId;
  return {
    context: current ? state.context : null,
    loading: current && state.loading,
    error: current ? state.error : '',
    refresh,
  };
}
