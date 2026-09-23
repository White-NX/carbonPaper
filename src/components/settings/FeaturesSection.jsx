import React from 'react';
import { useTranslation } from 'react-i18next';
import { Layers } from 'lucide-react';
import FeatureModeCard from './organize/FeatureModeCard';
import { FEATURE_MODE_OPTIONS, getFeatureMode } from './organize/featureModes';
import { SettingsErrorBanner } from './SettingsPrimitives';
import SmartClusterCard from './organize/SmartClusterCard';
import BackgroundSchedulingCard from './organize/BackgroundSchedulingCard';
import { useFeaturesController } from './useFeaturesController';

export default function FeaturesSection() {
  const { t } = useTranslation();
  const {
    config,
    loading,
    featureSaving,
    featureError,
    customControlsOpen,
    setCustomControlsOpen,
    scModelAvailable,
    scStatus,
    scDownloading,
    scDownloadLog,
    scDownloadError,
    handleFeatureModeChange,
    handleCustomFeatureToggle,
    handleBackgroundTimingChange,
    backgroundTimingSaving,
    backgroundTimingError,
    handleDownloadReranker,
    handleDrainNow,
    handleRescanAll,
    featureMode,
    featureModeOptions,
    selectedFeatureMode,
  } = useFeaturesController({
    t,
    featureModeDefinitions: FEATURE_MODE_OPTIONS,
    getFeatureMode,
  });

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        {t('settings.features.loading', '加载中...')}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {featureError && <SettingsErrorBanner>{featureError}</SettingsErrorBanner>}
      <section className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Layers className="w-4 h-4" />
          {t('settings.features.management.title', '功能管理')}
        </label>

        <div className="space-y-3">
          <FeatureModeCard
            disabled={featureSaving}
            config={config}
            featureMode={featureMode}
            featureModeOptions={featureModeOptions}
            selectedFeatureMode={selectedFeatureMode}
            customControlsOpen={customControlsOpen}
            scModelAvailable={scModelAvailable}
            onFeatureModeChange={handleFeatureModeChange}
            onCustomFeatureToggle={handleCustomFeatureToggle}
            onToggleCustomControls={() => setCustomControlsOpen((open) => !open)}
          />

          <BackgroundSchedulingCard
            value={config.background_scheduling_mode || 'auto'}
            saving={backgroundTimingSaving}
            error={backgroundTimingError}
            onChange={handleBackgroundTimingChange}
          />

          <SmartClusterCard
            config={config}
            scModelAvailable={scModelAvailable}
            scStatus={scStatus}
            scDownloading={scDownloading}
            scDownloadLog={scDownloadLog}
            scDownloadError={scDownloadError}
            onDownloadReranker={handleDownloadReranker}
            onDrainNow={handleDrainNow}
            onRescanAll={handleRescanAll}
          />
        </div>
      </section>
    </div>
  );
}
