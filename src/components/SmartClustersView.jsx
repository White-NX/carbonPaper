import React, { useState, useEffect, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Sparkles, Plus, Loader2, RefreshCw, AlertCircle, X,
  Zap, Clock, FileText, CircleDot, PauseCircle, Inbox,
} from 'lucide-react';
import {
  listSmartClusters, deleteSmartCluster, renameSmartCluster,
  toggleSmartClusterEnabled, getSmartClusterAssignments,
  smartClusterDrainNow, getSmartClusterStatus, createSmartCluster,
  smartClusterStopDrain,
} from '../lib/task_api';
import { fetchThumbnailBatch, getSmartClusterWorkerStatus } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';
import ClusterRow, { formatRelativeStamp } from './cluster/ClusterRow';
import SmartClusterCreateView from './SmartClusterCreateView';
import { PageHeader, StatusChip } from './PageHeader';
import { ConfirmDialog } from './ConfirmDialog';

function clusterName(cluster) {
  if (!cluster) return '';
  return cluster.display_name || cluster.anchor_text || '';
}

function clusterSubtitle(cluster) {
  if (!cluster) return '';
  if (cluster.last_window_title) {
    return cluster.last_process_name
      ? `${cluster.last_process_name} · ${cluster.last_window_title}`
      : cluster.last_window_title;
  }
  if (cluster.last_process_name) {
    return cluster.last_process_name;
  }
  return '';
}

function formatTimestamp(ts) {
  if (!ts) return '—';
  try {
    const d = new Date(ts.includes('T') ? ts : `${ts.replace(' ', 'T')}Z`);
    if (Number.isNaN(d.getTime())) return '—';
    return d.toLocaleString();
  } catch {
    return '—';
  }
}

function isAuthRequiredError(err) {
  return String(err?.message || err || '').includes('AUTH_REQUIRED');
}

function emitAuthRequired() {
  window.dispatchEvent(new CustomEvent('cp-auth-required'));
}

function extractSnapshotId(item) {
  if (item === null || item === undefined) return null;
  if (typeof item === 'number' && item > 0) return item;
  if (typeof item === 'string') {
    const match = item.match(/#?(\d{2,})/);
    return match ? Number(match[1]) : null;
  }
  if (typeof item === 'object') {
    const id = item.screenshot_id ?? item.snapshot_id ?? item.id;
    return typeof id === 'number' && id > 0 ? id : null;
  }
  return null;
}

function getEvidenceRef(item, index) {
  if (item && typeof item === 'object') {
    return item.ref ?? item.reference ?? item.index ?? index + 1;
  }
  return index + 1;
}

function formatKeyPoint(item) {
  if (item === null || item === undefined) return '';
  if (typeof item === 'string') return item;
  if (typeof item === 'number' || typeof item === 'boolean') return String(item);
  if (typeof item === 'object') {
    const text = item.point || item.text || item.summary || item.title || item.label;
    if (text) return text;
    try {
      return JSON.stringify(item);
    } catch {
      return String(item);
    }
  }
  return '';
}

function formatEvidenceItem(item, index) {
  const snapshotId = extractSnapshotId(item);
  const ref = getEvidenceRef(item, index);
  if (item === null || item === undefined) {
    return { ref, snapshotId, text: '', payload: null };
  }
  if (typeof item === 'string') {
    return {
      ref,
      snapshotId,
      text: item.replace(/^#?\d+\s*[:：-]?\s*/, '').trim(),
      payload: snapshotId ? { screenshot_id: snapshotId } : null,
    };
  }
  if (typeof item === 'number') {
    return {
      ref,
      snapshotId,
      text: '',
      payload: snapshotId ? { screenshot_id: snapshotId } : null,
    };
  }
  if (typeof item !== 'object') {
    return { ref, snapshotId, text: String(item), payload: null };
  }

  const parts = [];
  if (item.label || item.title) parts.push(item.label || item.title);
  if (item.window_title) parts.push(item.window_title);
  if (item.excerpt) parts.push(`"${item.excerpt}"`);
  if (item.text) parts.push(item.text);
  if (item.time || item.created_at) parts.push(item.time || item.created_at);

  return {
    ref,
    snapshotId,
    text: parts.join(' · '),
    payload: snapshotId
      ? {
        screenshot_id: snapshotId,
        id: snapshotId,
        window_title: item.window_title || item.title || item.label || null,
        process_name: item.process_name || null,
        category: item.category || null,
        created_at: item.created_at || item.time || null,
      }
      : null,
  };
}

function getEvidenceByRef(evidenceItems, ref) {
  const normalizedRef = String(ref);
  return evidenceItems.find((item, index) => String(getEvidenceRef(item, index)) === normalizedRef);
}

function isSafeMarkdownUrl(url) {
  return /^(https?:|mailto:)/i.test(String(url || '').trim());
}

function MarkdownInline({ text, evidenceItems, onOpenCitation }) {
  if (!text) return null;

  const parts = [];
  const pattern = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^)]+\)|\[(\d+)\])/g;
  let lastIndex = 0;
  let match;

  while ((match = pattern.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }

    const token = match[0];
    const linkMatch = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
    const citationMatch = token.match(/^\[(\d+)\]$/);
    if (linkMatch) {
      const [, label, url] = linkMatch;
      if (isSafeMarkdownUrl(url)) {
        parts.push(
          <a
            key={`link-${match.index}`}
            href={url}
            target="_blank"
            rel="noreferrer"
            className="text-ide-accent underline decoration-ide-accent/40 underline-offset-2 hover:text-ide-accent/80"
          >
            <MarkdownInline text={label} evidenceItems={evidenceItems} onOpenCitation={onOpenCitation} />
          </a>,
        );
      } else {
        parts.push(label);
      }
    } else if (citationMatch) {
      const ref = citationMatch[1];
      const evidence = getEvidenceByRef(evidenceItems, ref);
      const snapshotId = extractSnapshotId(evidence);
      if (evidence && snapshotId) {
        parts.push(
          <button
            key={`${ref}-${match.index}`}
            type="button"
            onClick={() => onOpenCitation(evidence)}
            className="mx-0.5 inline-flex h-5 min-w-5 items-center justify-center rounded border border-ide-accent/40 bg-ide-accent/10 px-1.5 text-[11px] font-medium text-ide-accent hover:bg-ide-accent/20"
            title={`#${snapshotId}`}
          >
            {ref}
          </button>,
        );
      } else {
        parts.push(token);
      }
    } else if (token.startsWith('`')) {
      parts.push(
        <code key={`code-${match.index}`} className="rounded bg-ide-bg px-1 py-0.5 font-mono text-[0.92em] text-ide-accent">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith('**')) {
      parts.push(<strong key={`bold-${match.index}`}>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith('*')) {
      parts.push(<em key={`italic-${match.index}`}>{token.slice(1, -1)}</em>);
    } else {
      parts.push(token);
    }

    lastIndex = match.index + token.length;
  }

  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return parts;
}

function MarkdownText({ text, evidenceItems = [], onOpenCitation, className }) {
  if (!text) return null;
  const paragraphs = String(text).split(/\n\s*\n/).filter((p) => p.trim());
  return (
    <div className={className}>
      {paragraphs.map((p, idx) => (
        <p key={idx} className="leading-relaxed">
          <MarkdownInline text={p} evidenceItems={evidenceItems} onOpenCitation={onOpenCitation} />
        </p>
      ))}
    </div>
  );
}

export default function SmartClustersView({
  onSelectScreenshot,
  onOpenSnapshotPreview,
  active = true,
}) {
  const { t } = useTranslation();
  const [clusters, setClusters] = useState([]);
  const [selectedId, setSelectedId] = useState(null);
  const [assignments, setAssignments] = useState([]);
  const [statusData, setStatusData] = useState(null);
  const [workerStatus, setWorkerStatus] = useState(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [creating, setCreating] = useState(false);
  const [pendingDeleteId, setPendingDeleteId] = useState(null);
  const [deleteLoading, setDeleteLoading] = useState(false);
  const [drainRequested, setDrainRequested] = useState(false);
  const [thumbnailCache, setThumbnailCache] = useState({});

  const loadClusters = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await listSmartClusters();
      setClusters(list || []);
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      setError(err?.message || String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  const loadStatus = useCallback(async () => {
    try {
      const [s, ws] = await Promise.allSettled([
        getSmartClusterStatus(),
        getSmartClusterWorkerStatus(),
      ]);
      if (s.status === 'fulfilled' && s.value) setStatusData(s.value);
      if (ws.status === 'fulfilled' && ws.value) setWorkerStatus(ws.value);
    } catch (err) {
      console.warn('loadStatus error:', err);
    }
  }, []);

  useEffect(() => {
    if (!active) return;
    loadClusters();
    loadStatus();
  }, [active, loadClusters, loadStatus]);

  const selected = useMemo(
    () => clusters.find((c) => c.id === selectedId) || null,
    [clusters, selectedId],
  );

  const selectedSummary = selected?.summary || null;
  const selectedEvidence = useMemo(() => {
    if (!selectedSummary?.evidence) return [];
    if (Array.isArray(selectedSummary.evidence)) return selectedSummary.evidence;
    if (typeof selectedSummary.evidence === 'object') return Object.values(selectedSummary.evidence);
    return [];
  }, [selectedSummary]);

  const selectedKeyPoints = useMemo(() => {
    if (!selectedSummary?.key_points) return [];
    if (Array.isArray(selectedSummary.key_points)) return selectedSummary.key_points;
    if (typeof selectedSummary.key_points === 'object') return Object.values(selectedSummary.key_points);
    return [];
  }, [selectedSummary]);

  const loadAssignments = useCallback(async (clusterId) => {
    if (!clusterId) {
      setAssignments([]);
      return;
    }
    try {
      const res = await getSmartClusterAssignments(clusterId, 0, 50);
      const items = res?.items || res || [];
      setAssignments(items);
      const ids = items.map((a) => a.screenshot_id).filter(Boolean);
      if (ids.length > 0) {
        fetchThumbnailBatch(ids)
          .then((batch) => {
            if (batch) setThumbnailCache((prev) => ({ ...prev, ...batch }));
          })
          .catch((err) => console.error('fetchThumbnailBatch failed:', err));
      }
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      console.error('loadAssignments failed:', err);
    }
  }, []);

  useEffect(() => {
    if (selectedId) loadAssignments(selectedId);
    else setAssignments([]);
  }, [selectedId, loadAssignments]);

  const handleTogglePause = useCallback(async (id) => {
    const c = clusters.find((item) => item.id === id);
    if (!c) return;
    try {
      await toggleSmartClusterEnabled(id, !c.enabled);
      await loadClusters();
      await loadStatus();
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      setError(err?.message || String(err));
    }
  }, [clusters, loadClusters, loadStatus]);

  const handleRename = useCallback(async (id, newName) => {
    try {
      await renameSmartCluster(id, newName);
      await loadClusters();
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      throw err;
    }
  }, [loadClusters]);

  const handleDelete = useCallback((id) => {
    setPendingDeleteId(id);
  }, []);

  const handleConfirmDelete = useCallback(async () => {
    if (!pendingDeleteId) return;
    setDeleteLoading(true);
    try {
      await deleteSmartCluster(pendingDeleteId);
      if (selectedId === pendingDeleteId) setSelectedId(null);
      setPendingDeleteId(null);
      await loadClusters();
      await loadStatus();
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        setPendingDeleteId(null);
        return;
      }
      setError(err?.message || String(err));
    } finally {
      setDeleteLoading(false);
    }
  }, [pendingDeleteId, selectedId, loadClusters, loadStatus]);

  const handleCancelDelete = useCallback(() => {
    if (deleteLoading) return;
    setPendingDeleteId(null);
  }, [deleteLoading]);

  const handleDrainNow = useCallback(async () => {
    setDrainRequested(true);
    try {
      await smartClusterDrainNow();
      await loadStatus();
    } catch (err) {
      setDrainRequested(false);
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      setError(err?.message || String(err));
    }
  }, [loadStatus]);

  const handleStopDrain = useCallback(async () => {
    setDrainRequested(false);
    try {
      await smartClusterStopDrain();
      await loadStatus();
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      setError(err?.message || String(err));
    }
  }, [loadStatus]);

  const handleSaveCalibration = useCallback(async (req) => {
    try {
      await createSmartCluster(req);
      setCreating(false);
      await loadClusters();
      await loadStatus();
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      console.error('Create smart cluster failed:', err);
      throw err;
    }
  }, [loadClusters, loadStatus]);

  const handleOpenSummaryEvidence = useCallback((item, index = 0) => {
    const evidence = formatEvidenceItem(item, index);
    if (!evidence.payload) return;
    const sourceDetail = clusterName(selected) || null;
    const payload = {
      ...evidence.payload,
      sourceLabel: t('smartClusters.aiSummary', 'AI 汇总'),
      sourceDetail,
      sourceType: 'smart-cluster-summary',
    };

    if (onOpenSnapshotPreview) {
      onOpenSnapshotPreview(payload, {
        sourceLabel: t('smartClusters.aiSummary', 'AI 汇总'),
        sourceDetail,
        sourceType: 'smart-cluster-summary',
      });
      return;
    }

    onSelectScreenshot?.(payload);
  }, [onOpenSnapshotPreview, onSelectScreenshot, selected, t]);

  const activeClusters = useMemo(() => clusters.filter((c) => c.enabled), [clusters]);
  const pausedClusters = useMemo(() => clusters.filter((c) => !c.enabled), [clusters]);

  const activeCount = activeClusters.length;
  const pausedCount = pausedClusters.length;
  const pendingCount = statusData?.pending_count ?? workerStatus?.pending_count ?? 0;
  const unverifiableCount = statusData?.unverifiable_thresholds ?? workerStatus?.unverifiableThresholds ?? 0;

  const isDraining = drainRequested
    || Boolean(statusData?.is_force_running)
    || Boolean(statusData?.is_running)
    || Boolean(workerStatus?.forceRunning)
    || Boolean(workerStatus?.running);

  const overview = useMemo(() => {
    let total = 0;
    let recent = 0;
    let latest = null;

    for (const c of clusters) {
      const count = Number(c.assignment_count) || 0;
      const rCount = Number(c.recent_assignment_count) || 0;
      total += count;
      recent += rCount;
      if (c.last_assigned_at) {
        if (!latest || new Date(c.last_assigned_at) > new Date(latest.last_assigned_at)) {
          latest = c;
        }
      }
    }
    return { total, recent, latest };
  }, [clusters]);

  if (creating) {
    return (
      <SmartClusterCreateView
        onSelectScreenshot={onSelectScreenshot}
        onSave={handleSaveCalibration}
        onCancel={() => setCreating(false)}
      />
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden">
      <PageHeader>
        <button
          type="button"
          onClick={() => setCreating(true)}
          className="flex h-[34px] shrink-0 items-center gap-1.5 rounded-md bg-ide-accent px-3.5 text-xs font-medium text-white transition hover:brightness-110"
        >
          <Plus className="h-3.5 w-3.5" />
          {t('smartClusters.newCluster', '新建智能聚类')}
        </button>

        <span className="mx-1 h-5 w-px shrink-0 bg-ide-border" aria-hidden="true" />

        <StatusChip icon={<CircleDot className="h-3 w-3" />} tone="accent">
          {t('smartClusters.chipRunning', '{{count}} 个运行中', {
            count: statusData?.enabled_cluster_count ?? activeCount,
          })}
        </StatusChip>
        {pausedCount > 0 && (
          <StatusChip icon={<PauseCircle className="h-3 w-3" />}>
            {t('smartClusters.chipPaused', '{{count}} 个已暂停', { count: pausedCount })}
          </StatusChip>
        )}
        {pendingCount > 0 && (
          <StatusChip
            icon={<Inbox className="h-3 w-3" />}
            title={t('smartClusters.idleWarning', '后台评分仅在系统空闲时运行')}
            action={isDraining ? (
              <button
                type="button"
                onClick={handleStopDrain}
                title={t('smartClusters.stopDrainTooltip', '停止处理')}
                className="flex h-[22px] items-center gap-1 rounded-full bg-ide-hover px-2 text-[11px] text-ide-muted transition-colors hover:text-ide-text"
              >
                <X className="h-2.5 w-2.5" />
                {t('smartClusters.stopDrain', '停止')}
              </button>
            ) : (
              <button
                type="button"
                onClick={handleDrainNow}
                title={t('smartClusters.processNowTooltip', '立即处理待归档快照队列')}
                className="flex h-[22px] items-center gap-1 rounded-full bg-ide-hover px-2 text-[11px] text-ide-muted transition-colors hover:text-ide-text disabled:opacity-40"
              >
                <Zap className="h-2.5 w-2.5" />
                {t('smartClusters.processNow', '立即处理')}
              </button>
            )}
          >
            {t('smartClusters.chipPending', '{{count}} 张待处理', { count: pendingCount })}
          </StatusChip>
        )}

        <button
          type="button"
          onClick={loadClusters}
          title={t('smartClusters.refresh', '刷新')}
          aria-label={t('smartClusters.refresh', '刷新')}
          className="ml-auto grid h-[30px] w-[30px] shrink-0 place-items-center rounded-md text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
        </button>
      </PageHeader>

      {unverifiableCount > 0 && (
        <div className="mx-6 mt-3 flex shrink-0 items-center gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2">
          <AlertCircle className="h-3.5 w-3.5 shrink-0 text-amber-400" />
          <span className="text-[11px] text-amber-400">
            {t(
              'smartClusters.unverifiableThresholds',
              '{{count}} 个智能聚类的匹配阈值无法在当前模型下生效，需重新创建。',
              { count: unverifiableCount },
            )}
          </span>
        </div>
      )}

      {error && (
        <div className="mx-6 mt-3 flex shrink-0 items-center gap-2 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2">
          <AlertCircle className="h-3.5 w-3.5 shrink-0 text-red-400" />
          <span className="flex-1 break-all text-xs text-red-400">{error}</span>
          <button onClick={() => setError(null)} className="text-red-400 hover:text-red-300">
            <X className="h-3 w-3" />
          </button>
        </div>
      )}

      {/* Content */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* List pane */}
        <div className="w-80 shrink-0 overflow-y-auto border-r border-ide-border custom-scrollbar">
          {loading && !clusters.length ? (
            <div className="flex h-32 items-center justify-center">
              <Loader2 className="h-5 w-5 animate-spin text-ide-muted" />
            </div>
          ) : !clusters.length ? (
            <p className="px-6 py-8 text-center text-[11.5px] leading-relaxed text-ide-muted">
              {t('smartClusters.noClustersHint', '新建智能聚类，程序将自动归档符合描述的快照')}
            </p>
          ) : (
            <>
              {activeClusters.map((c) => (
                <ClusterRow
                  key={c.id}
                  id={c.id}
                  title={clusterName(c)}
                  subtitle={clusterSubtitle(c)}
                  stamp={formatRelativeStamp(c.last_assigned_at || c.updated_at)}
                  accentColor={c.dominant_color || '#6b7280'}
                  count={c.assignment_count ?? 0}
                  recentCount={c.recent_assignment_count ?? 0}
                  selected={selectedId === c.id}
                  onSelect={setSelectedId}
                  onRename={handleRename}
                  onDelete={handleDelete}
                  onTogglePause={handleTogglePause}
                />
              ))}

              {pausedClusters.length > 0 && (
                <>
                  <div className="px-3 pt-3 pb-1 text-[11px] font-medium text-ide-muted">
                    {t('smartClusters.pausedGroup', '已暂停 {{count}} 个', { count: pausedClusters.length })}
                  </div>
                  {pausedClusters.map((c) => (
                    <ClusterRow
                      key={c.id}
                      id={c.id}
                      title={clusterName(c)}
                      subtitle={clusterSubtitle(c)}
                      stamp={formatRelativeStamp(c.last_assigned_at || c.updated_at)}
                      accentColor={c.dominant_color || '#6b7280'}
                      count={c.assignment_count ?? 0}
                      recentCount={c.recent_assignment_count ?? 0}
                      paused
                      selected={selectedId === c.id}
                      onSelect={setSelectedId}
                      onRename={handleRename}
                      onDelete={handleDelete}
                      onTogglePause={handleTogglePause}
                    />
                  ))}
                </>
              )}
            </>
          )}
        </div>

        {/* Detail pane */}
        <div className="relative flex-1 min-h-0 overflow-hidden">
          <div className="h-full overflow-y-auto">
            {selected ? (
              <div className="p-4 space-y-3">
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2 text-sm font-medium text-ide-text">
                      <span
                        className="h-2 w-2 rounded-[3px]"
                        style={{ backgroundColor: selected.dominant_color || '#6b7280' }}
                        aria-hidden="true"
                      />
                      <span className="truncate">{clusterName(selected)}</span>
                      {!selected.enabled && (
                        <span className="rounded border border-ide-border bg-ide-bg px-1.5 py-0.5 text-[10px] text-ide-muted">
                          {t('smartClusters.paused', '已暂停')}
                        </span>
                      )}
                    </div>
                    <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-ide-muted">
                      <span>
                        {t('smartClusters.archived', '已归档')}
                        <span className="mx-1 font-mono text-ide-text">{selected.assignment_count ?? 0}</span>
                        {t('smartClusters.snapshotsCount', '张快照')}
                      </span>
                      {selected.display_name && selected.display_name !== selected.anchor_text && (
                        <span className="truncate" title={selected.anchor_text}>
                          {t('smartClusters.collecting', '聚类规则：{{text}}', { text: selected.anchor_text })}
                        </span>
                      )}
                    </div>
                  </div>
                </div>

                {selectedSummary && (
                  <section className="rounded-md border border-ide-border bg-ide-panel/60 p-4 space-y-4">
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="flex items-center gap-2 text-[15px] font-bold leading-6 text-ide-text">
                          <FileText className="w-4 h-4 text-ide-accent" />
                          <span>{selectedSummary.title || t('smartClusters.aiSummary', 'AI 汇总')}</span>
                        </div>
                        <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-ide-muted">
                          {selectedSummary.model_name && (
                            <span>{selectedSummary.model_provider ? `${selectedSummary.model_provider} · ` : ''}{selectedSummary.model_name}</span>
                          )}
                          {selectedSummary.source_snapshot_count !== null && selectedSummary.source_snapshot_count !== undefined && (
                            <span>{t('smartClusters.summarySources', '{{count}} 张来源快照', { count: selectedSummary.source_snapshot_count })}</span>
                          )}
                          {selectedSummary.updated_at && (
                            <span>{t('smartClusters.summaryUpdatedAt', '汇总于 {{time}}', { time: formatTimestamp(selectedSummary.updated_at) })}</span>
                          )}
                        </div>
                      </div>
                    </div>

                    {selectedSummary.summary && (
                      <MarkdownText
                        text={selectedSummary.summary}
                        evidenceItems={selectedEvidence}
                        onOpenCitation={handleOpenSummaryEvidence}
                        className="space-y-2.5 text-[13px] font-normal leading-[1.65] text-ide-text/90"
                      />
                    )}

                    {selectedSummary.ocr_summary && (
                      <div className="space-y-2">
                        <div className="text-xs font-semibold text-ide-muted">
                          {t('smartClusters.ocrSummary', '文字内容概述')}
                        </div>
                        <MarkdownText
                          text={selectedSummary.ocr_summary}
                          evidenceItems={selectedEvidence}
                          onOpenCitation={handleOpenSummaryEvidence}
                          className="space-y-2.5 text-[13px] font-normal leading-[1.65] text-ide-text/85"
                        />
                      </div>
                    )}

                    {selectedKeyPoints.length > 0 && (
                      <div className="space-y-2">
                        <div className="text-xs font-semibold text-ide-muted">
                          {t('smartClusters.keyPoints', '要点')}
                        </div>
                        <ul className="space-y-1.5 text-[13px] font-normal leading-[1.6] text-ide-text/85">
                          {selectedKeyPoints.map((item, idx) => {
                            const text = formatKeyPoint(item);
                            return text ? (
                              <li key={idx} className="flex gap-2">
                                <span className="mt-2.5 h-1.5 w-1.5 shrink-0 rounded-full bg-ide-muted/70" />
                                <span className="min-w-0 break-words">
                                  <MarkdownInline text={text} evidenceItems={selectedEvidence} onOpenCitation={handleOpenSummaryEvidence} />
                                </span>
                              </li>
                            ) : null;
                          })}
                        </ul>
                      </div>
                    )}

                    {selectedEvidence.length > 0 && (
                      <details className="group">
                        <summary className="cursor-pointer select-none text-xs font-semibold text-ide-muted hover:text-ide-text">
                          {t('smartClusters.evidence', '来源')}
                        </summary>
                        <ul className="mt-2 space-y-1 text-[11px] text-ide-muted">
                          {selectedEvidence.map((item, idx) => {
                            const evidence = formatEvidenceItem(item, idx);
                            const label = evidence.text || t('smartClusters.evidenceSnapshot', '来源快照');
                            const content = (
                              <>
                                <span className="inline-flex h-4 min-w-4 items-center justify-center rounded border border-ide-border bg-ide-bg px-1 text-[10px] text-ide-muted">
                                  {evidence.ref}
                                </span>
                                {evidence.snapshotId && (
                                  <span className="font-mono text-ide-accent">#{evidence.snapshotId}</span>
                                )}
                                <span className="min-w-0 break-words">{label}</span>
                              </>
                            );
                            return (
                              <li key={idx}>
                                {evidence.payload ? (
                                  <button
                                    type="button"
                                    onClick={() => handleOpenSummaryEvidence(item, idx)}
                                    className="flex w-full items-start gap-2 rounded px-1 py-0.5 text-left hover:bg-ide-hover/30 hover:text-ide-text"
                                    title={evidence.snapshotId ? `#${evidence.snapshotId}` : undefined}
                                  >
                                    {content}
                                  </button>
                                ) : (
                                  <div className="flex items-start gap-2 px-1 py-0.5">
                                    {content}
                                  </div>
                                )}
                              </li>
                            );
                          })}
                        </ul>
                      </details>
                    )}
                  </section>
                )}

                {/* Assignments */}
                {assignments.length === 0 ? (
                  <div className="flex flex-col items-center justify-center h-40 text-sm text-ide-muted gap-2">
                    <Clock className="w-6 h-6 opacity-40" />
                    <span>{t('smartClusters.noAssignments', '暂无归档快照')}</span>
                    <span className="text-[11px] opacity-70">
                      {t('smartClusters.idleProcessingHint', '系统空闲时将自动处理待评分快照')}
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
                        }}
                        preloadedSrc={thumbnailCache[s.screenshot_id] || null}
                        onSelect={(payload) => {
                          onSelectScreenshot?.({ ...payload, assigned_at: s.assigned_at });
                        }}
                        onOpenFloatingPreview={onOpenSnapshotPreview
                          ? (payload) => {
                            onOpenSnapshotPreview({ ...payload, assigned_at: s.assigned_at }, {
                              thumbnailSrc: thumbnailCache[s.screenshot_id] || null,
                              sourceLabel: t('smartClusters.title', '智能聚类'),
                              sourceDetail: clusterName(selected),
                              sourceType: 'smart-cluster',
                            });
                          }
                          : undefined}
                        footerPersistent={false}
                      />
                    ))}
                  </div>
                )}
              </div>
            ) : (
              <div className="flex h-full items-center justify-center px-8">
                <div className="w-full max-w-md">
                  {clusters.length === 0 ? (
                    <div className="text-center">
                      <Sparkles className="mx-auto h-9 w-9 text-ide-muted/40" />
                      <p className="mt-4 text-sm font-medium text-ide-text">
                        {t('smartClusters.noClusters', '暂无智能聚类')}
                      </p>
                      <p className="mx-auto mt-2 max-w-xs text-[12px] leading-relaxed text-ide-muted">
                        {t('smartClusters.overviewEmptyHint', '输入自然语言描述创建聚类，程序将自动匹配并归档历史与新捕获的快照。')}
                      </p>
                      <button
                        type="button"
                        onClick={() => setCreating(true)}
                        className="mx-auto mt-5 flex h-[34px] items-center gap-1.5 rounded-md bg-ide-accent px-4 text-xs font-medium text-white transition hover:brightness-110"
                      >
                        <Plus className="h-3.5 w-3.5" />
                        {t('smartClusters.newCluster', '新建智能聚类')}
                      </button>
                    </div>
                  ) : (
                    <>
                      <div className="flex items-baseline gap-2">
                        <span className="font-mono text-[34px] leading-none tabular-nums text-ide-text">
                          {overview.total}
                        </span>
                        <span className="text-[12px] text-ide-muted">
                          {t('smartClusters.overviewTotal', '张快照已归档至 {{count}} 个聚类', {
                            count: clusters.length,
                          })}
                        </span>
                      </div>

                      <dl className="mt-6 space-y-3 border-t border-ide-border pt-4 text-[12px]">
                        <div className="flex items-baseline justify-between gap-4">
                          <dt className="text-ide-muted">{t('smartClusters.overviewRecent', '本周新增')}</dt>
                          <dd className="font-mono tabular-nums text-ide-text">{overview.recent}</dd>
                        </div>
                        {overview.latest && (
                          <div className="flex items-baseline justify-between gap-4">
                            <dt className="shrink-0 text-ide-muted">
                              {t('smartClusters.overviewLatest', '最近归档')}
                            </dt>
                            <dd className="flex min-w-0 items-center gap-2 text-ide-text">
                              <span
                                className="h-2 w-2 shrink-0 rounded-[3px]"
                                style={{ backgroundColor: overview.latest.dominant_color || '#6b7280' }}
                                aria-hidden="true"
                              />
                              <span className="truncate" title={clusterName(overview.latest)}>
                                {clusterName(overview.latest)}
                              </span>
                              <span className="shrink-0 font-mono text-[11px] text-ide-muted">
                                {formatRelativeStamp(overview.latest.last_assigned_at)}
                              </span>
                            </dd>
                          </div>
                        )}
                        {pendingCount > 0 && (
                          <div className="flex items-baseline justify-between gap-4">
                            <dt className="text-ide-muted">{t('smartClusters.overviewPending', '待处理')}</dt>
                            <dd className="font-mono tabular-nums text-ide-text">{pendingCount}</dd>
                          </div>
                        )}
                      </dl>

                      <p className="mt-6 text-[12px] text-ide-muted">
                        {t('smartClusters.selectClusterToView', '选择左侧聚类查看归档快照')}
                      </p>
                    </>
                  )}
                </div>
              </div>
            )}
          </div>

        </div>
      </div>
      <ConfirmDialog
        isOpen={Boolean(pendingDeleteId)}
        onCancel={handleCancelDelete}
        onConfirm={handleConfirmDelete}
        title={t('smartClusters.confirmDeleteTitle', '删除智能聚类？')}
        message={t('smartClusters.confirmDelete', '确定删除此智能聚类？已归档的快照不会被删除。')}
        confirmLabel={t('common.confirm', '删除')}
        cancelLabel={t('common.cancel', '取消')}
        confirmVariant="danger"
        loading={deleteLoading}
        loadingLabel={t('common.processing', '处理中…')}
      />
    </div>
  );
}
