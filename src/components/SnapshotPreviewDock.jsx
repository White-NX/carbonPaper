import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChevronDown, Clock, Copy, ExternalLink, GripVertical, Hash, Image as ImageIcon, Info, Loader2,
  Maximize2, Minimize2, Monitor, Tag, X,
} from 'lucide-react';
import { openUrl } from '@tauri-apps/plugin-opener';
import { InspectorImage } from './InspectorImage';
import { CategoryBadge } from './ThumbnailCard';
import { extractUrlsFromOcr } from '../lib/ocr_url_detector';
import { fetchImage, getScreenshotDetails } from '../lib/monitor_api';

const IMAGE_CACHE_LIMIT = 3;
const DETAILS_CACHE_LIMIT = 8;
const DRAG_MARGIN = 8;
const MIN_WIDTH = 560;
const MIN_HEIGHT = 360;
const OCR_DEFAULT_HEIGHT = 220;
const OCR_MIN_HEIGHT = 120;
const OCR_SIDEBAR_RESERVED_HEIGHT = 172;

function getInitialPosition() {
  if (typeof window === 'undefined') return { left: 360, top: 96 };
  const estimatedWidth = Math.min(900, Math.max(320, window.innerWidth - 32));
  const left = Math.max(16, Math.min(360, window.innerWidth - estimatedWidth - 16));
  const top = Math.max(16, Math.min(96, window.innerHeight - 160));
  return { left, top };
}

function getInitialSize() {
  if (typeof window === 'undefined') return { width: 900, height: 680 };
  return {
    width: Math.min(900, Math.max(MIN_WIDTH, window.innerWidth - 32)),
    height: Math.min(680, Math.max(MIN_HEIGHT, window.innerHeight - 80)),
  };
}

function getPreviewKey(item) {
  const id = item?.screenshot_id ?? item?.id ?? item?.metadata?.screenshot_id;
  if (typeof id === 'number' && id > 0) return `id:${id}`;
  const path = item?.image_path || item?.path || item?.metadata?.image_path;
  return path ? `path:${path}` : null;
}

function getTargetId(item) {
  const id = item?.screenshot_id ?? item?.id ?? item?.metadata?.screenshot_id;
  return typeof id === 'number' && id > 0 ? id : null;
}

function getTargetPath(item) {
  return item?.image_path || item?.path || item?.metadata?.image_path || null;
}

function formatTimestamp(value) {
  if (!value) return '—';
  if (typeof value === 'number') {
    const ms = value > 1e12 ? value : value * 1000;
    const parsed = new Date(ms);
    return Number.isNaN(parsed.getTime()) ? String(value) : parsed.toLocaleString();
  }
  const candidate = typeof value === 'string' && !value.includes('T')
    ? value.replace(' ', 'T')
    : value;
  const parsed = new Date(candidate);
  if (!Number.isNaN(parsed.getTime())) return parsed.toLocaleString();
  return String(value);
}

function makeOcrBoxes(ocrResults) {
  return (ocrResults || []).map((item, index) => {
    const points = item.box_coords || item.box;
    if (!points || !Array.isArray(points) || points.length === 0) return null;

    const xs = points.map((p) => p[0]);
    const ys = points.map((p) => p[1]);
    const minX = Math.min(...xs);
    const maxX = Math.max(...xs);
    const minY = Math.min(...ys);
    const maxY = Math.max(...ys);

    return {
      id: String(item.id ?? index),
      label: item.text,
      type: 'text',
      box: {
        x: minX,
        y: minY,
        width: maxX - minX,
        height: maxY - minY,
        unit: 'pixel',
      },
    };
  }).filter(Boolean);
}

function storeLimited(setter, orderRef, key, value, limit) {
  orderRef.current = [key, ...orderRef.current.filter((entry) => entry !== key)].slice(0, limit);
  const allowed = new Set(orderRef.current);
  setter((prev) => {
    const next = { ...prev, [key]: value };
    for (const existingKey of Object.keys(next)) {
      if (!allowed.has(existingKey)) delete next[existingKey];
    }
    return next;
  });
}

function MetadataRow({ icon: Icon, label, value, title, children }) {
  if (!value && !children) return null;
  return (
    <div className="flex gap-2 text-xs">
      <Icon className="mt-0.5 h-3.5 w-3.5 shrink-0 text-ide-muted" />
      <div className="min-w-0 flex-1">
        <div className="text-[10px] font-semibold uppercase text-ide-muted">{label}</div>
        <div className="mt-0.5 break-words text-ide-text" title={title || value}>
          {children || value}
        </div>
      </div>
    </div>
  );
}

function getLocalizedSourceLabel(tab, t) {
  switch (tab?.sourceType) {
    case 'main-preview':
      return t('snapshotPreview.sources.mainPreview');
    case 'advanced-search':
      return t('snapshotPreview.sources.advancedSearch');
    case 'task':
      return t('snapshotPreview.sources.tasks');
    case 'smart-cluster':
      return t('snapshotPreview.sources.smartClusters');
    default:
      return tab?.sourceLabel || null;
  }
}

export default function SnapshotPreviewDock({
  tabs,
  activeKey,
  onActiveChange,
  onCloseTab,
  onClear,
  onOpenInMainPreview,
  onOpenStandalone,
  standalone = false,
}) {
  const { t } = useTranslation();
  const [imageCache, setImageCache] = useState({});
  const [detailsCache, setDetailsCache] = useState({});
  const [errorMap, setErrorMap] = useState({});
  const [loadingKey, setLoadingKey] = useState(null);
  const [copied, setCopied] = useState(false);
  const [minimized, setMinimized] = useState(false);
  const [position, setPosition] = useState(getInitialPosition);
  const [size, setSize] = useState(getInitialSize);
  const [ocrExpanded, setOcrExpanded] = useState(false);
  const [ocrPanelHeight, setOcrPanelHeight] = useState(OCR_DEFAULT_HEIGHT);
  const dockRef = useRef(null);
  const imageOrderRef = useRef([]);
  const detailsOrderRef = useRef([]);
  const copyTimerRef = useRef(null);
  const requestIdRef = useRef(0);
  const dragStateRef = useRef(null);
  const resizeStateRef = useRef(null);
  const ocrResizeStateRef = useRef(null);
  const lastActiveKeyRef = useRef(null);

  const activeTab = useMemo(() => {
    if (!tabs?.length) return null;
    return tabs.find((tab) => getPreviewKey(tab) === activeKey) || tabs[0];
  }, [tabs, activeKey]);

  const resolvedActiveKey = activeTab ? getPreviewKey(activeTab) : null;
  const activeDetails = resolvedActiveKey ? detailsCache[resolvedActiveKey] : null;
  const activeImage = resolvedActiveKey ? imageCache[resolvedActiveKey] : null;
  const activeError = resolvedActiveKey ? errorMap[resolvedActiveKey] : null;
  const isLoading = loadingKey === resolvedActiveKey && (!activeImage || !activeDetails);
  const record = activeDetails?.record || {};
  const ocrResults = activeDetails?.ocr_results || [];

  const ocrText = useMemo(() => (
    ocrResults.map((result) => result.text).filter(Boolean).join('\n')
  ), [ocrResults]);

  const ocrBoxes = useMemo(() => makeOcrBoxes(ocrResults), [ocrResults]);

  const urls = useMemo(() => {
    if (record.page_url) return [record.page_url];
    return extractUrlsFromOcr(ocrResults).slice(0, 3);
  }, [record.page_url, ocrResults]);

  useEffect(() => {
    return () => {
      if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
    };
  }, []);

  const clampPosition = useCallback((left, top, width, height) => {
    if (typeof window === 'undefined') return { left, top };
    const maxLeft = Math.max(DRAG_MARGIN, window.innerWidth - width - DRAG_MARGIN);
    const maxTop = Math.max(DRAG_MARGIN, window.innerHeight - height - DRAG_MARGIN);
    return {
      left: Math.min(Math.max(DRAG_MARGIN, left), maxLeft),
      top: Math.min(Math.max(DRAG_MARGIN, top), maxTop),
    };
  }, []);

  const clampSize = useCallback((width, height, left, top) => {
    if (typeof window === 'undefined') return { width, height };
    const maxWidth = Math.max(MIN_WIDTH, window.innerWidth - left - DRAG_MARGIN);
    const maxHeight = Math.max(MIN_HEIGHT, window.innerHeight - top - DRAG_MARGIN);
    return {
      width: Math.min(Math.max(MIN_WIDTH, width), maxWidth),
      height: Math.min(Math.max(MIN_HEIGHT, height), maxHeight),
    };
  }, []);

  const clampOcrHeight = useCallback((height) => {
    const maxHeight = Math.max(OCR_MIN_HEIGHT, size.height - OCR_SIDEBAR_RESERVED_HEIGHT);
    return Math.min(Math.max(OCR_MIN_HEIGHT, height), maxHeight);
  }, [size.height]);

  const stopDrag = useCallback(() => {
    const state = dragStateRef.current;
    if (!state) return;
    document.body.style.userSelect = state.previousUserSelect;
    window.removeEventListener('pointermove', state.handleMove);
    window.removeEventListener('pointerup', state.handleEnd);
    window.removeEventListener('pointercancel', state.handleEnd);
    dragStateRef.current = null;
  }, []);

  const stopResize = useCallback(() => {
    const state = resizeStateRef.current;
    if (!state) return;
    document.body.style.userSelect = state.previousUserSelect;
    window.removeEventListener('pointermove', state.handleMove);
    window.removeEventListener('pointerup', state.handleEnd);
    window.removeEventListener('pointercancel', state.handleEnd);
    resizeStateRef.current = null;
  }, []);

  const stopOcrResize = useCallback(() => {
    const state = ocrResizeStateRef.current;
    if (!state) return;
    document.body.style.userSelect = state.previousUserSelect;
    window.removeEventListener('pointermove', state.handleMove);
    window.removeEventListener('pointerup', state.handleEnd);
    window.removeEventListener('pointercancel', state.handleEnd);
    ocrResizeStateRef.current = null;
  }, []);

  const beginDrag = useCallback((event) => {
    if (event.button !== undefined && event.button !== 0) return;
    if (event.target?.closest?.('[data-preview-control]')) return;

    const node = dockRef.current;
    if (!node) return;
    event.preventDefault();
    event.stopPropagation();
    const rect = node.getBoundingClientRect();
    const handleMove = (moveEvent) => {
      const state = dragStateRef.current;
      if (!state) return;
      moveEvent.preventDefault();
      const next = clampPosition(
        moveEvent.clientX - state.offsetX,
        moveEvent.clientY - state.offsetY,
        state.width,
        state.height
      );
      setPosition(next);
    };
    const handleEnd = () => stopDrag();

    dragStateRef.current = {
      offsetX: event.clientX - rect.left,
      offsetY: event.clientY - rect.top,
      width: rect.width,
      height: rect.height,
      previousUserSelect: document.body.style.userSelect,
      handleMove,
      handleEnd,
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('pointermove', handleMove);
    window.addEventListener('pointerup', handleEnd);
    window.addEventListener('pointercancel', handleEnd);
  }, [clampPosition, stopDrag]);

  const beginResize = useCallback((event, direction) => {
    if (event.button !== undefined && event.button !== 0) return;
    const node = dockRef.current;
    if (!node) return;
    event.preventDefault();
    event.stopPropagation();

    const startX = event.clientX;
    const startY = event.clientY;
    const startSize = { ...size };
    const startPosition = { ...position };
    const handleMove = (moveEvent) => {
      const state = resizeStateRef.current;
      if (!state) return;
      moveEvent.preventDefault();
      const nextWidth = direction.includes('e')
        ? state.startSize.width + (moveEvent.clientX - state.startX)
        : state.startSize.width;
      const nextHeight = direction.includes('s')
        ? state.startSize.height + (moveEvent.clientY - state.startY)
        : state.startSize.height;
      const next = clampSize(nextWidth, nextHeight, state.startPosition.left, state.startPosition.top);
      setSize(next);
      setPosition((prev) => clampPosition(prev.left, prev.top, next.width, next.height));
    };
    const handleEnd = () => stopResize();

    resizeStateRef.current = {
      startX,
      startY,
      startSize,
      startPosition,
      previousUserSelect: document.body.style.userSelect,
      handleMove,
      handleEnd,
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('pointermove', handleMove);
    window.addEventListener('pointerup', handleEnd);
    window.addEventListener('pointercancel', handleEnd);
  }, [clampPosition, clampSize, position, size, stopResize]);

  const beginOcrResize = useCallback((event) => {
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();

    const startY = event.clientY;
    const startHeight = ocrPanelHeight;
    const handleMove = (moveEvent) => {
      const state = ocrResizeStateRef.current;
      if (!state) return;
      moveEvent.preventDefault();
      setOcrPanelHeight(clampOcrHeight(state.startHeight + (state.startY - moveEvent.clientY)));
    };
    const handleEnd = () => stopOcrResize();

    ocrResizeStateRef.current = {
      startY,
      startHeight,
      previousUserSelect: document.body.style.userSelect,
      handleMove,
      handleEnd,
    };
    document.body.style.userSelect = 'none';
    window.addEventListener('pointermove', handleMove);
    window.addEventListener('pointerup', handleEnd);
    window.addEventListener('pointercancel', handleEnd);
  }, [clampOcrHeight, ocrPanelHeight, stopOcrResize]);

  useEffect(() => stopDrag, [stopDrag]);
  useEffect(() => stopResize, [stopResize]);
  useEffect(() => stopOcrResize, [stopOcrResize]);

  useEffect(() => {
    const handleResize = () => {
      const node = dockRef.current;
      if (!node) return;
      const rect = node.getBoundingClientRect();
      const nextSize = minimized ? { width: rect.width, height: rect.height } : clampSize(size.width, size.height, position.left, position.top);
      if (!minimized) {
        setSize((prev) => (
          prev.width === nextSize.width && prev.height === nextSize.height ? prev : nextSize
        ));
      }
      setPosition((prev) => {
        const next = clampPosition(prev.left, prev.top, nextSize.width, nextSize.height);
        return prev.left === next.left && prev.top === next.top ? prev : next;
      });
    };
    window.addEventListener('resize', handleResize);
    handleResize();
    return () => window.removeEventListener('resize', handleResize);
  }, [clampPosition, clampSize, minimized, position.left, position.top, size.height, size.width]);

  useEffect(() => {
    if (!activeKey || lastActiveKeyRef.current === activeKey) return;
    if (lastActiveKeyRef.current !== null) setMinimized(false);
    lastActiveKeyRef.current = activeKey;
  }, [activeKey]);

  useEffect(() => {
    setOcrPanelHeight((prev) => clampOcrHeight(prev));
  }, [clampOcrHeight]);

  useEffect(() => {
    if (!activeTab || !resolvedActiveKey) return undefined;

    const hasImage = Boolean(imageCache[resolvedActiveKey]);
    const hasDetails = Boolean(detailsCache[resolvedActiveKey]);
    if (hasImage && hasDetails) return undefined;

    const targetId = getTargetId(activeTab);
    const targetPath = getTargetPath(activeTab);
    if (!targetId && !targetPath) return undefined;

    let cancelled = false;
    const requestId = ++requestIdRef.current;
    setLoadingKey(resolvedActiveKey);
    setErrorMap((prev) => {
      if (!prev[resolvedActiveKey]) return prev;
      const next = { ...prev };
      delete next[resolvedActiveKey];
      return next;
    });

    (async () => {
      try {
        const [details, image] = await Promise.all([
          hasDetails ? Promise.resolve(detailsCache[resolvedActiveKey]) : getScreenshotDetails(targetId, targetPath),
          hasImage ? Promise.resolve(imageCache[resolvedActiveKey]) : fetchImage(targetId, targetPath),
        ]);

        if (cancelled || requestIdRef.current !== requestId) return;

        if (details?.error) throw new Error(details.error);
        if (details && !hasDetails) {
          storeLimited(setDetailsCache, detailsOrderRef, resolvedActiveKey, details, DETAILS_CACHE_LIMIT);
        }
        if (image && !hasImage) {
          storeLimited(setImageCache, imageOrderRef, resolvedActiveKey, image, IMAGE_CACHE_LIMIT);
        }
      } catch (err) {
        if (!cancelled) {
          setErrorMap((prev) => ({ ...prev, [resolvedActiveKey]: err?.message || String(err) }));
        }
      } finally {
        if (!cancelled && requestIdRef.current === requestId) {
          setLoadingKey(null);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [activeTab, resolvedActiveKey, detailsCache, imageCache]);

  const copyText = useCallback(async (text) => {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
      copyTimerRef.current = setTimeout(() => setCopied(false), 1200);
    } catch (err) {
      console.error('Failed to copy preview text:', err);
    }
  }, []);

  const handleOpenUrl = useCallback((url) => {
    if (!url) return;
    openUrl(url).catch((err) => console.error('Failed to open URL:', err));
  }, []);

  if (!tabs?.length || !activeTab || !resolvedActiveKey) return null;

  const processName = record.process_name || activeTab.process_name || activeTab.metadata?.process_name || t('advancedSearch.unknown', 'Unknown');
  const rawWindowTitle = record.window_title || activeTab.window_title || activeTab.metadata?.window_title || null;
  const windowTitle = rawWindowTitle || '—';
  const displayTitle = rawWindowTitle || processName;
  const category = record.category || activeTab.category || activeTab.metadata?.category || null;
  const timestamp = record.created_at || activeTab.created_at || activeTab.metadata?.created_at || activeTab.screenshot_created_at;
  const screenshotId = getTargetId(activeTab);
  const score = activeTab.rerank_score;
  const assignedAt = activeTab.assigned_at;
  const sourceLabel = getLocalizedSourceLabel(activeTab, t);
  const sourceDetail = activeTab.sourceDetail;
  const tabTitle = displayTitle || `#${screenshotId || ''}`;

  const shellClass = standalone
    ? 'relative flex h-full w-full flex-col overflow-hidden bg-ide-panel'
    : 'fixed z-40 flex max-w-[calc(100vw-1rem)] max-h-[calc(100vh-1rem)] flex-col overflow-hidden rounded-lg border border-ide-border bg-ide-panel shadow-xl';
  const shellStyle = standalone
    ? undefined
    : { left: position.left, top: position.top, width: size.width, height: size.height };

  if (minimized) {
    return (
      <div
        ref={dockRef}
        className="fixed z-40 flex w-80 max-w-[calc(100vw-1rem)] select-none items-center gap-2 overflow-hidden rounded-lg border border-ide-border bg-ide-panel px-2 py-1.5 shadow-xl"
        style={{ left: position.left, top: position.top }}
        onPointerDown={beginDrag}
      >
        <GripVertical className="h-4 w-4 shrink-0 cursor-move text-ide-muted" />
        <ImageIcon className="h-3.5 w-3.5 shrink-0 text-ide-accent" />
        <button
          type="button"
          data-preview-control
          className="min-w-0 flex-1 text-left"
          onClick={() => setMinimized(false)}
          title={t('snapshotPreview.restore')}
        >
          <div className="truncate text-xs font-medium text-ide-text">{tabTitle}</div>
          <div className="truncate text-[10px] text-ide-muted">
            {t('snapshotPreview.tabCount', { count: tabs.length })}
          </div>
        </button>
        <button
          type="button"
          data-preview-control
          className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
          onClick={() => setMinimized(false)}
          title={t('snapshotPreview.restore')}
        >
          <Maximize2 className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          data-preview-control
          className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
          onClick={onClear}
          title={t('snapshotPreview.closeAll')}
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    );
  }

  return (
    <div
      ref={dockRef}
      className={shellClass}
      style={shellStyle}
    >
      <div
        className="flex shrink-0 select-none items-center border-b border-ide-border bg-ide-panel"
        onPointerDown={standalone ? undefined : beginDrag}
      >
        {!standalone && (
          <div className="flex h-full shrink-0 cursor-move items-center px-2 text-ide-muted">
            <GripVertical className="h-4 w-4" />
          </div>
        )}
        <div className="flex min-w-0 flex-1 overflow-x-auto">
          {tabs.map((tab) => {
            const key = getPreviewKey(tab);
            const label = tab.window_title || tab.metadata?.window_title || tab.process_name || tab.metadata?.process_name || `#${tab.screenshot_id || tab.id || ''}`;
            const selected = key === resolvedActiveKey;
            return (
              <button
                key={key}
                type="button"
                data-preview-control
                onClick={() => onActiveChange?.(key)}
                className={`group flex max-w-48 shrink-0 items-center gap-2 border-r border-ide-border px-3 py-2 text-left text-xs transition-colors ${
                  selected
                    ? 'bg-ide-bg text-ide-text'
                    : 'text-ide-muted hover:bg-ide-hover/40 hover:text-ide-text'
                }`}
                title={tab.window_title || tab.metadata?.window_title || label}
              >
                <ImageIcon className={`h-3.5 w-3.5 shrink-0 ${selected ? 'text-ide-accent' : ''}`} />
                <span className="min-w-0 flex-1 truncate">{label}</span>
                <span
                  role="button"
                  tabIndex={0}
                  data-preview-control
                  className="rounded p-0.5 text-ide-muted hover:bg-ide-hover hover:text-ide-text"
                  onClick={(event) => {
                    event.stopPropagation();
                    onCloseTab?.(key);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter' || event.key === ' ') {
                      event.preventDefault();
                      event.stopPropagation();
                      onCloseTab?.(key);
                    }
                  }}
                  title={t('snapshotPreview.closeTab')}
                >
                  <X className="h-3 w-3" />
                </span>
              </button>
            );
          })}
        </div>
        <div className="flex shrink-0 items-center gap-1 px-2">
          {!standalone && (
            <button
              type="button"
              data-preview-control
              className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
              onClick={() => setMinimized(true)}
              title={t('snapshotPreview.minimize')}
            >
              <Minimize2 className="h-3.5 w-3.5" />
            </button>
          )}
          <button
            type="button"
            data-preview-control
            className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
            onClick={onClear}
            title={t('snapshotPreview.closeAll')}
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="main-preview-surface relative flex min-w-0 flex-1 items-center justify-center overflow-hidden">
          <div className="pointer-events-none absolute inset-0" aria-hidden="true">
            <div className="main-preview-orb main-preview-orb--a" />
            <div className="main-preview-orb main-preview-orb--b" />
            <div className="main-preview-grid" />
          </div>
          {activeImage ? (
            <div className="relative z-10 flex h-full w-full items-center justify-center p-3">
              <InspectorImage
                item={{ imageUrl: activeImage, prompt: tabTitle }}
                overlayBoxes={ocrBoxes}
                className="h-full w-full rounded-none border-0 bg-transparent"
              />
            </div>
          ) : activeTab.thumbnailSrc ? (
            <img
              src={activeTab.thumbnailSrc}
              alt={tabTitle}
              className="relative z-10 h-full w-full object-contain p-3 opacity-80"
              loading="lazy"
            />
          ) : (
            <div className="relative z-10 flex flex-col items-center gap-2 text-sm text-ide-muted">
              <Loader2 className="h-5 w-5 animate-spin" />
              <span>{t('snapshotPreview.loadingImage')}</span>
            </div>
          )}

          {(isLoading || activeError) && (
            <div className="absolute left-3 top-3 rounded border border-ide-border bg-ide-panel/95 px-2.5 py-1.5 text-xs shadow">
              {activeError ? (
                <span className="text-red-400">{activeError}</span>
              ) : (
                <span className="flex items-center gap-1.5 text-ide-muted">
                  <Loader2 className="h-3 w-3 animate-spin" />
                  {t('snapshotPreview.loadingDetails')}
                </span>
              )}
            </div>
          )}
        </div>

        <aside className="flex min-h-0 w-72 shrink-0 flex-col overflow-hidden border-l border-ide-border bg-ide-panel">
          <div className="min-h-0 flex-1 space-y-3 overflow-y-auto border-b border-ide-border p-3">
            {onOpenInMainPreview && (
              <button
                type="button"
                onClick={() => onOpenInMainPreview(activeTab)}
                className="flex w-full items-center justify-center gap-2 rounded-md border border-ide-accent bg-ide-accent/15 px-3 py-2.5 text-sm font-medium text-ide-accent transition-colors hover:bg-ide-accent/25"
              >
                <Maximize2 className="h-4 w-4" />
                {t('snapshotPreview.openMain')}
              </button>
            )}
            {onOpenStandalone && (
              <button
                type="button"
                onClick={onOpenStandalone}
                className="flex w-full items-center justify-center gap-2 rounded-md border border-ide-border bg-ide-bg px-3 py-2 text-xs font-medium text-ide-text transition-colors hover:bg-ide-hover"
              >
                <ExternalLink className="h-3.5 w-3.5" />
                {t('snapshotPreview.openStandalone')}
              </button>
            )}

            <div className="min-w-0">
              <div className="truncate text-sm font-semibold text-ide-text" title={displayTitle}>
                {displayTitle}
              </div>
              <div className="mt-0.5 line-clamp-2 text-xs text-ide-muted" title={rawWindowTitle ? processName : windowTitle}>
                {rawWindowTitle ? processName : windowTitle}
              </div>
            </div>

            <div className="space-y-2">
              {sourceLabel && (
                <MetadataRow icon={Info} label={t('snapshotPreview.source')}>
                  <div className="space-y-0.5">
                    <div className="text-ide-text">{sourceLabel}</div>
                    {sourceDetail && (
                      <div className="line-clamp-2 text-[11px] text-ide-muted" title={sourceDetail}>
                        {sourceDetail}
                      </div>
                    )}
                  </div>
                </MetadataRow>
              )}
              <MetadataRow icon={Clock} label={t('snapshotPreview.time')} value={formatTimestamp(timestamp)} />
              {assignedAt && (
                <MetadataRow icon={Clock} label={t('snapshotPreview.assignedAt')} value={formatTimestamp(assignedAt)} />
              )}
              <MetadataRow icon={Hash} label={t('snapshotPreview.id')} value={screenshotId ? String(screenshotId) : null} />
              {typeof score === 'number' && (
                <MetadataRow icon={Hash} label={t('snapshotPreview.score')} value={score.toFixed(3)} />
              )}
              {category && (
                <MetadataRow icon={Tag} label={t('snapshotPreview.category')}>
                  <CategoryBadge category={category} />
                </MetadataRow>
              )}
              <MetadataRow icon={Monitor} label={t('snapshotPreview.path')} value={getTargetPath(activeTab)} />
            </div>

            {urls.length > 0 && (
              <div className="space-y-1">
                <div className="text-[10px] font-semibold uppercase text-ide-muted">
                  {t('snapshotPreview.links')}
                </div>
                {urls.map((url) => (
                  <button
                    key={url}
                    type="button"
                    onClick={() => handleOpenUrl(url)}
                    className="flex w-full items-center gap-2 rounded border border-ide-border bg-ide-bg px-2 py-1.5 text-left text-xs text-ide-text transition-colors hover:bg-ide-hover"
                    title={url}
                  >
                    <ExternalLink className="h-3.5 w-3.5 shrink-0 text-ide-accent" />
                    <span className="truncate">{url}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          <div
            className="relative flex shrink-0 flex-col"
            style={ocrExpanded ? { height: ocrPanelHeight } : undefined}
          >
            {ocrExpanded && (
              <div
                className="absolute left-0 right-0 top-0 z-10 h-1.5 cursor-ns-resize bg-transparent hover:bg-ide-accent/30"
                onPointerDown={beginOcrResize}
                title={t('snapshotPreview.resizeOcr')}
              />
            )}
            <div className="flex shrink-0 items-center justify-between border-b border-ide-border bg-ide-panel px-3 py-2">
              <button
                type="button"
                className="flex min-w-0 flex-1 items-center gap-1.5 text-left text-xs font-medium text-ide-text hover:text-ide-accent"
                onClick={() => setOcrExpanded((prev) => !prev)}
                title={ocrExpanded ? t('snapshotPreview.collapseOcr') : t('snapshotPreview.expandOcr')}
              >
                <ChevronDown className={`h-3.5 w-3.5 shrink-0 transition-transform ${ocrExpanded ? '' : '-rotate-90'}`} />
                <span className="truncate">{t('snapshotPreview.ocrText')}</span>
              </button>
              <button
                type="button"
                disabled={!ocrText}
                onClick={() => copyText(ocrText)}
                className="ml-2 flex shrink-0 items-center gap-1 rounded border border-ide-border px-2 py-1 text-xs text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text disabled:cursor-not-allowed disabled:opacity-40"
                title={t('snapshotPreview.copyOcr')}
              >
                <Copy className="h-3 w-3" />
                {copied ? t('snapshotPreview.copied') : t('snapshotPreview.copy')}
              </button>
            </div>
            {ocrExpanded && (
              <textarea
                readOnly
                value={ocrText}
                className="min-h-0 flex-1 resize-none bg-ide-bg p-3 font-mono text-xs leading-relaxed text-ide-text outline-none placeholder:text-ide-muted"
                placeholder={isLoading ? t('snapshotPreview.loadingOcr') : t('snapshotPreview.noOcr')}
              />
            )}
          </div>
        </aside>
      </div>

      {!standalone && (
        <>
          <div
            className="absolute bottom-0 right-0 h-5 w-5 cursor-nwse-resize"
            onPointerDown={(event) => beginResize(event, 'se')}
            title={t('snapshotPreview.resize')}
          >
            <div className="absolute bottom-1 right-1 h-3 w-3 border-b-2 border-r-2 border-ide-muted/60" />
          </div>
          <div
            className="absolute bottom-0 left-0 right-5 h-1.5 cursor-ns-resize"
            onPointerDown={(event) => beginResize(event, 's')}
          />
          <div
            className="absolute bottom-5 right-0 top-10 w-1.5 cursor-ew-resize"
            onPointerDown={(event) => beginResize(event, 'e')}
          />
        </>
      )}
    </div>
  );
}
