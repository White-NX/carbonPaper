import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import { saveAdvancedConfig } from '../../../lib/settings_api';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';
import { useSettingsActivity } from '../SettingsActivityContext';

export function useAdvancedPreferences({ monitorStatus, t, active = true, devicesActive = active }) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [configSaving, setConfigSaving] = useState(false);
  const [configError, setConfigError] = useState('');
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);
  const [gpus, setGpus] = useState([]);
  const [gpuLoading, setGpuLoading] = useState(false);
  const saving = useRef(false);
  const request = useRef(0);
  useSettingsActivity('advanced-preferences', { busy: configSaving });

  const loadConfig = useCallback(async () => {
    const id = ++request.current;
    try {
      const result = await invoke('get_advanced_config');
      if (request.current === id) { setConfig(result); setConfigError(''); }
    } catch (error) {
      if (request.current === id) setConfigError(t('settings.feedback.readFailed'));
    } finally { if (request.current === id) setLoading(false); }
  }, [t]);
  useEffect(() => {
    if (active) loadConfig();
    return () => { request.current += 1; };
  }, [active, loadConfig]);
  useTauriEventListener('settings-preferences-changed', ({ payload }) => {
    if (active && payload?.includes('advanced') && !saving.current) loadConfig();
  });

  useEffect(() => {
    if (!devicesActive || !config?.use_dml) return undefined;
    let cancelled = false;
    setGpuLoading(true);
    invoke('enumerate_gpus').then((result) => { if (!cancelled) setGpus(result || []); })
      .catch(() => { if (!cancelled) setGpus([]); })
      .finally(() => { if (!cancelled) setGpuLoading(false); });
    return () => { cancelled = true; };
  }, [devicesActive, config?.use_dml]);

  const savePatch = async (patch) => {
    if (!config || saving.current) return false;
    saving.current = true;
    setConfigSaving(true);
    setConfigError('');
    try {
      await saveAdvancedConfig(patch);
      setConfig((current) => ({ ...current, ...patch }));
      if ('cpu_limit_enabled' in patch || 'cpu_limit_percent' in patch) setCpuChanged(true);
      if ('use_dml' in patch || 'dml_device_id' in patch) setDmlChanged(true);
      if (monitorStatus === 'running' && 'ocr_timeout_secs' in patch) {
        try {
          await withAuth(() => invoke('monitor_update_advanced_config', {
            ocrTimeoutSecs: patch.ocr_timeout_secs ?? config.ocr_timeout_secs ?? 120,
          }), { autoPrompt: true });
        } catch (error) { setConfigError(t('settings.feedback.savedPendingRestart')); setCpuChanged(true); }
      }
      return true;
    } catch (error) {
      setConfigError(t('settings.feedback.saveFailed', { error: String(error) }));
      return false;
    } finally { saving.current = false; setConfigSaving(false); }
  };

  return {
    config, loading, configSaving, configError, loadConfig, cpuChanged, dmlChanged, gpus, gpuLoading,
    selectedGpu: gpus.find((gpu) => gpu.id === config?.dml_device_id),
    clearCpuChanged: () => setCpuChanged(false), clearDmlChanged: () => setDmlChanged(false),
    handleToggle: (key) => savePatch({ [key]: !config[key] }),
    handleCpuPercentChange: (value) => savePatch({ cpu_limit_percent: value }),
    handleGpuChange: (value) => savePatch({ dml_device_id: value }),
    handleOcrTimeoutChange: (value) => {
      const parsed = Number(value);
      if (!Number.isInteger(parsed) || parsed < 30 || parsed > 600) return Promise.resolve(false);
      return savePatch({ ocr_timeout_secs: parsed });
    },
  };
}
