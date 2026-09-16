import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, Loader2, Play, X } from 'lucide-react';
import { SettingsButton, SettingsSelect } from '../SettingsControls';
import { CLUSTERING_INTERVAL_OPTIONS } from '../advanced/advancedOptions';

export default function ClusteringScheduleCard({
  config,
  saving = false,
  monitorStatus,
  clusteringDropdownOpen,
  clusteringAdvancedOpen,
  clusteringRunning,
  clusteringProgress,
  clusteringError,
  clusteringNotice,
  rangeStart,
  rangeEnd,
  lastClusteringRunLabel,
  onToggleDropdown,
  onToggleAdvanced,
  onIntervalChange,
  onRangeStartChange,
  onRangeEndChange,
  onRunClustering,
  onClearClusteringError,
  onClearClusteringNotice,
}) {
  const { t } = useTranslation();
  const progressLabel = clusteringProgress && ({
    preparing: t('settings.features.management.clustering.progress.preparing'),
    waiting_for_worker: t('settings.features.management.clustering.progress.waiting'),
    clustering: t('settings.features.management.clustering.progress.clustering'),
    saving: t('settings.features.management.clustering.progress.saving'),
    queued: t('settings.features.management.clustering.queued'),
    waiting_for_index: t('settings.features.management.clustering.progress.waiting_for_records'),
    awaiting_choice: t('settings.features.management.clustering.progress.awaiting_choice'),
    retry_wait: t('settings.features.management.clustering.progress.retry_wait'),
    interrupted: t('settings.features.management.clustering.progress.interrupted'),
    completed: t('settings.features.management.clustering.progress.completed'),
  }[clusteringProgress.phase] || t('settings.features.management.clustering.progress.preparing'));
  const progressCountLabel = Number.isFinite(clusteringProgress?.prepared_count)
    ? t('settings.features.management.clustering.progress.prepared_count', {
      count: clusteringProgress.prepared_count.toLocaleString(),
    })
    : null;

  if (!config.clustering_enabled) return null;

  return (
    <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0">
          <p className="text-sm text-ide-text font-medium">{t('settings.features.management.clustering.label', '任务聚类')}</p>
          <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.clustering.description', '使用 MiniLM 模型将相似活动分组为长期任务')}</p>
        </div>
      </div>

      <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center justify-between gap-4">
        <div className="flex-1 min-w-0">
          <p className="text-sm text-ide-muted">{t('settings.features.management.clustering.interval_label', '自动聚类间隔')}</p>
        </div>
        <SettingsSelect label={t('settings.features.management.clustering.interval_label')} value={config.clustering_interval || '1w'} disabled={saving}
          options={CLUSTERING_INTERVAL_OPTIONS.map((value) => ({ value, label: t(`settings.advanced.clustering.intervals.${value}`) }))} onChange={onIntervalChange} />
      </div>

      {clusteringProgress && (clusteringProgress.active || (progressLabel !== clusteringError && progressLabel !== clusteringNotice)) && (
        <div className="mt-3 space-y-2 rounded-lg border border-ide-border bg-ide-panel p-3">
          <div role="status" aria-live="polite" aria-atomic="true" className="space-y-1">
            <p className="text-xs font-medium text-ide-text">{progressLabel}</p>
            {progressCountLabel && (
              <p className="text-xs text-ide-muted tabular-nums">
                {progressCountLabel}
              </p>
            )}
          </div>
          {clusteringProgress.active && (
            <progress aria-label={progressLabel} className="block h-1.5 w-full accent-ide-accent" />
          )}
        </div>
      )}

      {clusteringError && (
        <div role="alert" className="mt-3 flex items-start gap-2 px-2.5 py-2 bg-red-500/10 border border-red-500/30 rounded-lg">
          <X className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5 cursor-pointer" onClick={onClearClusteringError} />
          <div className="text-xs text-red-400">
            <p>{clusteringError}</p>
            {progressLabel === clusteringError && progressCountLabel && <p className="mt-1 tabular-nums">{progressCountLabel}</p>}
          </div>
        </div>
      )}
      {clusteringNotice && !clusteringProgress?.active && (
        <div className="mt-3 flex items-start gap-2 px-2.5 py-2 bg-ide-accent/10 border border-ide-accent/30 rounded-lg">
          <X className="w-3.5 h-3.5 text-ide-accent shrink-0 mt-0.5 cursor-pointer" onClick={onClearClusteringNotice} />
          <div className="text-xs text-ide-text">
            <p>{clusteringNotice}</p>
            {progressLabel === clusteringNotice && progressCountLabel && <p className="mt-1 text-ide-muted tabular-nums">{progressCountLabel}</p>}
          </div>
        </div>
      )}

      <div className="mt-3 pt-3 border-t border-ide-border/50">
        <button
          type="button"
          onClick={onToggleAdvanced}
          className="flex w-full items-center justify-between gap-3 text-left"
        >
          <span className="text-sm text-ide-muted">{t('settings.features.management.clustering.advanced_label', '高级')}</span>
          <ChevronDown className={`w-4 h-4 text-ide-muted transition-transform ${clusteringAdvancedOpen ? 'rotate-180' : ''}`} />
        </button>

        {clusteringAdvancedOpen && (
          <div className="mt-3 space-y-3">
            <div className="grid grid-cols-1 sm:grid-cols-[1fr_auto_1fr] gap-2 items-center">
              <input
                type="date"
                aria-label={t('settings.features.management.clustering.rangeStart')}
                value={rangeStart}
                disabled={clusteringRunning}
                onChange={(e) => onRangeStartChange(e.target.value)}
                className="px-3 py-2 text-xs bg-ide-panel border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
              />
              <span className="hidden sm:block text-xs text-ide-muted">-</span>
              <input
                type="date"
                aria-label={t('settings.features.management.clustering.rangeEnd')}
                value={rangeEnd}
                disabled={clusteringRunning}
                onChange={(e) => onRangeEndChange(e.target.value)}
                className="px-3 py-2 text-xs bg-ide-panel border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
              />
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <SettingsButton
                onClick={onRunClustering}
                disabled={clusteringRunning || monitorStatus !== 'running'}
                variant="primary"
                icon={clusteringRunning ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : Play}
              >
                {t('settings.features.management.clustering.run_now', '立即运行聚类')}
              </SettingsButton>
              <span className="text-[11px] text-ide-muted">
                {t('tasks.lastRun')}: {lastClusteringRunLabel}
              </span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
