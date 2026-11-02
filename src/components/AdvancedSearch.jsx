import React from 'react';
import { Search, X, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { SearchModeTabs } from './search/SearchModeTabs';
import { TimeRangeChip, ProcessChip, CategoryChip, SearchTargetChip, RecapSemanticToggle } from './search/SearchFilters';
import { SearchStatsBar } from './search/SearchStatsBar';
import { SearchResultRow } from './search/SearchResultRow';
import { SearchLanding, SearchNoResults } from './search/SearchLanding';
import { AiSearchPanel } from './search/AiSearchPanel';
import { useAiSearch } from '../hooks/useAiSearch';
import { PageHeader } from './PageHeader';
import { useAdvancedSearchController } from '../hooks/useAdvancedSearchController';
import { useHmacMigrationStatus } from '../hooks/useHmacMigrationStatus';
import { buildSearchTimelineMarkers, searchResultMarkerId } from '../lib/timeline_search';
import { RecapSearchResultDetails, RecapSearchResultRow } from './search/RecapSearchResult';
import { useRecapSourceAction } from './recap/RecapSources';
import { VisualSearchCard } from './search/VisualSearchCard';
import { useVisualSearchController } from '../hooks/useVisualSearchController';
import { AiModeButton } from './search/AiModeButton';
import { AiSearchLanding } from './search/AiSearchLanding';
import { Button } from './ui/Button';

export function AdvancedSearch({
  active,
  searchParams,
  onSelectResult,
  onOpenSnapshotPreview,
  onTimelineSearchChange,
  onOpenRecapDay,
  isAuthenticated = true,
}) {
  const { t } = useTranslation();
  const [aiActive, setAiActive] = React.useState(false);
  const [aiDraft, setAiDraft] = React.useState('');
  const inputRef = React.useRef(null);
  const [landingSnapshot, setLandingSnapshot] = React.useState(null);
  const previousAiMode = React.useRef(false);
  const aiMode = aiActive && isAuthenticated;
  React.useEffect(() => {
    setAiActive(false);
    setAiDraft('');
    setLandingSnapshot(null);
  }, [searchParams?.query, searchParams?.refreshKey]);
  React.useEffect(() => {
    if (!active || !isAuthenticated) {
      setAiActive(false);
      setAiDraft('');
      setLandingSnapshot(null);
    }
  }, [active, isAuthenticated]);
  const {
    query,
    setQuery,
    debouncedQuery,
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
    rememberVisualQuery,
    landingCaptures,
    sentinelRef,
    queryTokens,
    rotatingMessage,
    handleSubmit,
    clearFilters,
    searchSourceDetail,
    resultSetKey,
    totals,
    unified,
    semanticUpdating,
    semanticStatus,
    searchTarget,
    setSearchTarget,
    recapSemantic,
    setRecapSemantic,
    useRecapSemantic,
    visualStartTime,
    visualEndTime,
    refreshKey,
  } = useAdvancedSearchController({
    active: active && isAuthenticated && !aiMode,
    searchParams,
    t,
  });
  const isMigrating = useHmacMigrationStatus();
  const ai = useAiSearch({ active: active && isAuthenticated && aiMode, isAuthenticated });
  const aiConversation = aiMode && ai.status !== 'idle';
  const canAskAi = Boolean(ai.settings && ai.provider);
  const showVisual = isAuthenticated && Boolean(query.trim()) && searchTarget !== 'recaps' && !isMigrating;
  const visual = useVisualSearchController({
    active: active && isAuthenticated && !aiMode && showVisual,
    query, debouncedQuery, processNames: selectedProcesses, categories: selectedCategories,
    startTime: visualStartTime, endTime: visualEndTime, refreshKey,
    sharedThumbnails: thumbnailCache,
  });
  const [gallerySearchKey, setGallerySearchKey] = React.useState(null);
  const visualGallery = showVisual && gallerySearchKey === visual.searchKey;
  const showSearchControls = !aiMode && (Boolean(query.trim()) || hasFilters || searchTarget === 'recaps');
  const showLandingStage = isAuthenticated && (aiMode || isLanding);
  const landingThumbnails = React.useMemo(() => active && isAuthenticated
    ? { ...landingSnapshot?.thumbnailCache, ...thumbnailCache } : {}, [active, isAuthenticated, landingSnapshot, thumbnailCache]);
  React.useLayoutEffect(() => {
    if (active && isAuthenticated && !aiConversation && (aiMode || previousAiMode.current)) {
      inputRef.current?.focus({ preventScroll: true });
    }
    previousAiMode.current = aiMode;
  }, [active, isAuthenticated, aiMode, aiConversation]);
  const scrollRef = React.useRef(null);
  const scrollPositions = React.useRef({ key: null, summary: 0, gallery: 0, wasGallery: false });
  React.useEffect(() => {
    setGallerySearchKey(null);
  }, [active, isAuthenticated, showVisual, visual.searchKey]);
  React.useLayoutEffect(() => {
    const previous = scrollPositions.current;
    if (previous.key !== visual.searchKey) {
      scrollPositions.current = { key: visual.searchKey, summary: 0, gallery: 0, wasGallery: false };
    }
    const scroll = scrollRef.current;
    if (scroll) {
      scroll.scrollTop = scrollPositions.current[visualGallery ? 'gallery' : 'summary'];
      if (visualGallery) scroll.querySelector('.visual-search-back')?.focus({ preventScroll: true });
      else if (previous.wasGallery && previous.key === visual.searchKey) {
        scroll.querySelector('.visual-search-open')?.focus({ preventScroll: true });
      }
    }
    scrollPositions.current.wasGallery = visualGallery;
  }, [visualGallery, visual.searchKey]);
  const openVisualGallery = () => {
    scrollPositions.current.summary = scrollRef.current?.scrollTop || 0;
    setGallerySearchKey(visual.searchKey);
  };
  const closeVisualGallery = () => {
    scrollPositions.current.gallery = scrollRef.current?.scrollTop || 0;
    setGallerySearchKey(null);
  };
  const hasVisualResults = visual.results.length > 0;
  React.useEffect(() => {
    if (visual.phase === 'ready' && hasVisualResults) rememberVisualQuery(debouncedQuery.trim());
  }, [visual.phase, visual.key, hasVisualResults, debouncedQuery, rememberVisualQuery]);
  const handleTabChange = (next) => {
    if (next === 'ai' && !aiMode) {
      // Hold the outgoing thumbnails while their retrieval controller pauses.
      setLandingSnapshot(isLanding ? { recentQueries, processOptions, landingCaptures, thumbnailCache, rotatingMessage } : null);
      if (!aiDraft.trim()) setAiDraft(query.trim());
    }
    setGallerySearchKey(null);
    setAiActive(next === 'ai');
  };
  const tabs = <SearchModeTabs mode={aiMode ? 'ai' : 'unified'} onChange={handleTabChange} />;
  const submitAi = (event) => {
    event.preventDefault();
    if (!active || !isAuthenticated || !canAskAi || ai.status !== 'idle' || !aiDraft.trim()) return;
    void ai.start(aiDraft.trim());
    setAiDraft('');
  };
  const [hoveredMarkerIds, setHoveredMarkerIds] = React.useState([]);
  const [selectedRecap, setSelectedRecap] = React.useState(null);
  const currentRecap = selectedRecap && results.find((item) => item.kind === 'recap'
    && item.recap.date === selectedRecap.date && item.recap.activity_id === selectedRecap.activity_id)?.recap;
  React.useEffect(() => {
    if (selectedRecap && !currentRecap) setSelectedRecap(null);
  }, [selectedRecap, currentRecap]);
  const recapAction = useRecapSourceAction(onSelectResult, onOpenSnapshotPreview);
  const recapTime = React.useCallback((ms) => new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).format(new Date(ms)), []);

  const hasResults = results.length > 0;
  const showStats = hasResults && !loading;
  const summaryStatus = unified && semanticUpdating ? t('advancedSearch.semanticUpdating')
    : unified && useRecapSemantic && ['disabled', 'not_prepared', 'partial', 'unavailable'].includes(semanticStatus)
      ? t('advancedSearch.semanticFallback') : undefined;
  const timelineMarkers = React.useMemo(
    () => (query.trim() ? buildSearchTimelineMarkers(results) : []),
    [query, results],
  );

  React.useEffect(() => {
    setHoveredMarkerIds([]);
  }, [resultSetKey]);

  React.useEffect(() => {
    const shouldShow = active
      && isAuthenticated && !aiMode && !visualGallery
      && Boolean(query.trim())
      && !loading
      && !error
      && timelineMarkers.length > 0;

    onTimelineSearchChange?.(shouldShow ? {
      markers: timelineMarkers,
      hoveredIds: hoveredMarkerIds,
      fitKey: `ocr:${resultSetKey}`,
    } : null);
  }, [active, isAuthenticated, aiMode, visualGallery, query, loading, error, timelineMarkers, hoveredMarkerIds, resultSetKey, onTimelineSearchChange]);

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

  const openVisualPreview = onOpenSnapshotPreview ? (payload) => {
    const id = payload.screenshot_id ?? payload.metadata?.screenshot_id;
    onOpenSnapshotPreview(payload, {
      thumbnailSrc: visual.thumbnails[id] || null,
      sourceLabel: t('advancedSearch.title'),
      sourceDetail: `${t('advancedSearch.visual.title')} · ${query.trim()}`,
      sourceType: 'advanced-search',
    });
  } : undefined;

  const renderLanding = () => {
    const snapshot = aiMode ? landingSnapshot : null;
    return <SearchLanding
      recentQueries={snapshot?.recentQueries ?? recentQueries}
      onUseQuery={applyRecentQuery}
      onClearRecent={clearRecentQueries}
      processes={snapshot?.processOptions ?? processOptions}
      onUseProcess={applyProcessFilter}
      latest={snapshot?.landingCaptures ?? landingCaptures}
      thumbnailCache={landingThumbnails}
      onSelectLatest={(item) => onSelectResult?.({
        ...item,
        id: item.screenshot_id || item.id,
        path: item.image_path || item.metadata?.image_path || item.path,
      })}
      eggMessage={snapshot?.rotatingMessage ?? rotatingMessage}
    />;
  };

  const renderBody = () => {
    if (!isAuthenticated) return null;
    if (loading) {
      return (
        <div className="flex items-center justify-center gap-2 py-16 text-ide-muted">
          <Loader2 className="h-4 w-4 animate-spin" />
          <span className="text-sm">{t('advancedSearch.search.searching')}</span>
        </div>
      );
    }

    if (!hasResults) {
      return (
        <SearchNoResults
          query={query.trim()}
          hasFilters={hasFilters}
          onClearFilters={clearFilters}
          compact={showVisual}
        />
      );
    }

    return (
      <div className="flex flex-col gap-0.5 px-4 py-2">
        {groupedResults.map((group, index) => group.primary.kind === 'recap' ? (
          <RecapSearchResultRow key={`recap:${group.primary.recap.date}:${group.primary.recap.activity_id}`}
            item={group.primary.recap} queryTokens={queryTokens} time={recapTime} action={recapAction}
            onDetails={() => setSelectedRecap(group.primary.recap)} onOpenDay={onOpenRecapDay} />
        ) : (
          <SearchResultRow
            key={`${group.primary.id || group.primary.image_path || index}-${index}`}
            group={group}
            mode="ocr"
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

  const pageClassName = `flex min-h-0 w-full flex-1 flex-col ${active ? 'opacity-100' : 'pointer-events-none opacity-0'} transition-opacity duration-200`;

  if (aiConversation) {
    return (
      <div className={pageClassName}>
        <AiSearchPanel controller={ai} tabs={tabs} onSelectResult={onSelectResult} header={PageHeader} />
      </div>
    );
  }

  return (
    <div
      className={pageClassName}
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
        onSubmit={aiMode ? submitAi : handleSubmit}
        bordered={showSearchControls}
        flushBottom={showSearchControls}
        className={showSearchControls ? '' : '!bg-ide-bg pt-8'}
        secondaryRow={showSearchControls ? (
          <div className="flex w-full flex-wrap items-center gap-x-7 gap-y-2">
            {tabs}

            <div className="flex w-full flex-wrap items-center gap-2 lg:w-auto lg:flex-1">
              <>
                <SearchTargetChip value={searchTarget} onChange={setSearchTarget} />
                {searchTarget !== 'snapshots' && <RecapSemanticToggle checked={recapSemantic} onChange={setRecapSemantic} />}
              </>
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
              {searchTarget !== 'recaps' && (
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
          </div>
        ) : undefined}
      >
        <div className={`flex min-h-10 w-full max-w-[620px] items-center gap-1 rounded-lg border border-ide-border bg-ide-bg pl-3.5 pr-1.5 transition-colors focus-within:border-ide-accent focus-within:ring-2 focus-within:ring-ide-accent/20 ${showSearchControls ? '' : 'mx-auto'}`}>
          <Search className="mr-2 h-4 w-4 shrink-0 text-ide-muted" aria-hidden="true" />
          {aiMode ? <textarea
            ref={inputRef}
            rows={1}
            aria-label={t('aiSearch.landing.placeholder')}
            placeholder={t('aiSearch.landing.placeholder')}
            value={active ? aiDraft : ''}
            onChange={(event) => setAiDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) {
                event.preventDefault();
                submitAi(event);
              }
            }}
            className="custom-scrollbar block max-h-[108px] min-h-9 min-w-0 flex-1 resize-none overflow-y-auto bg-transparent py-1.5 text-sm leading-6 text-ide-text outline-none placeholder:text-ide-muted [field-sizing:content]"
          /> : <input
            ref={inputRef}
            type="text"
            className="min-w-0 flex-1 bg-transparent text-sm text-ide-text outline-none placeholder:text-ide-muted"
            placeholder={t(searchTarget === 'mixed' ? 'advancedSearch.search.placeholder_unified' : `advancedSearch.search.placeholder_${searchTarget}`)}
            value={isAuthenticated ? query : ''}
            onChange={(event) => setQuery(event.target.value)}
          />}
          {!showSearchControls && <AiModeButton active={aiMode} disabled={!active || !isAuthenticated}
            onClick={() => handleTabChange(aiMode ? 'unified' : 'ai')} />}
          {(aiMode ? aiDraft : query) && <Button size="xs" variant="ghost" icon={<X className="h-3.5 w-3.5" aria-hidden="true" />}
            onClick={() => aiMode ? setAiDraft('') : setQuery('')} title={t('advancedSearch.processes.clear')}
            aria-label={t('advancedSearch.processes.clear')} className="h-7 w-7 p-0" />}
          <Button type="submit" size="xs" variant="primary" icon={<Search className="h-4 w-4" aria-hidden="true" />}
            title={t(aiMode ? 'aiSearch.send' : 'advancedSearch.search.go')}
            aria-label={t(aiMode ? 'aiSearch.send' : 'advancedSearch.search.go')}
            disabled={!active || !isAuthenticated || (aiMode && (!canAskAi || !aiDraft.trim()))}
            className="h-[30px] w-[30px] p-0" />
        </div>
      </PageHeader>

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto custom-scrollbar">
        {showLandingStage ? <div className="search-landing-stage" data-mode={aiMode ? 'ai' : 'unified'}>
          <div className="search-landing-view" aria-hidden={aiMode} inert={aiMode ? '' : undefined}>
            {renderLanding()}
          </div>
          <div className="search-landing-view" aria-hidden={!aiMode} inert={!aiMode ? '' : undefined}>
            <AiSearchLanding loading={aiMode && !ai.settings} needsSetup={aiMode && Boolean(ai.settings) && !ai.provider}
              selectedQuestion={aiDraft} onChooseQuestion={(question) => {
                setAiDraft(question);
                inputRef.current?.focus({ preventScroll: true });
              }} />
          </div>
        </div> : <div className="search-overview" hidden={visualGallery}>
          {showStats && (
            <SearchStatsBar count={totals?.complete === false ? results.length : totals?.total ?? results.length}
              totalKnown={Boolean(totals) && totals.complete !== false} elapsedMs={elapsedMs}
              updating={unified && semanticUpdating} statusMessage={summaryStatus} />
          )}
          {!showStats && summaryStatus && <p role="status" className="sr-only">{summaryStatus}</p>}
          {showVisual && <VisualSearchCard controller={visual}
            onOpenGallery={openVisualGallery}
            onSelectResult={(payload) => onSelectResult?.(payload)} onOpenFloatingPreview={openVisualPreview} />}
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
        </div>}
        {visualGallery && <VisualSearchCard controller={visual} fullPage scrollRef={scrollRef}
          onBack={closeVisualGallery} onSelectResult={(payload) => onSelectResult?.(payload)}
          onOpenFloatingPreview={openVisualPreview} />}
      </div>
      {currentRecap && active && isAuthenticated && <RecapSearchResultDetails item={currentRecap} action={recapAction} time={recapTime}
        onClose={() => setSelectedRecap(null)} onOpenDay={(date) => { setSelectedRecap(null); onOpenRecapDay?.(date); }} />}
    </div>
  );
}
