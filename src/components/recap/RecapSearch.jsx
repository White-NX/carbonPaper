import React, { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowRight, CalendarDays, Check, ChevronRight, FileText, Search, SlidersHorizontal, Sparkles, X } from 'lucide-react';
import useRecapSearch from '../../hooks/useRecapSearch';
import { localDate, recapErrorKey } from '../../lib/recap_api';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';
import { ProgressBlock } from '../overlay/ProgressBlock';
import { Dialog } from '../Dialog';
import { SourceThumbnail } from './RecapSources';
import { SourcesDialog } from './RecapDialogs';
import RecapKeyDetails from './RecapKeyDetails';

function Highlight({ text, query }) {
  const terms = [...new Set(query.trim().split(/\s+/).filter(Boolean))].sort((a, b) => b.length - a.length).slice(0, 20);
  if (!terms.length) return text;
  const pattern = new RegExp(`(${terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'gi');
  return String(text).split(pattern).map((part, index) => index % 2
    ? <mark key={index} className="rounded-sm bg-ide-accent/15 px-0.5 text-inherit">{part}</mark> : part);
}

function ResultDetails({ item, action, time, onClose, onOpenDay }) {
  const { t } = useTranslation();
  const [sources, setSources] = useState(null);
  return <>
    <Dialog isOpen title={t('recap.search.activity')} onClose={onClose} maxWidth="max-w-[760px]" contentClassName="p-5 sm:p-7">
      <p className="mb-2 text-xs tabular-nums text-ide-muted">{item.date} · {time(item.start_ms)}{item.end_ms !== item.start_ms && `–${time(item.end_ms)}`}</p>
      <h2 className="break-words text-xl font-semibold leading-relaxed">{item.task_title}</h2>
      <p className="mt-4 whitespace-pre-wrap break-words text-sm leading-7">{item.text}</p>
      <RecapKeyDetails details={item.details} sources={item.sources} onSources={setSources} />
      <div className="mt-6 border-t border-ide-border pt-5">
        <div className="mb-3 flex items-center justify-between gap-3"><h3 className="text-xs font-semibold">{t('recap.sources')}</h3>
          <Button size="xs" variant="ghost" onClick={() => setSources(item.sources)}>{t('recap.viewSources', { count: item.sources.length })}</Button>
        </div>
        <div className="grid grid-cols-3 gap-3">{item.sources.slice(0, 3).map((source) => <SourceThumbnail key={source.id} source={source} action={action} time={time} />)}</div>
      </div>
      <div className="mt-6 flex flex-wrap justify-end gap-2">
        <Button variant="ghost" onClick={onClose}>{t('recap.close')}</Button>
        <Button icon={CalendarDays} onClick={() => onOpenDay(item.date)}>{t('recap.search.openDay')}</Button>
      </div>
    </Dialog>
    {sources && <SourcesDialog sources={sources} action={action} time={time} onClose={() => setSources(null)} />}
  </>;
}

export default function RecapSearch({ active, authenticated, action, time, onOpenDay }) {
  const { t, i18n } = useTranslation();
  const controller = useRecapSearch(active, authenticated);
  const { filters, update, result, status, loading, loadingMore, preparing, error, indexError, changed, submitted } = controller;
  const [selected, setSelected] = useState(null);
  const [options, setOptions] = useState(false);
  const [range, setRange] = useState('all');
  const dateFormatter = useMemo(() => new Intl.DateTimeFormat(i18n.language, { year: 'numeric', month: 'short', day: 'numeric' }), [i18n.language]);
  useEffect(() => { setSelected(null); }, [result, active, authenticated]);
  const hasIndex = (status?.indexed_chunks || 0) > 0;
  const pending = status && status.indexed_chunks < status.total_chunks;
  const rangeInvalid = filters.start_date && filters.end_date && filters.start_date > filters.end_date;
  const modes = [{ id: 'hybrid', icon: Sparkles }, { id: 'keyword', icon: Search }, { id: 'semantic', icon: Sparkles }];
  const chooseRange = (value) => {
    setRange(value);
    if (value === 'custom') return;
    const start = new Date();
    start.setDate(start.getDate() - (value === 'week' ? 6 : 29));
    update({ start_date: value === 'all' ? '' : localDate(start), end_date: value === 'all' ? '' : localDate() });
  };
  const submit = (event) => { event.preventDefault(); if (!rangeInvalid) controller.search(); };
  const empty = result && !result.items.length;
  return <div className="recap-search flex h-full min-h-0 flex-col">
    <div className="shrink-0 border-b border-ide-border px-5 pb-5 pt-6 sm:px-7">
      <div className="mx-auto max-w-[1000px]">
        <div className="mb-4 flex flex-wrap items-baseline gap-x-3 gap-y-1"><h2 className="text-lg font-semibold tracking-tight">{t('recap.search.title')}</h2><p className="text-xs text-ide-muted">{t('recap.search.subtitle')}</p></div>
        <form onSubmit={submit} aria-label={t('recap.search.form')}>
          <div className="recap-search-input flex items-center gap-3 rounded-xl border border-ide-border bg-ide-panel px-4 py-2.5 focus-within:border-ide-accent/60">
            <Search className="h-4 w-4 shrink-0 text-ide-muted" aria-hidden="true" />
            <input aria-label={t('recap.search.query')} type="search" maxLength={256} value={filters.query} onChange={(event) => update({ query: event.target.value })}
              placeholder={t('recap.search.placeholder')} className="min-w-0 flex-1 bg-transparent py-1.5 text-sm text-ide-text outline-none placeholder:text-ide-muted" />
            <Button type="submit" variant="primary" loading={loading} disabled={!filters.query.trim() || Boolean(rangeInvalid)} icon={ArrowRight}>{t('recap.search.submit')}</Button>
          </div>
          <div className="mt-3 flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap items-center gap-1" role="group" aria-label={t('recap.search.mode')}>
              {modes.map(({ id, icon }) => <Button key={id} size="xs" variant={filters.mode === id ? 'secondary' : 'ghost'} icon={filters.mode === id ? Check : icon}
                aria-pressed={filters.mode === id} onClick={() => update({ mode: id })}>{t(`recap.search.modes.${id}`)}</Button>)}
            </div>
            <div className="flex items-center gap-2">
              <label className="flex items-center gap-1.5 text-xs text-ide-muted"><CalendarDays className="h-3.5 w-3.5" aria-hidden="true" />
                <select aria-label={t('recap.search.range')} value={range} onChange={(event) => chooseRange(event.target.value)} className="recap-focus min-w-0 rounded-md bg-ide-bg py-1.5 text-ide-text">
                  {['all', 'week', 'month', 'custom'].map((value) => <option key={value} value={value}>{t(`recap.search.ranges.${value}`)}</option>)}
                </select>
              </label>
              <Button size="xs" variant="ghost" icon={SlidersHorizontal} aria-expanded={options} aria-controls="recap-search-options" onClick={() => setOptions(!options)}>{t('recap.search.options')}</Button>
            </div>
          </div>
          {range === 'custom' && <div className="mt-3 flex flex-wrap items-center gap-3 text-xs">
            <label className="flex shrink-0 items-center gap-2 whitespace-nowrap text-ide-muted">{t('recap.search.startDate')}<input aria-label={t('recap.search.startDate')} type="date" className="recap-input recap-search-date" value={filters.start_date} onChange={(event) => update({ start_date: event.target.value })} /></label>
            <label className="flex shrink-0 items-center gap-2 whitespace-nowrap text-ide-muted">{t('recap.search.endDate')}<input aria-label={t('recap.search.endDate')} type="date" className="recap-input recap-search-date" value={filters.end_date} onChange={(event) => update({ end_date: event.target.value })} /></label>
          </div>}
          {options && <div id="recap-search-options" className="mt-3 rounded-lg border border-ide-border bg-ide-panel/50 px-3 py-3">
            <label className="flex cursor-pointer items-start gap-2.5 text-xs"><input className="mt-0.5 accent-[var(--ide-accent)]" type="checkbox" checked={filters.include_details} onChange={(event) => update({ include_details: event.target.checked })} />
              <span><span className="block font-medium">{t('recap.search.includeDetails')}</span><span className="mt-1 block leading-relaxed text-ide-muted">{t('recap.search.includeDetailsHint')}</span></span>
            </label>
          </div>}
          {rangeInvalid && <p role="alert" className="mt-2 text-xs text-ide-error">{t('recap.search.invalidRange')}</p>}
        </form>
      </div>
    </div>
    <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-5 sm:px-7" aria-busy={loading}>
      <div className="mx-auto max-w-[1000px]">
        {preparing ? <div className="mb-5 flex items-center gap-4 rounded-xl border border-ide-border bg-ide-panel px-4 py-4">
          <ProgressBlock label={t('recap.search.preparing')} percent={status?.total_chunks ? status.indexed_chunks / status.total_chunks * 100 : undefined} detail={t('recap.search.preparingHint')} />
          <Button variant="ghost" size="xs" icon={X} onClick={controller.stop}>{t('recap.stop')}</Button>
        </div> : pending && <Banner className="mb-5" title={t(hasIndex ? 'recap.search.indexPartial' : 'recap.search.indexOffer')}
          action={<Button size="xs" onClick={controller.prepare}>{t(hasIndex ? 'recap.search.updateIndex' : 'recap.search.prepare')}</Button>}>
          {t('recap.search.indexHint')}
        </Banner>}
        {indexError && !preparing && <Banner tone="warning" className="mb-4" action={<Button size="xs" variant="ghost" onClick={controller.prepare}>{t('common.retry')}</Button>}>{t(recapErrorKey(indexError))}</Banner>}
        {error && <Banner tone="error" className="mb-4" action={<Button size="xs" variant="ghost" onClick={controller.search}>{t('common.retry')}</Button>}>{t(recapErrorKey(error))}</Banner>}
        {changed && !result && submitted && !loading && <Banner className="mb-4" action={<Button size="xs" onClick={controller.search}>{t('recap.search.submit')}</Button>}>{t('recap.search.changed')}</Banner>}
        {result?.mode === 'hybrid' && ['not_prepared', 'unavailable', 'partial'].includes(result.semantic_status) && <p role="status" className="mb-4 text-xs leading-relaxed text-ide-muted">{t(result.semantic_status === 'partial' ? 'recap.search.partialResults' : 'recap.search.keywordFallback')}</p>}
        {loading ? <div role="status" aria-label={t('recap.search.searching')} className="space-y-5 py-2">{[0, 1, 2].map((key) => <div key={key} className="space-y-3 border-b border-ide-border pb-6 motion-safe:animate-pulse"><div className="h-2.5 w-24 rounded bg-ide-active" /><div className="h-4 w-2/5 rounded bg-ide-active" /><div className="h-3 w-4/5 rounded bg-ide-active" /></div>)}</div>
          : result?.items.length > 0 ? <>
            <div className="mb-3 flex flex-wrap items-center justify-between gap-2 text-xs text-ide-muted"><p role="status">{t('recap.search.resultCount', { count: result.total })}</p><span>{t('recap.search.sorted')}</span></div>
            <div className="divide-y divide-ide-border/70 border-y border-ide-border/70">
              {result.items.map((item) => <button type="button" key={`${item.date}:${item.activity_id}`} onClick={() => setSelected(item)} className="recap-focus recap-search-result group flex w-full items-start gap-4 rounded-lg px-3 py-5 text-left transition-colors hover:bg-ide-hover">
                <span className="recap-search-result-date w-28 shrink-0 pt-0.5 text-xs text-ide-muted"><span className="block">{dateFormatter.format(new Date(`${item.date}T12:00:00`))}</span><time className="mt-1.5 block tabular-nums">{time(item.start_ms)}</time></span>
                <span className="min-w-0 flex-1"><span className="block break-words text-sm font-semibold"><Highlight text={item.task_title} query={submitted?.query || ''} /></span>
                  <span className="mt-2 block whitespace-pre-wrap break-words text-[13px] leading-6 text-ide-muted"><Highlight text={item.text} query={submitted?.query || ''} /></span>
                  {item.details?.length > 0 && <span className="mt-2 line-clamp-2 border-l-2 border-ide-accent/30 pl-3 text-xs leading-6 text-ide-muted"><Highlight text={item.details[0].text} query={submitted?.query || ''} /></span>}
                  <span className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ide-muted"><span className="inline-flex items-center gap-1"><FileText className="h-3 w-3" aria-hidden="true" />{t('recap.search.sources', { count: item.sources.length })}</span>
                    <span>{[...new Set(item.sources.map((source) => source.process_name))].slice(0, 3).join(' · ')}</span>
                  </span>
                </span>
                <ChevronRight className="mt-1 h-4 w-4 shrink-0 text-ide-muted transition-transform group-hover:translate-x-0.5" aria-hidden="true" />
              </button>)}
            </div>
            {result.next_offset != null && <div className="flex justify-center py-6"><Button loading={loadingMore} onClick={controller.loadMore}>{t('recap.search.more')}</Button></div>}
            <details className="mt-5 text-[11px] text-ide-muted"><summary className="recap-focus w-fit cursor-pointer rounded">{t('recap.search.diagnostics')}</summary>
              <p className="mt-2 leading-6">{t('recap.search.timing', { ms: result.elapsed_ms })} · {t('recap.search.coverage', { indexed: result.indexed_chunks, total: result.total_chunks })}</p>
            </details>
          </> : !error && !changed && <div className="mx-auto max-w-lg py-12 text-center sm:py-16">
            <div className="mx-auto mb-5 grid h-12 w-12 place-items-center rounded-2xl border border-ide-border bg-ide-panel text-ide-accent"><Search className="h-5 w-5" aria-hidden="true" /></div>
            <h3 className="text-base font-semibold">{t(empty ? 'recap.search.emptyTitle' : status?.activities === 0 ? 'recap.search.noRecapsTitle' : 'recap.search.landingTitle')}</h3>
            <p className="mt-3 text-sm leading-7 text-ide-muted">{t(empty ? 'recap.search.emptyHint' : status?.activities === 0 ? 'recap.search.noRecapsHint' : 'recap.search.landingHint')}</p>
            {!empty && status?.activities === 0 && <Button className="mt-5" icon={CalendarDays} onClick={() => onOpenDay(localDate())}>{t('recap.search.openDay')}</Button>}
            {!empty && status?.activities > 0 && <div className="mt-6 flex flex-wrap justify-center gap-2">{['exampleOne', 'exampleTwo'].map((key) => <Button key={key} variant="ghost" size="xs" onClick={() => update({ query: t(`recap.search.${key}`) })}>{t(`recap.search.${key}`)}</Button>)}</div>}
          </div>}
      </div>
    </div>
    {selected && active && authenticated && <ResultDetails item={selected} action={action} time={time} onClose={() => setSelected(null)} onOpenDay={(date) => { setSelected(null); onOpenDay(date); }} />}
  </div>;
}
