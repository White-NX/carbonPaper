import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ChevronDown, Cpu, Database, Monitor, RefreshCw, Zap } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';
import { SettingsSwitch } from '../SettingsControls';

function ChangedNotice({
  children,
  monitorStatus,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
    <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
      <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
      <p className="text-xs text-ide-warning-muted flex-1">{children}</p>
      {monitorStatus === 'running' && onRestartMonitor && (
        <button
          onClick={() => { onRestartMonitor(); onClearChanged(); }}
          className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
        >
          {t('settings.advanced.quick_restart')}
        </button>
      )}
    </div>
  );
}

export function OcrEngineCard({
  config,
  status,
  statusLoading,
  modelStatus,
  modelDownloading,
  onToggle,
  onRestart,
  onDownloadModel,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Cpu className="w-4 h-4" />
        {t('settings.advanced.rust_ocr.title', 'OCR 引擎')}
      </label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3">
          <p className="text-sm text-ide-text font-medium">
            {t('settings.advanced.rust_ocr.raw_rgb', 'Rust Raw RGB OCR')}
          </p>
          <p className="text-xs text-ide-muted mt-1">
            {t(
              'settings.advanced.rust_ocr.raw_rgb_desc',
              '隔离的 Rust ML 进程直接读取 RGB 捕获帧。',
            )}
          </p>
        </div>

        <div className="flex items-center justify-between gap-4">
            <div className="flex-1 min-w-0">
              <p className="text-sm text-ide-text font-medium">
                {t('settings.advanced.rust_ocr.dml_beta', 'DirectML Beta')}
              </p>
              <p className="text-xs text-ide-muted mt-1">
                {t('settings.advanced.rust_ocr.dml_beta_desc', '临时实验开关，默认关闭；未来会废弃并合并到统一的 DirectML 设置。')}
              </p>
            </div>
            <SettingsSwitch
              checked={Boolean(config.rust_ocr_dml_beta)}
              onChange={() => onToggle('rust_ocr_dml_beta')}
            />
        </div>

        <div className="flex items-center justify-between gap-4 rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3">
          <div className="min-w-0 text-xs">
            <p className="text-ide-text font-medium">
              {modelStatus?.installed
                ? (['bundled', 'portable'].includes(modelStatus?.source)
                  ? t('settings.advanced.rust_ocr.model_bundled', 'PP-OCRv5 Mobile 随 CarbonPaper 安装')
                  : t('settings.advanced.rust_ocr.model_repaired', 'PP-OCRv5 Mobile 已通过在线修复安装'))
                : t('settings.advanced.rust_ocr.model_damaged', 'PP-OCRv5 Mobile 安装资源缺失或损坏')}
            </p>
            <p className="text-ide-muted mt-1 truncate" title={modelStatus?.path}>
              {modelStatus?.path || t('settings.advanced.rust_ocr.model_checking', '正在检查模型…')}
            </p>
          </div>
          {!modelStatus?.installed && (
            <button
              onClick={onDownloadModel}
              disabled={modelDownloading}
              className="px-3 py-1.5 text-xs rounded bg-ide-accent text-white hover:opacity-90 disabled:opacity-50"
            >
              {modelDownloading
                ? t('settings.advanced.rust_ocr.model_downloading', '下载中…')
                : t('settings.advanced.rust_ocr.model_repair', '在线修复')}
            </button>
          )}
        </div>

        <div className="flex items-center justify-between gap-4 rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3">
            <div className="min-w-0 text-xs">
              <p className="text-ide-text font-medium">
                {statusLoading
                  ? t('settings.advanced.rust_ocr.status_loading', '正在读取状态…')
                  : `${status?.state || 'stopped'} · ${status?.provider || 'none'} · ${status?.model_id || 'ppocrv5-ch-mobile'}`}
              </p>
              {!statusLoading && (
                <p className="text-ide-muted mt-1">
                  {t('settings.advanced.rust_ocr.status_counts', '成功 {{success}} · 失败 {{failure}} · 最近 {{elapsed}} ms', {
                    success: status?.success_count ?? 0,
                    failure: status?.failure_count ?? 0,
                    elapsed: status?.last_elapsed_ms == null ? '-' : Math.round(status.last_elapsed_ms),
                  })}
                </p>
              )}
            </div>
            <button
              onClick={onRestart}
              className="p-2 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors"
              title={t('settings.advanced.rust_ocr.restart', '重启 Rust ML 进程')}
            >
              <RefreshCw className="w-4 h-4" />
            </button>
        </div>
      </div>
    </div>
  );
}

export function DmlAccelerationCard({
  config,
  monitorStatus,
  dmlChanged,
  gpus,
  gpuLoading,
  selectedGpu,
  gpuDropdownOpen,
  onToggle,
  onToggleGpuDropdown,
  onGpuChange,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
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
            onChange={() => onToggle('use_dml')}
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
                      onToggleGpuDropdown();
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
                          onClick={() => onGpuChange(gpu.id)}
                          className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between gap-2 ${gpu.id === config.dml_device_id ? 'bg-ide-accent/10' : ''}`}
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
          <ChangedNotice
            monitorStatus={monitorStatus}
            onRestartMonitor={onRestartMonitor}
            onClearChanged={onClearChanged}
          >
            {t('settings.advanced.dml.changed_notice')}
          </ChangedNotice>
        )}
      </div>
    </div>
  );
}

/**
 * Semantic retrieval backend: the rollback lever and the local diagnostic the
 * enum rule requires. Deliberately small — it reports which backend answered
 * the last natural-language query and why Rust stood down, not a percentile
 * table. The retired shadow card was a development instrument; this is not.
 */
export function SemanticBackendCard({ config, status, statusLoading, onToggleRustIndex, onRefresh }) {
  const { t } = useTranslation();
  const backend = status?.backend;
  const usesRustIndex = (config.semantic_index || 'rust') === 'rust';
  // Captures waiting for an idle window to be encoded. Ordinary operation, not
  // a fault, so it is shown plainly; only a stalled job — one whose retry
  // budget is spent and which nothing will pick up again — gets a warning.
  const backlog = backend?.index_backlog ?? 0;
  const stalled = backend?.index_stalled ?? 0;

  const servedLabel = {
    rust: t('settings.advanced.semantic_backend.served_rust'),
    python: t('settings.advanced.semantic_backend.served_python'),
  }[backend?.last_query_backend] || t('settings.advanced.semantic_backend.served_unknown');

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Database className="w-4 h-4" />
        {t('settings.advanced.semantic_backend.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.semantic_backend.label')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.semantic_backend.description')}
            </p>
          </div>
          <SettingsSwitch checked={usesRustIndex} onChange={() => onToggleRustIndex(!usesRustIndex)} />
        </div>

        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3 space-y-2">
          <div className="flex items-center justify-between">
            <p className="text-xs text-ide-muted">
              {t('settings.advanced.semantic_backend.diagnostic')}
            </p>
            <button
              onClick={onRefresh}
              disabled={statusLoading}
              className="p-1.5 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50"
              title={t('settings.advanced.semantic_backend.refresh')}
            >
              <RefreshCw className={`w-3.5 h-3.5 ${statusLoading ? 'animate-spin' : ''}`} />
            </button>
          </div>

          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
            <span className="text-ide-muted">{t('settings.advanced.semantic_backend.last_query')}</span>
            <span className="text-ide-text text-right">{servedLabel}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_backend.indexed')}</span>
            <span className="text-ide-text text-right">{backend?.indexed_vectors ?? '—'}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_backend.backlog')}</span>
            <span className="text-ide-text text-right">{backend?.index_backlog ?? '—'}</span>
            {stalled > 0 && (
              <>
                <span className="text-ide-muted">{t('settings.advanced.semantic_backend.stalled')}</span>
                <span className="text-ide-warning text-right">{stalled}</span>
              </>
            )}
            <span className="text-ide-muted">{t('settings.advanced.semantic_backend.fallbacks')}</span>
            <span className="text-ide-text text-right">{backend?.fallback_count ?? 0}</span>
          </div>

          {backend?.last_fallback_reason && (
            <p className="text-[11px] text-ide-warning-muted leading-snug">
              {t('settings.advanced.semantic_backend.last_reason')}: {backend.last_fallback_reason}
            </p>
          )}
          {backlog > 0 && (
            <p className="text-[11px] text-ide-muted leading-snug">
              {t('settings.advanced.semantic_backend.backlog_hint')}
            </p>
          )}
          {stalled > 0 && (
            <p className="text-[11px] text-ide-warning-muted leading-snug">
              {t('settings.advanced.semantic_backend.stalled_hint')}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

export function OnnxRuntimeCard({
  config,
  monitorStatus,
  onnxChanged,
  onToggle,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
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
            onChange={() => onToggle('use_onnx')}
          />
        </div>

        {onnxChanged && (
          <ChangedNotice
            monitorStatus={monitorStatus}
            onRestartMonitor={onRestartMonitor}
            onClearChanged={onClearChanged}
          >
            {t('settings.advanced.onnx.changed_notice')}
          </ChangedNotice>
        )}
      </div>
    </div>
  );
}
