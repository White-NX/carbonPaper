import React from 'react';
import { Search, X, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { ThumbnailCard } from './ThumbnailCard';
import { SearchModeTabs } from './search/SearchModeTabs';
import { TimeRangeChip, ProcessChip, CategoryChip } from './search/SearchFilters';
import { SearchStatsBar } from './search/SearchStatsBar';
import { SearchResultRow } from './search/SearchResultRow';
import { SearchLanding, SearchNoResults } from './search/SearchLanding';
import { PageHeader } from './PageHeader';
import { useAdvancedSearchController } from '../hooks/useAdvancedSearchController';
import { useHmacMigrationStatus } from '../hooks/useHmacMigrationStatus';
import { buildSearchTimelineMarkers, searchResultMarkerId } from '../lib/timeline_search';

export function AdvancedSearch({
  active,
  searchParams,
  onSelectResult,
  onOpenSnapshotPreview,
  searchMode,
  onSearchModeChange,
  onTimelineSearchChange,
}) {
  const { t } = useTranslation();
  const {
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
    endDate,
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
  } = useAdvancedSearchController({
    active,
    searchParams,
    searchMode,
    onSearchModeChange,
    t,
  });
  const isMigrating = useHmacMigrationStatus();
  const [hoveredMarkerIds, setHoveredMarkerIds] = React.useState([]);

  const hasResults = results.length > 0;
  const showStats = hasResults && !loading;
  const timelineMarkers = React.useMemo(
    () => (mode === 'ocr' && query.trim() ? buildSearchTimelineMarkers(results) : []),
    [mode, query, results],
  );

  React.useEffect(() => {
    setHoveredMarkerIds([]);
  }, [resultSetKey, mode]);

  React.useEffect(() => {
    const shouldShow = active
      && mode === 'ocr'
      && Boolean(query.trim())
      && !loading
      && !error
      && timelineMarkers.length > 0;

    onTimelineSearchChange?.(shouldShow ? {
      markers: timelineMarkers,
      hoveredIds: hoveredMarkerIds,
      fitKey: `ocr:${resultSetKey}`,
    } : null);
  }, [active, mode, query, loading, error, timelineMarkers, hoveredMarkerIds, resultSetKey, onTimelineSearchChange]);

  React.useEffect(() => () => onTimelineSearchChange?.(null), [onTimelineSearchChange]);

  const handleHoverResults = React.useCallback((items) => {
    setHoveredMarkerIds(items ? [...new Set(items.map((item, index) => searchResultMarkerId(item, index)))] : []);
  }, []);

  /** 把结果条目交给主预览区或独立预览窗，两处都要带上来源信息。 */
  const openFloatingPreview = onOpenSnapshotPreview
    ? (payload) => {
      const id = payload.screenshot_id ?? payload.metadata?.screenshot_id;
      onOpenSnapshotPreview(payload, {
        thumbnailSrc: thumbnailCache[id] || null,
        sourceLabel: t('advancedSearch.title'),
        sourceDetail: searchSourceDetail,
        sourceType: 'advanced-search',
      });
    }
    : undefined;

  const renderBody = () => {
    if (loading) {
      return (
        <div className="flex items-center justify-center gap-2 py-16 text-ide-muted">
          <Loader2 className="h-4 w-4 animate-spin" />
          <span className="text-sm">{t('advancedSearch.search.searching')}</span>
        </div>
      );
    }

    if (!hasResults) {
      return isLanding ? (
        <SearchLanding
          recentQueries={recentQueries}
          onUseQuery={applyRecentQuery}
          onClearRecent={clearRecentQueries}
          processes={processOptions}
          onUseProcess={applyProcessFilter}
          latest={landingCaptures}
          thumbnailCache={thumbnailCache}
          onSelectLatest={(item) => onSelectResult?.({
            ...item,
            id: item.screenshot_id || item.id,
            path: item.image_path || item.metadata?.image_path || item.path,
          })}
          eggMessage={rotatingMessage}
        />
      ) : (
        <SearchNoResults
          query={query.trim()}
          mode={mode}
          hasFilters={hasFilters}
          onClearFilters={clearFilters}
          onSwitchToVisual={() => handleModeChange('nl')}
        />
      );
    }

    if (mode === 'nl') {
      return (
        <div className="grid grid-cols-2 gap-3 p-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
          {results.map((item, index) => (
            <ThumbnailCard
              key={`${item.id || item.image_path || index}-${index}`}
              item={item}
              sourceType="search"
              onSelect={(payload) => onSelectResult?.(payload)}
              onOpenFloatingPreview={openFloatingPreview}
              preloadedSrc={thumbnailCache[item.screenshot_id ?? item.metadata?.screenshot_id] || null}
            />
          ))}
        </div>
      );
    }

    return (
      <div className="flex flex-col gap-0.5 px-4 py-2">
        {groupedResults.map((group, index) => (
          <SearchResultRow
            key={`${group.primary.id || group.primary.image_path || index}-${index}`}
            group={group}
            mode={mode}
            queryTokens={queryTokens}
            thumbnailCache={thumbnailCache}
            onSelect={(payload) => onSelectResult?.(payload)}
            onOpenFloatingPreview={openFloatingPreview}
            onHoverResults={handleHoverResults}
          />
        ))}
      </div>
    );
  };

  return (
    <div
      className={`flex min-h-0 w-full flex-1 flex-col ${active ? 'opacity-100' : 'pointer-events-none opacity-0'} transition-opacity duration-200`}
    >
      {isMigrating && (
        <div className="flex items-start gap-4 border border-yellow-500/20 bg-yellow-500/10 p-3">
          <div className="shrink-0 rounded-full bg-yellow-500/20 p-2 text-yellow-500">
            <Loader2 className="h-5 w-5 animate-spin" />
          </div>
          <div className="flex flex-col gap-1">
            <h3 className="text-sm font-bold text-yellow-500">
              {t('settings.storageManagement.migration.search_unavailable_title')}
            </h3>
            <p className="max-w-2xl text-xs leading-relaxed text-ide-muted">
              {t('settings.storageManagement.migration.search_unavailable_desc')}
            </p>
          </div>
        </div>
      )}

      <PageHeader
        as="form"
        onSubmit={handleSubmit}
        bordered={!showStats}
        flushBottom
        secondaryRow={(
          <>
            <SearchModeTabs mode={mode} onChange={handleModeChange} />

            <div className="flex flex-1 flex-wrap items-center gap-2">
              <TimeRangeChip
                preset={rangePreset}
                startDate={startDate}
                endDate={endDate}
                onChange={setTimeRange}
              />
              <ProcessChip
                processes={processOptions}
                selected={selectedProcesses}
                onChange={setSelectedProcesses}
              />
              {mode === 'ocr' && (
                <CategoryChip
                  categories={categoryOptions}
                  selected={selectedCategories}
                  onChange={setSelectedCategories}
                />
              )}
              {hasFilters && (
                <button
                  type="button"
                  onClick={clearFilters}
                  className="ml-auto flex h-[30px] items-center gap-1.5 rounded-full px-3 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  {t('advancedSearch.filters.reset')}
                </button>
              )}
            </div>
          </>
        )}
      >
        <div className="flex h-10 w-full max-w-[620px] items-center gap-1 rounded-lg border border-ide-border bg-ide-bg pl-3.5 pr-1.5 transition-colors focus-within:border-ide-accent focus-within:ring-2 focus-within:ring-ide-accent/20">
          <input
            type="text"
            className="min-w-0 flex-1 bg-transparent text-sm text-ide-text outline-none placeholder:text-ide-muted"
            placeholder={mode === 'ocr'
              ? t('advancedSearch.search.placeholder_ocr')
              : t('advancedSearch.search.placeholder_nl')}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          {query && (
            <button
              type="button"
              onClick={() => setQuery('')}
              title={t('advancedSearch.processes.clear')}
              className="grid h-7 w-7 place-items-center rounded-md text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
          <button
            type="submit"
            title={t('advancedSearch.search.go')}
            aria-label={t('advancedSearch.search.go')}
            className="grid h-[30px] w-[30px] shrink-0 place-items-center rounded-md bg-ide-accent text-white transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60"
          >
            <Search className="h-4 w-4" />
          </button>
        </div>
      </PageHeader>

      {showStats && (
        <SearchStatsBar count={results.length} elapsedMs={elapsedMs} hasMore={hasMore} mode={mode} />
      )}

      {mode === 'nl' && !isLanding && (
        <p className="shrink-0 border-b border-ide-border bg-ide-panel px-6 pb-2 text-[11px] leading-relaxed text-ide-muted">
          {t('advancedSearch.nl_notice')}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto custom-scrollbar">
        {error && (
          <div className="mx-4 mt-3 shrink-0 break-words rounded border border-red-500/20 bg-red-500/10 p-4 text-sm text-red-400">
            {t('advancedSearch.search.error', { message: error })}
          </div>
        )}

        {renderBody()}

        <div ref={sentinelRef} className="flex items-center justify-center py-4 text-xs text-ide-muted">
          {loadingMore && (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              {t('advancedSearch.loading_more')}
            </>
          )}
          {!loadingMore && !hasMore && hasResults && <span>{t('advancedSearch.no_more')}</span>}
        </div>
      </div>
    </div>
  );
}
