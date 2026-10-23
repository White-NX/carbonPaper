import React, { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle } from 'lucide-react';
import useRecapProgress from '../../hooks/useRecapProgress';
import { recapErrorKey } from '../../lib/recap_api';
import { ProgressRing } from '../overlay/ProgressBlock';
import { Button } from '../ui/Button';

/** Streaming updates stop here, rather than invalidating the reading surface. */
export default function RecapRunStatus({ date, active, pending = false, updated, onRunning, onComplete, onDetails }) {
  const { t, i18n } = useTranslation();
  const { progress, error } = useRecapProgress(date, active, true, false);
  const running = Boolean(progress && progress.finished_at_ms == null);
  const finished = progress?.finished_at_ms;
  useEffect(() => { onRunning(running); }, [running, onRunning]);
  useEffect(() => { if (finished != null) onComplete(); }, [finished, onComplete]);
  if (error) return <div role="alert" className="ml-auto min-w-0">
    <Button variant="ghost" size="xs" icon={AlertCircle} onClick={onDetails} aria-haspopup="dialog"
      title={t(recapErrorKey(error))} aria-label={`${t(recapErrorKey(error))} · ${t('recap.generationDetails')}`}
      className="max-w-full min-w-0 text-[11px] font-normal text-ide-error">
      <span className="truncate">{t('recap.runStatus.unavailable')}</span>
    </Button>
  </div>;
  if (!running && !(pending && !progress)) return updated
    ? <span className="ml-auto hidden min-w-0 truncate text-[11px] text-ide-muted sm:block">{updated}</span> : null;
  const total = progress?.total_batches;
  const hasCount = Number.isFinite(total) && total > 0 && Number.isFinite(progress?.completed_batches);
  const count = hasCount ? Math.min(total, Math.max(0, progress.completed_batches)) : null;
  const paused = progress?.stage === 'paused';
  const finishing = hasCount && count >= total;
  const label = t(paused ? 'recap.runStatus.paused' : finishing ? 'recap.runStatus.finishing' : 'recap.runStatus.organizing');
  const description = t(paused ? 'recap.progress.stages.paused' : finishing ? 'recap.diagnostics.finishing' : 'recap.generating');
  const detail = hasCount ? t('recap.progress.completed', { count, total }) : null;
  const title = [description, detail].filter(Boolean).join(' · ');
  return <div role="status" className="ml-auto min-w-0">
    <Button variant="ghost" size="xs" onClick={onDetails} aria-haspopup="dialog"
      title={title} aria-label={`${title} · ${t('recap.generationDetails')}`}
      icon={<ProgressRing label={t(finishing && !paused ? 'recap.diagnostics.finishing' : 'recap.progress.completedLabel')}
        current={finishing && !paused ? undefined : count} total={total} active={!paused} />}
      className="max-w-full min-w-0 text-[11px] font-normal">
      <span className="truncate">{label}{hasCount && <span className="tabular-nums"> · {count.toLocaleString(i18n.language)}/{total.toLocaleString(i18n.language)}</span>}</span>
    </Button>
  </div>;
}
