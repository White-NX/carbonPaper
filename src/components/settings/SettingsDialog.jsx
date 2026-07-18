import React, { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Settings as SettingsIcon,
  Shield,
  Activity,
  Image as ImageIcon,
  Database,
  HardDrive,
  Wrench,
  Search,
  Sparkles,
  SlidersHorizontal,
  Info,
} from 'lucide-react';
import { Dialog } from '../Dialog';
import MonitorServiceSection from './MonitorServiceSection';
import GeneralOptionsSection from './GeneralOptionsSection';
import CaptureFiltersSection from './CaptureFiltersSection';
import SecuritySection from './SecuritySection';
import StorageManagementSection from './StorageManagementSection';
import AboutSection from './AboutSection';
import AdvancedSection from './AdvancedSection';
import FeaturesSection from './FeaturesSection';
import LanguageSection from './LanguageSection';
import BrowserExtensionSection from './BrowserExtensionSection';
import AiEmbeddingSection from './AiEmbeddingSection';
import { useSettingsDialogController } from './useSettingsDialogController';

function SettingsDialog({
  isOpen,
  onClose,
  autoStartMonitor,
  onAutoStartMonitorChange,
  onManualStartMonitor,
  onManualStopMonitor,
  onRecordsDeleted,
  sessionTimeout,
  onSessionTimeoutChange,
  isSessionValid,
  onLockSession,
  powerSavingSuppressed,
  powerSavingMode,
  onPowerSavingModeChange,
}) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState('general');
  const {
    lowResolutionAnalysis,
    toggleLowResolutionAnalysis,
    sendTelemetryDiagnostics,
    toggleTelemetryDiagnostics,
    monitorStatus,
    filterSettings,
    processInput,
    setProcessInput,
    titleInput,
    setTitleInput,
    filtersDirty,
    savingFilters,
    saveFiltersMessage,
    autoLaunchEnabled,
    autoLaunchLoading,
    autoLaunchMessage,
    storage,
    analysisLoading,
    analysisRefreshing,
    analysisError,
    checkingUpdate,
    upToDate,
    updateInfo,
    updateError,
    downloading,
    downloadProgress,
    isDeleting,
    deleteMessage,
    addProcessTags,
    addTitleTags,
    removeProcessTag,
    removeTitleTag,
    handleToggleProtected,
    handleQuickDelete,
    handleSaveFilters,
    handleStartMonitor,
    handleStopMonitor,
    handleRestartMonitor,
    handlePauseMonitor,
    handleResumeMonitor,
    handleToggleAutoLaunch,
    handleRefreshAnalysis,
    handleCheckUpdate,
    handleDownloadUpdate,
  } = useSettingsDialogController({
    isOpen,
    activeTab,
    onManualStartMonitor,
    onManualStopMonitor,
    onRecordsDeleted,
    t,
  });

  const storageSegments = useMemo(() => {
    if (!storage) return [];
    return [
      { key: 'models', label: t('settings.storage.models'), bytes: storage.models_bytes, icon: Activity, color: 'bg-indigo-500/70' },
      { key: 'images', label: t('settings.storage.images'), bytes: storage.images_bytes, icon: ImageIcon, color: 'bg-sky-500/70' },
      { key: 'database', label: t('settings.storage.database'), bytes: storage.database_bytes, icon: Database, color: 'bg-emerald-500/70' },
      { key: 'other', label: t('settings.storage.other'), bytes: storage.other_bytes, icon: HardDrive, color: 'bg-amber-500/70' },
    ];
  }, [storage, t]);

  const totalStorage = storage?.total_bytes || 0;

  const tabs = [
    { id: 'general', label: t('settings.tabs.general'), icon: SettingsIcon },
    { id: 'capture', label: t('settings.tabs.capture'), icon: Search },
    { id: 'organize', label: t('settings.tabs.organize'), icon: Sparkles },
    { id: 'privacy', label: t('settings.tabs.privacy'), icon: Shield },
    { id: 'maintenance', label: t('settings.tabs.maintenance'), icon: Wrench },
    { id: 'advanced', label: t('settings.tabs.advanced'), icon: SlidersHorizontal },
    { id: 'about', label: t('settings.tabs.about'), icon: Info },
  ];

  return (
    <Dialog
      isOpen={isOpen}
      onClose={onClose}
      title={t('settings.title')}
      maxWidth="max-w-3xl"
      className="h-[550px]"
      contentClassName="flex flex-col"
    >
      <div className="flex bg-ide-bg flex-1 overflow-hidden">
        <div className="w-40 border-r border-ide-border bg-ide-panel p-2 flex flex-col gap-1 shrink-0 overflow-y-auto">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-3 px-3 py-2 rounded text-sm transition-colors text-left ${
                activeTab === tab.id ? 'bg-ide-accent text-white font-medium' : 'text-ide-text hover:bg-ide-hover'
              }`}
            >
              <tab.icon className="w-4 h-4 shrink-0" />
              <span className="min-w-0 leading-tight">{tab.label}</span>
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto p-6 scrollbar-thin">
          {activeTab === 'general' && (
            <div className="space-y-8">
              <LanguageSection />

              <MonitorServiceSection
                monitorStatus={monitorStatus}
                onStart={handleStartMonitor}
                onStop={handleStopMonitor}
                onPause={handlePauseMonitor}
                onResume={handleResumeMonitor}
                onRestart={handleRestartMonitor}
                autoStartMonitor={autoStartMonitor}
                onAutoStartMonitorChange={onAutoStartMonitorChange}
                autoLaunchEnabled={autoLaunchEnabled}
                autoLaunchLoading={autoLaunchLoading}
                autoLaunchMessage={autoLaunchMessage}
                onToggleAutoLaunch={handleToggleAutoLaunch}
                powerSavingSuppressed={powerSavingSuppressed}
              />

              <GeneralOptionsSection
                lowResolutionAnalysis={lowResolutionAnalysis}
                onToggleLowRes={toggleLowResolutionAnalysis}
                sendTelemetryDiagnostics={sendTelemetryDiagnostics}
                onToggleTelemetry={toggleTelemetryDiagnostics}
                powerSavingMode={powerSavingMode}
                onTogglePowerSaving={(next) => onPowerSavingModeChange?.(next)}
              />
            </div>
          )}

          {activeTab === 'capture' && (
            <div className="space-y-8">
              <CaptureFiltersSection
                filterSettings={filterSettings}
                processInput={processInput}
                titleInput={titleInput}
                onProcessInputChange={setProcessInput}
                onTitleInputChange={setTitleInput}
                onAddProcess={addProcessTags}
                onAddTitle={addTitleTags}
                onRemoveProcess={removeProcessTag}
                onRemoveTitle={removeTitleTag}
                onToggleProtected={handleToggleProtected}
                onSave={handleSaveFilters}
                filtersDirty={filtersDirty}
                savingFilters={savingFilters}
                saveFiltersMessage={saveFiltersMessage}
                onQuickDelete={handleQuickDelete}
                isDeleting={isDeleting}
                deleteMessage={deleteMessage}
              />
            </div>
          )}

          {activeTab === 'organize' && (
            <FeaturesSection monitorStatus={monitorStatus} />
          )}

          {activeTab === 'privacy' && (
            <div className="space-y-8">
              <SecuritySection
                sessionTimeout={sessionTimeout}
                onSessionTimeoutChange={onSessionTimeoutChange}
                isSessionValid={isSessionValid}
                onLockSession={onLockSession}
              />

              <BrowserExtensionSection />
            </div>
          )}

          <div className={activeTab === 'privacy' ? 'mt-8' : 'hidden'}>
            <AiEmbeddingSection />
          </div>

          {activeTab === 'maintenance' && (
            <div className="space-y-8">
              <StorageManagementSection
                storageSegments={storageSegments}
                totalStorage={totalStorage}
                storage={storage}
                loading={analysisLoading}
                refreshing={analysisRefreshing}
                error={analysisError}
                onRefresh={handleRefreshAnalysis}
                monitorStatus={monitorStatus}
              />
            </div>
          )}

          {activeTab === 'advanced' && (
            <div className="space-y-8">
              <AdvancedSection monitorStatus={monitorStatus} onRestartMonitor={handleRestartMonitor} />
            </div>
          )}

          {activeTab === 'about' && (
            <div className="space-y-8">
              <AboutSection
                checking={checkingUpdate}
                upToDate={upToDate}
                onCheckUpdate={handleCheckUpdate}
                updateInfo={updateInfo}
                updateError={updateError}
                downloading={downloading}
                downloadProgress={downloadProgress}
                onDownloadUpdate={handleDownloadUpdate}
              />
            </div>
          )}
        </div>
      </div>
    </Dialog>
  );
}

export default SettingsDialog;
