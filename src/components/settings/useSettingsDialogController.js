import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { updateMonitorFilters, deleteRecordsByTimeRange } from '../../lib/monitor_api';
import { getAnalysisOverview } from '../../lib/analysis_api';
import { defaultFilterSettings, formatInvokeError, normalizeList } from './filterUtils';
import { REFRESH_INTERVAL_MS } from './analysisUtils';
import { checkForUpdate, downloadAndInstallUpdate } from '../../lib/update_api';
import { withAuth } from '../../lib/auth_api';

function readInitialFilterSettings() {
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
}

export function useSettingsDialogController({
  isOpen,
  activeTab,
  onManualStartMonitor,
  onManualStopMonitor,
  onRecordsDeleted,
  t,
}) {
  const [lowResolutionAnalysis, setLowResolutionAnalysis] = useState(() => localStorage.getItem('lowResolutionAnalysis') === 'true');
  const [sendTelemetryDiagnostics, setSendTelemetryDiagnostics] = useState(() => localStorage.getItem('sendTelemetryDiagnostics') === 'true');
  const [monitorStatus, setMonitorStatus] = useState('stopped');
  const monitorStatusRef = useRef('stopped');
  const [filterSettings, setFilterSettings] = useState(readInitialFilterSettings);
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
  const [updateInfo, setUpdateInfo] = useState(null);
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
      } catch {
        setMonitorStatus('running');
        monitorStatusRef.current = 'running';
      }
    } catch {
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
      titles: (prev.titles || []).filter((item) => item !== tag),
    }));
    setFiltersDirty(true);
    setSaveFiltersMessage('');
  };

  const handleToggleProtected = () => {
    setFilterSettings((prev) => ({ ...prev, ignoreProtected: !prev.ignoreProtected }));
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
      await withAuth(() => invoke('start_monitor'), { autoPrompt: true });
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
      await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
    } catch (e) {
      console.error('Failed to stop monitor', e);
    } finally {
      onManualStopMonitor?.();
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  };

  const handleRestartMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
      setMonitorStatus('waiting');
      monitorStatusRef.current = 'waiting';
      await withAuth(() => invoke('start_monitor'), { autoPrompt: true });
      await checkMonitorStatus();
    } catch (e) {
      console.error('Failed to restart monitor', e);
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
      await checkMonitorStatus();
    }
  };

  const handlePauseMonitor = async () => {
    try {
      await withAuth(() => invoke('pause_monitor'), { autoPrompt: true });
      await checkMonitorStatus();
    } catch (e) {
      console.error(e);
    }
  };

  const handleResumeMonitor = async () => {
    try {
      await withAuth(() => invoke('resume_monitor'), { autoPrompt: true });
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
    [analysisRefreshing, t],
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
    } catch {
      // ignore
    }
  }, [lowResolutionAnalysis]);

  useEffect(() => {
    try {
      localStorage.setItem('sendTelemetryDiagnostics', sendTelemetryDiagnostics ? 'true' : 'false');
    } catch {
      // ignore
    }
  }, [sendTelemetryDiagnostics]);

  useEffect(() => {
    if (!isOpen || activeTab !== 'analysis') return undefined;
    loadAnalysisOverview(false);
    const timer = setInterval(() => loadAnalysisOverview(false), REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [isOpen, activeTab, loadAnalysisOverview]);

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
    setDownloadProgress({ phase: 'downloading', downloaded: 0, contentLength: 0 });
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

  return {
    lowResolutionAnalysis,
    toggleLowResolutionAnalysis: () => setLowResolutionAnalysis((value) => !value),
    sendTelemetryDiagnostics,
    toggleTelemetryDiagnostics: () => setSendTelemetryDiagnostics((value) => !value),
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
  };
}
