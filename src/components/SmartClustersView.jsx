import React, { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Sparkles, Plus, Loader2, RefreshCw, AlertCircle, X,
  Zap, Image as ImageIcon, Clock, Hash, Trash2,
} from 'lucide-react';
import {
  listSmartClusters, deleteSmartCluster, updateSmartClusterAnchor,
  toggleSmartClusterEnabled, getSmartClusterAssignments,
  smartClusterDrainNow, getSmartClusterStatus, createSmartCluster,
  smartClusterStopDrain,
} from '../lib/task_api';
import { fetchThumbnailBatch, getSmartClusterWorkerStatus } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';
import ClusterCard from './ClusterCard';
import NlClusterView from './NlClusterView';

function formatTimestamp(ts) {
  if (!ts) return '—';
  try {
    const d = new Date(ts.includes('T') ? ts : ts.replace(' ', 'T') + 'Z');
    if (Number.isNaN(d.getTime())) return '—';
    return d.toLocaleString();
  } catch {
    return '—';
  }
}

function formatAssignedAt(ts) {
  if (!ts) return '';
  const d = new Date(ts.includes('T') ? ts : ts.replace(' ', 'T') + 'Z');
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString();
}

export default function SmartClustersView({ backendOnline, onSelectScreenshot, onOpenSnapshotPreview }) {
  const { t } = useTranslation();
  const [clusters, setClusters] = useState([]);
  const [selectedId, setSelectedId] = useState(null);
  const [assignments, setAssignments] = useState([]);
  const [loading, setLoading] = useState(false);
  const [statusData, setStatusData] = useState(null);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState(null);
  const [thumbnailCache, setThumbnailCache] = useState({});

  const selected = clusters.find(c => c.id === selectedId) || null;

  const loadClusters = useCallback(async () => {
    setLoading(true);
    try {
      const result = await listSmartClusters();
      setClusters(result || []);
    } catch (err) {
      console.error('Failed to load smart clusters:', err);
      setError(err?.message || String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadStatus = useCallback(async () => {
    if (!backendOnline) return;
    try {
      const s = await getSmartClusterStatus();
      const w = await getSmartClusterWorkerStatus();
      setStatusData({
        ...s,
        is_running: w.running && w.pending_count > 0,
        is_force_running: w.forceRunning && w.pending_count > 0,
      });
    } catch { /* ignore */ }
  }, [backendOnline]);

  const loadAssignments = useCallback(async (id) => {
    if (!id) { setAssignments([]); return; }
    try {
      const result = await getSmartClusterAssignments(id, 0, 100);
      setAssignments(result || []);
    } catch (err) {
      console.error('Failed to load assignments:', err);
    }
  }, []);

  // Polling helper
  const handlePoll = useCallback(async () => {
    await loadStatus();
    try {
      const result = await listSmartClusters();
      setClusters(result || []);
    } catch { /* ignore */ }
    if (selectedId) {
      await loadAssignments(selectedId);
    }
  }, [loadStatus, selectedId, loadAssignments]);

  useEffect(() => {
    loadClusters();
    loadStatus();
    const interval = setInterval(handlePoll, 10000);
    return () => clearInterval(interval);
  }, [loadClusters, loadStatus, handlePoll]);

  // Load assignments when a cluster is selected
  useEffect(() => {
    loadAssignments(selectedId);
  }, [selectedId, loadAssignments]);

  // Batch-load thumbnails
  useEffect(() => {
    if (!assignments.length) { setThumbnailCache({}); return; }
    let active = true;
    const ids = [...new Set(assignments
      .map(s => s.screenshot_id)
      .filter(id => typeof id === 'number' && id > 0))];
    if (!ids.length) return;
    fetchThumbnailBatch(ids)
      .then(batch => { if (active && batch) setThumbnailCache(batch); })
      .catch(err => console.error('thumbnail batch failed:', err));
    return () => { active = false; };
  }, [assignments]);

  const handleRename = async (id, label) => {
    try {
      await updateSmartClusterAnchor(id, label);
      await loadClusters();
    } catch (err) {
      console.error('Rename failed:', err);
      setError(err?.message || String(err));
      throw err;
    }
  };

  const handleDelete = async (id) => {
    if (!confirm(t('smartClusters.confirmDelete', '确定要删除这个智能聚类吗？已分配的快照不会被删除。'))) return;
    try {
      await deleteSmartCluster(id);
      if (selectedId === id) setSelectedId(null);
      await loadClusters();
    } catch (err) {
      console.error('Delete failed:', err);
      setError(err?.message || String(err));
    }
  };

  const handleTogglePause = async (id) => {
    const cluster = clusters.find(c => c.id === id);
    if (!cluster) return;
    try {
      await toggleSmartClusterEnabled(id, !cluster.enabled);
      await loadClusters();
    } catch (err) {
      console.error('Toggle failed:', err);
      setError(err?.message || String(err));
    }
  };

  const handleDrainNow = async () => {
    try {
      await smartClusterDrainNow();
      setTimeout(loadStatus, 500);
    } catch (err) {
      console.error('Drain now failed:', err);
    }
  };

  const handleStopDrain = async () => {
    try {
      await smartClusterStopDrain();
      setTimeout(loadStatus, 500);
    } catch (err) {
      console.error('Stop drain failed:', err);
    }
  };

  const handleSaveCalibration = useCallback(async (req) => {
    try {
      await createSmartCluster(req);
      setCreating(false);
      await loadClusters();
      await loadStatus();
    } catch (err) {
      console.error('Create smart cluster failed:', err);
      throw err;
    }
  }, [loadClusters, loadStatus]);

  // Render the calibration sub-page when creating
  if (creating) {
    return (
      <NlClusterView
        mode="calibrate"
        backendOnline={backendOnline}
        onSelectScreenshot={onSelectScreenshot}
        onSaveCalibration={handleSaveCalibration}
        onCancelCalibration={() => setCreating(false)}
      />
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* Toolbar */}
      <div className="shrink-0 border-b border-ide-border bg-ide-panel px-4 py-2.5 space-y-2">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-ide-text flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-ide-accent" />
            {t('smartClusters.title', '智能聚类')}
            <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">{t('smartClusters.beta', 'beta')}</span>
          </h2>
          <div className="flex items-center gap-1.5">
            <button
              onClick={() => setCreating(true)}
              disabled={!backendOnline}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-accent bg-ide-accent/20 text-ide-accent hover:bg-ide-accent/30 disabled:opacity-40 transition-colors"
            >
              <Plus className="w-3 h-3" />
              {t('smartClusters.newCluster', '新建智能聚类')}
            </button>
            <button
              onClick={handleDrainNow}
              disabled={!backendOnline || !statusData?.pending_count || statusData?.is_running}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-border text-ide-muted hover:text-ide-text hover:bg-ide-hover/30 disabled:opacity-40 transition-colors"
              title={t('smartClusters.processNowTooltip', '立即处理待处理队列')}
            >
              <Zap className="w-3 h-3" />
              {t('smartClusters.processNow', '立即处理')}
            </button>
            {statusData?.is_force_running && (
              <button
                onClick={handleStopDrain}
                className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-rose-500/30 bg-rose-500/10 text-rose-400 hover:bg-rose-500/20 transition-colors"
                title={t('smartClusters.stopDrainTooltip', '停止处理')}
              >
                <X className="w-3 h-3 text-rose-400" />
                {t('smartClusters.stopDrain', '停止')}
              </button>
            )}
            <button
              onClick={loadClusters}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-border text-ide-muted hover:text-ide-text hover:bg-ide-hover/30 transition-colors"
            >
              <RefreshCw className={`w-3.5 h-3.5 ${loading ? 'animate-spin' : ''}`} />
              {t('smartClusters.refresh', '刷新')}
            </button>
          </div>
        </div>

        {/* Status row */}
        <div className="flex items-center gap-3 text-[11px] text-ide-muted">
          <span>{t('smartClusters.statusClusters', '聚类:')} <span className="text-ide-text font-mono">{statusData?.enabled_cluster_count ?? 0}/{statusData?.total_cluster_count ?? clusters.length}</span> {t('smartClusters.statusEnabled', '已启用')}</span>
          <span>·</span>
          <span>{t('smartClusters.statusPending', '待处理:')} <span className="text-ide-text font-mono">{statusData?.pending_count ?? 0}</span></span>
          <span className="ml-auto opacity-70">
            {t('smartClusters.idleWarning', '后台工作线程仅在系统空闲时运行（无键鼠输入 ≥5分钟、AC 通电、非全屏游戏）')}
          </span>
        </div>

        {error && (
          <div className="flex items-center gap-2 px-2.5 py-1.5 bg-red-500/10 border border-red-500/30 rounded-lg">
            <AlertCircle className="w-3.5 h-3.5 text-red-400 shrink-0" />
            <span className="text-xs text-red-400 break-all flex-1">{error}</span>
            <button onClick={() => setError(null)} className="text-red-400 hover:text-red-300">
              <X className="w-3 h-3" />
            </button>
          </div>
        )}
      </div>

      {/* Content */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* List pane */}
        <div className="w-80 shrink-0 border-r border-ide-border overflow-y-auto p-2 space-y-2">
          {loading && !clusters.length ? (
            <div className="flex items-center justify-center h-32">
              <Loader2 className="w-5 h-5 animate-spin text-ide-muted" />
            </div>
          ) : !clusters.length ? (
            <div className="flex flex-col items-center justify-center h-48 text-sm text-ide-muted gap-3 px-6 text-center">
              <Sparkles className="w-8 h-8 opacity-40" />
              <div>
                <p className="text-ide-text font-medium">{t('smartClusters.noClusters', '还没有智能聚类')}</p>
                <p className="text-[11px] opacity-70 mt-1">
                  {t('smartClusters.noClustersHint', '新建一个智能聚类，让相关快照自动归档')}
                </p>
              </div>
              <button
                onClick={() => setCreating(true)}
                className="flex items-center gap-1 px-3 py-1.5 text-xs rounded bg-ide-accent text-white hover:bg-ide-accent/90 transition-colors"
              >
                <Plus className="w-3 h-3" />
                {t('smartClusters.createFirst', '创建第一个聚类')}
              </button>
            </div>
          ) : (
            clusters.map((c) => (
              <ClusterCard
                key={c.id}
                variant="smart"
                id={c.id}
                title={c.anchor_text}
                subtitle={null}
                accentColor={c.dominant_color || '#6b7280'}
                metaChips={[
                  { key: 'count', icon: ImageIcon, text: String(c.assignment_count ?? 0) },
                  { key: 'thresh', icon: Hash, text: `≥${(c.threshold ?? 0).toFixed(2)}` },
                ]}
                timeRange={t('smartClusters.updatedAt', '更新于 {{time}}', { time: formatTimestamp(c.updated_at) })}
                status={c.enabled ? 'active' : 'paused'}
                selected={selectedId === c.id}
                onSelect={setSelectedId}
                onRename={handleRename}
                onDelete={handleDelete}
                onTogglePause={handleTogglePause}
              />
            ))
          )}
        </div>

        {/* Detail pane */}
        <div className="relative flex-1 min-h-0 overflow-hidden">
          <div className="h-full overflow-y-auto">
            {selected ? (
              <div className="p-4 space-y-3">
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="text-sm text-ide-text font-medium flex items-center gap-2">
                      <div
                        className="w-2 h-2 rounded-full"
                        style={{ backgroundColor: selected.dominant_color || '#6b7280' }}
                      />
                      {selected.anchor_text}
                      <span className={`px-1.5 py-0.5 rounded text-[10px] ${
                        selected.enabled
                          ? 'bg-emerald-500/15 text-emerald-400 border border-emerald-500/30'
                          : 'bg-ide-bg text-ide-muted border border-ide-border'
                      }`}>
                        {selected.enabled ? t('smartClusters.enabled', '已启用') : t('smartClusters.paused', '已暂停')}
                      </span>
                    </div>
                    <div className="text-[11px] text-ide-muted mt-1 flex items-center gap-3">
                      <span>{t('smartClusters.threshold', '阈值:')} <span className="font-mono text-ide-text">{(selected.threshold ?? 0).toFixed(2)}</span></span>
                      <span>·</span>
                      <span>{t('smartClusters.archived', '已归档:')} <span className="font-mono text-ide-text">{selected.assignment_count ?? 0}</span> {t('smartClusters.snapshotsCount', '张快照')}</span>
                    </div>
                  </div>
                </div>

                {/* Assignments */}
                {assignments.length === 0 ? (
                  <div className="flex flex-col items-center justify-center h-40 text-sm text-ide-muted gap-2">
                    <Clock className="w-6 h-6 opacity-40" />
                    <span>{t('smartClusters.noAssignments', '暂无已分配的快照')}</span>
                    <span className="text-[11px] opacity-70">
                      {t('smartClusters.idleProcessingHint', '后台工作线程会在系统空闲时陆续处理待评分队列')}
                    </span>
                  </div>
                ) : (
                  <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
                    {assignments.map((s) => (
                      <ThumbnailCard
                        key={s.screenshot_id}
                        sourceType="clusters"
                        item={{
                          screenshot_id: s.screenshot_id,
                          image_path: s.image_path,
                          process_name: s.process_name,
                          window_title: s.window_title,
                          category: s.category,
                          created_at: s.created_at,
                          assigned_at: s.assigned_at,
                          rerank_score: s.rerank_score,
                        }}
                        preloadedSrc={thumbnailCache[s.screenshot_id] || null}
                        onSelect={(payload) => {
                          const enriched = {
                            ...payload,
                            assigned_at: s.assigned_at,
                            rerank_score: s.rerank_score,
                          };
                          onSelectScreenshot?.(enriched);
                        }}
                        onOpenFloatingPreview={onOpenSnapshotPreview
                          ? (payload) => {
                            const enriched = {
                              ...payload,
                              assigned_at: s.assigned_at,
                              rerank_score: s.rerank_score,
                            };
                            onOpenSnapshotPreview(enriched, {
                              thumbnailSrc: thumbnailCache[s.screenshot_id] || null,
                              sourceLabel: t('smartClusters.title', '智能聚类'),
                              sourceDetail: selected.anchor_text,
                              sourceType: 'smart-cluster',
                            });
                          }
                          : undefined}
                        footerText={s.rerank_score !== null && s.rerank_score !== undefined
                          ? `score ${s.rerank_score.toFixed(2)}`
                          : null}
                        footerPersistent={false}
                      />
                    ))}
                  </div>
                )}
              </div>
            ) : (
              <div className="flex items-center justify-center h-full text-sm text-ide-muted">
                {t('smartClusters.selectClusterToView', '选择左侧聚类查看已归档的快照')}
              </div>
            )}
          </div>

        </div>
      </div>
    </div>
  );
}
