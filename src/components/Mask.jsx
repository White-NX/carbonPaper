import React from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RotateCcw, Download, X } from 'lucide-react';
import { useRequiredModelDownload } from '../hooks/useRequiredModelDownload';


export default function Mask({ modelsNeedDownload, missingModels, onModelsDownloadComplete }) {
  const { t } = useTranslation();
  const {
    modelDownloadLog,
    modelDownloadError,
    modelDownloadLogRef,
    isClosedByUser,
    setIsClosedByUser,
    retryModelDownload,
  } = useRequiredModelDownload({
    modelsNeedDownload,
    missingModels,
    onModelsDownloadComplete,
    t,
  });

  if (!modelsNeedDownload || isClosedByUser) return null;

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <Download className="w-12 h-12 mb-4 opacity-50" />
      <p className="text-lg font-semibold">{t('mask.model_download.title')}</p>
      <p className="text-sm opacity-70 mt-1">{t('mask.model_download.subtitle')}</p>

      <div className="mt-6 w-full max-w-2xl px-6 relative">
        <button
          onClick={() => setIsClosedByUser(true)}
          className="absolute -top-10 right-6 text-ide-muted hover:text-ide-text transition-colors flex items-center gap-1 text-xs px-2.5 py-1 bg-ide-panel border border-ide-border rounded-md"
          title={t('mask.model_download.run_in_background', '后台运行')}
        >
          <X size={14} />
          {t('mask.model_download.run_in_background', '后台运行')}
        </button>
        <textarea
          ref={modelDownloadLogRef}
          readOnly
          value={modelDownloadLog.join('\n')}
          rows={10}
          className={`w-full bg-ide-bg border ${modelDownloadError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${modelDownloadError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
        />
        {modelDownloadError ? (
          <div className="mt-3 flex items-center gap-3">
            <span className="text-xs text-rose-400">{t('mask.model_download.failed', { error: modelDownloadError })}</span>
            <button
              onClick={retryModelDownload}
              className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
            >
              <RotateCcw className="w-3 h-3" />
              {t('mask.model_download.retry')}
            </button>
          </div>
        ) : (
          <div className="mt-3 flex items-center gap-2 text-xs text-ide-muted">
            <Loader2 className="w-4 h-4 animate-spin" />
            {t('mask.model_download.syncing')}
          </div>
        )}
      </div>
    </div>
  );
}
