import React from 'react';
import { useTranslation } from 'react-i18next';
import { SettingsSwitch } from './SettingsControls';
import CardClickBehaviorCard from './general/CardClickBehaviorCard';
import ResourcePolicyCard from './general/ResourcePolicyCard';
import WindowBehaviorCard from './general/WindowBehaviorCard';
import { useGeneralOptionsController } from './useGeneralOptionsController';

export default function GeneralOptionsSection({
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

        <div className="w-full h-px bg-ide-border/50" />

        <ResourcePolicyCard
          resourcePolicy={resourcePolicy}
          resourcePolicyOptions={resourcePolicyOptions}
          selectedResourcePolicy={selectedResourcePolicy}
          resourcePolicyLoading={resourcePolicyLoading}
          gameModeLoading={gameModeLoading}
          powerSavingMode={powerSavingMode}
          gameModeEnabled={gameModeEnabled}
          gameModeActive={gameModeActive}
          gameModePermanent={gameModePermanent}
          fullscreenPaused={fullscreenPaused}
          useDml={useDml}
          onResourcePolicyChange={handleResourcePolicyChange}
          onSetPowerSaving={handleSetPowerSaving}
          onSetGameMode={handleSetGameMode}
        />

        <div className="w-full h-px bg-ide-border/50" />

        <WindowBehaviorCard
          lightweightConfig={lightweightConfig}
          onLightweightConfigChange={handleLightweightConfigChange}
          onSwitchToLightweight={handleSwitchToLightweight}
        />

        <div className="w-full h-px bg-ide-border/50" />

        <CardClickBehaviorCard
          cardClickBehaviorSearch={cardClickBehaviorSearch}
          cardClickBehaviorClusters={cardClickBehaviorClusters}
          cardClickBehaviorActivityContext={cardClickBehaviorActivityContext}
          onSetCardClickBehavior={setCardClickBehavior}
        />
      </div>
    </div>
  );
}
