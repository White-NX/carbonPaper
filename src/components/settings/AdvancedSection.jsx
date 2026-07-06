import React from 'react';
import { useTranslation } from 'react-i18next';
import { Cpu, ListOrdered, ChevronDown, AlertTriangle, Info, Zap, Monitor, Layers, Database, Loader2, Globe, Clock } from 'lucide-react';
import SettingsHelpTooltip from './SettingsHelpTooltip';
import { SettingsSwitch } from './SettingsControls';
import { useAdvancedSectionController } from './useAdvancedSectionController';

const CPU_PERCENT_OPTIONS = [5, 10, 15, 20, 30, 50];
const OCR_QUEUE_SIZE_OPTIONS = [1, 2, 3, 5, 10];

export default function AdvancedSection({ monitorStatus, onRestartMonitor }) {
  const { t } = useTranslation();
  const {
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
    clearCpuChanged,
    clearDmlChanged,
    clearOnnxChanged,
    handleToggle,
    handleCpuPercentChange,
    handleQueueSizeChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
  } = useAdvancedSectionController({ monitorStatus, t });

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        {t('settings.advanced.loading')}
      </div>
    );
  }

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
            <SettingsSwitch
              checked={config.cpu_limit_enabled}
              onChange={() => handleToggle('cpu_limit_enabled')}
            />
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
                  onClick={() => { onRestartMonitor(); clearCpuChanged(); }}
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
            <SettingsSwitch
              checked={!config.capture_on_ocr_busy}
              onChange={() => handleToggle('capture_on_ocr_busy')}
            />
          </div>

          {/* 队列大小限制 */}
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.ocr.queue_limit_label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.ocr.queue_limit_desc')}</p>
            </div>
            <SettingsSwitch
              checked={config.ocr_queue_limit_enabled}
              onChange={() => handleToggle('ocr_queue_limit_enabled')}
            />
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

          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium flex items-center gap-2">
                <Clock className="w-4 h-4 text-ide-muted" />
                {t('settings.advanced.ocr.timeout_label', 'OCR 超时时间')}
              </p>
              <p className="text-xs text-ide-muted mt-1">
                {t('settings.advanced.ocr.timeout_desc', '设定 OCR 任务的超时时间。冷启动固定允许 180 秒。')}
              </p>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              <input
                type="number"
                min="30"
                max="600"
                step="10"
                value={config.ocr_timeout_secs || 120}
                onChange={(e) => handleOcrTimeoutDraftChange(e.target.value)}
                onBlur={(e) => handleOcrTimeoutChange(e.target.value)}
                className="w-24 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text text-right"
              />
              <span className="text-xs text-ide-muted">{t('settings.advanced.ocr.seconds', '秒')}</span>
            </div>
          </div>

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
              <p className="text-sm text-ide-text font-medium">
                {t('settings.advanced.dml.label')}
                <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.directml')}</SettingsHelpTooltip>
              </p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.description')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.notice')}</p>
            </div>
            <SettingsSwitch
              checked={config.use_dml}
              onChange={() => handleToggle('use_dml')}
            />
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
                  onClick={() => { onRestartMonitor(); clearDmlChanged(); }}
                  className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
                >
                  {t('settings.advanced.quick_restart')}
                </button>
              )}
            </div>
          )}
        </div>
      </div>

      {/* ONNX 推理 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Zap className="w-4 h-4" />
          {t('settings.advanced.onnx.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">
                {t('settings.advanced.onnx.label')}
                <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.onnx')}</SettingsHelpTooltip>
              </p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.onnx.description')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.onnx.notice')}</p>
            </div>
            <SettingsSwitch
              checked={config.use_onnx}
              onChange={() => handleToggle('use_onnx')}
            />
          </div>

          {onnxChanged && (
            <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
              <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
              <p className="text-xs text-ide-warning-muted flex-1">{t('settings.advanced.onnx.changed_notice')}</p>
              {monitorStatus === 'running' && onRestartMonitor && (
                <button
                  onClick={() => { onRestartMonitor(); clearOnnxChanged(); }}
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
                      onClick={() => handleClusteringIntervalChange(interval)}
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

          <div className="flex items-center justify-between gap-4 border-t border-ide-border/50 pt-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">
                {t('settings.advanced.clustering.allow_full_low_memory_label')}
                <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.pacmap_hdbscan')}</SettingsHelpTooltip>
              </p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.clustering.allow_full_low_memory_desc')}</p>
            </div>
            <SettingsSwitch
              checked={config.clustering_allow_full_low_memory}
              onChange={() => handleToggle('clustering_allow_full_low_memory')}
            />
          </div>

          <div className="flex items-start gap-2 p-2.5 bg-ide-panel/50 border border-ide-border/30 rounded-lg">
            <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted leading-relaxed">
              {t('settings.advanced.clustering.info')}
              <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.minilm')}</SettingsHelpTooltip>
            </p>
          </div>
        </div>
      </div>

      {/* 网络控制 */}
      <div className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Globe className="w-4 h-4" />
          {t('settings.advanced.network.title')}
        </label>

        <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">{t('settings.advanced.network.label')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.network.description')}</p>
            </div>
            <SettingsSwitch
              checked={config.network_enabled}
              onChange={() => handleToggle('network_enabled')}
            />
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
