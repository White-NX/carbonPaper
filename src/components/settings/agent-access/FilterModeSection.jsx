import React from 'react';
import { useTranslation } from 'react-i18next';
import { CONTENT_FILTER_MODES } from './agentAccessConstants';

function RadioDot({ selected }) {
  return (
    <div className={`mt-1 w-4 h-4 rounded-full border-2 shrink-0 flex items-center justify-center ${
      selected ? 'border-ide-accent' : 'border-ide-muted'
    }`}>
      {selected && (
        <div className="w-2 h-2 rounded-full bg-ide-accent" />
      )}
    </div>
  );
}

export default function FilterModeSection({
  filterMode,
  onFilterModeChange,
}) {
  const { t } = useTranslation();

  return (
    <div>
      <label className="block mb-2 font-semibold text-ide-text">
        {t('settings.ai_embedding.content_filter.filter_mode.label')}
      </label>
      <div className="space-y-1">
        {CONTENT_FILTER_MODES.map((mode) => (
          <button
            key={mode}
            onClick={() => onFilterModeChange(mode)}
            className={`w-full flex items-start gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
              filterMode === mode
                ? 'bg-ide-accent/10'
                : 'hover:bg-ide-hover'
            }`}
          >
            <RadioDot selected={filterMode === mode} />
            <div>
              <span className="text-ide-text font-semibold text-sm">
                {t(`settings.ai_embedding.content_filter.filter_mode.${mode}.title`)}
              </span>
              <p className="text-ide-muted text-xs mt-0.5">
                {t(`settings.ai_embedding.content_filter.filter_mode.${mode}.description`)}
              </p>
            </div>
          </button>
        ))}
      </div>
    </div>
  );
}
