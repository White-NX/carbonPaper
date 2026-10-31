import React, { useId, useMemo } from 'react';
import { ChevronDown, ChevronUp, Image as ImageIcon, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { ThumbnailCard } from '../ThumbnailCard';
import { Button } from '../ui/Button';

/** A compact visual preview opens into the existing screenshot gallery. */
export function VisualSearchCard({ controller, onSelectResult, onOpenFloatingPreview }) {
  const { t, i18n } = useTranslation();
  const regionId = useId();
  const { phase, results, thumbnails, expanded, setExpanded, hasMore, loadingMore, pageError, retry, loadMore } = controller;
  const waiting = phase === 'loading' || phase === 'idle';
  const failed = phase === 'error';
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
  const hint = waiting ? t('advancedSearch.visual.loading') : failed ? t('advancedSearch.visual.unavailable')
    : results.length ? t('advancedSearch.visual.hint') : t('advancedSearch.visual.empty');

  return (
    <section className="visual-search-card mx-6 mb-4 mt-4 overflow-hidden rounded-xl border border-ide-border bg-ide-panel">
      <button type="button" aria-label={t('advancedSearch.visual.title')} aria-expanded={expanded} aria-controls={regionId}
        onClick={() => setExpanded(!expanded)}
        className="group flex w-full items-center gap-3 px-4 py-3.5 text-left transition-colors hover:bg-ide-hover focus-visible:outline-none focus-visible:brightness-90 dark:focus-visible:brightness-125">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-ide-accent/15 bg-ide-accent/10 text-ide-accent" aria-hidden="true">
          <ImageIcon className="h-[18px] w-[18px]" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="text-[13px] font-semibold text-ide-text">{t('advancedSearch.visual.title')}</span>
            {phase === 'ready' && results.length > 0 && (
              <span className="rounded-full border border-ide-border bg-ide-bg px-2 py-0.5 text-[10px] font-medium tabular-nums text-ide-muted">
                {t(hasMore ? 'advancedSearch.visual.count_more' : 'advancedSearch.visual.count', { count: results.length })}
              </span>
            )}
          </span>
          <span className="mt-1 flex items-center gap-1.5 text-[11px] leading-relaxed text-ide-muted" aria-live="polite">
            {waiting && <Loader2 className="h-3 w-3 shrink-0 animate-spin motion-reduce:animate-none" aria-hidden="true" />}
            <span className="truncate">{hint}</span>
          </span>
        </span>
        {!expanded && (waiting || results.length > 0) && (
          <span className="visual-search-previews flex shrink-0 gap-1.5" aria-hidden="true">
            {waiting ? [0, 1, 2].map((index) => (
              <span key={index} className="h-11 w-[68px] animate-pulse rounded-md border border-ide-border bg-ide-bg motion-reduce:animate-none" />
            )) : results.slice(0, 3).map((item) => (
              <span key={item.screenshot_id} className="h-11 w-[68px] overflow-hidden rounded-md border border-ide-border bg-ide-bg">
                {thumbnails[item.screenshot_id]
                  ? <img src={thumbnails[item.screenshot_id]} alt="" className="h-full w-full object-cover" loading="lazy" />
                  : <span className="grid h-full w-full place-items-center text-ide-muted"><ImageIcon className="h-3.5 w-3.5 opacity-50" /></span>}
              </span>
            ))}
          </span>
        )}
        <span className="visual-search-disclosure ml-1 shrink-0 text-[11px] text-ide-muted transition-colors group-hover:text-ide-text">
          {t(expanded ? 'advancedSearch.visual.collapse' : 'advancedSearch.visual.expand')}
        </span>
        <ChevronDown className={`h-4 w-4 shrink-0 text-ide-muted transition-transform motion-reduce:transition-none ${expanded ? 'rotate-180' : ''}`} aria-hidden="true" />
      </button>

      <div id={regionId} role="region" aria-label={t('advancedSearch.visual.results')} hidden={!expanded}
        className="border-t border-ide-border px-4 pb-3 pt-4">
        {expanded && <>
          {waiting ? (
            <div className="visual-search-grid grid gap-3" aria-hidden="true">
              {[0, 1, 2, 3, 4, 5].map((index) => <div key={index} className="aspect-video animate-pulse rounded border border-ide-border bg-ide-bg motion-reduce:animate-none" />)}
            </div>
          ) : failed ? (
            <div className="flex flex-wrap items-center justify-between gap-3 py-2">
              <p role="status" className="text-xs leading-relaxed text-ide-muted">{t('advancedSearch.visual.retry_hint')}</p>
              <Button size="xs" icon={RefreshCw} onClick={retry}>{t('advancedSearch.visual.retry')}</Button>
            </div>
          ) : results.length === 0 ? (
            <p className="py-3 text-xs leading-relaxed text-ide-muted">{t('advancedSearch.visual.empty_hint')}</p>
          ) : (
            <>
              <div className="visual-search-grid grid gap-3">
                {results.map((item) => {
                  const time = resultTime(item);
                  const name = item.process_name || t('advancedSearch.unknown');
                  return <div key={item.screenshot_id} className="min-w-0">
                    <ThumbnailCard item={item} sourceType="search" imageKind="original" showFooter={false}
                      ariaLabel={[item.window_title || name, time?.full].filter(Boolean).join(' · ')}
                      onSelect={onSelectResult} onOpenFloatingPreview={onOpenFloatingPreview} />
                    <div className="flex items-baseline justify-between gap-2 px-0.5 pt-1.5">
                      <span className="min-w-0 truncate text-[11px] font-medium text-ide-text" title={name}>{name}</span>
                      {time && <time dateTime={time.iso} title={time.full} className="shrink-0 text-[10px] tabular-nums text-ide-muted">{time.short}</time>}
                    </div>
                  </div>;
                })}
              </div>
              <div className="mt-3 flex flex-wrap items-center justify-between gap-2 border-t border-ide-border pt-3">
                <span className="text-[11px] leading-relaxed text-ide-muted" role={pageError ? 'status' : undefined}>
                  {t(pageError ? 'advancedSearch.visual.page_error' : 'advancedSearch.visual.order')}
                </span>
                <div className="ml-auto flex items-center gap-2">
                  {hasMore && <Button size="xs" loading={loadingMore} icon={pageError ? RefreshCw : undefined}
                    onClick={pageError ? retry : loadMore}>{t(pageError ? 'advancedSearch.visual.retry' : 'advancedSearch.visual.more')}</Button>}
                  <Button size="xs" variant="ghost" icon={ChevronUp} onClick={() => setExpanded(false)}>{t('advancedSearch.visual.collapse')}</Button>
                </div>
              </div>
            </>
          )}
        </>}
      </div>
    </section>
  );
}
