import React from 'react';
import { useTranslation } from 'react-i18next';
import { SettingsGroup, SettingsSection, SettingsErrorBanner } from './SettingsPrimitives';
import CardClickBehaviorCard from './general/CardClickBehaviorCard';
import ResourcePolicyCard from './general/ResourcePolicyCard';
import WindowBehaviorCard from './general/WindowBehaviorCard';
import { useGeneralOptionsController } from './useGeneralOptionsController';

export default function GeneralOptionsSection({
  powerSavingMode: externalPowerSavingMode,
  onTogglePowerSaving,
}) {
  const { t } = useTranslation();
  const {
    optionError,
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
    <SettingsSection title={t('settings.general.title')}>
      {optionError && <SettingsErrorBanner>{optionError}</SettingsErrorBanner>}
      <SettingsGroup className="space-y-4">
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
      </SettingsGroup>
    </SettingsSection>
  );
}
