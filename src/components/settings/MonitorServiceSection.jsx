import React from 'react';
import { useTranslation } from 'react-i18next';
import { Play, Pause, Square as StopSquare, Loader2, RotateCw, Squirrel, Circle } from 'lucide-react';
import SettingsHelpTooltip from './SettingsHelpTooltip';
import { SettingsSwitch } from './SettingsControls';

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
  powerSavingSuppressed,
}) {
  const { t } = useTranslation();
  return (
    <div className="space-y-3">
      <div className="flex items-center gap-1.5 px-1">
        <Squirrel className="w-4 h-4 text-ide-accent" />
        <label className="text-sm font-semibold text-ide-accent block">{t('settings.general.monitor.title')}</label>
        <SettingsHelpTooltip>{t('settings.general.monitor.tooltip')}</SettingsHelpTooltip>
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
            {powerSavingSuppressed && monitorStatus === 'stopped' && (
              <p className="text-xs text-yellow-500">{t('settings.general.monitor.power_saving_blocked')}</p>
            )}
          </div>
          <div className="flex gap-2 shrink-0">
            {monitorStatus === 'stopped' || monitorStatus === 'waiting' ? (
              <button
                onClick={onStart}
                disabled={monitorStatus === 'loading' || monitorStatus === 'waiting' || powerSavingSuppressed}
                className="flex items-center gap-2 px-4 py-2 bg-green-600 hover:bg-green-700 text-white rounded-lg text-xs font-medium transition-colors disabled:opacity-50"
                title={powerSavingSuppressed ? t('settings.general.monitor.power_saving_blocked') : undefined}
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
            <SettingsSwitch
              checked={autoStartMonitor}
              onChange={(next) => onAutoStartMonitorChange?.(next)}
              title={t('settings.general.monitor.autoStart.tooltip')}
            />
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
