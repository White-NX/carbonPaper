import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  batchGetCategories,
  fetchThumbnailBatch,
  getCategoriesFromDb,
  listProcesses,
  listRecentScreenshots,
  searchScreenshots,
} from '../lib/monitor_api';
import { groupSearchResults, sampleRecentCaptures } from '../lib/search_grouping';
import { clearRecentSearches, loadRecentSearches, pushRecentSearch } from '../lib/recent_searches';

const PAGE_SIZE = 40;
const NL_PAGE_SIZE = 100;
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
  searchParams,
  searchMode,
  onSearchModeChange,
  t,
}) {
  const [query, setQuery] = useState(searchParams?.query || '');
  const [mode, setMode] = useState(searchMode ?? (searchParams?.mode || 'ocr'));
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
  const observerRef = useRef(null);
  const sentinelRef = useRef(null);
  const lastParamsRef = useRef({ query: '', mode: 'ocr' });
  const searchIdRef = useRef(0);
  /**
   * 筛选选项和最近截图都是「打开面板时看一眼」的数据，来回切换面板时不必反复
   * 重查。这里记下上一次取数用的令牌，只有用户显式刷新才会让它们重新取。
   * 取空时不记令牌，免得把一次失败或一次冷启动缓存到整个会话结束。
   */
  const optionsTokenRef = useRef(null);
  const landingTokenRef = useRef(null);

  const debouncedQuery = useDebouncedValue(query, 400);
  /** 重取的判据：只有用户显式刷新时它才变化。 */
  const reloadToken = searchParams?.refreshKey ?? 0;
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
    if (!active) return;
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
  }, [active, reloadToken]);

  const handleModeChange = useCallback((nextMode) => {
    if (nextMode === mode) return;
    searchIdRef.current += 1;
    setResults([]);
    setThumbnailCache({});
    setHasMore(false);
    setLoading(false);
    setLoadingMore(false);
    setError(null);
    offsetRef.current = 0;
    if (searchMode === undefined) {
      setMode(nextMode);
    }
    onSearchModeChange?.(nextMode);
  }, [onSearchModeChange, mode, searchMode]);

  useEffect(() => {
    if (searchMode === undefined || searchMode === mode) return;
    setMode(searchMode);
  }, [searchMode, mode]);

  useEffect(() => {
    if (!searchParams) return;
    const { query: nextQuery = '', mode: nextMode = 'ocr' } = searchParams;
    const prev = lastParamsRef.current;
    if (prev.query !== nextQuery) {
      setQuery(nextQuery);
    }
    if (prev.mode !== nextMode) {
      handleModeChange(nextMode);
    }
    lastParamsRef.current = { query: nextQuery, mode: nextMode };
  }, [searchParams, handleModeChange]);

  const queryTokens = useMemo(() => {
    return debouncedQuery.trim()
      .split(/\s+/)
      .map((token) => token.trim())
      .filter(Boolean);
  }, [debouncedQuery]);

  /** 除关键词以外是否还有生效的筛选条件。 */
  const hasFilters = useMemo(
    () => selectedProcesses.length > 0 || selectedCategories.length > 0 || Boolean(startDate) || Boolean(endDate),
    [selectedProcesses, selectedCategories, startDate, endDate]
  );

  const computeTimestamp = useCallback((value) => {
    if (!value) return null;
    const parsed = Date.parse(value);
    if (Number.isNaN(parsed)) return null;
    return Math.floor(parsed / 1000);
  }, []);

  const enrichNlCategories = async (fetched, currentSearchId) => {
    if (mode !== 'nl' || fetched.length === 0) return;
    const hashes = fetched
      .map((item) => (item.image_path || '').replace('memory://', ''))
      .filter(Boolean);
    if (hashes.length === 0) return;
    const categoryMap = await batchGetCategories(hashes);
    if (searchIdRef.current === currentSearchId) {
      for (const item of fetched) {
        const hash = (item.image_path || '').replace('memory://', '');
        if (hash && categoryMap[hash] !== undefined) {
          item.category = categoryMap[hash];
        }
      }
    }
  };

  const resetAndFetch = useCallback(async () => {
    if (!active) return;
    const normalizedQuery = debouncedQuery.trim();
    if (!normalizedQuery && !hasFilters) {
      searchIdRef.current += 1;
      setResults([]);
      setThumbnailCache({});
      setHasMore(false);
      setLoading(false);
      setLoadingMore(false);
      setError(null);
      setElapsedMs(null);
      offsetRef.current = 0;
      return;
    }

    const currentSearchId = ++searchIdRef.current;
    const startedAt = performance.now();
    setLoading(true);
    setLoadingMore(false);
    setError(null);
    setThumbnailCache({});
    offsetRef.current = 0;
    const pageSize = mode === 'nl' ? NL_PAGE_SIZE : PAGE_SIZE;
    try {
      const fetched = await searchScreenshots(normalizedQuery, mode, {
        limit: pageSize,
        offset: 0,
        processNames: selectedProcesses,
        categories: mode === 'ocr' ? selectedCategories : [],
        startTime: computeTimestamp(startDate),
        endTime: computeTimestamp(endDate),
        fuzzy: mode !== 'nl',
      });
      if (searchIdRef.current !== currentSearchId) return;

      await enrichNlCategories(fetched, currentSearchId);

      if (searchIdRef.current !== currentSearchId) return;
      setResults(fetched);
      setResultSetKey(currentSearchId);
      setHasMore(fetched.length === pageSize);
      setElapsedMs(performance.now() - startedAt);
      offsetRef.current = fetched.length;

      // 只记下真正搜到东西的查询，免得输入过程中的半截词污染历史。
      if (normalizedQuery && fetched.length > 0) {
        setRecentQueries(pushRecentSearch(normalizedQuery, mode));
      }
    } catch (e) {
      console.error('Advanced search resetAndFetch failed:', e);
      if (searchIdRef.current === currentSearchId) {
        setError(e.message || String(e));
        setResults([]);
        setHasMore(false);
        setElapsedMs(null);
      }
    } finally {
      if (searchIdRef.current === currentSearchId) {
        setLoading(false);
      }
    }
  }, [active, debouncedQuery, mode, selectedProcesses, selectedCategories, startDate, endDate, computeTimestamp, hasFilters]);

  useEffect(() => {
    resetAndFetch();
  }, [resetAndFetch]);

  useEffect(() => {
    if (!searchParams) return;
    if (!active) return;
    if (searchParams.refreshKey === undefined) return;
    resetAndFetch();
  }, [searchParams?.refreshKey, active, resetAndFetch]);

  /**
   * 着陆区的最近截图。
   *
   * 走的是专门的 listRecentScreenshots，而不是空查询的搜索回退路径：后者连接
   * ocr_results，一张含 12 个文本块的截图就能把 12 个格子全占满，而且排序键跨表
   * 用不上索引，实测要 5.3 秒并在此期间挡住 OCR 提交。
   *
   * 取回后按来源折叠再挑选，理由见 sampleRecentCaptures。
   */
  const isLanding = !debouncedQuery.trim() && !hasFilters;
  useEffect(() => {
    if (!active || !isLanding) return undefined;
    if (landingTokenRef.current === reloadToken) return undefined;
    let mounted = true;
    (async () => {
      const fetched = await listRecentScreenshots(LANDING_FETCH_COUNT);
      if (!mounted) return;
      if (fetched.length > 0) landingTokenRef.current = reloadToken;
      setLandingCaptures(sampleRecentCaptures(fetched, LANDING_CAPTURE_COUNT));
    })();
    return () => { mounted = false; };
  }, [active, isLanding, reloadToken]);

  useEffect(() => {
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
  }, [results, landingCaptures]);

  const loadMore = useCallback(async () => {
    if (!hasMore || loadingMore || loading) return;
    const currentSearchId = searchIdRef.current;
    setLoadingMore(true);
    setError(null);
    const pageSize = mode === 'nl' ? NL_PAGE_SIZE : PAGE_SIZE;
    try {
      const fetched = await searchScreenshots(debouncedQuery.trim(), mode, {
        limit: pageSize,
        offset: offsetRef.current,
        processNames: selectedProcesses,
        categories: mode === 'ocr' ? selectedCategories : [],
        startTime: computeTimestamp(startDate),
        endTime: computeTimestamp(endDate),
        fuzzy: mode !== 'nl',
      });
      if (searchIdRef.current !== currentSearchId) return;

      await enrichNlCategories(fetched, currentSearchId);

      if (searchIdRef.current !== currentSearchId) return;
      setResults((prev) => [...prev, ...fetched]);
      setHasMore(fetched.length === pageSize);
      offsetRef.current += fetched.length;
    } catch (e) {
      console.error('Advanced search loadMore failed:', e);
      if (searchIdRef.current === currentSearchId) {
        setError(e.message || String(e));
        setHasMore(false);
      }
    } finally {
      if (searchIdRef.current === currentSearchId) {
        setLoadingMore(false);
      }
    }
  }, [debouncedQuery, mode, selectedProcesses, selectedCategories, startDate, endDate, computeTimestamp, hasMore, loadingMore, loading]);

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
    resetAndFetch();
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
    if (entry.mode && entry.mode !== mode) {
      handleModeChange(entry.mode);
    }
  }, [mode, handleModeChange]);

  /** 从着陆区直接按来源筛选。 */
  const applyProcessFilter = useCallback((processName) => {
    setSelectedProcesses([processName]);
  }, []);

  const clearRecentQueries = useCallback(() => {
    setRecentQueries(clearRecentSearches());
  }, []);

  /** 相邻时间里同一窗口的重复命中折叠成一组。 */
  const groupedResults = useMemo(() => groupSearchResults(results, mode), [results, mode]);

  const searchSourceDetail = useMemo(() => {
    // 预览面板的来源标签用完整说法，「文字搜索 · 关键词」比「文字 · 关键词」清楚。
    const modeLabel = mode === 'nl' ? t('advancedSearch.modes.nl_full') : t('advancedSearch.modes.ocr_full');
    const trimmed = query.trim();
    return trimmed ? `${modeLabel} · ${trimmed}` : modeLabel;
  }, [mode, query, t]);

  return {
    query,
    setQuery,
    mode,
    results,
    groupedResults,
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
    landingCaptures,
    sentinelRef,
    queryTokens,
    rotatingMessage,
    handleModeChange,
    handleSubmit,
    clearFilters,
    searchSourceDetail,
    resultSetKey,
  };
}
