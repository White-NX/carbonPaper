import React, { useMemo, useState, useEffect, useRef, useCallback } from 'react';
import { Monitor, Clock, Globe, ExternalLink, Tag, Info, Layers } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { computeLinkScores, updateScreenshotCategory } from '../lib/monitor_api';
import { getRelatedScreenshots } from '../lib/task_api';
import { openUrl } from '@tauri-apps/plugin-opener';
import { CATEGORY_LIST, CATEGORY_COLORS } from '../lib/categories';
import { ThumbnailCard } from './ThumbnailCard';

export default function DetailCard({ selectedEvent, selectedDetails, onCategoryChange, onSelectRelated }) {
  const { t } = useTranslation();
  const [scoredLinks, setScoredLinks] = useState([]);
  const [editingCategory, setEditingCategory] = useState(false);
  const [localCategory, setLocalCategory] = useState(null);
  const [relatedResult, setRelatedResult] = useState(null);

  // Card visibility state
  const [isVisible, setIsVisible] = useState(false);
  const [isHoveringCard, setIsHoveringCard] = useState(false);
  const timerRef = useRef(null);

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

  // Category state
  const currentCategory = localCategory ?? selectedDetails?.record?.category ?? selectedEvent?.category ?? null;

  useEffect(() => {
    setLocalCategory(null);
    setEditingCategory(false);
  }, [selectedEvent?.id]);

  // Fetch related screenshots (same task cluster)
  useEffect(() => {
    const id = selectedEvent?.id;
    if (!id || id <= 0) {
      setRelatedResult(null);
      return;
    }
    let cancelled = false;
    getRelatedScreenshots(id, 8)
      .then((result) => {
        if (!cancelled) {
          setRelatedResult(result?.screenshots?.length ? result : null);
        }
      })
      .catch(() => {
        if (!cancelled) setRelatedResult(null);
      });
    return () => { cancelled = true; };
  }, [selectedEvent?.id]);

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

  const getHostname = (url) => {
    try {
      return new URL(url).hostname;
    } catch {
      return url;
    }
  };

  const handleOpenUrl = (url) => {
    openUrl(url).catch((err) => console.error('Failed to open URL:', err));
  };

  // Dismiss card on click outside (image / blank area)
  const handleBackdropClick = useCallback(() => {
    if (isVisible) {
      clearTimer();
      setIsVisible(false);
    }
  }, [isVisible, clearTimer]);

  if (!selectedEvent) return null;

  return (
    <>
      {/* Click-to-dismiss backdrop (covers the entire preview area behind the card) */}
      {isVisible && (
        <div
          className="absolute inset-0 z-10"
          onClick={handleBackdropClick}
        />
      )}

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
        className={`absolute top-4 left-4 w-72 max-h-[calc(100%-2rem)] overflow-y-auto z-20 rounded-xl border border-ide-border bg-ide-panel shadow-lg transition-all duration-300 ${
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

          {/* Related screenshots (same task cluster) */}
          {relatedResult && relatedResult.screenshots.length > 0 && (
            <div>
              <label className="text-xs text-ide-muted uppercase font-bold flex items-center gap-1">
                {t('sidebar.related.title')}
                <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
              </label>
              {relatedResult.task_label && (
                <div className="mt-0.5 text-[11px] text-ide-muted/70 truncate" title={relatedResult.task_label}>
                  {relatedResult.task_label}
                </div>
              )}
              <div className="mt-1.5 grid grid-cols-2 gap-1.5">
                {relatedResult.screenshots.map((s) => (
                  <ThumbnailCard
                    key={s.screenshot_id}
                    item={{
                      screenshot_id: s.screenshot_id,
                      image_path: s.image_path,
                      process_name: s.process_name,
                      window_title: s.window_title,
                      category: s.category,
                      created_at: s.created_at,
                    }}
                    onSelect={(payload) => onSelectRelated?.(payload)}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
