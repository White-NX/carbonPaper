import React from 'react';
import { useTranslation } from 'react-i18next';
import { Check, Copy, RefreshCw } from 'lucide-react';

export default function AccessTokenRow({
  tokenCopied,
  actionLoading,
  onCopyCurrentToken,
  onRequestResetToken,
}) {
  const { t } = useTranslation();

  return (
    <div className="flex items-center justify-between gap-4">
      <div className="flex-1">
        <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.token.title')}</label>
        <p className="text-xs text-ide-muted">{t('settings.ai_embedding.token.description')}</p>
        {tokenCopied && (
          <p className="text-xs text-green-400 mt-1">{t('settings.ai_embedding.token.copied')}</p>
        )}
      </div>
      <button
        onClick={onCopyCurrentToken}
        disabled={actionLoading}
        className="shrink-0 px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-50"
      >
        {tokenCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : <Copy className="w-3.5 h-3.5" />}
        {t('settings.ai_embedding.token.copy')}
      </button>
      <button
        onClick={onRequestResetToken}
        disabled={actionLoading}
        className="shrink-0 px-3 py-1.5 text-xs text-red-400 hover:text-red-300 hover:bg-red-500/10 border border-red-500/30 rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-50"
      >
        <RefreshCw className="w-3.5 h-3.5" />
        {t('settings.ai_embedding.token.reset')}
      </button>
    </div>
  );
}
