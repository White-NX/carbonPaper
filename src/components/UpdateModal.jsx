import React from 'react';
import PropTypes from 'prop-types';
import { useTranslation } from 'react-i18next';
import { Download, RefreshCw, AlertCircle, ArrowUpCircle, Loader2 } from 'lucide-react';
import { cn } from '../lib/utils';

export function UpdateModal({
  isVisible,
  updateInfo,
  downloading,
  downloadProgress,
  downloadError,
  onDownload,
  onLater,
  onClose
}) {
  const { t } = useTranslation();

  if (!isVisible || !updateInfo) return null;

  const { version, body, critical } = updateInfo;
  const progressPercent = downloadProgress
    ? Math.round((downloadProgress.downloaded / downloadProgress.contentLength) * 100)
    : 0;

  const IconComponent = critical ? AlertCircle : ArrowUpCircle;
  const iconColorClass = critical ? 'text-red-500' : 'text-ide-accent';
  const borderColorClass = critical ? 'border-red-500/50' : 'border-ide-border';

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className={cn("w-full max-w-lg bg-ide-panel border rounded-xl p-6 shadow-2xl flex flex-col max-h-[85vh]", borderColorClass)}>
        {/* Header */}
        <div className="flex items-center gap-3 mb-1 shrink-0">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center shrink-0">
            <IconComponent className={cn("w-5 h-5", iconColorClass)} />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-ide-text">
              {critical ? t('updateModal.titleCritical') : t('updateModal.title')}
            </h2>
            <p className="text-xs text-ide-muted">
              {t('updateModal.version', { version })}
            </p>
          </div>
        </div>

        {/* Content */}
        <div className="mt-4 space-y-3 flex-1 overflow-hidden flex flex-col">
          {critical && (
            <div className="p-3 bg-red-500/10 border border-red-500/20 rounded-lg shrink-0">
              <p className="text-xs text-red-400 leading-relaxed">
                {t('updateModal.criticalNotice')}
              </p>
            </div>
          )}

          <div className="flex-1 overflow-y-auto p-4 bg-ide-bg rounded-lg border border-ide-border text-sm text-ide-text/90 leading-relaxed whitespace-pre-wrap custom-scrollbar">
            {body || ''}
          </div>

          {downloadError && (
            <div className="mt-2 text-xs px-3 py-2 rounded bg-red-500/10 text-red-400 shrink-0">
              {t('updateModal.downloadFailed', { error: downloadError })}
            </div>
          )}

          {downloading && (
            <div className="w-full space-y-1.5 shrink-0 pt-2">
              <div className="flex justify-between text-xs">
                <span className="text-ide-text font-medium">{t('updateModal.downloading')}</span>
                <span className="text-ide-muted">{progressPercent}%</span>
              </div>
              <div className="w-full h-2 bg-ide-bg rounded-full overflow-hidden border border-ide-border">
                <div 
                  className="h-full bg-ide-accent rounded-full transition-all duration-300 ease-out" 
                  style={{ width: `${progressPercent}%` }}
                />
              </div>
            </div>
          )}
        </div>

        {/* Actions */}
        <div className="mt-5 flex items-center justify-end gap-2 shrink-0">
          {!critical && !downloading && (
            <button
              onClick={onLater}
              disabled={downloading}
              className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
            >
              {t('updateModal.later')}
            </button>
          )}

          {downloadError ? (
            <button
              onClick={onDownload}
              disabled={downloading}
              className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
            >
              <RefreshCw className="w-3.5 h-3.5" />
              {t('updateModal.retry')}
            </button>
          ) : (
            <button
              onClick={onDownload}
              disabled={downloading}
              className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
            >
              {downloading ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <Download className="w-3.5 h-3.5" />
              )}
              {downloading ? t('updateModal.downloading') : t('updateModal.downloadInstall')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

UpdateModal.propTypes = {
  isVisible: PropTypes.bool.isRequired,
  updateInfo: PropTypes.shape({
    version: PropTypes.string,
    body: PropTypes.string,
    critical: PropTypes.bool,
  }),
  downloading: PropTypes.bool,
  downloadProgress: PropTypes.shape({
    downloaded: PropTypes.number,
    contentLength: PropTypes.number,
  }),
  downloadError: PropTypes.string,
  onDownload: PropTypes.func.isRequired,
  onLater: PropTypes.func.isRequired,
  onClose: PropTypes.func.isRequired,
};
