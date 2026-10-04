import React, { useEffect, useId, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, ChevronRight, Circle, CircleAlert, Loader2, PauseCircle, Square } from 'lucide-react';
import { recapErrorKey } from '../../lib/recap_api';
import { recapAttemptStatus, recapDiagnosticOutcome, recapDiagnosticPeriods, recapRequestDuration } from '../../lib/recap_diagnostics';
import { ProgressBlock } from '../overlay/ProgressBlock';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';

const ACTIVE = new Set(['preparing', 'collecting', 'screening', 'waiting', 'thinking', 'writing', 'validating', 'summarizing', 'saving', 'running', 'updating']);
const PROBLEMS = new Set(['failed', 'partial', 'summaryFailed', 'summaryPending', 'cancelled', 'paused', 'interrupted']);
const TABS = ['reply', 'reasoning', 'requestDetails'];
const EXTRA_STATES = new Set(['empty', 'pending', 'summaryFailed', 'summaryPending', 'updating', 'interrupted', 'noHistory']);

function statusLabel(status, t) {
  return t(EXTRA_STATES.has(status) ? `recap.diagnostics.states.${status}` : `recap.progress.stages.${status}`);
}

function StatusIcon({ status }) {
  const Icon = status === 'ready' || status === 'empty' ? CheckCircle2
    : status === 'failed' || status === 'partial' || status === 'summaryFailed' ? CircleAlert
      : status === 'paused' ? PauseCircle : status === 'cancelled' || status === 'interrupted' ? Square : ACTIVE.has(status) ? Loader2 : Circle;
  const color = status === 'ready' || status === 'empty' ? 'text-ide-info-success'
    : status === 'failed' ? 'text-ide-error' : ['partial', 'summaryFailed'].includes(status) ? 'text-ide-warning'
      : ACTIVE.has(status) ? 'text-ide-accent' : 'text-ide-muted';
  return <Icon aria-hidden="true" className={`h-4 w-4 shrink-0 ${color} ${ACTIVE.has(status) ? 'animate-spin motion-reduce:animate-none' : ''}`} />;
}

function duration(ms, t) {
  if (!Number.isFinite(ms)) return t('recap.diagnostics.durationUnavailable');
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return seconds < 60 ? t('recap.diagnostics.seconds', { count: seconds })
    : t('recap.diagnostics.minutesSeconds', { minutes: Math.floor(seconds / 60), seconds: seconds % 60 });
}

export function DiagnosticError({ error, tone = 'error', message }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const id = useId();
  return <Banner tone={tone}>
    <p>{message || t(recapErrorKey(error), { defaultValue: t('recap.errors.unknown') })}</p>
    {error && <>
      <Button variant="ghost" size="xs" className="mt-1 -ml-2" aria-expanded={open} aria-controls={id} onClick={() => setOpen(value => !value)}>{t('recap.errorDetails')}</Button>
      {open && <pre id={id} className="mt-1 whitespace-pre-wrap break-all font-mono leading-relaxed">{error}</pre>}
    </>}
  </Banner>;
}

function AttemptDetails({ attempt, progress, now }) {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState('reply');
  const id = useId();
  const running = progress != null && progress.finished_at_ms == null && attempt.finished_at_ms == null;
  const value = tokens => tokens == null ? t(running ? 'recap.progress.pendingUsage' : 'recap.progress.unknownUsage') : tokens.toLocaleString(i18n.language);
  const text = tab === 'reasoning' ? attempt.reasoning : attempt.text;
  const chars = tab === 'reasoning' ? attempt.reasoning_chars : attempt.text_chars;
  return <div className="mt-2 rounded-lg bg-ide-bg p-3 sm:p-4">
    <div role="tablist" aria-label={t('recap.diagnostics.requestContent')} className="mb-3 flex flex-wrap gap-x-3 border-b border-ide-border">
      {TABS.map(name => <Button key={name} variant="ghost" size="xs" role="tab" id={`${id}-${name}`} aria-selected={tab === name} aria-controls={`${id}-panel`}
        tabIndex={tab === name ? 0 : -1} className={`rounded-none border-0 border-b-2 px-0 pb-2 ${tab === name ? 'border-ide-accent text-ide-accent' : 'border-transparent'}`}
        onClick={() => setTab(name)} onKeyDown={event => {
          if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
          event.preventDefault();
          const next = event.key === 'Home' ? 0 : event.key === 'End' ? TABS.length - 1 : (TABS.indexOf(name) + (event.key === 'ArrowRight' ? 1 : TABS.length - 1)) % TABS.length;
          setTab(TABS[next]);
          document.getElementById(`${id}-${TABS[next]}`)?.focus();
        }}>{t(`recap.diagnostics.tabs.${name}`)}</Button>)}
    </div>
    <div role="tabpanel" id={`${id}-panel`} aria-labelledby={`${id}-${tab}`} className="min-w-0 text-xs">
      {tab === 'requestDetails' ? <dl className="recap-request-fields grid gap-x-5 gap-y-2 tabular-nums [&>dt]:text-ide-muted [&>dd]:min-w-0 [&>dd]:break-words">
        <dt>{t('recap.diagnostics.model')}</dt><dd>{attempt.model}</dd>
        <dt>{t('recap.diagnostics.requestDuration')}</dt><dd>{duration(recapRequestDuration(attempt, now, progress), t)}</dd>
        <dt>{t('recap.progress.outputLimit')}</dt><dd>{value(attempt.max_output_tokens)}</dd>
        <dt>{t('recap.progress.inputUsage')}</dt><dd>{value(attempt.input_tokens)}</dd>
        <dt>{t('recap.progress.outputUsage')}</dt><dd>{value(attempt.output_tokens)}</dd>
        <dt>{t('recap.progress.reasoningUsage')}</dt><dd>{value(attempt.reasoning_tokens)}</dd>
        <dt>{t('recap.progress.received')}</dt><dd>{t('recap.progress.receivedCounts', { reasoning: attempt.reasoning_chars ?? 0, text: attempt.text_chars ?? 0 })}</dd>
        <dt>{t('recap.progress.requestId')}</dt><dd className="[overflow-wrap:anywhere] font-mono">{attempt.id}</dd>
        {attempt.error && <><dt>{t('recap.diagnostics.rawError')}</dt><dd className="whitespace-pre-wrap [overflow-wrap:anywhere] font-mono">{attempt.error}</dd></>}
      </dl> : <>
        {text ? <>
          <p className="mb-2 text-ide-muted">{t(running ? 'recap.diagnostics.receivingPreview' : 'recap.diagnostics.savedPreview')}{chars > [...text].length && ` · ${t('recap.progress.previewLimited')}`}</p>
          <pre className="whitespace-pre-wrap [overflow-wrap:anywhere] font-mono leading-relaxed">{text}</pre>
        </> : <p className="text-ide-muted">{t(running ? 'recap.diagnostics.waitingPreview' : 'recap.diagnostics.noPreview')}</p>}
      </>}
    </div>
  </div>;
}

function Attempt({ attempt, progress, now, onInspect }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const id = useId();
  const status = recapAttemptStatus(attempt, progress);
  return <li className="min-w-0 py-1">
    <Button variant="ghost" size="md" aria-expanded={open} aria-controls={id} onClick={() => { setOpen(value => !value); onInspect(); }}
      className={`w-full justify-start gap-2 border-0 px-2 py-2.5 text-left font-normal whitespace-normal text-ide-text ${open ? 'bg-ide-accent/10' : ''}`}>
      <StatusIcon status={status} /><span className="min-w-0 flex-1">{t(`recap.progress.attempts.${attempt.kind}`)}
        <span className={status === 'ready' ? 'sr-only' : 'ml-2 text-xs text-ide-muted'}>{statusLabel(status, t)}</span></span>
      <span className="shrink-0 text-xs tabular-nums text-ide-muted">{duration(recapRequestDuration(attempt, now, progress), t)}</span>
      <ChevronRight aria-hidden="true" className={`h-3.5 w-3.5 shrink-0 text-ide-muted ${open ? 'rotate-90' : ''}`} />
    </Button>
    {open && <div id={id}>
      {attempt.error && <div className="mt-2"><Banner tone="error">{t(recapErrorKey(attempt.error), { defaultValue: t('recap.errors.unknown') })}</Banner></div>}
      <AttemptDetails attempt={attempt} progress={progress} now={now} />
    </div>}
  </li>;
}

function Period({ period, progress, now, automaticOpen }) {
  const { t, i18n } = useTranslation();
  const [chosenOpen, setChosenOpen] = useState(null);
  const id = useId();
  const open = chosenOpen ?? automaticOpen;
  const time = ms => new Date(ms).toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
  const range = Number.isFinite(period.end_ms) ? `${time(period.start_ms)}–${time(period.end_ms)}` : t('recap.diagnostics.periodFrom', { start: time(period.start_ms) });
  const attemptProgress = attempt => period.live.some(live => live.id === attempt.id) ? progress : null;
  const durations = period.attempts.map(attempt => recapRequestDuration(attempt, now, attemptProgress(attempt)));
  const total = durations.length && durations.every(value => value != null) ? durations.reduce((sum, value) => sum + value, 0) : null;
  const visibleError = !period.active && (period.error || period.summary_error);
  const errorInAttempt = period.attempts.some(attempt => attempt.error === visibleError);
  return <li className="min-w-0 border-b border-ide-border last:border-b-0">
    <Button variant="ghost" size="md" aria-expanded={open} aria-controls={id} onClick={() => setChosenOpen(!open)}
      className="w-full justify-start gap-3 rounded-none border-0 px-0 py-4 text-left whitespace-normal text-ide-text">
      <ChevronRight aria-hidden="true" className={`h-4 w-4 shrink-0 text-ide-muted ${open ? 'rotate-90' : ''}`} />
      <span className="min-w-0 flex-1"><span className="font-semibold tabular-nums">{range}</span>
        <span className="mt-1 block text-xs font-normal text-ide-muted">{period.active ? statusLabel(period.status, t) : !period.attempts.length ? t('recap.diagnostics.noRequests') : total == null ? t('recap.diagnostics.durationUnavailable') : t('recap.diagnostics.totalRequestDuration', { duration: duration(total, t) })}</span>
      </span>
      <span className="recap-period-status flex max-w-[42%] items-center justify-end gap-1.5 text-xs font-normal"><StatusIcon status={period.status} />{statusLabel(period.active ? 'running' : period.status, t)}</span>
    </Button>
    {open && <div id={id} className="min-w-0 pb-4 pl-2 sm:pl-7">
      {!period.active && ['summaryFailed', 'summaryPending'].includes(period.status) && <div className="mb-2"><Banner tone={period.summary_error ? 'warning' : 'info'}>{t(period.summary_error ? 'recap.summaryUnavailable' : 'recap.summaryPending')}</Banner></div>}
      {visibleError && !errorInAttempt && <div className="mb-2"><DiagnosticError error={visibleError} tone={period.status === 'failed' ? 'error' : 'warning'} /></div>}
      {period.attempts.length ? <ol aria-label={t('recap.diagnostics.steps')}>
        {period.attempts.map(attempt => <Attempt key={attempt.id} attempt={attempt} progress={attemptProgress(attempt)} now={now} onInspect={() => setChosenOpen(true)} />)}
      </ol> : <p className="py-2 text-xs text-ide-muted">{t(period.active ? 'recap.diagnostics.preparingRequests' : 'recap.diagnostics.noRequests')}</p>}
    </div>}
  </li>;
}

export default function RecapProgress({ progress, batches = [], error }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now);
  const running = progress != null && progress.finished_at_ms == null;
  const finishing = running && progress.total_batches > 0 && progress.completed_batches >= progress.total_batches;
  useEffect(() => {
    if (!running) return undefined;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [running]);
  const periods = useMemo(() => recapDiagnosticPeriods(batches, progress), [batches, progress]);
  const runError = progress ? progress.error : error;
  const status = progress && progress.stage !== 'ready' ? progress.stage : runError ? 'failed' : recapDiagnosticOutcome(periods);
  const completed = periods.filter(period => ['ready', 'empty'].includes(period.status)).length;
  const current = periods.find(period => period.active) || periods.find(period => PROBLEMS.has(period.status));
  return <section aria-label={t('recap.progress.title')} className="text-sm text-ide-text">
    <div className="space-y-3 pb-3 pt-1">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div><div role="status" className="flex items-center gap-2 font-medium"><StatusIcon status={status} />{running ? t('recap.generating') : status === 'ready' ? t('recap.diagnostics.ready') : statusLabel(status, t)}</div>
          {periods.length > 0 && <p className="mt-1 pl-6 text-xs text-ide-muted">{t('recap.diagnostics.completedPeriods', { count: completed, total: periods.length })}</p>}
        </div>
        {progress && <span className="text-xs tabular-nums text-ide-muted">{t('recap.diagnostics.runDuration', { duration: duration((progress.finished_at_ms ?? now) - progress.started_at_ms, t) })}</span>}
      </div>
      {running && <ProgressBlock label={t(finishing ? 'recap.diagnostics.finishing' : 'recap.progress.completedLabel')} current={finishing ? undefined : progress.completed_batches} total={progress.total_batches} showCount={false}
        detail={!finishing && progress.total_batches > 0 ? t('recap.progress.completed', { count: progress.completed_batches, total: progress.total_batches }) : undefined} />}
      {runError && !['cancelled', 'paused'].includes(status) && <DiagnosticError error={runError} />}
    </div>
    <ol aria-label={t('recap.diagnostics.periods')}>
      {periods.map(period => <Period key={period.start_ms} period={period} progress={progress} now={now} automaticOpen={current?.start_ms === period.start_ms} />)}
    </ol>
  </section>;
}
