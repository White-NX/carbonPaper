import React, { useCallback, useEffect, useRef, useState } from 'react';
import { emitTo, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Image as ImageIcon, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import SnapshotPreviewDock from './SnapshotPreviewDock';
import {
  getSnapshotPreviewKey,
  sanitizeSnapshotPreviewState,
  SNAPSHOT_PREVIEW_WINDOW_STATE_KEY,
} from '../lib/snapshot_preview';

function readPreviewState() {
  try {
    const raw = localStorage.getItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY);
    if (!raw) return { tabs: [], activeKey: null };
    const parsed = JSON.parse(raw);
    return sanitizeSnapshotPreviewState(parsed);
  } catch {
    return { tabs: [], activeKey: null };
  }
}

function getSnapshotTitle(tab, t) {
  if (!tab) return t('snapshotPreview.windowTitle');
  const id = tab.screenshot_id || tab.id;
  return tab.window_title
    || tab.metadata?.window_title
    || tab.process_name
    || tab.metadata?.process_name
    || (id ? t('snapshotPreview.snapshotWithId', { id }) : t('snapshotPreview.windowTitle'));
}

function applyThemeFromStorage() {
  const saved = localStorage.getItem('theme');
  const prefersDark = window.matchMedia?.('(prefers-color-scheme: dark)').matches;
  const dark = saved ? saved === 'dark' : prefersDark;
  document.documentElement.classList.toggle('dark', !!dark);
}

function StandaloneTitleBar({ title }) {
  const { t } = useTranslation();
  const appWindow = getCurrentWindow();

  const startDrag = (event) => {
    if (event.button !== 0) return;
    appWindow.startDragging().catch(() => {});
  };

  const closeWindow = () => {
    appWindow.close().catch(() => {});
  };

  return (
    <div
      className="flex h-10 shrink-0 select-none items-center gap-2 border-b border-ide-border bg-ide-panel px-3"
      onMouseDown={startDrag}
      data-tauri-drag-region
    >
      <ImageIcon className="h-4 w-4 shrink-0 text-ide-accent" />
      <div className="min-w-0 flex-1 truncate text-sm font-medium text-ide-text" title={title}>
        {title}
      </div>
      <button
        type="button"
        className="rounded p-1.5 text-ide-muted transition-colors hover:bg-red-500/15 hover:text-red-400"
        onMouseDown={(event) => event.stopPropagation()}
        onClick={closeWindow}
        title={t('common.close')}
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}

export default function SnapshotPreviewStandalone() {
  const { t } = useTranslation();
  const [tabs, setTabs] = useState(() => readPreviewState().tabs);
  const [activeKey, setActiveKey] = useState(() => readPreviewState().activeKey);
  const suppressNextEmitRef = useRef(false);
  const closeTimerRef = useRef(null);

  const activeTab = tabs.find((tab) => getSnapshotPreviewKey(tab) === activeKey) || tabs[0] || null;
  const title = getSnapshotTitle(activeTab, t);

  const persistAndNotify = useCallback((nextTabs, nextActiveKey) => {
    const state = {
      tabs: nextTabs,
      activeKey: nextActiveKey,
      updatedAt: Date.now(),
    };
    try {
      localStorage.setItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY, JSON.stringify(sanitizeSnapshotPreviewState(state)));
      emitTo('main', 'snapshot-preview-state-changed', state).catch(() => {});
    } catch {
      // best-effort POC sync
    }
  }, []);

  useEffect(() => {
    applyThemeFromStorage();
    const handleStorage = (event) => {
      if (event.key === 'theme') applyThemeFromStorage();
    };
    window.addEventListener('storage', handleStorage);
    return () => window.removeEventListener('storage', handleStorage);
  }, []);

  useEffect(() => {
    document.title = title;
  }, [title]);

  useEffect(() => {
    let unlisten;
    listen('snapshot-preview-state', (event) => {
      const payload = event.payload || {};
      suppressNextEmitRef.current = true;
      setTabs(Array.isArray(payload.tabs) ? payload.tabs : []);
      setActiveKey(payload.activeKey || null);
    }).then((fn) => {
      unlisten = fn;
    });

    const handleStorage = (event) => {
      if (event.key !== SNAPSHOT_PREVIEW_WINDOW_STATE_KEY) return;
      const next = readPreviewState();
      suppressNextEmitRef.current = true;
      setTabs(next.tabs);
      setActiveKey(next.activeKey);
    };
    window.addEventListener('storage', handleStorage);

    return () => {
      if (unlisten) unlisten();
      window.removeEventListener('storage', handleStorage);
    };
  }, []);

  useEffect(() => {
    if (tabs.length > 0) {
      if (closeTimerRef.current) {
        clearTimeout(closeTimerRef.current);
        closeTimerRef.current = null;
      }
      return undefined;
    }

    closeTimerRef.current = setTimeout(() => {
      getCurrentWindow().close().catch(() => {});
    }, 120);

    return () => {
      if (closeTimerRef.current) {
        clearTimeout(closeTimerRef.current);
        closeTimerRef.current = null;
      }
    };
  }, [tabs.length]);

  useEffect(() => {
    if (suppressNextEmitRef.current) {
      suppressNextEmitRef.current = false;
      return;
    }
    persistAndNotify(tabs, activeKey);
  }, [activeKey, persistAndNotify, tabs]);

  const handleActiveChange = useCallback((key) => {
    setActiveKey(key);
  }, []);

  const closeTab = useCallback((key) => {
    setTabs((prev) => {
      const closingIndex = prev.findIndex((tab) => getSnapshotPreviewKey(tab) === key);
      const next = prev.filter((tab) => getSnapshotPreviewKey(tab) !== key);
      if (activeKey === key) {
        const fallback = next[closingIndex] || next[closingIndex - 1] || next[0] || null;
        setActiveKey(fallback ? getSnapshotPreviewKey(fallback) : null);
      }
      return next;
    });
  }, [activeKey]);

  const clearTabs = useCallback(() => {
    setTabs([]);
    setActiveKey(null);
  }, []);

  const openInMainPreview = useCallback((tab) => {
    const target = tab || activeTab;
    if (!target) return;
    emitTo('main', 'snapshot-preview-open-main', target).catch(() => {});
  }, [activeTab]);

  return (
    <div className="h-screen w-screen overflow-hidden bg-transparent p-[3px] text-ide-text">
      <div className="flex h-full w-full flex-col overflow-hidden rounded-md border border-ide-border bg-ide-bg shadow-2xl">
        <StandaloneTitleBar title={title} />
        <div className="min-h-0 flex-1">
          {tabs.length > 0 && (
            <SnapshotPreviewDock
              tabs={tabs}
              activeKey={activeKey}
              onActiveChange={handleActiveChange}
              onCloseTab={closeTab}
              onClear={clearTabs}
              onOpenInMainPreview={openInMainPreview}
              standalone
            />
          )}
        </div>
      </div>
    </div>
  );
}
