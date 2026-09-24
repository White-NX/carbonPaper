import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, ChevronUp } from 'lucide-react';
import { SettingsSwitch } from '../SettingsControls';
import { PII_ENTITY_TYPES } from './agentAccessConstants';

export default function PiiDetectionSection({
  piiEnabled,
  piiEntities,
  piiMaskLongNumbers,
  showPiiAdvanced,
  onPiiToggle,
  onTogglePiiAdvanced,
  onPiiEntityToggle,
  onPiiMaskLongNumbersToggle,
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
          title={t('settings.ai_embedding.content_filter.pii.title')}
          checked={piiEnabled}
          onChange={onPiiToggle}
        />
      </div>

      {piiEnabled && (
        <div className="space-y-2 mt-3">
          <button
            onClick={onTogglePiiAdvanced}
            className="flex items-center gap-1 text-xs text-ide-muted hover:text-ide-text transition-colors"
          >
            {showPiiAdvanced ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
            {t('settings.ai_embedding.content_filter.pii.entity_types_label')}
          </button>

          {showPiiAdvanced && (
            <div className="space-y-3 pl-1">
              <div className="grid grid-cols-2 gap-2">
                {PII_ENTITY_TYPES.map((entityType) => (
                  <label key={entityType} className="flex items-center gap-2 cursor-pointer text-xs text-ide-text">
                    <input
                      type="checkbox"
                      checked={Boolean(piiEntities[entityType])}
                      onChange={() => onPiiEntityToggle(entityType)}
                      className="w-3.5 h-3.5 rounded border-ide-border text-ide-accent focus:ring-ide-accent/50 bg-ide-panel"
                    />
                    {t(`settings.ai_embedding.content_filter.pii.entity_types.${entityType}`)}
                  </label>
                ))}
              </div>
              <div className="flex items-center justify-between gap-4">
                <div>
                  <p className="text-xs font-medium text-ide-text">
                    {t('settings.ai_embedding.content_filter.pii.long_numbers.title')}
                  </p>
                  <p className="text-[11px] text-ide-muted mt-0.5">
                    {t('settings.ai_embedding.content_filter.pii.long_numbers.description')}
                  </p>
                </div>
                <SettingsSwitch
                  title={t('settings.ai_embedding.content_filter.pii.long_numbers.title')}
                  checked={piiMaskLongNumbers}
                  onChange={onPiiMaskLongNumbersToggle}
                />
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
