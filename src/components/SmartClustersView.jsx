import React, { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Sparkles, Plus, Loader2, RefreshCw, AlertCircle, X,
  Zap, Image as ImageIcon, Clock, Hash, FileText,
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

function isAuthRequiredError(err) {
  return String(err?.message || err || '').includes('AUTH_REQUIRED');
}

function normalizeSummaryList(value) {
  if (!value) return [];
  if (Array.isArray(value)) return value;
  if (typeof value === 'object') return Object.values(value);
  return [value];
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
          </a>
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
          </button>
        );
      } else {
        parts.push(token);
      }
    } else if (token.startsWith('`')) {
      parts.push(
        <code key={`code-${match.index}`} className="rounded bg-ide-bg px-1 py-0.5 font-mono text-[0.92em] text-ide-accent">
          {token.slice(1, -1)}
        </code>
      );
    } else if (token.startsWith('**')) {
      parts.push(<strong key={`bold-${match.index}`} className="font-bold text-ide-text">{token.slice(2, -2)}</strong>);
    } else if (token.startsWith('*')) {
      parts.push(<em key={`em-${match.index}`} className="italic">{token.slice(1, -1)}</em>);
    } else {
      parts.push(token);
    }
    lastIndex = pattern.lastIndex;
  }

  if (lastIndex < text.length) {
    parts.push(text.slice(lastIndex));
  }

  return <>{parts}</>;
}

function parseMarkdownBlocks(text) {
  const lines = String(text || '').replace(/\r\n/g, '\n').split('\n');
  const blocks = [];
  let i = 0;

  while (i < lines.length) {
    const raw = lines[i];
    const line = raw.trim();
    if (!line) {
      i += 1;
      continue;
    }

    const fence = line.match(/^```(\w+)?\s*$/);
    if (fence) {
      const codeLines = [];
      i += 1;
      while (i < lines.length && !lines[i].trim().startsWith('```')) {
        codeLines.push(lines[i]);
        i += 1;
      }
      if (i < lines.length) i += 1;
      blocks.push({ type: 'code', text: codeLines.join('\n') });
      continue;
    }

    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      blocks.push({ type: 'heading', level: heading[1].length, text: heading[2].trim() });
      i += 1;
      continue;
    }

    const unordered = line.match(/^[-*]\s+(.+)$/);
    if (unordered) {
      const items = [];
      while (i < lines.length) {
        const item = lines[i].trim().match(/^[-*]\s+(.+)$/);
        if (!item) break;
        items.push(item[1].trim());
        i += 1;
      }
      blocks.push({ type: 'ul', items });
      continue;
    }

    const ordered = line.match(/^\d+\.\s+(.+)$/);
    if (ordered) {
      const items = [];
      while (i < lines.length) {
        const item = lines[i].trim().match(/^\d+\.\s+(.+)$/);
        if (!item) break;
        items.push(item[1].trim());
        i += 1;
      }
      blocks.push({ type: 'ol', items });
      continue;
    }

    const paragraph = [];
    while (i < lines.length) {
      const current = lines[i].trim();
      if (!current) break;
      if (/^```/.test(current) || /^(#{1,3})\s+/.test(current) || /^[-*]\s+/.test(current) || /^\d+\.\s+/.test(current)) break;
      paragraph.push(current);
      i += 1;
    }
    blocks.push({ type: 'p', text: paragraph.join(' ') });
  }

  return blocks;
}

function MarkdownText({ text, evidenceItems, onOpenCitation, className }) {
  if (!text) return null;
  const blocks = parseMarkdownBlocks(text);

  return (
    <div className={className}>
      {blocks.map((block, idx) => {
        if (block.type === 'heading') {
          const headingClass = block.level === 1
            ? 'text-[15px] font-bold text-ide-text'
            : block.level === 2
              ? 'text-[14px] font-bold text-ide-text'
              : 'text-[13px] font-semibold text-ide-text';
          return (
            <div key={idx} className={headingClass}>
              <MarkdownInline text={block.text} evidenceItems={evidenceItems} onOpenCitation={onOpenCitation} />
            </div>
          );
        }
        if (block.type === 'ul' || block.type === 'ol') {
          const ListTag = block.type;
          return (
            <ListTag key={idx} className={`space-y-1 pl-5 ${block.type === 'ul' ? 'list-disc' : 'list-decimal'}`}>
              {block.items.map((item, itemIdx) => (
                <li key={itemIdx} className="break-words">
                  <MarkdownInline text={item} evidenceItems={evidenceItems} onOpenCitation={onOpenCitation} />
                </li>
              ))}
            </ListTag>
          );
        }
        if (block.type === 'code') {
          return (
            <pre key={idx} className="overflow-x-auto rounded-md border border-ide-border bg-ide-bg p-3 text-xs leading-5 text-ide-text">
              <code>{block.text}</code>
            </pre>
          );
        }
        return (
          <p key={idx} className="break-words">
            <MarkdownInline text={block.text} evidenceItems={evidenceItems} onOpenCitation={onOpenCitation} />
          </p>
        );
      })}
    </div>
  );
}

function emitAuthRequired() {
  if (typeof window !== 'undefined') {
    window.dispatchEvent(new CustomEvent('cp-auth-required'));
  }
}

export default function SmartClustersView({
  backendOnline,
  isAuthenticated = true,
  active = true,
  onSelectScreenshot,
  onOpenSnapshotPreview,
}) {
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
  const selectedSummary = selected?.summary || null;
  const selectedKeyPoints = normalizeSummaryList(selectedSummary?.key_points);
  const selectedEvidence = normalizeSummaryList(selectedSummary?.evidence);

  const loadClusters = useCallback(async () => {
    if (!active || !isAuthenticated) {
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const result = await listSmartClusters();
      setClusters(result || []);
      setError(null);
    } catch (err) {
      if (isAuthRequiredError(err)) {
        setError(null);
        emitAuthRequired();
        return;
      }
      console.error('Failed to load smart clusters:', err);
      setError(err?.message || String(err));
    } finally {
      setLoading(false);
    }
  }, [active, isAuthenticated]);

  const loadStatus = useCallback(async () => {
    if (!active || !backendOnline || !isAuthenticated) return;
    try {
      const s = await getSmartClusterStatus();
      const w = await getSmartClusterWorkerStatus();
      setStatusData({
        ...s,
        is_running: w.running && w.pending_count > 0,
        is_force_running: w.forceRunning && w.pending_count > 0,
      });
    } catch (err) {
      if (isAuthRequiredError(err)) emitAuthRequired();
    }
  }, [active, backendOnline, isAuthenticated]);

  const loadAssignments = useCallback(async (id) => {
    if (!active || !isAuthenticated || !id) { setAssignments([]); return; }
    try {
      const result = await getSmartClusterAssignments(id, 0, 100);
      setAssignments(result || []);
    } catch (err) {
      if (isAuthRequiredError(err)) {
        emitAuthRequired();
        return;
      }
      console.error('Failed to load assignments:', err);
    }
  }, [active, isAuthenticated]);

  // Polling helper
  const handlePoll = useCallback(async () => {
    if (!active || !isAuthenticated) return;
    await loadStatus();
    try {
      const result = await listSmartClusters();
      setClusters(result || []);
    } catch (err) {
      if (isAuthRequiredError(err)) emitAuthRequired();
    }
    if (selectedId) {
      await loadAssignments(selectedId);
    }
  }, [active, isAuthenticated, loadStatus, selectedId, loadAssignments]);

  useEffect(() => {
    if (!active || !isAuthenticated) {
      setLoading(false);
      setError(null);
      return undefined;
    }
    loadClusters();
    loadStatus();
    const interval = setInterval(handlePoll, 10000);
    return () => clearInterval(interval);
  }, [active, isAuthenticated, loadClusters, loadStatus, handlePoll]);

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
      if (isAuthRequiredError(err)) {
        setError(null);
        emitAuthRequired();
        return;
      }
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
      if (isAuthRequiredError(err)) {
        setError(null);
        emitAuthRequired();
        return;
      }
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
      if (isAuthRequiredError(err)) {
        setError(null);
        emitAuthRequired();
        return;
      }
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
    const payload = {
      ...evidence.payload,
      sourceLabel: t('smartClusters.aiSummary', 'AI 汇总'),
      sourceDetail: selected?.anchor_text || null,
      sourceType: 'smart-cluster-summary',
    };

    if (onOpenSnapshotPreview) {
      onOpenSnapshotPreview(payload, {
        sourceLabel: t('smartClusters.aiSummary', 'AI 汇总'),
        sourceDetail: selected?.anchor_text || null,
        sourceType: 'smart-cluster-summary',
      });
      return;
    }

    onSelectScreenshot?.(payload);
  }, [onOpenSnapshotPreview, onSelectScreenshot, selected?.anchor_text, t]);

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
            {t('smartClusters.idleWarning', '后台工作线程仅在系统空闲时运行')}
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
                          {t('smartClusters.ocrSummary', 'OCR 统合概述')}
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
                          {t('smartClusters.evidence', '证据')}
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
