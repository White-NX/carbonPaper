import React from 'react';
import { useTranslation } from 'react-i18next';
import { SettingsSegmentedControl, SettingsSwitch } from '../SettingsControls';

export default function ResourcePolicyCard({
  resourcePolicy,
  resourcePolicyOptions,
  selectedResourcePolicy,
  resourcePolicyLoading,
  gameModeLoading,
  powerSavingMode,
  gameModeEnabled,
  gameModeActive,
  gameModePermanent,
  fullscreenPaused,
  useDml,
  onResourcePolicyChange,
  onSetPowerSaving,
  onSetGameMode,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <div>
        <label className="block font-semibold text-ide-text mb-1">{t('settings.general.resourcePolicy.label')}</label>
        <p className="text-xs text-ide-muted">{t('settings.general.resourcePolicy.description')}</p>
      </div>

      <SettingsSegmentedControl
        value={resourcePolicy}
        options={resourcePolicyOptions}
        onChange={onResourcePolicyChange}
        columns={4}
        disabled={resourcePolicyLoading || gameModeLoading}
      />

      <p className="text-xs text-ide-muted">{selectedResourcePolicy.description}</p>

      {resourcePolicy === 'custom' && (
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
              onChange={(next) => onSetPowerSaving(next)}
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
              onChange={(next) => onSetGameMode(next)}
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
  );
}
