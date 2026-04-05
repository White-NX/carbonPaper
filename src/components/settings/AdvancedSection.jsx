import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Cpu, ListOrdered, ChevronDown, AlertTriangle, Info, Zap, Monitor, Layers, Database, Loader2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

const CPU_PERCENT_OPTIONS = [5, 10, 15, 20, 30, 50];
const OCR_QUEUE_SIZE_OPTIONS = [1, 2, 3, 5, 10];

export default function AdvancedSection({ monitorStatus, onRestartMonitor }) {
  const { t } = useTranslation();
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [cpuDropdownOpen, setCpuDropdownOpen] = useState(false);
  const [queueDropdownOpen, setQueueDropdownOpen] = useState(false);
  const [gpuDropdownOpen, setGpuDropdownOpen] = useState(false);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);
  const [gpus, setGpus] = useState([]);
  const [gpuLoading, setGpuLoading] = useState(false);
  const [vacuumRunning, setVacuumRunning] = useState(false);
  const [vacuumMessage, setVacuumMessage] = useState('');

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

  useEffect(() => {
    loadConfig();
  }, []);

  // Load GPU list when DML is enabled
  useEffect(() => {
    if (config?.use_dml) {
      loadGpus();
    }
  }, [config?.use_dml]);

  const loadGpus = async () => {
    setGpuLoading(true);
    try {
      const result = await invoke('enumerate_gpus');
      const gpuList = result || [];
      setGpus(gpuList);
      // if current dml_device_id is not in the new gpu list, reset to first gpu or null
      if (config && gpuList.length > 0 && !gpuList.some((g) => g.id === config.dml_device_id)) {
        const fallbackId = gpuList[0].id;
        const newConfig = { ...config, dml_device_id: fallbackId };
        await saveConfig(newConfig);
      }
    } catch (err) {
      console.error('Failed to enumerate GPUs:', err);
      setGpus([]);
    } finally {
      setGpuLoading(false);
    }
  };

  const saveConfig = async (newConfig) => {
    setConfig(newConfig);
    try {
      await invoke('set_advanced_config', { config: newConfig });
    } catch (err) {
      console.error('Failed to save advanced config:', err);
    }
  };

  const syncOcrConfigToMonitor = async (newConfig) => {
    if (monitorStatus !== 'running') return;
    try {
      await invoke('execute_monitor_command', {
        payload: {
          command: 'update_advanced_config',
          capture_on_ocr_busy: newConfig.capture_on_ocr_busy,
          ocr_queue_max_size: newConfig.ocr_queue_limit_enabled
            ? newConfig.ocr_queue_max_size
            : 999999,
        },
      });
    } catch (err) {
      console.error('Failed to sync OCR config to monitor:', err);
    }
  };

  const handleToggle = async (key) => {
    const newConfig = { ...config, [key]: !config[key] };
    await saveConfig(newConfig);
    if (key === 'cpu_limit_enabled') {
      setCpuChanged(true);
    }
    if (key === 'use_dml') {
      setDmlChanged(true);
    }
    if (key === 'capture_on_ocr_busy' || key === 'ocr_queue_limit_enabled') {
      await syncOcrConfigToMonitor(newConfig);
    }
  };

  const handleCpuPercentChange = async (value) => {
    setCpuDropdownOpen(false);
    const newConfig = { ...config, cpu_limit_percent: value };
    await saveConfig(newConfig);
    setCpuChanged(true);
  };

  const handleQueueSizeChange = async (value) => {
    setQueueDropdownOpen(false);
    const newConfig = { ...config, ocr_queue_max_size: value };
    await saveConfig(newConfig);
    await syncOcrConfigToMonitor(newConfig);
  };

  const handleGpuChange = async (deviceId) => {
    setGpuDropdownOpen(false);
    const newConfig = { ...config, dml_device_id: deviceId };
    await saveConfig(newConfig);
    setDmlChanged(true);
  };

  const refreshVacuumRunningStatus = async () => {
    try {
      const status = await invoke('storage_get_startup_vacuum_status');
      setVacuumRunning(Boolean(status?.in_progress));
    } catch {
      setVacuumRunning(false);
    }
  };

  const handleManualVacuum = async () => {
    setVacuumMessage('');
    setVacuumRunning(true);
    try {
      const result = await invoke('storage_run_manual_vacuum');
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

  // Close dropdowns on outside click
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
  }, [cpuDropdownOpen, queueDropdownOpen, gpuDropdownOpen]);

  useEffect(() => {
    refreshVacuumRunningStatus();
  }, []);

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        {t('settings.advanced.loading')}
      </div>
    );
  }

  const selectedGpu = gpus.find((g) => g.id === config.dml_device_id) || gpus[0];

  return (
    <div className="space-y-6">

      <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
        <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
        <p className="text-xs text-ide-warning-muted">{t('settings.advanced.warning')}</p>
      </div>

      {/* CPU 限制 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Cpu className="w-4 h-4" />
          {t('settings.advanced.cpu.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.cpu.label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.cpu.description')}</p>
            </div>
            <button
              onClick={() => handleToggle('cpu_limit_enabled')}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${config.cpu_limit_enabled ? 'bg-ide-accent' : 'bg-ide-border'
                }`}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.cpu_limit_enabled ? 'translate-x-5' : 'translate-x-0.5'
                  }`}
              />
            </button>
          </div>

          {config.cpu_limit_enabled && (
            <div className="flex items-center justify-between gap-4">
              <p className="text-sm text-ide-muted">{t('settings.advanced.cpu.percent_label')}</p>
              <div className="relative">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    setCpuDropdownOpen(!cpuDropdownOpen);
                  }}
                  className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[100px]"
                >
                  <span className="flex-1 text-left">{config.cpu_limit_percent}%</span>
                  <ChevronDown
                    className={`w-4 h-4 text-ide-muted transition-transform ${cpuDropdownOpen ? 'rotate-180' : ''}`}
                  />
                </button>
                {cpuDropdownOpen && (
                  <div
                    className="absolute right-0 top-full mt-2 w-32 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                    onClick={(e) => e.stopPropagation()}
                  >
                    {CPU_PERCENT_OPTIONS.map((pct) => (
                      <button
                        key={pct}
                        onClick={() => handleCpuPercentChange(pct)}
                        className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${pct === config.cpu_limit_percent ? 'bg-ide-accent/10' : ''
                          }`}
                      >
                        <span className="text-sm text-ide-text">{pct}%</span>
                        {pct === config.cpu_limit_percent && (
                          <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}

          {cpuChanged && (
            <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
              <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
              <p className="text-xs text-ide-warning-muted flex-1">{t('settings.advanced.cpu.changed_notice')}</p>
              {monitorStatus === 'running' && onRestartMonitor && (
                <button
                  onClick={() => { onRestartMonitor(); setCpuChanged(false); }}
                  className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
                >
                  {t('settings.advanced.quick_restart')}
                </button>
              )}
            </div>
          )}
        </div>
      </div>

      {/* OCR 队列设置 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <ListOrdered className="w-4 h-4" />
          {t('settings.advanced.ocr.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          {/* 截图暂停开关 */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.ocr.pause_label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.ocr.pause_desc')}</p>
            </div>
            <button
              onClick={() => handleToggle('capture_on_ocr_busy')}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${!config.capture_on_ocr_busy ? 'bg-ide-accent' : 'bg-ide-border'
                }`}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${!config.capture_on_ocr_busy ? 'translate-x-5' : 'translate-x-0.5'
                  }`}
              />
            </button>
          </div>

          {/* 队列大小限制 */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.ocr.queue_limit_label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.ocr.queue_limit_desc')}</p>
            </div>
            <button
              onClick={() => handleToggle('ocr_queue_limit_enabled')}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${config.ocr_queue_limit_enabled ? 'bg-ide-accent' : 'bg-ide-border'
                }`}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.ocr_queue_limit_enabled ? 'translate-x-5' : 'translate-x-0.5'
                  }`}
              />
            </button>
          </div>

          {config.ocr_queue_limit_enabled && (
            <div className="flex items-center justify-between gap-4">
              <p className="text-sm text-ide-muted">{t('settings.advanced.ocr.max_queue_label')}</p>
              <div className="relative">
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    setQueueDropdownOpen(!queueDropdownOpen);
                  }}
                  className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[100px]"
                >
                  <span className="flex-1 text-left">{config.ocr_queue_max_size}</span>
                  <ChevronDown
                    className={`w-4 h-4 text-ide-muted transition-transform ${queueDropdownOpen ? 'rotate-180' : ''}`}
                  />
                </button>
                {queueDropdownOpen && (
                  <div
                    className="absolute right-0 top-full mt-2 w-32 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                    onClick={(e) => e.stopPropagation()}
                  >
                    {OCR_QUEUE_SIZE_OPTIONS.map((size) => (
                      <button
                        key={size}
                        onClick={() => handleQueueSizeChange(size)}
                        className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${size === config.ocr_queue_max_size ? 'bg-ide-accent/10' : ''
                          }`}
                      >
                        <span className="text-sm text-ide-text">{size}</span>
                        {size === config.ocr_queue_max_size && (
                          <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}

          {/* 信息提示 */}
          <div className="flex items-start gap-2 p-2.5 bg-ide-panel/50 border border-ide-border/30 rounded-lg">
            <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">{t('settings.advanced.ocr.info')}</p>
          </div>
        </div>
      </div>
      {/* DirectML 推理加速 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Zap className="w-4 h-4" />
          {t('settings.advanced.dml.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.dml.label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.description')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.notice')}</p>
            </div>
            <button
              onClick={() => handleToggle('use_dml')}
              className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${config.use_dml ? 'bg-ide-accent' : 'bg-ide-border'
                }`}
            >
              <div
                className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.use_dml ? 'translate-x-5' : 'translate-x-0.5'
                  }`}
              />
            </button>
          </div>

          {config.use_dml && (
            <div className="flex items-center justify-between gap-4">
              <div className="flex items-center gap-2">
                <Monitor className="w-4 h-4 text-ide-muted" />
                <p className="text-sm text-ide-muted">{t('settings.advanced.dml.gpu_select')}</p>
              </div>
              <div className="relative">
                {gpuLoading ? (
                  <p className="text-xs text-ide-muted px-4 py-2">{t('settings.advanced.dml.gpu_loading')}</p>
                ) : gpus.length === 0 ? (
                  <p className="text-xs text-ide-muted px-4 py-2">{t('settings.advanced.dml.gpu_none')}</p>
                ) : (
                  <>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        setGpuDropdownOpen(!gpuDropdownOpen);
                      }}
                      className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[180px] max-w-[280px]"
                    >
                      <span className="flex-1 text-left truncate">{selectedGpu?.name || `GPU ${config.dml_device_id}`}</span>
                      <ChevronDown
                        className={`w-4 h-4 text-ide-muted transition-transform shrink-0 ${gpuDropdownOpen ? 'rotate-180' : ''}`}
                      />
                    </button>
                    {gpuDropdownOpen && (
                      <div
                        className="absolute right-0 top-full mt-2 w-72 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                        onClick={(e) => e.stopPropagation()}
                      >
                        {gpus.map((gpu) => (
                          <button
                            key={gpu.id}
                            onClick={() => handleGpuChange(gpu.id)}
                            className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between gap-2 ${gpu.id === config.dml_device_id ? 'bg-ide-accent/10' : ''
                              }`}
                          >
                            <span className="text-sm text-ide-text truncate">{gpu.name}</span>
                            {gpu.id === config.dml_device_id && (
                              <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                            )}
                          </button>
                        ))}
                      </div>
                    )}
                  </>
                )}
              </div>
            </div>
          )}

          {dmlChanged && (
            <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
              <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
              <p className="text-xs text-ide-warning-muted flex-1">{t('settings.advanced.dml.changed_notice')}</p>
              {monitorStatus === 'running' && onRestartMonitor && (
                <button
                  onClick={() => { onRestartMonitor(); setDmlChanged(false); }}
                  className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
                >
                  {t('settings.advanced.quick_restart')}
                </button>
              )}
            </div>
          )}
        </div>
      </div>

      {/* 聚类间隔 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Layers className="w-4 h-4" />
          {t('settings.advanced.clustering.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.clustering.interval_label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.clustering.interval_desc')}</p>
            </div>
            <div className="relative">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  setClusteringDropdownOpen(!clusteringDropdownOpen);
                }}
                className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[120px]"
              >
                <span className="flex-1 text-left">{t(`settings.advanced.clustering.intervals.${config.clustering_interval || '1w'}`)}</span>
                <ChevronDown
                  className={`w-4 h-4 text-ide-muted transition-transform ${clusteringDropdownOpen ? 'rotate-180' : ''}`}
                />
              </button>
              {clusteringDropdownOpen && (
                <div
                  className="absolute right-0 top-full mt-2 w-40 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                >
                  {['1d', '1w', '1m', '6m'].map((interval) => (
                    <button
                      key={interval}
                      onClick={async () => {
                        setClusteringDropdownOpen(false);
                        const newConfig = { ...config, clustering_interval: interval };
                        await saveConfig(newConfig);
                        // Also sync to Python backend
                        try {
                          await invoke('execute_monitor_command', {
                            payload: { command: 'set_clustering_interval', interval },
                          });
                        } catch { /* best-effort */ }
                      }}
                      className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${interval === (config.clustering_interval || '1w') ? 'bg-ide-accent/10' : ''
                        }`}
                    >
                      <span className="text-sm text-ide-text">{t(`settings.advanced.clustering.intervals.${interval}`)}</span>
                      {interval === (config.clustering_interval || '1w') && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="flex items-start gap-2 p-2.5 bg-ide-panel/50 border border-ide-border/30 rounded-lg">
            <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">{t('settings.advanced.clustering.info')}</p>
          </div>
        </div>
      </div>

      {/* 数据库维护 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Database className="w-4 h-4" />
          {t('settings.advanced.vacuum.title', '数据库维护')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.vacuum.label', '手动执行数据库优化')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.vacuum.description', '执行 VACUUM 可回收数据库空间并整理存储结构，过程可能持续数秒到数分钟。')}</p>
            </div>
            <button
              onClick={handleManualVacuum}
              disabled={vacuumRunning}
              className="shrink-0 flex items-center gap-2 px-3 py-2 rounded-lg text-xs font-medium transition-colors border border-ide-border bg-ide-panel hover:bg-ide-hover text-ide-text disabled:opacity-60"
            >
              {vacuumRunning && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
              {vacuumRunning
                ? t('settings.advanced.vacuum.running', '优化中...')
                : t('settings.advanced.vacuum.action', '立即优化')}
            </button>
          </div>

          {vacuumMessage && (
            <div className="text-xs text-ide-muted bg-ide-panel/50 border border-ide-border/30 rounded-lg px-3 py-2">
              {vacuumMessage}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
