import React, { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Check, ChevronLeft, ChevronRight, Clock, Eye, Globe, Image as ImageIcon,
  Loader2, Maximize2, Pencil, RefreshCw, Trash2, Unlink, X,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { deleteTask, getTaskScreenshots, removeTaskScreenshot, updateTaskLabel } from '../lib/task_api';
import { fetchThumbnailBatch } from '../lib/monitor_api';
import { getHostname } from '../lib/activity_context';
import { ConfirmDialog } from './ConfirmDialog';

const PAGE_SIZE = 24;

function formatTimeRange(start, end) {
  if (!start || !end) return null;
  const startDate = new Date(start * 1000);
  const endDate = new Date(end * 1000);
  if (Number.isNaN(startDate.getTime()) || Number.isNaN(endDate.getTime())) return null;
  if (startDate.toDateString() === endDate.toDateString()) return startDate.toLocaleDateString();
  return `${startDate.toLocaleDateString()} - ${endDate.toLocaleDateString()}`;
}

function formatTimestamp(ts, fallback) {
  if (ts) {
    const d = new Date(ts * 1000);
    if (!Number.isNaN(d.getTime())) return d.toLocaleString();
  }
  if (fallback) {
    const d = new Date(fallback);
    if (!Number.isNaN(d.getTime())) return d.toLocaleString();
  }
  return '';
}

function ActivitySnapshotCard({ item, thumbnailSrc, onSelect, onOpenFloatingPreview, onRemove, removing }) {
  const { t } = useTranslation();
  const host = getHostname(item.page_url);
  const title = item.window_title || host || item.process_name || t('activityContext.snapshot');
  const subtitle = [
    formatTimestamp(item.timestamp, item.created_at),
    host || item.process_name,
  ].filter(Boolean).join(' - ');
  const cardClickBehavior = localStorage.getItem('cardClickBehavior_activityContext') || 'preview';
  const isStandaloneDefault = cardClickBehavior === 'standalone' && !!onOpenFloatingPreview;
  const normalizedItem = {
    ...item,
    id: item.screenshot_id || item.id,
    path: item.image_path || item.path,
  };

  const handleSelect = () => {
    if (isStandaloneDefault) {
      onOpenFloatingPreview(normalizedItem);
    } else {
      onSelect?.(normalizedItem);
    }
  };

  const handleAlternateAction = (event) => {
    event.preventDefault();
    event.stopPropagation();
    if (isStandaloneDefault) {
      onSelect?.(normalizedItem);
    } else {
      onOpenFloatingPreview?.(normalizedItem);
    }
  };

  return (
    <div className="group relative overflow-hidden rounded border border-ide-border bg-ide-panel transition-colors hover:border-ide-accent/70">
      <button
        type="button"
        className="block w-full text-left"
        onClick={handleSelect}
      >
        <div className="aspect-video w-full overflow-hidden bg-ide-bg">
          {thumbnailSrc ? (
            <img src={thumbnailSrc} alt="" className="h-full w-full object-cover" loading="lazy" />
          ) : (
            <div className="flex h-full w-full items-center justify-center text-ide-muted">
              <ImageIcon className="h-5 w-5" />
            </div>
          )}
        </div>
        <div className="min-w-0 p-2">
          <div className="truncate text-xs font-medium text-ide-text" title={title}>{title}</div>
          <div className="mt-0.5 truncate text-[11px] text-ide-muted" title={subtitle}>{subtitle}</div>
        </div>
      </button>
      <button
        type="button"
        className="absolute left-1 top-1 rounded border border-white/15 bg-black/60 p-1.5 text-white opacity-0 shadow-sm transition hover:bg-black/80 disabled:opacity-50 group-hover:opacity-100"
        onClick={(event) => {
          event.stopPropagation();
          onRemove(item);
        }}
        disabled={removing}
        title={t('activityContext.removeSnapshot')}
        aria-label={t('activityContext.removeSnapshot')}
      >
        {removing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Unlink className="h-3.5 w-3.5" />}
      </button>
      {onOpenFloatingPreview && (
        <button
          type="button"
          className="absolute right-1 top-1 z-10 rounded border border-white/15 bg-black/60 p-1.5 text-white opacity-0 shadow-sm transition hover:bg-black/80 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/70 group-hover:opacity-100"
          onClick={handleAlternateAction}
          title={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
          aria-label={isStandaloneDefault ? t('previewAction.openMainPreview') : t('previewAction.openFloatingPreview')}
        >
          {isStandaloneDefault ? <Eye className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />}
        </button>
      )}
    </div>
  );
}

export default function ActivityContextDrawer({
  relatedResult,
  activityContext,
  onClose,
  onSelectScreenshot,
  onOpenFloatingPreview,
  onActivityChanged,
}) {
  const { t } = useTranslation();
  const taskId = relatedResult?.task_id;
  const [page, setPage] = useState(0);
  const [items, setItems] = useState([]);
  const [thumbnailMap, setThumbnailMap] = useState({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [snapshotCount, setSnapshotCount] = useState(relatedResult?.snapshot_count || 0);
  const [title, setTitle] = useState(activityContext?.title || relatedResult?.task_label || '');
  const [editingTitle, setEditingTitle] = useState(false);
  const [draftTitle, setDraftTitle] = useState(title);
  const [savingTitle, setSavingTitle] = useState(false);
  const [removingId, setRemovingId] = useState(null);
  const [deletingTask, setDeletingTask] = useState(false);
  const [pendingConfirm, setPendingConfirm] = useState(null);

  useEffect(() => {
    setSnapshotCount(relatedResult?.snapshot_count || 0);
    const nextTitle = activityContext?.title || relatedResult?.task_label || '';
    setTitle(nextTitle);
    setDraftTitle(nextTitle);
    setPage(0);
    setPendingConfirm(null);
  }, [activityContext?.title, relatedResult?.snapshot_count, relatedResult?.task_id, relatedResult?.task_label]);

  const maxPage = useMemo(() => {
    if (!snapshotCount) return 0;
    return Math.max(0, Math.ceil(snapshotCount / PAGE_SIZE) - 1);
  }, [snapshotCount]);

  const loadPage = useCallback(async () => {
    if (!taskId || taskId < 0) return;
    setLoading(true);
    setError(null);
    try {
      const result = await getTaskScreenshots(taskId, page, PAGE_SIZE);
      setItems(result || []);
    } catch (err) {
      setError(String(err?.message || err));
      setItems([]);
    } finally {
      setLoading(false);
    }
  }, [page, taskId]);

  useEffect(() => {
    loadPage();
  }, [loadPage, refreshKey]);

  useEffect(() => {
    if (!items.length) {
      setThumbnailMap({});
      return;
    }
    let cancelled = false;
    const ids = items
      .map((item) => item.screenshot_id)
      .filter((id) => typeof id === 'number' && id > 0);
    if (!ids.length) return;
    fetchThumbnailBatch(ids)
      .then((map) => {
        if (!cancelled) setThumbnailMap(map || {});
      })
      .catch(() => {
        if (!cancelled) setThumbnailMap({});
      });
    return () => { cancelled = true; };
  }, [items]);

  const handleSaveTitle = async () => {
    const label = draftTitle.trim();
    if (!taskId || !label) return;
    setSavingTitle(true);
    setError(null);
    try {
      await updateTaskLabel(taskId, label);
      setTitle(label);
      setEditingTitle(false);
      onActivityChanged?.({ type: 'renamed', taskId, label });
    } catch (err) {
      setError(String(err?.message || err));
    } finally {
      setSavingTitle(false);
    }
  };

  const requestRemoveSnapshot = (item) => {
    if (!taskId || !item?.screenshot_id || removingId) return;
    setPendingConfirm({ type: 'snapshot', item });
  };

  const executeRemoveSnapshot = async (item) => {
    if (!taskId || !item?.screenshot_id) return false;
    setRemovingId(item.screenshot_id);
    setError(null);
    try {
      const remaining = await removeTaskScreenshot(taskId, item.screenshot_id);
      setSnapshotCount(remaining);
      onActivityChanged?.({ type: 'snapshot_removed', taskId, screenshotId: item.screenshot_id, remaining });
      if (remaining <= 0) {
        onClose();
        return true;
      }
      if (page > 0 && (page * PAGE_SIZE) >= remaining) {
        setPage((p) => Math.max(0, p - 1));
      } else {
        setRefreshKey((k) => k + 1);
      }
      return true;
    } catch (err) {
      setError(String(err?.message || err));
      return false;
    } finally {
      setRemovingId(null);
    }
  };

  const requestDeleteActivity = () => {
    if (!taskId || deletingTask) return;
    setPendingConfirm({ type: 'activity' });
  };

  const executeDeleteActivity = async () => {
    if (!taskId) return false;
    setDeletingTask(true);
    setError(null);
    try {
      await deleteTask(taskId);
      onActivityChanged?.({ type: 'deleted', taskId });
      onClose();
      return true;
    } catch (err) {
      setError(String(err?.message || err));
      return false;
    } finally {
      setDeletingTask(false);
    }
  };

  const handleSelect = (item) => {
    onSelectScreenshot?.({
      screenshot_id: item.screenshot_id,
      id: item.screenshot_id,
      image_path: item.image_path,
      path: item.image_path,
      process_name: item.process_name,
      window_title: item.window_title,
      category: item.category,
      created_at: item.created_at,
      page_url: item.page_url,
    });
  };

  const rangeLabel = formatTimeRange(activityContext?.startTime, activityContext?.endTime);
  const confirmLoading = pendingConfirm?.type === 'activity'
    ? deletingTask
    : Boolean(pendingConfirm?.item?.screenshot_id && removingId === pendingConfirm.item.screenshot_id);
  const confirmConfig = pendingConfirm?.type === 'activity'
    ? {
      title: t('activityContext.deleteActivity'),
      message: t('activityContext.deleteActivityConfirm'),
      confirmLabel: t('activityContext.deleteActivity'),
    }
    : pendingConfirm?.type === 'snapshot'
      ? {
        title: t('activityContext.removeSnapshot'),
        message: t('activityContext.removeSnapshotConfirm'),
        confirmLabel: t('activityContext.removeSnapshot'),
      }
      : null;

  const handleConfirmAction = async () => {
    if (!pendingConfirm || confirmLoading) return;
    const currentConfirm = pendingConfirm;
    const success = currentConfirm.type === 'activity'
      ? await executeDeleteActivity()
      : await executeRemoveSnapshot(currentConfirm.item);
    if (success) {
      setPendingConfirm(null);
    }
  };

  const handleCancelConfirm = () => {
    if (confirmLoading) return;
    setPendingConfirm(null);
  };

  return (
    <div
      className="absolute inset-y-0 right-0 z-30 flex w-full justify-end bg-black/20"
      data-testid="activity-context-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose?.();
        }
      }}
    >
      <aside
        className="flex h-full w-full max-w-[560px] flex-col border-l border-ide-border bg-ide-bg shadow-2xl"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="shrink-0 border-b border-ide-border bg-ide-panel px-4 py-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 text-[11px] font-bold uppercase text-ide-muted">
                <LayersLabel />
                {t('activityContext.title')}
              </div>
              {editingTitle ? (
                <div className="mt-2 flex items-center gap-2">
                  <input
                    value={draftTitle}
                    onChange={(e) => setDraftTitle(e.target.value)}
                    className="min-w-0 flex-1 rounded border border-ide-border bg-ide-bg px-2 py-1.5 text-sm text-ide-text focus:border-ide-accent focus:outline-none"
                    autoFocus
                  />
                  <button
                    type="button"
                    className="rounded border border-ide-accent/40 bg-ide-accent/15 p-1.5 text-ide-accent disabled:opacity-40"
                    onClick={handleSaveTitle}
                    disabled={savingTitle || !draftTitle.trim()}
                    title={t('common.save', 'Save')}
                  >
                    {savingTitle ? <Loader2 className="h-4 w-4 animate-spin" /> : <Check className="h-4 w-4" />}
                  </button>
                  <button
                    type="button"
                    className="rounded border border-ide-border p-1.5 text-ide-muted hover:bg-ide-hover"
                    onClick={() => {
                      setEditingTitle(false);
                      setDraftTitle(title);
                    }}
                    title={t('common.cancel')}
                  >
                    <X className="h-4 w-4" />
                  </button>
                </div>
              ) : (
                <div className="mt-1 flex min-w-0 items-center gap-2">
                  <h3 className="min-w-0 flex-1 truncate text-base font-semibold text-ide-text" title={title}>
                    {title || t('activityContext.untitled')}
                  </h3>
                  <button
                    type="button"
                    className="rounded p-1.5 text-ide-muted hover:bg-ide-hover hover:text-ide-text"
                    onClick={() => {
                      setDraftTitle(title);
                      setEditingTitle(true);
                    }}
                    title={t('activityContext.rename')}
                  >
                    <Pencil className="h-4 w-4" />
                  </button>
                </div>
              )}
            </div>
            <button
              type="button"
              className="rounded p-1.5 text-ide-muted hover:bg-ide-hover hover:text-ide-text"
              onClick={onClose}
              title={t('common.close')}
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <div className="mt-3 flex flex-wrap gap-1.5 text-[11px] text-ide-muted">
            <span className="rounded border border-ide-border bg-ide-bg px-2 py-1">
              {t('activityContext.count', { count: snapshotCount || 0 })}
            </span>
            {rangeLabel && (
              <span className="inline-flex items-center gap-1 rounded border border-ide-border bg-ide-bg px-2 py-1">
                <Clock className="h-3 w-3" />
                {rangeLabel}
              </span>
            )}
            {(activityContext?.host || activityContext?.category) && (
              <span className="inline-flex max-w-full items-center gap-1 truncate rounded border border-ide-border bg-ide-bg px-2 py-1">
                {activityContext?.host && <Globe className="h-3 w-3 shrink-0" />}
                <span className="truncate">{activityContext.host || activityContext.category}</span>
              </span>
            )}
          </div>

          <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-1.5">
              <button
                type="button"
                className="inline-flex items-center gap-1 rounded border border-ide-border px-2 py-1 text-xs text-ide-muted hover:bg-ide-hover hover:text-ide-text"
                onClick={() => setRefreshKey((k) => k + 1)}
                disabled={loading}
              >
                <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
                {t('activityContext.refresh')}
              </button>
              <button
                type="button"
                className="inline-flex items-center gap-1 rounded border border-red-500/30 px-2 py-1 text-xs text-red-300 hover:bg-red-500/10 disabled:opacity-40"
                onClick={requestDeleteActivity}
                disabled={deletingTask}
              >
                {deletingTask ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Trash2 className="h-3.5 w-3.5" />}
                {t('activityContext.deleteActivity')}
              </button>
            </div>
            <div className="flex items-center gap-1.5 text-xs text-ide-muted">
              <button
                type="button"
                className="rounded border border-ide-border p-1 hover:bg-ide-hover disabled:opacity-40"
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={page <= 0 || loading}
                title={t('activityContext.previousPage')}
              >
                <ChevronLeft className="h-4 w-4" />
              </button>
              <span>{t('activityContext.page', { current: page + 1, total: maxPage + 1 })}</span>
              <button
                type="button"
                className="rounded border border-ide-border p-1 hover:bg-ide-hover disabled:opacity-40"
                onClick={() => setPage((p) => Math.min(maxPage, p + 1))}
                disabled={page >= maxPage || loading}
                title={t('activityContext.nextPage')}
              >
                <ChevronRight className="h-4 w-4" />
              </button>
            </div>
          </div>
        </header>

        {error && (
          <div className="mx-4 mt-3 rounded border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-300">
            {error}
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {loading && !items.length ? (
            <div className="flex h-40 items-center justify-center text-ide-muted">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : items.length ? (
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
              {items.map((item) => (
                <ActivitySnapshotCard
                  key={item.screenshot_id}
                  item={item}
                  thumbnailSrc={thumbnailMap[item.screenshot_id] || null}
                  onSelect={handleSelect}
                  onOpenFloatingPreview={onOpenFloatingPreview}
                  onRemove={requestRemoveSnapshot}
                  removing={removingId === item.screenshot_id}
                />
              ))}
            </div>
          ) : (
            <div className="flex h-40 items-center justify-center text-sm text-ide-muted">
              {t('activityContext.empty')}
            </div>
          )}
        </div>
        <ConfirmDialog
          isOpen={Boolean(confirmConfig)}
          onCancel={handleCancelConfirm}
          onConfirm={handleConfirmAction}
          title={confirmConfig?.title || t('activityContext.title')}
          message={confirmConfig?.message || ''}
          confirmLabel={confirmConfig?.confirmLabel || t('activityContext.deleteActivity')}
          cancelLabel={t('common.cancel')}
          confirmVariant="danger"
          loading={confirmLoading}
        />
      </aside>
    </div>
  );
}

function LayersLabel() {
  return <span className="h-1.5 w-1.5 rounded-full bg-ide-accent" />;
}
