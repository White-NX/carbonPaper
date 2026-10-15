import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { cancelRecapSearchIndex, getRecapSearchStatus, prepareRecapSearch, searchRecaps } from '../lib/recap_api';

const initialFilters = { query: '', mode: 'hybrid', start_date: '', end_date: '', include_details: true };
const requestFor = (filters, offset = 0) => ({ ...filters, query: filters.query.trim(), start_date: filters.start_date || null, end_date: filters.end_date || null, offset, limit: 20 });

/** Owns request generations, index progress and invalidation separately from
 * rendering. Late searches cannot restore protected or superseded results. */
export default function useRecapSearch(active, authenticated) {
  const [filters, setFilters] = useState(initialFilters);
  const [result, setResult] = useState(null);
  const [status, setStatus] = useState(null);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [error, setError] = useState('');
  const [indexError, setIndexError] = useState('');
  const [changed, setChanged] = useState(false);
  const [submitted, setSubmitted] = useState(null);
  const values = useRef(filters);
  values.current = filters;
  const resultRef = useRef(result);
  resultRef.current = result;
  const submittedRef = useRef(submitted);
  submittedRef.current = submitted;
  const enabled = useRef(false);
  enabled.current = active && authenticated;
  const sequence = useRef(0);
  const lifetime = useRef(0);
  const refreshStatus = useRef(async () => {});

  const run = useCallback(async (request, append = false) => {
    if (!enabled.current || !request.query) return;
    const token = ++sequence.current;
    setError(''); setChanged(false);
    setLoading(!append); setLoadingMore(append);
    if (!append) { setResult(null); setSubmitted(request); }
    try {
      const response = await searchRecaps(request);
      if (token !== sequence.current || !enabled.current) return;
      if (append && (resultRef.current?.revision !== response.revision
        || resultRef.current?.ranking_revision !== response.ranking_revision)) {
        setChanged(true); setResult(null); return;
      }
      setResult((previous) => append && previous ? { ...response, items: [...previous.items, ...response.items] } : response);
    } catch (cause) {
      if (token === sequence.current && enabled.current) {
        if (String(cause).includes('RECAP_SEARCH_RESULTS_CHANGED') || String(cause).includes('RECAP_SOURCE_CHANGED')) {
          setChanged(true); setResult(null);
        } else { setError(String(cause)); if (!append) setResult(null); }
      }
    } finally {
      if (token === sequence.current) { setLoading(false); setLoadingMore(false); }
    }
  }, []);

  useEffect(() => {
    sequence.current += 1;
    lifetime.current += 1;
    setLoading(false); setLoadingMore(false); setPreparing(false);
    if (!authenticated) {
      setFilters(initialFilters); setResult(null); setStatus(null); setSubmitted(null);
      setError(''); setIndexError(''); setChanged(false);
    }
    if (!active || !authenticated) return undefined;
    let live = true;
    let inFlight = false;
    let pending = false;
    let previousRevision = null;
    const load = async () => {
      if (!live || document.hidden) return;
      if (inFlight) { pending = true; return; }
      inFlight = true;
      try {
        const next = await getRecapSearchStatus();
        if (!live) return;
        if ((previousRevision && previousRevision !== next.revision)
          || (resultRef.current && resultRef.current.revision !== next.revision)) {
          sequence.current += 1;
          setResult(null); setChanged(true); setLoading(false); setLoadingMore(false);
        }
        previousRevision = next.revision;
        setStatus(next); setIndexError(next.error || '');
      } catch (cause) {
        if (live) { sequence.current += 1; setResult(null); setStatus(null); setLoading(false); setLoadingMore(false); setIndexError(String(cause)); }
      }
      finally { inFlight = false; if (pending && live) { pending = false; load(); } }
    };
    refreshStatus.current = load;
    const invalidate = () => {
      sequence.current += 1;
      setResult(null); setChanged(true); setLoading(false); setLoadingMore(false);
      load();
    };
    const subscriptions = [listen('recap-search-progress', load), listen('recap-changed', invalidate)];
    Promise.all(subscriptions).then(load).catch((cause) => { if (live) setIndexError(String(cause)); });
    const timer = setInterval(load, 15000);
    window.addEventListener('focus', load);
    document.addEventListener('visibilitychange', load);
    return () => {
      live = false; sequence.current += 1; lifetime.current += 1;
      refreshStatus.current = async () => {};
      clearInterval(timer);
      window.removeEventListener('focus', load);
      document.removeEventListener('visibilitychange', load);
      subscriptions.forEach((subscription) => subscription.then((unlisten) => unlisten()).catch(() => {}));
    };
  }, [active, authenticated]);

  const update = useCallback((patch) => {
    const next = { ...values.current, ...patch };
    values.current = next; setFilters(next);
    if (!('query' in patch) && submittedRef.current) {
      if (next.start_date && next.end_date && next.start_date > next.end_date) {
        sequence.current += 1; setResult(null); setLoading(false); setLoadingMore(false);
      } else run(requestFor(next));
    }
  }, [run]);
  const search = useCallback(() => run(requestFor(values.current)), [run]);
  const loadMore = () => {
    if (result?.next_offset == null || loading || loadingMore || !submitted) return;
    run({ ...submitted, offset: result.next_offset, ...(result.ranking_revision ? { ranking_revision: result.ranking_revision } : {}) }, true);
  };
  const prepare = async () => {
    if (preparing || status?.running) return;
    const token = lifetime.current;
    setPreparing(true); setIndexError('');
    try {
      await prepareRecapSearch();
      if (token === lifetime.current && enabled.current) {
        await refreshStatus.current();
        if (submitted && values.current.query.trim() === submitted.query) run({ ...submitted, offset: 0 });
      }
    } catch (cause) {
      if (token === lifetime.current && !String(cause).includes('AI_CANCELLED')) setIndexError(String(cause));
    } finally {
      if (token === lifetime.current) { setPreparing(false); refreshStatus.current(); }
    }
  };
  const stop = async () => {
    try { await cancelRecapSearchIndex(); }
    catch (cause) { if (enabled.current) setIndexError(String(cause)); }
  };
  return { filters, update, result, status, loading, loadingMore, error, indexError, changed, submitted,
    search, loadMore, prepare, stop, preparing: preparing || Boolean(status?.running) };
}
