import React, { useMemo, useState, useEffect, useRef, useCallback } from 'react';
import { Monitor, Clock, Globe, ExternalLink, Tag, Info, Layers, Image as ImageIcon, Maximize2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { computeLinkScores, updateScreenshotCategory, fetchThumbnailBatch } from '../lib/monitor_api';
import { getRelatedScreenshots } from '../lib/task_api';
import { openUrl } from '@tauri-apps/plugin-opener';
import { CATEGORY_LIST, CATEGORY_COLORS } from '../lib/categories';
import { buildActivityContext, getHostname } from '../lib/activity_context';
import ActivityContextDrawer from './ActivityContextDrawer';

export default function DetailCard({ selectedEvent, selectedDetails, onCategoryChange, onSelectRelated, onOpenFloatingPreview }) {
  const { t } = useTranslation();
  const [scoredLinks, setScoredLinks] = useState([]);
  const [editingCategory, setEditingCategory] = useState(false);
  const [localCategory, setLocalCategory] = useState(null);
  const [relatedResult, setRelatedResult] = useState(null);
  const [thumbnailMap, setThumbnailMap] = useState({});
  const [activityDrawerOpen, setActivityDrawerOpen] = useState(false);

  // Card visibility state
  const [isVisible, setIsVisible] = useState(false);
  const [isHoveringCard, setIsHoveringCard] = useState(false);
  const timerRef = useRef(null);
  const cardRef = useRef(null);

  const iconSrc = useMemo(() => {
    const raw = selectedDetails?.record?.process_icon || selectedEvent?.processIcon || selectedDetails?.record?.page_icon;
    if (!raw) return null;
    if (raw.startsWith('data:') || raw.startsWith('http://') || raw.startsWith('https://')) return raw;
    return `data:image/png;base64,${raw}`;
  }, [selectedDetails?.record?.process_icon, selectedEvent?.processIcon, selectedDetails?.record?.page_icon]);

  // Score visible_links
  useEffect(() => {
    const links = selectedDetails?.record?.visible_links;
    if (!links || links.length === 0) {
      setScoredLinks([]);
      return;
    }
    let cancelled = false;
    computeLinkScores(links)
      .then((results) => {
        if (!cancelled) setScoredLinks(results || []);
      })
      .catch((err) => {
        console.error('Failed to compute link scores:', err);
        if (!cancelled) setScoredLinks([]);
      });
    return () => { cancelled = true; };
  }, [selectedDetails?.record?.visible_links]);

  const pageUrl = selectedDetails?.record?.page_url;
  const activityContext = useMemo(() => buildActivityContext({
    selectedEvent,
    selectedRecord: selectedDetails?.record,
    relatedResult,
  }), [selectedDetails?.record, selectedEvent, relatedResult]);

  // Category state
  const currentCategory = localCategory ?? selectedDetails?.record?.category ?? selectedEvent?.category ?? null;

  useEffect(() => {
    setLocalCategory(null);
    setEditingCategory(false);
  }, [selectedEvent?.id]);

  const fetchRelated = useCallback(async () => {
    const id = selectedEvent?.id;
    if (!id || id <= 0) {
      return null;
    }
    return getRelatedScreenshots(id, 8);
  }, [selectedEvent?.id]);

  const commitRelatedResult = useCallback((result, { keepEmpty = false } = {}) => {
    const hasTask = result?.task_id !== undefined && result.task_id >= 0;
    const hasRelatedScreenshots = result?.screenshots?.length > 0;
    setRelatedResult(hasRelatedScreenshots || (keepEmpty && hasTask) ? result : null);
  }, []);

  // Fetch related screenshots (same task cluster)
  useEffect(() => {
    if (!selectedEvent?.id || selectedEvent.id <= 0) {
      setRelatedResult(null);
      setActivityDrawerOpen(false);
      return;
    }
    let cancelled = false;
    fetchRelated()
      .then((result) => {
        if (!cancelled) {
          commitRelatedResult(result);
        }
      })
      .catch(() => {
        if (!cancelled) setRelatedResult(null);
      });
    return () => { cancelled = true; };
  }, [commitRelatedResult, fetchRelated, selectedEvent?.id]);

  // Batch load thumbnails for related screenshots
  useEffect(() => {
    if (!relatedResult?.screenshots?.length) {
      setThumbnailMap({});
      return;
    }
    let cancelled = false;
    const ids = relatedResult.screenshots
      .map(s => s.screenshot_id)
      .filter(id => typeof id === 'number' && id > 0);
    if (ids.length === 0) return;
    fetchThumbnailBatch(ids).then(map => {
      if (!cancelled) setThumbnailMap(map || {});
    }).catch(() => {
      if (!cancelled) setThumbnailMap({});
    });
    return () => { cancelled = true; };
  }, [relatedResult]);

  // --- Auto show/hide logic ---
  const clearTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const startHideTimer = useCallback(() => {
    clearTimer();
    timerRef.current = setTimeout(() => {
      setIsVisible(false);
    }, 5000);
  }, [clearTimer]);

  // Show card when selectedEvent changes
  useEffect(() => {
    if (selectedEvent) {
      setIsVisible(true);
      startHideTimer();
    } else {
      setIsVisible(false);
      clearTimer();
    }
    return clearTimer;
  }, [selectedEvent?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // Pause timer while hovering
  useEffect(() => {
    if (isHoveringCard) {
      clearTimer();
    } else if (isVisible) {
      startHideTimer();
    }
  }, [isHoveringCard]); // eslint-disable-line react-hooks/exhaustive-deps

  const handleMouseEnterCard = () => setIsHoveringCard(true);
  const handleMouseLeaveCard = () => setIsHoveringCard(false);

  // Called from parent when user hovers the edge zone
  const handleEdgeHover = useCallback(() => {
    if (!selectedEvent) return;
    setIsVisible(true);
    startHideTimer();
  }, [selectedEvent, startHideTimer]);

  // Expose the edge-hover handler via ref so parent can call it
  // We use a different approach: parent passes a callback registration
  // Instead, we export handleEdgeHover via a ref-forwarding pattern
  // For simplicity, we'll use the onEdgeHover prop pattern in parent

  const handleCategoryChange = async (newCategory) => {
    if (!selectedEvent?.id) return;
    setLocalCategory(newCategory);
    setEditingCategory(false);
    try {
      await updateScreenshotCategory(selectedEvent.id, newCategory);
      if (onCategoryChange) onCategoryChange(selectedEvent.id, newCategory);
    } catch (err) {
      console.error('Failed to update category:', err);
    }
  };

  const handleOpenUrl = (url) => {
    openUrl(url).catch((err) => console.error('Failed to open URL:', err));
  };

  const formatTaskRange = (start, end) => {
    if (!start || !end) return null;
    const startDate = new Date(start * 1000);
    const endDate = new Date(end * 1000);
    if (Number.isNaN(startDate.getTime()) || Number.isNaN(endDate.getTime())) return null;
    const sameDay = startDate.toDateString() === endDate.toDateString();
    return sameDay
      ? startDate.toLocaleDateString()
      : `${startDate.toLocaleDateString()} - ${endDate.toLocaleDateString()}`;
  };

  const formatRelativeToCurrent = (timestamp) => {
    const current = activityContext.currentTimestamp;
    if (!timestamp || !current) return null;
    const deltaSecs = Math.round(timestamp - current);
    if (Math.abs(deltaSecs) < 60) return t('sidebar.related.now');
    const abs = Math.abs(deltaSecs);
    const value = abs < 3600
      ? `${Math.round(abs / 60)}m`
      : abs < 86400
        ? `${Math.round(abs / 3600)}h`
        : `${Math.round(abs / 86400)}d`;
    return deltaSecs < 0
      ? t('sidebar.related.before', { value })
      : t('sidebar.related.after', { value });
  };

  const relationLabel = (relation) => {
    if (relation === 'before') return t('sidebar.related.previous');
    if (relation === 'after') return t('sidebar.related.next');
    return t('sidebar.related.nearby');
  };

  const relatedRows = useMemo(() => {
    const rows = relatedResult?.screenshots || [];
    return rows.slice(0, 5).map((s) => {
      const host = getHostname(s.page_url);
      const relative = formatRelativeToCurrent(s.timestamp);
      const subtitle = [
        relationLabel(s.relation),
        relative,
        host || s.process_name,
      ].filter(Boolean).join(' - ');
      return {
        ...s,
        title: s.window_title || host || s.process_name || t('sidebar.related.snapshot'),
        subtitle,
      };
    });
  }, [relatedResult, activityContext.currentTimestamp, t]); // eslint-disable-line react-hooks/exhaustive-deps

  const handleActivityChanged = useCallback((change) => {
    if (change?.type === 'deleted') {
      setRelatedResult(null);
      setActivityDrawerOpen(false);
      return;
    }
    fetchRelated()
      .then((result) => {
        commitRelatedResult(result, { keepEmpty: activityDrawerOpen });
      })
      .catch(() => {});
  }, [activityDrawerOpen, commitRelatedResult, fetchRelated]);

  // Dismiss on outside press without intercepting the underlying image drag.
  useEffect(() => {
    if (!isVisible) return undefined;

    const handleOutsidePointerDown = (event) => {
      const cardEl = cardRef.current;
      if (cardEl?.contains(event.target)) return;

      clearTimer();
      setIsHoveringCard(false);
      setIsVisible(false);
    };

    document.addEventListener('pointerdown', handleOutsidePointerDown, true);
    return () => {
      document.removeEventListener('pointerdown', handleOutsidePointerDown, true);
    };
  }, [isVisible, clearTimer]);

  if (!selectedEvent) return null;

  return (
    <>
      {/* Collapsed indicator pill — hover to re-open */}
      <div
        className={`absolute top-4 left-4 z-20 flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg bg-ide-panel shadow cursor-pointer select-none transition-all duration-300 ${
          !isVisible
            ? 'opacity-100 translate-x-0 pointer-events-auto'
            : 'opacity-0 -translate-x-2 pointer-events-none'
        }`}
        onMouseEnter={handleEdgeHover}
      >
        <Info size={14} className="text-ide-accent shrink-0" />
        <span className="text-xs text-ide-muted whitespace-nowrap">{selectedDetails?.record?.process_name || selectedEvent.appName}</span>
      </div>

      {/* Invisible left-edge hover zone to re-trigger card */}
      {!isVisible && (
        <div
          className="absolute left-0 top-0 bottom-0 w-8 z-10"
          onMouseEnter={handleEdgeHover}
        />
      )}

      {/* Detail card */}
      <div
        ref={cardRef}
        className={`absolute top-4 left-4 w-80 max-h-[calc(100%-2rem)] overflow-y-auto z-20 rounded-xl border border-ide-border bg-ide-panel shadow-lg transition-all duration-300 ${
          isVisible
            ? 'opacity-100 translate-x-0 pointer-events-auto'
            : 'opacity-0 -translate-x-4 pointer-events-none'
        }`}
        onMouseEnter={handleMouseEnterCard}
        onMouseLeave={handleMouseLeaveCard}
      >
        <div className="p-4 text-sm text-ide-muted space-y-3">
          {/* Process info */}
          <div>
            <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.process')}</label>
            <div className="flex items-center gap-2 mt-1">
              <div className="w-8 h-8 rounded shrink-0 flex items-center justify-center overflow-hidden">
                {iconSrc ? (
                  <img src={iconSrc} alt={selectedDetails?.record?.process_name || selectedEvent.appName || 'app'} className="w-6 h-6 object-cover" />
                ) : (
                  <div className="w-7 h-7 bg-blue-500/20 text-blue-500 flex items-center justify-center rounded">
                    <Monitor size={16} />
                  </div>
                )}
              </div>
              <div className="overflow-hidden">
                <div className="font-medium truncate text-ide-text" title={selectedDetails?.record?.process_name || selectedEvent.appName}>
                  {selectedDetails?.record?.process_name || selectedEvent.appName}
                </div>
              </div>
            </div>
          </div>

          {/* Window title */}
          <div>
            <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.window')}</label>
            <div className="mt-1 text-sm break-words opacity-80 line-clamp-2 select-text" title={selectedDetails?.record?.window_title || selectedEvent.windowTitle}>
              {selectedDetails?.record?.window_title || selectedEvent.windowTitle}
            </div>
          </div>

          {/* Timestamp */}
          <div>
            <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.time')}</label>
            <div className="flex items-center gap-2 mt-1 text-sm opacity-80">
              <Clock size={14} />
              {new Date(selectedEvent.timestamp).toLocaleString()}
            </div>
          </div>

          {/* Category */}
          <div>
            <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.labels.category', '分类')}</label>
            <div className="mt-1">
              {editingCategory ? (
                <div className="space-y-1 max-h-32 overflow-y-auto">
                  {CATEGORY_LIST.map((cat) => (
                    <button
                      key={cat}
                      className={`w-full text-left px-2 py-1 rounded text-xs hover:bg-ide-hover flex items-center gap-2 ${
                        currentCategory === cat ? 'bg-ide-active' : ''
                      }`}
                      onClick={() => handleCategoryChange(cat)}
                    >
                      <span
                        className="w-2 h-2 rounded-full shrink-0"
                        style={{ backgroundColor: CATEGORY_COLORS[cat] || '#6b7280' }}
                      />
                      {cat}
                    </button>
                  ))}
                </div>
              ) : (
                <button
                  className="flex items-center gap-2 text-sm opacity-80 hover:opacity-100 cursor-pointer"
                  onClick={() => setEditingCategory(true)}
                >
                  <Tag size={14} />
                  {currentCategory && currentCategory !== '未分类' ? (
                    <span className="px-1.5 py-0.5 rounded text-xs text-white" style={{ backgroundColor: CATEGORY_COLORS[currentCategory] || '#6b7280' }}>
                      {currentCategory}
                    </span>
                  ) : (
                    <span className="text-ide-muted">{t('sidebar.labels.uncategorized', '未分类')}</span>
                  )}
                </button>
              )}
            </div>
          </div>

          {/* Links */}
          {(pageUrl || scoredLinks.length > 0) && (
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold">{t('sidebar.links.title')}</label>
              <div className="mt-1 space-y-1">
                {pageUrl && (
                  <div
                    className="hover:bg-ide-hover/60 cursor-pointer rounded p-2 text-xs flex items-start gap-2 group"
                    onClick={() => handleOpenUrl(pageUrl)}
                    title={pageUrl}
                  >
                    <Globe size={14} className="shrink-0 mt-0.5 text-blue-400" />
                    <div className="overflow-hidden flex-1 min-w-0">
                      <div className="font-medium text-blue-400 truncate">{t('sidebar.links.currentPage')}</div>
                      <div className="truncate opacity-60">{getHostname(pageUrl)}</div>
                    </div>
                    <ExternalLink size={12} className="shrink-0 mt-0.5 opacity-0 group-hover:opacity-60" />
                  </div>
                )}
                {scoredLinks.map((link, idx) => (
                  <div
                    key={idx}
                    className="hover:bg-ide-hover/60 cursor-pointer rounded p-2 text-xs flex items-start gap-2 group"
                    onClick={() => handleOpenUrl(link.url)}
                    title={link.text}
                  >
                    <ExternalLink size={14} className="shrink-0 mt-0.5 opacity-60" />
                    <div className="overflow-hidden flex-1 min-w-0">
                      <div className="truncate">{link.text || link.url}</div>
                      <div className="truncate opacity-60">{getHostname(link.url)}</div>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Activity context (same task cluster) */}
          {relatedResult && relatedResult.screenshots.length > 0 && (
            <div className="pt-3 border-t border-ide-border/70">
              <div className="flex items-center justify-between gap-2">
                <label className="text-xs text-ide-muted uppercase font-bold flex items-center gap-1.5">
                  <Layers size={13} className="text-ide-accent" />
                  {t('sidebar.related.title')}
                  <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
                </label>
                <button
                  type="button"
                  className="inline-flex items-center gap-1 rounded border border-ide-border px-1.5 py-1 text-[11px] text-ide-muted hover:bg-ide-hover hover:text-ide-text"
                  onClick={() => {
                    setActivityDrawerOpen(true);
                    clearTimer();
                  }}
                  title={t('activityContext.open')}
                >
                  <Maximize2 size={12} />
                  {t('activityContext.open')}
                </button>
              </div>
              <div className="mt-1 min-w-0">
                <div className="text-sm font-medium text-ide-text truncate" title={activityContext.title || relatedResult.task_label || ''}>
                  {activityContext.title || relatedResult.task_label || t('sidebar.related.untitled')}
                </div>
                <div className="mt-1 flex flex-wrap gap-1.5 text-[10px] text-ide-muted">
                  {activityContext.snapshotCount && (
                    <span className="px-1.5 py-0.5 rounded border border-ide-border bg-ide-bg">
                      {t('sidebar.related.count', { count: activityContext.snapshotCount })}
                    </span>
                  )}
                  {formatTaskRange(activityContext.startTime, activityContext.endTime) && (
                    <span className="px-1.5 py-0.5 rounded border border-ide-border bg-ide-bg">
                      {formatTaskRange(activityContext.startTime, activityContext.endTime)}
                    </span>
                  )}
                  {(activityContext.host || activityContext.category) && (
                    <span className="px-1.5 py-0.5 rounded border border-ide-border bg-ide-bg truncate max-w-full">
                      {activityContext.host || activityContext.category}
                    </span>
                  )}
                </div>
              </div>
              <div className="mt-2 space-y-1.5">
                {relatedRows.map((s) => {
                  const thumb = thumbnailMap[s.screenshot_id] ?? null;
                  return (
                    <button
                      key={s.screenshot_id}
                      type="button"
                      className="w-full min-w-0 flex items-center gap-2 rounded p-1.5 text-left transition-colors hover:bg-ide-hover/60"
                      onClick={() => onSelectRelated?.({
                        screenshot_id: s.screenshot_id,
                        id: s.screenshot_id,
                        image_path: s.image_path,
                        path: s.image_path,
                        process_name: s.process_name,
                        window_title: s.window_title,
                        category: s.category,
                        created_at: s.created_at,
                        page_url: s.page_url,
                      })}
                    >
                      <div className="h-10 w-[4.5rem] shrink-0 overflow-hidden rounded border border-ide-border bg-ide-bg">
                        {thumb ? (
                          <img src={thumb} alt="" className="h-full w-full object-cover" loading="lazy" />
                        ) : (
                          <div className="flex h-full w-full items-center justify-center text-ide-muted">
                            <ImageIcon size={14} />
                          </div>
                        )}
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-xs font-medium text-ide-text" title={s.title}>
                          {s.title}
                        </div>
                        <div className="mt-0.5 truncate text-[11px] text-ide-muted" title={s.subtitle}>
                          {s.subtitle}
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      </div>
      {activityDrawerOpen && relatedResult && (
        <ActivityContextDrawer
          relatedResult={relatedResult}
          activityContext={activityContext}
          onClose={() => setActivityDrawerOpen(false)}
          onSelectScreenshot={(payload) => {
            onSelectRelated?.(payload);
          }}
          onOpenFloatingPreview={onOpenFloatingPreview}
          onActivityChanged={handleActivityChanged}
        />
      )}
    </>
  );
}
