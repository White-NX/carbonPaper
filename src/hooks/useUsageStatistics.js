import { useCallback, useEffect, useRef, useState } from 'react';
import { getUsageAppDetails, getUsageDay } from '../lib/usage_api';
import { localDate } from '../lib/recap_api';

function useUsageRead(key, active, authenticated, read) {
  const [state, setState] = useState({ key, value: null, error: '', loading: false });
  const refreshRef = useRef(async () => {});
  const refresh = useCallback(() => refreshRef.current(), []);
  useEffect(() => {
    if (!authenticated) {
      setState({ key, value: null, error: '', loading: false });
      return undefined;
    }
    if (!active || !key) return undefined;
    let live = true;
    let inFlight = null;
    let pending = false;
    const load = () => {
      if (!live || document.hidden) return Promise.resolve();
      if (inFlight) { pending = true; return inFlight; }
      setState((old) => ({ key, value: old.key === key ? old.value : null, error: '', loading: true }));
      inFlight = (async () => {
        try {
          const value = await read();
          if (live) setState({ key, value, error: '', loading: false });
        } catch (error) {
          if (live) setState({ key, value: null, error: String(error), loading: false });
        } finally {
          inFlight = null;
          if (live && pending) { pending = false; load(); }
        }
      })();
      return inFlight;
    };
    refreshRef.current = load;
    load();
    const timer = setInterval(load, 15000);
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => {
      live = false;
      refreshRef.current = async () => {};
      clearInterval(timer);
      window.removeEventListener('focus', load);
      document.removeEventListener('visibilitychange', load);
    };
  }, [key, active, authenticated, read]);
  const current = authenticated && state.key === key;
  return { value: current ? state.value : null, error: current ? state.error : '', loading: active && (!current || state.loading), refresh };
}

export default function useUsageStatistics(active, authenticated) {
  const [date, setDate] = useState(localDate);
  const [today, setToday] = useState(localDate);
  const [selectedId, setSelectedId] = useState(null);
  const previousToday = useRef(today);
  useEffect(() => {
    if (!active) return undefined;
    const tick = () => {
      const current = localDate();
      if (current !== previousToday.current) {
        const previous = previousToday.current;
        setDate((selected) => selected === previous ? current : selected);
        previousToday.current = current;
        setToday(current);
      }
    };
    tick();
    const timer = setInterval(tick, 15000);
    return () => clearInterval(timer);
  }, [active]);
  const readDay = useCallback(() => getUsageDay(date), [date]);
  const overview = useUsageRead(date, active, authenticated, readDay);
  const day = overview.value?.date === date ? overview.value : null;
  const selectedApp = day?.apps.find((app) => app.id === selectedId) || null;
  const appId = selectedApp?.id || null;
  const readDetails = useCallback(() => getUsageAppDetails(date, appId), [date, appId]);
  const details = useUsageRead(appId ? `${date}:${appId}` : null, active, authenticated, readDetails);
  const selectDate = useCallback((value) => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value) || value > localDate()) return;
    setSelectedId(null);
    setDate(value);
  }, []);
  const selectApp = useCallback((id) => setSelectedId((current) => current === id ? null : id), []);
  const refresh = useCallback(() => { overview.refresh(); if (appId) details.refresh(); }, [overview.refresh, details.refresh, appId]);
  return { date, today, day, loading: overview.loading, error: overview.error, refresh, selectDate,
    selectedApp, selectApp, clearApp: () => setSelectedId(null),
    details: details.value?.date === date && details.value?.app_id === appId ? details.value : null,
    detailsError: details.error, detailsLoading: details.loading };
}
