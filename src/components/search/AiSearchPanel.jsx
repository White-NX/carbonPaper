import React, { useEffect, useId, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle, ArrowDown, ArrowUp, BookOpen, Bot, Clock, ChevronDown, Loader2, Plus, Search, Settings, Square, X } from 'lucide-react';
import { Button } from '../ui/Button';
import { HitThumbnail } from './SearchResultRow';
import { aiErrorDetail, aiErrorKey } from '../../lib/ai_api';
import { citedIds, parseAnswer } from '../../lib/ai_answer';
import { openSettingsWindow } from '../../lib/settings_api';
import { useFollowOutput } from '../../hooks/useFollowOutput';

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

function Inline({ parts, onCite, onCiteHover, onCiteFocus, known }) {
  return parts.map((part, index) => {
    switch (part.type) {
      case 'bold': return <strong key={index} className="font-semibold">{part.text}</strong>;
      case 'code': return <code key={index} className="rounded bg-ide-hover px-1 py-0.5 text-[12px]">{part.text}</code>;
      case 'cite': return (
        <button key={index} type="button" onClick={() => onCite(part.id)} disabled={!known.has(part.id)}
          onMouseEnter={() => { if (known.has(part.id)) onCiteHover?.(part.id); }} onMouseLeave={() => onCiteHover?.(null)}
          onFocus={() => onCiteFocus?.(part.id)} onBlur={() => onCiteFocus?.(null)}
          className="mx-0.5 inline-flex items-center rounded bg-ide-accent/15 px-1.5 text-[11px] font-medium text-ide-accent align-baseline hover:bg-ide-accent/25 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:bg-ide-hover disabled:text-ide-muted">
          #{part.id}
        </button>
      );
      default: return <React.Fragment key={index}>{part.text}</React.Fragment>;
    }
  });
}

function Answer({ text, ...citationProps }) {
  const blocks = useMemo(() => parseAnswer(text), [text]);
  return (
    <div className="space-y-4 break-words text-sm leading-7 text-ide-text">
      {blocks.map((block, index) => {
        if (block.type === 'table') return (
          <div key={index} className="max-w-full overflow-x-auto rounded-md border border-ide-border">
            <table className="w-full border-collapse text-sm">
              <thead className="bg-ide-hover">
                <tr>{block.header.map((cell, column) => (
                  <th key={column} scope="col" style={{ textAlign: block.align[column] }} className="min-w-24 border-b border-ide-border px-3 py-2 font-semibold">
                    <Inline parts={cell} {...citationProps} />
                  </th>
                ))}</tr>
              </thead>
              <tbody>{block.rows.map((row, rowIndex) => (
                <tr key={rowIndex} className="border-b border-ide-border last:border-b-0">
                  {row.map((cell, column) => (
                    <td key={column} style={{ textAlign: block.align[column] }} className="px-3 py-2 align-top">
                      <Inline parts={cell} {...citationProps} />
                    </td>
                  ))}
                </tr>
              ))}</tbody>
            </table>
          </div>
        );
        if (block.type === 'heading') return <h3 key={index} className="pt-1 font-semibold"><Inline parts={block.parts} {...citationProps} /></h3>;
        if (block.type === 'list') {
          const Tag = block.ordered ? 'ol' : 'ul';
          return (
            <Tag key={index} className={`space-y-1 pl-5 ${block.ordered ? 'list-decimal' : 'list-disc'}`}>
              {block.items.map((item, i) => <li key={i}><Inline parts={item} {...citationProps} /></li>)}
            </Tag>
          );
        }
        return (
          <p key={index}>
            {block.lines.map((line, i) => <React.Fragment key={i}>{i > 0 && <br />}<Inline parts={line} {...citationProps} /></React.Fragment>)}
          </p>
        );
      })}
    </div>
  );
}

function stepKind(step) {
  if (['search_ocr_text', 'search_nl'].includes(step.name)) return 'searches';
  if (step.name === 'get_snapshot_details') return 'screenshots';
  if (step.name === 'get_snapshots_by_time_range') return 'records';
  return 'archives';
}

function summarizeSteps(steps, t) {
  const counts = steps.reduce((all, step) => {
    const kind = stepKind(step);
    all[kind] = (all[kind] || 0) + 1;
    return all;
  }, {});
  return Object.entries(counts).map(([kind, count]) => t(`aiSearch.steps.${kind}`, { count })).join(t('aiSearch.steps.separator'));
}

function describeDuration(elapsedMs, t) {
  if (!Number.isFinite(elapsedMs)) return '';
  const total = Math.max(1, Math.ceil(elapsedMs / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  if (!minutes) return t('aiSearch.duration.seconds', { count: seconds });
  if (!seconds) return t('aiSearch.duration.minutes', { count: minutes });
  return t('aiSearch.duration.minutesSeconds', { minutes, seconds });
}

/** Collect all reasoning and search activity while keeping the final answer visible. */
function Activity({ steps, running, elapsedMs, t, onBrowse, children }) {
  const [open, setOpen] = useState(running);
  const detailsId = useId();
  useEffect(() => { setOpen(running); }, [running]);
  const summary = [summarizeSteps(steps, t), !running && describeDuration(elapsedMs, t)].filter(Boolean).join(t('aiSearch.steps.separator'));
  return (
    <div className="space-y-3" onClick={onBrowse}>
      <button type="button" aria-expanded={open} aria-controls={detailsId} onClick={() => setOpen((v) => !v)}
        className="flex max-w-full items-center gap-2 rounded py-1 text-left text-xs text-ide-muted transition-colors hover:text-ide-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
        {running && <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" />}
        <span>{summary || t(running ? 'aiSearch.thinking' : 'aiSearch.reasoning')}</span>
        <ChevronDown className={`h-3.5 w-3.5 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      <div id={detailsId} hidden={!open} className="space-y-4 border-l border-ide-border pl-4">{children}</div>
    </div>
  );
}

export function Steps({ steps, running, t, children }) {
  const [open, setOpen] = useState(false);
  if (!steps.length && !children) return running ? (
    <div className="flex items-center gap-2 text-xs text-ide-muted"><Loader2 className="h-3.5 w-3.5 animate-spin" />{t('aiSearch.thinking')}</div>
  ) : null;
  const summary = summarizeSteps(steps, t);
  return (
    <div className="text-left text-xs text-ide-muted">
      <button type="button" onClick={() => setOpen((v) => !v)} aria-expanded={open}
        className="flex max-w-full items-center gap-1.5 py-1 text-left transition-colors hover:text-ide-text">
        {running && <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" />}
        <span>{summary || t('aiSearch.reasoning')}</span>
        <ChevronDown className={`h-3.5 w-3.5 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && (
        <ol className="ml-1 mt-2 space-y-3 border-l border-ide-border pl-3">
          {children}
          {steps.map((step) => {
            const Icon = step.status === 'running' ? Loader2 : step.status === 'cancelled' ? Square : step.status === 'failed' ? X
              : stepKind(step) === 'searches' ? Search : stepKind(step) === 'records' ? Clock : BookOpen;
            return (
              <li key={step.id} className="relative flex items-start gap-2">
                <Icon className={`mt-0.5 h-3.5 w-3.5 shrink-0 ${step.status === 'running' ? 'animate-spin' : ''} ${step.status === 'failed' ? 'text-ide-error' : ''}`} />
                <div className="min-w-0 space-y-1">
                  <p className="break-words">{describeStep(step, t)}</p>
                  {step.status === 'done' && typeof step.itemCount === 'number' && <p>{t('aiSearch.steps.results', { count: step.itemCount })}</p>}
                  {step.status === 'cancelled' && <p>{t('aiSearch.cancelled')}</p>}
                  {step.status === 'failed' && <p className="text-ide-error">{t('aiSearch.steps.failed')}</p>}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

export function Reasoning({ text, t }) {
  const [open, setOpen] = useState(false);
  if (!text) return null;
  return (
    <div className="text-xs text-ide-muted">
      <button type="button" aria-expanded={open} onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 py-1 transition-colors hover:text-ide-text">
        {t('aiSearch.reasoning')}<ChevronDown className={`h-3 w-3 ${open ? 'rotate-180' : ''}`} />
      </button>
      <p className={`whitespace-pre-wrap break-words leading-relaxed ${open ? '' : 'line-clamp-3'}`}>{text}</p>
    </div>
  );
}

/** HitThumbnail refetches when its item changes identity, so keep it stable. */
function SourceThumbnail({ id }) {
  const item = useMemo(() => ({ screenshot_id: id }), [id]);
  return <HitThumbnail item={item} className="aspect-video w-full" />;
}

function Sources({ refs, onSelect, activeId, t }) {
  const gridRef = useRef(null);
  const titleId = useId();
  useEffect(() => {
    const grid = gridRef.current;
    if (!grid || activeId == null || getComputedStyle(grid).overflowY !== 'auto') return;
    const card = grid.querySelector(`[data-source-id="${activeId}"]`);
    if (!card) return;
    const bounds = grid.getBoundingClientRect();
    const target = card.getBoundingClientRect();
    // Scroll only the sidebar so hovering never moves the answer under the pointer.
    if (target.top < bounds.top) grid.scrollTop -= bounds.top - target.top;
    else if (target.bottom > bounds.bottom) grid.scrollTop += target.bottom - bounds.bottom;
  }, [activeId]);
  if (refs.length === 0) return null;
  return (
    <aside aria-labelledby={titleId} className="ai-search-sources min-w-0">
      <h3 id={titleId} className="mb-3 text-xs font-medium text-ide-muted">{t('aiSearch.sources')}</h3>
      <div ref={gridRef} className="ai-search-source-grid custom-scrollbar grid gap-3 p-0.5">
        {refs.map((ref) => (
          <button key={ref.id} type="button" data-source-id={ref.id} onClick={() => onSelect(ref)}
            className="group flex min-w-0 flex-col rounded-lg p-1 text-left transition-colors hover:bg-ide-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
            <SourceThumbnail id={ref.id} />
            <span data-highlighted={activeId === ref.id} className={`mt-1 flex w-full min-w-0 flex-col gap-1 rounded-b border-b-2 px-1.5 py-1.5 transition-colors ${activeId === ref.id ? 'border-ide-accent bg-ide-accent/15' : 'border-transparent group-hover:bg-ide-hover'}`}>
              <span className="flex items-center gap-1.5 text-[11px] text-ide-muted">
                <span className="font-medium text-ide-accent">#{ref.id}</span>
                <span className="truncate">{ref.process_name || ''}</span>
              </span>
              {ref.window_title && <span className="line-clamp-1 text-[11px] text-ide-text">{ref.window_title}</span>}
              {snapshotTime(ref) !== undefined && <span className="text-[10px] text-ide-muted">{formatTime(snapshotTime(ref))}</span>}
            </span>
          </button>
        ))}
      </div>
    </aside>
  );
}

function reasoningText(round) {
  return [round.reasoning, ...Array.from((round.text || '').matchAll(/<think>([\s\S]*?)(?:<\/think>|$)/g), (match) => match[1])].filter(Boolean).join('\n');
}

function SearchTurn({ c, t, onSelectResult, previous, onBrowse }) {
  const running = c.status === 'running';
  const [hoveredCitation, setHoveredCitation] = useState(null);
  const [focusedCitation, setFocusedCitation] = useState(null);
  const refs = useMemo(() => {
    const byId = new Map();
    // Earlier sightings win; later ones only fill fields that were missing.
    const add = (ref) => { if (ref?.id) byId.set(ref.id, { ...ref, ...byId.get(ref.id) }); };
    previous.forEach((turn) => {
      turn.steps?.forEach((step) => step.snapshots?.forEach(add));
      turn.outcome?.snapshots?.forEach(add);
    });
    c.steps.forEach((step) => step.snapshots?.forEach(add));
    c.outcome?.snapshots?.forEach(add);
    return byId;
  }, [c.steps, c.outcome, previous]);

  const sources = useMemo(() => {
    const text = [c.answer, ...(c.rounds || []).map((round) => round.text)].join('\n');
    const cited = citedIds(text).filter((id) => refs.has(id)).map((id) => refs.get(id));
    // Every known citation needs a card, including citations in expanded activity.
    if (cited.length > 0 || running) return cited;
    return [...refs.values()].slice(0, MAX_SOURCES);
  }, [c.answer, c.rounds, refs, running]);

  const known = useMemo(() => new Set(refs.keys()), [refs]);
  const select = (ref) => onSelectResult?.({ ...ref, screenshot_id: ref.id });
  const cite = (id) => { const ref = refs.get(id); if (ref) select(ref); };
  const citationProps = { onCite: cite, onCiteHover: setHoveredCitation, onCiteFocus: setFocusedCitation, known };
  const latestRound = c.rounds?.at(-1);
  const latestHasTools = latestRound && c.steps.some((step) => step.round === latestRound.id);
  const answerOutsideActivity = !latestHasTools || c.status === 'done';
  const hasActivity = running || c.steps.length > 0 || c.rounds?.some((round) => reasoningText(round) || round !== latestRound || latestHasTools)
    || reasoningText({ text: c.answer }) || Number.isFinite(c.elapsedMs);

  return (
    <article className={`ai-search-turn ${sources.length ? '' : 'ai-search-turn-without-sources'}`}>
      <div className="min-w-0 space-y-4">
        <p className="text-sm font-medium text-ide-text">{c.question}</p>
        {hasActivity && <Activity steps={c.steps} running={running} elapsedMs={c.elapsedMs} t={t} onBrowse={onBrowse}>
          {c.rounds?.length ? <>
            <Steps steps={c.steps.filter((step) => !c.rounds.some((round) => round.id === step.round))} running={false} t={t} />
            {c.rounds.map((round) => (
              <div key={round.id} className="space-y-3">
                <Reasoning text={reasoningText(round)} t={t} />
                {round.text && (round !== latestRound || !answerOutsideActivity) && <Answer text={round.text} {...citationProps} />}
                <Steps steps={c.steps.filter((step) => step.round === round.id)} running={false} t={t} />
              </div>
            ))}
          </> : <>
            <Reasoning text={reasoningText({ text: c.answer })} t={t} />
            <Steps steps={c.steps} running={false} t={t} />
          </>}
        </Activity>}
        {c.answer && answerOutsideActivity && <Answer text={c.answer} {...citationProps} />}
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
      </div>
      <Sources refs={sources} onSelect={select} activeId={hoveredCitation ?? focusedCitation} t={t} />
    </article>
  );
}

/**
 * AI search: the user asks a question, the model searches the history with
 * the same tools external assistants use, and answers with citations.
 */
export function AiSearchPanel({ controller: c, tabs, onSelectResult, header: Header }) {
  const { t } = useTranslation();
  const [input, setInput] = useState('');
  const inputRef = useRef(null);
  const running = c.status === 'running';
  const noProvider = c.settings && !c.provider;
  const hasConversation = Boolean(c.question || c.turns?.length);
  const canCompose = Boolean(c.settings && c.provider && c.status !== 'consent');
  const title = c.turns?.[0]?.question || c.question || t('aiSearch.newConversation');
  const { scrollRef, contentRef, following, onScroll, onWheel, resume, pause } = useFollowOutput(`${c.turns?.length || 0}:${c.question}`);

  useEffect(() => {
    if (canCompose && !running) inputRef.current?.focus();
  }, [canCompose, running, hasConversation]);

  const submit = (event) => {
    event.preventDefault();
    if (running || !canCompose) return;
    if (input.trim()) { c.start(input.trim()); setInput(''); }
  };

  const composer = (
    <form onSubmit={submit} className="flex w-full items-end gap-3 rounded-[25px] border border-ide-border bg-ide-panel p-1.5 pl-5 shadow-sm transition-colors focus-within:border-ide-accent focus-within:ring-2 focus-within:ring-ide-accent/20">
      <textarea
        ref={inputRef}
        rows={1}
        aria-label={t(hasConversation ? 'aiSearch.followUp' : 'aiSearch.placeholder')}
        placeholder={t(hasConversation ? 'aiSearch.followUp' : 'aiSearch.placeholder')}
        value={input}
        onChange={(event) => setInput(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) {
            event.preventDefault();
            submit(event);
          }
        }}
        className="custom-scrollbar block max-h-[108px] min-h-9 min-w-0 flex-1 resize-none overflow-y-auto overscroll-contain bg-transparent py-1.5 text-sm leading-6 text-ide-text outline-none placeholder:text-ide-muted [field-sizing:content]"
      />
      <button type={running ? 'button' : 'submit'} onClick={running ? c.cancel : undefined}
        disabled={!running && !input.trim()}
        title={running ? t('aiSearch.stop') : `${t('aiSearch.send')} · ${t('aiSearch.composerHint')}`}
        aria-label={t(running ? 'aiSearch.stop' : 'aiSearch.send')}
        className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-ide-accent text-white transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:opacity-40">
        {running ? <Square className="h-3.5 w-3.5" /> : <ArrowUp className="h-4 w-4" />}
      </button>
    </form>
  );

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
        <div className="mx-auto flex w-full max-w-2xl flex-col gap-5 px-6 py-12">
          <Bot className="mx-auto h-8 w-8 text-ide-accent" />
          <h2 className="text-center text-xl font-medium text-ide-text">{t('aiSearch.landing.title')}</h2>
          {composer}
          <div className="space-y-1.5">
            {t('aiSearch.landing.examples', { returnObjects: true }).map((example) => (
              <button key={example} type="button" onClick={() => { setInput(''); c.start(example); }}
                className="block w-full rounded-lg border border-ide-border px-3 py-2 text-left text-sm text-ide-text transition-colors hover:bg-ide-hover">
                {example}
              </button>
            ))}
          </div>
          <p className="text-center text-xs text-ide-muted">{t('aiSearch.landing.using', { name: c.provider.name })}</p>
        </div>
      );
    }
    return (
      <div className="ai-search-conversation mx-auto space-y-8 px-6 py-5">
        {(c.turns || []).map((turn, index) => <SearchTurn key={index} c={turn} t={t} onSelectResult={onSelectResult} previous={c.turns.slice(0, index)} onBrowse={pause} />)}
        <SearchTurn key={(c.turns || []).length} c={c} t={t} onSelectResult={onSelectResult} previous={c.turns || []} onBrowse={pause} />
      </div>
    );
  };

  return (
    <>
      <Header bordered flushBottom secondaryRow={tabs}>
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <Bot className="h-4 w-4 shrink-0 text-ide-accent" />
          <h1 title={title} className="truncate text-sm font-medium text-ide-text">{title}</h1>
        </div>
        {hasConversation && <button type="button" disabled={running || c.status === 'consent'} onClick={() => { c.reset(); setInput(''); }}
          className="flex shrink-0 items-center gap-1.5 rounded-lg border border-ide-border px-3 py-2 text-xs text-ide-text transition-colors hover:bg-ide-hover disabled:opacity-40"><Plus className="h-3.5 w-3.5" />{t('aiSearch.newConversation')}</button>}
      </Header>
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="ai-search-surface relative min-h-0 flex-1">
          <div ref={scrollRef} onScroll={onScroll} onWheel={onWheel} className="h-full overflow-y-auto custom-scrollbar [overflow-anchor:none]">
            <div ref={contentRef} className={!hasConversation ? 'flex min-h-full items-center justify-center' : undefined}>{renderBody()}</div>
          </div>
        {!following && c.question && <button type="button" onClick={resume}
          className="absolute bottom-4 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-ide-border bg-ide-panel px-3 py-2 text-xs text-ide-text shadow-lg transition-colors hover:bg-ide-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
          <ArrowDown className="h-3.5 w-3.5" />{t('aiSearch.latestOutput')}
        </button>}
        </div>
        {hasConversation && canCompose && <div className="shrink-0 bg-ide-bg px-4 py-2.5 sm:px-6">
          <div className="mx-auto w-full max-w-[880px]">{composer}</div>
        </div>}
      </div>
    </>
  );
}
