import React from 'react';
import { useTranslation } from 'react-i18next';
import { Layers, Database, ChevronDown, RefreshCw, ExternalLink, Sparkles, Download, Zap, RotateCcw, Loader2, AlertTriangle, Play, X } from 'lucide-react';
import { SettingsButton, SettingsSegmentedControl, SettingsSwitch } from './SettingsControls';
import { useFeaturesController } from './useFeaturesController';

const FEATURE_MODE_OPTIONS = [
  {
    value: 'minimal',
    config: {
      classification_enabled: false,
      clustering_enabled: false,
      smart_cluster_enabled: false,
    },
  },
  {
    value: 'basic',
    config: {
      classification_enabled: true,
      clustering_enabled: false,
      smart_cluster_enabled: false,
    },
  },
  {
    value: 'organized',
    config: {
      classification_enabled: true,
      clustering_enabled: true,
      smart_cluster_enabled: false,
    },
  },
  {
    value: 'smart',
    config: {
      classification_enabled: true,
      clustering_enabled: true,
      smart_cluster_enabled: true,
    },
  },
];

function getFeatureMode(config) {
  const match = FEATURE_MODE_OPTIONS.find((option) => (
    Boolean(config.classification_enabled) === option.config.classification_enabled
    && Boolean(config.clustering_enabled) === option.config.clustering_enabled
    && Boolean(config.smart_cluster_enabled) === option.config.smart_cluster_enabled
  ));
  return match?.value || 'custom';
}

export default function FeaturesSection({ monitorStatus }) {
  const { t } = useTranslation();
  const {
    config,
    loading,
    models,
    modelsLoading,
    clusteringDropdownOpen,
    setClusteringDropdownOpen,
    clusteringAdvancedOpen,
    setClusteringAdvancedOpen,
    clusteringRunning,
    clusteringError,
    clusteringNotice,
    rangeStart,
    setRangeStart,
    rangeEnd,
    setRangeEnd,
    customControlsOpen,
    setCustomControlsOpen,
    scModelAvailable,
    scStatus,
    scDownloading,
    scDownloadLog,
    scDownloadError,
    handleOpenLocation,
    handleFeatureModeChange,
    handleCustomFeatureToggle,
    handleClusteringIntervalChange,
    handleRunClustering,
    handleDownloadReranker,
    handleDrainNow,
    handleRescanAll,
    formatSize,
    lastClusteringRunLabel,
    featureMode,
    featureModeOptions,
    selectedFeatureMode,
    loadModels,
  } = useFeaturesController({
    monitorStatus,
    t,
    featureModeDefinitions: FEATURE_MODE_OPTIONS,
    getFeatureMode,
  });

  if (loading || !config) {
    return (
      <div className="flex items-center justify-center py-12 text-ide-muted text-sm">
        {t('settings.features.loading', '加载中...')}
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* 功能管理 */}
      <section className="space-y-3">
        <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
          <Layers className="w-4 h-4" />
          {t('settings.features.management.title', '功能管理')}
        </label>
        
        <div className="space-y-3">
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-3">
            <div>
              <p className="text-sm text-ide-text font-medium">{t('settings.features.management.featureMode.label', '功能等级')}</p>
              <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.featureMode.description', '选择截图语义功能的启用深度')}</p>
            </div>

            <SettingsSegmentedControl
              value={featureMode}
              options={featureModeOptions}
              onChange={handleFeatureModeChange}
              density="card"
              className="grid-cols-2 md:grid-cols-4"
            />

            {featureMode !== 'custom' && (
              <p className="text-xs text-ide-muted">{selectedFeatureMode.description}</p>
            )}

            <div className="pt-3 border-t border-ide-border/50">
              <button
                type="button"
                onClick={() => setCustomControlsOpen((open) => !open)}
                className={`flex w-full items-center justify-between gap-3 text-left rounded-lg px-2 py-1.5 transition-colors ${
                  customControlsOpen
                    ? 'bg-ide-panel/70 text-ide-text'
                    : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text'
                }`}
              >
                <span className="text-sm font-medium">{t('settings.features.management.featureMode.customControls.label')}</span>
                <ChevronDown className={`w-4 h-4 transition-transform ${customControlsOpen ? 'rotate-180' : ''}`} />
              </button>

              {customControlsOpen && (
                <div className="mt-3 space-y-3 rounded-lg border border-ide-border/70 bg-ide-panel/35 p-3">
                  {[
                    {
                      key: 'classification_enabled',
                      label: t('settings.features.management.classification.label', '内容分类'),
                      description: t('settings.features.management.classification.description', '使用 BGE 模型自动分类截图内容'),
                    },
                    {
                      key: 'clustering_enabled',
                      label: t('settings.features.management.clustering.label', '任务聚类'),
                      description: t('settings.features.management.clustering.description', '使用 MiniLM 模型将相似活动分组为长期任务'),
                    },
                    {
                      key: 'smart_cluster_enabled',
                      label: t('settings.features.management.smartCluster.label', '智能聚类（按描述自动归档）'),
                      description: scModelAvailable
                        ? t('settings.features.management.smartCluster.description', '输入一句话描述（如 "对加利福尼亚山脉的研究"），自动归档相关快照。仅在系统空闲时计算。')
                        : t('settings.features.management.smartCluster.modelMissing', '请先下载模型'),
                      disabled: !scModelAvailable && !config.smart_cluster_enabled,
                    },
                  ].map((item, index) => (
                    <div key={item.key} className={index > 0 ? 'border-t border-ide-border/50 pt-3' : ''}>
                      <div className="flex items-center justify-between gap-4">
                        <div className="min-w-0">
                          <p className="text-sm font-medium text-ide-text">{item.label}</p>
                          <p className={`mt-1 text-xs ${item.disabled ? 'text-ide-warning-muted' : 'text-ide-muted'}`}>{item.description}</p>
                        </div>
                        <SettingsSwitch
                          checked={Boolean(config[item.key])}
                          onChange={() => handleCustomFeatureToggle(item.key)}
                          disabled={item.disabled}
                          title={item.disabled ? t('settings.features.management.smartCluster.modelMissing', '请先下载模型') : item.label}
                        />
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

          {config.clustering_enabled && (
            <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
              <div className="flex items-start justify-between gap-4">
                <div className="flex-1 min-w-0">
                  <p className="text-sm text-ide-text font-medium">{t('settings.features.management.clustering.label', '任务聚类')}</p>
                  <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.clustering.description', '使用 MiniLM 模型将相似活动分组为长期任务')}</p>
                </div>
              </div>

              <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center justify-between gap-4">
                <div className="flex-1 min-w-0">
                  <p className="text-sm text-ide-muted">{t('settings.features.management.clustering.interval_label', '自动聚类间隔')}</p>
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
                    <ChevronDown className={`w-4 h-4 text-ide-muted transition-transform ${clusteringDropdownOpen ? 'rotate-180' : ''}`} />
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
                            await handleClusteringIntervalChange(interval);
                          }}
                          className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${interval === (config.clustering_interval || '1w') ? 'bg-ide-accent/10' : ''}`}
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

              <div className="mt-3 pt-3 border-t border-ide-border/50">
                <button
                  type="button"
                  onClick={() => setClusteringAdvancedOpen((v) => !v)}
                  className="flex w-full items-center justify-between gap-3 text-left"
                >
                  <span className="text-sm text-ide-muted">{t('settings.features.management.clustering.advanced_label', '高级')}</span>
                  <ChevronDown className={`w-4 h-4 text-ide-muted transition-transform ${clusteringAdvancedOpen ? 'rotate-180' : ''}`} />
                </button>

                {clusteringAdvancedOpen && (
                  <div className="mt-3 space-y-3">
                    <div className="grid grid-cols-1 sm:grid-cols-[1fr_auto_1fr] gap-2 items-center">
                      <input
                        type="date"
                        value={rangeStart}
                        onChange={(e) => setRangeStart(e.target.value)}
                        className="px-3 py-2 text-xs bg-ide-panel border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
                      />
                      <span className="hidden sm:block text-xs text-ide-muted">-</span>
                      <input
                        type="date"
                        value={rangeEnd}
                        onChange={(e) => setRangeEnd(e.target.value)}
                        className="px-3 py-2 text-xs bg-ide-panel border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
                      />
                    </div>

                    <div className="flex flex-wrap items-center gap-2">
                      <SettingsButton
                        onClick={handleRunClustering}
                        disabled={clusteringRunning || monitorStatus !== 'running'}
                        variant="primary"
                        icon={clusteringRunning ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : Play}
                      >
                        {t('settings.features.management.clustering.run_now', '立即运行聚类')}
                      </SettingsButton>
                      <span className="text-[11px] text-ide-muted">
                        {t('tasks.lastRun')}: {lastClusteringRunLabel}
                      </span>
                    </div>

                    {clusteringError && (
                      <div className="flex items-start gap-2 px-2.5 py-2 bg-red-500/10 border border-red-500/30 rounded-lg">
                        <X className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5 cursor-pointer" onClick={() => setClusteringError(null)} />
                        <span className="text-xs text-red-400">{clusteringError}</span>
                      </div>
                    )}
                    {clusteringNotice && (
                      <div className="flex items-start gap-2 px-2.5 py-2 bg-ide-accent/10 border border-ide-accent/30 rounded-lg">
                        <X className="w-3.5 h-3.5 text-ide-accent shrink-0 mt-0.5 cursor-pointer" onClick={() => setClusteringNotice(null)} />
                        <span className="text-xs text-ide-text">{clusteringNotice}</span>
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>
          )}

          {/* 智能聚类 (Smart Cluster) */}
          {(config.smart_cluster_enabled || !scModelAvailable || scDownloading || scDownloadError) && (
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
            <div className="flex items-start justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="text-sm text-ide-text font-medium flex items-center gap-1.5">
                  <Sparkles className="w-3.5 h-3.5 text-ide-accent" />
                  {t('settings.features.management.smartCluster.label', '智能聚类（按描述自动归档）')}
                </p>
                <p className="text-xs text-ide-muted mt-1">
                  {t('settings.features.management.smartCluster.description', '输入一句话描述（如 "对加利福尼亚山脉的研究"），自动归档相关快照。仅在系统空闲时计算。')}
                </p>
              </div>
            </div>

            {config.smart_cluster_enabled && !config.clustering_enabled && (
              <div className="mt-4 flex items-start gap-2.5 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
                <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0 mt-0.5" />
                <p className="text-xs leading-relaxed text-ide-warning-muted">
                  {t('settings.features.management.smartCluster.clusteringDisabledWarning', '任务聚类已关闭。智能聚类仍可在重扫时运行，但由于缺少截图采集阶段的自动文本向量化，候选召回需要临时编码，可能导致处理速度显著下降并增加资源占用。')}
                </p>
              </div>
            )}

            {/* Model not downloaded — show download CTA */}
            {!scModelAvailable && !scDownloading && !scDownloadError && (
              <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center justify-between gap-3">
                <p className="text-xs text-ide-muted">
                  {t('settings.features.management.smartCluster.modelNotDownloaded', 'bge-reranker-v2-m3 (uint8, ~570MB) 尚未下载')}
                </p>
                <SettingsButton
                  onClick={handleDownloadReranker}
                  variant="primary"
                  icon={Download}
                >
                  {t('settings.features.management.smartCluster.downloadModel', '下载模型')}
                </SettingsButton>
              </div>
            )}

            {/* Download in progress */}
            {scDownloading && (
              <div className="mt-4 pt-4 border-t border-ide-border/50">
                <div className="flex items-center gap-2 text-xs text-ide-muted mb-2">
                  <Loader2 className="w-3.5 h-3.5 animate-spin" />
                  {t('settings.features.management.smartCluster.downloading', '正在下载…')}
                </div>
                <textarea
                  readOnly
                  value={scDownloadLog.slice(-12).join('\n')}
                  rows={6}
                  className="w-full bg-ide-panel border border-ide-border rounded p-2 text-[11px] font-mono text-ide-muted resize-none"
                />
              </div>
            )}

            {scDownloadError && (
              <div className="mt-4 pt-4 border-t border-ide-border/50 flex items-center gap-2">
                <span className="flex-1 text-xs text-rose-400 break-all">{scDownloadError}</span>
                <SettingsButton
                  onClick={handleDownloadReranker}
                  variant="primary"
                  size="xs"
                  icon={RotateCcw}
                >
                  {t('settings.features.management.smartCluster.retry', '重试')}
                </SettingsButton>
              </div>
            )}

            {/* Enabled state — show status + manual triggers */}
            {scModelAvailable && config.smart_cluster_enabled && (
              <div className="mt-4 pt-4 border-t border-ide-border/50 space-y-3">
                <div className="flex items-center justify-between gap-2 text-xs">
                  <span className="text-ide-muted">
                    {t('settings.features.management.smartCluster.status', '状态')}：
                  </span>
                  <span className="text-ide-text flex items-center gap-3">
                    <span>{t('settings.features.management.smartCluster.pending', '待处理')}: <span className="font-mono">{scStatus?.pending_count ?? '—'}</span></span>
                    <span className="opacity-50">·</span>
                    <span>{t('settings.features.management.smartCluster.activeClusters', '启用的聚类')}: <span className="font-mono">{scStatus?.enabled_cluster_count ?? 0}/{scStatus?.total_cluster_count ?? 0}</span></span>
                  </span>
                </div>
                <div className="flex items-center gap-2 flex-wrap">
                  <SettingsButton
                    onClick={handleDrainNow}
                    icon={Zap}
                  >
                    {t('settings.features.management.smartCluster.drainNow', '立即处理待处理队列')}
                  </SettingsButton>
                  <SettingsButton
                    onClick={handleRescanAll}
                    icon={RefreshCw}
                  >
                    {t('settings.features.management.smartCluster.rescanAll', '全部重新匹配 hot 层')}
                  </SettingsButton>
                </div>
              </div>
            )}
          </div>
          )}
        </div>
      </section>
      
      {/* 模型管理 */}
      <section className="space-y-3 mt-8">
        <div className="flex items-center justify-between px-1">
          <label className="text-sm font-semibold text-ide-accent flex items-center gap-2">
            <Database className="w-4 h-4" />
            {t('settings.features.models.title', '模型管理')}
          </label>
          <button
            onClick={loadModels}
            disabled={modelsLoading || monitorStatus !== 'running'}
            className="p-1 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            title={t('settings.features.models.refresh', '刷新')}
          >
            <RefreshCw className={`w-4 h-4 ${modelsLoading ? 'animate-spin' : ''}`} />
          </button>
        </div>
        
        <div className="bg-ide-bg border border-ide-border rounded-xl overflow-hidden">
          <table className="w-full text-xs">
            <thead className="bg-ide-panel">
              <tr>
                <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.name', '模型')}</th>
                <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.purpose', '用途')}</th>
                <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.size', '大小')}</th>
                <th className="px-3 py-2 text-left font-medium text-ide-text">{t('settings.features.models.location', '位置')}</th>
              </tr>
            </thead>
            <tbody>
              {models.length > 0 ? (
                models.map(m => (
                  <tr key={m.name} className="border-t border-ide-border hover:bg-ide-hover transition-colors">
                    <td className="px-3 py-1.5 font-medium">{m.name}</td>
                    <td className="px-3 py-1.5 text-ide-muted whitespace-nowrap">{m.purpose}</td>
                    <td className="px-3 py-1.5 text-ide-muted whitespace-nowrap">{formatSize(m.size)}</td>
                    <td className="px-3 py-1.5 text-ide-muted">
                      <button
                        onClick={() => handleOpenLocation(m.path)}
                        className="p-1 hover:text-ide-text hover:bg-ide-panel rounded transition-colors"
                        title={m.path}
                      >
                        <ExternalLink className="w-3.5 h-3.5" />
                      </button>
                    </td>
                  </tr>
                ))
              ) : (
                <tr>
                  <td colSpan="4" className="px-4 py-6 text-center text-ide-muted text-sm">
                    {t('settings.features.models.empty', '暂无模型数据')}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
