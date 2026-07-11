import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, ChevronUp } from 'lucide-react';
import { CONTENT_FILTER_CATEGORIES, CONTENT_FILTER_LEVELS } from './agentAccessConstants';

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

export default function ContentFilterSection({
  filterLevel,
  filterEnabled,
  filterCategories,
  showAdvanced,
  onToggleAdvanced,
  onLevelChange,
  onCategoryToggle,
}) {
  const { t } = useTranslation();

  return (
    <div>
      <label className="block mb-2 font-semibold text-ide-text">
        {t('settings.ai_embedding.content_filter.level_label')}
      </label>
      <div className="space-y-1">
        {CONTENT_FILTER_LEVELS.map((level) => (
          <button
            key={level}
            onClick={() => onLevelChange(level)}
            className={`w-full flex items-start gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
              filterLevel === level
                ? 'bg-ide-accent/10'
                : 'hover:bg-ide-hover'
            }`}
          >
            <RadioDot selected={filterLevel === level} />
            <div>
              <span className="text-ide-text font-semibold text-sm">
                {t(`settings.ai_embedding.content_filter.levels.${level}.title`)}
              </span>
              <p className="text-ide-muted text-xs mt-0.5">
                {t(`settings.ai_embedding.content_filter.levels.${level}.description`)}
              </p>
            </div>
          </button>
        ))}
        {filterLevel === 'custom' && (
          <div className="flex items-start gap-3 px-3 py-2 rounded-lg bg-ide-accent/10">
            <RadioDot selected />
            <div>
              <span className="text-ide-text font-semibold text-sm">
                {t('settings.ai_embedding.content_filter.levels.custom.title')}
              </span>
              <p className="text-ide-muted text-xs mt-0.5">
                {t('settings.ai_embedding.content_filter.levels.custom.description')}
              </p>
            </div>
          </div>
        )}
      </div>

      <button
        onClick={onToggleAdvanced}
        className="flex items-center gap-1 mt-3 text-xs text-ide-muted hover:text-ide-text transition-colors"
      >
        {showAdvanced ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
        {t('settings.ai_embedding.content_filter.advanced_toggle')}
      </button>

      {showAdvanced && (
        <div className="grid grid-cols-2 gap-2 pl-1 mt-2">
          {CONTENT_FILTER_CATEGORIES.map((cat) => (
            <label key={cat} className="flex items-center gap-2 cursor-pointer text-xs text-ide-text">
              <input
                type="checkbox"
                checked={filterCategories[cat] ?? true}
                onChange={() => onCategoryToggle(cat)}
                disabled={!filterEnabled}
                className="w-3.5 h-3.5 rounded border-ide-border text-ide-accent focus:ring-ide-accent/50 bg-ide-panel disabled:opacity-50"
              />
              {t(`settings.ai_embedding.content_filter.categories.${cat}`)}
            </label>
          ))}
        </div>
      )}
    </div>
  );
}
