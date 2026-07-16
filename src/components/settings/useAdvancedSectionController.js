import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';

export function useAdvancedSectionController({ monitorStatus, t }) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [cpuDropdownOpen, setCpuDropdownOpen] = useState(false);
  const [gpuDropdownOpen, setGpuDropdownOpen] = useState(false);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);
  const [onnxChanged, setOnnxChanged] = useState(false);
  const [gpus, setGpus] = useState([]);
  const [gpuLoading, setGpuLoading] = useState(false);
  const [vacuumRunning, setVacuumRunning] = useState(false);
  const [vacuumMessage, setVacuumMessage] = useState('');
  const [mlOcrStatus, setMlOcrStatus] = useState(null);
  const [mlOcrStatusLoading, setMlOcrStatusLoading] = useState(false);
  const [rustOcrModelStatus, setRustOcrModelStatus] = useState(null);
  const [rustOcrModelDownloading, setRustOcrModelDownloading] = useState(false);

  const saveConfig = async (newConfig) => {
    const previousConfig = config;
    setConfig(newConfig);
    try {
      await withAuth(() => invoke('set_advanced_config', { config: newConfig }), { autoPrompt: true });
      return true;
    } catch (err) {
      setConfig(previousConfig);
      console.error('Failed to save advanced config:', err);
      return false;
    }
  };

  const syncOcrConfigToMonitor = async (newConfig) => {
    if (monitorStatus !== 'running') return;
    try {
      await withAuth(() => invoke('monitor_update_advanced_config', {
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
      setGpuDropdownOpen(false);
      setClusteringDropdownOpen(false);
    };
    if (cpuDropdownOpen || gpuDropdownOpen || clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
    return undefined;
  }, [cpuDropdownOpen, gpuDropdownOpen, clusteringDropdownOpen]);

  useEffect(() => {
    refreshVacuumRunningStatus();
  }, []);

  const refreshMlOcrStatus = async () => {
    setMlOcrStatusLoading(true);
    try {
      setMlOcrStatus(await invoke('get_ml_ocr_status'));
    } catch (err) {
      console.warn('Failed to read Rust ML OCR status:', err);
    } finally {
      setMlOcrStatusLoading(false);
    }
  };

  const refreshRustOcrModelStatus = async () => {
    try {
      setRustOcrModelStatus(await invoke('get_rust_ocr_model_status'));
    } catch (err) {
      console.warn('Failed to read Rust OCR model status:', err);
    }
  };

  useEffect(() => {
    refreshRustOcrModelStatus();
  }, []);

  useEffect(() => {
    refreshMlOcrStatus();
    const timer = window.setInterval(refreshMlOcrStatus, 5000);
    return () => window.clearInterval(timer);
  }, []);

  const handleDownloadRustOcrModel = async () => {
    setRustOcrModelDownloading(true);
    try {
      const status = await invoke('download_rust_ocr_model');
      setRustOcrModelStatus(status);
      await refreshMlOcrStatus();
    } catch (err) {
      console.error('Failed to download Rust OCR model:', err);
    } finally {
      setRustOcrModelDownloading(false);
    }
  };

  const handleToggle = async (key) => {
    const newConfig = { ...config, [key]: !config[key] };
    const saved = await saveConfig(newConfig);
    if (!saved) return;
    if (key === 'cpu_limit_enabled') setCpuChanged(true);
    if (key === 'use_dml') setDmlChanged(true);
    if (key === 'use_onnx') setOnnxChanged(true);
    if (key === 'clustering_allow_full_low_memory') {
      await syncOcrConfigToMonitor(newConfig);
    }
    if (key === 'rust_ocr_dml_beta') {
      try {
        await withAuth(
          () => invoke('restart_ml_ocr_worker'),
          { autoPrompt: true },
        );
      } catch (err) {
        console.warn('Failed to restart Rust ML OCR worker:', err);
      }
      await refreshMlOcrStatus();
    }
  };

  const handleRestartMlOcr = async () => {
    try {
      await withAuth(
        () => invoke('restart_ml_ocr_worker'),
        { autoPrompt: true },
      );
    } finally {
      await refreshMlOcrStatus();
    }
  };

  const handleCpuPercentChange = async (value) => {
    setCpuDropdownOpen(false);
    await saveConfig({ ...config, cpu_limit_percent: value });
    setCpuChanged(true);
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
    mlOcrStatus,
    mlOcrStatusLoading,
    rustOcrModelStatus,
    rustOcrModelDownloading,
    setCpuDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged: () => setCpuChanged(false),
    clearDmlChanged: () => setDmlChanged(false),
    clearOnnxChanged: () => setOnnxChanged(false),
    handleToggle,
    handleCpuPercentChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
    handleRestartMlOcr,
    handleDownloadRustOcrModel,
  };
}
