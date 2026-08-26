import React from 'react';
import { useTranslation } from 'react-i18next';
import AdvancedWarning from './advanced/AdvancedWarning';
import ClusteringTechnicalCard from './advanced/ClusteringTechnicalCard';
import CpuLimitCard from './advanced/CpuLimitCard';
import DatabaseMaintenanceCard from './advanced/DatabaseMaintenanceCard';
import { BackgroundSchedulerCard, ClassificationBackendCard, ClipBackendCard, DmlAccelerationCard, OcrEngineCard, SemanticBackendCard } from './advanced/InferenceCards';
import NetworkAccessCard from './advanced/NetworkAccessCard';
import OcrQueueCard from './advanced/OcrQueueCard';
import { useAdvancedSectionController } from './useAdvancedSectionController';

export default function AdvancedSection({ monitorStatus, onRestartMonitor }) {
  const { t } = useTranslation();
  const {
    config,
    loading,
    cpuDropdownOpen,
    gpuDropdownOpen,
    clusteringDropdownOpen,
    cpuChanged,
    dmlChanged,
    gpus,
    gpuLoading,
    vacuumRunning,
    vacuumMessage,
    selectedGpu,
    mlOcrStatus,
    mlOcrStatusLoading,
    rustOcrModelStatus,
    rustOcrModelDownloading,
    setCpuDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged,
    clearDmlChanged,
    handleToggle,
    handleCpuPercentChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
    handleRestartMlOcr,
    handleDownloadRustOcrModel,
    semanticStatus,
    semanticStatusLoading,
    semanticIndexRunning,
    semanticIndexPhase,
    semanticIndexRetryAt,
    semanticIndexRun,
    clipIndexRunning,
    clipIndexPhase,
    clipIndexRetryAt,
    clipIndexRun,
    clipIndexStopping,
    clipIndexProgress,
    clipAnnRetrying,
    clipBackfill,
    clipBackfillBusy,
    handleClipBackfillDecision,
    handleRunClipIndexNow,
    handleStopClipIndex,
    handleRetryClipAnn,
    semanticIndexProgress,
    semanticIndexStopping,
    handleRunSemanticIndexNow,
    handleStopSemanticIndex,
    refreshSemanticStatus,
    backgroundProcessingEnabled,
    backgroundSchedulerStatus,
    backgroundProcessingSaving,
    refreshBackgroundSchedulerStatus,
    handleBackgroundProcessingChange,
  } = useAdvancedSectionController({ monitorStatus, t });

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        {t('settings.advanced.loading')}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <AdvancedWarning />

      <CpuLimitCard
        config={config}
        monitorStatus={monitorStatus}
        cpuDropdownOpen={cpuDropdownOpen}
        cpuChanged={cpuChanged}
        onToggle={handleToggle}
        onToggleDropdown={() => setCpuDropdownOpen(!cpuDropdownOpen)}
        onPercentChange={handleCpuPercentChange}
        onRestartMonitor={onRestartMonitor}
        onClearChanged={clearCpuChanged}
      />

      <OcrQueueCard
        config={config}
        onOcrTimeoutDraftChange={handleOcrTimeoutDraftChange}
        onOcrTimeoutChange={handleOcrTimeoutChange}
      />

      <OcrEngineCard
        config={config}
        status={mlOcrStatus}
        statusLoading={mlOcrStatusLoading}
        modelStatus={rustOcrModelStatus}
        modelDownloading={rustOcrModelDownloading}
        onToggle={handleToggle}
        onRestart={handleRestartMlOcr}
        onDownloadModel={handleDownloadRustOcrModel}
      />

      <DmlAccelerationCard
        config={config}
        monitorStatus={monitorStatus}
        dmlChanged={dmlChanged}
        gpus={gpus}
        gpuLoading={gpuLoading}
        selectedGpu={selectedGpu}
        gpuDropdownOpen={gpuDropdownOpen}
        onToggle={handleToggle}
        onToggleGpuDropdown={() => setGpuDropdownOpen(!gpuDropdownOpen)}
        onGpuChange={handleGpuChange}
        onRestartMonitor={onRestartMonitor}
        onClearChanged={clearDmlChanged}
      />

      <ClassificationBackendCard
        status={semanticStatus}
      />

      <BackgroundSchedulerCard
        enabled={backgroundProcessingEnabled}
        saving={backgroundProcessingSaving}
        status={backgroundSchedulerStatus}
        onChange={handleBackgroundProcessingChange}
        onRefresh={refreshBackgroundSchedulerStatus}
      />

      <SemanticBackendCard
        status={semanticStatus}
        statusLoading={semanticStatusLoading}
        onRefresh={refreshSemanticStatus}
        onRunIndexNow={handleRunSemanticIndexNow}
        onStopIndexNow={handleStopSemanticIndex}
        indexRunning={semanticIndexRunning}
        indexPhase={semanticIndexPhase}
        indexRetryAt={semanticIndexRetryAt}
        indexStopping={semanticIndexStopping}
        indexProgress={semanticIndexProgress}
        indexRun={semanticIndexRun}
      />

      <ClipBackendCard
        status={semanticStatus}
        statusLoading={semanticStatusLoading}
        onRefresh={refreshSemanticStatus}
        onRunIndexNow={handleRunClipIndexNow}
        onStopIndexNow={handleStopClipIndex}
        onRetryAnn={handleRetryClipAnn}
        annRetrying={clipAnnRetrying}
        indexRunning={clipIndexRunning}
        indexPhase={clipIndexPhase}
        indexRetryAt={clipIndexRetryAt}
        indexStopping={clipIndexStopping}
        indexProgress={clipIndexProgress}
        indexRun={clipIndexRun}
        backfill={clipBackfill}
        backfillBusy={clipBackfillBusy}
        onBackfillDecision={handleClipBackfillDecision}
      />

      <ClusteringTechnicalCard
        config={config}
        clusteringDropdownOpen={clusteringDropdownOpen}
        onToggle={handleToggle}
        onToggleDropdown={() => setClusteringDropdownOpen(!clusteringDropdownOpen)}
        onIntervalChange={handleClusteringIntervalChange}
      />

      <NetworkAccessCard
        config={config}
        onToggle={handleToggle}
      />

      <DatabaseMaintenanceCard
        vacuumRunning={vacuumRunning}
        vacuumMessage={vacuumMessage}
        onManualVacuum={handleManualVacuum}
      />
    </div>
  );
}
