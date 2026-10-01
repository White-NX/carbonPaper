import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { Reasoning } from '../search/AiSearchPanel';
import { recapErrorKey } from '../../lib/recap_api';
import { ProgressBlock } from '../overlay/ProgressBlock';

function duration(start, end) {
  const seconds = Math.max(0, Math.floor((end - start) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}

export default function RecapProgress({ progress, batches = [] }) {
  const { t, i18n } = useTranslation();
  const [now, setNow] = useState(Date.now);
  const running = progress && !progress.finished_at_ms;
  useEffect(() => {
    if (!running) return undefined;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [running]);
  const liveAttempts = progress?.attempts || [];
  const liveBatches = new Set(liveAttempts.map((attempt) => attempt.batch_start_ms));
  const attempts = [...batches.filter((batch) => !liveBatches.has(batch.start_ms)).flatMap((batch) => batch.attempts || []), ...liveAttempts];
  if (!progress && attempts.length === 0) return null;
  const time = (ms) => new Date(ms).toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit' });
  const value = (tokens, waiting = false) => tokens == null ? t(waiting ? 'recap.progress.pendingUsage' : 'recap.progress.unknownUsage') : tokens.toLocaleString(i18n.language);
  return <section aria-label={t('recap.progress.title')} className="space-y-3 rounded-xl border border-ide-border bg-ide-panel p-4 text-sm">
    {progress && <div role="status" className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="flex items-center gap-2">{running && <Loader2 className="h-4 w-4 animate-spin" />}{t(`recap.progress.stages.${progress.stage}`)}</span>
        <span className="text-xs tabular-nums text-ide-muted">{duration(progress.started_at_ms, progress.finished_at_ms || now)}</span>
      </div>
      {progress.total_batches > 0 && <>
        <p className="text-xs text-ide-muted">{t('recap.progress.completed', { count: progress.completed_batches, total: progress.total_batches })}{running && progress.batch_start_ms != null && ` · ${t('recap.progress.period', { start: time(progress.batch_start_ms) })}`}</p>
        <ProgressBlock label={t('recap.progress.completedLabel')} current={progress.completed_batches} total={progress.total_batches} showCount={false} />
      </>}
    </div>}
    {attempts.length > 0 && <details>
      <summary className="cursor-pointer text-ide-muted">{t('recap.progress.title')}</summary>
      <ol className="mt-3 space-y-4 border-l border-ide-border pl-4">
        {attempts.map((attempt) => <li key={attempt.id} className="space-y-2">
          <div className="flex flex-wrap justify-between gap-2 text-xs">
            <span>{time(attempt.batch_start_ms)} · {t(`recap.progress.attempts.${attempt.kind}`)} · {attempt.model}</span>
            <span className="text-ide-muted">{t(`recap.progress.stages.${attempt.status}`)} · {duration(attempt.started_at_ms, attempt.finished_at_ms || now)}</span>
          </div>
          <div className="max-h-64 overflow-y-auto"><Reasoning text={attempt.reasoning} t={t} /></div>
          {attempt.text && <details className="text-xs text-ide-muted"><summary className="cursor-pointer">{t('recap.progress.reply')}</summary><pre className="mt-2 max-h-64 overflow-y-auto whitespace-pre-wrap break-words font-mono leading-relaxed">{attempt.text}</pre></details>}
          {(attempt.reasoning_chars > [...(attempt.reasoning || '')].length || attempt.text_chars > [...(attempt.text || '')].length) && <p className="text-xs text-ide-muted">{t('recap.progress.previewLimited')}</p>}
          {attempt.error && <p className="text-xs text-ide-error">{t(recapErrorKey(attempt.error), { defaultValue: t('recap.errors.unknown') })}</p>}
          <details className="text-xs text-ide-muted">
            <summary className="cursor-pointer">{t('recap.progress.requestDetails')}</summary>
            <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 tabular-nums">
              <dt>{t('recap.progress.outputLimit')}</dt><dd>{value(attempt.max_output_tokens)}</dd>
              <dt>{t('recap.progress.inputUsage')}</dt><dd>{value(attempt.input_tokens, !attempt.finished_at_ms)}</dd>
              <dt>{t('recap.progress.outputUsage')}</dt><dd>{value(attempt.output_tokens, !attempt.finished_at_ms)}</dd>
              <dt>{t('recap.progress.reasoningUsage')}</dt><dd>{value(attempt.reasoning_tokens, !attempt.finished_at_ms)}</dd>
              <dt>{t('recap.progress.received')}</dt><dd>{t('recap.progress.receivedCounts', { reasoning: attempt.reasoning_chars, text: attempt.text_chars })}</dd>
              <dt>{t('recap.progress.requestId')}</dt><dd className="break-all font-mono">{attempt.id}</dd>
              {attempt.error && <><dt>{t('recap.errorDetails')}</dt><dd className="break-all font-mono">{attempt.error}</dd></>}
            </dl>
          </details>
        </li>)}
      </ol>
    </details>}
  </section>;
}
