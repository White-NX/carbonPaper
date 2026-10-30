import React, { useId, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle, CircleDot, Inbox, PauseCircle, Plus, RefreshCw, X, Zap } from 'lucide-react';
import { StatusChip } from '../PageHeader';
import { Button } from '../ui/Button';

export default function ClusterListHeader({
  activeCount,
  pausedCount,
  pendingCount,
  isDraining,
  drainFailed,
  loading,
  onCreate,
  onRefresh,
  onProcessNow,
  onStopProcessing,
}) {
  const { t } = useTranslation();
  const statusId = useId();
  const [statusOpen, setStatusOpen] = useState(false);

  return (
    <header className="cluster-list-header shrink-0 border-b border-ide-border bg-ide-panel px-3 py-2">
      <div className="flex items-center justify-between gap-2">
        <Button variant="primary" icon={Plus} onClick={onCreate}>
          {t('smartClusters.newCluster')}
        </Button>
        <div className="flex items-center gap-1">
        {isDraining && <Button variant="ghost" size="xs" icon={X} onClick={onStopProcessing}
          title={t('smartClusters.stopDrainTooltip')} aria-label={t('smartClusters.stopDrain')}
          className="text-ide-error" />}
        <Button variant="ghost" size="xs" icon={drainFailed ? AlertCircle : Inbox}
          aria-label={t('smartClusters.statusDetails')} aria-controls={statusId} aria-expanded={statusOpen}
          title={pendingCount > 0 ? t('smartClusters.chipPending', { count: pendingCount }) : t('smartClusters.statusDetails')}
          onClick={() => setStatusOpen((open) => !open)} className={drainFailed ? 'text-ide-warning' : ''}>
          <span className="cluster-status-label">{t('smartClusters.statusDetails')}</span>
          {pendingCount > 0 && <span className="rounded bg-ide-accent/10 px-1 text-[10px] tabular-nums">{pendingCount}</span>}
        </Button>
        <Button
          variant="ghost"
          size="xs"
          icon={RefreshCw}
          loading={loading}
          onClick={onRefresh}
          title={t('smartClusters.refresh')}
          aria-label={t('smartClusters.refresh')}
        />
        </div>
      </div>
      {statusOpen && <div id={statusId} role="region" aria-label={t('smartClusters.statusDetails')} className="mt-2 flex flex-wrap items-center gap-1.5">
        <StatusChip icon={<CircleDot className="h-3 w-3" />} tone="accent">
          {t('smartClusters.chipRunning', { count: activeCount })}
        </StatusChip>
        {pausedCount > 0 && (
          <StatusChip icon={<PauseCircle className="h-3 w-3" />}>
            {t('smartClusters.chipPaused', { count: pausedCount })}
          </StatusChip>
        )}
        {pendingCount > 0 && (
          <>
            <StatusChip icon={<Inbox className="h-3 w-3" />} title={t('smartClusters.idleWarning')}>
              {t('smartClusters.chipPending', { count: pendingCount })}
            </StatusChip>
            {!isDraining && <Button
              variant="secondary"
              size="xs"
              icon={Zap}
              onClick={onProcessNow}
              title={t('smartClusters.processNowTooltip')}
              className="rounded-full"
            >
              {drainFailed ? t('smartClusters.retryFailed') : t('smartClusters.processNow')}
            </Button>}
          </>
        )}
        {drainFailed && (
          <StatusChip icon={<AlertCircle className="h-3 w-3" />}>
            {t('smartClusters.processingFailed')}
          </StatusChip>
        )}
      </div>}
    </header>
  );
}
