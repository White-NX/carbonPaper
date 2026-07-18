import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Database, Loader2, RefreshCw, RotateCcw } from 'lucide-react';
import { SettingsButton } from '../SettingsControls';
import { SettingsCard } from '../SettingsPrimitives';

export default function IndexHealthCard({
  indexHealth,
  indexHealthLoading,
  indexHealthError,
  vectorRetrying,
  vectorRetryBacklog,
  deleteQueuePending,
  lastIndexingError,
  lastIndexingErrorAt,
  storageIpcLabel,
  storageIpcRetryAfter,
  monitorStatus,
  onRefresh,
  onRetryVectorIndexing,
  formatIndexCount,
}) {
  const { t } = useTranslation();
  const canUseMonitor = monitorStatus === 'running' || indexHealth?.monitor_available;

  return (
    <SettingsCard>
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center gap-2 min-w-0">
          <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
            <Database className="w-4 h-4" />
          </div>
          <div className="min-w-0">
            <h3 className="font-semibold">{t('settings.features.management.indexHealth.label', '索引健康')}</h3>
            <p className="text-[11px] text-ide-muted">
              {t('settings.features.management.indexHealth.description', '截图、OCR、向量索引和后台重试队列的当前状态')}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <button
            type="button"
            onClick={() => onRefresh({ refreshVector: canUseMonitor })}
            disabled={indexHealthLoading}
            className="p-1.5 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            title={t('settings.features.management.indexHealth.refresh', '刷新')}
          >
            <RefreshCw className={`w-4 h-4 ${indexHealthLoading ? 'animate-spin' : ''}`} />
          </button>
          <SettingsButton
            onClick={onRetryVectorIndexing}
            disabled={vectorRetrying || !vectorRetryBacklog || !canUseMonitor}
            title={t('settings.features.management.indexHealth.retry', '重试失败向量')}
            icon={vectorRetrying ? <Loader2 className="w-3 h-3 animate-spin" /> : RotateCcw}
          >
            {t('settings.features.management.indexHealth.retry', '重试失败向量')}
          </SettingsButton>
        </div>
      </div>

      <div className="mt-4 grid grid-cols-2 md:grid-cols-4 gap-x-4 gap-y-3 text-xs">
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.screenshots', '截图')}</p>
          <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.screenshots_count)}</p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.ocrRows', 'OCR 行')}</p>
          <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.ocr_rows_count)}</p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.vectorRows', '向量')}</p>
          <p className="mt-1 font-mono text-ide-text">
            {formatIndexCount(indexHealth?.vector_rows_count, indexHealth?.worker_started === false ? t('settings.features.management.indexHealth.notLoaded', '未加载') : '—')}
          </p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.vectorRetry', '向量重试')}</p>
          <p className="mt-1 font-mono text-ide-text">{formatIndexCount(vectorRetryBacklog)}</p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.deleteQueue', '删除队列')}</p>
          <p className="mt-1 font-mono text-ide-text">{formatIndexCount(deleteQueuePending)}</p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.smartPending', '智能聚类待处理')}</p>
          <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.smart_cluster_pending_count)}</p>
        </div>
        <div>
          <p className="text-ide-muted">{t('settings.features.management.indexHealth.storageIpc', '存储 IPC')}</p>
          <p className="mt-1 font-mono text-ide-text">
            {storageIpcLabel}
            {storageIpcRetryAfter ? ` ${storageIpcRetryAfter}s` : ''}
          </p>
        </div>
      </div>

      {(indexHealthError || indexHealth?.monitor_error || lastIndexingError) && (
        <div className="mt-4 flex items-start gap-2 px-2.5 py-2 bg-red-500/10 border border-red-500/30 rounded-lg">
          <AlertTriangle className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5" />
          <div className="min-w-0 text-xs text-red-300">
            <p className="break-all">{indexHealthError || indexHealth?.monitor_error || lastIndexingError}</p>
            {lastIndexingErrorAt && (
              <p className="mt-1 text-red-300/70">{lastIndexingErrorAt}</p>
            )}
          </div>
        </div>
      )}
    </SettingsCard>
  );
}
