import React, { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import useRecapProgress from '../../hooks/useRecapProgress';
import { ProgressBlock } from '../overlay/ProgressBlock';
import { RecapError } from './RecapDialogs';

/** Streaming updates stop here, rather than invalidating the reading surface. */
export default function RecapRunStatus({ date, active, onRunning, onComplete }) {
  const { t } = useTranslation();
  const { progress, error } = useRecapProgress(date, active, true, false);
  const running = Boolean(progress && !progress.finished_at_ms);
  const finished = progress?.finished_at_ms;
  useEffect(() => { onRunning(running); }, [running, onRunning]);
  useEffect(() => { if (finished) onComplete(); }, [finished, onComplete]);
  if (error) return <div className="border-b border-ide-border px-6 py-2"><RecapError error={error} /></div>;
  if (!running) return null;
  return <div className="shrink-0 border-b border-ide-border bg-ide-panel px-6 py-3" role="status">
    <div className="mx-auto max-w-[1120px]"><ProgressBlock label={t(progress.stage === 'paused' ? 'recap.progress.stages.paused' : 'recap.generating')}
      current={progress.completed_batches} total={progress.total_batches} showCount={false}
      detail={progress.total_batches > 0 ? t('recap.progress.completed', { count: progress.completed_batches, total: progress.total_batches }) : undefined} /></div>
  </div>;
}
