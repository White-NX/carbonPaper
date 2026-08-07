import React from 'react';
import { useTranslation } from 'react-i18next';
import AdvancedWarning from './advanced/AdvancedWarning';
import ClusteringTechnicalCard from './advanced/ClusteringTechnicalCard';
import CpuLimitCard from './advanced/CpuLimitCard';
import DatabaseMaintenanceCard from './advanced/DatabaseMaintenanceCard';
import { ClipBackendCard, DmlAccelerationCard, OcrEngineCard, OnnxRuntimeCard, SemanticBackendCard } from './advanced/InferenceCards';
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
    onnxChanged,
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
    clearOnnxChanged,
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
    semanticIndexRun,
    clipIndexRunning,
    clipIndexRun,
    clipIndexStopping,
    clipIndexProgress,
    clipBackfill,
    clipBackfillBusy,
    handleClipBackfillDecision,
    handleToggleRustClipIndex,
    handleRunClipIndexNow,
    handleStopClipIndex,
    semanticIndexProgress,
    semanticIndexStopping,
    handleToggleRustSemanticIndex,
    handleRunSemanticIndexNow,
    handleStopSemanticIndex,
    refreshSemanticStatus,
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

      <OnnxRuntimeCard
        config={config}
        monitorStatus={monitorStatus}
        onnxChanged={onnxChanged}
        onToggle={handleToggle}
        onRestartMonitor={onRestartMonitor}
        onClearChanged={clearOnnxChanged}
      />

      <SemanticBackendCard
        config={config}
        status={semanticStatus}
        statusLoading={semanticStatusLoading}
        onToggleRustIndex={handleToggleRustSemanticIndex}
        onRefresh={refreshSemanticStatus}
        onRunIndexNow={handleRunSemanticIndexNow}
        onStopIndexNow={handleStopSemanticIndex}
        indexRunning={semanticIndexRunning}
        indexStopping={semanticIndexStopping}
        indexProgress={semanticIndexProgress}
        indexRun={semanticIndexRun}
      />

      <ClipBackendCard
        config={config}
        status={semanticStatus}
        statusLoading={semanticStatusLoading}
        onToggleRustIndex={handleToggleRustClipIndex}
        onRefresh={refreshSemanticStatus}
        onRunIndexNow={handleRunClipIndexNow}
        onStopIndexNow={handleStopClipIndex}
        indexRunning={clipIndexRunning}
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
