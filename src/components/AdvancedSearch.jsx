import React, { useState, useEffect, useMemo, useRef } from 'react';
import { Search, SlidersHorizontal, Filter, CalendarRange, X, Loader2, RefreshCw, Type, Image as ImageIcon, Tag, Maximize2, Eye } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { fetchImage } from '../lib/monitor_api';
import { CATEGORY_COLORS } from '../lib/categories';
import { ThumbnailCard, CategoryBadge } from './ThumbnailCard';
import { useAdvancedSearchController } from '../hooks/useAdvancedSearchController';
import { useHmacMigrationStatus } from '../hooks/useHmacMigrationStatus';

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

function ResultPreview({ item, mode, onSelect, onOpenFloatingPreview, queryTokens, preloadedSrc = null }) {
  const { t } = useTranslation();
  const [imageSrc, setImageSrc] = useState(preloadedSrc);
  const [loadingImage, setLoadingImage] = useState(!preloadedSrc);

  useEffect(() => {
    if (preloadedSrc) {
      setImageSrc(preloadedSrc);
      setLoadingImage(false);
      return;
    }
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
  }, [item, preloadedSrc]);

  const processName = mode === 'nl' ? item.metadata?.process_name : item.process_name;
  const windowTitle = mode === 'nl' ? item.metadata?.window_title : item.window_title;
  const createdAt = item.screenshot_created_at || item.created_at || item.metadata?.created_at;
  const displayText = mode === 'nl' ? item.ocr_text : item.text;
  const normalizedText = displayText ? displayText.trim() : '';
  const categoryValue = item.category || item.metadata?.category || null;

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

  const normalizedItem = {
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.metadata?.image_path || item.path,
  };

  const cardClickBehavior = localStorage.getItem('cardClickBehavior_search') || 'preview';
  const isStandaloneDefault = cardClickBehavior === 'standalone' && !!onOpenFloatingPreview;

  const handleSelect = (event) => {
    event.stopPropagation();
    if (isStandaloneDefault) {
      onOpenFloatingPreview(normalizedItem);
    } else {
      onSelect?.(normalizedItem);
    }
  };

  const handleAlternateAction = (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (isStandaloneDefault) {
      onSelect?.(normalizedItem);
    } else {
      onOpenFloatingPreview?.(normalizedItem);
    }
  };

  return (
    <div className="group relative w-full border-b border-ide-border transition-colors hover:bg-ide-hover/40 focus-within:ring-2 focus-within:ring-inset focus-within:ring-ide-accent/60">
      <button
        type="button"
        className="flex w-full gap-4 p-3 text-left focus-visible:outline-none"
        onClick={handleSelect}
      >
        <div className="w-36 h-24 rounded border border-ide-border overflow-hidden bg-black flex items-center justify-center text-ide-muted text-xs">
          {loadingImage && <Loader2 className="w-4 h-4 animate-spin" />}
          {!loadingImage && imageSrc && (
            <img src={imageSrc} alt="Preview" className="w-full h-full object-cover" />
          )}
          {!loadingImage && !imageSrc && <span>{t('advancedSearch.no_image')}</span>}
        </div>
        <div className="flex-1 min-w-0 space-y-2">
          <div className="flex items-center justify-between gap-4">
            <span className="text-sm font-semibold text-blue-300 truncate">
              {processName || t('advancedSearch.unknown')}
            </span>
            <div className="flex items-center gap-2 flex-shrink-0">
              <CategoryBadge category={categoryValue} />
              {formattedTimestamp && (
                <span className="text-[11px] text-ide-muted whitespace-nowrap">
                  {formattedTimestamp}
                </span>
              )}
            </div>
          </div>
          {windowTitle && (
            <div className="text-xs font-semibold text-ide-text truncate" title={windowTitle}>
              {windowTitle}
            </div>
          )}
          <div className="text-xs leading-relaxed break-words">
            {normalizedText
              ? highlightMatches(normalizedText, queryTokens)
              : <span className="italic text-ide-muted">{t('advancedSearch.no_ocr_text')}</span>}
          </div>
          {mode === 'nl' && item.similarity !== undefined && (
            <div className="text-[10px] text-ide-muted">
              Similarity: {item.similarity.toFixed(2)}
            </div>
          )}
        </div>
      </button>
      {onOpenFloatingPreview && (
        <button
          type="button"
          className="absolute left-[122px] top-[18px] z-10 rounded border border-ide-border bg-ide-panel/95 p-1.5 text-ide-muted opacity-0 shadow-sm transition hover:bg-ide-hover hover:text-ide-text focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70 group-hover:opacity-100"
          onClick={handleAlternateAction}
          title={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
          aria-label={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
        >
          {isStandaloneDefault ? <Eye className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />}
        </button>
      )}
    </div>
  );
}

function ProcessFilter({ processes, selected, onChange }) {
  const { t } = useTranslation();
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
        {selected.length > 0 ? t('advancedSearch.processes.count', { count: selected.length }) : t('advancedSearch.processes.all')}
      </button>
      {open && (
        <div className="absolute z-30 mt-2 min-w-[220px] max-h-60 overflow-y-auto bg-ide-panel border border-ide-border rounded shadow-lg p-2 space-y-1">
          <div className="flex items-center justify-between text-[11px] text-ide-muted mb-1">
            <span>{t('advancedSearch.processes.select')}</span>
            {selected.length > 0 && (
              <button className="text-blue-300" onClick={() => onChange([])}>{t('advancedSearch.processes.clear')}</button>
            )}
          </div>
          {processes.length === 0 && (
            <div className="text-xs text-ide-muted px-2 py-3">{t('advancedSearch.processes.no_data')}</div>
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

function CategoryFilter({ categories, selected, onChange }) {
  const { t } = useTranslation();
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
        <Tag className="w-3.5 h-3.5" />
        {selected.length > 0 ? t('advancedSearch.categories.count', { count: selected.length }) : t('advancedSearch.categories.all')}
      </button>
      {open && (
        <div className="absolute z-30 mt-2 min-w-[200px] max-h-60 overflow-y-auto bg-ide-panel border border-ide-border rounded shadow-lg p-2 space-y-1">
          <div className="flex items-center justify-between text-[11px] text-ide-muted mb-1">
            <span>{t('advancedSearch.categories.select')}</span>
            {selected.length > 0 && (
              <button className="text-blue-300" onClick={() => onChange([])}>{t('advancedSearch.categories.clear')}</button>
            )}
          </div>
          {categories.length === 0 && (
            <div className="text-xs text-ide-muted px-2 py-3">{t('advancedSearch.categories.no_data')}</div>
          )}
          {categories.map((cat) => {
            const isChecked = selected.includes(cat);
            const color = CATEGORY_COLORS[cat] || '#6b7280';
            return (
              <label
                key={cat}
                className="flex items-center gap-2 text-xs px-2 py-1 rounded hover:bg-ide-hover/30 cursor-pointer"
              >
                <input
                  type="checkbox"
                  className="accent-blue-400"
                  checked={isChecked}
                  onChange={() => toggleValue(cat)}
                />
                <span
                  className="w-2 h-2 rounded-full flex-shrink-0"
                  style={{ backgroundColor: color }}
                />
                <span>{cat}</span>
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function AdvancedSearch({ active, searchParams, onSelectResult, onOpenSnapshotPreview, searchMode, onSearchModeChange, backendOnline }) {
  const { t } = useTranslation();
  const {
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
  } = useAdvancedSearchController({
    active,
    searchParams,
    searchMode,
    onSearchModeChange,
    backendOnline,
    t,
  });
  const isMigrating = useHmacMigrationStatus();

  return (
    <div className={`flex flex-col flex-1 min-h-0 w-full ${active ? 'opacity-100' : 'opacity-0 pointer-events-none'} transition-opacity duration-200`}>
      {isMigrating && (
        <div className="p-3 bg-yellow-500/10 border border-yellow-500/20 flex  gap-4 items-start">
          <div className="p-2 bg-yellow-500/20 rounded-full text-yellow-500 shrink-0">
            <Loader2 className="w-5 h-5 animate-spin" />
          </div>
          <div className="flex flex-col gap-1">
            <h3 className="text-sm font-bold text-yellow-500">
              {t('settings.storageManagement.migration.search_unavailable_title')}
            </h3>
            <p className="text-xs text-ide-muted leading-relaxed max-w-2xl">
              {t('settings.storageManagement.migration.search_unavailable_desc')}
            </p>
          </div>
        </div>
      )}
      <form className="shrink-0 border-b border-ide-border bg-ide-panel px-4 py-2.5 space-y-2" onSubmit={handleSubmit}>
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-ide-text flex items-center gap-2">
            <Search className="w-4 h-4 text-ide-accent" />
            {t('advancedSearch.title')}
          </h2>
          <button
            type="button"
            className="flex items-center gap-1 px-3 py-1.5 text-xs border border-ide-border rounded text-ide-muted hover:text-ide-text hover:bg-ide-hover/30"
            onClick={clearFilters}
          >
            <RefreshCw className="w-3.5 h-3.5" /> 重置
          </button>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <div className="flex items-center bg-ide-bg border border-ide-border rounded-md overflow-hidden">
            <input
              type="text"
              className="bg-transparent text-sm px-3 py-1.5 focus:outline-none min-w-[260px]"
              placeholder={mode === 'ocr' ? t('advancedSearch.search.placeholder_ocr') : t('advancedSearch.search.placeholder_nl')}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <button type="submit" className="px-3 text-xs text-blue-300 hover:text-blue-200">{t('advancedSearch.search.go')}</button>
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
              <Type className="w-3.5 h-3.5" /> {t('advancedSearch.modes.ocr')}
            </button>
            <button
              type="button"
              className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border ${mode === 'nl' ? 'border-green-400 text-green-300 bg-green-400/10' : 'border-ide-border text-ide-muted hover:bg-ide-hover/30'} ${backendOnline === false ? 'opacity-50 cursor-not-allowed' : ''}`}
              onClick={() => { if (backendOnline === false) return; handleModeChange('nl'); }}
              title={backendOnline === false ? t('search.nl.disabled_hint') : ''}
            >
              <ImageIcon className="w-3.5 h-3.5" /> {t('advancedSearch.modes.nl')}
            </button>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3">
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
          <ProcessFilter processes={processOptions} selected={selectedProcesses} onChange={setSelectedProcesses} />
          {mode === 'ocr' && (
            <CategoryFilter categories={categoryOptions} selected={selectedCategories} onChange={setSelectedCategories} />
          )}
        </div>
        {mode === 'nl' && (
          <div className="text-ide-muted mt-2 text-sm">
            Results obtained using natural language image search may not accurate.
          </div>
        )}
      </form>
      <div className="flex-1 min-h-0 overflow-y-auto custom-scrollbar">
        {error && (
          <div className="p-4 mx-4 mt-3 bg-red-500/10 border border-red-500/20 rounded text-red-400 text-sm break-words shrink-0">
            {t('advancedSearch.search.error', { message: error })}
          </div>
        )}
        {loading && (
          <div className="flex items-center justify-center py-8 text-ide-muted gap-2">
            <Loader2 className="w-4 h-4 animate-spin" />
            <span className="text-sm">{t('advancedSearch.search.searching')}</span>
          </div>
        )}
        {!loading && !error && results.length === 0 && (
          <div className="flex flex-col items-center justify-center py-16 text-ide-muted gap-2 text-sm">
            {(!query.trim() && selectedProcesses.length === 0 && selectedCategories.length === 0 && !startDate && !endDate) ? (
              <>
                <Search className="w-5 h-5" />
                <span>{t('advancedSearch.search.enter_keyword')}</span>
                <span className="text-xs">{t('advancedSearch.search.press_enter')}</span>
                <span className="text-xs">{t('advancedSearch.search.did_you_know', { msg: rotatingMessage })}</span>
              </>
            ) : (
              <>
                <SlidersHorizontal className="w-5 h-5" />
                <span>{t('advancedSearch.search.no_results')}</span>
                <span className="text-xs">{t('advancedSearch.search.adjust')}</span>
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
                sourceType="search"
                onSelect={(payload) => onSelectResult?.(payload)}
                onOpenFloatingPreview={onOpenSnapshotPreview
                  ? (payload) => {
                    const id = payload.screenshot_id ?? payload.metadata?.screenshot_id;
                    onOpenSnapshotPreview(payload, {
                      thumbnailSrc: thumbnailCache[id] || null,
                      sourceLabel: t('advancedSearch.title'),
                      sourceDetail: searchSourceDetail,
                      sourceType: 'advanced-search',
                    });
                  }
                  : undefined}
                preloadedSrc={thumbnailCache[item.screenshot_id ?? item.metadata?.screenshot_id] || null}
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
                  sourceType="search"
                  queryTokens={queryTokens}
                  onSelect={(payload) => onSelectResult?.(payload)}
                  onOpenFloatingPreview={onOpenSnapshotPreview
                    ? (payload) => {
                      const id = payload.screenshot_id ?? payload.metadata?.screenshot_id;
                      onOpenSnapshotPreview(payload, {
                        thumbnailSrc: thumbnailCache[id] || null,
                        sourceLabel: t('advancedSearch.title'),
                        sourceDetail: searchSourceDetail,
                        sourceType: 'advanced-search',
                      });
                    }
                    : undefined}
                  preloadedSrc={thumbnailCache[item.screenshot_id ?? item.metadata?.screenshot_id] || null}
                />
              </li>
            ))}
          </ul>
        )}
        <div ref={sentinelRef} className="py-4 flex items-center justify-center text-ide-muted text-xs">
          {loadingMore && (
            <>
              <Loader2 className="w-4 h-4 animate-spin mr-2" />
              {t('advancedSearch.loading_more')}
            </>
          )}
          {!loadingMore && hasMore && <span>{t('advancedSearch.scroll_load')}</span>}
          {!loadingMore && !hasMore && results.length > 0 && <span>{t('advancedSearch.no_more')}</span>}
        </div>
      </div>
    </div>
  );
}
