import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  fetchThumbnailBatch,
  getCategoriesFromDb,
  listProcesses,
  listRecentScreenshots,
} from '../lib/monitor_api';
import { searchSnapshotsAndRecaps } from '../lib/search_api';
import { groupSearchResults, sampleRecentCaptures } from '../lib/search_grouping';
import { clearRecentSearches, loadRecentSearches, pushRecentSearch } from '../lib/recent_searches';
import { createLatestTaskQueue } from '../lib/latest_task_queue';

const PAGE_SIZE = 40;
/** 着陆区展示的最近截图数量。 */
const LANDING_CAPTURE_COUNT = 12;
/**
 * 着陆区实际取回的条数。
 *
 * 取得比展示的多，是因为要先按来源折叠掉连续的重复捕获再挑选；实测取 200 条
 * 才够填满 12 格，而这条查询走索引，取 60 条和取 400 条都在一毫秒以内。
 */
const LANDING_FETCH_COUNT = 200;

function useDebouncedValue(value, delay = 300) {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const handle = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(handle);
  }, [value, delay]);
  return debounced;
}

export function useAdvancedSearchController({
  active,
  authenticated = true,
  recordsRevision = 0,
  searchParams,
  t,
}) {
  const incomingQuery = searchParams?.query || '';
  const sourceRefreshKey = searchParams?.refreshKey ?? 0;
  const reloadToken = `${sourceRefreshKey}:${recordsRevision}`;
  const [queryState, setQueryState] = useState(() => ({ query: incomingQuery, sourceQuery: incomingQuery, refreshKey: sourceRefreshKey }));
  // Apply a top-bar submission before either search channel can start with an old local edit.
  const query = queryState.sourceQuery === incomingQuery && queryState.refreshKey === sourceRefreshKey
    ? queryState.query : incomingQuery;
  const setQuery = useCallback((value) => {
    setQueryState({ query: value, sourceQuery: incomingQuery, refreshKey: sourceRefreshKey });
  }, [incomingQuery, sourceRefreshKey]);
  const [searchTarget, setSearchTarget] = useState('mixed');
  const [recapSemantic, setRecapSemantic] = useState(true);
  const [results, setResults] = useState([]);
  const [thumbnailCache, setThumbnailCache] = useState({});
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [error, setError] = useState(null);
  const [selectedProcesses, setSelectedProcesses] = useState([]);
  const [processOptions, setProcessOptions] = useState([]);
  const [selectedCategories, setSelectedCategories] = useState([]);
  const [categoryOptions, setCategoryOptions] = useState([]);
  const [startDate, setStartDate] = useState('');
  const [endDate, setEndDate] = useState('');
  const [rangePreset, setRangePreset] = useState('any');
  const [elapsedMs, setElapsedMs] = useState(null);
  const [resultSetKey, setResultSetKey] = useState(0);
  const [recentQueries, setRecentQueries] = useState(() => loadRecentSearches());
  const [landingCaptures, setLandingCaptures] = useState([]);
  const offsetRef = useRef(0);
  const stageRef = useRef(false);
  const rankingRevisionRef = useRef(null);
  const [semanticUpdating, setSemanticUpdating] = useState(false);
  const [semanticStatus, setSemanticStatus] = useState('empty');
  const [totals, setTotals] = useState(null);
  const useRecapSemantic = searchTarget !== 'snapshots' && recapSemantic;
  const unified = Boolean(query.trim() || searchTarget === 'recaps' || selectedProcesses.length || (searchTarget !== 'recaps' && selectedCategories.length) || startDate || endDate);
  const keywordQueue = useMemo(() => createLatestTaskQueue(), []);
  const semanticQueue = useMemo(() => createLatestTaskQueue(), []);
  const observerRef = useRef(null);
  const sentinelRef = useRef(null);
  const searchIdRef = useRef(0);
  const completedRequestRef = useRef(null);
  /**
   * 筛选选项和最近截图都是「打开面板时看一眼」的数据，来回切换面板时不必反复
   * 重查。这里记下上一次取数用的令牌，只有用户显式刷新才会让它们重新取。
   * 取空时不记令牌，免得把一次失败或一次冷启动缓存到整个会话结束。
   */
  const optionsTokenRef = useRef(null);
  const landingTokenRef = useRef(null);

  const debouncedQuery = useDebouncedValue(query, 400);
  useEffect(() => {
    searchIdRef.current += 1;
    completedRequestRef.current = null;
    keywordQueue.cancel();
    semanticQueue.cancel();
    setSemanticUpdating(false);
    setLoading(false);
    setLoadingMore(false);
    setResults([]);
    setThumbnailCache({});
    setHasMore(false);
    setTotals(null);
  }, [query, authenticated, reloadToken, searchTarget, useRecapSemantic, selectedProcesses, selectedCategories, startDate, endDate, keywordQueue, semanticQueue]);
  useEffect(() => {
    if (active) return;
    // Keep completed text results mounted while cancelling unfinished work.
    searchIdRef.current += 1;
    keywordQueue.cancel();
    semanticQueue.cancel();
    setSemanticUpdating(false);
    setLoading(false);
    setLoadingMore(false);
  }, [active, keywordQueue, semanticQueue]);
  useEffect(() => {
    if (authenticated) return;
    optionsTokenRef.current = null;
    landingTokenRef.current = null;
    setLandingCaptures([]);
    setProcessOptions([]);
    setCategoryOptions([]);
  }, [authenticated]);
  useEffect(() => () => {
    searchIdRef.current += 1;
    keywordQueue.cancel();
    semanticQueue.cancel();
  }, [keywordQueue, semanticQueue]);
  const rotatingMessages = useMemo(() => t('advancedSearch.rotating', { returnObjects: true }) || [], [t]);
  const [rotatingMessage, setRotatingMessage] = useState(() => {
    return rotatingMessages.length > 0 ? rotatingMessages[Math.floor(Math.random() * rotatingMessages.length)] : '';
  });

  useEffect(() => {
    const id = setInterval(() => {
      setRotatingMessage((prev) => {
        if (rotatingMessages.length <= 1) return prev;
        let next = prev;
        while (next === prev) {
          next = rotatingMessages[Math.floor(Math.random() * rotatingMessages.length)];
        }
        return next;
      });
    }, 5000);
    return () => clearInterval(id);
  }, [rotatingMessages]);

  useEffect(() => {
    if (!active || !authenticated) return;
    if (optionsTokenRef.current === reloadToken) return;
    let mounted = true;
    (async () => {
      const [data, cats] = await Promise.all([listProcesses(), getCategoriesFromDb()]);
      if (mounted) {
        if (data.length > 0 || cats.length > 0) optionsTokenRef.current = reloadToken;
        setProcessOptions(data);
        setCategoryOptions(cats);
      }
    })();
    return () => {
      mounted = false;
    };
  }, [active, authenticated, reloadToken]);

  const queryTokens = useMemo(() => {
    return debouncedQuery.trim()
      .split(/\s+/)
      .map((token) => token.trim())
      .filter(Boolean);
  }, [debouncedQuery]);

  /** 除关键词以外是否还有生效的筛选条件。 */
  const hasFilters = useMemo(
    () => selectedProcesses.length > 0 || (searchTarget !== 'recaps' && selectedCategories.length > 0) || Boolean(startDate) || Boolean(endDate),
    [searchTarget, selectedProcesses, selectedCategories, startDate, endDate]
  );

  const computeTimestamp = useCallback((value) => {
    if (!value) return null;
    const parsed = Date.parse(value);
    if (Number.isNaN(parsed)) return null;
    return Math.floor(parsed / 1000);
  }, []);

  const resetAndFetch = useCallback(async (force = false) => {
    if (!active || !authenticated) return;
    if (query.trim() !== debouncedQuery.trim()) return;
    const normalizedQuery = debouncedQuery.trim();
    const options = { limit: PAGE_SIZE, processNames: selectedProcesses,
      categories: searchTarget === 'recaps' ? [] : selectedCategories, target: searchTarget,
      includeSemantic: false, startTime: computeTimestamp(startDate), endTime: computeTimestamp(endDate) };
    const requestKey = JSON.stringify([normalizedQuery, options, useRecapSemantic, reloadToken]);
    if (!force && completedRequestRef.current === requestKey) return;
    if (!normalizedQuery && !hasFilters && searchTarget !== 'recaps') {
      searchIdRef.current += 1;
      setResults([]);
      setThumbnailCache({});
      setHasMore(false);
      setLoading(false);
      setLoadingMore(false);
      setError(null);
      setElapsedMs(null);
      offsetRef.current = 0;
      completedRequestRef.current = requestKey;
      return;
    }

    const currentSearchId = ++searchIdRef.current;
    completedRequestRef.current = null;
    const startedAt = performance.now();
    setLoading(true);
    setSemanticUpdating(false);
    setLoadingMore(false);
    setError(null);
    setThumbnailCache({});
    offsetRef.current = 0;
    rankingRevisionRef.current = null;
    try {
      const apply = (response, semantic) => {
        const items = response.items.map((item) => item.kind === 'snapshot'
          ? { ...item.snapshot, kind: 'snapshot' } : { kind: 'recap', recap: item.recap });
        setResults(items); setResultSetKey(currentSearchId);
        setHasMore(response.next_offset != null);
        offsetRef.current = response.next_offset ?? items.length;
        stageRef.current = semantic;
        rankingRevisionRef.current = response.ranking_revision ?? null;
        setSemanticStatus(response.semantic_status);
        setTotals({ total: response.total, snapshots: response.snapshot_count, recaps: response.recap_count, complete: response.complete });
        setElapsedMs(performance.now() - startedAt); setLoading(false);
        if (items.length) setRecentQueries(pushRecentSearch(normalizedQuery, 'ocr'));
      };
      const keywords = await keywordQueue.run(requestKey,
        (signal) => searchSnapshotsAndRecaps(normalizedQuery, { ...options, signal }));
      if (searchIdRef.current !== currentSearchId) return;
      apply(keywords, false);
      if (!normalizedQuery || !useRecapSemantic || keywords.semantic_status === 'disabled') {
        completedRequestRef.current = requestKey;
        return;
      }
      setSemanticUpdating(true);
      try {
        const combined = await semanticQueue.run(requestKey,
          (signal) => searchSnapshotsAndRecaps(normalizedQuery, { ...options, includeSemantic: true, signal }));
        if (searchIdRef.current === currentSearchId) apply(combined, true);
      } catch (error) {
        if (searchIdRef.current === currentSearchId) {
          setSemanticStatus('unavailable');
        }
      } finally {
        if (searchIdRef.current === currentSearchId) {
          setSemanticUpdating(false);
          completedRequestRef.current = requestKey;
        }
      }
      return;
    } catch (e) {
      if (e?.name === 'AbortError') return;
      console.error('Advanced search resetAndFetch failed:', e);
      if (searchIdRef.current === currentSearchId) {
        setError(e?.message || String(e));
        setResults([]);
        setHasMore(false);
        setElapsedMs(null);
      }
    } finally {
      if (searchIdRef.current === currentSearchId) {
        setLoading(false);
      }
    }
  }, [active, authenticated, query, debouncedQuery, searchTarget, useRecapSemantic, selectedProcesses, selectedCategories, startDate, endDate, computeTimestamp, hasFilters, reloadToken, keywordQueue, semanticQueue]);

  useEffect(() => {
    // Query/filter changes and explicit refresh share one trigger. A separate
    // refresh effect also reruns when resetAndFetch changes, doubling searches
    // whenever the parent supplies a refreshKey.
    resetAndFetch();
  }, [resetAndFetch, reloadToken]);

  /**
   * 着陆区的最近截图。
   *
   * 走的是专门的 listRecentScreenshots，而不是空查询的搜索回退路径：后者连接
   * ocr_results，一张含 12 个文本块的截图就能把 12 个格子全占满，而且排序键跨表
   * 用不上索引，实测要 5.3 秒并在此期间挡住 OCR 提交。
   *
   * 取回后按来源折叠再挑选，理由见 sampleRecentCaptures。
   */
  const isLanding = !debouncedQuery.trim() && !hasFilters && searchTarget !== 'recaps';
  useEffect(() => {
    if (!active || !authenticated || !isLanding) return undefined;
    if (landingTokenRef.current === reloadToken) return undefined;
    let mounted = true;
    (async () => {
      const fetched = await listRecentScreenshots(LANDING_FETCH_COUNT);
      if (!mounted) return;
      if (fetched.length > 0) landingTokenRef.current = reloadToken;
      setLandingCaptures(sampleRecentCaptures(fetched, LANDING_CAPTURE_COUNT));
    })();
    return () => { mounted = false; };
  }, [active, authenticated, isLanding, reloadToken]);

  useEffect(() => {
    if (!active || !authenticated) return undefined;
    const source = results.length > 0 ? results : landingCaptures;
    if (source.length === 0) return;
    let activeBatch = true;
    (async () => {
      const ids = source
        .map((item) => item.screenshot_id ?? item.metadata?.screenshot_id)
        .filter((id) => typeof id === 'number' && id > 0);
      const uniqueIds = [...new Set(ids)].filter((id) => !thumbnailCache[id]);
      if (uniqueIds.length === 0) return;
      const batch = await fetchThumbnailBatch(uniqueIds);
      if (activeBatch && batch) {
        setThumbnailCache((prev) => ({ ...prev, ...batch }));
      }
    })();
    return () => { activeBatch = false; };
  }, [results, landingCaptures, active, authenticated]);

  const loadMore = useCallback(async () => {
    if (!active || !authenticated || !hasMore || loadingMore || loading || semanticUpdating) return;
    const currentSearchId = searchIdRef.current;
    setLoadingMore(true);
    setError(null);
    try {
      const options = {
        limit: PAGE_SIZE, offset: offsetRef.current,
        includeSemantic: stageRef.current, target: searchTarget, processNames: selectedProcesses,
        rankingRevision: rankingRevisionRef.current,
        categories: searchTarget === 'recaps' ? [] : selectedCategories,
        startTime: computeTimestamp(startDate), endTime: computeTimestamp(endDate),
      };
      const requestKey = JSON.stringify([debouncedQuery.trim(), options, reloadToken]);
      const response = await (stageRef.current ? semanticQueue : keywordQueue).run(requestKey,
        (signal) => searchSnapshotsAndRecaps(debouncedQuery.trim(), { ...options, signal }));
      if (searchIdRef.current !== currentSearchId) return;
      const items = response.items.map((item) => item.kind === 'snapshot'
        ? { ...item.snapshot, kind: 'snapshot' } : { kind: 'recap', recap: item.recap });
      const restarted = response.offset === 0;
      setResults((previous) => {
        const prefix = restarted ? [] : previous;
        const key = (item) => item.kind === 'recap'
          ? `recap:${item.recap.date}:${item.recap.activity_id}`
          : `snapshot:${item.screenshot_id}`;
        const seen = new Set(prefix.map(key));
        return [...prefix, ...items.filter((item) => {
          const id = key(item);
          if (seen.has(id)) return false;
          seen.add(id);
          return true;
        })];
      });
      rankingRevisionRef.current = response.ranking_revision ?? null;
      if (restarted) {
        setResultSetKey(++searchIdRef.current);
        setLoadingMore(false);
      }
      setSemanticStatus(response.semantic_status);
      setTotals({ total: response.total, snapshots: response.snapshot_count, recaps: response.recap_count, complete: response.complete });
      offsetRef.current = response.next_offset ?? (response.offset ?? offsetRef.current) + items.length;
      setHasMore(response.next_offset != null);
      return;
    } catch (e) {
      if (e?.name === 'AbortError') return;
      console.error('Advanced search loadMore failed:', e);
      if (searchIdRef.current === currentSearchId) {
        setError(e?.message || String(e));
        setHasMore(false);
      }
    } finally {
      if (searchIdRef.current === currentSearchId) {
        setLoadingMore(false);
      }
    }
  }, [active, authenticated, debouncedQuery, searchTarget, selectedProcesses, selectedCategories, startDate, endDate, computeTimestamp, hasMore, loadingMore, loading, semanticUpdating, reloadToken, keywordQueue, semanticQueue]);

  useEffect(() => {
    if (!active) return;
    const node = sentinelRef.current;
    if (!node) return;
    if (observerRef.current) {
      observerRef.current.disconnect();
    }
    observerRef.current = new IntersectionObserver((entries) => {
      const [entry] = entries;
      if (entry?.isIntersecting) {
        loadMore();
      }
    }, { threshold: 0.6 });

    observerRef.current.observe(node);
    return () => {
      if (observerRef.current) {
        observerRef.current.disconnect();
      }
    };
  }, [active, loadMore, results]);

  const handleSubmit = (event) => {
    event.preventDefault();
    resetAndFetch(true);
  };

  const clearFilters = () => {
    setSelectedProcesses([]);
    setSelectedCategories([]);
    setStartDate('');
    setEndDate('');
    setRangePreset('any');
  };

  /** 时间范围的统一入口：预设档位和手填区间都从这里走。 */
  const setTimeRange = useCallback((preset, nextStartDate, nextEndDate) => {
    setRangePreset(preset);
    setStartDate(nextStartDate);
    setEndDate(nextEndDate);
  }, []);

  /** 重跑一条历史搜索。 */
  const applyRecentQuery = useCallback((entry) => {
    if (!entry?.query) return;
    setQuery(entry.query);
  }, [setQuery]);

  /** 从着陆区直接按来源筛选。 */
  const applyProcessFilter = useCallback((processName) => {
    setSelectedProcesses([processName]);
  }, []);

  const clearRecentQueries = useCallback(() => {
    setRecentQueries(clearRecentSearches());
  }, []);

  const rememberVisualQuery = useCallback((value) => {
    setRecentQueries(pushRecentSearch(value, 'ocr'));
  }, []);

  /** 相邻时间里同一窗口的重复命中折叠成一组。 */
  const groupedResults = useMemo(() => results.reduce((groups, item) => {
    if (item.kind === 'recap') return [...groups, { primary: item, duplicates: [] }];
    const previous = groups.at(-1);
    if (previous && previous.primary.kind !== 'recap') {
      const pair = groupSearchResults([previous.primary, ...previous.duplicates, item], 'ocr');
      if (pair.length === 1) return [...groups.slice(0, -1), pair[0]];
    }
    return [...groups, { primary: item, duplicates: [] }];
  }, []), [results]);

  const searchSourceDetail = useMemo(() => {
    // 来源标签带上查询，便于从预览返回综合搜索。
    const modeLabel = t('advancedSearch.modes.unified');
    const trimmed = query.trim();
    return trimmed ? `${modeLabel} · ${trimmed}` : modeLabel;
  }, [query, t]);

  return {
    query,
    setQuery,
    debouncedQuery,
    searchTarget,
    setSearchTarget,
    recapSemantic,
    setRecapSemantic,
    useRecapSemantic,
    results,
    groupedResults,
    unified,
    semanticUpdating,
    semanticStatus,
    totals,
    thumbnailCache,
    loading,
    loadingMore,
    hasMore,
    error,
    elapsedMs,
    hasFilters,
    isLanding,
    selectedProcesses,
    setSelectedProcesses,
    processOptions,
    selectedCategories,
    setSelectedCategories,
    categoryOptions,
    startDate,
    setStartDate,
    endDate,
    setEndDate,
    rangePreset,
    setTimeRange,
    recentQueries,
    applyRecentQuery,
    applyProcessFilter,
    clearRecentQueries,
    rememberVisualQuery,
    landingCaptures,
    sentinelRef,
    queryTokens,
    rotatingMessage,
    handleSubmit,
    clearFilters,
    searchSourceDetail,
    resultSetKey,
    visualStartTime: computeTimestamp(startDate),
    visualEndTime: computeTimestamp(endDate),
    refreshKey: reloadToken,
  };
}
