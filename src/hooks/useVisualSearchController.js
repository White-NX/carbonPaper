import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { fetchThumbnailBatch, searchScreenshots } from '../lib/monitor_api';
import { createLatestTaskQueue } from '../lib/latest_task_queue';

export const VISUAL_SEARCH_SUMMARY_SIZE = 10;
export const VISUAL_SEARCH_PAGE_SIZE = 50;
const EMPTY_RESULTS = [];
const EMPTY_THUMBNAILS = {};
const emptyState = (key, phase = 'idle') => ({ key, phase, results: EMPTY_RESULTS,
  thumbnails: EMPTY_THUMBNAILS, offset: 0, summaryCount: VISUAL_SEARCH_SUMMARY_SIZE,
  hasMore: false, loadingMore: false, pageError: false });

function normalizeResult(item) {
  const metadata = item.metadata || {};
  const id = item.screenshot_id ?? metadata.screenshot_id ?? item.id;
  return { ...item, id, screenshot_id: id,
    image_path: item.image_path || metadata.image_path || item.path,
    process_name: item.process_name || metadata.process_name,
    window_title: item.window_title || metadata.window_title,
    created_at: item.created_at || metadata.created_at || metadata.timestamp,
    category: item.category || metadata.category || null };
}

/** Visual retrieval updates independently from the text and recap channels. */
export function useVisualSearchController({ active, query, debouncedQuery, processNames,
  categories, startTime, endTime, refreshKey, sharedThumbnails = EMPTY_THUMBNAILS }) {
  const [retryRevision, setRetryRevision] = useState(0);
  const queue = useMemo(() => createLatestTaskQueue(), []);
  const generation = useRef(0);
  const pageRequest = useRef(null);
  const normalized = query.trim();
  const ready = active && Boolean(normalized) && normalized === debouncedQuery.trim();
  const searchKey = JSON.stringify([normalized, processNames, categories, startTime, endTime, refreshKey]);
  const key = JSON.stringify([searchKey, retryRevision]);
  const [state, setState] = useState(() => emptyState(key));
  const current = ready && state.key === key ? state : emptyState(key, active && normalized ? 'loading' : 'idle');

  useEffect(() => {
    const version = ++generation.current;
    queue.cancel();
    pageRequest.current = null;
    if (!ready) {
      setState(emptyState(key));
      return () => queue.cancel();
    }
    setState(emptyState(key, 'loading'));
    const limit = VISUAL_SEARCH_SUMMARY_SIZE;
    queue.run(`${key}:0:${limit}`, (signal) => searchScreenshots(normalized, 'nl', {
      limit, offset: 0, processNames, categories, startTime, endTime, fuzzy: false, includeOcr: false, signal,
    })).then((items) => {
      if (generation.current !== version) return;
      setState({ ...emptyState(key, 'ready'), results: items.map(normalizeResult),
        offset: items.length, hasMore: items.length === limit });
    }).catch((error) => {
      if (generation.current === version && error?.name !== 'AbortError') {
        setState(emptyState(key, 'error'));
      }
    });
    return () => { generation.current += 1; queue.cancel(); };
  }, [active, ready, key, normalized, processNames, categories, startTime, endTime, queue]);

  const loadPage = useCallback(async (limit, restart = false) => {
    if (!ready || current.phase !== 'ready' || !current.hasMore || current.loadingMore || pageRequest.current) return;
    const version = generation.current;
    const offset = restart ? 0 : current.offset;
    const pageKey = `${key}:${offset}:${limit}`;
    pageRequest.current = pageKey;
    setState((previous) => ({ ...previous, loadingMore: true, pageError: false }));
    try {
      const items = await queue.run(pageKey, (signal) => searchScreenshots(normalized, 'nl', {
        limit, offset, processNames, categories, startTime, endTime,
        fuzzy: false, includeOcr: false, signal,
      }));
      if (generation.current !== version) return;
      setState((previous) => {
        const seen = new Set(restart ? [] : previous.results.map((item) => item.screenshot_id));
        const next = items.map(normalizeResult).filter((item) => {
          if (seen.has(item.screenshot_id)) return false;
          seen.add(item.screenshot_id); return true;
        });
        return { ...previous, results: restart ? next : [...previous.results, ...next], offset: offset + items.length,
          hasMore: items.length === limit, loadingMore: false };
      });
    } catch (error) {
      if (generation.current === version && error?.name !== 'AbortError') {
        setState((previous) => ({ ...previous, loadingMore: false, pageError: true }));
      }
    } finally {
      if (pageRequest.current === pageKey) pageRequest.current = null;
    }
  }, [ready, current.phase, current.hasMore, current.loadingMore, current.offset, key, normalized,
    processNames, categories, startTime, endTime, queue]);

  const loadMore = useCallback(() => loadPage(VISUAL_SEARCH_SUMMARY_SIZE), [loadPage]);
  // Establish a full first gallery page before continuing from its raw offset.
  // The summary can keep showing its ten cached results while this is in flight.
  const loadGalleryPage = useCallback(() => loadPage(VISUAL_SEARCH_PAGE_SIZE, current.offset < VISUAL_SEARCH_PAGE_SIZE),
    [loadPage, current.offset]);
  const galleryReady = current.offset >= VISUAL_SEARCH_PAGE_SIZE || !current.hasMore;

  const showMore = useCallback(() => {
    if (!ready || current.phase !== 'ready' || current.loadingMore || pageRequest.current) return;
    const nextCount = current.summaryCount > current.results.length
      ? current.summaryCount : current.summaryCount + VISUAL_SEARCH_SUMMARY_SIZE;
    if (current.results.length <= current.summaryCount && !current.hasMore) return;
    setState((previous) => ({ ...previous, summaryCount: nextCount }));
    if (current.results.length < nextCount) void loadMore();
  }, [ready, current.phase, current.loadingMore, current.summaryCount, current.results.length,
    current.hasMore, loadMore]);

  useEffect(() => {
    if (!ready || current.results.length === 0) return undefined;
    const ids = current.results.map((item) => item.screenshot_id)
      .filter((id) => typeof id === 'number' && id > 0 && current.thumbnails[id] === undefined && sharedThumbnails[id] === undefined);
    if (!ids.length) return undefined;
    const request = new AbortController();
    fetchThumbnailBatch([...new Set(ids)], { signal: request.signal }).then((thumbnails) => {
      if (request.signal.aborted) return;
      setState((previous) => previous.key === key
        ? { ...previous, thumbnails: { ...previous.thumbnails, ...Object.fromEntries(ids.map((id) => [id, thumbnails?.[id] || null])) } }
        : previous);
    }).catch(() => {});
    return () => request.abort();
  }, [ready, current.results, current.thumbnails, sharedThumbnails, key]);

  const retry = useCallback(() => {
    if (current.phase === 'error') setRetryRevision((revision) => revision + 1);
    else void loadMore();
  }, [current.phase, loadMore]);

  const thumbnails = useMemo(() => ready ? { ...sharedThumbnails, ...current.thumbnails } : EMPTY_THUMBNAILS,
    [ready, sharedThumbnails, current.thumbnails]);
  return { ...current, searchKey, thumbnails, galleryReady, loadGalleryPage, summaryResults: current.results.slice(0, current.summaryCount),
    summaryHasMore: current.results.length > current.summaryCount || current.hasMore,
    summaryPageError: current.pageError && current.results.length <= current.summaryCount, showMore, loadMore, retry };
}
