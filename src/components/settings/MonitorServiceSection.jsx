import React from 'react';
import { useTranslation } from 'react-i18next';
import { Play, Pause, Square as StopSquare, Loader2, RotateCw, HelpCircle } from 'lucide-react';

export default function MonitorServiceSection({
  monitorStatus,
  onStart,
  onStop,
  onPause,
  onResume,
  onRestart,
  autoStartMonitor,
  onAutoStartMonitorChange,
  autoLaunchEnabled,
  autoLaunchLoading,
  autoLaunchMessage,
  onToggleAutoLaunch,
}) {
  const { t } = useTranslation();
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-1.5 px-1">
        <label className="text-sm font-semibold text-ide-accent block">{t('settings.general.monitor.title')}</label>
        <div className="relative group">
          <HelpCircle className="w-3.5 h-3.5 text-ide-muted cursor-help" />
          <div className="absolute left-1/2 -translate-x-1/2 top-full mt-2 w-60 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg shadow-lg text-xs text-ide-muted opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto transition-opacity z-50">
            {t('settings.general.monitor.tooltip')}
          </div>
        </div>
      </div>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block mb-1 font-semibold text-ide-text">
              {t('settings.general.monitor.status_label')}{' '}
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
            <p className="text-xs text-ide-muted">{t('settings.general.monitor.description')}</p>
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
                {monitorStatus === 'waiting' ? t('settings.general.monitor.starting') : t('settings.general.monitor.start')}
              </button>
            ) : (
              <>
                {monitorStatus === 'paused' ? (
                  <button
                    onClick={onResume}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-green-500 transition-colors"
                    title={t('settings.general.monitor.resume')}
                  >
                    <Play className="w-4 h-4 fill-current" />
                  </button>
                ) : (
                  <button
                    onClick={onPause}
                    className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-yellow-500 transition-colors"
                    title={t('settings.general.monitor.pause')}
                  >
                    <Pause className="w-4 h-4 fill-current" />
                  </button>
                )}
                <button
                  onClick={onStop}
                  className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-red-500 transition-colors"
                  title={t('settings.general.monitor.stop')}
                >
                  <StopSquare className="w-4 h-4 fill-current" />
                </button>
                <button
                  onClick={onRestart}
                  className="p-2 bg-ide-panel hover:bg-ide-hover border border-ide-border rounded-lg text-blue-400 transition-colors"
                  title={t('settings.general.monitor.restart')}
                >
                  <RotateCw className="w-4 h-4" />
                </button>
              </>
            )}
          </div>
        </div>

        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
            <div>
              <label className="block mb-1 font-semibold text-ide-text">{t('settings.general.monitor.autoStart.label')}</label>
              <p className="text-xs text-ide-muted">{t('settings.general.monitor.autoStart.description')}</p>
            </div>
            <button
              onClick={() => onAutoStartMonitorChange?.(!autoStartMonitor)}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${
                autoStartMonitor ? 'bg-ide-accent' : 'bg-ide-border'
              }`}
              title={t('settings.general.monitor.autoStart.tooltip')}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                  autoStartMonitor ? 'translate-x-5' : 'translate-x-0.5'
                }`}
              />
            </button>
          </div>

        <div className="flex items-center justify-between gap-4">
            <div className="flex-1">
              <label className="block mb-1 font-semibold text-ide-text">{t('settings.general.monitor.autoLaunch.label')}</label>
              <p className="text-xs text-ide-muted mb-1">{t('settings.general.monitor.autoLaunch.description')}</p>
              <p className="text-xs text-ide-muted/80">
                {autoLaunchMessage ||
                  (autoLaunchEnabled === null
                    ? t('settings.general.monitor.autoLaunch.reading')
                    : autoLaunchEnabled
                      ? t('settings.general.monitor.autoLaunch.enabledMessage')
                      : t('settings.general.monitor.autoLaunch.disabledMessage'))}
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
              {autoLaunchEnabled ? t('settings.general.monitor.autoLaunch.disable') : t('settings.general.monitor.autoLaunch.enable')}
            </button>
          </div>
      </div>
    </div>
  );
}
