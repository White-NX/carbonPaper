import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Gamepad2 } from 'lucide-react';

export default function GeneralOptionsSection({
  lowResolutionAnalysis,
  onToggleLowRes,
  sendTelemetryDiagnostics,
  onToggleTelemetry,
}) {
  const { t } = useTranslation();
  const [gameModeEnabled, setGameModeEnabled] = useState(false);
  const [gameModeActive, setGameModeActive] = useState(false);
  const [gameModePermanent, setGameModePermanent] = useState(false);
  const [useDml, setUseDml] = useState(false);
  const [gameModeLoading, setGameModeLoading] = useState(true);

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
            className={`w-11 h-6 shrink-0 rounded-full transition-colors relative ${sendTelemetryDiagnostics ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
              }`}
            title={t('settings.general.telemetry.label')}
          >
            <div
              className="absolute top-1 w-4 h-4 rounded-full bg-white transition-transform shadow-sm"
              style={{ left: sendTelemetryDiagnostics ? 'calc(100% - 1.25rem)' : '0.25rem' }}
            />
          </button>
        </div>

        {/* Game Mode */}
        <div className="w-full h-px bg-ide-border/50" />

        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <Gamepad2 className="w-4 h-4 text-ide-text" />
              <label className="block font-semibold text-ide-text">{t('settings.general.gameMode.label')}</label>
            </div>
            <p className="text-xs text-ide-muted">
              {t('settings.general.gameMode.description')}
            </p>
            {!useDml && !gameModeLoading && (
              <p className="text-xs text-amber-400 mt-1">
                {t('settings.general.gameMode.requires_dml')}
              </p>
            )}
            {gameModeEnabled && useDml && (
              <p className={`text-xs mt-1 ${gameModeActive ? 'text-amber-400' : 'text-emerald-400'}`}>
                {gameModePermanent
                  ? t('settings.general.gameMode.permanent')
                  : gameModeActive
                    ? t('settings.general.gameMode.active')
                    : t('settings.general.gameMode.inactive')
                }
              </p>
            )}
          </div>
          <button
            onClick={handleToggleGameMode}
            disabled={!useDml || gameModeLoading}
            className={`w-11 h-6 shrink-0 rounded-full transition-colors relative ${
              !useDml || gameModeLoading
                ? 'bg-ide-border opacity-50 cursor-not-allowed'
                : gameModeEnabled
                  ? 'bg-ide-accent'
                  : 'bg-ide-panel border border-ide-border'
            }`}
            title={t('settings.general.gameMode.label')}
          >
            <div
              className="absolute top-1 w-4 h-4 rounded-full bg-white transition-transform shadow-sm"
              style={{ left: gameModeEnabled && useDml ? 'calc(100% - 1.25rem)' : '0.25rem' }}
            />
          </button>
        </div>

      </div>
    </div>
  );
}
