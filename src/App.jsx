import React, { useCallback, useEffect, useState } from 'react';
import Timeline from './components/Timeline';
import SettingsDialog from './components/settings/SettingsDialog';
import Mask from './components/Mask';
import AuthMask from './components/AuthMask';
import SecurityAlertMask from './components/SecurityAlertMask';
import ExtensionSetupWizard from './components/ExtensionSetupWizard';
import ClusteringSetupWizard from './components/ClusteringSetupWizard';
import SmartClusterSetupWizard from './components/SmartClusterSetupWizard';
import ActivityBar from './components/ActivityBar';
import MainArea from './components/MainArea';
import TopBar from './components/TopBar';
import { NotificationToast, NotificationPanel } from './components/Notifications';
import ErrorWindow from './components/ErrorWindow';
import HmacMigrationDialog from './components/HmacMigrationDialog';
import StartupVacuumDialog from './components/StartupVacuumDialog';
import { deleteScreenshot, deleteRecordsByTimeRange } from './lib/monitor_api';
import { UpdateModal } from './components/UpdateModal';
import { useAppTheme } from './hooks/useAppTheme';
import { useAppWindowActions, usePowerSavingState, useWindowMaximizedState } from './hooks/useAppWindowState';
import { useAppNotifications } from './hooks/useAppNotifications';
import { useAuthSession } from './hooks/useAuthSession';
import { useCriticalErrors } from './hooks/useCriticalErrors';
import { useMonitorLifecycle } from './hooks/useMonitorLifecycle';
import { usePythonEnvironment } from './hooks/usePythonEnvironment';
import { useSelectedSnapshot, normalizeTimestampToMs } from './hooks/useSelectedSnapshot';
import { useStartupWizards } from './hooks/useStartupWizards';
import { useUpdateManager } from './hooks/useUpdateManager';

function App() {
  useEffect(() => {
    const handleContextMenu = (e) => {
      if (['INPUT', 'TEXTAREA'].includes(e.target.tagName)) return;
      e.preventDefault();
      return false;
    };
    document.addEventListener('contextmenu', handleContextMenu);
    return () => {
      document.removeEventListener('contextmenu', handleContextMenu);
    };
  }, []);

  const [showSettings, setShowSettings] = useState(false);
  const [activeTab, setActiveTab] = useState('preview');
  const [sidebarExpanded, setSidebarExpanded] = useState(false);
  const [searchMode, setSearchMode] = useState('ocr');
  const [advancedSearchParams, setAdvancedSearchParams] = useState({ query: '', mode: 'ocr', refreshKey: Date.now() });

  const { darkMode, setDarkMode } = useAppTheme();
  const { powerSavingMode, setPowerSavingMode, powerSavingSuppressed, windowFocused } = usePowerSavingState();
  const isMaximized = useWindowMaximizedState();
  const { minimize, toggleMaximize, hideToTray, restartApp, exitApp } = useAppWindowActions();
  const {
    showNotifications,
    setShowNotifications,
    notifications,
    toastNotifications,
    pushNotification,
    dismissNotification,
    handleToastClose,
    clearNotifications,
    securityAlert,
    setSecurityAlert,
    formatErrorDetails,
    reportBackendError,
    resetBackendErrorDedupe,
  } = useAppNotifications();
  const {
    isAuthenticated,
    authError,
    setAuthError,
    sessionTimeout,
    setSessionTimeout,
    handleAuthSuccess,
    handleLockSession,
  } = useAuthSession();
  const { criticalErrors, criticalErrorLogPath } = useCriticalErrors();
  const {
    pythonVersion,
    depsNeedUpdate,
    depsSyncing,
    depsCheckDone,
    modelsNeedDownload,
    missingModels,
    refreshPythonVersion,
    handleDepsSync,
    handleModelsDownloadComplete,
  } = usePythonEnvironment();
  const {
    autoStartMonitor,
    setAutoStartMonitor,
    handleManualStartMonitor,
    handleManualStopMonitor,
    backendStatus,
    monitorPaused,
    backendError,
    handleStartBackend,
    handlePauseMonitor,
    handleResumeMonitor,
  } = useMonitorLifecycle({
    pythonVersion,
    depsNeedUpdate,
    depsSyncing,
    depsCheckDone,
    modelsNeedDownload,
    powerSavingSuppressed,
    formatErrorDetails,
    reportBackendError,
    resetBackendErrorDedupe,
  });
  const {
    showExtensionSetup,
    showClusteringSetup,
    showSmartClusterSetup,
    handleExtensionSetupComplete,
    handleClusteringSetupComplete,
    handleSmartClusterSetupComplete,
  } = useStartupWizards({
    backendStatus,
    isAuthenticated,
    setActiveTab,
    pushNotification,
  });
  const {
    selectedEvent,
    setSelectedEvent,
    selectedDetails,
    selectedImageSrc,
    isLoadingDetails,
    lastError,
    highlightedEventId,
    setHighlightedEventId,
    timelineJump,
    setTimelineJump,
    timelineRefreshKey,
    ocrBoxes,
    clearSelection,
    bumpTimelineRefresh,
  } = useSelectedSnapshot();
  const {
    updateModalVisible,
    updateInfo,
    updateDownloading,
    updateDownloadProgress,
    updateDownloadError,
    setUpdateModalVisible,
    handleDownloadUpdate,
    handleLater,
  } = useUpdateManager();

  const handleCopyText = (text) => {
    navigator.clipboard.writeText(text);
  };

  const handleGlobalClick = useCallback((event) => {
    const target = event.target;
    if (target && target.closest && target.closest('[data-keep-selection]')) {
      return;
    }
    if (highlightedEventId !== null) {
      setHighlightedEventId(null);
    }
  }, [highlightedEventId, setHighlightedEventId]);

  const selectSearchResult = useCallback((res) => {
    const screenshotId = res.screenshot_id !== undefined ? res.screenshot_id : (res.metadata?.screenshot_id);
    const imagePath = res.image_path || res.metadata?.image_path;
    const timestamp = res.screenshot_created_at || res.metadata?.screenshot_created_at || res.metadata?.created_at || res.created_at || new Date().toISOString();
    const isNl = res.similarity !== undefined || res.distance !== undefined || (res.metadata?.screenshot_id !== undefined && res.screenshot_id === undefined);
    const timestampMs = normalizeTimestampToMs(timestamp, { assumeUtc: !isNl });

    if (screenshotId !== undefined || imagePath) {
      setSelectedEvent({
        id: screenshotId || -1,
        path: imagePath,
        appName: res.process_name || res.metadata?.process_name,
        windowTitle: res.window_title || res.metadata?.window_title,
        timestamp: timestampMs ?? Date.now(),
        _fromNlSearch: isNl,
      });
      setHighlightedEventId(screenshotId || -1);
      if (timestampMs) {
        setTimelineJump({ time: timestampMs, ts: Date.now() });
      }
    }
    setActiveTab('preview');
  }, [setHighlightedEventId, setSelectedEvent, setTimelineJump]);

  const handleSearchSubmit = ({ query, mode }) => {
    setActiveTab('advanced-search');
    setSearchMode(mode);
    setAdvancedSearchParams({ query, mode, refreshKey: Date.now() });
  };

  return (
    <div
      data-tauri-drag-region
      className="h-screen w-screen text-ide-text overflow-hidden font-sans topbar-acrylic flex flex-col"
      onClickCapture={handleGlobalClick}
    >
      <TopBar
        darkMode={darkMode}
        setDarkMode={setDarkMode}
        setShowSettings={setShowSettings}
        showNotifications={showNotifications}
        setShowNotifications={setShowNotifications}
        isMaximized={isMaximized}
        onMinimize={minimize}
        onToggleMaximize={toggleMaximize}
        onHideToTray={hideToTray}
        onSearchSelect={selectSearchResult}
        onSearchSubmit={handleSearchSubmit}
        searchMode={searchMode}
        onSearchModeChange={setSearchMode}
        backendStatus={backendStatus}
        monitorPaused={monitorPaused}
        handleStartBackend={handleStartBackend}
        handlePauseMonitor={handlePauseMonitor}
        handleResumeMonitor={handleResumeMonitor}
        backendOnline={backendStatus === 'online'}
        isAuthenticated={isAuthenticated}
      />

      <div className={`flex-1 min-h-0 flex flex-col overflow-hidden relative ${isMaximized ? '' : 'mx-[3px] mb-[3px] rounded-md'}`}>
        <Mask
          backendStatus={backendStatus}
          pythonVersion={pythonVersion}
          backendError={backendError}
          handleStartBackend={handleStartBackend}
          onRefreshPythonVersion={refreshPythonVersion}
          depsNeedUpdate={depsNeedUpdate}
          depsSyncing={depsSyncing}
          onDepsSync={handleDepsSync}
          modelsNeedDownload={modelsNeedDownload}
          missingModels={missingModels}
          onModelsDownloadComplete={handleModelsDownloadComplete}
        />

        <AuthMask
          isVisible={pythonVersion && !isAuthenticated}
          onAuthSuccess={handleAuthSuccess}
          authError={authError}
          setAuthError={setAuthError}
        />

        <SecurityAlertMask
          alert={securityAlert}
          onDismiss={() => setSecurityAlert(null)}
        />

        <ErrorWindow
          isVisible={criticalErrors.length > 0}
          errors={criticalErrors}
          logPath={criticalErrorLogPath}
          onRestart={restartApp}
          onExit={exitApp}
        />

        <StartupVacuumDialog />

        {isAuthenticated && <HmacMigrationDialog />}

        <ExtensionSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && showExtensionSetup}
          onComplete={handleExtensionSetupComplete}
        />

        <ClusteringSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && !showExtensionSetup && showClusteringSetup}
          onComplete={handleClusteringSetupComplete}
        />

        <SmartClusterSetupWizard
          isVisible={backendStatus === 'online' && isAuthenticated && !showExtensionSetup && !showClusteringSetup && showSmartClusterSetup}
          onComplete={handleSmartClusterSetupComplete}
        />

        <Timeline
          onSelectEvent={(evt) => {
            setSelectedEvent(evt);
            setHighlightedEventId(evt?.id ?? null);
          }}
          onClearHighlight={() => setHighlightedEventId(null)}
          jumpTimestamp={timelineJump}
          highlightedEventId={highlightedEventId}
          refreshKey={timelineRefreshKey}
          sqlPaused={!windowFocused}
        />

        <main className="flex-1 flex flex-col md:flex-row overflow-hidden relative bg-ide-bg">
          <ActivityBar
            activeTab={activeTab}
            setActiveTab={setActiveTab}
            expanded={sidebarExpanded}
            onToggleExpand={() => setSidebarExpanded((prev) => !prev)}
          />

          <MainArea
            activeTab={activeTab}
            setActiveTab={setActiveTab}
            selectedImageSrc={selectedImageSrc}
            isLoadingDetails={isLoadingDetails}
            selectedEvent={selectedEvent}
            selectedDetails={selectedDetails}
            lastError={lastError}
            ocrBoxes={ocrBoxes}
            advancedSearchParams={advancedSearchParams}
            searchMode={searchMode}
            onSearchModeChange={setSearchMode}
            backendOnline={backendStatus === 'online'}
            isAuthenticated={isAuthenticated}
            onAdvancedSelect={selectSearchResult}
            onInspectorBoxClick={(box) => handleCopyText(box.label)}
            onDeleteRecord={async (id) => {
              try {
                await deleteScreenshot(id);
                clearSelection();
                bumpTimelineRefresh();
              } catch (e) {
                console.error('Failed to delete record', e);
              }
            }}
            onDeleteNearbyRecords={async (timestamp, minutes) => {
              try {
                const ts = normalizeTimestampToMs(timestamp);
                if (ts) {
                  await deleteRecordsByTimeRange(minutes, ts);
                }
                clearSelection();
                bumpTimelineRefresh();
              } catch (e) {
                console.error('Failed to delete nearby records', e);
              }
            }}
            onCopyText={handleCopyText}
          />
        </main>
      </div>

      <NotificationToast
        notifications={toastNotifications}
        onClose={handleToastClose}
      />
      <NotificationPanel
        notifications={notifications}
        onClear={clearNotifications}
        onDismiss={dismissNotification}
        isOpen={showNotifications}
        onClosePanel={() => setShowNotifications(false)}
      />

      <UpdateModal
        isVisible={updateModalVisible}
        updateInfo={updateInfo}
        downloading={updateDownloading}
        downloadProgress={updateDownloadProgress}
        downloadError={updateDownloadError}
        onDownload={handleDownloadUpdate}
        onLater={handleLater}
        onClose={() => setUpdateModalVisible(false)}
      />

      <SettingsDialog
        isOpen={showSettings && isAuthenticated}
        onClose={() => {
          setShowSettings(false);
          refreshPythonVersion();
        }}
        autoStartMonitor={autoStartMonitor}
        onRecordsDeleted={bumpTimelineRefresh}
        powerSavingSuppressed={powerSavingSuppressed}
        powerSavingMode={powerSavingMode}
        onPowerSavingModeChange={setPowerSavingMode}
        onAutoStartMonitorChange={setAutoStartMonitor}
        onManualStartMonitor={handleManualStartMonitor}
        onManualStopMonitor={handleManualStopMonitor}
        sessionTimeout={sessionTimeout}
        onSessionTimeoutChange={setSessionTimeout}
        isSessionValid={isAuthenticated}
        onLockSession={handleLockSession}
      />
    </div>
  );
}

export default App;
