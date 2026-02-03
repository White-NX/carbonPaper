import React from 'react';
import { Play, Pause, Square as StopSquare, Loader2 } from 'lucide-react';

export default function MonitorServiceSection({
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
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 block">Monitor Service</label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block mb-1 font-semibold text-ide-text">
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
            <p className="text-xs text-ide-muted">Control the background screenshot & OCR service.</p>
          </div>
          <div className="flex gap-2 shrink-0">
            {monitorStatus === 'stopped' || monitorStatus === 'waiting' ? (
              <button
                onClick={onStart}
                disabled={monitorStatus === 'loading' || monitorStatus === 'waiting'}
                className="flex items-center gap-2 px-4 py-2 bg-green-600 hover:bg-green-700 text-white rounded-lg text-xs font-medium transition-colors disabled:opacity-50"
              >
                {monitorStatus === 'waiting' ? (
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                ) : (
                  <Play className="w-3.5 h-3.5 fill-current" />
                )}
                {monitorStatus === 'waiting' ? 'Starting...' : 'Start Service'}
              </button>
            ) : (
              <>
                {monitorStatus === 'paused' ? (
                  <button
                    onClick={onResume}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-green-500 transition-colors"
                    title="Resume"
                  >
                    <Play className="w-4 h-4 fill-current" />
                  </button>
                ) : (
                  <button
                    onClick={onPause}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-yellow-500 transition-colors"
                    title="Pause"
                  >
                    <Pause className="w-4 h-4 fill-current" />
                  </button>
                )}
                <button
                  onClick={onStop}
                  className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-red-500 transition-colors"
                  title="Stop"
                >
                  <StopSquare className="w-4 h-4 fill-current" />
                </button>
              </>
            )}
          </div>
        </div>

        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
            <div>
              <label className="block mb-1 font-semibold text-ide-text">启动时自动子服务</label>
              <p className="text-xs text-ide-muted">开启后应用启动时会自动尝试拉起 Python 子服务。</p>
            </div>
            <button
              onClick={() => onAutoStartMonitorChange?.(!autoStartMonitor)}
              className={`w-11 h-6 shrink-0 rounded-full transition-colors relative ${
                autoStartMonitor ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
              }`}
              title="应用启动后自动运行截图/OCR后台"
            >
              <div
                className="absolute top-1 w-4 h-4 rounded-full bg-white transition-transform shadow-sm"
                style={{ left: autoStartMonitor ? 'calc(100% - 1.25rem)' : '0.25rem' }}
              />
            </button>
          </div>

        <div className="flex items-center justify-between gap-4">
            <div className="flex-1">
              <label className="block mb-1 font-semibold text-ide-text">开机自启动</label>
              <p className="text-xs text-ide-muted mb-1">写入注册表 HKLM，需管理员确认。</p>
              <p className="text-xs text-ide-muted/80">
                {autoLaunchMessage ||
                  (autoLaunchEnabled === null
                    ? '正在读取开机自启动状态...'
                    : autoLaunchEnabled
                      ? '已开启：电脑开机后自动启动 CarbonPaper'
                      : '未开启：不会随系统启动')}
              </p>
            </div>
            <button
              onClick={onToggleAutoLaunch}
              disabled={autoLaunchLoading}
              className={`shrink-0 flex items-center gap-2 px-3 py-2 rounded-lg text-xs font-medium transition-colors border border-ide-border ${
                autoLaunchEnabled ? 'bg-green-600 hover:bg-green-700 text-white border-transparent' : 'bg-ide-panel hover:bg-ide-hover text-ide-text'
              } disabled:opacity-50`}
            >
              {autoLaunchLoading && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
              {autoLaunchEnabled ? '关闭开机自启' : '开启开机自启'}
            </button>
          </div>
      </div>
    </div>
  );
}
