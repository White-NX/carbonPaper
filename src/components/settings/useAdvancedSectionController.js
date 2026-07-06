import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';

export function useAdvancedSectionController({ monitorStatus, t }) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [cpuDropdownOpen, setCpuDropdownOpen] = useState(false);
  const [queueDropdownOpen, setQueueDropdownOpen] = useState(false);
  const [gpuDropdownOpen, setGpuDropdownOpen] = useState(false);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);
  const [onnxChanged, setOnnxChanged] = useState(false);
  const [gpus, setGpus] = useState([]);
  const [gpuLoading, setGpuLoading] = useState(false);
  const [vacuumRunning, setVacuumRunning] = useState(false);
  const [vacuumMessage, setVacuumMessage] = useState('');

  const saveConfig = async (newConfig) => {
    setConfig(newConfig);
    try {
      await withAuth(() => invoke('set_advanced_config', { config: newConfig }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to save advanced config:', err);
    }
  };

  const syncOcrConfigToMonitor = async (newConfig) => {
    if (monitorStatus !== 'running') return;
    try {
      await withAuth(() => invoke('monitor_update_advanced_config', {
        captureOnOcrBusy: newConfig.capture_on_ocr_busy,
        ocrQueueMaxSize: newConfig.ocr_queue_limit_enabled
          ? newConfig.ocr_queue_max_size
          : 999999,
        ocrTimeoutSecs: newConfig.ocr_timeout_secs || 120,
        clusteringAllowFullLowMemory: Boolean(newConfig.clustering_allow_full_low_memory),
      }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to sync OCR config to monitor:', err);
    }
  };

  const loadConfig = async () => {
    try {
      const result = await invoke('get_advanced_config');
      setConfig(result);
    } catch (err) {
      console.error('Failed to load advanced config:', err);
    } finally {
      setLoading(false);
    }
  };

  const loadGpus = async () => {
    setGpuLoading(true);
    try {
      const result = await invoke('enumerate_gpus');
      const gpuList = result || [];
      setGpus(gpuList);
      if (config && gpuList.length > 0 && !gpuList.some((gpu) => gpu.id === config.dml_device_id)) {
        await saveConfig({ ...config, dml_device_id: gpuList[0].id });
      }
    } catch (err) {
      console.error('Failed to enumerate GPUs:', err);
      setGpus([]);
    } finally {
      setGpuLoading(false);
    }
  };

  const refreshVacuumRunningStatus = async () => {
    try {
      const status = await invoke('storage_get_startup_vacuum_status');
      setVacuumRunning(Boolean(status?.in_progress));
    } catch {
      setVacuumRunning(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  useEffect(() => {
    if (config?.use_dml) {
      loadGpus();
    }
  }, [config?.use_dml]);

  useEffect(() => {
    const handler = () => {
      setCpuDropdownOpen(false);
      setQueueDropdownOpen(false);
      setGpuDropdownOpen(false);
      setClusteringDropdownOpen(false);
    };
    if (cpuDropdownOpen || queueDropdownOpen || gpuDropdownOpen || clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
    return undefined;
  }, [cpuDropdownOpen, queueDropdownOpen, gpuDropdownOpen, clusteringDropdownOpen]);

  useEffect(() => {
    refreshVacuumRunningStatus();
  }, []);

  const handleToggle = async (key) => {
    const newConfig = { ...config, [key]: !config[key] };
    await saveConfig(newConfig);
    if (key === 'cpu_limit_enabled') setCpuChanged(true);
    if (key === 'use_dml') setDmlChanged(true);
    if (key === 'use_onnx') setOnnxChanged(true);
    if (key === 'capture_on_ocr_busy' || key === 'ocr_queue_limit_enabled' || key === 'clustering_allow_full_low_memory') {
      await syncOcrConfigToMonitor(newConfig);
    }
  };

  const handleCpuPercentChange = async (value) => {
    setCpuDropdownOpen(false);
    await saveConfig({ ...config, cpu_limit_percent: value });
    setCpuChanged(true);
  };

  const handleQueueSizeChange = async (value) => {
    setQueueDropdownOpen(false);
    const newConfig = { ...config, ocr_queue_max_size: value };
    await saveConfig(newConfig);
    await syncOcrConfigToMonitor(newConfig);
  };

  const handleOcrTimeoutDraftChange = (value) => {
    setConfig({ ...config, ocr_timeout_secs: value });
  };

  const handleOcrTimeoutChange = async (value) => {
    const parsed = Number.parseInt(value, 10);
    const next = Number.isFinite(parsed) ? Math.min(600, Math.max(30, parsed)) : 120;
    const newConfig = { ...config, ocr_timeout_secs: next };
    await saveConfig(newConfig);
    await syncOcrConfigToMonitor(newConfig);
  };

  const handleGpuChange = async (deviceId) => {
    setGpuDropdownOpen(false);
    await saveConfig({ ...config, dml_device_id: deviceId });
    setDmlChanged(true);
  };

  const handleClusteringIntervalChange = async (interval) => {
    setClusteringDropdownOpen(false);
    const newConfig = { ...config, clustering_interval: interval };
    await saveConfig(newConfig);
    try {
      await withAuth(() => invoke('monitor_set_clustering_interval', { interval }), { autoPrompt: true });
    } catch {
      // Best effort; persisted config will be applied on the next monitor refresh.
    }
  };

  const handleManualVacuum = async () => {
    setVacuumMessage('');
    setVacuumRunning(true);
    try {
      const result = await withAuth(() => invoke('storage_run_manual_vacuum'), { autoPrompt: true });
      if (result?.already_running) {
        setVacuumMessage(t('settings.advanced.vacuum.already_running', '已有数据库优化任务正在执行，请稍候。'));
      } else {
        setVacuumMessage(t('settings.advanced.vacuum.success', '数据库优化已完成。'));
      }
    } catch (err) {
      const msg = err?.message || err?.toString() || t('settings.advanced.vacuum.error', '数据库优化失败');
      setVacuumMessage(t('settings.advanced.vacuum.error_with_detail', '数据库优化失败：{{error}}', { error: msg }));
    } finally {
      await refreshVacuumRunningStatus();
    }
  };

  const selectedGpu = config ? (gpus.find((gpu) => gpu.id === config.dml_device_id) || gpus[0]) : null;

  return {
    config,
    loading,
    cpuDropdownOpen,
    queueDropdownOpen,
    gpuDropdownOpen,
    clusteringDropdownOpen,
    cpuChanged,
    dmlChanged,
    onnxChanged,
    gpus,
    gpuLoading,
    vacuumRunning,
    vacuumMessage,
    selectedGpu,
    setCpuDropdownOpen,
    setQueueDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged: () => setCpuChanged(false),
    clearDmlChanged: () => setDmlChanged(false),
    clearOnnxChanged: () => setOnnxChanged(false),
    handleToggle,
    handleCpuPercentChange,
    handleQueueSizeChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
  };
}
