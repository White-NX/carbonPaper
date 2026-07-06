import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, Minimize2 } from 'lucide-react';
import { SettingsButton, SettingsSegmentedControl, SettingsSwitch } from './SettingsControls';
import { useGeneralOptionsController } from './useGeneralOptionsController';

function DropdownSelect({ value, onChange, options }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);

  useEffect(() => {
    const handleClickOutside = (event) => {
      if (ref.current && !ref.current.contains(event.target)) {
        setOpen(false);
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [open]);

  const selectedOption = options.find((opt) => opt.value === value) || options[0];

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="flex items-center gap-2 px-3 py-1.5 bg-ide-panel border border-ide-border rounded-lg text-xs text-ide-text hover:bg-ide-hover transition-colors min-w-[160px]"
      >
        <span className="flex-1 text-left">{selectedOption.label}</span>
        <ChevronDown className={`w-3.5 h-3.5 text-ide-muted transition-transform ${open ? 'rotate-180' : ''}`} />
      </button>
      {open && (
        <div className="absolute right-0 top-full mt-1.5 w-44 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden">
          {options.map((opt) => (
            <button
              type="button"
              key={opt.value}
              onClick={() => {
                setOpen(false);
                onChange(opt.value);
              }}
              className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${opt.value === value ? 'bg-ide-accent/10' : ''}`}
            >
              <span className="text-xs text-ide-text">{opt.label}</span>
              {opt.value === value && (
                <div className="w-1.5 h-1.5 rounded-full bg-ide-accent shrink-0" />
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function GeneralOptionsSection({
  lowResolutionAnalysis,
  onToggleLowRes,
  sendTelemetryDiagnostics,
  onToggleTelemetry,
  powerSavingMode: externalPowerSavingMode,
  onTogglePowerSaving,
}) {
  const { t } = useTranslation();
  const {
    powerSavingMode,
    gameModeEnabled,
    gameModeActive,
    gameModePermanent,
    fullscreenPaused,
    useDml,
    gameModeLoading,
    resourcePolicyLoading,
    lightweightConfig,
    cardClickBehaviorSearch,
    cardClickBehaviorClusters,
    cardClickBehaviorActivityContext,
    resourcePolicy,
    resourcePolicyOptions,
    selectedResourcePolicy,
    handleSetPowerSaving,
    handleSetGameMode,
    handleResourcePolicyChange,
    handleLightweightConfigChange,
    handleSwitchToLightweight,
    setCardClickBehavior,
  } = useGeneralOptionsController({ externalPowerSavingMode, onTogglePowerSaving, t });

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 block">{t('settings.general.title')}</label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.telemetry.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.telemetry.description')}
            </p>
          </div>
          <SettingsSwitch
            checked={sendTelemetryDiagnostics}
            onChange={onToggleTelemetry}
            title={t('settings.general.telemetry.label')}
          />
        </div>

        {/* Resource policy */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="space-y-3">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.resourcePolicy.label')}</label>
            <p className="text-xs text-ide-muted">{t('settings.general.resourcePolicy.description')}</p>
          </div>

          <SettingsSegmentedControl
            value={resourcePolicy}
            options={resourcePolicyOptions}
            onChange={handleResourcePolicyChange}
            columns={4}
            disabled={resourcePolicyLoading || gameModeLoading}
          />

          <p className="text-xs text-ide-muted">{selectedResourcePolicy.description}</p>

          {(resourcePolicy === 'custom') && (
            <div className="space-y-3 rounded-lg border border-ide-border/70 bg-ide-panel/40 p-3">
              <div className="flex items-center justify-between gap-4">
                <div>
                  <label className="block font-semibold text-ide-text mb-1">{t('settings.general.powerSaving.label')}</label>
                  <p className="text-xs text-ide-muted">
                    {t('settings.general.powerSaving.description')}
                  </p>
                </div>
                <SettingsSwitch
                  checked={powerSavingMode}
                  onChange={(next) => handleSetPowerSaving(next)}
                  disabled={resourcePolicyLoading}
                  title={t('settings.general.powerSaving.label')}
                />
              </div>

              <div className="w-full h-px bg-ide-border/50" />

              <div className="flex items-center justify-between gap-4">
                <div className="flex-1 min-w-0">
                  <label className="block font-semibold text-ide-text mb-1">{t('settings.general.gameMode.label')}</label>
                  <p className="text-xs text-ide-muted">
                    {t('settings.general.gameMode.description')}
                  </p>
                </div>
                <SettingsSwitch
                  checked={gameModeEnabled}
                  onChange={(next) => handleSetGameMode(next)}
                  disabled={gameModeLoading || resourcePolicyLoading}
                  title={t('settings.general.gameMode.label')}
                />
              </div>
            </div>
          )}

          <div>
            {gameModeEnabled && useDml && (
              <p className={`text-xs mt-1 ${gameModeActive ? 'text-ide-warning' : 'text-ide-info-success'}`}>
                {gameModePermanent
                  ? t('settings.general.gameMode.permanent')
                  : gameModeActive
                    ? t('settings.general.gameMode.active')
                    : t('settings.general.gameMode.inactive')
                }
              </p>
            )}
            {gameModeEnabled && fullscreenPaused && (
              <p className="text-xs mt-1 text-ide-warning">
                {t('settings.general.gameMode.fullscreen_paused')}
              </p>
            )}
          </div>
        </div>

        {/* Window and background behavior */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="space-y-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.windowBehavior.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.windowBehavior.description')}
            </p>
          </div>

          <div className="grid gap-4 md:grid-cols-2">
            <div className="space-y-2">
              <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.startup.label')}</label>
              <SettingsSegmentedControl
                value={lightweightConfig.start_with_window_hidden}
                options={[
                  {
                    value: false,
                    label: t('settings.general.windowBehavior.startup.showWindow.label'),
                    description: t('settings.general.windowBehavior.startup.showWindow.description'),
                  },
                  {
                    value: true,
                    label: t('settings.general.windowBehavior.startup.background.label'),
                    description: t('settings.general.windowBehavior.startup.background.description'),
                  },
                ]}
                onChange={(next) => handleLightweightConfigChange('start_with_window_hidden', next)}
                columns={2}
              />
              <p className="text-xs text-ide-muted">
                {lightweightConfig.start_with_window_hidden
                  ? t('settings.general.windowBehavior.startup.background.description')
                  : t('settings.general.windowBehavior.startup.showWindow.description')}
              </p>
            </div>

            <div className="space-y-2">
              <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.closeWindow.label')}</label>
              <SettingsSegmentedControl
                value={lightweightConfig.auto_lightweight_enabled}
                options={[
                  {
                    value: false,
                    label: t('settings.general.windowBehavior.closeWindow.current.label'),
                    description: t('settings.general.windowBehavior.closeWindow.current.description'),
                  },
                  {
                    value: true,
                    label: t('settings.general.windowBehavior.closeWindow.background.label'),
                    description: t('settings.general.windowBehavior.closeWindow.background.description'),
                  },
                ]}
                onChange={(next) => handleLightweightConfigChange('auto_lightweight_enabled', next)}
                columns={2}
              />
              <p className="text-xs text-ide-muted">
                {lightweightConfig.auto_lightweight_enabled
                  ? t('settings.general.windowBehavior.closeWindow.background.description')
                  : t('settings.general.windowBehavior.closeWindow.current.description')}
              </p>
              {lightweightConfig.auto_lightweight_enabled && (
                <div className="flex items-center gap-2">
                  <span className="text-xs text-ide-muted">{t('settings.general.windowBehavior.delay.label')}</span>
                  <input
                    type="number"
                    min="1"
                    max="60"
                    value={lightweightConfig.auto_lightweight_delay_minutes}
                    onChange={(e) => {
                      const val = parseInt(e.target.value, 10);
                      if (!isNaN(val) && val >= 1 && val <= 60) {
                        handleLightweightConfigChange('auto_lightweight_delay_minutes', val);
                      }
                    }}
                    onBlur={(e) => {
                      const val = parseInt(e.target.value, 10);
                      if (isNaN(val) || val < 1 || val > 60) {
                        handleLightweightConfigChange('auto_lightweight_delay_minutes', 5);
                      }
                    }}
                    className="w-16 px-2 py-1 bg-ide-panel border border-ide-border rounded text-ide-text text-xs"
                  />
                  <span className="text-xs text-ide-muted">{t('settings.general.windowBehavior.delay.unit')}</span>
                </div>
              )}
            </div>
          </div>

          <div className="flex items-center justify-between gap-4 rounded-lg border border-ide-border/70 bg-ide-panel/40 px-3 py-2.5">
            <div className="min-w-0">
              <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.switchNow.label')}</label>
              <p className="text-xs text-ide-muted">{t('settings.general.windowBehavior.switchNow.description')}</p>
            </div>
            <SettingsButton
              onClick={handleSwitchToLightweight}
              icon={Minimize2}
              className="shrink-0"
            >
              {t('settings.general.windowBehavior.switchNow.button')}
            </SettingsButton>
          </div>
        </div>

        {/* Card Click Behavior */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="space-y-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.cardClickBehavior.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.cardClickBehavior.description')}
            </p>
          </div>

          <div className="space-y-3 pl-4 border-l-2 border-ide-border">
            {/* Advanced Search */}
            <div className="flex items-center justify-between gap-4">
              <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.searchLabel')}</label>
              <DropdownSelect
                value={cardClickBehaviorSearch}
                onChange={(val) => setCardClickBehavior('search', val)}
                options={[
                  { value: 'preview', label: t('settings.general.cardClickBehavior.preview') },
                  { value: 'standalone', label: t('settings.general.cardClickBehavior.standalone') },
                ]}
              />
            </div>

            {/* Smart Clusters */}
            <div className="flex items-center justify-between gap-4">
              <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.clustersLabel')}</label>
              <DropdownSelect
                value={cardClickBehaviorClusters}
                onChange={(val) => setCardClickBehavior('clusters', val)}
                options={[
                  { value: 'preview', label: t('settings.general.cardClickBehavior.preview') },
                  { value: 'standalone', label: t('settings.general.cardClickBehavior.standalone') },
                ]}
              />
            </div>

            {/* Activity Context */}
            <div className="flex items-center justify-between gap-4">
              <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.activityContextLabel')}</label>
              <DropdownSelect
                value={cardClickBehaviorActivityContext}
                onChange={(val) => setCardClickBehavior('activityContext', val)}
                options={[
                  { value: 'preview', label: t('settings.general.cardClickBehavior.preview') },
                  { value: 'standalone', label: t('settings.general.cardClickBehavior.standalone') },
                ]}
              />
            </div>
          </div>
        </div>

      </div>
    </div>
  );
}
