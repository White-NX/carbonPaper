import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import WorkspaceTimelines from './components/WorkspaceTimelines';
import { useSettingsHost } from './hooks/useSettingsHost';
import { useSavedCaptureFilters } from './components/settings/hooks/useSavedCaptureFilters';
import Mask from './components/Mask';
import AuthMask from './components/AuthMask';
import AppBoundUpgradePrompt from './components/AppBoundUpgradePrompt';
import ActivityBar from './components/ActivityBar';
import MainArea from './components/MainArea';
import TopBar from './components/TopBar';
import WorkspaceHeader from './components/WorkspaceHeader';
import { setPreference, usePreference } from './lib/preference_store';
import { useSnapshotPreviewWindow } from './hooks/useSnapshotPreviewWindow';
import { NotificationToast, NotificationPanel } from './components/Notifications';
import ErrorWindow from './components/ErrorWindow';
import HmacMigrationDialog from './components/HmacMigrationDialog';
import ClipBackfillDialog from './components/ClipBackfillDialog';
import StartupVacuumDialog from './components/StartupVacuumDialog';
import VectorMigrationOverlay from './components/VectorMigrationOverlay';
import OcrModelRepairCard from './components/OcrModelRepairCard';
import { deleteScreenshot, deleteRecordsByTimeRange, warmupThumbnails } from './lib/monitor_api';
import { UpdateModal } from './components/UpdateModal';
import { useAppTheme } from './hooks/useAppTheme';
import { useAppWindowActions, usePowerSavingState, useWindowMaximizedState } from './hooks/useAppWindowState';
import { useAppNotifications } from './hooks/useAppNotifications';
import { useAuthSession } from './hooks/useAuthSession';
import { DialogVisibilityContext } from './hooks/useDialogFocus';
import { useCriticalErrors } from './hooks/useCriticalErrors';
import { useMonitorLifecycle } from './hooks/useMonitorLifecycle';
import { useRequiredModels } from './hooks/useRequiredModels';
import { useSelectedSnapshot, normalizeTimestampToMs } from './hooks/useSelectedSnapshot';
import { searchResultToSnapshotEvent } from './lib/snapshot_preview';
import { useRequiredModelDownload } from './hooks/useRequiredModelDownload';
import { useUpdateManager } from './hooks/useUpdateManager';
import { useTauriEventListener } from './hooks/useTauriEventListener';
import { initAuthListeners } from './lib/auth_api';
import { downloadSizeMb, missingRequiredModels } from './lib/modelSizes';
import OnboardingOverlay from './components/onboarding/OnboardingOverlay';
import { useOnboarding } from './components/onboarding/useOnboarding';
import { OverlayCoordinatorProvider, useOverlayCoordinator } from './components/overlay';

function App() {
  const { t } = useTranslation();
  const overlays = useOverlayCoordinator();

  useEffect(() => initAuthListeners(), []);

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

  const [showOcrModelRepair, setShowOcrModelRepair] = useState(false);
  const [activeTab, setActiveTab] = useState('preview');
  const sidebarExpanded = usePreference('workspaceNavigationExpanded', 'true') === 'true';
  const [searchMode, setSearchMode] = useState('ocr');
  const searchInputRef = useRef(null);
  const [advancedSearchParams, setAdvancedSearchParams] = useState({ query: '', mode: 'ocr', refreshKey: Date.now() });
  const [searchFocusRequest, setSearchFocusRequest] = useState(0);
  const [returnTo, setReturnTo] = useState(null);
  const returnFocusRef = useRef(null);
  const [previewRequest, setPreviewRequest] = useState(null);
  const [timelineSearch, setTimelineSearch] = useState(null);

  const openOcrModelRepair = useCallback(() => {
    setShowOcrModelRepair(true);
    invoke('take_ocr_model_repair_request').catch(() => {});
  }, []);

  useTauriEventListener('show-ocr-model-repair', openOcrModelRepair, [openOcrModelRepair]);

  useEffect(() => {
    invoke('take_ocr_model_repair_request')
      .then((requested) => {
        if (requested) openOcrModelRepair();
      })
      .catch(() => {});
  }, [openOcrModelRepair]);

  const { darkMode, setDarkMode } = useAppTheme();
  const { powerSavingMode, setPowerSavingMode, powerSavingSuppressed, powerStatusReady, windowFocused } = usePowerSavingState();
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
    modelsCheckDone,
    modelsNeedDownload,
    missingModels,
    refreshRequiredModels,
    handleModelsDownloadComplete,
  } = useRequiredModels();
  const requiredDownload = useRequiredModelDownload({
    modelsNeedDownload,
    missingModels,
    onModelsDownloadComplete: handleModelsDownloadComplete,
    t,
  });
  const requiredSizeMb = downloadSizeMb(missingRequiredModels(missingModels));
  const onboarding = useOnboarding({
    isAuthenticated,
    requiredModelsReady: modelsCheckDone && !modelsNeedDownload,
    requiredDownload,
    pushNotification,
    t,
  });
  const {
    autoStartMonitor,
    setAutoStartMonitor,
    handleManualStartMonitor,
    handleManualStopMonitor,
    backendStatus,
    monitorPowerSavingSuppressed,
    monitorPaused,
    handleStartBackend,
    handlePauseMonitor,
    handleResumeMonitor,
    handleSettingsMonitorAction,
  } = useMonitorLifecycle({
    modelsCheckDone,
    modelsNeedDownload,
    captureHeld: onboarding.captureHeld,
    powerSavingSuppressed,
    powerStatusReady,
    formatErrorDetails,
    reportBackendError,
    resetBackendErrorDedupe,
    t,
  });

  useEffect(() => {
    if (backendStatus !== 'online' || !isAuthenticated) return;
    warmupThumbnails()
      .catch((err) => console.warn('[Warmup] Thumbnail warmup failed:', err));
  }, [backendStatus, isAuthenticated]);

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
  } = useSelectedSnapshot({ enabled: isAuthenticated });
  const { showSettings, openSettings } = useSettingsHost({
    onMonitorAction: handleSettingsMonitorAction,
    onRecordsChanged: bumpTimelineRefresh,
    onClosed: refreshRequiredModels,
    onError: (error) => reportBackendError(t('settings.title'), String(error)),
  });
  useSavedCaptureFilters({ monitorStatus: backendStatus === 'online' ? 'running' : backendStatus, enabled: isAuthenticated });
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
    return navigator.clipboard.writeText(text);
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

  const navigate = useCallback((page, options = {}) => {
    setReturnTo(null);
    returnFocusRef.current = null;
    setActiveTab(page);
    if (page === 'advanced-search' && options.focusSearch !== false) setSearchFocusRequest((request) => request + 1);
  }, []);

  const returnToSource = useCallback(() => {
    if (!returnTo) return;
    const trigger = returnFocusRef.current;
    navigate(returnTo, { focusSearch: false });
    requestAnimationFrame(() => { if (trigger?.isConnected) trigger.focus({ preventScroll: true }); });
  }, [navigate, returnTo]);

  const selectSearchResult = useCallback((res, options = {}) => {
    const event = searchResultToSnapshotEvent(res);
    if (!event) return;
    if (activeTab !== 'preview') {
      setReturnTo(activeTab);
      returnFocusRef.current = options.trigger || document.activeElement;
    }
    setSelectedEvent(event);
    setHighlightedEventId(event.id);
    if (event.timestamp !== null) {
      setTimelineJump({ time: event.timestamp, ts: Date.now() });
    }
    setActiveTab('preview');
  }, [activeTab, setHighlightedEventId, setSelectedEvent, setTimelineJump]);

  const handleSearchSubmit = useCallback(({ query, mode }) => {
    if (!isAuthenticated || overlays.active !== null || document.querySelector('[aria-modal="true"]')) return;
    navigate('advanced-search');
    setSearchMode(mode);
    setAdvancedSearchParams((params) => ({ query, mode, refreshKey: params.refreshKey + 1 }));
  }, [isAuthenticated, navigate, overlays.active]);

  const focusQuickSearch = useCallback(() => {
    if (!isAuthenticated || overlays.active !== null || document.querySelector('[aria-modal="true"]')) return;
    searchInputRef.current?.focus();
  }, [isAuthenticated, overlays.active]);

  useEffect(() => {
    const keydown = (event) => {
      if (event.defaultPrevented || document.querySelector('[aria-modal="true"]')) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k' && isAuthenticated && overlays.active === null) {
        event.preventDefault(); focusQuickSearch();
      } else if (event.altKey && event.key === 'ArrowLeft' && activeTab === 'preview' && returnTo && isAuthenticated && overlays.active === null) {
        event.preventDefault(); returnToSource();
      }
    };
    window.addEventListener('keydown', keydown);
    return () => window.removeEventListener('keydown', keydown);
  }, [activeTab, focusQuickSearch, isAuthenticated, overlays.active, returnTo, returnToSource]);

  const previewWindows = useSnapshotPreviewWindow({ isAuthenticated, onOpenTimeline: selectSearchResult });

  return (
    <OverlayCoordinatorProvider value={overlays.value}>
    <DialogVisibilityContext.Provider value={isAuthenticated}>
    <div
      data-tauri-drag-region
      className="h-screen w-screen text-ide-text overflow-hidden font-sans topbar-acrylic flex flex-col"
      onClickCapture={handleGlobalClick}
    >
      <TopBar
        darkMode={darkMode}
        setDarkMode={setDarkMode}
        setShowSettings={() => openSettings()}
        showNotifications={showNotifications}
        setShowNotifications={setShowNotifications}
        isMaximized={isMaximized}
        onMinimize={minimize}
        onToggleMaximize={toggleMaximize}
        onHideToTray={hideToTray}
        searchInputRef={searchInputRef}
        onSearchSelect={selectSearchResult}
        onSearchSubmit={handleSearchSubmit}
        searchMode={searchMode}
        onSearchModeChange={setSearchMode}
        previewCount={previewWindows.tabs.length}
        onRestorePreviews={previewWindows.restore}
        backendStatus={backendStatus}
        powerSavingSuppressed={monitorPowerSavingSuppressed}
        monitorPaused={monitorPaused}
        handleStartBackend={handleStartBackend}
        handlePauseMonitor={handlePauseMonitor}
        handleResumeMonitor={handleResumeMonitor}
        isAuthenticated={isAuthenticated}
        interfaceLocked={overlays.active !== null}
      />

      <div className={`flex-1 min-h-0 flex flex-col overflow-hidden relative ${isMaximized ? '' : 'mx-[3px] mb-[3px] rounded-md'}`}>
        <Mask
          modelsNeedDownload={modelsNeedDownload}
          sizeMb={requiredSizeMb}
          download={requiredDownload}
        />

        <AuthMask
          isVisible={!isAuthenticated}
          onAuthSuccess={handleAuthSuccess}
          authError={authError}
          setAuthError={setAuthError}
        />

        <ErrorWindow
          isVisible={criticalErrors.length > 0}
          errors={criticalErrors}
          logPath={criticalErrorLogPath}
          onRestart={restartApp}
          onExit={exitApp}
        />

        <StartupVacuumDialog />

        {/* Startup overlays share this content area and the coordinator shows
            one at a time (components/overlay/coordinator.js). The top bar is
            locked while any of them is on screen. */}
        <VectorMigrationOverlay />

        {isAuthenticated && <HmacMigrationDialog />}
        {isAuthenticated && <ClipBackfillDialog />}

        <OnboardingOverlay
          onboarding={onboarding}
          required={{
            needDownload: modelsNeedDownload,
            sizeMb: requiredSizeMb,
            progress: requiredDownload.overallProgress,
            error: requiredDownload.modelDownloadError,
            retry: requiredDownload.retryModelDownload,
          }}
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

        <AppBoundUpgradePrompt visible={isAuthenticated && !showSettings} />

        <main className="flex-1 min-h-0 flex flex-row overflow-hidden relative bg-ide-bg">
          <ActivityBar
            activeTab={activeTab}
            setActiveTab={navigate}
            expanded={sidebarExpanded}
            onToggleExpand={() => setPreference('workspaceNavigationExpanded', !sidebarExpanded)}
          />

          <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
            <WorkspaceHeader activeTab={activeTab} />
            <WorkspaceTimelines
              activeTab={activeTab}
              onSelectEvent={(evt) => {
                if (activeTab === 'advanced-search' && evt) {
                  setPreviewRequest({ item: { ...evt, screenshot_id: evt.id, image_path: evt.imagePath, captured_at_ms: evt.timestamp }, token: Date.now() });
                  return;
                }
                setSelectedEvent(evt);
                setHighlightedEventId(evt?.id ?? null);
              }}
              onClearHighlight={() => setHighlightedEventId(null)}
              jumpTimestamp={timelineJump}
              highlightedEventId={highlightedEventId}
              refreshKey={timelineRefreshKey}
              sqlPaused={!windowFocused}
              searchState={timelineSearch}
              onSelectSearchResult={(item) => setPreviewRequest({ item, token: Date.now() })}
            />

            <MainArea
              activeTab={activeTab}
              returnTo={returnTo}
              onReturn={returnToSource}
              setActiveTab={navigate}
              searchFocusRequest={searchFocusRequest}
              recordsRevision={timelineRefreshKey}
              previewRequest={previewRequest}
              onOpenSnapshotPreview={previewWindows.open}
              selectedImageSrc={selectedImageSrc}
              isLoadingDetails={isLoadingDetails}
              selectedEvent={selectedEvent}
              selectedDetails={selectedDetails}
              lastError={lastError}
              onRetrySnapshot={() => setSelectedEvent((event) => event ? { ...event } : null)}
              ocrBoxes={ocrBoxes}
              advancedSearchParams={advancedSearchParams}
              searchMode={searchMode}
              onSearchModeChange={setSearchMode}
              isAuthenticated={isAuthenticated}
              onTimelineSearchChange={setTimelineSearch}
              onAdvancedSelect={selectSearchResult}
              onInspectorBoxClick={(box) => handleCopyText(box.label)}
              onDeleteRecord={async (id) => {
                try {
                  await deleteScreenshot(id);
                  clearSelection();
                  bumpTimelineRefresh();
                } catch (e) {
                  console.error('Failed to delete record', e);
                  throw e;
                }
              }}
              onDeleteNearbyRecords={async (timestamp, minutes) => {
                try {
                  const ts = Number.isFinite(timestamp) ? timestamp : normalizeTimestampToMs(timestamp);
                  if (ts !== null) {
                    await deleteRecordsByTimeRange(minutes, ts);
                  } else throw new Error(t('snapshotPreview.timestampUnavailable'));
                  clearSelection();
                  bumpTimelineRefresh();
                } catch (e) {
                  console.error('Failed to delete nearby records', e);
                  throw e;
                }
              }}
              onCopyText={handleCopyText}
            />
          </div>
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

      <OcrModelRepairCard
        isOpen={showOcrModelRepair}
        onClose={() => setShowOcrModelRepair(false)}
      />
    </div>
    </DialogVisibilityContext.Provider>
    </OverlayCoordinatorProvider>
  );
}

export default App;
