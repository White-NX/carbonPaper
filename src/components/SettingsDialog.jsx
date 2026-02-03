import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  Play,
  Pause,
  Square as StopSquare,
  Loader2,
  X,
  Settings as SettingsIcon,
  Shield,
  Info,
  BarChart3,
  Activity,
  HardDrive,
  Image as ImageIcon,
  Database,
  RefreshCw,
  Github,
  User,
  CheckCircle2,
} from 'lucide-react';
import { Dialog } from './Dialog';
import { updateMonitorFilters } from '../lib/monitor_api';
import { APP_VERSION } from '../lib/version';
import { getAnalysisOverview } from '../lib/analysis_api';

const defaultFilterSettings = {
  processes: ['carbonpaper.exe'],
  titles: ['carbonpaper', 'pornhub'],
  ignoreProtected: true,
};

const REFRESH_INTERVAL_MS = 30000;

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

const formatBytes = (bytes) => {
  if (bytes === null || bytes === undefined) return '--';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / Math.pow(1024, index);
  return `${value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2)} ${units[index]}`;
};

const formatTimestamp = (ms) => {
  if (!ms) return '--';
  return new Date(ms).toLocaleString();
};

const buildLinePath = (points) => {
  if (!points || points.length === 0) return '';
  const times = points.map((p) => p.timestamp_ms);
  const values = points.map((p) => p.rss_bytes);
  const minTime = Math.min(...times);
  const maxTime = Math.max(...times);
  const minVal = Math.min(...values);
  const maxVal = Math.max(...values);
  const spanTime = Math.max(maxTime - minTime, 1);
  const spanVal = Math.max(maxVal - minVal, 1);

  return points
    .map((p, index) => {
      const x = ((p.timestamp_ms - minTime) / spanTime) * 100;
      const y = 100 - ((p.rss_bytes - minVal) / spanVal) * 100;
      return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`;
    })
    .join(' ');
};

// Monitor service control section
function MonitorServiceSection({
  monitorStatus,
  onStart,
  onStop,
  onPause,
  onResume,
  autoStartMonitor,
  onAutoStartMonitorChange,
  autoLaunchEnabled,
  autoLaunchLoading,
  autoLaunchMessage,
  onToggleAutoLaunch,
}) {
  return (
    <div className="space-y-2">
      <label className="text-sm font-semibold text-ide-accent">Monitor Service</label>
      <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted">
        <div className="flex items-center justify-between">
          <div>
            <label className="ide-label block mb-1 font-semibold text-ide-text">
              Status:{' '}
              <span
                className={`${
                  monitorStatus === 'running'
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
                onClick={onStart}
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
                    onClick={onResume}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded text-green-500 transition-colors"
                    title="Resume"
                  >
                    <Play className="w-4 h-4 fill-current" />
                  </button>
                ) : (
                  <button
                    onClick={onPause}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded text-yellow-500 transition-colors"
                    title="Pause"
                  >
                    <Pause className="w-4 h-4 fill-current" />
                  </button>
                )}
                <button
                  onClick={onStop}
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
              className={`w-10 h-5 rounded-full transition-colors relative ${
                autoStartMonitor ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
              }`}
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
              onClick={onToggleAutoLaunch}
              disabled={autoLaunchLoading}
              className={`flex items-center gap-2 px-3 py-1.5 rounded text-xs transition-colors border border-ide-border ${
                autoLaunchEnabled ? 'bg-green-600 hover:bg-green-700 text-white' : 'bg-ide-panel hover:bg-ide-hover text-ide-text'
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
  );
}

function GeneralOptionsSection({ lowResolutionAnalysis, onToggleLowRes, sendTelemetryDiagnostics, onToggleTelemetry }) {
  return (
    <div className="space-y-2">
      <label className="text-sm font-semibold text-ide-accent">General</label>
      <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted">
        <div className="flex items-center justify-between">
          <div>
            <label className="ide-label font-semibold text-ide-text">采用低分辨率图片进行数据分析（无效占位选项）</label>
            <p className="text-xs text-ide-muted toggle-desc">低分辨率图片分析可以提高性能，但可能会降低准确性。</p>
          </div>
          <button
            onClick={onToggleLowRes}
            className={`w-10 h-5 rounded-full transition-colors relative ${
              lowResolutionAnalysis ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
            }`}
            title="采用低分辨率图片"
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
            onClick={onToggleTelemetry}
            className={`w-10 h-5 rounded-full transition-colors relative ${
              sendTelemetryDiagnostics ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
            }`}
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
  );
}

function CaptureFiltersSection({
  filterSettings,
  processInput,
  titleInput,
  onProcessInputChange,
  onTitleInputChange,
  onAddProcess,
  onAddTitle,
  onRemoveProcess,
  onRemoveTitle,
  onToggleProtected,
  onSave,
  filtersDirty,
  savingFilters,
  saveFiltersMessage,
}) {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <label className="text-sm font-semibold text-ide-accent">Capture Filters</label>
        <div className="p-3 bg-ide-bg border border-ide-border rounded text-sm text-ide-muted space-y-3">
          <div className="space-y-2">
            <div>
              <label className="ide-label block mb-1 font-semibold text-ide-text">按进程名称忽略</label>
              <div className="flex flex-wrap gap-2 mb-2">
                {(filterSettings.processes || []).map((p) => (
                  <span
                    key={p}
                    className="inline-flex items-center gap-1 px-2 py-1 bg-ide-panel border border-ide-border rounded text-xs text-ide-text"
                  >
                    {p}
                    <button onClick={() => onRemoveProcess(p)} className="text-ide-muted hover:text-ide-text" title="移除">
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
                  onChange={(e) => onProcessInputChange(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ',') {
                      e.preventDefault();
                      onAddProcess();
                    }
                  }}
                  placeholder="chrome.exe, obs64.exe"
                />
                <button
                  onClick={onAddProcess}
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
                  <span
                    key={t}
                    className="inline-flex items-center gap-1 px-2 py-1 bg-ide-panel border border-ide-border rounded text-xs text-ide-text"
                  >
                    {t}
                    <button onClick={() => onRemoveTitle(t)} className="text-ide-muted hover:text-ide-text" title="移除">
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
                  onChange={(e) => onTitleInputChange(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ',') {
                      e.preventDefault();
                      onAddTitle();
                    }
                  }}
                  placeholder="内部系统, 私人窗口"
                />
                <button
                  onClick={onAddTitle}
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
              onClick={onToggleProtected}
              className={`w-10 h-5 rounded-full transition-colors relative ${
                filterSettings.ignoreProtected ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
              }`}
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
              onClick={onSave}
              disabled={!filtersDirty || savingFilters}
              className="flex items-center gap-2 px-3 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs transition-colors disabled:opacity-50"
            >
              {savingFilters && <Loader2 className="w-3 h-3 animate-spin" />} 保存过滤规则
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function AnalysisOverviewSection({
  memorySeries,
  memoryStats,
  storageSegments,
  totalStorage,
  storage,
  loading,
  refreshing,
  error,
  onRefresh,
}) {
  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between shrink-0">
        <div className="space-y-1">
          <h2 className="text-xl font-semibold">Analysis</h2>
          <p className="text-xs text-ide-muted">最近半小时内的 Python 子服务资源视图与本地存储占用。</p>
          <p className="text-xs text-ide-muted">此处显示的内存占用为未压缩内存，实际占用约为未压缩值的1/7</p>
        </div>
        <button
          type="button"
          onClick={onRefresh}
          disabled={refreshing}
          className="flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          刷新存储
        </button>
      </div>

      {error && (
        <div className="shrink-0 px-4 py-2 rounded-lg border border-red-500/40 text-xs text-red-200 bg-red-500/10">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 gap-6 w-full">
        <div className="flex flex-col gap-6">
          <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6 flex flex-col">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-2">
                <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                  <Activity className="w-4 h-4" />
                </div>
                <div>
                  <h3 className="font-semibold">Python 内存波动</h3>
                  <p className="text-[11px] text-ide-muted">最近 30 分钟</p>
                </div>
              </div>
              <div className="text-[11px] text-ide-muted">更新: {memoryStats ? formatTimestamp(memoryStats.lastUpdated) : '--'}</div>
            </div>

            <div className="h-64 w-full rounded-xl border border-ide-border bg-ide-bg/70 p-4 relative">
              {loading ? (
                <div className="absolute inset-0 flex items-center justify-center text-sm text-ide-muted">加载中...</div>
              ) : memorySeries.length === 0 ? (
                <div className="absolute inset-0 flex items-center justify-center text-sm text-ide-muted">暂无数据</div>
              ) : (
                <svg viewBox="0 0 100 100" preserveAspectRatio="none" className="w-full h-full">
                  <defs>
                    <linearGradient id="memoryGradient" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="rgba(59, 130, 246, 0.45)" />
                      <stop offset="100%" stopColor="rgba(59, 130, 246, 0)" />
                    </linearGradient>
                  </defs>
                  <path d={`${buildLinePath(memorySeries)} L 100 100 L 0 100 Z`} fill="url(#memoryGradient)" />
                  <path d={buildLinePath(memorySeries)} fill="none" stroke="#60A5FA" strokeWidth="1.5" />
                </svg>
              )}
            </div>

            <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 mt-4 text-xs">
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">当前</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.latest) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">最低</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.min) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">最高</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.max) : '--'}</div>
              </div>
              <div className="rounded-lg border border-ide-border bg-ide-bg/70 p-3">
                <div className="text-ide-muted">平均</div>
                <div className="text-sm font-semibold">{memoryStats ? formatBytes(memoryStats.avg) : '--'}</div>
              </div>
            </div>
          </div>
        </div>

        <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6 flex flex-col">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                <HardDrive className="w-4 h-4" />
              </div>
              <div>
                <h3 className="font-semibold">本地存储占用</h3>
                <p className="text-[11px] text-ide-muted">LocalAppData/CarbonPaper</p>
              </div>
            </div>
            <div className="text-[11px] text-ide-muted">缓存: {storage?.cached_at_ms ? formatTimestamp(storage.cached_at_ms) : '--'}</div>
          </div>

          <div className="rounded-xl border border-ide-border bg-ide-bg/70 p-4 mb-4">
            <div className="text-xs text-ide-muted">总占用</div>
            <div className="text-2xl font-semibold mt-1">{formatBytes(totalStorage)}</div>
            <div className="text-[11px] text-ide-muted mt-1 truncate">{storage?.root_path || '--'}</div>
          </div>

          <div className="space-y-3">
            {storageSegments.map((segment) => {
              const percent = totalStorage ? Math.min((segment.bytes / totalStorage) * 100, 100) : 0;
              const Icon = segment.icon;
              return (
                <div key={segment.key} className="rounded-lg border border-ide-border bg-ide-bg/60 p-3">
                  <div className="flex items-center justify-between text-xs">
                    <div className="flex items-center gap-2">
                      <Icon className="w-3.5 h-3.5 text-ide-muted" />
                      <span className="text-ide-text">{segment.label}</span>
                    </div>
                    <span className="text-ide-muted">{formatBytes(segment.bytes)}</span>
                  </div>
                  <div className="mt-2 h-2 rounded-full bg-ide-border/60 overflow-hidden">
                    <div className={`h-full ${segment.color}`} style={{ width: `${percent}%` }} />
                  </div>
                  <div className="mt-1 text-[10px] text-ide-muted text-right">{percent.toFixed(1)}%</div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}

function AboutSection({ checking, upToDate, onCheckUpdate }) {
  return (
    <div className="flex w-full h-full gap-6 overflow-hidden text-ide-text select-none">
      <div className="flex-1 flex flex-col min-w-0 pt-2">
        <div className="flex items-center gap-6 mb-6">
          <div className="relative w-20 h-20 shrink-0 flex items-center justify-center bg-gradient-to-br from-ide-panel to-ide-bg rounded-3xl shadow-xl border border-ide-border group cursor-default">
            <div className="absolute inset-0 bg-ide-accent/5 rounded-3xl transform rotate-3 group-hover:rotate-6 transition-transform duration-500" />
            <svg
              className="w-10 h-10 text-ide-accent relative z-10 drop-shadow-lg"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
              <polyline points="14 2 14 8 20 8" />
              <line x1="16" y1="13" x2="8" y2="13" />
              <line x1="16" y1="17" x2="8" y2="17" />
              <polyline points="10 9 9 9 8 9" />
            </svg>
          </div>

          <div className="flex flex-col items-start">
            <h1 className="text-3xl font-bold text-ide-text tracking-tight mb-2">CarbonPaper - 复写纸</h1>
            <span className="px-3 py-1 rounded-full bg-ide-panel border border-ide-border text-xs font-mono text-ide-muted">
              {APP_VERSION}
            </span>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto pr-4 text-ide-muted leading-relaxed text-sm space-y-4">
          <h3 className="text-ide-text font-semibold text-lg">关于项目</h3>
          <p>
            复写纸（carbonpaper）是一款开源的屏幕文字捕捉与智能检索工具，旨在帮助用户高效地记录和查找屏幕上的文字内容。
            通过集成本地的OCR技术和语义搜索算法，复写纸能够实时捕捉屏幕文字，并将其转换为可搜索的文本数据。
          </p>
          <p>该项目目前处于早期开发技术性验证阶段。仅少量分发。如遇到问题，请直接联系作者。</p>

          <h3 className="text-ide-text font-semibold text-lg pt-3">您的所有数据都本地处理</h3>
          <p>
            所有处理均在您的设备本地进行。OCR、向量嵌入和数据库存储均为100%离线。您的数据在未经授权的前提下不会离开设备。
          </p>

          <h3 className="text-ide-text font-semibold text-lg pt-3">有关诊断遥测数据的说明</h3>
          <p>
            应用处于技术验证阶段，会收集一些基本的诊断遥测数据以帮助改进应用。这些数据包括但不限于：应用日志、性能指标和使用统计信息。这些数据均为匿名收集，
            <span className="font-semibold text-ide-text">绝对不包含任何个人身份信息和OCR、向量以及数据库数据。</span>
            您可以在设置中选择关闭诊断数据收集功能。
          </p>

          <h3 className="text-ide-text font-semibold text-lg pt-3">核心功能</h3>
          <ul className="list-disc list-inside space-y-1 pl-2">
            <li>实时屏幕OCR</li>
            <li>使用向量嵌入的语义搜索</li>
            <li>历史上下文时间线视图</li>
            <li>隐私过滤器（隐身模式检测）</li>
            <li>低资源占用</li>
          </ul>

          <h3 className="text-ide-text font-semibold text-lg pt-3">目前已知的问题</h3>
          <ul className="list-disc list-inside space-y-1 pl-2">
            <li>OCR识别准确率有待提升。默认采用的低分辨率OCR方案虽然可以节省不少性能，但是准确率比较堪忧。</li>
            <li>启动python子服务存在效率不高的问题。</li>
            <li>使用自然语言搜索时，可能出现不准确的结果。</li>
            <li>在用户焦点处于不可截取的窗口时，会导致时间轴错误显示为长时间停留在上一个焦点。</li>
            <li>某些情况下（如快速切换焦点，画面复杂），可能导致基于OCR的关键词忽略出现错误。</li>
            <li>暂不支持删除历史记录条目。</li>
          </ul>
        </div>
      </div>

      <div className="w-72 flex flex-col gap-4 shrink-0">
        <div className="bg-ide-panel/50 border border-ide-border rounded-2xl p-5 backdrop-blur-sm">
          <div className="flex items-center gap-2 mb-3">
            <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
              <Github className="w-4 h-4 text-ide-text" />
            </div>
            <h3 className="font-semibold text-ide-text">Contributors</h3>
          </div>
          <div className="flex flex-wrap gap-3">
            {[1, 2, 3].map((i) => (
              <div
                key={i}
                className="w-10 h-10 rounded-full bg-ide-bg border border-ide-border flex items-center justify-center text-ide-muted hover:border-ide-accent hover:text-ide-accent transition-all cursor-pointer transform hover:-translate-y-0.5"
              >
                <User className="w-5 h-5" />
              </div>
            ))}
            <button className="h-10 px-4 rounded-full bg-ide-bg border border-dashed border-ide-border text-xs text-ide-muted hover:text-ide-text hover:border-ide-accent hover:bg-ide-accent/5 transition-all flex items-center gap-2">
              + Join
            </button>
          </div>
        </div>

        <div className="bg-ide-panel/50 border border-ide-border rounded-2xl p-5 backdrop-blur-sm flex items-center justify-between group">
          <div>
            <h3 className="font-semibold text-ide-text mb-1">Check Updates</h3>
            <p className="text-xs text-ide-muted">
              {upToDate ? 'Currently on the latest version' : 'New features might be available'}
            </p>
          </div>

          <button
            onClick={onCheckUpdate}
            disabled={checking || upToDate}
            className={`h-10 px-4 rounded-lg text-sm font-medium transition-all flex items-center gap-2 shadow-sm ${
              upToDate
                ? 'bg-emerald-500/10 text-emerald-500 cursor-default border border-emerald-500/20'
                : 'bg-ide-bg border border-ide-border text-ide-text hover:border-ide-accent hover:text-ide-accent hover:shadow-md active:scale-95'
            }`}
          >
            {checking ? (
              <RefreshCw className="w-4 h-4 animate-spin" />
            ) : upToDate ? (
              <CheckCircle2 className="w-4 h-4" />
            ) : (
              <RefreshCw className="w-4 h-4 group-hover:rotate-180 transition-transform duration-500" />
            )}
            {checking ? 'Checking...' : upToDate ? 'Latest' : 'Check Now'}
          </button>
        </div>

        <div className="relative group overflow-hidden bg-gradient-to-br from-indigo-500/10 via-purple-500/5 to-ide-panel border border-indigo-500/20 rounded-2xl p-6 cursor-pointer transition-all duration-300 hover:-translate-y-1 hover:shadow-card">
          <div className="absolute -top-10 -left-10 w-36 h-36 rounded-full bg-gradient-to-br from-indigo-400/20 to-transparent blur-2xl group-hover:scale-110 transition-transform duration-700" />
          <div className="absolute top-6 left-14 w-14 h-14 rounded-full bg-purple-400/10 blur-xl group-hover:translate-x-2 transition-transform duration-500" />
          <Github
            className="absolute -bottom-10 -right-10 w-44 h-44 text-indigo-500/5 rotate-12 group-hover:scale-110 group-hover:rotate-[15deg] group-hover:text-indigo-500/10 transition-all duration-500 ease-out"
            strokeWidth={1}
          />
          <div className="relative z-10">
            <h2 className="text-2xl font-bold text-ide-text mb-2 group-hover:text-indigo-500 transition-colors">Check us on Github</h2>
            <p className="text-ide-muted font-light leading-relaxed">Star our repository and contribute to the development. Your support keeps the code flowing.</p>
          </div>
        </div>
      </div>
    </div>
  );
}

function SettingsDialog({
  isOpen,
  onClose,
  autoStartMonitor,
  onAutoStartMonitorChange,
  onManualStartMonitor,
  onManualStopMonitor,
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

  const memoryStats = useMemo(() => {
    if (!memorySeries.length) return null;
    const values = memorySeries.map((point) => point.rss_bytes);
    const total = values.reduce((sum, value) => sum + value, 0);
    return {
      latest: values[values.length - 1],
      min: Math.min(...values),
      max: Math.max(...values),
      avg: Math.round(total / values.length),
      lastUpdated: memorySeries[memorySeries.length - 1]?.timestamp_ms,
    };
  }, [memorySeries]);

  const storageSegments = useMemo(() => {
    if (!storage) return [];
    return [
      { key: 'models', label: '模型', bytes: storage.models_bytes, icon: Activity, color: 'bg-indigo-500/70' },
      { key: 'images', label: '图片', bytes: storage.images_bytes, icon: ImageIcon, color: 'bg-sky-500/70' },
      { key: 'database', label: '数据库', bytes: storage.database_bytes, icon: Database, color: 'bg-emerald-500/70' },
      { key: 'other', label: '程序关键依赖', bytes: storage.other_bytes, icon: HardDrive, color: 'bg-amber-500/70' },
    ];
  }, [storage]);

  const totalStorage = storage?.total_bytes || 0;

  const handleCheckUpdate = () => {
    setCheckingUpdate(true);
    setTimeout(() => {
      setCheckingUpdate(false);
      setUpToDate(true);
    }, 1500);
  };

  const tabs = [
    { id: 'general', label: '通用', icon: SettingsIcon },
    { id: 'security', label: '安全', icon: Shield },
    { id: 'analysis', label: '分析', icon: BarChart3 },
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
            />
          )}

          {activeTab === 'analysis' && (
            <AnalysisOverviewSection
              memorySeries={memorySeries}
              memoryStats={memoryStats}
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
            <AboutSection checking={checkingUpdate} upToDate={upToDate} onCheckUpdate={handleCheckUpdate} />
          )}
        </div>
      </div>
    </Dialog>
  );
}

export { default } from './settings/SettingsDialog';
