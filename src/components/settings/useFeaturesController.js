import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { useSmartClusterControls } from './organize/useSmartClusterControls';
import { saveAdvancedConfig } from '../../lib/settings_api';
import { useSettingsActive, useSettingsActivity } from './SettingsActivityContext';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';

export function useFeaturesController({
  t,
  featureModeDefinitions,
  getFeatureMode,
}) {
  const active = useSettingsActive();
  const [config, setConfig] = useState(null);
  const [featureSaving, setFeatureSaving] = useState(false);
  const [featureError, setFeatureError] = useState('');
  const savingConfig = useRef(false);
  const [loading, setLoading] = useState(true);
  const [backgroundTimingSaving, setBackgroundTimingSaving] = useState(false);
  const [backgroundTimingError, setBackgroundTimingError] = useState(false);
  const [customControlsOpen, setCustomControlsOpen] = useState(false);
  const smartCluster = useSmartClusterControls();
  useSettingsActivity('feature-operation', { busy: featureSaving || backgroundTimingSaving });
  const { scModelAvailable } = smartCluster;

  const loadConfig = async () => {
    try {
      const result = await invoke('get_advanced_config');
      if (result.classification_enabled === undefined) result.classification_enabled = true;
      setConfig(result);
    } catch (err) {
      setFeatureError(t('settings.feedback.readFailed'));
      console.error('Failed to load advanced config:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (active) loadConfig();
  }, [active]);
  useTauriEventListener('settings-preferences-changed', ({ payload }) => {
    if (active && payload?.includes('advanced') && !savingConfig.current) loadConfig();
  });

  const saveConfig = async (newConfig) => {
    if (savingConfig.current) return false;
    const patch = Object.fromEntries(Object.entries(newConfig).filter(([key, value]) => !Object.is(config?.[key], value)));
    savingConfig.current = true;
    setFeatureSaving(true);
    setFeatureError('');
    try {
      await saveAdvancedConfig(patch);
      setConfig((current) => ({ ...current, ...patch }));
      return true;
    } catch (err) {
      setFeatureError(t('settings.feedback.saveFailed', { error: String(err) }));
      return false;
    } finally { savingConfig.current = false; setFeatureSaving(false); }
  };

  const handleFeatureModeChange = async (mode) => {
    if (!config) return;
    if (mode === 'smart' && !scModelAvailable) return;

    const option = featureModeDefinitions.find((item) => item.value === mode);
    if (!option) return;

    setCustomControlsOpen(false);
    await saveConfig({
      ...config,
      ...option.config,
    });
  };

  const handleCustomFeatureToggle = async (key) => {
    if (!config) return;
    if (key === 'smart_cluster_enabled' && !config.smart_cluster_enabled && !scModelAvailable) return;

    await saveConfig({
      ...config,
      [key]: !config[key],
    });
  };

  const handleBackgroundTimingChange = async (mode) => {
    if (!config || backgroundTimingSaving || !['auto', 'idle_only'].includes(mode)) return;
    setBackgroundTimingSaving(true);
    setBackgroundTimingError(false);
    try {
      await withAuth(() => invoke('set_advanced_config', {
        config: { background_scheduling_mode: mode },
      }), { autoPrompt: true });
      setConfig((current) => ({ ...current, background_scheduling_mode: mode }));
    } catch (error) {
      setBackgroundTimingError(true);
      console.warn('Failed to save background timing:', error);
    } finally {
      setBackgroundTimingSaving(false);
    }
  };

  const featureMode = config ? getFeatureMode(config) : 'minimal';
  const featureModeOptions = featureModeDefinitions.map((option) => ({
    ...option,
    label: t(`settings.features.management.featureMode.options.${option.value}.label`),
    description: t(`settings.features.management.featureMode.options.${option.value}.description`),
    disabled: option.value === 'smart' && !scModelAvailable,
    title: option.value === 'smart' && !scModelAvailable
      ? t('settings.features.management.smartCluster.modelMissing', '请先下载模型')
      : t(`settings.features.management.featureMode.options.${option.value}.description`),
  }));
  const selectedFeatureMode = featureModeOptions.find((option) => option.value === featureMode) || featureModeOptions[0];

  useEffect(() => {
    if (featureMode === 'custom') {
      setCustomControlsOpen(true);
    }
  }, [featureMode]);

  return {
    config,
    loading,
    featureSaving,
    featureError,
    customControlsOpen,
    setCustomControlsOpen,
    scModelAvailable,
    ...smartCluster,
    handleFeatureModeChange,
    handleCustomFeatureToggle,
    handleBackgroundTimingChange,
    backgroundTimingSaving,
    backgroundTimingError,
    featureMode,
    featureModeOptions,
    selectedFeatureMode,
  };
}
