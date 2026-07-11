import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, Info, Layers } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';
import { SettingsSwitch } from '../SettingsControls';
import { CLUSTERING_INTERVAL_OPTIONS } from './advancedOptions';

export default function ClusteringTechnicalCard({
  config,
  clusteringDropdownOpen,
  onToggle,
  onToggleDropdown,
  onIntervalChange,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Layers className="w-4 h-4" />
        {t('settings.advanced.clustering.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.clustering.interval_label')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.clustering.interval_desc')}</p>
          </div>
          <div className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                onToggleDropdown();
              }}
              className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[120px]"
            >
              <span className="flex-1 text-left">{t(`settings.advanced.clustering.intervals.${config.clustering_interval || '1w'}`)}</span>
              <ChevronDown
                className={`w-4 h-4 text-ide-muted transition-transform ${clusteringDropdownOpen ? 'rotate-180' : ''}`}
              />
            </button>
            {clusteringDropdownOpen && (
              <div
                className="absolute right-0 top-full mt-2 w-40 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                onClick={(e) => e.stopPropagation()}
              >
                {CLUSTERING_INTERVAL_OPTIONS.map((interval) => (
                  <button
                    key={interval}
                    onClick={() => onIntervalChange(interval)}
                    className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${interval === (config.clustering_interval || '1w') ? 'bg-ide-accent/10' : ''}`}
                  >
                    <span className="text-sm text-ide-text">{t(`settings.advanced.clustering.intervals.${interval}`)}</span>
                    {interval === (config.clustering_interval || '1w') && (
                      <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between gap-4 border-t border-ide-border/50 pt-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.clustering.allow_full_low_memory_label')}
              <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.pacmap_hdbscan')}</SettingsHelpTooltip>
            </p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.clustering.allow_full_low_memory_desc')}</p>
          </div>
          <SettingsSwitch
            checked={config.clustering_allow_full_low_memory}
            onChange={() => onToggle('clustering_allow_full_low_memory')}
          />
        </div>

        <div className="flex items-start gap-2 p-2.5 bg-ide-panel/50 border border-ide-border/30 rounded-lg">
          <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
          <p className="text-xs text-ide-muted leading-relaxed">
            {t('settings.advanced.clustering.info')}
            <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.minilm')}</SettingsHelpTooltip>
          </p>
        </div>
      </div>
    </div>
  );
}
