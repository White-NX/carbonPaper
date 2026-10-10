import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Download, Loader2, RefreshCw, RotateCcw, Sparkles, Zap } from 'lucide-react';
import { SettingsButton } from '../SettingsControls';
import { SettingsDisclosure } from '../SettingsPrimitives';

export default function SmartClusterCard({
  config,
  scModelAvailable,
  scStatus,
  scDownloading,
  scDownloadLog,
  scDownloadError,
  onDownloadReranker,
  onDrainNow,
  onRescanAll,
}) {
  const { t } = useTranslation();

  if (!config.smart_cluster_enabled && scModelAvailable && !scDownloading && !scDownloadError) {
    return null;
  }

  return (
    <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
      <div className="flex items-start justify-between gap-4">
        <div className="flex-1 min-w-0">
          <p className="text-sm text-ide-text font-medium flex items-center gap-1.5">
            <Sparkles className="w-3.5 h-3.5 text-ide-accent" />
            {t('settings.features.management.smartCluster.label')}
          </p>
          <p className="text-xs text-ide-muted mt-1">
            {t('settings.features.management.smartCluster.description')}
          </p>
        </div>
      </div>

      {!scModelAvailable && !scDownloading && !scDownloadError && (
        <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center justify-between gap-3">
          <p className="text-xs text-ide-muted">
            {t('settings.features.management.smartCluster.modelNotDownloaded')}
          </p>
          <SettingsButton
            onClick={onDownloadReranker}
            variant="primary"
            icon={Download}
          >
            {t('settings.features.management.smartCluster.downloadModel')}
          </SettingsButton>
        </div>
      )}

      {scDownloading && (
        <div className="mt-4 pt-4 border-t border-ide-border/50">
          <div className="flex items-center gap-2 text-xs text-ide-muted mb-2">
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
            {t('settings.features.management.smartCluster.downloading')}
          </div>
          <progress aria-label={t('settings.features.management.smartCluster.downloading')} className="h-1.5 w-full accent-ide-accent" />
          <SettingsDisclosure title={t('settings.details.download')}>
          <textarea
            readOnly
            value={scDownloadLog.slice(-12).join('\n')}
            rows={6}
            className="w-full bg-ide-panel border border-ide-border rounded p-2 text-[11px] font-mono text-ide-muted resize-none"
          />
          </SettingsDisclosure>
        </div>
      )}

      {scDownloadError && (
        <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center gap-2">
          <span className="flex-1 text-xs text-rose-400 break-all">{scDownloadError}</span>
          <SettingsButton
            onClick={onDownloadReranker}
            variant="primary"
            size="xs"
            icon={RotateCcw}
          >
            {t('settings.features.management.smartCluster.retry')}
          </SettingsButton>
        </div>
      )}

      {scModelAvailable && config.smart_cluster_enabled && (
        <div className="mt-4 pt-4 border-t border-ide-border/50 space-y-3">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="text-ide-muted">
              {t('settings.features.management.smartCluster.status')}:
            </span>
            <span className="text-ide-text flex items-center gap-3">
              <span>{t('settings.features.management.smartCluster.pending')}: <span className="font-mono">{scStatus?.pending_count ?? '--'}</span></span>
              <span className="opacity-50">·</span>
              <span>{t('settings.features.management.smartCluster.activeClusters')}: <span className="font-mono">{scStatus?.enabled_cluster_count ?? 0}/{scStatus?.total_cluster_count ?? 0}</span></span>
            </span>
          </div>
          <div className="flex items-center gap-2 flex-wrap">
            <SettingsButton
              onClick={onDrainNow}
              icon={Zap}
            >
              {t('settings.features.management.smartCluster.drainNow')}
            </SettingsButton>
            <SettingsButton
              onClick={onRescanAll}
              icon={RefreshCw}
            >
              {t('settings.features.management.smartCluster.rescanAll')}
            </SettingsButton>
          </div>
        </div>
      )}
    </div>
  );
}
