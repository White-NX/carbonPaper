import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getRecapDay, getRecapSettings, listRecapDays } from '../lib/recap_api';

const LIGHT_VIEW = { includeRecords: false, includeAttempts: false };

export default function useRecapDay(date, active, authenticated) {
  const [state, setState] = useState({ date, day: null, settings: null, days: [], error: '' });
  const refreshRef = useRef(async () => {});
  const refresh = useCallback(() => refreshRef.current(), []);
  useEffect(() => {
    if (!authenticated) {
      setState({ date, day: null, settings: null, days: [], error: '' });
      return undefined;
    }
    if (!active) return undefined;
    let live = true;
    let inFlight = null;
    let pending = false;
    setState((previous) => previous.date === date ? previous : { ...previous, date, day: null, error: '' });
    const load = () => {
      if (!live || document.hidden) return Promise.resolve();
      if (inFlight) { pending = true; return inFlight; }
      inFlight = (async () => {
        try {
          const [day, settings, days] = await Promise.all([getRecapDay(date, LIGHT_VIEW), getRecapSettings(), listRecapDays()]);
          if (live) setState({ date, day, settings, days, error: '' });
        } catch (error) {
          if (live) setState((previous) => ({ ...previous, day: null, error: String(error) }));
        } finally {
          inFlight = null;
          if (pending && live) { pending = false; load(); }
        }
      })();
      return inFlight;
    };
    refreshRef.current = load;
    const subscription = listen('recap-changed', load);
    subscription.then(load).catch((error) => { if (live) setState((previous) => ({ ...previous, error: String(error) })); });
    const timer = setInterval(load, 15000);
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => {
      live = false;
      refreshRef.current = async () => {};
      clearInterval(timer);
      window.removeEventListener('focus', load);
      document.removeEventListener('visibilitychange', load);
      subscription.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [date, active, authenticated]);
  return { ...state, day: authenticated && state.date === date ? state.day : null, refresh };
}
