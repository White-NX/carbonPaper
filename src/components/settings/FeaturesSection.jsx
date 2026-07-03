import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Layers, Database, ChevronDown, RefreshCw, ExternalLink, Sparkles, Download, Zap, RotateCcw, Loader2, AlertTriangle, Play, X } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { withAuth } from '../../lib/auth_api';
import { getClusteringStatus, runClustering, saveClusteringResults } from '../../lib/task_api';
import { getIndexHealth, retryVectorIndexing } from '../../lib/monitor_api';

export default function FeaturesSection({ monitorStatus }) {
  const { t } = useTranslation();
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [models, setModels] = useState([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [clusteringAdvancedOpen, setClusteringAdvancedOpen] = useState(false);
  const [clusteringRunning, setClusteringRunning] = useState(false);
  const [clusteringError, setClusteringError] = useState(null);
  const [clusteringNotice, setClusteringNotice] = useState(null);
  const [clusteringStatus, setClusteringStatus] = useState(null);
  const [rangeStart, setRangeStart] = useState('');
  const [rangeEnd, setRangeEnd] = useState('');
  const [indexHealth, setIndexHealth] = useState(null);
  const [indexHealthLoading, setIndexHealthLoading] = useState(false);
  const [indexHealthError, setIndexHealthError] = useState(null);
  const [vectorRetrying, setVectorRetrying] = useState(false);

  // Smart Cluster state
  const [scModelAvailable, setScModelAvailable] = useState(false);
  const [scStatus, setScStatus] = useState(null); // { pending_count, enabled_cluster_count, total_cluster_count }
  const [scDownloading, setScDownloading] = useState(false);
  const [scDownloadLog, setScDownloadLog] = useState([]);
  const [scDownloadError, setScDownloadError] = useState(null);
  const scDownloadStartedRef = useRef(false);

  const loadConfig = async () => {
    try {
      const result = await invoke('get_advanced_config');
      // Default to true if not present in older configs
      if (result.clustering_enabled === undefined) result.clustering_enabled = true;
      if (result.classification_enabled === undefined) result.classification_enabled = true;
      setConfig(result);
    } catch (err) {
      console.error('Failed to load advanced config:', err);
    } finally {
      setLoading(false);
    }
  };

  const loadModels = async () => {
    console.log('[FeaturesSection] loadModels called. monitorStatus:', monitorStatus);
    if (monitorStatus !== 'running') {
      console.log('[FeaturesSection] Aborting loadModels because monitorStatus is not running.');
      return;
    }
    setModelsLoading(true);
    try {
      console.log('[FeaturesSection] Invoking monitor_get_all_models');
      const res = await withAuth(() => invoke('monitor_get_all_models'));
      console.log('[FeaturesSection] Received raw response from backend:', res);
      const parsedRes = typeof res === 'string' ? JSON.parse(res) : res;
      console.log('[FeaturesSection] Parsed response:', parsedRes);
      if (parsedRes && parsedRes.status === 'success' && parsedRes.models) {
        console.log('[FeaturesSection] Successfully setting models state with', parsedRes.models.length, 'items');
        setModels(parsedRes.models);
      } else {
        console.warn('[FeaturesSection] Response format unexpected or not successful:', parsedRes);
      }
    } catch (err) {
      console.error('[FeaturesSection] Failed to fetch models:', err);
    } finally {
      setModelsLoading(false);
      console.log('[FeaturesSection] loadModels completed.');
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  useEffect(() => {
    if (monitorStatus === 'running') {
      loadModels();
    }
  }, [monitorStatus]);

  const refreshClusteringStatus = async () => {
    if (monitorStatus !== 'running') return;
    try {
      const result = await getClusteringStatus();
      if (result?.status === 'success') {
        setClusteringStatus(result);
      }
    } catch { /* ignore */ }
  };

  useEffect(() => {
    refreshClusteringStatus();
  }, [monitorStatus]);

  const loadIndexHealth = async ({ refreshVector = false } = {}) => {
    setIndexHealthLoading(true);
    setIndexHealthError(null);
    try {
      const result = await getIndexHealth({ refreshVector });
      setIndexHealth(result);
    } catch (err) {
      const message = err?.message || String(err);
      setIndexHealthError(message);
    } finally {
      setIndexHealthLoading(false);
    }
  };

  useEffect(() => {
    if (monitorStatus === 'running') {
      loadIndexHealth({ refreshVector: false });
    }
  }, [monitorStatus]);

  // Smart Cluster: check model availability + poll status
  const refreshSmartClusterModel = async () => {
    try {
      const modelStatus = await invoke('check_model_files');
      const reranker = modelStatus?.['bge-reranker-v2-m3'];
      setScModelAvailable(reranker?.complete === true);
    } catch (err) {
      console.warn('Failed to check reranker model:', err);
    }
  };

  const refreshSmartClusterStatus = async () => {
    try {
      const s = await withAuth(() => invoke('smart_cluster_status'));
      setScStatus(s);
    } catch { /* ignore */ }
  };

  useEffect(() => {
    refreshSmartClusterModel();
    refreshSmartClusterStatus();
    const interval = setInterval(() => {
      refreshSmartClusterStatus();
    }, 10000);
    return () => clearInterval(interval);
  }, []);

  // Forward install-log to local panel while downloading
  useEffect(() => {
    if (!scDownloading) return;
    let mounted = true;
    let unlisten;
    (async () => {
      try {
        unlisten = await listen('install-log', (event) => {
          if (!mounted) return;
          const line = event?.payload?.line || JSON.stringify(event?.payload || {});
          const ts = new Date().toLocaleTimeString();
          setScDownloadLog((prev) => [...prev, `[${ts}] ${line}`]);
        });
      } catch { /* ignore */ }
    })();
    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [scDownloading]);

  // Close dropdown on outside click
  useEffect(() => {
    const handler = () => {
      setClusteringDropdownOpen(false);
    };
    if (clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
  }, [clusteringDropdownOpen]);

  const saveConfig = async (newConfig) => {
    setConfig(newConfig);
    try {
      await withAuth(() => invoke('set_advanced_config', { config: newConfig }), { autoPrompt: true });
      // Notify python backend
      await withAuth(() => invoke('monitor_update_feature_config', {
        clusteringEnabled: newConfig.clustering_enabled,
        classificationEnabled: newConfig.classification_enabled,
      }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to save advanced config:', err);
    }
  };

  const handleOpenLocation = async (path) => {
    try {
      await invoke('open_path', { path });
    } catch (err) {
      console.error('Failed to open location:', err);
    }
  };

  const handleToggle = async (key) => {
    if (!config) return;
    const newConfig = { ...config, [key]: !config[key] };
    await saveConfig(newConfig);
  };

  const handleRunClustering = async () => {
    setClusteringRunning(true);
    setClusteringError(null);
    setClusteringNotice(null);
    try {
      const options = { manual: true };
      if (rangeStart) options.startTime = new Date(rangeStart).getTime() / 1000;
      if (rangeEnd) options.endTime = new Date(rangeEnd).getTime() / 1000;

      let result = await runClustering(options);
      if (result?.status === 'needs_user_choice') {
        const hasCompleteRange = Boolean(rangeStart && rangeEnd);
        const count = result?.estimate?.count ?? result?.n_total ?? 0;
        const memory = result?.estimate?.memory || {};
        const scope = hasCompleteRange
          ? t('tasks.clusteringRangeScope')
          : t('tasks.clusteringAllScope');
        const reason = result.reason === 'low_memory'
          ? t('tasks.clusteringLowMemoryReason')
          : t('tasks.clusteringLargeRangeReason');
        const useBatched = window.confirm(t('tasks.clusteringDegradePrompt', {
          scope,
          count,
          reason,
          estimatedGb: memory.estimated_peak_bytes
            ? (memory.estimated_peak_bytes / (1024 ** 3)).toFixed(1)
            : '-',
        }));
        result = await runClustering({
          ...options,
          clusteringMode: useBatched ? 'batched' : 'full',
        });
      }

      if (result?.status === 'empty') {
        setClusteringError(t('tasks.noData'));
      }

      if (result?.clusters?.length) {
        const taskRequests = result.clusters.map((cl) => ({
          auto_label: cl.dominant_process || null,
          dominant_process: cl.dominant_process || null,
          dominant_category: cl.dominant_category || null,
          start_time: cl.start_time || null,
          end_time: cl.end_time || null,
          snapshot_count: cl.snapshot_count || 0,
          layer: 'hot',
          screenshot_ids: (cl.snapshot_ids || []).map((id) => Number(id)),
          confidences: null,
        }));
        await saveClusteringResults(taskRequests);
        setClusteringNotice(t('settings.features.management.clustering.completed', {
          count: taskRequests.length,
        }));
      }

      if (result?.degraded) {
        setClusteringNotice(t('tasks.clusteringDegradedNotice', {
          sampleSize: result.sample_size ?? 0,
          assignedCount: result.assigned_count ?? 0,
        }));
      }

      await refreshClusteringStatus();
    } catch (err) {
      const msg = String(err?.message || err);
      if (msg.includes('not found') || msg.includes('ModelNotAvailable') || msg.includes('not downloaded')) {
        setClusteringError(t('tasks.modelMissing'));
      } else {
        setClusteringError(msg);
      }
      console.error('Clustering failed:', err);
    } finally {
      setClusteringRunning(false);
    }
  };

  const handleDownloadReranker = async () => {
    if (scDownloadStartedRef.current) return;
    scDownloadStartedRef.current = true;
    setScDownloading(true);
    setScDownloadLog([]);
    setScDownloadError(null);
    try {
      setScDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] Downloading bge-reranker-v2-m3 (uint8, ~570MB)…`]);
      await invoke('download_model', {
        repo: 'onnx-community/bge-reranker-v2-m3-ONNX',
        subdir: 'bge-reranker-v2-m3',
        files: [
          'config.json',
          'tokenizer.json',
          'tokenizer_config.json',
          'special_tokens_map.json',
          'onnx/model_uint8.onnx',
        ],
      });
      await invoke('mark_smart_cluster_setup_done', { dismissedPermanently: false });
      await refreshSmartClusterModel();
    } catch (err) {
      setScDownloadError(err?.message || String(err));
      scDownloadStartedRef.current = false;
    } finally {
      setScDownloading(false);
    }
  };

  const handleDrainNow = async () => {
    try {
      await withAuth(() => invoke('monitor_smart_cluster_drain_now'), { autoPrompt: true });
      // Refresh status soon after triggering.
      setTimeout(refreshSmartClusterStatus, 500);
    } catch (err) {
      console.warn('Failed to trigger drain_now:', err);
    }
  };

  const handleRescanAll = async () => {
    try {
      await withAuth(() => invoke('smart_cluster_rescan_all'), { autoPrompt: true });
      setTimeout(refreshSmartClusterStatus, 500);
    } catch (err) {
      console.warn('Failed to trigger rescan:', err);
    }
  };

  const handleRetryVectorIndexing = async () => {
    setVectorRetrying(true);
    setIndexHealthError(null);
    try {
      await retryVectorIndexing(32);
      await loadIndexHealth({ refreshVector: true });
    } catch (err) {
      const message = err?.message || String(err);
      setIndexHealthError(message);
    } finally {
      setVectorRetrying(false);
    }
  };

  const formatSize = (sizeStr) => {
    if (!sizeStr) return '-';
    return sizeStr;
  };

  const formatIndexCount = (value, fallback = '—') => {
    if (typeof value === 'number' && Number.isFinite(value)) {
      return value.toLocaleString();
    }
    return fallback;
  };

  const vectorRetryBacklog = indexHealth?.pending_retry_queue_count;
  const deleteQueuePending = indexHealth
    ? (indexHealth.delete_queue?.pending_screenshots ?? 0) + (indexHealth.delete_queue?.pending_ocr ?? 0)
    : null;
  const lastIndexingError = indexHealth?.last_indexing_error;
  const lastIndexingErrorAt = indexHealth?.last_indexing_error_at
    ? new Date(indexHealth.last_indexing_error_at * 1000).toLocaleString()
    : null;
  const storageIpc = indexHealth?.storage_ipc || indexHealth?.python?.storage_ipc || null;
  const storageIpcState = storageIpc?.circuit_state;
  const storageIpcLabel = storageIpcState === 'open'
    ? t('settings.features.management.indexHealth.ipcOpen', '熔断')
    : storageIpcState === 'half_open'
      ? t('settings.features.management.indexHealth.ipcHalfOpen', '探测')
      : storageIpcState === 'closed'
        ? t('settings.features.management.indexHealth.ipcClosed', '正常')
        : '—';
  const storageIpcRetryAfter = typeof storageIpc?.retry_after_secs === 'number'
    ? Math.ceil(storageIpc.retry_after_secs)
    : null;

  const lastClusteringRunLabel = clusteringStatus?.config?.last_run
    ? new Date(clusteringStatus.config.last_run * 1000).toLocaleString()
    : t('tasks.never');

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
          {/* 任务聚类 */}
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
            <div className="flex items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="text-sm text-ide-text font-medium">{t('settings.features.management.clustering.label', '任务聚类')}</p>
                <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.clustering.description', '使用 MiniLM 模型将相似活动分组为长期任务')}</p>
              </div>
              <button
                onClick={() => handleToggle('clustering_enabled')}
                className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${config.clustering_enabled ? 'bg-ide-accent' : 'bg-ide-border'}`}
              >
                <div
                  className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.clustering_enabled ? 'translate-x-5' : 'translate-x-0.5'}`}
                />
              </button>
            </div>
            
            {/* 聚类间隔设置 - 仅在启用时显示 */}
            {config.clustering_enabled && (
              <>
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
                              setClusteringDropdownOpen(false);
                              const newConfig = { ...config, clustering_interval: interval };
                              await saveConfig(newConfig);
                              try {
                                await withAuth(() => invoke('monitor_set_clustering_interval', { interval }), { autoPrompt: true });
                              } catch { /* best-effort */ }
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
                        <button
                          onClick={handleRunClustering}
                          disabled={clusteringRunning || monitorStatus !== 'running'}
                          className="flex items-center gap-1.5 px-3 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                        >
                          {clusteringRunning ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Play className="w-3.5 h-3.5" />}
                          {t('settings.features.management.clustering.run_now', '立即运行聚类')}
                        </button>
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
              </>
            )}
          </div>
          
          {/* 分类 */}
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
            <div className="flex items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="text-sm text-ide-text font-medium">{t('settings.features.management.classification.label', '内容分类')}</p>
                <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.classification.description', '使用 BGE 模型自动分类截图内容')}</p>
              </div>
              <button
                onClick={() => handleToggle('classification_enabled')}
                className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${config.classification_enabled ? 'bg-ide-accent' : 'bg-ide-border'}`}
              >
                <div
                  className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.classification_enabled ? 'translate-x-5' : 'translate-x-0.5'}`}
                />
              </button>
            </div>
          </div>

          {/* 索引健康 */}
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
            <div className="flex items-start justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="text-sm text-ide-text font-medium flex items-center gap-1.5">
                  <Database className="w-3.5 h-3.5 text-ide-accent" />
                  {t('settings.features.management.indexHealth.label', '索引健康')}
                </p>
                <p className="text-xs text-ide-muted mt-1">
                  {t('settings.features.management.indexHealth.description', '截图、OCR、向量索引和后台重试队列的当前状态')}
                </p>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <button
                  type="button"
                  onClick={() => loadIndexHealth({ refreshVector: true })}
                  disabled={indexHealthLoading}
                  className="p-1.5 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  title={t('settings.features.management.indexHealth.refresh', '刷新')}
                >
                  <RefreshCw className={`w-4 h-4 ${indexHealthLoading ? 'animate-spin' : ''}`} />
                </button>
                <button
                  type="button"
                  onClick={handleRetryVectorIndexing}
                  disabled={vectorRetrying || !vectorRetryBacklog || !indexHealth?.monitor_available}
                  className="flex items-center gap-1.5 px-3 py-1.5 bg-ide-panel border border-ide-border hover:bg-ide-hover/40 text-ide-text rounded text-xs transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                  title={t('settings.features.management.indexHealth.retry', '重试失败向量')}
                >
                  {vectorRetrying ? <Loader2 className="w-3 h-3 animate-spin" /> : <RotateCcw className="w-3 h-3" />}
                  {t('settings.features.management.indexHealth.retry', '重试失败向量')}
                </button>
              </div>
            </div>

            <div className="mt-4 grid grid-cols-2 md:grid-cols-3 gap-x-4 gap-y-3 text-xs">
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.screenshots', '截图')}</p>
                <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.screenshots_count)}</p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.ocrRows', 'OCR 行')}</p>
                <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.ocr_rows_count)}</p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.vectorRows', '向量')}</p>
                <p className="mt-1 font-mono text-ide-text">
                  {formatIndexCount(indexHealth?.vector_rows_count, indexHealth?.worker_started === false ? t('settings.features.management.indexHealth.notLoaded', '未加载') : '—')}
                </p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.vectorRetry', '向量重试')}</p>
                <p className="mt-1 font-mono text-ide-text">{formatIndexCount(vectorRetryBacklog)}</p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.deleteQueue', '删除队列')}</p>
                <p className="mt-1 font-mono text-ide-text">{formatIndexCount(deleteQueuePending)}</p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.smartPending', '智能聚类待处理')}</p>
                <p className="mt-1 font-mono text-ide-text">{formatIndexCount(indexHealth?.smart_cluster_pending_count)}</p>
              </div>
              <div>
                <p className="text-ide-muted">{t('settings.features.management.indexHealth.storageIpc', '存储 IPC')}</p>
                <p className="mt-1 font-mono text-ide-text">
                  {storageIpcLabel}
                  {storageIpcRetryAfter ? ` ${storageIpcRetryAfter}s` : ''}
                </p>
              </div>
            </div>

            {(indexHealthError || indexHealth?.monitor_error || lastIndexingError) && (
              <div className="mt-4 flex items-start gap-2 px-2.5 py-2 bg-red-500/10 border border-red-500/30 rounded-lg">
                <AlertTriangle className="w-3.5 h-3.5 text-red-400 shrink-0 mt-0.5" />
                <div className="min-w-0 text-xs text-red-300">
                  <p className="break-all">{indexHealthError || indexHealth?.monitor_error || lastIndexingError}</p>
                  {lastIndexingErrorAt && (
                    <p className="mt-1 text-red-300/70">{lastIndexingErrorAt}</p>
                  )}
                </div>
              </div>
            )}
          </div>

          {/* 智能聚类 (Smart Cluster) */}
          <div className="p-4 bg-ide-bg border border-ide-border rounded-xl">
            <div className="flex items-center justify-between gap-4">
              <div className="flex-1 min-w-0">
                <p className="text-sm text-ide-text font-medium flex items-center gap-1.5">
                  <Sparkles className="w-3.5 h-3.5 text-ide-accent" />
                  {t('settings.features.management.smartCluster.label', '智能聚类（按描述自动归档）')}
                </p>
                <p className="text-xs text-ide-muted mt-1">
                  {t('settings.features.management.smartCluster.description', '输入一句话描述（如 "对加利福尼亚山脉的研究"），自动归档相关快照。仅在系统空闲时计算。')}
                </p>
              </div>
              <button
                onClick={() => scModelAvailable && handleToggle('smart_cluster_enabled')}
                disabled={!scModelAvailable}
                className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${
                  !scModelAvailable
                    ? 'bg-ide-border opacity-40 cursor-not-allowed'
                    : config.smart_cluster_enabled
                      ? 'bg-ide-accent'
                      : 'bg-ide-border'
                }`}
                title={!scModelAvailable ? t('settings.features.management.smartCluster.modelMissing', '请先下载模型') : undefined}
              >
                <div
                  className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${config.smart_cluster_enabled && scModelAvailable ? 'translate-x-5' : 'translate-x-0.5'}`}
                />
              </button>
            </div>

            {!config.clustering_enabled && (
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
                <button
                  onClick={handleDownloadReranker}
                  className="flex items-center gap-1.5 px-3 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-xs font-medium transition-colors"
                >
                  <Download className="w-3 h-3" />
                  {t('settings.features.management.smartCluster.downloadModel', '下载模型')}
                </button>
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
                <button
                  onClick={handleDownloadReranker}
                  className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
                >
                  <RotateCcw className="w-3 h-3" />
                  {t('settings.features.management.smartCluster.retry', '重试')}
                </button>
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
                  <button
                    onClick={handleDrainNow}
                    className="flex items-center gap-1.5 px-3 py-1.5 bg-ide-panel border border-ide-border hover:bg-ide-hover/40 text-ide-text rounded text-xs transition-colors"
                  >
                    <Zap className="w-3 h-3" />
                    {t('settings.features.management.smartCluster.drainNow', '立即处理待处理队列')}
                  </button>
                  <button
                    onClick={handleRescanAll}
                    className="flex items-center gap-1.5 px-3 py-1.5 bg-ide-panel border border-ide-border hover:bg-ide-hover/40 text-ide-text rounded text-xs transition-colors"
                  >
                    <RefreshCw className="w-3 h-3" />
                    {t('settings.features.management.smartCluster.rescanAll', '全部重新匹配 hot 层')}
                  </button>
                </div>
              </div>
            )}
          </div>
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
