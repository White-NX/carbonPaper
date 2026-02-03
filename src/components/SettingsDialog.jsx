import React, { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Play, Pause, Square as StopSquare, Loader2, X } from 'lucide-react';
import { Dialog } from './Dialog';
import { updateMonitorFilters } from '../lib/monitor_api';

const defaultFilterSettings = {
  processes: ['carbonpaper.exe'],
  titles: ['carbonpaper', 'pornhub'],
  ignoreProtected: true,
};

const normalizeList = (value) =>
  value
    .split(/[\,\n]+/)
    .map((v) => v.trim())
    .filter(Boolean)
    .map((v) => v.toLowerCase());

const formatInvokeError = (error) => {
  if (!error) return '未知错误';
  if (typeof error === 'string') return error;
  if (typeof error === 'object') {
    if (typeof error.message === 'string' && error.message.trim()) return error.message;
    try {
      return JSON.stringify(error);
    } catch (e) {
      return '未知错误';
    }
  }
  return String(error);
};

function SettingsDialog({
  isOpen,
  onClose,
  autoStartMonitor,
  onAutoStartMonitorChange,
  onManualStartMonitor,
  onManualStopMonitor
}) {
  const [lowResolutionAnalysis, setLowResolutionAnalysis] = useState(() => localStorage.getItem('lowResolutionAnalysis') === 'true');
  const [sendTelemetryDiagnostics, setSendTelemetryDiagnostics] = useState(() => localStorage.getItem('sendTelemetryDiagnostics') === 'true');
  const [monitorStatus, setMonitorStatus] = useState('stopped'); // 'stopped', 'running', 'paused', 'loading', 'waiting'
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
        // Keep waiting until IPC is reachable instead of falling back to stopped.
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

  return (
    <Dialog isOpen={isOpen} onClose={onClose} title="Settings">
      <div className="p-4 space-y-4 text-ide-text">
        <div className="space-y-2">
          <label className="text-sm font-semibold text-ide-accent">Monitor Service</label>
          <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted">
            <div className="flex items-center justify-between">
              <div>
                <label className="ide-label block mb-1 font-semibold text-ide-text">
                  Status:{' '}
                  <span
                    className={`${monitorStatus === 'running'
                      ? 'text-green-500'
                      : monitorStatus === 'paused'
                        ? 'text-yellow-500'
                        : monitorStatus === 'waiting'
                          ? 'text-orange-400'
                          : 'text-red-500'
                      }`}
                  >
                    {monitorStatus.toUpperCase()}
                  </span>
                </label>
                <p className="text-xs text-ide-muted toggle-desc">Control the background screenshot & OCR service.</p>
              </div>
              <div className="flex gap-2">
                {monitorStatus === 'stopped' || monitorStatus === 'waiting' ? (
                  <button
                    onClick={handleStartMonitor}
                    disabled={monitorStatus === 'loading' || monitorStatus === 'waiting'}
                    className="flex items-center gap-2 px-3 py-1.5 bg-green-600 hover:bg-green-700 text-white rounded text-xs transition-colors disabled:opacity-50"
                  >
                    {monitorStatus === 'waiting' ? (
                      <Loader2 className="w-3 h-3 animate-spin" />
                    ) : (
                      <Play className="w-3 h-3 fill-current" />
                    )}
                    {monitorStatus === 'waiting' ? 'Starting...' : 'Start Service'}
                  </button>
                ) : (
                  <>
                    {monitorStatus === 'paused' ? (
                      <button
                        onClick={handleResumeMonitor}
                        className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded text-green-500 transition-colors"
                        title="Resume"
                      >
                        <Play className="w-4 h-4 fill-current" />
                      </button>
                    ) : (
                      <button
                        onClick={handlePauseMonitor}
                        className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded text-yellow-500 transition-colors"
                        title="Pause"
                      >
                        <Pause className="w-4 h-4 fill-current" />
                      </button>
                    )}
                    <button
                      onClick={handleStopMonitor}
                      className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded text-red-500 transition-colors"
                      title="Stop"
                    >
                      <StopSquare className="w-4 h-4 fill-current" />
                    </button>
                  </>
                )}
              </div>
            </div>
            <div className="mt-3 space-y-3 text-ide-text">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <label className="ide-label block mb-1 font-semibold text-ide-text">启动时自动子服务</label>
                  <p className="text-xs text-ide-muted toggle-desc">开启后应用启动时会自动尝试拉起 Python 子服务。</p>
                </div>
                <button
                  onClick={() => onAutoStartMonitorChange?.(!autoStartMonitor)}
                  className={`w-10 h-5 rounded-full transition-colors relative ${autoStartMonitor ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'}`}
                  title="应用启动后自动运行截图/OCR后台"
                >
                  <div
                    className="absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform"
                    style={{ left: autoStartMonitor ? 'calc(100% - 18px)' : '2px' }}
                  />
                </button>
              </div>

              <div className="flex items-center justify-between gap-3">
                <div>
                  <label className="ide-label block mb-1 font-semibold text-ide-text">开机自启动</label>
                  <p className="text-xs text-ide-muted toggle-desc">写入注册表 HKLM，需管理员确认。</p>
                </div>
                <button
                  onClick={handleToggleAutoLaunch}
                  disabled={autoLaunchLoading}
                  className={`flex items-center gap-2 px-3 py-1.5 rounded text-xs transition-colors border border-ide-border ${autoLaunchEnabled ? 'bg-green-600 hover:bg-green-700 text-white' : 'bg-ide-panel hover:bg-ide-hover text-ide-text'
                    } disabled:opacity-50`}
                >
                  {autoLaunchLoading && <Loader2 className="w-3 h-3 animate-spin" />}
                  {autoLaunchEnabled ? '关闭开机自启' : '开启开机自启'}
                </button>
              </div>
              <p className="text-xs text-ide-muted toggle-desc">
                {autoLaunchMessage ||
                  (autoLaunchEnabled === null
                    ? '正在读取开机自启动状态...'
                    : autoLaunchEnabled
                      ? '已开启：电脑开机后自动启动 CarbonPaper'
                      : '未开启：不会随系统启动')}
              </p>
            </div>
          </div>
        </div>

        <div className="space-y-2">
          <label className="text-sm font-semibold text-ide-accent">Capture Filters</label>
          <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted space-y-3">
            <div className="space-y-2">
              <div>
                <label className="ide-label block mb-1 font-semibold text-ide-text">按进程名称忽略</label>
                <div className="flex flex-wrap gap-2 mb-2">
                  {(filterSettings.processes || []).map((p) => (
                    <span key={p} className="inline-flex items-center gap-1 px-2 py-1 bg-ide-panel border border-ide-border rounded text-xs text-ide-text">
                      {p}
                      <button onClick={() => removeProcessTag(p)} className="text-ide-muted hover:text-ide-text" title="移除">
                        <X className="w-3 h-3" />
                      </button>
                    </span>
                  ))}
                  {(filterSettings.processes || []).length === 0 && <span className="text-xs text-ide-muted">暂无规则</span>}
                </div>
                <div className="flex gap-2">
                  <input
                    className="flex-1 bg-ide-panel border border-ide-border rounded px-2 py-1 text-xs text-ide-text focus:outline-none focus:border-ide-accent"
                    value={processInput}
                    onChange={(e) => setProcessInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ',') {
                        e.preventDefault();
                        addProcessTags();
                      }
                    }}
                    placeholder="chrome.exe, obs64.exe"
                  />
                  <button
                    onClick={addProcessTags}
                    disabled={!processInput.trim()}
                    className="px-3 py-1 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs transition-colors disabled:opacity-50"
                  >
                    添加
                  </button>
                </div>
                <p className="text-xs text-ide-muted mt-1 toggle-desc">按进程可执行名匹配，自动转为小写。</p>
              </div>

              <div>
                <label className="ide-label block mb-1 font-semibold text-ide-text">按窗口名关键词忽略</label>
                <div className="flex flex-wrap gap-2 mb-2">
                  {(filterSettings.titles || []).map((t) => (
                    <span key={t} className="inline-flex items-center gap-1 px-2 py-1 bg-ide-panel border border-ide-border rounded text-xs text-ide-text">
                      {t}
                      <button onClick={() => removeTitleTag(t)} className="text-ide-muted hover:text-ide-text" title="移除">
                        <X className="w-3 h-3" />
                      </button>
                    </span>
                  ))}
                  {(filterSettings.titles || []).length === 0 && <span className="text-xs text-ide-muted">暂无规则</span>}
                </div>
                <div className="flex gap-2">
                  <input
                    className="flex-1 bg-ide-panel border border-ide-border rounded px-2 py-1 text-xs text-ide-text focus:outline-none focus:border-ide-accent"
                    value={titleInput}
                    onChange={(e) => setTitleInput(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' || e.key === ',') {
                        e.preventDefault();
                        addTitleTags();
                      }
                    }}
                    placeholder="内部系统, 私人窗口"
                  />
                  <button
                    onClick={addTitleTags}
                    disabled={!titleInput.trim()}
                    className="px-3 py-1 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs transition-colors disabled:opacity-50"
                  >
                    添加
                  </button>
                </div>
                <p className="text-xs text-ide-muted mt-1 toggle-desc">包含匹配，忽略大小写。</p>
              </div>
            </div>

            <div className="flex items-center justify-between">
              <div>
                <label className="ide-label block mb-1 font-semibold text-ide-text">不捕获受保护的窗口</label>
                <p className="text-xs text-ide-muted toggle-desc">关闭后，将尝试捕获设置了屏幕保护属性的窗口。</p>
              </div>
              <button
                onClick={() => {
                  setFilterSettings((prev) => ({ ...prev, ignoreProtected: !prev.ignoreProtected }));
                  setFiltersDirty(true);
                  setSaveFiltersMessage('');
                }}
                className={`w-10 h-5 rounded-full transition-colors relative ${filterSettings.ignoreProtected ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'}`}
              >
                <div
                  className="absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform"
                  style={{ left: filterSettings.ignoreProtected ? 'calc(100% - 18px)' : '2px' }}
                />
              </button>
            </div>

            <div className="flex items-center justify-between gap-3 pt-1">
              <div className="text-xs text-ide-muted">{saveFiltersMessage}</div>
              <button
                onClick={handleSaveFilters}
                disabled={!filtersDirty || savingFilters}
                className="flex items-center gap-2 px-3 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs transition-colors disabled:opacity-50"
              >
                {savingFilters && <Loader2 className="w-3 h-3 animate-spin" />} 保存过滤规则
              </button>
            </div>
          </div>
        </div>

        <div className="space-y-2">
          <label className="text-sm font-semibold text-ide-accent">General</label>
          <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted">
            <div className="flex items-center justify-between">
              <div>
                <label className="ide-label font-semibold text-ide-text">采用低分辨率图片进行数据分析（无效占位选项）</label>
                <p className="text-xs text-ide-muted toggle-desc">低分辨率图片分析可以提高性能，但可能会降低准确性。</p>
              </div>
              <button
                onClick={() => setLowResolutionAnalysis((v) => !v)}
                className={`w-10 h-5 rounded-full transition-colors relative ${lowResolutionAnalysis ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'}`}
                title="应用启动后自动运行截图/OCR后台"
              >
                <div
                  className="absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform"
                  style={{ left: lowResolutionAnalysis ? 'calc(100% - 18px)' : '2px' }}
                />
              </button>
            </div>
            <div className="flex items-center justify-between gap-3 pt-4">
              <div>
                <label className="ide-label font-semibold text-ide-text">Allow sending telemetry diagnostic data</label>
                <p className="text-xs text-ide-muted toggle-desc">
                  Allow program to upload diagnostic information that does not contain privacy data to the telemetry server.
                </p>
              </div>
              <button
                onClick={() => setSendTelemetryDiagnostics((v) => !v)}
                className={`w-10 h-5 rounded-full transition-colors relative ${sendTelemetryDiagnostics ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'}`}
                title="允许发送诊断数据"
              >
                <div
                  className="absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform"
                  style={{ left: sendTelemetryDiagnostics ? 'calc(100% - 18px)' : '2px' }}
                />
              </button>
            </div>
          </div>
        </div>
      </div>
    </Dialog>
  );
}

export default SettingsDialog;
