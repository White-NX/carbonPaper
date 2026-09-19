import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getLightweightConfig, setLightweightConfig, switchToLightweightMode } from '../../lib/lightweight_api';
import { withAuth } from '../../lib/auth_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';
import { setPreference } from '../../lib/preference_store';
import { notifySettingsChanged } from '../../lib/settings_api';
import { useSettingsActive, useSettingsActivity } from './SettingsActivityContext';

const RESOURCE_POLICY_STORAGE_KEY = 'settings.resourcePolicy';

const RESOURCE_POLICY_OPTIONS = [
  {
    value: 'complete',
    powerSaving: false,
    gameMode: false,
    colorClass: {
      selected: 'bg-amber-100 text-amber-800 border-amber-500 dark:bg-amber-500/20 dark:text-amber-200 dark:border-amber-400/50',
      idle: 'text-amber-700 hover:bg-amber-100 hover:text-amber-800 dark:text-amber-300/90 dark:hover:bg-amber-500/10 dark:hover:text-amber-200',
    },
  },
  {
    value: 'balanced',
    powerSaving: true,
    gameMode: false,
    colorClass: {
      selected: 'bg-sky-100 text-sky-800 border-sky-500 dark:bg-sky-500/20 dark:text-sky-200 dark:border-sky-400/50',
      idle: 'text-sky-700 hover:bg-sky-100 hover:text-sky-800 dark:text-sky-300/90 dark:hover:bg-sky-500/10 dark:hover:text-sky-200',
    },
  },
  {
    value: 'performance',
    powerSaving: true,
    gameMode: true,
    colorClass: {
      selected: 'bg-emerald-100 text-emerald-800 border-emerald-500 dark:bg-emerald-500/20 dark:text-emerald-200 dark:border-emerald-400/50',
      idle: 'text-emerald-700 hover:bg-emerald-100 hover:text-emerald-800 dark:text-emerald-300/90 dark:hover:bg-emerald-500/10 dark:hover:text-emerald-200',
    },
  },
  {
    value: 'custom',
    colorClass: {
      selected: 'bg-ide-accent/20 text-ide-text border-ide-accent/50',
      idle: 'text-ide-muted hover:bg-ide-hover hover:text-ide-text',
    },
  },
];

function getResourcePolicy(powerSaving, gameMode) {
  if (!powerSaving && !gameMode) return 'complete';
  if (powerSaving && !gameMode) return 'balanced';
  if (powerSaving && gameMode) return 'performance';
  return 'custom';
}

export function useGeneralOptionsController({ externalPowerSavingMode, onTogglePowerSaving, t }) {
  const active = useSettingsActive();
  const [optionError, setOptionError] = useState('');
  const [windowConfigSaving, setWindowConfigSaving] = useState(false);
  const windowQueue = useRef(Promise.resolve());
  const windowWrites = useRef(0);
  const [powerSavingMode, setPowerSavingMode] = useState(externalPowerSavingMode !== false);
  const [gameModeEnabled, setGameModeEnabled] = useState(false);
  const [gameModeActive, setGameModeActive] = useState(false);
  const [gameModePermanent, setGameModePermanent] = useState(false);
  const [fullscreenPaused, setFullscreenPaused] = useState(false);
  const [useDml, setUseDml] = useState(false);
  const [gameModeLoading, setGameModeLoading] = useState(true);
  const [resourcePolicyLoading, setResourcePolicyLoading] = useState(false);
  useSettingsActivity('general-options', { busy: resourcePolicyLoading || windowConfigSaving });
  const [manualResourcePolicy, setManualResourcePolicy] = useState(() => {
    if (typeof window === 'undefined') return null;
    return localStorage.getItem(RESOURCE_POLICY_STORAGE_KEY) === 'custom' ? 'custom' : null;
  });
  const [lightweightConfig, setLightweightConfigState] = useState({
    start_with_window_hidden: false,
    auto_lightweight_enabled: false,
    auto_lightweight_delay_minutes: 5,
  });
  const [cardClickBehaviorSearch, setCardClickBehaviorSearch] = useState(() => localStorage.getItem('cardClickBehavior_search') || 'preview');
  const [cardClickBehaviorClusters, setCardClickBehaviorClusters] = useState(() => localStorage.getItem('cardClickBehavior_clusters') || 'standalone');

  useEffect(() => {
    if (active) getLightweightConfig().then(setLightweightConfigState).catch((error) => setOptionError(t('settings.feedback.readFailed')));
  }, [active, t]);

  useEffect(() => {
    setPowerSavingMode(externalPowerSavingMode !== false);
  }, [externalPowerSavingMode]);

  useTauriEventListener('power-saving-changed', (event) => {
    const payload = event.payload || {};
    setPowerSavingMode(payload.enabled !== false);
  });

  const handleSetPowerSaving = async (next) => {
    const previous = powerSavingMode;
    setPowerSavingMode(next);
    onTogglePowerSaving?.(next);
    try {
      await withAuth(() => invoke('set_power_saving_enabled', { enabled: next }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to set power saving mode:', err);
      setOptionError(t('settings.feedback.saveFailed', { error: String(err) }));
      setPowerSavingMode(previous);
      onTogglePowerSaving?.(previous);
      return false;
    }
  };

  useEffect(() => {
    if (!active) return;
    (async () => {
      try {
        const config = await invoke('get_advanced_config');
        setUseDml(config.use_dml || false);
        setGameModeEnabled(config.game_mode_enabled || false);

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
  }, [active]);

  useTauriEventListener('game-mode-status', (event) => {
    setGameModeActive(event.payload?.active || false);
    setGameModePermanent(event.payload?.permanent || false);
    if (event.payload?.fullscreen_paused !== undefined) {
      setFullscreenPaused(event.payload.fullscreen_paused);
    }
  });

  const handleSetGameMode = async (next) => {
    const previous = gameModeEnabled;
    setGameModeEnabled(next);
    try {
      await withAuth(() => invoke('toggle_game_mode', { enabled: next }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to set game mode:', err);
      setOptionError(t('settings.feedback.saveFailed', { error: String(err) }));
      setGameModeEnabled(previous);
      return false;
    }
  };

  const handleResourcePolicyChange = async (nextPolicy) => {
    if (nextPolicy === 'custom') {
      setManualResourcePolicy('custom');
      localStorage.setItem(RESOURCE_POLICY_STORAGE_KEY, 'custom');
      return;
    }
    const option = RESOURCE_POLICY_OPTIONS.find((item) => item.value === nextPolicy);
    if (!option || resourcePolicyLoading) return;

    const previousPowerSaving = powerSavingMode;
    const previousGameMode = gameModeEnabled;
    const previousManualResourcePolicy = manualResourcePolicy;
    setResourcePolicyLoading(true);
    setOptionError('');
    setManualResourcePolicy(null);
    localStorage.removeItem(RESOURCE_POLICY_STORAGE_KEY);
    setPowerSavingMode(option.powerSaving);
    setGameModeEnabled(option.gameMode);
    onTogglePowerSaving?.(option.powerSaving);
    try {
      if (previousGameMode !== option.gameMode) {
        await withAuth(() => invoke('toggle_game_mode', { enabled: option.gameMode }), { autoPrompt: true });
      }
      if (previousPowerSaving !== option.powerSaving) {
        await withAuth(() => invoke('set_power_saving_enabled', { enabled: option.powerSaving }), { autoPrompt: true });
      }
    } catch (err) {
      console.error('Failed to change resource policy:', err);
      setOptionError(t('settings.feedback.saveFailed', { error: String(err) }));
      if (previousGameMode !== option.gameMode) {
        try {
          await withAuth(() => invoke('toggle_game_mode', { enabled: previousGameMode }), { autoPrompt: true });
        } catch (rollbackErr) {
          console.error('Failed to roll back game mode:', rollbackErr);
        }
      }
      if (previousPowerSaving !== option.powerSaving) {
        try {
          await withAuth(() => invoke('set_power_saving_enabled', { enabled: previousPowerSaving }), { autoPrompt: true });
        } catch (rollbackErr) {
          console.error('Failed to roll back power saving mode:', rollbackErr);
        }
      }
      setPowerSavingMode(previousPowerSaving);
      setGameModeEnabled(previousGameMode);
      setManualResourcePolicy(previousManualResourcePolicy);
      if (previousManualResourcePolicy === 'custom') {
        localStorage.setItem(RESOURCE_POLICY_STORAGE_KEY, 'custom');
      } else {
        localStorage.removeItem(RESOURCE_POLICY_STORAGE_KEY);
      }
      onTogglePowerSaving?.(previousPowerSaving);
    } finally {
      setResourcePolicyLoading(false);
      notifySettingsChanged(['advanced', 'power']);
    }
  };

  const handleLightweightConfigChange = async (key, value) => {
    setLightweightConfigState((current) => ({ ...current, [key]: value }));
    windowWrites.current += 1;
    setWindowConfigSaving(true);
    setOptionError('');
    windowQueue.current = windowQueue.current.then(async () => {
      try { await setLightweightConfig({ [key]: value }); }
      catch (error) {
        setOptionError(t('settings.feedback.saveFailed', { error: String(error) }));
        try { setLightweightConfigState(await getLightweightConfig()); } catch { }
      } finally {
        windowWrites.current -= 1;
        setWindowConfigSaving(windowWrites.current > 0);
      }
    });
    return windowQueue.current;
  };

  const handleSwitchToLightweight = async () => {
    try {
      await switchToLightweightMode();
    } catch (error) {
      console.error('Failed to switch to lightweight mode:', error);
    }
  };

  const setCardClickBehavior = (scope, value) => {
    setPreference(`cardClickBehavior_${scope}`, value);
    if (scope === 'search') setCardClickBehaviorSearch(value);
    if (scope === 'clusters') setCardClickBehaviorClusters(value);
  };

  const derivedResourcePolicy = getResourcePolicy(powerSavingMode, gameModeEnabled);
  const resourcePolicy = manualResourcePolicy || derivedResourcePolicy;
  const resourcePolicyOptions = RESOURCE_POLICY_OPTIONS.map((option) => ({
    ...option,
    label: t(`settings.general.resourcePolicy.options.${option.value}.label`),
    description: t(`settings.general.resourcePolicy.options.${option.value}.description`),
    selectedClassName: option.colorClass.selected,
    idleClassName: `border-transparent ${option.colorClass.idle}`,
  }));
  const selectedResourcePolicy = resourcePolicyOptions.find((option) => option.value === resourcePolicy) || resourcePolicyOptions[2];

  return {
    optionError,
    powerSavingMode,
    gameModeEnabled,
    gameModeActive,
    gameModePermanent,
    fullscreenPaused,
    useDml,
    gameModeLoading,
    resourcePolicyLoading,
    lightweightConfig,
    cardClickBehaviorSearch,
    cardClickBehaviorClusters,
    resourcePolicy,
    resourcePolicyOptions,
    selectedResourcePolicy,
    handleSetPowerSaving,
    handleSetGameMode,
    handleResourcePolicyChange,
    handleLightweightConfigChange,
    handleSwitchToLightweight,
    setCardClickBehavior,
  };
}
