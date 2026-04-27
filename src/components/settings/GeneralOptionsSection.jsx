import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Circle } from 'lucide-react';
import { getLightweightConfig, setLightweightConfig, switchToLightweightMode } from '../../lib/lightweight_api';

export default function GeneralOptionsSection({
  lowResolutionAnalysis,
  onToggleLowRes,
  sendTelemetryDiagnostics,
  onToggleTelemetry,
  powerSavingMode,
  onTogglePowerSaving,
}) {
  const { t } = useTranslation();
  const [gameModeEnabled, setGameModeEnabled] = useState(false);
  const [gameModeActive, setGameModeActive] = useState(false);
  const [gameModePermanent, setGameModePermanent] = useState(false);
  const [fullscreenPaused, setFullscreenPaused] = useState(false);
  const [useDml, setUseDml] = useState(false);
  const [gameModeLoading, setGameModeLoading] = useState(true);

  // 轻量模式状态
  const [lightweightConfig, setLightweightConfigState] = useState({
    start_with_window_hidden: false,
    auto_lightweight_enabled: false,
    auto_lightweight_delay_minutes: 5,
  });

  useEffect(() => {
    getLightweightConfig().then(setLightweightConfigState).catch(console.error);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const config = await invoke('get_advanced_config');
        setUseDml(config.use_dml || false);
        setGameModeEnabled(config.game_mode_enabled || false);

        // get initial game mode status
        const status = await invoke('get_game_mode_status');
        setGameModeActive(status.active || false);
        setGameModePermanent(status.permanent || false);
        setFullscreenPaused(status.fullscreen_paused || false);
      } catch (err) {
        console.error('Failed to load config for game mode:', err);
      } finally {
        setGameModeLoading(false);
      }
    })();
  }, []);

  useEffect(() => {
    const unlisten = listen('game-mode-status', (event) => {
      setGameModeActive(event.payload?.active || false);
      setGameModePermanent(event.payload?.permanent || false);
      if (event.payload?.fullscreen_paused !== undefined) {
        setFullscreenPaused(event.payload.fullscreen_paused);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const handleToggleGameMode = async () => {
    const next = !gameModeEnabled;
    setGameModeEnabled(next);
    try {
      await invoke('toggle_game_mode', { enabled: next });
    } catch (err) {
      console.error('Failed to toggle game mode:', err);
      setGameModeEnabled(!next);
    }
  };

  const handleLightweightConfigChange = async (key, value) => {
    const newConfig = { ...lightweightConfig, [key]: value };
    setLightweightConfigState(newConfig);
    try {
      await setLightweightConfig(newConfig);
    } catch (error) {
      console.error('Failed to save lightweight config:', error);
    }
  };

  const handleSwitchToLightweight = async () => {
    try {
      await switchToLightweightMode();
      // 窗口将被销毁，此代码不会执行
    } catch (error) {
      console.error('Failed to switch to lightweight mode:', error);
    }
  };

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 block">{t('settings.general.title')}</label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.telemetry.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.telemetry.description')}
            </p>
          </div>
          <button
            onClick={onToggleTelemetry}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${sendTelemetryDiagnostics ? 'bg-ide-accent' : 'bg-ide-border'
              }`}
            title={t('settings.general.telemetry.label')}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${sendTelemetryDiagnostics ? 'translate-x-5' : 'translate-x-0.5'
                }`}
            />
          </button>
        </div>

        {/* Power saving */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.powerSaving.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.powerSaving.description')}
            </p>
          </div>
          <button
            onClick={onTogglePowerSaving}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${powerSavingMode ? 'bg-ide-accent' : 'bg-ide-border'
              }`}
            title={t('settings.general.powerSaving.label')}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${powerSavingMode ? 'translate-x-5' : 'translate-x-0.5'
                }`}
            />
          </button>
        </div>

        {/* Game Mode */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <label className="block font-semibold text-ide-text">{t('settings.general.gameMode.label')}</label>
            </div>
            <p className="text-xs text-ide-muted">
              {t('settings.general.gameMode.description')}
            </p>
            {gameModeEnabled && useDml && (
              <p className={`text-xs mt-1 ${gameModeActive ? 'text-ide-warning' : 'text-ide-info-success'}`}>
                {gameModePermanent
                  ? t('settings.general.gameMode.permanent')
                  : gameModeActive
                    ? t('settings.general.gameMode.active')
                    : t('settings.general.gameMode.inactive')
                }
              </p>
            )}
            {gameModeEnabled && fullscreenPaused && (
              <p className="text-xs mt-1 text-ide-warning">
                {t('settings.general.gameMode.fullscreen_paused')}
              </p>
            )}
          </div>
          <button
            onClick={handleToggleGameMode}
            disabled={gameModeLoading}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${gameModeLoading
              ? 'bg-ide-border opacity-50 cursor-not-allowed'
              : gameModeEnabled
                ? 'bg-ide-accent'
                : 'bg-ide-border'
              }`}
            title={t('settings.general.gameMode.label')}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${gameModeEnabled ? 'translate-x-5' : 'translate-x-0.5'
                }`}
            />
          </button>
        </div>

        {/* 轻量模式 */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.lightweightMode.startHidden.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.lightweightMode.startHidden.description')}
            </p>
          </div>
          <button
            onClick={() => handleLightweightConfigChange('start_with_window_hidden', !lightweightConfig.start_with_window_hidden)}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${lightweightConfig.start_with_window_hidden ? 'bg-ide-accent' : 'bg-ide-border'}`}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${lightweightConfig.start_with_window_hidden ? 'translate-x-5' : 'translate-x-0.5'}`}
            />
          </button>
        </div>

        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div className="flex-1">
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.lightweightMode.autoSwitch.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.lightweightMode.autoSwitch.description')}
            </p>
            {lightweightConfig.auto_lightweight_enabled && (
              <div className="flex items-center gap-2 mt-2">
                <span className="text-xs text-ide-muted">{t('settings.general.lightweightMode.autoSwitch.delayLabel')}</span>
                <input
                  type="number"
                  min="1"
                  max="60"
                  value={lightweightConfig.auto_lightweight_delay_minutes}
                  onChange={(e) => {
                    const val = parseInt(e.target.value, 10);
                    // 只有当值是有效数字时才更新，否则使用默认值 5
                    if (!isNaN(val) && val >= 1 && val <= 60) {
                      handleLightweightConfigChange('auto_lightweight_delay_minutes', val);
                    } else if (e.target.value === '') {
                      // 如果用户清空输入框，暂时不更新状态，等待用户输入
                      // 但为了避免显示 NaN，我们保持当前值
                    }
                  }}
                  onBlur={(e) => {
                    // 失焦时，如果值无效，恢复为默认值 5
                    const val = parseInt(e.target.value, 10);
                    if (isNaN(val) || val < 1 || val > 60) {
                      handleLightweightConfigChange('auto_lightweight_delay_minutes', 5);
                    }
                  }}
                  className="w-16 px-2 py-1 bg-ide-panel border border-ide-border rounded text-ide-text text-xs"
                />
                <span className="text-xs text-ide-muted">{t('settings.general.lightweightMode.autoSwitch.delayUnit')}</span>
              </div>
            )}
          </div>
          <button
            onClick={() => handleLightweightConfigChange('auto_lightweight_enabled', !lightweightConfig.auto_lightweight_enabled)}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${lightweightConfig.auto_lightweight_enabled ? 'bg-ide-accent' : 'bg-ide-border'}`}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${lightweightConfig.auto_lightweight_enabled ? 'translate-x-5' : 'translate-x-0.5'}`}
            />
          </button>
        </div>

        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.lightweightMode.switchNow.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.lightweightMode.switchNow.description')}
            </p>
          </div>
          <button
            onClick={handleSwitchToLightweight}
            className="px-3 py-1.5 bg-ide-panel border border-ide-border rounded text-ide-text text-sm hover:bg-ide-bg transition-colors"
          >
            {t('settings.general.lightweightMode.switchNow.button')}
          </button>
        </div>

      </div>
    </div>
  );
}
