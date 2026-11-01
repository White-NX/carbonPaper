import React, { useEffect, useId, useMemo, useRef } from 'react';
import { ArrowLeft, ChevronDown, ChevronRight, Image as ImageIcon, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { VISUAL_SEARCH_PAGE_SIZE, VISUAL_SEARCH_SUMMARY_SIZE } from '../../hooks/useVisualSearchController';
import { ThumbnailCard } from '../ThumbnailCard';
import { Button, focusStyle } from '../ui/Button';

/** Inline visual results lead into a gallery under the same search. */
export function VisualSearchCard({ controller, onSelectResult, onOpenFloatingPreview,
  onOpenGallery, onBack, fullPage = false, scrollRef }) {
  const { t, i18n } = useTranslation();
  const regionId = useId();
  const sentinelRef = useRef(null);
  const { phase, results, summaryResults, hasMore, summaryHasMore,
    loadingMore, pageError, summaryPageError, retry, loadGalleryPage, galleryReady, showMore } = controller;
  const waiting = phase === 'loading' || phase === 'idle';
  const visibleResults = fullPage ? results : summaryResults;
  const canShowMore = fullPage ? hasMore : summaryHasMore;
  const pagingError = fullPage ? pageError : summaryPageError;
  const pageSize = fullPage ? VISUAL_SEARCH_PAGE_SIZE : VISUAL_SEARCH_SUMMARY_SIZE;
  const dateFormatter = useMemo(() => new Intl.DateTimeFormat(i18n?.language, {
    month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
  }), [i18n?.language]);
  const timeFormatter = useMemo(() => new Intl.DateTimeFormat(i18n?.language, {
    hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
  }), [i18n?.language]);
  const resultTime = (item) => {
    const timestamp = typeof item.created_at === 'number' ? item.created_at * 1000 : Date.parse(item.created_at);
    const date = new Date(timestamp);
    if (!Number.isFinite(date.getTime())) return null;
    const today = new Date();
    const short = date.toDateString() === today.toDateString() ? timeFormatter.format(date)
      : date.getFullYear() === today.getFullYear() ? dateFormatter.format(date)
        : date.toLocaleString(i18n?.language, { year: 'numeric', month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
    return { short, full: date.toLocaleString(i18n?.language), iso: date.toISOString() };
  };

  useEffect(() => {
    if (fullPage && phase === 'ready' && !galleryReady && !loadingMore && !pageError) void loadGalleryPage();
  }, [fullPage, phase, galleryReady, loadingMore, pageError, loadGalleryPage]);

  useEffect(() => {
    if (!fullPage || phase !== 'ready' || !galleryReady || !hasMore || loadingMore || pageError
      || typeof IntersectionObserver === 'undefined') return undefined;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void loadGalleryPage();
    }, { root: scrollRef?.current || null, rootMargin: '200px' });
    observer.observe(sentinelRef.current);
    return () => observer.disconnect();
  }, [fullPage, phase, galleryReady, hasMore, loadingMore, pageError, loadGalleryPage, scrollRef]);

  return (
    <section className={`visual-search-section mx-6 mt-4 ${fullPage ? 'mb-6' : 'mb-4'}`}>
      {fullPage && <Button size="xs" variant="ghost" icon={ArrowLeft} onClick={onBack} className="visual-search-back mb-3 -ml-2">
        {t('advancedSearch.visual.back')}
      </Button>}
      <h2 className="mb-3 flex items-center gap-2 text-[13px] font-medium text-ide-text">
        {fullPage ? <><ImageIcon className="h-4 w-4 text-ide-accent" aria-hidden="true" />{t('advancedSearch.visual.title')}</> : (
          <button type="button" onClick={onOpenGallery} aria-label={t('advancedSearch.visual.open')}
            className={`visual-search-open group inline-flex items-center gap-2 rounded py-1 text-ide-text transition-colors hover:text-ide-accent ${focusStyle}`}>
            <ImageIcon className="h-4 w-4 text-ide-accent" aria-hidden="true" />
            <span>{t('advancedSearch.visual.title')}</span>
            <ChevronRight className="h-3.5 w-3.5 text-ide-muted transition-colors group-hover:text-ide-accent" aria-hidden="true" />
          </button>
        )}
      </h2>

      <div id={regionId} role="region" aria-label={t('advancedSearch.visual.results')}>
        {waiting ? <>
          <p role="status" className="mb-3 flex items-center gap-1.5 text-xs text-ide-muted">
            <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" aria-hidden="true" />
            {t('advancedSearch.visual.loading')}
          </p>
          <div className={`visual-search-grid ${fullPage ? 'visual-search-gallery-grid' : 'visual-search-summary-grid'} grid gap-3`} aria-hidden="true">
            {Array.from({ length: pageSize }, (_, index) => <div key={index}
              className="aspect-video animate-pulse rounded border border-ide-border bg-ide-panel motion-reduce:animate-none" />)}
          </div>
        </> : phase === 'error' ? (
          <div className="flex flex-wrap items-center gap-3 py-2">
            <p role="status" className="text-xs leading-relaxed text-ide-muted">{t('advancedSearch.visual.retry_hint')}</p>
            <Button size="xs" icon={RefreshCw} onClick={retry}>{t('advancedSearch.visual.retry')}</Button>
          </div>
        ) : visibleResults.length === 0 ? (
          <p className="py-2 text-xs leading-relaxed text-ide-muted">{t('advancedSearch.visual.empty_hint')}</p>
        ) : (
          <div className={`visual-search-grid ${fullPage ? 'visual-search-gallery-grid' : 'visual-search-summary-grid'} grid gap-x-3 gap-y-4`}>
            {visibleResults.map((item) => {
              const time = resultTime(item);
              const name = item.process_name || t('advancedSearch.unknown');
              const title = item.window_title || name;
              const relevance = Number.isFinite(item.similarity)
                ? t('advancedSearch.visual.relevance', { score: item.similarity.toFixed(2) }) : null;
              return <div key={item.screenshot_id} className="min-w-0">
                <ThumbnailCard item={item} sourceType="search" imageKind="original" showFooter={false}
                  ariaLabel={[title, time?.full, relevance].filter(Boolean).join(' · ')}
                  onSelect={onSelectResult} onOpenFloatingPreview={onOpenFloatingPreview} />
                <div className="mt-1.5 flex items-baseline gap-2 text-xs">
                  <p className="min-w-0 flex-1 truncate text-ide-text" title={title}>{title}</p>
                  {relevance && <span className="shrink-0 text-[11px] tabular-nums text-ide-accent" title={t('advancedSearch.visual.relevanceHint')}>
                    {relevance}
                  </span>}
                </div>
                <div className="flex items-baseline justify-between gap-2 pt-1 text-[11px] text-ide-muted">
                  <span className="min-w-0 truncate" title={name}>{name}</span>
                  {time && <time dateTime={time.iso} title={time.full} className="shrink-0 tabular-nums">{time.short}</time>}
                </div>
              </div>;
            })}
          </div>
        )}

        {pagingError && <p role="status" className="mt-3 text-xs text-ide-muted">{t('advancedSearch.visual.page_error')}</p>}
        {phase === 'ready' && canShowMore && (
          <div className="mt-4 flex items-center gap-3">
            <span className="min-w-0 flex-1 border-t border-ide-border" aria-hidden="true" />
            <Button size="xs" loading={loadingMore} icon={pagingError ? RefreshCw : ChevronDown}
              onClick={fullPage ? loadGalleryPage : showMore} className="rounded-full px-4 py-1.5"
              aria-controls={regionId}>
              {t(pagingError ? 'advancedSearch.visual.retry' : 'advancedSearch.visual.more', { count: pageSize })}
            </Button>
            <span className="min-w-0 flex-1 border-t border-ide-border" aria-hidden="true" />
          </div>
        )}
        {fullPage && <div ref={sentinelRef} className="h-1" aria-hidden="true" />}
      </div>
    </section>
  );
}
