import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AdvancedSearch } from './AdvancedSearch';
import { InspectorImage } from './InspectorImage';
import DetailCard from './DetailCard';
import SmartClustersView from './SmartClustersView';
import { Image as ImageIcon, Loader2, Copy, Maximize2, X } from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { openUrl } from '@tauri-apps/plugin-opener';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { listen } from '@tauri-apps/api/event';
import PreviewActionBar from './PreviewActionBar';
import {
  getSnapshotPreviewKey,
  normalizeSnapshotPreviewItem,
  sanitizeSnapshotPreviewState,
  SNAPSHOT_PREVIEW_TAB_LIMIT,
  SNAPSHOT_PREVIEW_WINDOW_LABEL,
  SNAPSHOT_PREVIEW_WINDOW_STATE_KEY,
} from '../lib/snapshot_preview';

function upsertSnapshotPreviewTab(prevTabs, normalized, key) {
  const existingIndex = prevTabs.findIndex((tab) => getSnapshotPreviewKey(tab) === key);
  if (existingIndex >= 0) {
    return prevTabs.map((tab, index) => (
      index === existingIndex
        ? { ...tab, ...normalized, thumbnailSrc: normalized.thumbnailSrc || tab.thumbnailSrc || null }
        : tab
    ));
  }
  return [...prevTabs, normalized].slice(-SNAPSHOT_PREVIEW_TAB_LIMIT);
}

function SnapshotPreviewFloatButton({ tabs, activeKey, onOpen, onClear }) {
  const { t } = useTranslation();
  if (!tabs.length) return null;
  const active = tabs.find((tab) => getSnapshotPreviewKey(tab) === activeKey) || tabs[tabs.length - 1];
  const title = active?.window_title || active?.metadata?.window_title || active?.process_name || active?.metadata?.process_name || t('snapshotPreview.windowTitle');

  return (
    <div className="fixed bottom-4 right-4 z-40 flex max-w-80 items-center gap-2 rounded-lg border border-ide-border bg-ide-panel px-2 py-1.5 shadow-xl">
      <button
        type="button"
        className="flex min-w-0 flex-1 items-center gap-2 text-left"
        onClick={onOpen}
        title={t('snapshotPreview.openWindow')}
      >
        <ImageIcon className="h-4 w-4 shrink-0 text-ide-accent" />
        <span className="min-w-0">
          <span className="block truncate text-xs font-medium text-ide-text">{title}</span>
          <span className="block text-[10px] text-ide-muted">{t('snapshotPreview.tabCount', { count: tabs.length })}</span>
        </span>
      </button>
      <button
        type="button"
        className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
        onClick={onOpen}
        title={t('snapshotPreview.openWindow')}
      >
        <Maximize2 className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        className="rounded p-1.5 text-ide-muted transition-colors hover:bg-ide-hover hover:text-ide-text"
        onClick={onClear}
        title={t('snapshotPreview.clearTabs')}
      >
        <X className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}

// OCR Content Panel Component
function OcrContentPanel({ selectedDetails, onClose, onCopyText }) {
  const ocrText = selectedDetails?.ocr_results?.map(r => r.text).join('\n') || '';
  
  return (
    <div className="absolute right-0 top-0 bottom-0 w-64 bg-ide-panel border-l border-ide-border flex flex-col z-20 shadow-xl">
      <div className="flex items-center justify-between px-3 py-2 border-b border-ide-border bg-ide-bg shrink-0">
        <span className="text-xs font-medium">OCR Content</span>
        <button
          onClick={onClose}
          className="p-1 hover:bg-ide-hover rounded transition-colors"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
      <div className="flex-1 overflow-hidden">
        {/* 当 record.status 为 pending 时显示斜体文案 */}
        {selectedDetails?.record?.status === 'pending' ? (
          <div className="w-full h-full flex items-center justify-center text-sm italic text-ide-muted p-3">OCR Processing…</div>
        ) : (
          <textarea
            className="w-full h-full bg-ide-bg p-3 text-xs font-mono text-ide-text resize-none focus:outline-none leading-relaxed"
            readOnly
            value={ocrText}
            placeholder={selectedDetails ? "No text detected" : "Select an image to view OCR content"}
          />
        )}
      </div>
      {selectedDetails?.ocr_results?.length > 0 && (
        <div className="p-2 border-t border-ide-border bg-ide-panel shrink-0 flex justify-end">
          <button
            onClick={() => onCopyText(ocrText)}
            className="flex items-center gap-2 px-3 py-1.5 bg-ide-bg hover:bg-ide-hover border border-ide-border rounded text-xs transition-colors"
          >
            <Copy size={12} /> Copy All
          </button>
        </div>
      )}
    </div>
  );
}

export default function MainArea({
  activeTab,
  setActiveTab,
  selectedImageSrc,
  isLoadingDetails,
  selectedEvent,
  selectedDetails,
  lastError,
  ocrBoxes,
  onAdvancedSelect,
  advancedSearchParams,
  onInspectorBoxClick,
  searchMode,
  onSearchModeChange,
  onDeleteRecord,
  onDeleteNearbyRecords,
  onCopyText,
  backendOnline,
  isAuthenticated,
}) {
  const { t } = useTranslation();
  const [showOcrPanel, setShowOcrPanel] = useState(false);
  const [snapshotPreviewTabs, setSnapshotPreviewTabs] = useState([]);
  const [activeSnapshotPreviewKey, setActiveSnapshotPreviewKey] = useState(null);
  const snapshotPreviewTabsRef = useRef([]);
  const activeSnapshotPreviewKeyRef = useRef(null);

  useEffect(() => {
    snapshotPreviewTabsRef.current = snapshotPreviewTabs;
  }, [snapshotPreviewTabs]);

  useEffect(() => {
    activeSnapshotPreviewKeyRef.current = activeSnapshotPreviewKey;
  }, [activeSnapshotPreviewKey]);

  const handleShowMore = () => {
    setShowOcrPanel(!showOcrPanel);
  };

  const handleOpenUrl = async (url) => {
    if (!url) return;
    try {
      await openUrl(url);
    } catch (error) {
      console.error('Failed to open url', error);
    }
  };

  const handleCopyText = (text) => {
    navigator.clipboard.writeText(text);
    onCopyText?.(text);
  };

  const buildSnapshotPreviewWindowState = useCallback((tabs = snapshotPreviewTabsRef.current, activeKey = activeSnapshotPreviewKeyRef.current) => ({
    tabs,
    activeKey,
    updatedAt: Date.now(),
  }), []);

  const syncSnapshotPreviewWindow = useCallback(async (state = buildSnapshotPreviewWindowState()) => {
    try {
      localStorage.setItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY, JSON.stringify(sanitizeSnapshotPreviewState(state)));
      const existing = await WebviewWindow.getByLabel(SNAPSHOT_PREVIEW_WINDOW_LABEL);
      if (existing) {
        await existing.emit('snapshot-preview-state', state);
      }
    } catch (err) {
      console.warn('Failed to sync snapshot preview window:', err);
    }
  }, [buildSnapshotPreviewWindowState]);

  const openStandaloneSnapshotPreview = useCallback(async (stateOverride = null) => {
    const state = stateOverride || buildSnapshotPreviewWindowState();
    if (!state.tabs.length) return;
    await syncSnapshotPreviewWindow(state);

    try {
      const existing = await WebviewWindow.getByLabel(SNAPSHOT_PREVIEW_WINDOW_LABEL);
      if (existing) {
        await existing.show();
        await existing.setFocus();
        await existing.emit('snapshot-preview-state', state);
        return;
      }

      const previewWindow = new WebviewWindow(SNAPSHOT_PREVIEW_WINDOW_LABEL, {
        url: 'index.html?window=snapshot-preview',
        title: t('snapshotPreview.nativeWindowTitle'),
        width: 1040,
        height: 720,
        minWidth: 700,
        minHeight: 460,
        resizable: true,
        decorations: false,
        transparent: true,
        shadow: true,
        focus: true,
      });

      previewWindow.once('tauri://created', () => {
        previewWindow.emit('snapshot-preview-state', state).catch((err) => {
          console.warn('Failed to send initial snapshot preview state:', err);
        });
      });
      previewWindow.once('tauri://error', (event) => {
        console.error('Failed to create snapshot preview window:', event.payload);
      });
    } catch (err) {
      console.error('Failed to open snapshot preview window:', err);
    }
  }, [buildSnapshotPreviewWindowState, syncSnapshotPreviewWindow, t]);

  const openSnapshotPreview = useCallback((item, options = {}) => {
    const normalized = normalizeSnapshotPreviewItem(item, options);
    const key = getSnapshotPreviewKey(normalized);
    if (!key) return;

    const nextTabs = upsertSnapshotPreviewTab(snapshotPreviewTabsRef.current, normalized, key);
    const nextState = buildSnapshotPreviewWindowState(nextTabs, key);
    snapshotPreviewTabsRef.current = nextTabs;
    activeSnapshotPreviewKeyRef.current = key;
    setSnapshotPreviewTabs(nextTabs);
    setActiveSnapshotPreviewKey(key);
    openStandaloneSnapshotPreview(nextState);
  }, [buildSnapshotPreviewWindowState, openStandaloneSnapshotPreview]);

  const clearSnapshotPreviewTabs = useCallback(() => {
    snapshotPreviewTabsRef.current = [];
    activeSnapshotPreviewKeyRef.current = null;
    setSnapshotPreviewTabs([]);
    setActiveSnapshotPreviewKey(null);
    syncSnapshotPreviewWindow({ tabs: [], activeKey: null, updatedAt: Date.now() });
  }, [syncSnapshotPreviewWindow]);

  useEffect(() => {
    let unlisten;
    listen('snapshot-preview-state-changed', (event) => {
      const payload = event.payload || {};
      const nextTabs = Array.isArray(payload.tabs) ? payload.tabs : [];
      const nextActiveKey = payload.activeKey || null;
      snapshotPreviewTabsRef.current = nextTabs;
      activeSnapshotPreviewKeyRef.current = nextActiveKey;
      setSnapshotPreviewTabs(nextTabs);
      setActiveSnapshotPreviewKey(nextActiveKey);
      try {
        localStorage.setItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY, JSON.stringify(sanitizeSnapshotPreviewState({
          tabs: nextTabs,
          activeKey: nextActiveKey,
          updatedAt: payload.updatedAt || Date.now(),
        })));
      } catch {
        // best effort
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten;
    listen('snapshot-preview-open-main', (event) => {
      const payload = event.payload || {};
      const screenshotId = payload.screenshot_id ?? payload.id ?? payload.metadata?.screenshot_id;
      const imagePath = payload.image_path || payload.path || payload.metadata?.image_path;
      if (screenshotId === undefined && !imagePath) return;

      onAdvancedSelect?.(payload);
      setActiveTab('preview');
      const currentWindow = getCurrentWindow();
      currentWindow.show().catch(() => {});
      currentWindow.setFocus().catch(() => {});
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [onAdvancedSelect, setActiveTab]);

  const openCurrentPreviewInDock = useCallback(() => {
    if (!selectedEvent) return;
    openSnapshotPreview({
      ...selectedEvent,
      screenshot_id: selectedEvent.id,
      image_path: selectedEvent.path || selectedEvent.image_path,
      process_name: selectedDetails?.record?.process_name || selectedEvent.appName,
      window_title: selectedDetails?.record?.window_title || selectedEvent.windowTitle,
      category: selectedDetails?.record?.category || selectedEvent.category,
      created_at: selectedDetails?.record?.created_at || selectedEvent.timestamp,
    }, {
      sourceLabel: t('snapshotPreview.sources.mainPreview'),
      sourceDetail: selectedDetails?.record?.window_title || selectedEvent.windowTitle || selectedEvent.appName || null,
      sourceType: 'main-preview',
    });
  }, [openSnapshotPreview, selectedDetails, selectedEvent, t]);

  return (
    <section className="flex flex-col bg-ide-bg overflow-hidden relative flex-1">
      <div className="flex-1 flex flex-col min-h-0 overflow-hidden">
        <div className={`${activeTab === 'preview' ? 'flex' : 'hidden'} main-preview-surface flex-1 items-center justify-center overflow-hidden relative min-w-0 min-h-0`}>
          <div className="pointer-events-none absolute inset-0" aria-hidden="true">
            <div className="main-preview-orb main-preview-orb--a" />
            <div className="main-preview-orb main-preview-orb--b" />
            <div className="main-preview-grid" />
          </div>

          {selectedImageSrc ? (
            <div className="absolute inset-0 z-10 w-full h-full">
              <InspectorImage
                item={{ imageUrl: selectedImageSrc }}
                overlayBoxes={ocrBoxes}
                onBoxClick={onInspectorBoxClick}
                className="w-full h-full rounded-none border-none bg-transparent"
              />
            </div>
          ) : (
            <div className="text-ide-muted text-sm flex flex-col items-center gap-2">
              {isLoadingDetails ? (
                <>
                  <Loader2 className="w-6 h-6 animate-spin" />
                  <span>Loading...</span>
                </>
              ) : (
                <div className="flex flex-col items-center gap-1 text-center">
                  <span>{selectedEvent ? (lastError || "Image not found on disk") : ""}</span>
                  {selectedEvent && <span className="text-xs opacity-50 font-mono">ID: {selectedEvent.id}</span>}
                </div>
              )}
            </div>
          )}

          {!selectedEvent && !selectedImageSrc && !isLoadingDetails && (
            <div className="pointer-events-none absolute left-8 bottom-10 text-left select-none">
              <div className="text-ide-text opacity-85 text-[clamp(4.2rem,7vw,5.8rem)] leading-none font-black tracking-tight">Carbonpaper</div>
              <div className="mt-1.5 text-base md:text-lg font-medium text-ide-muted mx-3">
                Under <span className="font-semibold text-ide-text opacity-90">GPL-3</span> Licence
              </div>
            </div>
          )}

          {/* Preview Action Bar */}
          {activeTab === 'preview' && selectedEvent && (
            <PreviewActionBar
              selectedEvent={selectedEvent}
              selectedDetails={selectedDetails}
              onDeleteRecord={onDeleteRecord}
              onDeleteNearbyRecords={onDeleteNearbyRecords}
              onOpenUrl={handleOpenUrl}
              onShowMore={handleShowMore}
              onOpenFloatingPreview={openCurrentPreviewInDock}
              showOcrPanel={showOcrPanel}
            />
          )}

          {/* Detail Card (floating overlay) */}
          <DetailCard
            selectedEvent={selectedEvent}
            selectedDetails={selectedDetails}
            onSelectRelated={onAdvancedSelect}
            onOpenFloatingPreview={openSnapshotPreview}
          />

          {/* OCR Content Panel */}
          {showOcrPanel && (
            <OcrContentPanel
              selectedDetails={selectedDetails}
              onClose={() => setShowOcrPanel(false)}
              onCopyText={handleCopyText}
            />
          )}
        </div>

        <div className={`${activeTab === 'advanced-search' ? 'flex flex-col' : 'hidden'} flex-1 w-full min-w-0 min-h-0 overflow-hidden`}>
          <AdvancedSearch
            active={activeTab === 'advanced-search'}
            searchParams={advancedSearchParams}
            onSelectResult={onAdvancedSelect}
            onOpenSnapshotPreview={openSnapshotPreview}
            searchMode={searchMode}
            onSearchModeChange={onSearchModeChange}
            backendOnline={backendOnline}
          />
        </div>

        <div className={`${activeTab === 'smart-cluster' ? 'flex flex-col' : 'hidden'} flex-1 w-full min-w-0 min-h-0 overflow-hidden`}>
          <SmartClustersView
            backendOnline={backendOnline}
            isAuthenticated={isAuthenticated}
            active={activeTab === 'smart-cluster'}
            onOpenSnapshotPreview={openSnapshotPreview}
            onSelectScreenshot={(evt) => {
              onAdvancedSelect?.(evt);
            }}
          />
        </div>

        <SnapshotPreviewFloatButton
          tabs={snapshotPreviewTabs}
          activeKey={activeSnapshotPreviewKey}
          onOpen={() => openStandaloneSnapshotPreview()}
          onClear={clearSnapshotPreviewTabs}
        />
      </div>
    </section>
  );
}
