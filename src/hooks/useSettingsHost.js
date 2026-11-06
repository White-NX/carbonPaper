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
  onboarding: ['onboarding'],
  'whats-new': ['onboarding', 'whats_new'],
  'app-bound': ['appBound'],
  'app-bound-repair': ['appBound', 'repair'],
};

export function useSettingsHost({ onRecordsChanged, onClosed, onError }) {
  const [showSettings, setShowSettings] = useState(false);
  const openSettings = useCallback(async (tab, section) => {
    try {
      await openSettingsWindow(tab, section);
      setShowSettings(true);
    } catch (error) { onError?.(error); }
  }, [onError]);
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
