import { useCallback, useEffect, useRef, useState } from 'react';
import { getUsageAppDetails, getUsageDay, moveUsageDate, setUsageAppCategory } from '../lib/usage_api';
import { listen } from '@tauri-apps/api/event';
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

const DATE_REPEAT_WINDOW_MS = 70;
const navigationDate = ({ requestedDate, day, error }) => error ? day?.date ?? requestedDate : requestedDate;

export default function useUsageStatistics(active, authenticated, transitionDate) {
  const [today, setToday] = useState(localDate);
  const [overview, setOverview] = useState(() => ({ requestedDate: today, day: null, loading: true, error: '' }));
  const [selectedId, setSelectedId] = useState(null);
  const current = useRef(overview);
  const requestRef = useRef(null);
  const refreshCategoryRef = useRef(null);
  const previousToday = useRef(today);

  useEffect(() => {
    const publish = (next) => {
      current.current = next;
      setOverview(next);
    };
    if (!authenticated) {
      publish({ ...current.current, day: null, loading: true, error: '' });
      setSelectedId(null);
      return undefined;
    }
    if (!active) return undefined;

    let live = true;
    let inFlight = null;
    let selectionTimer = null;
    let refreshCategoriesPending = false;
    const load = () => {
      const selection = current.current;
      if (!live || document.hidden || inFlight || selectionTimer !== null) return inFlight;
      if (!selection.loading && !refreshCategoriesPending) return undefined;
      const { requestedDate, day: previousDay, error: previousError } = selection;
      // A category refresh after a navigation error updates only the displayed day.
      const date = selection.loading ? requestedDate : previousDay?.date ?? requestedDate;
      refreshCategoriesPending = false;
      const isCurrent = () => live && current.current === selection;

      // The native read cannot be canceled. Keep only the latest selection waiting behind it.
      inFlight = (async () => {
        try {
          const day = await getUsageDay(date);
          if (!isCurrent()) return;
          if (day?.date !== date) throw new Error('USAGE_QUERY_FAILED');
          const commit = () => {
            if (!isCurrent()) return false;
            if (previousDay?.date !== day.date || previousDay.generation !== day.generation) setSelectedId(null);
            publish({ requestedDate, day, loading: false, error: previousError });
            return true;
          };
          if (transitionDate && previousDay && previousDay.date !== day.date) {
            await transitionDate(previousDay, day, commit);
          } else {
            commit();
          }
        } catch (error) {
          if (isCurrent()) {
            const message = String(error);
            const locked = /\bAUTH_REQUIRED\b/.test(message);
            if (locked) {
              refreshCategoriesPending = false;
              setSelectedId(null);
            }
            publish({ ...selection, day: locked ? null : previousDay, loading: false, error: locked ? message : previousError || message });
          }
        }
      })().finally(() => {
        inFlight = null;
        return load();
      });
      return inFlight;
    };
    const request = (requestedDate, { refresh = false, coalesce = false } = {}) => {
      const previous = current.current;
      if (previous.loading && previous.requestedDate === requestedDate) return load();
      const alreadyDisplayed = previous.day?.date === requestedDate;
      if (!refresh && alreadyDisplayed && !previous.loading && !previous.error) return;
      publish({ ...previous, requestedDate, loading: refresh || !alreadyDisplayed, error: '' });
      clearTimeout(selectionTimer);
      selectionTimer = null;
      if (coalesce && !alreadyDisplayed) {
        selectionTimer = setTimeout(() => {
          selectionTimer = null;
          load();
        }, DATE_REPEAT_WINDOW_MS);
        return;
      }
      return load();
    };
    const refreshCategories = () => {
      if (!live) return undefined;
      refreshCategoriesPending = true;
      return load();
    };
    const poll = () => {
      const now = localDate();
      if (now !== previousToday.current) {
        const followToday = current.current.requestedDate === previousToday.current;
        previousToday.current = now;
        setToday(now);
        if (followToday) {
          request(now);
          return;
        }
      }
      const { day, requestedDate, loading, error } = current.current;
      if (document.hidden || inFlight) return;
      if (loading || refreshCategoriesPending) load();
      else if (!error || !day || requestedDate === day.date) request(requestedDate, { refresh: true });
    };

    requestRef.current = request;
    refreshCategoryRef.current = refreshCategories;
    poll();
    const timer = setInterval(poll, 15000);
    window.addEventListener('focus', poll);
    document.addEventListener('visibilitychange', poll);
    let unlisten;
    listen('usage-categories-changed', () => refreshCategories())
      .then((stop) => { if (live) unlisten = stop; else stop(); }).catch(() => {});
    return () => {
      live = false;
      requestRef.current = null;
      refreshCategoryRef.current = null;
      clearTimeout(selectionTimer);
      clearInterval(timer);
      window.removeEventListener('focus', poll);
      document.removeEventListener('visibilitychange', poll);
      unlisten?.();
    };
  }, [active, authenticated, transitionDate]);

  const { requestedDate, error, loading } = overview;
  const day = authenticated ? overview.day : null;
  const date = day?.date ?? requestedDate;
  const selectedApp = day?.apps.find((app) => app.id === selectedId) ?? null;
  const appId = selectedApp?.id || null;
  const readDetails = useCallback(() => getUsageAppDetails(date, appId), [date, appId]);
  const details = useUsageRead(appId ? `${date}:${appId}` : null, active, authenticated, readDetails);
  const selectDate = useCallback((value, options) => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value) || value > localDate()) return;
    requestRef.current?.(value, options);
  }, []);
  const stepDate = useCallback((step) => {
    selectDate(moveUsageDate(navigationDate(current.current), step), { coalesce: true });
  }, [selectDate]);
  const selectApp = useCallback((id) => setSelectedId((current) => current === id ? null : id), []);
  const clearApp = useCallback(() => setSelectedId(null), []);
  const refresh = useCallback(() => {
    const promise = requestRef.current?.(current.current.requestedDate, { refresh: true });
    if (appId) details.refresh();
    return promise;
  }, [details.refresh, appId]);
  const saveCategory = useCallback(async (id, category, generation) => {
    if (!active || !authenticated) throw new Error('AUTH_REQUIRED');
    if (!day || day.generation !== generation) throw new Error('USAGE_SOURCE_CHANGED');
    if (!day.apps.some((app) => app.id === id)) throw new Error('USAGE_INVALID_APP');
    const refreshCategories = refreshCategoryRef.current;
    await setUsageAppCategory(id, category, generation);
    if (refreshCategoryRef.current === refreshCategories) await refreshCategories?.();
  }, [active, authenticated, day]);
  return { date, today, day, requestedDate, navigationDate: navigationDate(overview),
    loading: active && authenticated && loading, error: authenticated ? error : '', refresh, selectDate, stepDate,
    selectedApp, selectApp, clearApp, saveCategory,
    details: details.value?.date === date && details.value?.app_id === appId ? details.value : null,
    detailsError: details.error, detailsLoading: details.loading };
}
