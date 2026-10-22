import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarDays, ChevronRight, FileText } from 'lucide-react';
import { Button } from '../ui/Button';
import { Dialog } from '../Dialog';
import { SourceThumbnail } from '../recap/RecapSources';
import { SourcesDialog } from '../recap/RecapDialogs';
import RecapKeyDetails from '../recap/RecapKeyDetails';
import { buildSnippet } from '../../lib/search_snippet';

function Highlight({ text, tokens }) {
  if (!text) return null;
  const terms = tokens.filter(Boolean).slice(0, 20);
  if (!terms.length) return text;
  const pattern = new RegExp(`(${terms.map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'gi');
  return text.split(pattern).map((part, index) => index % 2
    ? <mark key={index} className="rounded-sm bg-ide-accent/20 text-ide-accent">{part}</mark> : part);
}

/** Recaps keep their activity context inside the shared relevance stream. */
export function RecapSearchResultRow({ item, time, queryTokens, onDetails, onOpenDay, action }) {
  const { t } = useTranslation();
  const sources = item.sources || [];
  const processes = [...new Set(sources.map((source) => source.process_name))].slice(0, 3);
  const detail = (item.details || []).find((entry) => queryTokens.some((token) => entry.text.toLowerCase().includes(token.toLowerCase()))) || item.details?.[0];
  return <article className="group rounded-lg px-2.5 py-3 transition-colors hover:bg-ide-hover">
    <div className="flex gap-3.5">
      <div className="flex w-[120px] shrink-0 flex-col gap-1.5 pt-0.5 text-[11px] text-ide-muted">
        <span className="inline-flex items-center gap-1.5 text-xs font-medium text-ide-accent"><FileText className="h-3.5 w-3.5" />{t('advancedSearch.resultTypes.recap')}</span>
        <time className="tabular-nums">{item.date}</time>
        <time className="tabular-nums">{time(item.start_ms)}{item.end_ms !== item.start_ms && `–${time(item.end_ms)}`}</time>
      </div>
      <div className="min-w-0 flex-1">
        <button type="button" onClick={onDetails} className="block w-full rounded text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
          <span className="flex items-start gap-3"><span className="min-w-0 flex-1 break-words text-sm font-semibold text-ide-text"><Highlight text={item.task_title} tokens={queryTokens} /></span><ChevronRight className="h-4 w-4 shrink-0 text-ide-muted" /></span>
          <span className="mt-1.5 line-clamp-2 block break-words text-[13px] leading-6 text-ide-text"><Highlight text={buildSnippet(item.text, queryTokens)} tokens={queryTokens} /></span>
          {detail && <span className="mt-1.5 line-clamp-2 block border-l-2 border-ide-accent/30 pl-2 text-xs leading-5 text-ide-muted"><Highlight text={buildSnippet(detail.text, queryTokens)} tokens={queryTokens} /></span>}
        </button>
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ide-muted">
          <span>{t('recap.search.sources', { count: sources.length })}</span><span>{processes.join(' · ')}</span>
          {Number.isFinite(item.semantic_score) && <span className="font-mono tabular-nums" title={t('advancedSearch.recapRelevanceHint')}>
            {t('advancedSearch.recapRelevance', { score: item.semantic_score.toFixed(2) })}
          </span>}
          {onOpenDay && <button type="button" className="rounded hover:text-ide-accent focus-visible:ring-2 focus-visible:ring-ide-accent/60" onClick={() => onOpenDay(item.date)}>{t('recap.search.openDay')}</button>}
          {sources[0] && <button type="button" className="rounded hover:text-ide-accent focus-visible:ring-2 focus-visible:ring-ide-accent/60" onClick={() => action.open(sources[0])}>{t('advancedSearch.viewSource')}</button>}
        </div>
      </div>
    </div>
  </article>;
}

export function RecapSearchResultDetails({ item, action, time, onClose, onOpenDay }) {
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

