import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  batchGetCategories,
  fetchThumbnailBatch,
  getCategoriesFromDb,
  listProcesses,
  searchScreenshots,
} from '../lib/monitor_api';

const PAGE_SIZE = 40;
const NL_PAGE_SIZE = 100;

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
  backendOnline,
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
  const offsetRef = useRef(0);
  const observerRef = useRef(null);
  const sentinelRef = useRef(null);
  const lastParamsRef = useRef({ query: '', mode: 'ocr' });
  const searchIdRef = useRef(0);

  const debouncedQuery = useDebouncedValue(query, 400);
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
    let mounted = true;
    (async () => {
      const [data, cats] = await Promise.all([listProcesses(), getCategoriesFromDb()]);
      if (mounted) {
        setProcessOptions(data);
        setCategoryOptions(cats);
      }
    })();
    return () => {
      mounted = false;
    };
  }, [active]);

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
    if (backendOnline === false && mode === 'nl') {
      handleModeChange('ocr');
    }
  }, [backendOnline, mode, handleModeChange]);

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
    const hasFilters = selectedProcesses.length > 0 || selectedCategories.length > 0 || startDate || endDate;
    if (!normalizedQuery && !hasFilters) {
      searchIdRef.current += 1;
      setResults([]);
      setThumbnailCache({});
      setHasMore(false);
      setLoading(false);
      setLoadingMore(false);
      setError(null);
      offsetRef.current = 0;
      return;
    }

    const currentSearchId = ++searchIdRef.current;
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
      setHasMore(fetched.length === pageSize);
      offsetRef.current = fetched.length;
    } catch (e) {
      console.error('Advanced search resetAndFetch failed:', e);
      if (searchIdRef.current === currentSearchId) {
        setError(e.message || String(e));
        setResults([]);
        setHasMore(false);
      }
    } finally {
      if (searchIdRef.current === currentSearchId) {
        setLoading(false);
      }
    }
  }, [active, debouncedQuery, mode, selectedProcesses, selectedCategories, startDate, endDate, computeTimestamp]);

  useEffect(() => {
    resetAndFetch();
  }, [resetAndFetch]);

  useEffect(() => {
    if (!searchParams) return;
    if (!active) return;
    if (searchParams.refreshKey === undefined) return;
    resetAndFetch();
  }, [searchParams?.refreshKey, active, resetAndFetch]);

  useEffect(() => {
    if (results.length === 0) return;
    let activeBatch = true;
    (async () => {
      const ids = results
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
  }, [results]);

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
  };

  const searchSourceDetail = useMemo(() => {
    const modeLabel = mode === 'nl' ? t('advancedSearch.modes.nl') : t('advancedSearch.modes.ocr');
    const trimmed = query.trim();
    return trimmed ? `${modeLabel} · ${trimmed}` : modeLabel;
  }, [mode, query, t]);

  return {
    query,
    setQuery,
    mode,
    results,
    thumbnailCache,
    loading,
    loadingMore,
    hasMore,
    error,
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
    sentinelRef,
    queryTokens,
    rotatingMessage,
    handleModeChange,
    handleSubmit,
    clearFilters,
    searchSourceDetail,
  };
}
