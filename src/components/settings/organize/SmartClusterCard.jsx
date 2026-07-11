import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Download, Loader2, RefreshCw, RotateCcw, Sparkles, Zap } from 'lucide-react';
import { SettingsButton } from '../SettingsControls';

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
            {t('settings.features.management.smartCluster.label', '智能聚类（按描述自动归档）')}
          </p>
          <p className="text-xs text-ide-muted mt-1">
            {t('settings.features.management.smartCluster.description', '输入一句话描述（如 "对加利福尼亚山脉的研究"），自动归档相关快照。仅在系统空闲时计算。')}
          </p>
        </div>
      </div>

      {config.smart_cluster_enabled && !config.clustering_enabled && (
        <div className="mt-4 flex items-start gap-2.5 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
          <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0 mt-0.5" />
          <p className="text-xs leading-relaxed text-ide-warning-muted">
            {t('settings.features.management.smartCluster.clusteringDisabledWarning', '任务聚类已关闭。智能聚类仍可在重扫时运行，但由于缺少截图采集阶段的自动文本向量化，候选召回需要临时编码，可能导致处理速度显著下降并增加资源占用。')}
          </p>
        </div>
      )}

      {!scModelAvailable && !scDownloading && !scDownloadError && (
        <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center justify-between gap-3">
          <p className="text-xs text-ide-muted">
            {t('settings.features.management.smartCluster.modelNotDownloaded', 'bge-reranker-v2-m3 (uint8, ~570MB) 尚未下载')}
          </p>
          <SettingsButton
            onClick={onDownloadReranker}
            variant="primary"
            icon={Download}
          >
            {t('settings.features.management.smartCluster.downloadModel', '下载模型')}
          </SettingsButton>
        </div>
      )}

      {scDownloading && (
        <div className="mt-4 pt-4 border-t border-ide-border/50">
          <div className="flex items-center gap-2 text-xs text-ide-muted mb-2">
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
            {t('settings.features.management.smartCluster.downloading', '正在下载…')}
          </div>
          <textarea
            readOnly
            value={scDownloadLog.slice(-12).join('\n')}
            rows={6}
            className="w-full bg-ide-panel border border-ide-border rounded p-2 text-[11px] font-mono text-ide-muted resize-none"
          />
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
            {t('settings.features.management.smartCluster.retry', '重试')}
          </SettingsButton>
        </div>
      )}

      {scModelAvailable && config.smart_cluster_enabled && (
        <div className="mt-4 pt-4 border-t border-ide-border/50 space-y-3">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="text-ide-muted">
              {t('settings.features.management.smartCluster.status', '状态')}:
            </span>
            <span className="text-ide-text flex items-center gap-3">
              <span>{t('settings.features.management.smartCluster.pending', '待处理')}: <span className="font-mono">{scStatus?.pending_count ?? '--'}</span></span>
              <span className="opacity-50">·</span>
              <span>{t('settings.features.management.smartCluster.activeClusters', '启用的聚类')}: <span className="font-mono">{scStatus?.enabled_cluster_count ?? 0}/{scStatus?.total_cluster_count ?? 0}</span></span>
            </span>
          </div>
          <div className="flex items-center gap-2 flex-wrap">
            <SettingsButton
              onClick={onDrainNow}
              icon={Zap}
            >
              {t('settings.features.management.smartCluster.drainNow', '立即处理待处理队列')}
            </SettingsButton>
            <SettingsButton
              onClick={onRescanAll}
              icon={RefreshCw}
            >
              {t('settings.features.management.smartCluster.rescanAll', '全部重新匹配 hot 层')}
            </SettingsButton>
          </div>
        </div>
      )}
    </div>
  );
}
