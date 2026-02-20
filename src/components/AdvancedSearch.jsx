import React, { useState, useEffect, useMemo, useRef, useCallback } from 'react';
import { Search, SlidersHorizontal, Filter, CalendarRange, X, Loader2, RefreshCw, Type, Image as ImageIcon } from 'lucide-react';
import { searchScreenshots, fetchImage, listProcesses } from '../lib/monitor_api';

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

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

function highlightMatches(text, tokens) {
  if (!text) return null;
  const usableTokens = tokens.filter(Boolean);
  if (usableTokens.length === 0) {
    return <span className="text-ide-muted">{text}</span>;
  }
  const pattern = new RegExp(`(${usableTokens.map(escapeRegExp).join('|')})`, 'gi');
  const lowered = usableTokens.map((t) => t.toLowerCase());
  const segments = text.split(pattern);
  return (
    <span className="text-ide-muted">
      {segments.map((segment, index) => {
        const isMatch = lowered.includes(segment.toLowerCase());
        return isMatch ? (
          <span key={`${segment}-${index}`} className="font-semibold text-blue-300">
            {segment}
          </span>
        ) : (
          <React.Fragment key={`${segment}-${index}`}>{segment}</React.Fragment>
        );
      })}
    </span>
  );
}

function ResultPreview({ item, mode, onSelect, queryTokens }) {
  const [imageSrc, setImageSrc] = useState(null);
  const [loadingImage, setLoadingImage] = useState(false);

  useEffect(() => {
    let active = true;
    const loadImage = async () => {
      if (!item) return;
      const screenshotId = item.screenshot_id ?? item.metadata?.screenshot_id;
      const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
      const targetPath = item.image_path || item.metadata?.image_path || item.path;
      if (!id && !targetPath) return;
      setLoadingImage(true);
      const dataUrl = await fetchImage(id, id ? null : targetPath);
      if (active) {
        setImageSrc(dataUrl);
        setLoadingImage(false);
      }
    };
    loadImage();
    return () => {
      active = false;
    };
  }, [item]);

  const processName = mode === 'nl' ? item.metadata?.process_name : item.process_name;
  const windowTitle = mode === 'nl' ? item.metadata?.window_title : item.window_title;
  const createdAt = item.screenshot_created_at || item.created_at || item.metadata?.created_at;
  const displayText = mode === 'nl' ? item.ocr_text : item.text;
  const normalizedText = displayText ? displayText.trim() : '';

  const formattedTimestamp = useMemo(() => {
    if (!createdAt) return null;

    if (typeof createdAt === 'number') {
      return new Date(createdAt * 1000).toLocaleString();
    }

    const candidate = typeof createdAt === 'string' ? createdAt.trim() : createdAt;
    if (!candidate) return null;

    const isoLike = typeof candidate === 'string' && candidate.includes('T') ? candidate : String(candidate).replace(' ', 'T');
    const parsed = new Date(isoLike);
    if (!Number.isNaN(parsed.getTime())) {
      return parsed.toLocaleString();
    }

    const numericValue = Number(candidate);
    if (!Number.isNaN(numericValue)) {
      return new Date(numericValue * 1000).toLocaleString();
    }

    return String(candidate);
  }, [createdAt]);

  return (
    <button
      className="w-full text-left border-b border-ide-border hover:bg-ide-hover/40 transition-colors"
      onClick={(event) => { 
        event.stopPropagation(); 
        // Normalize the item to ensure 'path' field is set for App.jsx compatibility
        const normalizedItem = {
          ...item,
          id: item.screenshot_id || item.id,
          path: item.image_path || item.metadata?.image_path || item.path,
        };
        onSelect(normalizedItem); 
      }}
    >
      <div className="flex gap-4 p-3">
        <div className="w-36 h-24 rounded border border-ide-border overflow-hidden bg-black flex items-center justify-center text-ide-muted text-xs">
          {loadingImage && <Loader2 className="w-4 h-4 animate-spin" />}
          {!loadingImage && imageSrc && (
            <img src={imageSrc} alt="Preview" className="w-full h-full object-cover" />
          )}
          {!loadingImage && !imageSrc && <span>No Image</span>}
        </div>
        <div className="flex-1 min-w-0 space-y-2">
          <div className="flex items-center justify-between gap-4">
            <span className="text-sm font-semibold text-blue-300 truncate">
              {processName || 'Unknown Process'}
            </span>
            {formattedTimestamp && (
              <span className="text-[11px] text-ide-muted whitespace-nowrap">
                {formattedTimestamp}
              </span>
            )}
          </div>
          {windowTitle && (
            <div className="text-xs font-semibold text-ide-text truncate" title={windowTitle}>
              {windowTitle}
            </div>
          )}
          <div className="text-xs leading-relaxed break-words">
            {normalizedText
              ? highlightMatches(normalizedText, queryTokens)
              : <span className="italic text-ide-muted">No OCR text</span>}
          </div>
          {mode === 'nl' && item.similarity !== undefined && (
            <div className="text-[10px] text-ide-muted">
              Similarity: {item.similarity.toFixed(2)}
            </div>
          )}
        </div>
      </div>
    </button>
  );
}

function ThumbnailCard({ item, onSelect }) {
  const [imageSrc, setImageSrc] = useState(null);
  const [loadingImage, setLoadingImage] = useState(false);

  useEffect(() => {
    let active = true;
    const loadImage = async () => {
      if (!item) return;
      const screenshotId = item.screenshot_id ?? item.metadata?.screenshot_id;
      const id = typeof screenshotId === 'number' && screenshotId > 0 ? screenshotId : null;
      const targetPath = item.image_path || item.metadata?.image_path || item.path;
      if (!id && !targetPath) return;
      setLoadingImage(true);
      const dataUrl = await fetchImage(id, id ? null : targetPath);
      if (active) {
        setImageSrc(dataUrl);
        setLoadingImage(false);
      }
    };
    loadImage();
    return () => { active = false; };
  }, [item]);

  const processName = item.metadata?.process_name;
  const similarity = item.similarity;

  const normalizedItem = {
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.metadata?.image_path || item.path,
  };

  return (
    <button
      className="group relative aspect-video overflow-hidden rounded border
                 border-ide-border bg-ide-panel hover:border-ide-accent/70
                 transition focus-visible:outline-none focus-visible:ring-2
                 focus-visible:ring-ide-accent/60"
      onClick={(event) => {
        event.stopPropagation();
        onSelect(normalizedItem);
      }}
    >
      {imageSrc ? (
        <img src={imageSrc} className="h-full w-full object-cover" loading="lazy" />
      ) : loadingImage ? (
        <div className="flex h-full w-full items-center justify-center">
          <Loader2 className="w-4 h-4 animate-spin text-ide-muted" />
        </div>
      ) : (
        <div className="flex h-full w-full items-center justify-center bg-ide-bg text-ide-muted text-xs">
          No Image
        </div>
      )}
      <div className="pointer-events-none absolute inset-0 flex flex-col justify-end
                      bg-gradient-to-t from-black/60 to-transparent opacity-0
                      transition group-hover:opacity-100 p-2">
        <span className="text-xs text-white font-semibold truncate">{processName || 'Unknown'}</span>
        {similarity !== undefined && (
          <span className="text-[10px] text-white/80">
            Similarity: {similarity.toFixed(2)}
          </span>
        )}
      </div>
    </button>
  );
}

function ProcessFilter({ processes, selected, onChange }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);

  useEffect(() => {
    const handleClick = (event) => {
      if (ref.current && !ref.current.contains(event.target)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, []);

  const toggleValue = (value) => {
    if (selected.includes(value)) {
      onChange(selected.filter((item) => item !== value));
    } else {
      onChange([...selected, value]);
    }
  };

  return (
    <div className="relative" ref={ref}>
      <button
        className="flex items-center gap-2 px-3 py-1.5 bg-ide-panel border border-ide-border rounded text-xs hover:bg-ide-hover/40"
        onClick={() => setOpen((prev) => !prev)}
        type="button"
      >
        <Filter className="w-3.5 h-3.5" />
        {selected.length > 0 ? `${selected.length} Processes` : 'All Processes'}
      </button>
      {open && (
        <div className="absolute z-30 mt-2 min-w-[220px] max-h-60 overflow-y-auto bg-ide-panel border border-ide-border rounded shadow-lg p-2 space-y-1">
          <div className="flex items-center justify-between text-[11px] text-ide-muted mb-1">
            <span>Select Processes</span>
            {selected.length > 0 && (
              <button className="text-blue-300" onClick={() => onChange([])}>Clear</button>
            )}
          </div>
          {processes.length === 0 && (
            <div className="text-xs text-ide-muted px-2 py-3">No process data</div>
          )}
          {processes.map((entry) => {
            const value = entry.process_name;
            const isChecked = selected.includes(value);
            return (
              <label
                key={value}
                className="flex items-center justify-between gap-2 text-xs px-2 py-1 rounded hover:bg-ide-hover/30 cursor-pointer"
              >
                <span className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    className="accent-blue-400"
                    checked={isChecked}
                    onChange={() => toggleValue(value)}
                  />
                  <span>{value}</span>
                </span>
                <span className="text-ide-muted text-[11px]">{entry.count}</span>
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function AdvancedSearch({ active, searchParams, onSelectResult, searchMode, onSearchModeChange }) {
  const [query, setQuery] = useState(searchParams?.query || '');
  const [mode, setMode] = useState(searchMode ?? (searchParams?.mode || 'ocr'));
  const [results, setResults] = useState([]);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [selectedProcesses, setSelectedProcesses] = useState([]);
  const [processOptions, setProcessOptions] = useState([]);
  const [startDate, setStartDate] = useState('');
  const [endDate, setEndDate] = useState('');
  const offsetRef = useRef(0);
  const observerRef = useRef(null);
  const sentinelRef = useRef(null);
  const lastParamsRef = useRef({ query: '', mode: 'ocr' });

  const debouncedQuery = useDebouncedValue(query, 400);

  const rotatingMessages = useMemo(() => [
    "此程序目前只是一个玩具！",
    "迷迭香真的很可爱！",
    "使用LLM的搜索会在将来加入！",
    "洁尔佩塔——我的洁尔佩塔！",
    "阿米娅你听我解释，我真的没有偏向佩丽卡！"
  ], []);

  const [rotatingMessage, setRotatingMessage] = useState(() => {
    return rotatingMessages[Math.floor(Math.random() * rotatingMessages.length)];
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
    let mounted = true;
    (async () => {
      const data = await listProcesses();
      if (mounted) setProcessOptions(data);
    })();
    return () => {
      mounted = false;
    };
  }, []);

  const handleModeChange = useCallback((nextMode) => {
    setMode(nextMode);
    onSearchModeChange?.(nextMode);
  }, [onSearchModeChange]);

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

  const computeTimestamp = useCallback((value) => {
    if (!value) return null;
    const parsed = Date.parse(value);
    if (Number.isNaN(parsed)) return null;
    return Math.floor(parsed / 1000);
  }, []);

  const resetAndFetch = useCallback(async () => {
    if (!active) return;
    const normalizedQuery = debouncedQuery.trim();
    const hasFilters = selectedProcesses.length > 0 || startDate || endDate;
    if (!normalizedQuery && !hasFilters) {
      setResults([]);
      setHasMore(false);
      offsetRef.current = 0;
      return;
    }

    setLoading(true);
    offsetRef.current = 0;
    const pageSize = mode === 'nl' ? NL_PAGE_SIZE : PAGE_SIZE;
    const fetched = await searchScreenshots(normalizedQuery, mode, {
      limit: pageSize,
      offset: 0,
      processNames: selectedProcesses,
      startTime: computeTimestamp(startDate),
      endTime: computeTimestamp(endDate),
      fuzzy: mode !== 'nl'
    });
    setResults(fetched);
    setHasMore(fetched.length === pageSize);
    offsetRef.current = fetched.length;
    setLoading(false);
  }, [active, debouncedQuery, mode, selectedProcesses, startDate, endDate, computeTimestamp]);

  useEffect(() => {
    resetAndFetch();
  }, [resetAndFetch]);

  useEffect(() => {
    if (!searchParams) return;
    if (!active) return;
    if (searchParams.refreshKey === undefined) return;
    resetAndFetch();
  }, [searchParams?.refreshKey, active, resetAndFetch]);

  const loadMore = useCallback(async () => {
    if (!hasMore || loadingMore || loading) return;
    setLoadingMore(true);
    const pageSize = mode === 'nl' ? NL_PAGE_SIZE : PAGE_SIZE;
    const fetched = await searchScreenshots(debouncedQuery.trim(), mode, {
      limit: pageSize,
      offset: offsetRef.current,
      processNames: selectedProcesses,
      startTime: computeTimestamp(startDate),
      endTime: computeTimestamp(endDate),
      fuzzy: mode !== 'nl'
    });
    setResults((prev) => [...prev, ...fetched]);
    setHasMore(fetched.length === pageSize);
    offsetRef.current += fetched.length;
    setLoadingMore(false);
  }, [debouncedQuery, mode, selectedProcesses, startDate, endDate, computeTimestamp, hasMore, loadingMore, loading]);

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
    setStartDate('');
    setEndDate('');
  };

  return (
    <div className={`flex flex-col flex-1 min-h-0 w-full ${active ? 'opacity-100' : 'opacity-0 pointer-events-none'} transition-opacity duration-200`}>
      <form className="border-b border-ide-border bg-ide-panel p-4" onSubmit={handleSubmit}>
        <div className="flex flex-wrap items-center gap-3">
          <div className="flex items-center bg-ide-bg border border-ide-border rounded-md overflow-hidden">
            <div className="flex items-center gap-2 px-3 border-r border-ide-border text-ide-muted text-xs uppercase">
              <Search className="w-3.5 h-3.5" />
              Advanced
            </div>
            <input
              type="text"
              className="bg-transparent text-sm px-3 py-1.5 focus:outline-none min-w-[220px]"
              placeholder={mode === 'ocr' ? 'Search OCR text...' : 'Describe image content...'}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <button type="submit" className="px-3 text-xs text-blue-300 hover:text-blue-200">Go</button>
            {query && (
              <button type="button" className="px-2 text-ide-muted hover:text-ide-text" onClick={() => setQuery('')}>
                <X className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border ${mode === 'ocr' ? 'border-blue-400 text-blue-300 bg-blue-400/10' : 'border-ide-border text-ide-muted hover:bg-ide-hover/30'}`}
              onClick={() => handleModeChange('ocr')}
            >
              <Type className="w-3.5 h-3.5" /> OCR 模式
            </button>
            <button
              type="button"
              className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border ${mode === 'nl' ? 'border-green-400 text-green-300 bg-green-400/10' : 'border-ide-border text-ide-muted hover:bg-ide-hover/30'}`}
              onClick={() => handleModeChange('nl')}
            >
              <ImageIcon className="w-3.5 h-3.5" /> 自然语言
            </button>
          </div>
          <ProcessFilter processes={processOptions} selected={selectedProcesses} onChange={setSelectedProcesses} />
          <div className="flex items-center gap-2 text-xs text-ide-muted">
            <CalendarRange className="w-3.5 h-3.5" />
            <input
              type="datetime-local"
              value={startDate}
              onChange={(event) => setStartDate(event.target.value)}
              className="bg-ide-bg border border-ide-border rounded px-2 py-1"
            />
            <span className="text-ide-muted">-</span>
            <input
              type="datetime-local"
              value={endDate}
              onChange={(event) => setEndDate(event.target.value)}
              className="bg-ide-bg border border-ide-border rounded px-2 py-1"
            />
          </div>
          <button
            type="button"
            className="flex items-center gap-1 px-3 py-1.5 text-xs border border-ide-border rounded text-ide-muted hover:text-ide-text hover:bg-ide-hover/30"
            onClick={clearFilters}
          >
            <RefreshCw className="w-3.5 h-3.5" /> 重置
          </button>
        </div>
        {mode === 'nl' && (
          <div className="text-ide-muted mt-2 text-sm">
            Results obtained using natural language image search may not accurate.
          </div>
        )}
      </form>
      <div className="flex-1 min-h-0 overflow-y-auto custom-scrollbar">
        {loading && (
          <div className="flex items-center justify-center py-8 text-ide-muted gap-2">
            <Loader2 className="w-4 h-4 animate-spin" />
            <span className="text-sm">Searching...</span>
          </div>
        )}
        {!loading && results.length === 0 && (
          <div className="flex flex-col items-center justify-center py-16 text-ide-muted gap-2 text-sm">
            {(!query.trim() && selectedProcesses.length === 0 && !startDate && !endDate) ? (
              <>
                <Search className="w-5 h-5" />
                <span>Enter a keyword or choose filters to begin.</span>
                <span className="text-xs">Press Enter in the top search bar to jump here quickly.</span>
                <span className="text-xs">你知道吗：{rotatingMessage}</span>
              </>
            ) : (
              <>
                <SlidersHorizontal className="w-5 h-5" />
                <span>No matching results</span>
                <span className="text-xs">Adjust filters or try different keywords.</span>
              </>
            )}
          </div>
        )}
        {mode === 'nl' ? (
          <div className="grid grid-cols-2 gap-2 p-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
            {results.map((item, index) => (
              <ThumbnailCard
                key={`${item.id || item.image_path || index}-${index}`}
                item={item}
                onSelect={(payload) => onSelectResult?.(payload)}
              />
            ))}
          </div>
        ) : (
          <ul className="divide-y divide-ide-border">
            {results.map((item, index) => (
              <li key={`${item.id || item.image_path || index}-${index}`}>
                <ResultPreview
                  item={item}
                  mode={mode}
                  queryTokens={queryTokens}
                  onSelect={(payload) => onSelectResult?.(payload)}
                />
              </li>
            ))}
          </ul>
        )}
        <div ref={sentinelRef} className="py-4 flex items-center justify-center text-ide-muted text-xs">
          {loadingMore && (
            <>
              <Loader2 className="w-4 h-4 animate-spin mr-2" />
              Loading more results...
            </>
          )}
          {!loadingMore && hasMore && <span>Scroll to load more…</span>}
          {!loadingMore && !hasMore && results.length > 0 && <span>No more results</span>}
        </div>
      </div>
    </div>
  );
}
