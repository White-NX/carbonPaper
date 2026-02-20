import React, { useState, useEffect, useRef } from 'react';
import { Cpu, ListOrdered, ChevronDown, AlertTriangle, Info, Zap } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

const CPU_PERCENT_OPTIONS = [5, 10, 15, 20, 30, 50];
const OCR_QUEUE_SIZE_OPTIONS = [1, 2, 3, 5, 10];

export default function AdvancedSection({ monitorStatus }) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [cpuDropdownOpen, setCpuDropdownOpen] = useState(false);
  const [queueDropdownOpen, setQueueDropdownOpen] = useState(false);
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);

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

  // Close dropdowns on outside click
  useEffect(() => {
    const handler = () => {
      setCpuDropdownOpen(false);
      setQueueDropdownOpen(false);
    };
    if (cpuDropdownOpen || queueDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
  }, [cpuDropdownOpen, queueDropdownOpen]);

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        加载中...
      </div>
    );
  }

  return (
    <div className="space-y-6">

      <div className="flex items-center gap-2 p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-lg">
        <AlertTriangle className="w-4 h-4 text-amber-400 shrink-0" />
        <p className="text-xs text-amber-300/90">
          这些设置可能会影响监控服务的性能和稳定性，甚至影响操作系统整体性能。请谨慎调整。
        </p>
      </div>

      {/* CPU 限制 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Cpu className="w-4 h-4" />
          Python 子进程 CPU 限制
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">限制 CPU 占用率</p>
              <p className="text-xs text-ide-muted mt-1">
                限制 OCR 和向量化处理使用的 CPU 资源，避免影响系统性能
              </p>
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
              <p className="text-sm text-ide-muted">CPU 限制百分比</p>
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
            <div className="flex items-center gap-2 p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-lg">
              <AlertTriangle className="w-4 h-4 text-amber-400 shrink-0" />
              <p className="text-xs text-amber-300/90">
                CPU 限制更改将在下次启动监控服务时生效
              </p>
            </div>
          )}
        </div>
      </div>

      {/* OCR 队列设置 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <ListOrdered className="w-4 h-4" />
          OCR 处理队列
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          {/* 截图暂停开关 */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">OCR 队列有任务时暂停截图</p>
              <p className="text-xs text-ide-muted mt-1">
                开启后，当 OCR 队列中有未处理任务时将暂停截图，减少资源占用
              </p>
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
              <p className="text-sm text-ide-text font-medium">限制 OCR 任务队列大小</p>
              <p className="text-xs text-ide-muted mt-1">
                超过此大小时，暂停所有截图直到队列消化
              </p>
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
              <p className="text-sm text-ide-muted">最大队列大小</p>
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
            <p className="text-xs text-ide-muted leading-relaxed">
              OCR 队列设置会即时生效，无需重启监控服务
            </p>
          </div>
        </div>
      </div>
      {/* DirectML 推理加速 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Zap className="w-4 h-4" />
          OCR 推理加速
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">使用 DirectML 进行推理</p>
              <p className="text-xs text-ide-muted mt-1">
                启用后将使用 GPU 进行 OCR 推理加速，需要 DirectX 12 兼容的显卡
              </p>
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

          {dmlChanged && (
            <div className="flex items-center gap-2 p-2.5 bg-amber-500/10 border border-amber-500/20 rounded-lg">
              <AlertTriangle className="w-4 h-4 text-amber-400 shrink-0" />
              <p className="text-xs text-amber-300/90">
                DirectML 设置将在下次启动监控服务时生效
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
