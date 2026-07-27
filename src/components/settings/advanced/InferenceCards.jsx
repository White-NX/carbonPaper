import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ChevronDown, Cpu, FlaskConical, Monitor, RefreshCw, Zap } from 'lucide-react';
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

function formatPercent(value) {
  if (value == null) return '—';
  return `${(value * 100).toFixed(1)}%`;
}

function formatMs(value) {
  if (value == null) return '—';
  return `${Math.round(value)} ms`;
}

function formatErr(value) {
  if (value == null) return '—';
  return value.toExponential(1);
}

function formatCos(value) {
  if (value == null) return '—';
  return value.toFixed(4);
}

export function SemanticShadowCard({
  config,
  report,
  reportLoading,
  onToggleShadow,
  onRefresh,
  onRunProbe,
  probeRunning,
  probeSummary,
  onRunDocProbe,
  docProbeRunning,
}) {
  const { t } = useTranslation();
  const runtime = config.semantic_runtime || 'python';
  const shadowOn = runtime === 'rust_shadow';
  const doc = report?.doc_encoder;

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <FlaskConical className="w-4 h-4" />
        {t('settings.advanced.semantic_shadow.title', 'MiniLM 语义检索影子对比')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.semantic_shadow.label', '实时影子对比')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t(
                'settings.advanced.semantic_shadow.desc',
                '开启后，聚类页的每次自然语言检索都会在后台额外跑一次 Rust 精确扫描检索，与权威的 Chroma/Python 结果对比并记录本地诊断。不改变搜索结果，Python 保持权威。',
              )}
            </p>
          </div>
          <SettingsSwitch checked={shadowOn} onChange={() => onToggleShadow(!shadowOn)} />
        </div>

        <div className="flex items-center justify-between gap-4 rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3">
          <div className="min-w-0 flex-1">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.semantic_shadow.probe', '运行内置查询集')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t(
                'settings.advanced.semantic_shadow.probe_desc',
                '用一组内置查询自动驱动对比，无需手动搜索。需要监控进程在运行且 MiniLM 索引已就绪。',
              )}
            </p>
            {probeSummary && (
              <p className="text-xs text-ide-muted mt-1">
                {t(
                  'settings.advanced.semantic_shadow.probe_summary',
                  '本次：{{queries}} 条查询 · 有效 {{full}} · 提示 {{note}} · Python 失败 {{fail}}',
                  {
                    queries: probeSummary.queries ?? 0,
                    full: probeSummary.full_samples ?? 0,
                    note: probeSummary.note_samples ?? 0,
                    fail: probeSummary.python_failures ?? 0,
                  },
                )}
              </p>
            )}
          </div>
          <button
            onClick={onRunProbe}
            disabled={probeRunning}
            className="px-3 py-1.5 text-xs rounded bg-ide-accent text-white hover:opacity-90 disabled:opacity-50 shrink-0"
          >
            {probeRunning
              ? t('settings.advanced.semantic_shadow.probe_running', '运行中…')
              : t('settings.advanced.semantic_shadow.probe_run', '开始测试')}
          </button>
        </div>

        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3 space-y-2">
          <div className="flex items-center justify-between">
            <p className="text-xs text-ide-muted">
              {t('settings.advanced.semantic_shadow.samples', '样本数')}:{' '}
              <span className="text-ide-text font-medium">{report?.sample_count ?? 0}</span>
              <span className="text-ide-muted">
                {' '}({t('settings.advanced.semantic_shadow.steady', '稳态')}{' '}
                {report?.steady_sample_count ?? 0})
              </span>
              {report?.note_sample_count ? (
                <span className="text-ide-warning">
                  {' '}·{' '}
                  {t('settings.advanced.semantic_shadow.excluded', '已排除')}{' '}
                  {report.note_sample_count}
                </span>
              ) : null}
            </p>
            <button
              onClick={onRefresh}
              disabled={reportLoading}
              className="p-1.5 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50"
              title={t('settings.advanced.semantic_shadow.refresh', '刷新诊断')}
            >
              <RefreshCw className={`w-3.5 h-3.5 ${reportLoading ? 'animate-spin' : ''}`} />
            </button>
          </div>

          <div className="rounded border border-ide-border/60 bg-ide-panel/40 px-2.5 py-1.5 text-[11px] text-ide-warning-muted leading-snug">
            {t(
              'settings.advanced.semantic_shadow.scope',
              '作用域：仅 bi-encoder 检索层，reranker 已关闭 —— 这些数字不代表线上最终排序（带 rerank 的端到端对照属 M2.5 step 5）。',
            )}
          </div>

          <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.top1', 'Top-1 一致率')}</span>
            <span className="text-ide-text text-right">{formatPercent(report?.top1_agreement_rate)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.overlap50', 'Overlap@10 p50')}</span>
            <span className="text-ide-text text-right">{formatPercent(report?.overlap_p50)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.overlap05', 'Overlap@10 p05')}</span>
            <span className="text-ide-text text-right">{formatPercent(report?.overlap_p05)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.max_err', '查询编码 余弦最大误差')}</span>
            <span className="text-ide-text text-right">{formatErr(report?.max_abs_err)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.rust_ms', 'Rust 稳态 p50/p95')}</span>
            <span className="text-ide-text text-right">{formatMs(report?.rust_ms_p50)} / {formatMs(report?.rust_ms_p95)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.cold_ms', 'Rust 冷启动(首查)')}</span>
            <span className="text-ide-text text-right">{formatMs(report?.rust_cold_start_ms)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.embed_scan', '编码/扫描 p50')}</span>
            <span className="text-ide-text text-right">{formatMs(report?.rust_embed_ms_p50)} / {formatMs(report?.rust_scan_ms_p50)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.python_ms', 'Python p50/p95')}</span>
            <span className="text-ide-text text-right">{formatMs(report?.python_ms_p50)} / {formatMs(report?.python_ms_p95)}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.coverage', '覆盖 Rust/Chroma')}</span>
            <span className="text-ide-text text-right">{report?.rust_visible_latest ?? '—'} / {report?.chroma_scope_latest ?? '—'}</span>
            <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.divergence', '分歧 仅C/名次/仅R')}</span>
            <span className="text-ide-text text-right">
              <span className={report?.only_in_chroma_total ? 'text-ide-warning' : ''}>{report?.only_in_chroma_total ?? 0}</span>
              {' / '}{report?.in_both_diff_rank_total ?? 0}{' / '}{report?.only_in_rust_total ?? 0}
            </span>
          </div>
          <p className="text-[11px] text-ide-muted leading-snug">
            {t(
              'settings.advanced.semantic_shadow.divergence_hint',
              '分歧分解：仅C=Rust 缺失 Chroma 的文档（真分歧，需关注）；名次=两库都有但排名不同（近似/排序差异）；仅R=Rust 独有。扫描为 O(N) 精确扫描，规模上限为“覆盖 Rust”那一层。',
            )}
          </p>

          <div className="border-t border-ide-border/60 pt-2 space-y-2">
            <div className="flex items-center justify-between gap-3">
              <div className="min-w-0">
                <p className="text-xs text-ide-text font-medium">
                  {t('settings.advanced.semantic_shadow.doc_title', '文档编码对照')}
                </p>
                <p className="text-[11px] text-ide-muted mt-0.5 leading-snug">
                  {t(
                    'settings.advanced.semantic_shadow.doc_desc',
                    'Rust 重新编码迁移文档，与存储的 Python 文档向量比余弦——补上查询对比未覆盖的另一半链路（Rust 文档编码器）。',
                  )}
                </p>
              </div>
              <button
                onClick={onRunDocProbe}
                disabled={docProbeRunning}
                className="px-3 py-1.5 text-xs rounded bg-ide-accent text-white hover:opacity-90 disabled:opacity-50 shrink-0"
              >
                {docProbeRunning
                  ? t('settings.advanced.semantic_shadow.probe_running', '运行中…')
                  : t('settings.advanced.semantic_shadow.doc_run', '编码对照')}
              </button>
            </div>
            {doc ? (
              <>
                <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                  <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.doc_samples', '文档样本 对照/抽样')}</span>
                  <span className="text-ide-text text-right">{doc.doc_sample_count ?? 0} / {doc.requested ?? 0}</span>
                  <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.doc_cos', '余弦 p50/p05/最小')}</span>
                  <span className="text-ide-text text-right">{formatCos(doc.cos_p50)} / {formatCos(doc.cos_p05)} / {formatCos(doc.cos_min)}</span>
                  <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.doc_max_err', '文档编码 余弦最大误差')}</span>
                  <span className="text-ide-text text-right">{formatErr(doc.max_abs_err)}</span>
                  <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.doc_latency', '冷启动/稳态每文档')}</span>
                  <span className="text-ide-text text-right">{formatMs(doc.cold_start_ms)} / {formatMs(doc.steady_ms_per_doc)}</span>
                  <span className="text-ide-muted">{t('settings.advanced.semantic_shadow.doc_skipped', '跳过 源变更/缺失')}</span>
                  <span className="text-ide-text text-right">
                    <span className={doc.source_changed ? 'text-ide-warning' : ''}>{doc.source_changed ?? 0}</span>
                    {' / '}{doc.missing_text ?? 0}
                  </span>
                </div>
                <p className="text-[11px] text-ide-muted leading-snug">
                  {t(
                    'settings.advanced.semantic_shadow.doc_hint',
                    '余弦越接近 1 越好；最大误差=1−最小余弦。源变更=OCR/标题自迁移后已改（跳过，避免对比不同文本）；缺失=截图已删或文本为空。',
                  )}
                </p>
              </>
            ) : (
              <p className="text-[11px] text-ide-muted leading-snug">
                {t('settings.advanced.semantic_shadow.doc_empty', '尚无文档编码对照数据，点击“编码对照”运行一次。')}
              </p>
            )}
          </div>

          {report?.last_note && (
            <p className="text-xs text-ide-warning-muted mt-1">
              {t('settings.advanced.semantic_shadow.last_note', '最近提示')}: {report.last_note}
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
