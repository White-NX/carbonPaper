import React from 'react';
import { useTranslation } from 'react-i18next';
import { Layers } from 'lucide-react';
import ClusteringScheduleCard from './organize/ClusteringScheduleCard';
import FeatureModeCard from './organize/FeatureModeCard';
import { FEATURE_MODE_OPTIONS, getFeatureMode } from './organize/featureModes';
import ModelInventoryTable from './organize/ModelInventoryTable';
import SmartClusterCard from './organize/SmartClusterCard';
import { useFeaturesController } from './useFeaturesController';

export default function FeaturesSection({ monitorStatus }) {
  const { t } = useTranslation();
  const {
    config,
    loading,
    models,
    modelsLoading,
    clusteringDropdownOpen,
    setClusteringDropdownOpen,
    clusteringAdvancedOpen,
    setClusteringAdvancedOpen,
    clusteringRunning,
    clusteringError,
    clusteringNotice,
    rangeStart,
    setRangeStart,
    rangeEnd,
    setRangeEnd,
    customControlsOpen,
    setCustomControlsOpen,
    scModelAvailable,
    scStatus,
    scDownloading,
    scDownloadLog,
    scDownloadError,
    handleOpenLocation,
    handleFeatureModeChange,
    handleCustomFeatureToggle,
    handleClusteringIntervalChange,
    handleRunClustering,
    handleDownloadReranker,
    handleDrainNow,
    handleRescanAll,
    clearClusteringError,
    clearClusteringNotice,
    formatSize,
    lastClusteringRunLabel,
    featureMode,
    featureModeOptions,
    selectedFeatureMode,
    loadModels,
  } = useFeaturesController({
    monitorStatus,
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
      <section className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Layers className="w-4 h-4" />
          {t('settings.features.management.title', '功能管理')}
        </label>

        <div className="space-y-3">
          <FeatureModeCard
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

          <ClusteringScheduleCard
            config={config}
            monitorStatus={monitorStatus}
            clusteringDropdownOpen={clusteringDropdownOpen}
            clusteringAdvancedOpen={clusteringAdvancedOpen}
            clusteringRunning={clusteringRunning}
            clusteringError={clusteringError}
            clusteringNotice={clusteringNotice}
            rangeStart={rangeStart}
            rangeEnd={rangeEnd}
            lastClusteringRunLabel={lastClusteringRunLabel}
            onToggleDropdown={() => setClusteringDropdownOpen(!clusteringDropdownOpen)}
            onToggleAdvanced={() => setClusteringAdvancedOpen((value) => !value)}
            onIntervalChange={handleClusteringIntervalChange}
            onRangeStartChange={setRangeStart}
            onRangeEndChange={setRangeEnd}
            onRunClustering={handleRunClustering}
            onClearClusteringError={clearClusteringError}
            onClearClusteringNotice={clearClusteringNotice}
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

      <ModelInventoryTable
        models={models}
        modelsLoading={modelsLoading}
        monitorStatus={monitorStatus}
        onRefresh={loadModels}
        onOpenLocation={handleOpenLocation}
        formatSize={formatSize}
      />
    </div>
  );
}
