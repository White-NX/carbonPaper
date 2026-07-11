import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getLightweightConfig, setLightweightConfig, switchToLightweightMode } from '../../lib/lightweight_api';
import { withAuth } from '../../lib/auth_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';

const RESOURCE_POLICY_STORAGE_KEY = 'settings.resourcePolicy';

const RESOURCE_POLICY_OPTIONS = [
  {
    value: 'complete',
    powerSaving: false,
    gameMode: false,
    colorClass: {
      selected: 'bg-amber-500/20 text-amber-200 border-amber-400/50',
      idle: 'text-amber-300/90 hover:bg-amber-500/10 hover:text-amber-200',
    },
  },
  {
    value: 'balanced',
    powerSaving: true,
    gameMode: false,
    colorClass: {
      selected: 'bg-sky-500/20 text-sky-200 border-sky-400/50',
      idle: 'text-sky-300/90 hover:bg-sky-500/10 hover:text-sky-200',
    },
  },
  {
    value: 'performance',
    powerSaving: true,
    gameMode: true,
    colorClass: {
      selected: 'bg-emerald-500/20 text-emerald-200 border-emerald-400/50',
      idle: 'text-emerald-300/90 hover:bg-emerald-500/10 hover:text-emerald-200',
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
  const [powerSavingMode, setPowerSavingMode] = useState(externalPowerSavingMode !== false);
  const [gameModeEnabled, setGameModeEnabled] = useState(false);
  const [gameModeActive, setGameModeActive] = useState(false);
  const [gameModePermanent, setGameModePermanent] = useState(false);
  const [fullscreenPaused, setFullscreenPaused] = useState(false);
  const [useDml, setUseDml] = useState(false);
  const [gameModeLoading, setGameModeLoading] = useState(true);
  const [resourcePolicyLoading, setResourcePolicyLoading] = useState(false);
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
  const [cardClickBehaviorActivityContext, setCardClickBehaviorActivityContext] = useState(() => localStorage.getItem('cardClickBehavior_activityContext') || 'preview');

  useEffect(() => {
    getLightweightConfig().then(setLightweightConfigState).catch(console.error);
  }, []);

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
      setPowerSavingMode(previous);
      onTogglePowerSaving?.(previous);
      throw err;
    }
  };

  useEffect(() => {
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
  }, []);

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
      setGameModeEnabled(previous);
      throw err;
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
    } catch (error) {
      console.error('Failed to switch to lightweight mode:', error);
    }
  };

  const setCardClickBehavior = (scope, value) => {
    localStorage.setItem(`cardClickBehavior_${scope}`, value);
    if (scope === 'search') setCardClickBehaviorSearch(value);
    if (scope === 'clusters') setCardClickBehaviorClusters(value);
    if (scope === 'activityContext') setCardClickBehaviorActivityContext(value);
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
    cardClickBehaviorActivityContext,
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
