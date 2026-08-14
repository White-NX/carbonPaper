import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ChevronDown, Cpu, Database, Image as ImageIcon, Monitor, RefreshCw, Tags, Zap } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';
import { SettingsSwitch } from '../SettingsControls';
import { formatEstimate } from '../../ClipBackfillDialog';

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
 * Retrieval of screenshot text: the rollback lever and the local diagnostic the
 * enum rule requires. Deliberately small — it reports which backend answered
 * the last query and why Rust stood down, not a percentile table. The retired
 * shadow card was a development instrument; this is not.
 *
 * The scope this card covers is narrower than "semantic search", and the label
 * says so now. It is the MiniLM path over the text of a screenshot, which
 * serves the natural-language grouping view and Smart Cluster calibration. The
 * main search box's natural-language mode is Chinese-CLIP over images and is
 * untouched by anything here.
 *
 * The one action it carries is the manual indexing run, which exists because
 * capture-side indexing waits for an idle window on mains power. That is the
 * right default and a poor fit for a machine that is rarely either, so the
 * roadmap's "idle *or* an explicit manual run" needs its second half reachable
 * next to the backlog number that motivates pressing it.
 */
export function SemanticBackendCard({
  config,
  status,
  statusLoading,
  onToggleRustIndex,
  onRefresh,
  onRunIndexNow,
  onStopIndexNow,
  indexRunning,
  indexStopping,
  indexProgress,
  indexRun,
}) {
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

  // A finished run says what it did. `started: false` means a guard refused
  // before any encoding happened, which is a different message from "ran and
  // indexed nothing". While it is still going, the per-chunk progress event
  // takes the same line: the run drains the whole queue now, so on a deep
  // backlog it can last minutes and a silent line would read as a hang.
  const progressTotal = indexProgress?.total ?? 0;
  const progressProcessed = indexProgress?.processed ?? 0;
  const progressRatio = progressTotal > 0
    ? Math.min(1, progressProcessed / progressTotal)
    : null;

  let runMessage = null;
  if (indexRunning) {
    if (indexStopping) {
      runMessage = t('settings.advanced.semantic_backend.run_stopping');
    } else if (indexProgress) {
      runMessage = t('settings.advanced.semantic_backend.run_progress', {
        processed: progressProcessed,
        total: progressTotal,
        indexed: indexProgress.indexed ?? 0,
      });
    } else {
      // Nothing has been encoded yet: the pass is reading the ledger and
      // loading a 118 MB model, which is the longest silent stretch of a run.
      runMessage = t('settings.advanced.semantic_backend.run_preparing');
    }
  } else if (indexRun) {
    runMessage = indexRun.started
      ? t('settings.advanced.semantic_backend.run_done', {
        indexed: indexRun.indexed ?? 0,
        remaining: indexRun.remaining ?? 0,
      })
      : t('settings.advanced.semantic_backend.run_skipped', {
        reason: indexRun.skipped_reason || 'unknown',
      });
  }

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
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.semantic_backend.rollback_note')}
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

          <div className="pt-1 flex items-start justify-between gap-3">
            <div className="flex-1 min-w-0 space-y-1.5">
              <p className="text-[11px] text-ide-muted leading-snug">
                {runMessage || t('settings.advanced.semantic_backend.run_now_hint')}
              </p>
              {indexRunning && progressRatio !== null && (
                <div className="w-full bg-ide-panel border border-ide-border rounded-full h-1.5 overflow-hidden">
                  <div
                    className="bg-ide-accent h-full transition-all duration-300 ease-out"
                    style={{ width: `${Math.max(2, progressRatio * 100)}%` }}
                  />
                </div>
              )}
            </div>
            {/* Not gated on the switch above. Capture indexing runs whichever
                backend serves queries, so disabling the only control over it
                while it keeps running would hide the work rather than stop it. */}
            {indexRunning ? (
              <button
                onClick={onStopIndexNow}
                disabled={indexStopping}
                className="shrink-0 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {indexStopping
                  ? t('settings.advanced.semantic_backend.run_stop_pending')
                  : t('settings.advanced.semantic_backend.run_stop')}
              </button>
            ) : (
              <button
                onClick={onRunIndexNow}
                className="shrink-0 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors"
              >
                {t('settings.advanced.semantic_backend.run_now')}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * The M2.5 step-9 counterpart of {@link SemanticBackendCard}, for visual search.
 *
 * A separate card rather than a second row inside that one, because the two
 * govern different searches and roll back independently: this switch is the
 * main search box's natural-language mode — a text query matched against what a
 * screenshot *looks like* — while the other is the grouping view's search over
 * recognised text. A user who could not tell which was which would have no way
 * to act on either.
 */
export function ClipBackendCard({
  config,
  status,
  statusLoading,
  onToggleRustIndex,
  onRefresh,
  onRunIndexNow,
  onStopIndexNow,
  indexRunning,
  indexStopping,
  indexProgress,
  indexRun,
  backfill,
  backfillBusy,
  onBackfillDecision,
  onRetryAnn,
  annRetrying,
}) {
  const { t } = useTranslation();
  const backend = status?.clip_backend;
  const usesRustIndex = (config.clip_index || 'rust') === 'rust';
  const backlog = backend?.index_backlog ?? 0;
  const stalled = backend?.index_stalled ?? 0;
  const annBuildUnhealthy = backend?.ann_build_state && backend.ann_build_state !== 'healthy';
  const annRetryAt = backend?.ann_build_next_retry_at
    ? new Date(backend.ann_build_next_retry_at).toLocaleString()
    : null;
  const annStateLabel = {
    armed: t('settings.advanced.clip_backend.ann_ready'),
    arming: t('settings.advanced.clip_backend.ann_building'),
    exact_fallback: t('settings.advanced.clip_backend.ann_exact'),
    failed: t('settings.advanced.clip_backend.ann_failed'),
    disabled: t('settings.advanced.clip_backend.ann_disabled'),
    unavailable: t('settings.advanced.clip_backend.ann_unavailable'),
  }[backend?.ann_state] || backend?.ann_state || '—';

  const servedLabel = {
    rust: t('settings.advanced.clip_backend.served_rust'),
    python: t('settings.advanced.clip_backend.served_python'),
  }[backend?.last_query_backend] || t('settings.advanced.clip_backend.served_unknown');

  const progressTotal = indexProgress?.total ?? 0;
  const progressProcessed = indexProgress?.processed ?? 0;
  const progressRatio = progressTotal > 0
    ? Math.min(1, progressProcessed / progressTotal)
    : null;

  let runMessage = null;
  if (indexRunning) {
    if (indexStopping) {
      runMessage = t('settings.advanced.clip_backend.run_stopping');
    } else if (indexProgress) {
      runMessage = t('settings.advanced.clip_backend.run_progress', {
        processed: progressProcessed,
        total: progressTotal,
        indexed: indexProgress.indexed ?? 0,
      });
    } else {
      // Nothing encoded yet: the pass is reading the ledger and loading a
      // 177 MB model, which is the longest silent stretch of a run.
      runMessage = t('settings.advanced.clip_backend.run_preparing');
    }
  } else if (indexRun) {
    runMessage = indexRun.started
      ? t('settings.advanced.clip_backend.run_done', {
        indexed: indexRun.indexed ?? 0,
        remaining: indexRun.remaining ?? 0,
      })
      : t('settings.advanced.clip_backend.run_skipped', {
        reason: indexRun.skipped_reason || 'unknown',
      });
  }

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <ImageIcon className="w-4 h-4" />
        {t('settings.advanced.clip_backend.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.clip_backend.label')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.clip_backend.description')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.clip_backend.rollback_note')}
            </p>
          </div>
          <SettingsSwitch checked={usesRustIndex} onChange={() => onToggleRustIndex(!usesRustIndex)} />
        </div>

        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3 space-y-2">
          <div className="flex items-center justify-between">
            <p className="text-xs text-ide-muted">
              {t('settings.advanced.clip_backend.diagnostic')}
            </p>
            <button
              onClick={onRefresh}
              disabled={statusLoading}
              className="p-1.5 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50"
              title={t('settings.advanced.clip_backend.refresh')}
            >
              <RefreshCw className={`w-3.5 h-3.5 ${statusLoading ? 'animate-spin' : ''}`} />
            </button>
          </div>

          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
            <span className="text-ide-muted">{t('settings.advanced.clip_backend.last_query')}</span>
            <span className="text-ide-text text-right">{servedLabel}</span>
            <span className="text-ide-muted">{t('settings.advanced.clip_backend.indexed')}</span>
            <span className="text-ide-text text-right">{backend?.indexed_vectors ?? '—'}</span>
            <span className="text-ide-muted">{t('settings.advanced.clip_backend.backlog')}</span>
            <span className="text-ide-text text-right">{backend?.index_backlog ?? '—'}</span>
            {stalled > 0 && (
              <>
                <span className="text-ide-muted">{t('settings.advanced.clip_backend.stalled')}</span>
                <span className="text-ide-warning text-right">{stalled}</span>
              </>
            )}
            <span className="text-ide-muted">{t('settings.advanced.clip_backend.fallbacks')}</span>
            <span className="text-ide-text text-right">{backend?.fallback_count ?? 0}</span>
            <span className="text-ide-muted">{t('settings.advanced.clip_backend.ann_status')}</span>
            <span className="text-ide-text text-right">{annStateLabel}</span>
            {backend?.ann_generation != null && (
              <>
                <span className="text-ide-muted">{t('settings.advanced.clip_backend.ann_generation')}</span>
                <span className="text-ide-text text-right">{backend.ann_generation}</span>
              </>
            )}
          </div>

          {annBuildUnhealthy && (
            <div className="rounded-lg border border-ide-warning-border bg-ide-warning-bg p-3 space-y-2">
              <div className="flex items-start gap-2">
                <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0 mt-0.5" />
                <div className="min-w-0 flex-1">
                  <p className="text-xs font-medium text-ide-warning">
                    {backend.ann_build_state === 'circuit_open'
                      ? t('settings.advanced.clip_backend.ann_circuit_open')
                      : t('settings.advanced.clip_backend.ann_backoff')}
                  </p>
                  <p className="text-[11px] text-ide-warning-muted mt-1 leading-snug">
                    {t('settings.advanced.clip_backend.ann_search_still_available')}
                  </p>
                  <p className="text-[11px] text-ide-warning-muted mt-1 break-words">
                    {t('settings.advanced.clip_backend.ann_failure_detail', {
                      count: backend.ann_build_failure_count ?? 0,
                      code: backend.ann_build_error_code || 'unknown',
                      retryAt: annRetryAt || '—',
                    })}
                  </p>
                  {backend.ann_last_error && (
                    <p className="text-[11px] text-ide-warning-muted/80 mt-1 break-words">
                      {backend.ann_last_error}
                    </p>
                  )}
                </div>
                <button
                  onClick={onRetryAnn}
                  disabled={annRetrying}
                  className="shrink-0 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors disabled:opacity-50"
                >
                  {annRetrying
                    ? t('settings.advanced.clip_backend.ann_retrying')
                    : t('settings.advanced.clip_backend.ann_retry_now')}
                </button>
              </div>
            </div>
          )}

          {backend?.last_fallback_reason && (
            <p className="text-[11px] text-ide-warning-muted leading-snug">
              {t('settings.advanced.clip_backend.last_reason')}: {backend.last_fallback_reason}
            </p>
          )}
          {backlog > 0 && (
            <p className="text-[11px] text-ide-muted leading-snug">
              {t('settings.advanced.clip_backend.backlog_hint')}
            </p>
          )}
          {stalled > 0 && (
            <p className="text-[11px] text-ide-warning-muted leading-snug">
              {t('settings.advanced.clip_backend.stalled_hint')}
            </p>
          )}

          {/* The backfill decision, kept reversible. The dialog asks once when
              the migration settles; without a control here, "not now" would be
              permanent in practice, which is not what the user was told when
              they chose it. */}
          {backfill?.migration_settled && (backfill.never_indexed > 0 || backfill.decision) && (
            <div className="pt-1 flex items-start justify-between gap-3 border-t border-ide-border/40">
              <p className="text-[11px] text-ide-muted leading-snug flex-1 min-w-0 pt-1">
                {backfill.decision === 'approved'
                  ? t('clipBackfill.cardApproved')
                  : backfill.decision === 'declined'
                    ? t('clipBackfill.cardDeclined')
                    : t('clipBackfill.cardPending', {
                      count: backfill.never_indexed,
                      estimate: formatEstimate(t, backfill.estimated_seconds) ?? '—',
                    })}
              </p>
              <button
                onClick={() =>
                  onBackfillDecision(backfill.decision === 'approved' ? 'declined' : 'approved')
                }
                disabled={backfillBusy}
                className="shrink-0 mt-0.5 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors disabled:opacity-50"
              >
                {backfill.decision === 'approved'
                  ? t('clipBackfill.cardDecline')
                  : t('clipBackfill.cardApprove')}
              </button>
            </div>
          )}

          <div className="pt-1 flex items-start justify-between gap-3">
            <div className="flex-1 min-w-0 space-y-1.5">
              <p className="text-[11px] text-ide-muted leading-snug">
                {runMessage || t('settings.advanced.clip_backend.run_now_hint')}
              </p>
              {indexRunning && progressRatio !== null && (
                <div className="w-full bg-ide-panel border border-ide-border rounded-full h-1.5 overflow-hidden">
                  <div
                    className="bg-ide-accent h-full transition-all duration-300 ease-out"
                    style={{ width: `${Math.max(2, progressRatio * 100)}%` }}
                  />
                </div>
              )}
            </div>
            {indexRunning ? (
              <button
                onClick={onStopIndexNow}
                disabled={indexStopping}
                className="shrink-0 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {indexStopping
                  ? t('settings.advanced.clip_backend.run_stop_pending')
                  : t('settings.advanced.clip_backend.run_stop')}
              </button>
            ) : (
              <button
                onClick={onRunIndexNow}
                className="shrink-0 px-2.5 py-1.5 text-xs text-ide-text bg-ide-panel border border-ide-border rounded-lg hover:bg-ide-hover transition-colors"
              >
                {t('settings.advanced.clip_backend.run_now')}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export function ClassificationBackendCard({
  config,
  status,
  monitorStatus,
  runtimeChanged,
  onToggleRust,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();
  const usesRust = (config.classification_runtime || 'rust') === 'rust';
  const backend = status?.classification_backend;
  const servedLabel = {
    rust: t('settings.advanced.classification_backend.served_rust'),
    python: t('settings.advanced.classification_backend.served_python'),
  }[backend?.last_backend] || t('settings.advanced.classification_backend.served_unknown');
  const activeLabel = {
    rust: t('settings.advanced.classification_backend.runtime_rust'),
    python: t('settings.advanced.classification_backend.runtime_python'),
  }[backend?.active_runtime] || t('settings.advanced.classification_backend.runtime_stopped');

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Tags className="w-4 h-4" />
        {t('settings.advanced.classification_backend.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.classification_backend.label')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.classification_backend.description')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.classification_backend.rollback_note')}
            </p>
          </div>
          <SettingsSwitch checked={usesRust} onChange={() => onToggleRust(!usesRust)} />
        </div>

        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3 space-y-2">
          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
            <span className="text-ide-muted">
              {t('settings.advanced.classification_backend.selected')}
            </span>
            <span className="text-ide-text text-right">
              {usesRust
                ? t('settings.advanced.classification_backend.runtime_rust')
                : t('settings.advanced.classification_backend.runtime_python')}
            </span>
            <span className="text-ide-muted">
              {t('settings.advanced.classification_backend.last_inference')}
            </span>
            <span className="text-ide-text text-right">{servedLabel}</span>
            <span className="text-ide-muted">
              {t('settings.advanced.classification_backend.active')}
            </span>
            <span className="text-ide-text text-right">{activeLabel}</span>
            <span className="text-ide-muted">
              {t('settings.advanced.classification_backend.successes')}
            </span>
            <span className="text-ide-text text-right">{backend?.success_count ?? 0}</span>
            <span className="text-ide-muted">
              {t('settings.advanced.classification_backend.fallbacks')}
            </span>
            <span className="text-ide-text text-right">{backend?.fallback_count ?? 0}</span>
          </div>
          {backend?.last_error && (
            <p className="text-[11px] text-ide-warning-muted leading-snug break-words">
              {t('settings.advanced.classification_backend.last_reason')}: {backend.last_error}
            </p>
          )}
        </div>

        {runtimeChanged && (
          <ChangedNotice
            monitorStatus={monitorStatus}
            onRestartMonitor={onRestartMonitor}
            onClearChanged={onClearChanged}
          >
            {t('settings.advanced.classification_backend.changed_notice')}
          </ChangedNotice>
        )}
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
