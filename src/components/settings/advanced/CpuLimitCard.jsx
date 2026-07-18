import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ChevronDown, Cpu } from 'lucide-react';
import { SettingsSwitch } from '../SettingsControls';
import { CPU_PERCENT_OPTIONS } from './advancedOptions';

export default function CpuLimitCard({
  config,
  monitorStatus,
  cpuDropdownOpen,
  cpuChanged,
  onToggle,
  onToggleDropdown,
  onPercentChange,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Cpu className="w-4 h-4" />
        {t('settings.advanced.cpu.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.cpu.label')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.cpu.description')}</p>
          </div>
          <SettingsSwitch
            checked={config.cpu_limit_enabled}
            onChange={() => onToggle('cpu_limit_enabled')}
          />
        </div>

        {config.cpu_limit_enabled && (
          <div className="flex items-center justify-between gap-4">
            <p className="text-sm text-ide-muted">{t('settings.advanced.cpu.percent_label')}</p>
            <div className="relative">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleDropdown();
                }}
                className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[100px]"
              >
                <span className="flex-1 text-left">{config.cpu_limit_percent}%</span>
                <ChevronDown
                  className={`w-4 h-4 text-ide-muted transition-transform ${cpuDropdownOpen ? 'rotate-180' : ''}`}
                />
              </button>
              {cpuDropdownOpen && (
                <div
                  className="absolute right-0 top-full mt-2 w-32 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                >
                  {CPU_PERCENT_OPTIONS.map((pct) => (
                    <button
                      key={pct}
                      onClick={() => onPercentChange(pct)}
                      className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${pct === config.cpu_limit_percent ? 'bg-ide-accent/10' : ''}`}
                    >
                      <span className="text-sm text-ide-text">{pct}%</span>
                      {pct === config.cpu_limit_percent && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {cpuChanged && (
          <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
            <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
            <p className="text-xs text-ide-warning-muted flex-1">{t('settings.advanced.cpu.changed_notice')}</p>
            {monitorStatus === 'running' && onRestartMonitor && (
              <button
                onClick={() => { onRestartMonitor(); onClearChanged(); }}
                className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
              >
                {t('settings.advanced.quick_restart')}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
