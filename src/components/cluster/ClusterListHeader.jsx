import React from 'react';
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

  return (
    <header className="shrink-0 border-b border-ide-border bg-ide-panel px-3 py-3">
      <div className="flex items-center justify-between gap-2">
        <Button variant="primary" icon={Plus} onClick={onCreate}>
          {t('smartClusters.newCluster')}
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
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
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
            <Button
              variant="secondary"
              size="xs"
              icon={isDraining ? X : Zap}
              onClick={isDraining ? onStopProcessing : onProcessNow}
              title={isDraining ? t('smartClusters.stopDrainTooltip') : t('smartClusters.processNowTooltip')}
              className="rounded-full"
            >
              {isDraining ? t('smartClusters.stopDrain') : drainFailed ? t('smartClusters.retryFailed') : t('smartClusters.processNow')}
            </Button>
          </>
        )}
        {drainFailed && (
          <StatusChip icon={<AlertCircle className="h-3 w-3" />}>
            {t('smartClusters.processingFailed')}
          </StatusChip>
        )}
      </div>
    </header>
  );
}
