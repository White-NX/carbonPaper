import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Settings as SettingsIcon, Shield, Info, Activity, Image as ImageIcon, Database, HardDrive, Wrench } from 'lucide-react';
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
  const [updateInfo, setUpdateInfo] = useState(null); // { version, body, update }
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
        setDeleteMessage(`删除失败: ${result.error}`);
      } else {
        const count = result.deleted_count || 0;
        setDeleteMessage(`成功删除 ${count} 条记录`);
        onRecordsDeleted?.();
      }
    } catch (e) {
      setDeleteMessage(`删除失败: ${e?.message || e}`);
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
      setSaveFiltersMessage('已保存并同步到监控服务');
    } else if (result.reason === 'not_running') {
      setSaveFiltersMessage('已保存到本地，监控服务未启动，启动后会自动同步');
    } else if (result.reason === 'unsupported') {
      setSaveFiltersMessage('已保存到本地，但当前运行的监控进程不支持过滤命令，请重启监控服务');
    } else {
      setSaveFiltersMessage(`已保存到本地，同步失败：${result.error?.message || result.error || '未知错误'}`);
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
      setAutoLaunchMessage(e?.message || '读取开机自启动状态失败');
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
      setAutoLaunchMessage(Boolean(result) ? '已写入开机启动项' : '已移除开机启动项');
    } catch (e) {
      setAutoLaunchMessage(`操作失败：${formatInvokeError(e)}`);
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
        setAnalysisError(err?.message || 'Failed to load analysis data');
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

  const storageSegments = useMemo(() => {
    if (!storage) return [];
    return [
      { key: 'models', label: '模型', bytes: storage.models_bytes, icon: Activity, color: 'bg-indigo-500/70' },
      { key: 'images', label: '图片', bytes: storage.images_bytes, icon: ImageIcon, color: 'bg-sky-500/70' },
      { key: 'database', label: '数据库', bytes: storage.database_bytes, icon: Database, color: 'bg-emerald-500/70' },
      { key: 'other', label: '程序依赖', bytes: storage.other_bytes, icon: HardDrive, color: 'bg-amber-500/70' },
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
        setUpdateInfo({ version: result.version, body: result.body, update: result.update });
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
    if (!updateInfo?.update) return;
    setDownloading(true);
    setDownloadProgress({ downloaded: 0, contentLength: 0 });
    try {
      await downloadAndInstallUpdate(updateInfo.update, (progress) => {
        setDownloadProgress(progress);
      });
    } catch (err) {
      setUpdateError(err?.message || String(err));
    } finally {
      setDownloading(false);
    }
  };

  const tabs = [
    { id: 'general', label: '通用', icon: SettingsIcon },
    { id: 'security', label: '安全', icon: Shield },
    { id: 'advanced', label: '高级', icon: Wrench },
    { id: 'analysis', label: '存储管理', icon: HardDrive },
    { id: 'about', label: '关于', icon: Info },
  ];

  return (
    <Dialog
      isOpen={isOpen}
      onClose={onClose}
      title="设置"
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
            <AdvancedSection monitorStatus={monitorStatus} />
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
