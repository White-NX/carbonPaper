import React from 'react';
import { useTranslation } from 'react-i18next';
import AdvancedWarning from './advanced/AdvancedWarning';
import ClusteringTechnicalCard from './advanced/ClusteringTechnicalCard';
import CpuLimitCard from './advanced/CpuLimitCard';
import DatabaseMaintenanceCard from './advanced/DatabaseMaintenanceCard';
import { DmlAccelerationCard, OcrEngineCard, OnnxRuntimeCard } from './advanced/InferenceCards';
import NetworkAccessCard from './advanced/NetworkAccessCard';
import OcrQueueCard from './advanced/OcrQueueCard';
import { useAdvancedSectionController } from './useAdvancedSectionController';

export default function AdvancedSection({ monitorStatus, onRestartMonitor }) {
  const { t } = useTranslation();
  const {
    config,
    loading,
    cpuDropdownOpen,
    queueDropdownOpen,
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
    setQueueDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged,
    clearDmlChanged,
    clearOnnxChanged,
    handleToggle,
    handleCpuPercentChange,
    handleQueueSizeChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
    handleRestartMlOcr,
    handleDownloadRustOcrModel,
    handleRetryFailedOcr,
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
        queueDropdownOpen={queueDropdownOpen}
        onToggle={handleToggle}
        onToggleQueueDropdown={() => setQueueDropdownOpen(!queueDropdownOpen)}
        onQueueSizeChange={handleQueueSizeChange}
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
        onRetryFailed={handleRetryFailedOcr}
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
