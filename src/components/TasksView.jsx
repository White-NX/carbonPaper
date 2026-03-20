import React, { useState, useEffect, useCallback, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Layers, Play, Loader2, Calendar, Clock, Trash2, Pencil, Check, X,
  Merge, ChevronDown, RefreshCw, Snowflake, Flame, Image as ImageIcon,
  Eye, EyeOff, Gamepad2, MessageCircle,
} from 'lucide-react';
import { getTasks, getTaskScreenshots, deleteTask, updateTaskLabel, mergeTasks, runClustering, getClusteringStatus, saveClusteringResults } from '../lib/task_api';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { CATEGORY_COLORS, ENTERTAINMENT_CATEGORIES, SOCIAL_CATEGORIES } from '../lib/categories';
import { ThumbnailCard } from './ThumbnailCard';

// ── Helpers ────────────────────────────────────────────────────────────

function formatTimestamp(ts) {
  if (!ts) return '—';
  const d = new Date(ts * 1000);
  return d.toLocaleString();
}

function formatDuration(startTs, endTs) {
  if (!startTs || !endTs) return '—';
  const secs = Math.abs(endTs - startTs);
  if (secs < 60) return `${Math.round(secs)}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  if (secs < 86400) return `${(secs / 3600).toFixed(1)}h`;
  return `${(secs / 86400).toFixed(1)}d`;
}

// ── TaskCard ───────────────────────────────────────────────────────────

function TaskCard({ task, selected, onSelect, onRename, onDelete, mergeable, onToggleMerge, mergeChecked }) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [editLabel, setEditLabel] = useState('');
  const inputRef = useRef(null);

  const label = task.label || task.auto_label || t('tasks.unnamed');
  const catColor = CATEGORY_COLORS[task.dominant_category] || '#6b7280';
  const catLabel = task.dominant_category || null;

  const handleStartEdit = (e) => {
    e.stopPropagation();
    setEditLabel(label);
    setEditing(true);
    setTimeout(() => inputRef.current?.focus(), 50);
  };

  const handleSave = async (e) => {
    e.stopPropagation();
    if (editLabel.trim()) {
      await onRename(task.id, editLabel.trim());
    }
    setEditing(false);
  };

  const handleCancel = (e) => {
    e.stopPropagation();
    setEditing(false);
  };

  return (
    <div
      onClick={() => onSelect(task)}
      className={`group relative p-3 rounded-xl border cursor-pointer transition-all duration-150 ${
        selected
          ? 'bg-ide-accent/10 border-ide-accent/40 shadow-sm'
          : 'bg-ide-bg border-ide-border hover:bg-ide-hover hover:border-ide-border/60'
      }`}
    >
      {/* Category color bar */}
      <div
        className="absolute left-0 top-2 bottom-2 w-1 rounded-r"
        style={{ backgroundColor: catColor }}
      />

      <div className="pl-3 space-y-1.5">
        {/* Header row */}
        <div className="flex items-center gap-2">
          {mergeable && (
            <input
              type="checkbox"
              checked={mergeChecked}
              onClick={(e) => e.stopPropagation()}
              onChange={() => onToggleMerge(task.id)}
              className="w-3.5 h-3.5 rounded accent-ide-accent shrink-0"
            />
          )}
          {editing ? (
            <div className="flex items-center gap-1 flex-1 min-w-0">
              <input
                ref={inputRef}
                value={editLabel}
                onChange={(e) => setEditLabel(e.target.value)}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleSave(e);
                  if (e.key === 'Escape') handleCancel(e);
                }}
                className="flex-1 px-1.5 py-0.5 text-sm bg-ide-bg border border-ide-accent rounded text-ide-text focus:outline-none min-w-0"
              />
              <button onClick={handleSave} className="p-0.5 hover:bg-ide-hover rounded"><Check className="w-3.5 h-3.5 text-green-400" /></button>
              <button onClick={handleCancel} className="p-0.5 hover:bg-ide-hover rounded"><X className="w-3.5 h-3.5 text-red-400" /></button>
            </div>
          ) : (
            <span className="text-sm font-medium text-ide-text truncate flex-1">
              {label}
              {catLabel && (
                <span
                  className="ml-1.5 inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-normal align-middle"
                  style={{ backgroundColor: catColor + '33', color: catColor }}
                >
                  {catLabel}
                </span>
              )}
            </span>
          )}
          {!editing && (
            <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
              <button onClick={handleStartEdit} className="p-1 hover:bg-ide-hover rounded" title={t('tasks.rename')}>
                <Pencil className="w-3 h-3 text-ide-muted" />
              </button>
              <button
                onClick={(e) => { e.stopPropagation(); onDelete(task.id); }}
                className="p-1 hover:bg-ide-hover rounded"
                title={t('tasks.delete')}
              >
                <Trash2 className="w-3 h-3 text-ide-muted" />
              </button>
            </div>
          )}
        </div>

        {/* Meta row */}
        <div className="flex items-center gap-3 text-xs text-ide-muted">
          <span className="flex items-center gap-1">
            {task.layer === 'cold' ? <Snowflake className="w-3 h-3" /> : <Flame className="w-3 h-3" />}
            {task.layer}
          </span>
          <span className="flex items-center gap-1">
            <ImageIcon className="w-3 h-3" />
            {task.snapshot_count}
          </span>
          <span className="flex items-center gap-1">
            <Clock className="w-3 h-3" />
            {formatDuration(task.start_time, task.end_time)}
          </span>
        </div>

        {/* Time range */}
        <div className="text-[11px] text-ide-muted/60 truncate">
          {formatTimestamp(task.start_time)} — {formatTimestamp(task.end_time)}
        </div>
      </div>
    </div>
  );
}

// ── Main TasksView ─────────────────────────────────────────────────────

export default function TasksView({ backendOnline, onSelectScreenshot }) {
  const { t } = useTranslation();
  const [tasks, setTasks] = useState([]);
  const [selectedTask, setSelectedTask] = useState(null);
  const [screenshots, setScreenshots] = useState([]);
  const [loading, setLoading] = useState(false);
  const [clusteringRunning, setClusteringRunning] = useState(false);
  const [clusteringError, setClusteringError] = useState(null);
  const [mergeMode, setMergeMode] = useState(false);
  const [mergeSelection, setMergeSelection] = useState(new Set());
  const [clusteringStatus, setClusteringStatus] = useState(null);

  // Filter toggles
  const [showInactive, setShowInactive] = useState(false);
  const [showEntertainment, setShowEntertainment] = useState(false);
  const [showSocial, setShowSocial] = useState(false);

  // Date range for manual clustering
  const [rangeStart, setRangeStart] = useState('');
  const [rangeEnd, setRangeEnd] = useState('');

  // ── data loading ──

  const loadTasks = useCallback(async () => {
    setLoading(true);
    try {
      const result = await getTasks({
        hideInactive: !showInactive,
        hideEntertainment: !showEntertainment,
        hideSocial: !showSocial,
      });
      setTasks(result || []);
    } catch (err) {
      console.error('Failed to load tasks:', err);
    } finally {
      setLoading(false);
    }
  }, [showInactive, showEntertainment, showSocial]);

  const loadStatus = useCallback(async () => {
    if (!backendOnline) return;
    try {
      const result = await getClusteringStatus();
      if (result?.status === 'success') {
        setClusteringStatus(result);
      }
    } catch { /* ignore */ }
  }, [backendOnline]);

  useEffect(() => {
    loadTasks();
    loadStatus();
  }, [loadTasks, loadStatus]);

  useEffect(() => {
    if (!selectedTask) {
      setScreenshots([]);
      return;
    }
    (async () => {
      try {
        const result = await getTaskScreenshots(selectedTask.id);
        setScreenshots(result || []);
      } catch (err) {
        console.error('Failed to load task screenshots:', err);
      }
    })();
  }, [selectedTask]);

  // Batch-load thumbnails when screenshots change
  const [thumbnailCache, setThumbnailCache] = useState({});
  useEffect(() => {
    if (!screenshots.length) { setThumbnailCache({}); return; }
    let active = true;
    const ids = [...new Set(screenshots
      .map(s => s.screenshot_id)
      .filter(id => typeof id === 'number' && id > 0))];
    if (ids.length === 0) return;
    fetchThumbnailBatch(ids)
      .then(batch => {
        if (active && batch) setThumbnailCache(batch);
      })
      .catch(err => console.error('Failed to batch load thumbnails:', err));
    return () => { active = false; };
  }, [screenshots]);

  // ── actions ──

  const handleRunClustering = async () => {
    setClusteringRunning(true);
    setClusteringError(null);
    try {
      const options = {};
      if (rangeStart) options.startTime = new Date(rangeStart).getTime() / 1000;
      if (rangeEnd) options.endTime = new Date(rangeEnd).getTime() / 1000;
      const result = await runClustering(options);
      if (result?.status === 'empty') {
        setClusteringError(t('tasks.noData'));
      }

      // Persist clustering results to Rust SQLite so loadTasks() can find them
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
        try {
          await saveClusteringResults(taskRequests);
        } catch (saveErr) {
          console.error('Failed to save clustering results to DB:', saveErr);
        }
      }

      await loadTasks();
      await loadStatus();
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

  const handleDelete = async (taskId) => {
    try {
      await deleteTask(taskId);
      if (selectedTask?.id === taskId) setSelectedTask(null);
      await loadTasks();
    } catch (err) {
      console.error('Failed to delete task:', err);
    }
  };

  const handleRename = async (taskId, label) => {
    try {
      await updateTaskLabel(taskId, label);
      await loadTasks();
    } catch (err) {
      console.error('Failed to rename task:', err);
    }
  };

  const handleMerge = async () => {
    const ids = Array.from(mergeSelection);
    if (ids.length < 2) return;
    try {
      await mergeTasks(ids);
      setMergeMode(false);
      setMergeSelection(new Set());
      setSelectedTask(null);
      await loadTasks();
    } catch (err) {
      console.error('Failed to merge tasks:', err);
    }
  };

  const toggleMerge = (id) => {
    setMergeSelection((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const lastRunLabel = clusteringStatus?.config?.last_run
    ? new Date(clusteringStatus.config.last_run * 1000).toLocaleString()
    : t('tasks.never');

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* ── Toolbar ── */}
      <div className="shrink-0 border-b border-ide-border bg-ide-panel px-4 py-2.5 space-y-2">
        <div className="flex items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-ide-text flex items-center gap-2">
            <Layers className="w-4 h-4 text-ide-accent" />
            {t('tasks.title')}
            <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
          </h2>
          <div className="flex items-center gap-1.5">
            {mergeMode && (
              <>
                <button
                  onClick={handleMerge}
                  disabled={mergeSelection.size < 2}
                  className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-accent bg-ide-accent/20 text-ide-accent hover:bg-ide-accent/30 disabled:opacity-40 transition-colors"
                >
                  <Merge className="w-3 h-3" />
                  {t('tasks.merge')} ({mergeSelection.size})
                </button>
                <button
                  onClick={() => { setMergeMode(false); setMergeSelection(new Set()); }}
                  className="px-3 py-1.5 text-xs rounded border border-ide-border hover:bg-ide-hover/30 transition-colors text-ide-muted"
                >
                  {t('tasks.cancel')}
                </button>
              </>
            )}
            {!mergeMode && tasks.length > 1 && (
              <button
                onClick={() => setMergeMode(true)}
                className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-border hover:bg-ide-hover/30 transition-colors text-ide-muted"
              >
                <Merge className="w-3 h-3" />
                {t('tasks.mergeMode')}
              </button>
            )}
            <button
              onClick={loadTasks}
              className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-border text-ide-muted hover:text-ide-text hover:bg-ide-hover/30 transition-colors"
              title={t('tasks.refresh')}
            >
              <RefreshCw className={`w-3.5 h-3.5 text-ide-muted ${loading ? 'animate-spin' : ''}`} />
              {t('tasks.refresh')}
            </button>
          </div>
        </div>

        {/* ── Clustering controls ── */}
        <div className="flex items-center gap-2 flex-wrap">
          <input
            type="date"
            value={rangeStart}
            onChange={(e) => setRangeStart(e.target.value)}
            className="px-2 py-1 text-xs bg-ide-bg border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
          />
          <span className="text-xs text-ide-muted">—</span>
          <input
            type="date"
            value={rangeEnd}
            onChange={(e) => setRangeEnd(e.target.value)}
            className="px-2 py-1 text-xs bg-ide-bg border border-ide-border rounded-lg text-ide-text focus:outline-none focus:border-ide-accent"
          />
          <button
            onClick={handleRunClustering}
            disabled={clusteringRunning || !backendOnline}
            className="flex items-center gap-1 px-3 py-1.5 text-xs rounded border border-ide-accent bg-ide-accent/20 text-ide-accent hover:bg-ide-accent/30 disabled:opacity-40 transition-colors"
          >
            {clusteringRunning ? <Loader2 className="w-3 h-3 animate-spin" /> : <Play className="w-3 h-3" />}
            {t('tasks.runClustering')}
          </button>
          <span className="text-[11px] text-ide-muted ml-auto">
            {t('tasks.lastRun')}: {lastRunLabel}
          </span>
        </div>

        {/* ── Filter toggles ── */}
        <div className="flex items-center gap-2 flex-wrap">
          <button
            onClick={() => setShowInactive((v) => !v)}
            className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border transition-colors ${
              showInactive
                ? 'bg-ide-accent/15 border-ide-accent/40 text-ide-accent'
                : 'bg-ide-bg border-ide-border text-ide-muted hover:bg-ide-hover/30'
            }`}
            title={showInactive ? t('tasks.hideInactive') : t('tasks.showInactive')}
          >
            {showInactive ? <Eye className="w-3 h-3" /> : <EyeOff className="w-3 h-3" />}
            {t('tasks.inactiveTasks')}
          </button>
          <button
            onClick={() => setShowEntertainment((v) => !v)}
            className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border transition-colors ${
              showEntertainment
                ? 'bg-ide-accent/15 border-ide-accent/40 text-ide-accent'
                : 'bg-ide-bg border-ide-border text-ide-muted hover:bg-ide-hover/30'
            }`}
            title={showEntertainment ? t('tasks.hideEntertainment') : t('tasks.showEntertainment')}
          >
            <Gamepad2 className="w-3 h-3" />
            {t('tasks.entertainmentTasks')}
          </button>
          <button
            onClick={() => setShowSocial((v) => !v)}
            className={`flex items-center gap-1 px-3 py-1.5 text-xs rounded border transition-colors ${
              showSocial
                ? 'bg-ide-accent/15 border-ide-accent/40 text-ide-accent'
                : 'bg-ide-bg border-ide-border text-ide-muted hover:bg-ide-hover/30'
            }`}
            title={showSocial ? t('tasks.hideSocial') : t('tasks.showSocial')}
          >
            <MessageCircle className="w-3 h-3" />
            {t('tasks.socialTasks')}
          </button>
        </div>
        {clusteringError && (
          <div className="flex items-center gap-2 px-2.5 py-1.5 bg-red-500/10 border border-red-500/30 rounded-lg">
            <X className="w-3.5 h-3.5 text-red-400 shrink-0 cursor-pointer" onClick={() => setClusteringError(null)} />
            <span className="text-xs text-red-400">{clusteringError}</span>
          </div>
        )}
      </div>

      {/* ── Content ── */}
      <div className="flex-1 flex min-h-0 overflow-hidden">
        {/* Task list panel */}
        <div className="w-72 shrink-0 border-r border-ide-border overflow-y-auto p-2 space-y-1.5">
          {loading && !tasks.length ? (
            <div className="flex items-center justify-center h-32">
              <Loader2 className="w-5 h-5 animate-spin text-ide-muted" />
            </div>
          ) : tasks.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 text-sm text-ide-muted gap-2">
              <Layers className="w-6 h-6 opacity-40" />
              <span>{t('tasks.empty')}</span>
              <span className="text-[11px]">{t('tasks.emptyHint')}</span>
            </div>
          ) : (
            tasks.map((task) => (
              <TaskCard
                key={task.id}
                task={task}
                selected={selectedTask?.id === task.id}
                onSelect={setSelectedTask}
                onRename={handleRename}
                onDelete={handleDelete}
                mergeable={mergeMode}
                mergeChecked={mergeSelection.has(task.id)}
                onToggleMerge={toggleMerge}
              />
            ))
          )}
        </div>

        {/* Detail panel */}
        <div className="flex-1 overflow-y-auto">
          {selectedTask ? (
            <div className="p-4 space-y-3">
              <div className="flex items-center gap-2 text-sm font-medium text-ide-text">
                <div
                  className="w-2 h-2 rounded-full"
                  style={{ backgroundColor: CATEGORY_COLORS[selectedTask.dominant_category] || '#6b7280' }}
                />
                {selectedTask.label || selectedTask.auto_label || t('tasks.unnamed')}
                <span className="text-xs text-ide-muted font-normal ml-2">
                  {selectedTask.dominant_process || '—'} · {selectedTask.snapshot_count} {t('tasks.snapshots')}
                </span>
              </div>
              {screenshots.length === 0 ? (
                <div className="flex items-center justify-center h-32 text-sm text-ide-muted">
                  {t('tasks.noScreenshots')}
                </div>
              ) : (
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
                  {screenshots.map((s, index) => (
                    <ThumbnailCard
                      key={s.screenshot_id || index}
                      item={{
                        screenshot_id: s.screenshot_id,
                        image_path: s.image_path,
                        process_name: s.process_name,
                        window_title: s.window_title,
                        category: s.category,
                        created_at: s.created_at,
                      }}
                      preloadedSrc={thumbnailCache[s.screenshot_id] || null}
                      onSelect={(payload) => onSelectScreenshot?.(payload)}
                    />
                  ))}
                </div>
              )}
            </div>
          ) : (
            <div className="flex items-center justify-center h-full text-sm text-ide-muted">
              {t('tasks.selectHint')}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
