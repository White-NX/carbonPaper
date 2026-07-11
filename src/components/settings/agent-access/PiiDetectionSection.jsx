import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, ChevronUp, Download, Loader2, RefreshCw } from 'lucide-react';
import { SettingsSwitch } from '../SettingsControls';
import { SPACY_MODELS } from './agentAccessConstants';

export default function PiiDetectionSection({
  piiEnabled,
  piiEntities,
  spacyModels,
  downloadingModel,
  recheckLoading,
  showPiiAdvanced,
  onPiiToggle,
  onTogglePiiAdvanced,
  onPiiEntityToggle,
  onDownloadModel,
  onForceRecheck,
}) {
  const { t } = useTranslation();

  return (
    <div>
      <div className="flex items-center justify-between gap-4 mb-2">
        <div>
          <label className="block font-semibold text-ide-text">
            {t('settings.ai_embedding.content_filter.pii.title')}
          </label>
          <p className="text-xs text-ide-muted mt-0.5">
            {t('settings.ai_embedding.content_filter.pii.description')}
          </p>
        </div>
        <SettingsSwitch
          checked={piiEnabled}
          onChange={onPiiToggle}
        />
      </div>

      {piiEnabled && (
        <div className="space-y-2 mt-3">
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <p className="text-xs font-medium text-ide-text">{t('settings.ai_embedding.content_filter.pii.model_status')}</p>
              <button
                onClick={onForceRecheck}
                disabled={recheckLoading}
                className="flex items-center gap-1 px-2 py-1 text-[10px] text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50"
              >
                <RefreshCw className={`w-3 h-3 ${recheckLoading ? 'animate-spin' : ''}`} />
                {t('settings.ai_embedding.content_filter.pii.recheck')}
              </button>
            </div>
            {SPACY_MODELS.map(({ key, label, lang }) => {
              const currentLang = localStorage.getItem('language') || 'zh-CN';
              const isActive = (currentLang.startsWith('zh') && lang === 'zh')
                || (!currentLang.startsWith('zh') && lang === 'en');
              const installed = spacyModels[key]?.installed;
              const isDownloading = downloadingModel === key;
              return (
                <div key={key} className="flex items-center justify-between px-3 py-1.5 bg-ide-panel rounded-lg">
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="text-xs text-ide-text">{label}</span>
                    {isActive && (
                      <span className="px-1.5 py-0.5 bg-ide-accent/20 text-ide-accent text-[10px] rounded shrink-0">
                        {t('settings.ai_embedding.content_filter.pii.model_active')}
                      </span>
                    )}
                  </div>
                  <div className="flex items-center gap-2 shrink-0">
                    {installed ? (
                      <span className="text-xs text-green-400">
                        {t('settings.ai_embedding.content_filter.pii.model_installed')}
                      </span>
                    ) : isDownloading ? (
                      <span className="flex items-center gap-1 text-xs text-ide-muted">
                        <Loader2 className="w-3 h-3 animate-spin" />
                        {t('settings.ai_embedding.content_filter.pii.model_downloading')}
                      </span>
                    ) : (
                      <button
                        onClick={() => onDownloadModel(key)}
                        disabled={!!downloadingModel}
                        className="flex items-center gap-1 px-2 py-1 text-xs text-ide-accent hover:bg-ide-accent/10 rounded transition-colors disabled:opacity-50"
                      >
                        <Download className="w-3 h-3" />
                        {t('settings.ai_embedding.content_filter.pii.model_download')}
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
            <p className="text-[11px] text-ide-muted px-1">
              {t('settings.ai_embedding.content_filter.pii.model_note')}
            </p>
          </div>

          <button
            onClick={onTogglePiiAdvanced}
            className="flex items-center gap-1 text-xs text-ide-muted hover:text-ide-text transition-colors"
          >
            {showPiiAdvanced ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
            {t('settings.ai_embedding.content_filter.pii.entity_types_label')}
          </button>

          {showPiiAdvanced && (
            <div className="grid grid-cols-2 gap-2 pl-1">
              {Object.keys(piiEntities).map((entityType) => (
                <label key={entityType} className="flex items-center gap-2 cursor-pointer text-xs text-ide-text">
                  <input
                    type="checkbox"
                    checked={piiEntities[entityType]}
                    onChange={() => onPiiEntityToggle(entityType)}
                    className="w-3.5 h-3.5 rounded border-ide-border text-ide-accent focus:ring-ide-accent/50 bg-ide-panel"
                  />
                  {t(`settings.ai_embedding.content_filter.pii.entity_types.${entityType}`)}
                </label>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
