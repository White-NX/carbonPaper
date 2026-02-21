import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Settings as SettingsIcon, Shield, Info, Activity, Image as ImageIcon, Database, HardDrive, Wrench, Languages } from 'lucide-react';
import { Dialog } from '../Dialog';
import { updateMonitorFilters, deleteRecordsByTimeRange } from '../../lib/monitor_api';
import { getAnalysisOverview } from '../../lib/analysis_api';
import MonitorServiceSection from './MonitorServiceSection';
import GeneralOptionsSection from './GeneralOptionsSection';
import CaptureFiltersSection from './CaptureFiltersSection';
import SecuritySection from './SecuritySection';
import StorageManagementSection from './StorageManagementSection';
import AboutSection from './AboutSection';
import AdvancedSection from './AdvancedSection';
import LanguageSection from './LanguageSection';
import { defaultFilterSettings, formatInvokeError, normalizeList } from './filterUtils';
import { REFRESH_INTERVAL_MS } from './analysisUtils';
import { checkForUpdate, downloadAndInstallUpdate } from '../../lib/update_api';

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
}) {
  const [activeTab, setActiveTab] = useState('general');
  const [lowResolutionAnalysis, setLowResolutionAnalysis] = useState(() => localStorage.getItem('lowResolutionAnalysis') === 'true');
  const [sendTelemetryDiagnostics, setSendTelemetryDiagnostics] = useState(() => localStorage.getItem('sendTelemetryDiagnostics') === 'true');
  const [monitorStatus, setMonitorStatus] = useState('stopped');
  const monitorStatusRef = useRef('stopped');
  const [filterSettings, setFilterSettings] = useState(() => {
    try {
      const saved = JSON.parse(localStorage.getItem('monitorFilters') || 'null');
      if (saved && typeof saved === 'object') {
        return {
          ...defaultFilterSettings,
          ...saved,
          processes: Array.isArray(saved.processes) ? saved.processes : [],
          titles: Array.isArray(saved.titles) ? saved.titles : [],
          ignoreProtected: typeof saved.ignoreProtected === 'boolean' ? saved.ignoreProtected : true,
        };
      }
    } catch (e) {
      console.warn('Failed to read saved filters', e);
    }
    return defaultFilterSettings;
  });
  const [processInput, setProcessInput] = useState('');
  const [titleInput, setTitleInput] = useState('');
  const [filtersDirty, setFiltersDirty] = useState(false);
  const [savingFilters, setSavingFilters] = useState(false);
  const [saveFiltersMessage, setSaveFiltersMessage] = useState('');
  const [autoLaunchEnabled, setAutoLaunchEnabled] = useState(null);
  const [autoLaunchLoading, setAutoLaunchLoading] = useState(false);
  const [autoLaunchMessage, setAutoLaunchMessage] = useState('');

  const [memorySeries, setMemorySeries] = useState([]);
  const [storage, setStorage] = useState(null);
  const [analysisLoading, setAnalysisLoading] = useState(true);
  const [analysisRefreshing, setAnalysisRefreshing] = useState(false);
  const [analysisError, setAnalysisError] = useState('');
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [upToDate, setUpToDate] = useState(false);
  const [updateInfo, setUpdateInfo] = useState(null); // { version, body }
  const [updateError, setUpdateError] = useState('');
  const [downloading, setDownloading] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState({ downloaded: 0, contentLength: 0 });
  const [isDeleting, setIsDeleting] = useState(false);
  const [deleteMessage, setDeleteMessage] = useState('');

  const checkMonitorStatus = async () => {
    try {
      const resString = await invoke('get_monitor_status');
      try {
        const res = JSON.parse(resString);
        if (res.stopped) {
          setMonitorStatus('stopped');
          monitorStatusRef.current = 'stopped';
        } else if (res.paused) {
          setMonitorStatus('paused');
          monitorStatusRef.current = 'paused';
        } else {
          setMonitorStatus('running');
          monitorStatusRef.current = 'running';
        }
      } catch (parseError) {
        setMonitorStatus('running');
        monitorStatusRef.current = 'running';
      }
    } catch (e) {
      if (monitorStatusRef.current === 'waiting') {
        return;
      }
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  };

  const addProcessTags = () => {
    const items = normalizeList(processInput);
    if (!items.length) return;
    setFilterSettings((prev) => {
      const merged = Array.from(new Set([...(prev.processes || []), ...items]));
      return { ...prev, processes: merged };
    });
    setProcessInput('');
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const addTitleTags = () => {
    const items = normalizeList(titleInput);
    if (!items.length) return;
    setFilterSettings((prev) => {
      const merged = Array.from(new Set([...(prev.titles || []), ...items]));
      return { ...prev, titles: merged };
    });
    setTitleInput('');
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const removeProcessTag = (tag) => {
    setFilterSettings((prev) => ({
      ...prev,
      processes: (prev.processes || []).filter((p) => p !== tag),
    }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const removeTitleTag = (tag) => {
    setFilterSettings((prev) => ({
      ...prev,
      titles: (prev.titles || []).filter((t) => t !== tag),
    }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const syncFiltersToMonitor = async (filtersPayload = filterSettings) => {
    if (monitorStatus !== 'running') {
      return { ok: false, reason: 'not_running' };
    }
    try {
      await updateMonitorFilters({
        processes: filtersPayload.processes,
        titles: filtersPayload.titles,
        ignore_protected: filtersPayload.ignoreProtected,
      });
      return { ok: true };
    } catch (e) {
      if (e?.code === 'unsupported') {
        return { ok: false, reason: 'unsupported' };
      }
      return { ok: false, reason: 'error', error: e };
    }
  };

  const handleQuickDelete = async (minutes) => {
    setIsDeleting(true);
    setDeleteMessage('');
    try {
      const result = await deleteRecordsByTimeRange(minutes);
      if (result.error) {
        setDeleteMessage(t('settings.delete.failure', { error: result.error }));
      } else {
        const count = result.deleted_count || 0;
        setDeleteMessage(t('settings.delete.success', { count }));
        onRecordsDeleted?.();
      }
    } catch (e) {
      setDeleteMessage(t('settings.delete.failure', { error: e?.message || e }));
    } finally {
      setIsDeleting(false);
    }
  };

  const handleSaveFilters = async () => {
    setSavingFilters(true);
    setSaveFiltersMessage('');

    const nextFilters = { ...filterSettings };

    setFilterSettings(nextFilters);
    setFiltersDirty(false);

    const result = await syncFiltersToMonitor(nextFilters);
    setSavingFilters(false);
    if (result.ok) {
      setSaveFiltersMessage(t('settings.save_filters.synced'));
    } else if (result.reason === 'not_running') {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_not_running'));
    } else if (result.reason === 'unsupported') {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_unsupported'));
    } else {
      setSaveFiltersMessage(t('settings.save_filters.saved_local_sync_failed', { error: result.error?.message || result.error || 'Unknown error' }));
    }
  };

  const handleStartMonitor = async () => {
    setMonitorStatus('waiting');
    monitorStatusRef.current = 'waiting';
    onManualStartMonitor?.();
    try {
      await invoke('start_monitor');
    } catch (e) {
      console.error('Failed to start monitor', e);
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  };

  const handleStopMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      await invoke('stop_monitor');
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
      onManualStopMonitor?.();
    } catch (e) {
      console.error('Failed to stop monitor', e);
    }
  };

  const handleRestartMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      await invoke('stop_monitor');
      setMonitorStatus('waiting');
      monitorStatusRef.current = 'waiting';
      await invoke('start_monitor');
      await checkMonitorStatus();
    } catch (e) {
      console.error('Failed to restart monitor', e);
      await checkMonitorStatus();
    }
  };

  const handlePauseMonitor = async () => {
    try {
      await invoke('pause_monitor');
      await checkMonitorStatus();
    } catch (e) {
      console.error(e);
    }
  };

  const handleResumeMonitor = async () => {
    try {
      await invoke('resume_monitor');
      await checkMonitorStatus();
    } catch (e) {
      console.error(e);
    }
  };

  const refreshAutoLaunchStatus = async () => {
    setAutoLaunchLoading(true);
    setAutoLaunchMessage('');
    try {
      const enabled = await invoke('get_autostart_status');
      setAutoLaunchEnabled(Boolean(enabled));
    } catch (e) {
      setAutoLaunchMessage(e?.message || t('settings.autolaunch.read_error'));
      setAutoLaunchEnabled(null);
    } finally {
      setAutoLaunchLoading(false);
    }
  };

  const handleToggleAutoLaunch = async () => {
    setAutoLaunchLoading(true);
    setAutoLaunchMessage('');
    try {
      const next = !(autoLaunchEnabled ?? false);
      const result = await invoke('set_autostart', { enabled: next });
      setAutoLaunchEnabled(Boolean(result));
      setAutoLaunchMessage(Boolean(result) ? t('settings.autolaunch.enabled') : t('settings.autolaunch.disabled'));
    } catch (e) {
      setAutoLaunchMessage(t('settings.autolaunch.action_failed', { error: formatInvokeError(e) }));
    } finally {
      setAutoLaunchLoading(false);
    }
  };

  const loadAnalysisOverview = useCallback(
    async (forceStorage = false) => {
      try {
        setAnalysisError('');
        if (!analysisRefreshing) {
          setAnalysisLoading(true);
        }
        const result = await getAnalysisOverview(forceStorage);
        setMemorySeries(result?.memory || []);
        setStorage(result?.storage || null);
      } catch (err) {
        setAnalysisError(err?.message || t('settings.analysis.load_failed', { error: '' }));
      } finally {
        setAnalysisLoading(false);
        setAnalysisRefreshing(false);
      }
    },
    [analysisRefreshing],
  );

  const handleRefreshAnalysis = () => {
    setAnalysisRefreshing(true);
    loadAnalysisOverview(true);
  };

  useEffect(() => {
    let interval;
    if (isOpen) {
      checkMonitorStatus();
      refreshAutoLaunchStatus();
      interval = setInterval(checkMonitorStatus, 2000);
    }
    return () => clearInterval(interval);
  }, [isOpen]);

  useEffect(() => {
    localStorage.setItem('monitorFilters', JSON.stringify(filterSettings));
  }, [filterSettings]);

  useEffect(() => {
    if (monitorStatus === 'running') {
      syncFiltersToMonitor();
    }
  }, [monitorStatus]);

  useEffect(() => {
    monitorStatusRef.current = monitorStatus;
  }, [monitorStatus]);

  useEffect(() => {
    try {
      localStorage.setItem('lowResolutionAnalysis', lowResolutionAnalysis ? 'true' : 'false');
    } catch (e) {
      /* ignore */
    }
  }, [lowResolutionAnalysis]);

  useEffect(() => {
    try {
      localStorage.setItem('sendTelemetryDiagnostics', sendTelemetryDiagnostics ? 'true' : 'false');
    } catch (e) {
      /* ignore */
    }
  }, [sendTelemetryDiagnostics]);

  useEffect(() => {
    if (!isOpen || activeTab !== 'analysis') return undefined;
    loadAnalysisOverview(false);
    const timer = setInterval(() => loadAnalysisOverview(false), REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [isOpen, activeTab, loadAnalysisOverview]);

  const { t } = useTranslation();

  const storageSegments = useMemo(() => {
    if (!storage) return [];
    return [
      { key: 'models', label: t('settings.storage.models'), bytes: storage.models_bytes, icon: Activity, color: 'bg-indigo-500/70' },
      { key: 'images', label: t('settings.storage.images'), bytes: storage.images_bytes, icon: ImageIcon, color: 'bg-sky-500/70' },
      { key: 'database', label: t('settings.storage.database'), bytes: storage.database_bytes, icon: Database, color: 'bg-emerald-500/70' },
      { key: 'other', label: t('settings.storage.other'), bytes: storage.other_bytes, icon: HardDrive, color: 'bg-amber-500/70' },
    ];
  }, [storage]);

  const totalStorage = storage?.total_bytes || 0;

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpToDate(false);
    setUpdateInfo(null);
    setUpdateError('');
    try {
      const result = await checkForUpdate();
      if (result.available) {
        setUpdateInfo({ version: result.version, body: result.body });
      } else {
        setUpToDate(true);
      }
    } catch (err) {
      setUpdateError(err?.message || String(err));
    } finally {
      setCheckingUpdate(false);
    }
  };

  const handleDownloadUpdate = async () => {
    if (!updateInfo) return;
    setDownloading(true);
    setDownloadProgress({ downloaded: 0, contentLength: 0 });
    try {
      await downloadAndInstallUpdate((progress) => {
        setDownloadProgress(progress);
      });
    } catch (err) {
      setUpdateError(err?.message || String(err));
    } finally {
      setDownloading(false);
    }
  };

  const tabs = [
    { id: 'general', label: t('settings.tabs.general'), icon: SettingsIcon },
    { id: 'language', label: t('settings.tabs.language'), icon: Languages },
    { id: 'security', label: t('settings.tabs.security'), icon: Shield },
    { id: 'advanced', label: t('settings.tabs.advanced'), icon: Wrench },
    { id: 'analysis', label: t('settings.tabs.analysis'), icon: HardDrive },
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
              <tab.icon className="w-4 h-4" />
              {tab.label}
            </button>
          ))}
        </div>

        <div className="flex-1 overflow-y-auto p-6 scrollbar-thin">
          {activeTab === 'general' && (
            <div className="space-y-6">
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
              />

              <GeneralOptionsSection
                lowResolutionAnalysis={lowResolutionAnalysis}
                onToggleLowRes={() => setLowResolutionAnalysis((v) => !v)}
                sendTelemetryDiagnostics={sendTelemetryDiagnostics}
                onToggleTelemetry={() => setSendTelemetryDiagnostics((v) => !v)}
              />
            </div>
          )}

          {activeTab === 'language' && (
            <LanguageSection />
          )}

          {activeTab === 'security' && (
            <div className="space-y-8">
              {/* Windows Hello 安全设置 */}
              <SecuritySection
                sessionTimeout={sessionTimeout}
                onSessionTimeoutChange={onSessionTimeoutChange}
                isSessionValid={isSessionValid}
                onLockSession={onLockSession}
              />

              {/* 捕获过滤器设置 */}
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
                onToggleProtected={() => {
                  setFilterSettings((prev) => ({ ...prev, ignoreProtected: !prev.ignoreProtected }));
                  setFiltersDirty(true);
                  setSaveFiltersMessage('');
                }}
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

          {activeTab === 'advanced' && (
            <AdvancedSection monitorStatus={monitorStatus} onRestartMonitor={handleRestartMonitor} />
          )}

          {activeTab === 'analysis' && (
            <StorageManagementSection
              storageSegments={storageSegments}
              totalStorage={totalStorage}
              storage={storage}
              loading={analysisLoading}
              refreshing={analysisRefreshing}
              error={analysisError}
              onRefresh={handleRefreshAnalysis}
            />
          )}

          {activeTab === 'about' && (
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
          )}
        </div>
      </div>
    </Dialog>
  );
}

export default SettingsDialog;
