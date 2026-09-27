import React, { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle, Bot, Check, ChevronDown, Loader2, Search, Settings, Square, X } from 'lucide-react';
import { Button } from '../ui/Button';
import { HitThumbnail } from './SearchResultRow';
import { aiErrorDetail, aiErrorKey } from '../../lib/ai_api';
import { citedIds, parseAnswer } from '../../lib/ai_answer';
import { openSettingsWindow } from '../../lib/settings_api';

const MAX_SOURCES = 12;

function formatTime(value) {
  const ms = typeof value === 'number' && value < 1e12 ? value * 1000 : value;
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString();
}

function snapshotTime(ref) {
  return [ref.timestamp, ref.screenshot_created_at, ref.created_at].find((v) => v !== undefined && v !== null && v !== 0);
}

/** One line describing a tool call, e.g. 搜索文字「发票」. */
function describeStep(step, t) {
  const args = step.arguments || {};
  switch (step.name) {
    case 'search_ocr_text': return t('aiSearch.steps.search_ocr_text', { query: args.query ?? '' });
    case 'search_nl': return t('aiSearch.steps.search_nl', { query: args.query ?? '' });
    case 'get_snapshots_by_time_range':
      return t('aiSearch.steps.time_range', { start: formatTime(args.start_time), end: formatTime(args.end_time) });
    case 'get_snapshot_details': return t('aiSearch.steps.details', { id: args.id ?? '' });
    case 'get_smart_clusters': return t('aiSearch.steps.clusters');
    case 'get_smart_cluster_ocr_corpus':
    case 'get_smart_cluster_summary': return t('aiSearch.steps.cluster_content');
    default: return step.name;
  }
}

function Inline({ parts, onCite, known }) {
  return parts.map((part, index) => {
    switch (part.type) {
      case 'bold': return <strong key={index} className="font-semibold">{part.text}</strong>;
      case 'code': return <code key={index} className="rounded bg-ide-hover px-1 py-0.5 text-[12px]">{part.text}</code>;
      case 'cite': return (
        <button key={index} type="button" onClick={() => onCite(part.id)} disabled={!known.has(part.id)}
          className="mx-0.5 inline-flex items-center rounded bg-ide-accent/15 px-1.5 text-[11px] font-medium text-ide-accent align-baseline hover:bg-ide-accent/25 disabled:bg-ide-hover disabled:text-ide-muted">
          #{part.id}
        </button>
      );
      default: return <React.Fragment key={index}>{part.text}</React.Fragment>;
    }
  });
}

function Answer({ text, onCite, known }) {
  const blocks = useMemo(() => parseAnswer(text), [text]);
  return (
    <div className="space-y-3 text-sm leading-relaxed text-ide-text">
      {blocks.map((block, index) => {
        if (block.type === 'heading') return <h3 key={index} className="pt-1 font-semibold"><Inline parts={block.parts} onCite={onCite} known={known} /></h3>;
        if (block.type === 'list') {
          const Tag = block.ordered ? 'ol' : 'ul';
          return (
            <Tag key={index} className={`space-y-1 pl-5 ${block.ordered ? 'list-decimal' : 'list-disc'}`}>
              {block.items.map((item, i) => <li key={i}><Inline parts={item} onCite={onCite} known={known} /></li>)}
            </Tag>
          );
        }
        return (
          <p key={index}>
            {block.lines.map((line, i) => <React.Fragment key={i}>{i > 0 && <br />}<Inline parts={line} onCite={onCite} known={known} /></React.Fragment>)}
          </p>
        );
      })}
    </div>
  );
}

function Steps({ steps, running, t }) {
  const [open, setOpen] = useState(false);
  if (steps.length === 0) {
    return running ? (
      <div className="flex items-center gap-2 text-xs text-ide-muted"><Loader2 className="h-3.5 w-3.5 animate-spin" />{t('aiSearch.thinking')}</div>
    ) : null;
  }
  const current = [...steps].reverse().find((step) => step.status === 'running');
  return (
    <div className="rounded-lg border border-ide-border bg-ide-panel">
      <button type="button" onClick={() => setOpen((v) => !v)} aria-expanded={open}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-ide-muted hover:text-ide-text">
        {running ? <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" /> : <Check className="h-3.5 w-3.5 shrink-0" />}
        <span className="min-w-0 flex-1 truncate">
          {running && current ? describeStep(current, t) : t('aiSearch.steps.summary', { count: steps.length })}
        </span>
        <ChevronDown className={`h-3.5 w-3.5 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && (
        <ol className="space-y-1.5 border-t border-ide-border px-3 py-2 text-xs">
          {steps.map((step) => (
            <li key={step.id} className="flex items-start gap-2">
              {step.status === 'running' && <Loader2 className="mt-0.5 h-3 w-3 shrink-0 animate-spin text-ide-muted" />}
              {step.status === 'done' && <Check className="mt-0.5 h-3 w-3 shrink-0 text-ide-muted" />}
              {step.status === 'failed' && <X className="mt-0.5 h-3 w-3 shrink-0 text-ide-error" />}
              <span className="min-w-0 flex-1 break-words text-ide-text">{describeStep(step, t)}</span>
              {step.status === 'done' && typeof step.itemCount === 'number' && (
                <span className="shrink-0 text-ide-muted">{t('aiSearch.steps.results', { count: step.itemCount })}</span>
              )}
              {step.status === 'failed' && <span className="shrink-0 text-ide-error">{t('aiSearch.steps.failed')}</span>}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/** HitThumbnail refetches when its item changes identity, so keep it stable. */
function SourceThumbnail({ id }) {
  const item = useMemo(() => ({ screenshot_id: id }), [id]);
  return <HitThumbnail item={item} className="aspect-video w-full" />;
}

function Sources({ refs, onSelect, t }) {
  if (refs.length === 0) return null;
  return (
    <div className="space-y-2">
      <h3 className="text-xs font-medium text-ide-muted">{t('aiSearch.sources')}</h3>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
        {refs.map((ref) => (
          <button key={ref.id} type="button" onClick={() => onSelect(ref)}
            className="group flex flex-col gap-1.5 rounded-lg p-1.5 text-left transition-colors hover:bg-ide-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
            <SourceThumbnail id={ref.id} />
            <span className="flex items-center gap-1.5 text-[11px] text-ide-muted">
              <span className="font-medium text-ide-accent">#{ref.id}</span>
              <span className="truncate">{ref.process_name || ''}</span>
            </span>
            {ref.window_title && <span className="line-clamp-1 text-[11px] text-ide-text">{ref.window_title}</span>}
            {snapshotTime(ref) !== undefined && <span className="text-[10px] text-ide-muted">{formatTime(snapshotTime(ref))}</span>}
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * AI search: the user asks a question, the model searches the history with
 * the same tools external assistants use, and answers with citations.
 */
export function AiSearchPanel({ controller: c, tabs, onSelectResult, header: Header }) {
  const { t } = useTranslation();
  const [input, setInput] = useState('');
  const running = c.status === 'running';
  const noProvider = c.settings && !c.provider;

  const refs = useMemo(() => {
    const byId = new Map();
    // Earlier sightings win; later ones only fill fields that were missing.
    const add = (ref) => { if (ref?.id) byId.set(ref.id, { ...ref, ...byId.get(ref.id) }); };
    c.steps.forEach((step) => step.snapshots?.forEach(add));
    c.outcome?.snapshots?.forEach(add);
    return byId;
  }, [c.steps, c.outcome]);

  const sources = useMemo(() => {
    const cited = citedIds(c.answer).filter((id) => refs.has(id)).map((id) => refs.get(id));
    if (cited.length > 0 || running) return cited.slice(0, MAX_SOURCES);
    return [...refs.values()].slice(0, MAX_SOURCES);
  }, [c.answer, refs, running]);

  const known = useMemo(() => new Set(refs.keys()), [refs]);
  const select = (ref) => onSelectResult?.({ ...ref, screenshot_id: ref.id });
  const cite = (id) => { const ref = refs.get(id); if (ref) select(ref); };

  const submit = (event) => {
    event.preventDefault();
    if (running) { c.cancel(); return; }
    if (!noProvider) c.start(input);
  };

  const renderBody = () => {
    if (!c.settings) {
      return <div className="flex justify-center py-16 text-ide-muted"><Loader2 className="h-4 w-4 animate-spin" /></div>;
    }
    if (noProvider) {
      return (
        <div className="mx-auto flex max-w-md flex-col items-center gap-3 py-16 text-center">
          <Bot className="h-8 w-8 text-ide-muted" />
          <p className="text-sm font-medium">{t('aiSearch.setup.title')}</p>
          <p className="text-xs leading-relaxed text-ide-muted">{t('aiSearch.setup.description')}</p>
          <Button icon={Settings} onClick={() => openSettingsWindow('ai', 'ai-providers')}>{t('aiSearch.setup.button')}</Button>
        </div>
      );
    }
    if (c.status === 'consent') {
      return (
        <div className="mx-auto mt-10 max-w-lg space-y-3 rounded-xl border border-ide-border bg-ide-panel p-5">
          <p className="text-sm font-medium">{t('aiSearch.consent.title', { name: c.provider.name })}</p>
          <p className="text-xs leading-relaxed text-ide-muted">{t('aiSearch.consent.description')}</p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={c.declineConsent}>{t('common.cancel')}</Button>
            <Button variant="primary" onClick={c.acceptConsent}>{t('aiSearch.consent.accept')}</Button>
          </div>
        </div>
      );
    }
    if (c.status === 'idle') {
      return (
        <div className="mx-auto max-w-lg space-y-3 py-12">
          <p className="text-center text-xs text-ide-muted">{t('aiSearch.landing.using', { name: c.provider.name })}</p>
          <div className="space-y-1.5">
            {t('aiSearch.landing.examples', { returnObjects: true }).map((example) => (
              <button key={example} type="button" onClick={() => { setInput(example); c.start(example); }}
                className="block w-full rounded-lg border border-ide-border px-3 py-2 text-left text-sm text-ide-text transition-colors hover:bg-ide-hover">
                {example}
              </button>
            ))}
          </div>
        </div>
      );
    }
    return (
      <div className="mx-auto max-w-3xl space-y-4 px-6 py-5">
        <p className="text-sm font-medium text-ide-text">{c.question}</p>
        <Steps key={c.question + c.status} steps={c.steps} running={running} t={t} />
        {c.answer && <Answer text={c.answer} onCite={cite} known={known} />}
        {c.status === 'cancelled' && <p className="text-xs text-ide-muted">{t('aiSearch.cancelled')}</p>}
        {c.status === 'error' && (
          <div role="alert" className="flex items-start gap-2 rounded-lg border border-red-500/20 bg-red-500/10 p-3 text-sm text-red-400">
            <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
            <div>
              <p>{t(aiErrorKey(c.error))}</p>
              {aiErrorDetail(c.error) && <p className="mt-1 break-words text-xs opacity-80">{aiErrorDetail(c.error)}</p>}
            </div>
          </div>
        )}
        {c.outcome?.stopped_early && <p className="text-xs text-ide-muted">{t('aiSearch.stopped_early')}</p>}
        {c.outcome?.truncated && <p className="text-xs text-ide-muted">{t('aiSearch.truncated')}</p>}
        <Sources refs={sources} onSelect={select} t={t} />
      </div>
    );
  };

  return (
    <>
      <Header as="form" onSubmit={submit} bordered flushBottom secondaryRow={tabs}>
        <div className="flex h-10 w-full max-w-[620px] items-center gap-1 rounded-lg border border-ide-border bg-ide-bg pl-3.5 pr-1.5 transition-colors focus-within:border-ide-accent focus-within:ring-2 focus-within:ring-ide-accent/20">
          <input
            type="text"
            className="min-w-0 flex-1 bg-transparent text-sm text-ide-text outline-none placeholder:text-ide-muted"
            placeholder={t('aiSearch.placeholder')}
            value={input}
            disabled={noProvider}
            onChange={(event) => setInput(event.target.value)}
          />
          <button
            type="submit"
            disabled={noProvider || (!running && !input.trim())}
            title={running ? t('aiSearch.stop') : t('advancedSearch.search.go')}
            aria-label={running ? t('aiSearch.stop') : t('advancedSearch.search.go')}
            className="grid h-[30px] w-[30px] shrink-0 place-items-center rounded-md bg-ide-accent text-white transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:opacity-40"
          >
            {running ? <Square className="h-3.5 w-3.5" /> : <Search className="h-4 w-4" />}
          </button>
        </div>
      </Header>
      <div className="min-h-0 flex-1 overflow-y-auto custom-scrollbar">{renderBody()}</div>
    </>
  );
}
