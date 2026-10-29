import { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import {
  getSnapshotPreviewKey, normalizeSnapshotPreviewItem, sanitizeSnapshotPreviewState,
  SNAPSHOT_PREVIEW_TAB_LIMIT, SNAPSHOT_PREVIEW_WINDOW_LABEL, SNAPSHOT_PREVIEW_WINDOW_STATE_KEY,
} from '../lib/snapshot_preview';

export function useSnapshotPreviewWindow({ isAuthenticated, onOpenTimeline }) {
  const { t } = useTranslation();
  const [state, setState] = useState({ tabs: [], activeKey: null });
  const stateRef = useRef(state);
  const authenticatedRef = useRef(isAuthenticated);
  authenticatedRef.current = isAuthenticated;
  const creatingRef = useRef(null);
  const openTimelineRef = useRef(onOpenTimeline);
  openTimelineRef.current = onOpenTimeline;
  const publish = useCallback((next) => {
    const sanitized = sanitizeSnapshotPreviewState(next);
    stateRef.current = sanitized;
    setState(sanitized);
    try { localStorage.setItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY, JSON.stringify(sanitized)); } catch { /* best effort */ }
  }, []);

  const restore = useCallback(async () => {
    if (!authenticatedRef.current || !stateRef.current.tabs.length) return;
    const sync = async (window) => {
      if (!authenticatedRef.current) return;
      await window.emit('snapshot-preview-state', sanitizeSnapshotPreviewState(stateRef.current));
      if (!authenticatedRef.current) return;
      await window.show();
      if (authenticatedRef.current) await window.setFocus();
    };
    try {
      const existing = await WebviewWindow.getByLabel(SNAPSHOT_PREVIEW_WINDOW_LABEL);
      if (existing) { await sync(existing); return; }
      if (creatingRef.current || !authenticatedRef.current) return;
      const preview = new WebviewWindow(SNAPSHOT_PREVIEW_WINDOW_LABEL, {
        url: 'index.html?window=snapshot-preview', title: t('snapshotPreview.nativeWindowTitle'),
        width: 1120, height: 760, minWidth: 600, minHeight: 460,
        resizable: true, decorations: false, transparent: true, shadow: true, focus: true,
      });
      creatingRef.current = preview;
      preview.once('tauri://created', () => { creatingRef.current = null; sync(preview).catch(console.warn); });
      preview.once('tauri://error', () => { creatingRef.current = null; });
      preview.once('tauri://destroyed', () => publish({ tabs: [], activeKey: null, updatedAt: Date.now() }));
    } catch (error) { console.warn('Failed to open snapshot preview window', error); }
  }, [publish, t]);

  const open = useCallback((item, options = {}) => {
    if (!authenticatedRef.current) return;
    const normalized = normalizeSnapshotPreviewItem(item, options);
    const key = getSnapshotPreviewKey(normalized);
    if (!key) return;
    const previous = stateRef.current.tabs;
    const exists = previous.some((tab) => getSnapshotPreviewKey(tab) === key);
    const tabs = exists ? previous.map((tab) => getSnapshotPreviewKey(tab) === key ? { ...tab, ...normalized } : tab)
      : [...previous, normalized].slice(-SNAPSHOT_PREVIEW_TAB_LIMIT);
    publish({ tabs, activeKey: key, updatedAt: Date.now() });
    void restore();
  }, [publish, restore]);

  useEffect(() => {
    let live = true;
    let disposeState, disposeOpen;
    listen('snapshot-preview-state-changed', ({ payload }) => {
      if (!live) return;
      publish(sanitizeSnapshotPreviewState(payload || {}));
    }).then((dispose) => { if (live) disposeState = dispose; else dispose(); });
    listen('snapshot-preview-open-main', ({ payload }) => {
      if (!live || !authenticatedRef.current || !getSnapshotPreviewKey(payload)) return;
      openTimelineRef.current?.(payload);
      const window = getCurrentWindow();
      window.show().catch(() => {}); window.setFocus().catch(() => {});
    }).then((dispose) => { if (live) disposeOpen = dispose; else dispose(); });
    return () => { live = false; disposeState?.(); disposeOpen?.(); };
  }, [publish]);

  return { ...state, open, restore };
}
