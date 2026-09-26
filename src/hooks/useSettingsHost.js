import { useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTauriEventListener } from './useTauriEventListener';
import { openSettingsWindow } from '../lib/settings_api';
import { withAuth } from '../lib/auth_api';
import { showDebugOverlay } from '../components/overlay/coordinator';

// Developer-tools preview names mapped to `[overlayId, variant]`.
const OVERLAY_PREVIEWS = {
  update: ['update'],
  'critical-update': ['update', 'critical'],
  extension: ['extensionSetup'],
  'smart-cluster': ['smartClusterSetup'],
  'app-bound': ['appBound'],
  'app-bound-repair': ['appBound', 'repair'],
};

export function useSettingsHost({ onMonitorAction, onRecordsChanged, onClosed, onError }) {
  const [showSettings, setShowSettings] = useState(false);
  const openSettings = useCallback(async (tab, section) => {
    try {
      await openSettingsWindow(tab, section);
      setShowSettings(true);
    } catch (error) { onError?.(error); }
  }, [onError]);
  useTauriEventListener('settings-monitor-request', async ({ payload }) => {
    if (!Number.isSafeInteger(payload?.id)) return;
    let claimed = false;
    let error = null;
    try {
      // Events are notifications, not authority. The native main-only claim
      // returns the stored action exactly once, including after duplicate events.
      const action = await invoke('take_settings_monitor_action', { id: payload.id });
      if (!action) return;
      claimed = true;
      await onMonitorAction(action);
    }
    catch (cause) { error = cause?.message || String(cause); }
    if (claimed) await invoke('complete_settings_monitor_action', { id: payload.id, error }).catch((cause) => onError?.(cause));
  });
  useTauriEventListener('settings-window-changed', ({ payload }) => {
    setShowSettings(Boolean(payload));
    if (!payload) onClosed?.();
  });
  useTauriEventListener('settings-debug-preview', async ({ payload }) => {
    if (!import.meta.env.DEV) return;
    const commands = { error: 'trigger_test_error', ocr: 'debug_trigger_ocr_model_repair_notification' };
    try {
      if (commands[payload]) await withAuth(() => invoke(commands[payload]), { autoPrompt: true });
      else if (OVERLAY_PREVIEWS[payload]) showDebugOverlay(...OVERLAY_PREVIEWS[payload]);
    } catch (error) { onError?.(error); }
  });
  useTauriEventListener('settings-preferences-changed', ({ payload }) => {
    const keys = Array.isArray(payload) ? payload : [];
    if (keys.some((key) => ['records', 'storage'].includes(key))) onRecordsChanged?.();
    if (keys.includes('models')) onClosed?.();
  });
  return { showSettings, openSettings };
}
